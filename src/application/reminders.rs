//! Application layer: every rule about reminders lives here, shared by the chat flow, the
//! Mini App API and the commands. Callers handle presentation only.

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, NaiveTime, Utc, Weekday};
use thiserror::Error;

use super::ports::{Registration, ReminderRepository};
use crate::model::{
    Completion, Draft, HistoryPage, InputError, Nagging, Preferences, Recurrence, Reminder,
    ReminderChanges, ReminderSchedule, ReminderSettings, ReminderWrite, Snooze, UndoResult, User,
    UserTz,
};

pub const MAX_DESCRIPTION_CHARS: usize = 1000;
/// "Snooze until tomorrow" means this local time.
const SNOOZE_MORNING: NaiveTime = NaiveTime::from_hms_opt(9, 0, 0).expect("valid time");

#[derive(Debug, Error)]
pub enum Error {
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
    Internal(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Who a request is from, as reported by Telegram.
pub struct Profile {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
}

/// A requested reminder: `at` is local time in the user's timezone.
pub struct ReminderInput<'a> {
    pub description: &'a str,
    pub at: NaiveDateTime,
    pub recurrence: Recurrence,
}

#[derive(Clone)]
pub struct ReminderService<R: ReminderRepository> {
    repository: R,
}

impl<R: ReminderRepository> ReminderService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    #[cfg(test)]
    pub(crate) fn repository(&self) -> &R {
        &self.repository
    }

    pub async fn check_storage(&self) -> Result<()> {
        Ok(self.repository.ping().await?)
    }

    pub async fn preferences(&self, user_id: i64) -> Result<Preferences> {
        Ok(self.repository.preferences(user_id).await?)
    }

    /// A manually chosen language is never overwritten by Telegram's language code.
    pub async fn initialize_language(
        &self,
        user_id: i64,
        telegram_language: Option<&str>,
    ) -> Result<()> {
        let language = if telegram_language.is_some_and(|code| {
            code.eq_ignore_ascii_case("en") || code.to_ascii_lowercase().starts_with("en-")
        }) {
            "en"
        } else {
            "ru"
        };
        self.repository
            .initialize_language(user_id, language.into())
            .await?;
        Ok(())
    }

    pub async fn set_preferences(
        &self,
        user_id: i64,
        language: Option<&str>,
        nag_interval_minutes: Option<i64>,
        confirmation_check_minutes: Option<i64>,
    ) -> Result<Preferences> {
        if language.is_some_and(|value| !matches!(value, "ru" | "en")) {
            return Err(Error::InvalidPreferences("language"));
        }
        if nag_interval_minutes.is_some_and(|value| ![1, 5, 10, 20, 30, 60].contains(&value)) {
            return Err(Error::InvalidPreferences("nag_interval_minutes"));
        }
        if confirmation_check_minutes.is_some_and(|value| ![0, 20].contains(&value)) {
            return Err(Error::InvalidPreferences("confirmation_check_minutes"));
        }
        Ok(self
            .repository
            .set_preferences(
                user_id,
                language.map(str::to_owned),
                nag_interval_minutes,
                confirmation_check_minutes,
            )
            .await?)
    }

    // ---- users ---------------------------------------------------------------------------

    pub async fn user(&self, user_id: i64) -> Result<Option<User>> {
        Ok(self.repository.user(user_id).await?)
    }

    pub async fn registered(&self, user_id: i64) -> Result<User> {
        self.user(user_id).await?.ok_or(Error::NotRegistered)
    }

    /// Registers the user or changes their timezone.
    pub async fn set_timezone(&self, profile: Profile, tz: UserTz) -> Result<(User, Registration)> {
        let user = User {
            id: profile.id,
            first_name: profile.first_name,
            last_name: profile.last_name,
            tz,
        };
        let registration = self.repository.upsert_user(user.clone()).await?;
        Ok((user, registration))
    }

    /// Deletes the user and all their data. Returns whether they were registered.
    pub async fn unregister(&self, user_id: i64) -> Result<bool> {
        Ok(self.repository.delete_user(user_id).await?)
    }

    // ---- reminders -----------------------------------------------------------------------

    pub async fn list(&self, user_id: i64) -> Result<Vec<Reminder>> {
        Ok(self.repository.reminders_for_user(user_id).await?)
    }

    pub async fn reminder(&self, user_id: i64, reminder_id: i64) -> Result<Reminder> {
        self.repository
            .reminder(user_id, reminder_id)
            .await?
            .ok_or(Error::NotFound)
    }

    pub async fn create(
        &self,
        user: &User,
        input: ReminderInput<'_>,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let description = description(input.description)?;
        let due_at = first_due(input.at, input.recurrence, user.tz, now)?;
        Ok(self
            .repository
            .add_reminder_with_anchor(
                user.id,
                description,
                due_at,
                input.recurrence,
                Some(input.at),
            )
            .await?)
    }

    pub async fn create_with_settings(
        &self,
        user: &User,
        input: ReminderInput<'_>,
        settings: ReminderSettings,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        validate_settings(&settings)?;
        let write = ReminderWrite {
            description: description(input.description)?,
            due_at: first_due(input.at, input.recurrence, user.tz, now)?,
            recurrence: input.recurrence,
            anchor_local: Some(input.at),
            settings,
        };
        Ok(self.repository.insert_reminder(user.id, write).await?)
    }

    /// Validate the whole edit before atomically writing text, settings and schedule.
    pub async fn edit(
        &self,
        user: &User,
        reminder_id: i64,
        text: &str,
        schedule: Option<(NaiveDateTime, Recurrence)>,
        settings: Option<ReminderSettings>,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let description = Some(description(text)?);
        if let Some(settings) = &settings {
            validate_settings(settings)?;
        }
        let current = self.reminder(user.id, reminder_id).await?;
        let schedule = match schedule {
            Some((at, recurrence))
                if at != user.tz.local(current.due_at) || recurrence != current.recurrence =>
            {
                Some(ReminderSchedule {
                    due_at: first_due(at, recurrence, user.tz, now)?,
                    recurrence,
                    anchor_local: Some(at),
                })
            }
            _ => None,
        };
        self.repository
            .patch_reminder(
                user.id,
                reminder_id,
                ReminderChanges {
                    description,
                    schedule,
                    settings,
                },
            )
            .await?
            .ok_or(Error::NotFound)
    }

    pub async fn history(
        &self,
        user_id: i64,
        before: Option<i64>,
        limit: usize,
    ) -> Result<HistoryPage> {
        Ok(self
            .repository
            .completion_history(user_id, before, limit.clamp(1, 50))
            .await?)
    }

    pub async fn completion_for_source(
        &self,
        user_id: i64,
        nagging_id: i64,
    ) -> Result<Option<Completion>> {
        Ok(self
            .repository
            .completion_for_source(user_id, nagging_id)
            .await?)
    }

    pub async fn undo_completion(
        &self,
        user_id: i64,
        completion_id: i64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        match self
            .repository
            .undo_completion(user_id, completion_id, now)
            .await?
        {
            UndoResult::Restored => Ok(true),
            UndoResult::AlreadyRestored => Ok(false),
            UndoResult::Unavailable => Err(Error::CompletionUnavailable),
            UndoResult::NotFound => Err(Error::NotFound),
        }
    }

    /// Changes a reminder's time and repeat, and its description unless `description` is
    /// `None`.
    pub async fn update(
        &self,
        user: &User,
        reminder_id: i64,
        description: Option<&str>,
        at: NaiveDateTime,
        recurrence: Recurrence,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let description = description.map(self::description).transpose()?;
        let current = self.reminder(user.id, reminder_id).await?;
        if at == user.tz.local(current.due_at) && recurrence == current.recurrence {
            return self
                .repository
                .update_reminder_description(user.id, reminder_id, description)
                .await?
                .ok_or(Error::NotFound);
        }
        let due_at = first_due(at, recurrence, user.tz, now)?;
        self.repository
            .update_reminder_with_anchor(
                user.id,
                reminder_id,
                description,
                due_at,
                recurrence,
                Some(at),
            )
            .await?
            .ok_or(Error::NotFound)
    }

    /// Preserve an absolute duration's first instant through an ambiguous local clock.
    pub async fn update_at(
        &self,
        user: &User,
        reminder_id: i64,
        description: Option<&str>,
        at: DateTime<Utc>,
        recurrence: Recurrence,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let due_at = first_instant(at, recurrence, user.tz, now)?;
        let description = description.map(self::description).transpose()?;
        self.repository
            .update_reminder_with_anchor(
                user.id,
                reminder_id,
                description,
                due_at,
                recurrence,
                Some(user.tz.local(at)),
            )
            .await?
            .ok_or(Error::NotFound)
    }

    /// Explicit text-only edits preserve the schedule even if a reminder fired while its
    /// edit form was open. The presentation layer knows whether schedule fields changed.
    #[cfg(test)]
    pub async fn rename(&self, user_id: i64, reminder_id: i64, text: &str) -> Result<Reminder> {
        self.repository
            .update_reminder_description(user_id, reminder_id, Some(description(text)?))
            .await?
            .ok_or(Error::NotFound)
    }

    /// Deletes a reminder and stops its nagging; returns what was deleted.
    pub async fn delete(&self, user_id: i64, reminder_id: i64) -> Result<Reminder> {
        let reminder = self.reminder(user_id, reminder_id).await?;
        self.repository
            .delete_reminder(user_id, reminder_id)
            .await?;
        Ok(reminder)
    }

    pub async fn delete_all(&self, user_id: i64) -> Result<usize> {
        Ok(self.repository.delete_all_reminders(user_id).await?)
    }

    // ---- drafts (text waiting for a time) ------------------------------------------------

    pub async fn start_draft(&self, user_id: i64, text: &str, now: DateTime<Utc>) -> Result<Draft> {
        Ok(self
            .repository
            .create_draft(user_id, description(text)?, now)
            .await?)
    }

    pub async fn draft(&self, user_id: i64, draft_id: i64) -> Result<Draft> {
        self.repository
            .draft(user_id, draft_id)
            .await?
            .ok_or(Error::NotFound)
    }

    /// The user's draft if it was started within `window`.
    pub async fn pending_draft(
        &self,
        user_id: i64,
        window: Duration,
        now: DateTime<Utc>,
    ) -> Result<Option<Draft>> {
        Ok(self.repository.pending_draft(user_id, now - window).await?)
    }

    pub async fn cancel_draft(&self, user_id: i64, draft_id: i64) -> Result<()> {
        self.repository.delete_draft(user_id, draft_id).await?;
        Ok(())
    }

    /// Turns a draft into a reminder. `NotFound` if it was already saved or cancelled.
    pub async fn save_draft(
        &self,
        user: &User,
        draft_id: i64,
        at: NaiveDateTime,
        recurrence: Recurrence,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let due_at = first_due(at, recurrence, user.tz, now)?;
        self.repository
            .save_draft_with_anchor(user.id, draft_id, due_at, recurrence, Some(at))
            .await?
            .ok_or(Error::NotFound)
    }

    /// Keep the exact first instant and anchor future repeats in the local timezone.
    pub async fn save_draft_at(
        &self,
        user: &User,
        draft_id: i64,
        at: DateTime<Utc>,
        recurrence: Recurrence,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let due_at = first_instant(at, recurrence, user.tz, now)?;
        self.repository
            .save_draft_with_anchor(
                user.id,
                draft_id,
                due_at,
                recurrence,
                Some(user.tz.local(at)),
            )
            .await?
            .ok_or(Error::NotFound)
    }

    // ---- naggings ------------------------------------------------------------------------

    /// All unfinished occurrences, including those explicitly snoozed into the future.
    pub async fn pending(&self, user_id: i64) -> Result<Vec<Nagging>> {
        Ok(self.repository.naggings_for_user(user_id).await?)
    }

    /// Records completion and optionally schedules one check. Returns `false` if already done.
    #[cfg(test)]
    pub async fn done(&self, user_id: i64, nagging_id: i64) -> Result<bool> {
        self.done_at(user_id, nagging_id, Utc::now()).await
    }

    pub async fn done_at(&self, user_id: i64, nagging_id: i64, now: DateTime<Utc>) -> Result<bool> {
        Ok(self
            .repository
            .complete_nagging(user_id, nagging_id, now)
            .await?)
    }

    /// Stops naggings by reminder id, for buttons sent by the .NET bot.
    pub async fn done_legacy(&self, user_id: i64, reminder_id: i64) -> Result<bool> {
        Ok(self
            .repository
            .acknowledge_reminder(user_id, reminder_id)
            .await?)
    }

    /// Postpones a nagging; returns when it will nag again, or `None` if it was already done.
    pub async fn snooze(
        &self,
        user: &User,
        nagging_id: i64,
        snooze: Snooze,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>> {
        let until = match snooze {
            Snooze::TenMinutes => now + Duration::minutes(10),
            Snooze::OneHour => now + Duration::hours(1),
            Snooze::TomorrowMorning => {
                let tomorrow = user.tz.local(now).date() + Duration::days(1);
                user.tz.utc(tomorrow.and_time(SNOOZE_MORNING))
            }
        };
        let snoozed = self
            .repository
            .snooze_nagging(user.id, nagging_id, until)
            .await?;
        Ok(snoozed.then_some(until))
    }

    // ---- calendar feed -------------------------------------------------------------------

    /// The user's feed token; `reset` replaces it, revoking the old feed URL.
    pub async fn calendar_token(&self, user_id: i64, reset: bool) -> Result<String> {
        self.repository
            .calendar_token(user_id, reset)
            .await?
            .ok_or(Error::NotRegistered)
    }

    /// The feed owner and their reminders, if the token is valid.
    pub async fn calendar_feed(&self, token: &str) -> Result<Option<(User, Vec<Reminder>)>> {
        let Some(user) = self
            .repository
            .user_by_calendar_token(token.to_owned())
            .await?
        else {
            return Ok(None);
        };
        let reminders = self.repository.reminders_for_user(user.id).await?;
        Ok(Some((user, reminders)))
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

fn validate_settings(settings: &ReminderSettings) -> Result<()> {
    if settings
        .nag_interval_minutes
        .is_some_and(|value| ![1, 5, 10, 20, 30, 60].contains(&value))
    {
        return Err(Error::InvalidPreferences("nag_interval_minutes"));
    }
    if settings
        .confirmation_check_minutes
        .is_some_and(|value| ![0, 20].contains(&value))
    {
        return Err(Error::InvalidPreferences("confirmation_check_minutes"));
    }
    Ok(())
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
