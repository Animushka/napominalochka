//! HTTP request/response contracts and conversion into validated application values.
use super::ApiError;
use crate::adapters::i18n::Lang;
use crate::adapters::presentation::relative_lang;
use crate::application as service;
use crate::domain::{
    Completion, Nagging, Recurrence, Reminder, ReminderId, ReminderSettings, UserTz,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
pub(super) const LOCAL_FORMAT: &str = "%Y-%m-%dT%H:%M";

#[derive(Serialize)]
pub(super) struct StateView {
    pub(super) language: crate::domain::Language,
    pub(super) nag_interval_minutes: crate::domain::NagInterval,
    pub(super) confirmation_check_minutes: crate::domain::ConfirmationDelay,
    /// Display name, e.g. "Asia/Singapore (UTC+08:00)"; `None` until a timezone is chosen.
    pub(super) timezone: Option<String>,
    /// IANA name or offset, comparable with the device's timezone.
    pub(super) timezone_id: Option<String>,
    /// Current local time in the user's timezone, `YYYY-MM-DDTHH:MM`.
    pub(super) now: Option<String>,
    pub(super) reminders: Vec<ReminderView>,
    pub(super) pending: Vec<PendingView>,
}

#[derive(Serialize)]
pub(super) struct PendingView {
    pub(super) id: crate::domain::NaggingId,
    pub(super) reminder_id: Option<ReminderId>,
    pub(super) description: String,
    pub(super) next_at: String,
    pub(super) next_when: String,
    pub(super) relative: String,
}
impl PendingView {
    pub(super) fn new(pending: Nagging, tz: UserTz, now: DateTime<Utc>, lang: Lang) -> Self {
        let local = tz.local(pending.next_at);
        Self {
            id: pending.id,
            reminder_id: pending.reminder_id,
            description: pending.description,
            next_at: local.format(LOCAL_FORMAT).to_string(),
            next_when: lang.date(local),
            relative: relative_lang(now, pending.next_at, lang),
        }
    }
}

#[derive(Serialize)]
pub(super) struct ReminderView {
    pub(super) id: crate::domain::ReminderId,
    pub(super) description: String,
    /// Local time in the user's timezone, `YYYY-MM-DDTHH:MM` (for `<input type=date/time>`).
    pub(super) at: String,
    pub(super) recurrence: &'static str,
    pub(super) settings: ReminderSettings,
    pub(super) when: String,
    pub(super) relative: String,
    pub(super) repeat: String,
}

impl ReminderView {
    pub(super) fn new(reminder: &Reminder, tz: UserTz, now: DateTime<Utc>, lang: Lang) -> Self {
        let local = tz.local(reminder.due_at);
        Self {
            id: reminder.id,
            description: reminder.description.clone(),
            at: local.format(LOCAL_FORMAT).to_string(),
            recurrence: reminder.recurrence.as_str(),
            settings: reminder.settings.clone(),
            when: lang.date(local),
            relative: relative_lang(now, reminder.due_at, lang),
            repeat: lang.recurrence(reminder.recurrence, reminder.anchor_local.unwrap_or(local)),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SettingsInput {
    pub(super) nag_interval_minutes: Option<i64>,
    pub(super) confirmation_check_minutes: Option<i64>,
    #[serde(default = "voice_enabled_by_default")]
    pub(super) voice_enabled: bool,
}
fn voice_enabled_by_default() -> bool {
    true
}
impl TryFrom<SettingsInput> for ReminderSettings {
    type Error = service::Error;
    fn try_from(input: SettingsInput) -> service::Result<Self> {
        Ok(Self {
            nag_interval_minutes: input
                .nag_interval_minutes
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| service::Error::InvalidPreferences("nag_interval_minutes"))?,
            confirmation_check_minutes: input
                .confirmation_check_minutes
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| service::Error::InvalidPreferences("confirmation_check_minutes"))?,
            voice_enabled: input.voice_enabled,
        })
    }
}

#[derive(Deserialize)]
pub(super) struct TimezoneInput {
    pub(super) timezone: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreferencesInput {
    pub(super) language: Option<String>,
    pub(super) nag_interval_minutes: Option<i64>,
    pub(super) confirmation_check_minutes: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReminderBody {
    pub(super) description: String,
    pub(super) at: String,
    pub(super) recurrence: String,
    #[serde(default)]
    pub(super) preserve_schedule: bool,
    pub(super) settings: Option<SettingsInput>,
}

// ---- handlers -------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SnoozeInput {
    pub(super) duration: String,
}
#[derive(Serialize)]
pub(super) struct CalendarLink {
    pub(super) url: String,
    pub(super) webcal: String,
}

impl ReminderBody {
    /// Parses the wire format; everything else is validated by the service.
    pub(super) fn schedule(&self) -> Result<(NaiveDateTime, Recurrence), ApiError> {
        let at = NaiveDateTime::parse_from_str(&self.at, LOCAL_FORMAT).map_err(|_| {
            ApiError::bad_request("invalid_datetime", "Please pick a date and time.")
        })?;
        let recurrence = self.recurrence.parse().map_err(service::Error::from)?;
        Ok((at, recurrence))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HistoryQuery {
    pub(super) before: Option<crate::domain::CompletionId>,
    pub(super) limit: Option<usize>,
}
#[derive(Serialize)]
pub(super) struct CompletionView {
    pub(super) id: crate::domain::CompletionId,
    pub(super) description: String,
    pub(super) scheduled_at: Option<String>,
    pub(super) completed_at: String,
    pub(super) undone_at: Option<String>,
    pub(super) can_undo: bool,
}
impl CompletionView {
    pub(super) fn new(item: Completion, tz: UserTz) -> Self {
        let local = |at| tz.local(at).format(LOCAL_FORMAT).to_string();
        Self {
            id: item.id,
            description: item.description,
            scheduled_at: item.scheduled_at.map(local),
            completed_at: local(item.completed_at),
            undone_at: item.undone_at.map(local),
            can_undo: item.can_undo,
        }
    }
}
#[derive(Serialize)]
pub(super) struct HistoryView {
    pub(super) items: Vec<CompletionView>,
    pub(super) next_cursor: Option<crate::domain::CompletionId>,
}
