//! The reminder flow: what to do with text, locations and button presses.
//!
//! Handlers return replies instead of calling Telegram; the rules themselves live in
//! [`ReminderService`]. This module only decides what to show.

use crate::domain::UserId;
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
use crate::domain::{User, UserTz};

/// A typed time is applied to the user's draft only if the draft is this recent.
const TYPED_TIME_WINDOW: Duration = Duration::minutes(30);
/// How long a draft waits for an unregistered user to choose their timezone.
const TIMEZONE_WINDOW: Duration = Duration::hours(24);
/// Short unrecognised text from an unregistered user is treated as a failed timezone answer.
const MAX_TIMEZONE_ANSWER_CHARS: usize = 30;

/// The chat a message came from. `chat_id` identifies the user in the database (in private
/// chats it equals the Telegram user id).
pub(crate) struct Sender {
    pub(crate) chat_id: UserId,
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

pub(crate) async fn language(service: &ReminderService, user_id: UserId) -> service::Result<Lang> {
    Ok(service.preferences(user_id).await?.language)
}

/// The user's calendar feed link with instructions, or an explanation why there is none.
pub(crate) async fn calendar_link(
    service: &ReminderService,
    user_id: UserId,
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

#[cfg(test)]
mod tests;

mod callbacks;
pub(crate) use callbacks::on_callback;
