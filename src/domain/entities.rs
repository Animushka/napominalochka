//! Reminder entities, settings and completion records.
use super::{Recurrence, UserTz};
use chrono::{DateTime, NaiveDateTime, Utc};

/// Settings are independent of timezone registration and isolated by Telegram user id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Preferences {
    pub(crate) language: String,
    pub(crate) nag_interval_minutes: i64,
    pub(crate) confirmation_check_minutes: i64,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            language: "ru".into(),
            nag_interval_minutes: 10,
            confirmation_check_minutes: 0,
        }
    }
}

/// Overrides inherit user defaults; voice eligibility retains the account switch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ReminderSettings {
    pub(crate) nag_interval_minutes: Option<i64>,
    pub(crate) confirmation_check_minutes: Option<i64>,
    pub(crate) voice_enabled: bool,
}

impl Default for ReminderSettings {
    fn default() -> Self {
        Self {
            nag_interval_minutes: None,
            confirmation_check_minutes: None,
            voice_enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReminderWrite {
    pub(crate) description: String,
    pub(crate) due_at: DateTime<Utc>,
    pub(crate) recurrence: Recurrence,
    pub(crate) anchor_local: Option<NaiveDateTime>,
    pub(crate) settings: ReminderSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReminderSchedule {
    pub(crate) due_at: DateTime<Utc>,
    pub(crate) recurrence: Recurrence,
    pub(crate) anchor_local: Option<NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReminderChanges {
    pub(crate) description: Option<String>,
    pub(crate) schedule: Option<ReminderSchedule>,
    pub(crate) settings: Option<ReminderSettings>,
}

/// A completion mark records the user's action in the bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Completion {
    pub(crate) id: i64,
    pub(crate) user_id: i64,
    pub(crate) description: String,
    pub(crate) scheduled_at: Option<DateTime<Utc>>,
    pub(crate) completed_at: DateTime<Utc>,
    pub(crate) undone_at: Option<DateTime<Utc>>,
    pub(crate) can_undo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryPage {
    pub(crate) items: Vec<Completion>,
    pub(crate) next_cursor: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UndoResult {
    Restored,
    AlreadyRestored,
    Unavailable,
    NotFound,
}

/// A single optional check after completion, never part of the nagging loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Confirmation {
    pub(crate) id: i64,
    pub(crate) user_id: i64,
    pub(crate) description: String,
    pub(crate) due_at: DateTime<Utc>,
    pub(crate) completion_id: Option<i64>,
}

/// A calendar selection keeps its local wall clock; an elapsed interval keeps its instant.
/// This distinction matters when clocks move backwards and a local time occurs twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReminderTime {
    Local(NaiveDateTime),
    Instant(DateTime<Utc>),
}

impl ReminderTime {
    pub(crate) fn local(self, tz: UserTz) -> NaiveDateTime {
        match self {
            Self::Local(at) => at,
            Self::Instant(at) => tz.local(at),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct User {
    pub(crate) id: i64,
    pub(crate) first_name: String,
    pub(crate) last_name: String,
    pub(crate) tz: UserTz,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reminder {
    pub(crate) id: i64,
    pub(crate) user_id: i64,
    pub(crate) description: String,
    pub(crate) due_at: DateTime<Utc>,
    pub(crate) recurrence: Recurrence,
    /// Original local schedule; NULL in migrated rows until their next firing.
    pub(crate) anchor_local: Option<NaiveDateTime>,
    pub(crate) settings: ReminderSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Nagging {
    pub(crate) id: i64,
    pub(crate) reminder_id: Option<i64>,
    pub(crate) user_id: i64,
    pub(crate) description: String,
    pub(crate) next_at: DateTime<Utc>,
}

/// How long to postpone a nagging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Snooze {
    TenMinutes,
    OneHour,
    TomorrowMorning,
}

/// Text the user sent that is waiting for a time to be chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Draft {
    pub(crate) id: i64,
    pub(crate) user_id: i64,
    pub(crate) text: String,
    pub(crate) created_at: DateTime<Utc>,
}
