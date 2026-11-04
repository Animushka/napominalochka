//! Shared reminder use cases and validation for inbound adapters.

use chrono::{DateTime, Datelike, NaiveDateTime, NaiveTime, Utc, Weekday};
use std::sync::Arc;
use thiserror::Error;

use super::ports::ReminderRepository;
use crate::domain::{InputError, Recurrence, UserTz};

pub(crate) const MAX_DESCRIPTION_CHARS: usize = 1000;
/// "Snooze until tomorrow" means this local time.
const SNOOZE_MORNING: NaiveTime = NaiveTime::from_hms_opt(9, 0, 0).expect("valid time");

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("Please set your timezone first.")]
    NotRegistered,
    /// The reminder or draft does not exist or belongs to someone else.
    #[error("This reminder is no longer available.")]
    NotFound,
    #[error("This completion can no longer be undone.")]
    CompletionUnavailable,
    #[error("Invalid preference: {0}")]
    InvalidPreferences(&'static str),
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Storage(#[from] crate::application::ports::StorageError),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// Who a request is from, as reported by Telegram.
pub(crate) struct Profile {
    pub(crate) id: crate::domain::UserId,
    pub(crate) first_name: String,
    pub(crate) last_name: String,
}

/// A requested reminder: `at` is local time in the user's timezone.
pub(crate) struct ReminderInput<'a> {
    pub(crate) description: &'a str,
    pub(crate) at: NaiveDateTime,
    pub(crate) recurrence: Recurrence,
}

#[derive(Clone)]
pub(crate) struct ReminderService {
    repository: Arc<dyn ReminderRepository>,
}

impl ReminderService {
    pub(crate) fn new(repository: impl ReminderRepository) -> Self {
        Self {
            repository: Arc::new(repository),
        }
    }

    pub(crate) async fn check_storage(&self) -> Result<()> {
        Ok(self.repository.ping().await?)
    }
}

fn description(text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(InputError::EmptyDescription.into());
    }
    if text.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(InputError::DescriptionTooLong.into());
    }
    Ok(text.to_owned())
}

/// A time that has passed moves to the next occurrence; for one-off reminders it is an error.
fn first_due(
    at: NaiveDateTime,
    recurrence: Recurrence,
    tz: UserTz,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    recurrence
        .first_due(at, tz, now)
        .ok_or_else(|| InputError::InPast.into())
}

fn first_instant(
    at: DateTime<Utc>,
    recurrence: Recurrence,
    tz: UserTz,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    if at <= now {
        return Err(InputError::InPast.into());
    }
    let local = tz.local(at);
    if recurrence == Recurrence::Weekdays && matches!(local.weekday(), Weekday::Sat | Weekday::Sun)
    {
        first_due(local, recurrence, tz, now)
    } else {
        Ok(at)
    }
}

mod calendar;
mod drafts;
mod management;
mod pending;
mod preferences;
mod users;
