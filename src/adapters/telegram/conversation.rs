//! The reminder flow: what to do with text, locations and button presses.
//!
//! Handlers return replies instead of calling Telegram; the rules themselves live in
//! [`ReminderService`]. This module only decides what to show.

use chrono::{DateTime, Duration, Utc};
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use url::Url;

use crate::adapters::calendar;
use crate::adapters::i18n::Lang;
use crate::adapters::telegram::callback::{Action, Subject};
use crate::adapters::telegram::panels::{self, Panel};
use crate::adapters::telegram::time_input;
use crate::application as service;
use crate::application::{Profile, ReminderService};
use crate::domain::{InputError, ReminderTime, User, UserTz};

/// A typed time is applied to the user's draft only if the draft is this recent.
const TYPED_TIME_WINDOW: Duration = Duration::minutes(30);
/// How long a draft waits for an unregistered user to choose their timezone.
const TIMEZONE_WINDOW: Duration = Duration::hours(24);
/// Short unrecognised text from an unregistered user is treated as a failed timezone answer.
const MAX_TIMEZONE_ANSWER_CHARS: usize = 30;

/// The chat a message came from. `chat_id` identifies the user in the database (in private
/// chats it equals the Telegram user id).
pub(crate) struct Sender {
    pub(crate) chat_id: i64,
    pub(crate) first_name: String,
    pub(crate) last_name: String,
    pub(crate) private: bool,
}

impl Sender {
    fn profile(&self) -> Profile {
        Profile {
            id: self.chat_id,
            first_name: self.first_name.clone(),
            last_name: self.last_name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Markup {
    None,
    Inline(InlineKeyboardMarkup),
    /// One-tap "share my location" keyboard (private chats only).
    AskLocation,
    RemoveKeyboard,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Reply {
    pub(crate) text: String,
    pub(crate) markup: Markup,
}

impl Reply {
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            markup: Markup::None,
        }
    }
}

impl From<Panel> for Reply {
    fn from(panel: Panel) -> Self {
        Self {
            text: panel.text,
            markup: Markup::Inline(panel.keyboard),
        }
    }
}

/// Response to a button press: an optional popup and an optional edit of the message.
#[derive(Default)]
pub(crate) struct CallbackReply {
    pub(crate) toast: Option<String>,
    pub(crate) edit: Option<Panel>,
}

impl CallbackReply {
    fn edit(panel: Panel) -> Self {
        Self {
            toast: None,
            edit: Some(panel),
        }
    }

    fn toast(text: impl Into<String>) -> Self {
        Self {
            toast: Some(text.into()),
            edit: None,
        }
    }

    /// Replaces the message text and removes its buttons.
    fn finish(text: impl Into<String>, toast: Option<&str>) -> Self {
        Self {
            toast: toast.map(str::to_owned),
            edit: Some(Panel {
                text: text.into(),
                keyboard: InlineKeyboardMarkup::default(),
            }),
        }
    }
}

/// Plain text or a forwarded message.
pub(crate) async fn on_text(
    service: &ReminderService,
    sender: &Sender,
    text: &str,
    forwarded: bool,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Reply>> {
    let lang = language(service, sender.chat_id).await?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }

    let Some(user) = service.user(sender.chat_id).await? else {
        // Not registered: this is either the answer to "which timezone?" or a new reminder.
        let awaiting_timezone = service
            .pending_draft(sender.chat_id, TIMEZONE_WINDOW, now)
            .await?
            .is_some();
        if !forwarded && awaiting_timezone {
            match UserTz::parse(text) {
                Ok(tz) => return set_timezone(service, sender, tz, now).await,
                Err(error) if text.chars().count() <= MAX_TIMEZONE_ANSWER_CHARS => {
                    return Ok(vec![ask_timezone_lang(
                        sender,
                        &lang.input_error(&error),
                        lang,
                    )]);
                }
                Err(_) => {}
            }
        }
        let draft = service.start_draft(sender.chat_id, text, now).await?;
        return Ok(vec![ask_timezone_lang(
            sender,
            &lang.render("timezone.first", &[("description", draft.text)]),
            lang,
        )]);
    };

    if !forwarded
        && let Some(draft) = service
            .pending_draft(user.id, TYPED_TIME_WINDOW, now)
            .await?
        && let Some(at) = time_input::parse(text, user.tz, now)
    {
        return Ok(vec![
            panels::repeat_time_lang(Subject::Draft(draft.id), &draft.text, at, user.tz, lang)
                .into(),
        ]);
    }
    start_reminder(service, &user, text, now).await
}

/// Starts the "when?" flow for `text`.
pub(crate) async fn start_reminder(
    service: &ReminderService,
    user: &User,
    text: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Reply>> {
    let lang = language(service, user.id).await?;
    let draft = service.start_draft(user.id, text, now).await?;
    Ok(vec![
        panels::when_at_lang(Subject::Draft(draft.id), &draft.text, now, user.tz, lang).into(),
    ])
}

/// Starts a reminder for a sender who may not be registered yet (used by `/remindme`).
pub(crate) async fn start_reminder_for(
    service: &ReminderService,
    sender: &Sender,
    text: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Reply>> {
    match service.user(sender.chat_id).await? {
        Some(user) => start_reminder(service, &user, text, now).await,
        None => on_text(service, sender, text, true, now).await,
    }
}

pub(crate) async fn on_location(
    service: &ReminderService,
    sender: &Sender,
    latitude: f64,
    longitude: f64,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Reply>> {
    let lang = language(service, sender.chat_id).await?;
    let tz =
        tokio::task::spawn_blocking(move || UserTz::from_location(latitude, longitude)).await?;
    match tz {
        Some(tz) => set_timezone(service, sender, tz, now).await,
        None => Ok(vec![ask_timezone_lang(
            sender,
            &lang.text("timezone.location_failed"),
            lang,
        )]),
    }
}

/// Registers the sender (or updates their timezone) and resumes any pending reminder.
pub(crate) async fn set_timezone(
    service: &ReminderService,
    sender: &Sender,
    tz: UserTz,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Reply>> {
    let lang = language(service, sender.chat_id).await?;
    service.set_timezone(sender.profile(), tz).await?;

    let mut replies = vec![Reply {
        text: lang.render("timezone.set", &[("timezone", tz.to_string())]),
        markup: Markup::RemoveKeyboard,
    }];
    match service
        .pending_draft(sender.chat_id, TIMEZONE_WINDOW, now)
        .await?
    {
        Some(draft) => replies.push(
            panels::when_lang(Subject::Draft(draft.id), &draft.text, tz.local(now), lang).into(),
        ),
        None => replies.push(Reply::text(lang.text("reminder.create_hint"))),
    }
    Ok(replies)
}

pub(crate) fn ask_timezone_lang(sender: &Sender, intro: &str, lang: Lang) -> Reply {
    let text = lang.render(
        if sender.private {
            "timezone.private_hint"
        } else {
            "timezone.group_hint"
        },
        &[("intro", intro.to_owned())],
    );
    Reply {
        text,
        markup: if sender.private {
            Markup::AskLocation
        } else {
            Markup::None
        },
    }
}

pub(crate) async fn language(service: &ReminderService, user_id: i64) -> service::Result<Lang> {
    Ok(Lang::parse(&service.preferences(user_id).await?.language))
}

/// The user's calendar feed link with instructions, or an explanation why there is none.
pub(crate) async fn calendar_link(
    service: &ReminderService,
    user_id: i64,
    web_app: Option<&Url>,
    reset: bool,
) -> anyhow::Result<Panel> {
    let lang = language(service, user_id).await?;
    let text = |t: &str| Panel {
        text: t.to_owned(),
        keyboard: InlineKeyboardMarkup::default(),
    };
    let Some(base) = web_app else {
        return Ok(text(&lang.text("calendar.disabled")));
    };
    let token = match service.calendar_token(user_id, reset).await {
        Ok(token) => token,
        Err(service::Error::NotRegistered) => {
            return Ok(text(&lang.text("error.register")));
        }
        Err(error) => return Err(error.into()),
    };
    let Some(url) = calendar::feed_url(base, &token) else {
        return Ok(text(&lang.text("calendar.bad_url")));
    };
    Ok(Panel {
        text: calendar::instructions_lang(&url, lang),
        keyboard: InlineKeyboardMarkup::new([[InlineKeyboardButton::callback(
            lang.text("calendar.new_link"),
            Action::CalendarReset.encode(),
        )]]),
    })
}

pub(crate) async fn on_callback(
    service: &ReminderService,
    user_id: i64,
    data: &str,
    message_text: Option<&str>,
    web_app: Option<&Url>,
    now: DateTime<Utc>,
) -> anyhow::Result<CallbackReply> {
    let lang = language(service, user_id).await?;
    let Some(action) = Action::parse(data) else {
        return Ok(CallbackReply::toast(lang.text("button.invalid")));
    };
    match handle_action(service, user_id, action, message_text, web_app, now).await {
        Ok(reply) => Ok(reply),
        Err(service::Error::NotFound) => Ok(CallbackReply::finish(
            message_text.unwrap_or(&lang.text("error.missing")),
            Some(&lang.text("error.missing")),
        )),
        Err(service::Error::NotRegistered) => Ok(CallbackReply::toast(lang.text("error.register"))),
        Err(service::Error::Input(error)) => Ok(CallbackReply::toast(lang.input_error(&error))),
        Err(service::Error::InvalidPreferences(_)) => Ok(CallbackReply::toast(
            crate::adapters::i18n::Lang::parse(&service.preferences(user_id).await?.language)
                .text("error.preferences"),
        )),
        Err(service::Error::CompletionUnavailable) => {
            Ok(CallbackReply::toast(lang.text("undo.unavailable")))
        }
        Err(service::Error::Internal(error)) => Err(error),
    }
}

async fn handle_action(
    service: &ReminderService,
    user_id: i64,
    action: Action,
    message_text: Option<&str>,
    web_app: Option<&Url>,
    now: DateTime<Utc>,
) -> service::Result<CallbackReply> {
    let lang = language(service, user_id).await?;
    // Buttons on reminder messages ("⏰ <description>").
    let nag_text = message_text.map_or("", |t| t.trim_start_matches("⏰").trim_start());
    let already_done = || {
        CallbackReply::finish(
            message_text.unwrap_or_default(),
            Some(&lang.text("done.already")),
        )
    };

    match action {
        Action::Noop => return Ok(CallbackReply::default()),
        Action::CalendarReset => {
            return Ok(CallbackReply {
                toast: Some(lang.text("calendar.reset")),
                edit: Some(calendar_link(service, user_id, web_app, true).await?),
            });
        }
        Action::Done(id) | Action::LegacyNaggingAck(id) => {
            return Ok(if service.done_at(user_id, id, now).await? {
                let keyboard = match service.completion_for_source(user_id, id).await {
                    Ok(Some(completion)) if completion.can_undo => {
                        panels::completion_keyboard_lang(completion.id, lang)
                    }
                    Ok(_) => InlineKeyboardMarkup::default(),
                    Err(error) => {
                        // The completion succeeded. A failed optional UI read must not
                        // turn that success into a request to complete it again.
                        tracing::warn!(?error, user_id, "could not load completion undo button");
                        InlineKeyboardMarkup::default()
                    }
                };
                CallbackReply {
                    toast: Some(lang.text("done.toast")),
                    edit: Some(Panel {
                        text: format!("✅ {nag_text}"),
                        keyboard,
                    }),
                }
            } else {
                already_done()
            });
        }
        Action::UndoCompletion(id) => {
            let restored = service.undo_completion(user_id, id, now).await?;
            return Ok(CallbackReply::finish(
                lang.text(if restored {
                    "undo.restored_message"
                } else {
                    "undo.already_message"
                }),
                Some(&lang.text(if restored {
                    "undo.toast"
                } else {
                    "undo.already"
                })),
            ));
        }
        Action::LegacyReminderAck(id) => {
            return Ok(if service.done_legacy(user_id, id).await? {
                CallbackReply::finish(format!("✅ {nag_text}"), Some(&lang.text("done.toast")))
            } else {
                already_done()
            });
        }
        _ => {}
    }

    let user = service.registered(user_id).await?;
    let now_local = user.tz.local(now);

    Ok(match action {
        Action::Snooze(id, snooze) => match service.snooze(&user, id, snooze, now).await? {
            Some(until) => CallbackReply::finish(
                lang.render(
                    "snooze.until",
                    &[
                        ("description", nag_text.to_owned()),
                        ("date", lang.date(user.tz.local(until))),
                    ],
                ),
                Some(&lang.text("snooze.toast")),
            ),
            None => already_done(),
        },

        Action::When(subject) => {
            let text = subject_text(service, user_id, subject).await?;
            CallbackReply::edit(panels::when_at_lang(subject, &text, now, user.tz, lang))
        }
        Action::Calendar(subject, year, month) => {
            let text = subject_text(service, user_id, subject).await?;
            CallbackReply::edit(panels::calendar_lang(
                subject,
                &text,
                year,
                month,
                now_local.date(),
                lang,
            ))
        }
        Action::Day(subject, date) => {
            let text = subject_text(service, user_id, subject).await?;
            CallbackReply::edit(panels::hours_lang(subject, &text, date, now_local, lang))
        }
        Action::Hour(subject, date, hour) => {
            let text = subject_text(service, user_id, subject).await?;
            CallbackReply::edit(panels::minutes_lang(
                subject, &text, date, hour, now_local, lang,
            ))
        }
        Action::At(subject, at) => {
            let text = subject_text(service, user_id, subject).await?;
            CallbackReply::edit(panels::repeat_lang(subject, &text, at, lang))
        }

        Action::AtInstant(subject, at) => {
            let text = subject_text(service, user_id, subject).await?;
            CallbackReply::edit(panels::repeat_time_lang(
                subject,
                &text,
                ReminderTime::Instant(at),
                user.tz,
                lang,
            ))
        }

        Action::Save(subject, _, _) | Action::SaveInstant(subject, _, _) => {
            let (time, recurrence) = match action {
                Action::Save(_, at, recurrence) => (ReminderTime::Local(at), recurrence),
                Action::SaveInstant(_, at, recurrence) => (ReminderTime::Instant(at), recurrence),
                _ => unreachable!("save action matched above"),
            };
            let saved = match (subject, time) {
                (Subject::Draft(id), ReminderTime::Local(at)) => {
                    service.save_draft(&user, id, at, recurrence, now).await
                }
                (Subject::Draft(id), ReminderTime::Instant(at)) => {
                    service.save_draft_at(&user, id, at, recurrence, now).await
                }
                (Subject::Reminder(id), ReminderTime::Local(at)) => {
                    service.update(&user, id, None, at, recurrence, now).await
                }
                (Subject::Reminder(id), ReminderTime::Instant(at)) => {
                    service
                        .update_at(&user, id, None, at, recurrence, now)
                        .await
                }
            }
            .map(|reminder| {
                (
                    reminder,
                    lang.text(match subject {
                        Subject::Draft(_) => "reminder.saved",
                        Subject::Reminder(_) => "reminder.updated",
                    }),
                )
            });
            match saved {
                Ok((reminder, heading)) => CallbackReply {
                    toast: Some(lang.text("saved.toast")),
                    edit: Some(panels::saved_lang(&reminder, &user, now, &heading, lang)),
                },
                // The picked time passed while the user was choosing: ask again.
                Err(service::Error::Input(InputError::InPast)) => {
                    let text = subject_text(service, user_id, subject).await?;
                    CallbackReply {
                        toast: Some(lang.text("error.past_retry")),
                        edit: Some(panels::when_at_lang(subject, &text, now, user.tz, lang)),
                    }
                }
                Err(error) => return Err(error),
            }
        }

        Action::CancelDraft(id) => {
            service.cancel_draft(user_id, id).await?;
            CallbackReply::finish(lang.text("cancelled"), None)
        }
        Action::Manage(id) => {
            let reminder = service.reminder(user_id, id).await?;
            CallbackReply::edit(panels::manage_lang(&reminder, &user, now, lang))
        }
        Action::Delete(id) => {
            let reminder = service.delete(user_id, id).await?;
            CallbackReply::finish(
                lang.render("removed", &[("description", reminder.description)]),
                Some(&lang.text("removed.toast")),
            )
        }
        Action::List | Action::ListPage(_) => {
            let page = match action {
                Action::ListPage(page) => page,
                _ => 0,
            };
            let reminders = service.list(user_id).await?;
            CallbackReply::edit(panels::list_page_lang(&reminders, &user, page, lang))
        }

        Action::Noop
        | Action::CalendarReset
        | Action::Done(_)
        | Action::UndoCompletion(_)
        | Action::LegacyNaggingAck(_)
        | Action::LegacyReminderAck(_) => unreachable!("handled above"),
    })
}

async fn subject_text(
    service: &ReminderService,
    user_id: i64,
    subject: Subject,
) -> service::Result<String> {
    Ok(match subject {
        Subject::Draft(id) => service.draft(user_id, id).await?.text,
        Subject::Reminder(id) => service.reminder(user_id, id).await?.description,
    })
}

#[cfg(test)]
mod tests;
