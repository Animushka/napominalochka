//! Callback handling and reminder actions.
use super::{CallbackReply, calendar_link, language};
use crate::adapters::telegram::callback::{Action, Subject};
use crate::adapters::telegram::panels::{self, Panel};
use crate::application::{self as service, ReminderService};
use crate::domain::{InputError, ReminderTime, UserId};
use chrono::{DateTime, Utc};
use teloxide::types::InlineKeyboardMarkup;
use url::Url;

pub(crate) async fn on_callback(
    service: &ReminderService,
    user_id: UserId,
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
            service
                .preferences(user_id)
                .await?
                .language
                .text("error.preferences"),
        )),
        Err(service::Error::CompletionUnavailable) => {
            Ok(CallbackReply::toast(lang.text("undo.unavailable")))
        }
        Err(service::Error::Internal(error)) => Err(error),
        Err(service::Error::Storage(error)) => Err(error.into()),
    }
}

async fn handle_action(
    service: &ReminderService,
    user_id: UserId,
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
                        tracing::warn!(?error, user_id = %user_id, "could not load completion undo button");
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
    user_id: UserId,
    subject: Subject,
) -> service::Result<String> {
    Ok(match subject {
        Subject::Draft(id) => service.draft(user_id, id).await?.text,
        Subject::Reminder(id) => service.reminder(user_id, id).await?.description,
    })
}
