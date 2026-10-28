//! The reminder flow: what to do with text, locations and button presses.
//!
//! Handlers return replies instead of calling Telegram; the rules themselves live in
//! [`Service`]. This module only decides what to show.

use chrono::{DateTime, Duration, Utc};
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use url::Url;

use crate::calendar;
use crate::callback::{Action, Subject};
use crate::i18n::Lang;
use crate::model::{InputError, User, UserTz};
use crate::panels::{self, Panel};
use crate::service::{self, Profile, Service};
use crate::time_input;

/// A typed time is applied to the user's draft only if the draft is this recent.
const TYPED_TIME_WINDOW: Duration = Duration::minutes(30);
/// How long a draft waits for an unregistered user to choose their timezone.
const TIMEZONE_WINDOW: Duration = Duration::hours(24);
/// Short unrecognised text from an unregistered user is treated as a failed timezone answer.
const MAX_TIMEZONE_ANSWER_CHARS: usize = 30;

/// The chat a message came from. `chat_id` identifies the user in the database (in private
/// chats it equals the Telegram user id).
pub struct Sender {
    pub chat_id: i64,
    pub first_name: String,
    pub last_name: String,
    pub private: bool,
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
pub enum Markup {
    None,
    Inline(InlineKeyboardMarkup),
    /// One-tap "share my location" keyboard (private chats only).
    AskLocation,
    RemoveKeyboard,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub text: String,
    pub markup: Markup,
}

impl Reply {
    pub fn text(text: impl Into<String>) -> Self {
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
pub struct CallbackReply {
    pub toast: Option<String>,
    pub edit: Option<Panel>,
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
pub async fn on_text(
    service: &Service,
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
            panels::repeat_lang(Subject::Draft(draft.id), &draft.text, at, lang).into(),
        ]);
    }
    start_reminder(service, &user, text, now).await
}

/// Starts the "when?" flow for `text`.
pub async fn start_reminder(
    service: &Service,
    user: &User,
    text: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Reply>> {
    let lang = language(service, user.id).await?;
    let draft = service.start_draft(user.id, text, now).await?;
    Ok(vec![
        panels::when_lang(
            Subject::Draft(draft.id),
            &draft.text,
            user.tz.local(now),
            lang,
        )
        .into(),
    ])
}

/// Starts a reminder for a sender who may not be registered yet (used by `/remindme`).
pub async fn start_reminder_for(
    service: &Service,
    sender: &Sender,
    text: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Reply>> {
    match service.user(sender.chat_id).await? {
        Some(user) => start_reminder(service, &user, text, now).await,
        None => on_text(service, sender, text, true, now).await,
    }
}

pub async fn on_location(
    service: &Service,
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
pub async fn set_timezone(
    service: &Service,
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

pub fn ask_timezone_lang(sender: &Sender, intro: &str, lang: Lang) -> Reply {
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

pub async fn language(service: &Service, user_id: i64) -> service::Result<Lang> {
    Ok(Lang::parse(&service.preferences(user_id).await?.language))
}

/// The user's calendar feed link with instructions, or an explanation why there is none.
pub async fn calendar_link(
    service: &Service,
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

pub async fn on_callback(
    service: &Service,
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
            crate::i18n::Lang::parse(&service.preferences(user_id).await?.language)
                .text("error.preferences"),
        )),
        Err(service::Error::Internal(error)) => Err(error),
    }
}

async fn handle_action(
    service: &Service,
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
                CallbackReply::finish(format!("✅ {nag_text}"), Some(&lang.text("done.toast")))
            } else {
                already_done()
            });
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
            CallbackReply::edit(panels::when_lang(subject, &text, now_local, lang))
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

        Action::Save(subject, at, recurrence) => {
            let saved = match subject {
                Subject::Draft(id) => service
                    .save_draft(&user, id, at, recurrence, now)
                    .await
                    .map(|r| (r, lang.text("reminder.saved"))),
                Subject::Reminder(id) => service
                    .update(&user, id, None, at, recurrence, now)
                    .await
                    .map(|r| (r, lang.text("reminder.updated"))),
            };
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
                        edit: Some(panels::when_lang(subject, &text, now_local, lang)),
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
        Action::List => {
            let reminders = service.list(user_id).await?;
            CallbackReply::edit(panels::list_lang(&reminders, &user, lang))
        }

        Action::Noop
        | Action::CalendarReset
        | Action::Done(_)
        | Action::LegacyNaggingAck(_)
        | Action::LegacyReminderAck(_) => unreachable!("handled above"),
    })
}

async fn subject_text(
    service: &Service,
    user_id: i64,
    subject: Subject,
) -> service::Result<String> {
    Ok(match subject {
        Subject::Draft(id) => service.draft(user_id, id).await?.text,
        Subject::Reminder(id) => service.reminder(user_id, id).await?.description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::model::Recurrence;
    use crate::model::Snooze;
    use chrono::{NaiveDate, NaiveDateTime};
    use chrono_tz::Tz;
    use teloxide::types::InlineKeyboardButtonKind;

    const SGT: UserTz = UserTz::Named(Tz::Asia__Singapore);

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    fn now() -> DateTime<Utc> {
        SGT.utc(local(2026, 10, 13, 10, 30))
    }

    fn sender() -> Sender {
        Sender {
            chat_id: 7,
            first_name: "Ada".into(),
            last_name: String::new(),
            private: true,
        }
    }

    fn buttons(markup: &Markup) -> Vec<(String, Action)> {
        let Markup::Inline(keyboard) = markup else {
            return Vec::new();
        };
        keyboard
            .inline_keyboard
            .iter()
            .flatten()
            .filter_map(|b| match &b.kind {
                InlineKeyboardButtonKind::CallbackData(data) => {
                    Some((b.text.clone(), Action::parse(data)?))
                }
                _ => None,
            })
            .collect()
    }

    fn find(markup: &Markup, label: &str) -> Action {
        buttons(markup)
            .into_iter()
            .find(|(text, _)| text == label)
            .unwrap_or_else(|| panic!("no button {label:?} in {:?}", buttons(markup)))
            .1
    }

    async fn press(db: &Service, action: Action) -> CallbackReply {
        on_callback(db, 7, &action.encode(), Some("message"), None, now())
            .await
            .unwrap()
    }

    fn edit_markup(reply: &CallbackReply) -> Markup {
        Markup::Inline(reply.edit.as_ref().unwrap().keyboard.clone())
    }

    #[tokio::test]
    async fn new_user_is_asked_for_timezone_then_continues() {
        let store = Db::open_in_memory().unwrap();
        let db = Service::new(store.clone());
        db.set_preferences(7, Some("en"), None, None).await.unwrap();
        db.set_preferences(8, Some("en"), None, None).await.unwrap();
        let replies = on_text(&db, &sender(), "call mum", false, now())
            .await
            .unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].markup, Markup::AskLocation);

        let replies = on_text(&db, &sender(), "Atlantis", false, now())
            .await
            .unwrap();
        assert!(
            replies[0].text.contains("don't recognise"),
            "{}",
            replies[0].text
        );

        let replies = on_text(&db, &sender(), "Singapore", false, now())
            .await
            .unwrap();
        assert!(replies[0].text.contains("Asia/Singapore"));
        assert_eq!(replies[0].markup, Markup::RemoveKeyboard);
        assert!(replies[1].text.contains("call mum"));
        assert!(replies[1].text.contains("When should I remind you?"));
    }

    #[tokio::test]
    async fn quick_choice_flow_saves_in_two_taps() {
        let store = Db::open_in_memory().unwrap();
        let db = Service::new(store.clone());
        db.set_preferences(7, Some("en"), None, None).await.unwrap();
        db.set_preferences(8, Some("en"), None, None).await.unwrap();
        set_timezone(&db, &sender(), SGT, now()).await.unwrap();

        let replies = on_text(&db, &sender(), "call mum", false, now())
            .await
            .unwrap();
        let tomorrow = find(&replies[0].markup, "Tomorrow 09:00");

        let repeat = press(&db, tomorrow).await;
        let daily = find(&edit_markup(&repeat), "Every day");

        let saved = press(&db, daily).await;
        assert!(
            saved
                .edit
                .as_ref()
                .unwrap()
                .text
                .starts_with("✅ Reminder saved")
        );

        let reminders = store.reminders_for_user(7).await.unwrap();
        assert_eq!(reminders.len(), 1);
        assert_eq!(reminders[0].description, "call mum");
        assert_eq!(reminders[0].recurrence, Recurrence::Daily);
        assert_eq!(SGT.local(reminders[0].due_at), local(2026, 10, 14, 9, 0));

        // Pressing the same button again does not create a duplicate.
        press(&db, daily).await;
        assert_eq!(store.reminders_for_user(7).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn calendar_flow_picks_date_hour_and_minute() {
        let store = Db::open_in_memory().unwrap();
        let db = Service::new(store.clone());
        db.set_preferences(7, Some("en"), None, None).await.unwrap();
        db.set_preferences(8, Some("en"), None, None).await.unwrap();
        set_timezone(&db, &sender(), SGT, now()).await.unwrap();
        let replies = on_text(&db, &sender(), "dentist", false, now())
            .await
            .unwrap();

        let calendar = press(&db, find(&replies[0].markup, "📅 Pick a date")).await;
        let next_month = press(&db, find(&edit_markup(&calendar), "›")).await;
        let hours = press(&db, find(&edit_markup(&next_month), "2")).await;
        let minutes = press(&db, find(&edit_markup(&hours), "14")).await;
        let repeat = press(&db, find(&edit_markup(&minutes), "14:45")).await;
        press(&db, find(&edit_markup(&repeat), "Once")).await;

        let reminders = store.reminders_for_user(7).await.unwrap();
        assert_eq!(SGT.local(reminders[0].due_at), local(2026, 11, 2, 14, 45));
        assert_eq!(reminders[0].recurrence, Recurrence::Once);
    }

    #[tokio::test]
    async fn typed_time_answers_the_when_question() {
        let store = Db::open_in_memory().unwrap();
        let db = Service::new(store.clone());
        db.set_preferences(7, Some("en"), None, None).await.unwrap();
        db.set_preferences(8, Some("en"), None, None).await.unwrap();
        set_timezone(&db, &sender(), SGT, now()).await.unwrap();
        on_text(&db, &sender(), "pay rent", false, now())
            .await
            .unwrap();

        let replies = on_text(&db, &sender(), "fri 6pm", false, now())
            .await
            .unwrap();
        assert!(
            replies[0].text.contains("Fri 16 Oct 2026, 18:00"),
            "{}",
            replies[0].text
        );
        press(&db, find(&replies[0].markup, "Monthly · 16")).await;

        let reminders = store.reminders_for_user(7).await.unwrap();
        assert_eq!(reminders[0].description, "pay rent");
        assert_eq!(reminders[0].recurrence, Recurrence::Monthly);
    }

    #[tokio::test]
    async fn edit_and_delete_from_the_list() {
        let store = Db::open_in_memory().unwrap();
        let db = Service::new(store.clone());
        db.set_preferences(7, Some("en"), None, None).await.unwrap();
        db.set_preferences(8, Some("en"), None, None).await.unwrap();
        set_timezone(&db, &sender(), SGT, now()).await.unwrap();
        let replies = on_text(&db, &sender(), "stretch", false, now())
            .await
            .unwrap();
        let repeat = press(&db, find(&replies[0].markup, "1 hour")).await;
        press(&db, find(&edit_markup(&repeat), "Weekdays")).await;
        let reminder = store.reminders_for_user(7).await.unwrap().remove(0);

        let list = press(&db, Action::List).await;
        let manage = press(&db, find(&edit_markup(&list), "1. stretch")).await;
        let repeat = press(&db, find(&edit_markup(&manage), "🔁 Change repeat")).await;
        press(&db, find(&edit_markup(&repeat), "Every Tue")).await;
        assert_eq!(
            store
                .reminder(7, reminder.id)
                .await
                .unwrap()
                .unwrap()
                .recurrence,
            Recurrence::Weekly
        );

        let deleted = press(&db, Action::Delete(reminder.id)).await;
        assert!(deleted.edit.unwrap().text.contains("Removed: stretch"));
        assert!(store.reminders_for_user(7).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancel_discards_the_draft() {
        let store = Db::open_in_memory().unwrap();
        let db = Service::new(store.clone());
        db.set_preferences(7, Some("en"), None, None).await.unwrap();
        db.set_preferences(8, Some("en"), None, None).await.unwrap();
        set_timezone(&db, &sender(), SGT, now()).await.unwrap();
        let replies = on_text(&db, &sender(), "nothing", false, now())
            .await
            .unwrap();
        let quick = find(&replies[0].markup, "15 min");
        press(&db, find(&replies[0].markup, "✖ Cancel")).await;
        let reply = press(&db, quick).await;
        assert_eq!(
            reply.toast.as_deref(),
            Some("This reminder is no longer available.")
        );
    }

    #[tokio::test]
    async fn snooze_and_done_on_a_nag() {
        let store = Db::open_in_memory().unwrap();
        let db = Service::new(store.clone());
        db.set_preferences(7, Some("en"), None, None).await.unwrap();
        db.set_preferences(8, Some("en"), None, None).await.unwrap();
        set_timezone(&db, &sender(), SGT, now()).await.unwrap();
        let reminder = store
            .add_reminder(7, "pills".into(), now(), Recurrence::Once)
            .await
            .unwrap();
        let nagging = store
            .fire_reminder(reminder.id, now(), now())
            .await
            .unwrap()
            .unwrap();

        let reply = on_callback(
            &db,
            7,
            &Action::Snooze(nagging.id, Snooze::TomorrowMorning).encode(),
            Some("⏰ pills"),
            None,
            now(),
        )
        .await
        .unwrap();
        assert!(
            reply
                .edit
                .unwrap()
                .text
                .contains("Snoozed until Wed 14 Oct 2026, 09:00")
        );
        assert!(store.due_naggings(now()).await.unwrap().is_empty());

        let reply = on_callback(
            &db,
            7,
            &Action::Done(nagging.id).encode(),
            Some("⏰ pills"),
            None,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(reply.edit.unwrap().text, "✅ pills");
        assert!(
            store
                .due_naggings(now() + Duration::days(2))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn other_users_cannot_press_your_buttons() {
        let store = Db::open_in_memory().unwrap();
        let db = Service::new(store.clone());
        db.set_preferences(7, Some("en"), None, None).await.unwrap();
        db.set_preferences(8, Some("en"), None, None).await.unwrap();
        set_timezone(&db, &sender(), SGT, now()).await.unwrap();
        let mut other = sender();
        other.chat_id = 8;
        set_timezone(&db, &other, SGT, now()).await.unwrap();

        let replies = on_text(&db, &sender(), "secret", false, now())
            .await
            .unwrap();
        let quick = find(&replies[0].markup, "15 min");
        let reply = on_callback(&db, 8, &quick.encode(), None, None, now())
            .await
            .unwrap();
        assert_eq!(
            reply.toast.as_deref(),
            Some("This reminder is no longer available.")
        );
    }
}
