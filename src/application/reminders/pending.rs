//! Reminder pending use cases.
use super::{ReminderService, Result, SNOOZE_MORNING};
use crate::domain::{Nagging, Snooze, User};
use chrono::{DateTime, Duration, Utc};

impl ReminderService {
    /// All unfinished occurrences, including those explicitly snoozed into the future.
    pub(crate) async fn pending(&self, user_id: i64) -> Result<Vec<Nagging>> {
        Ok(self.repository.naggings_for_user(user_id).await?)
    }

    /// Records completion and optionally schedules one check. Returns `false` if already done.
    #[cfg(test)]
    pub(crate) async fn done(&self, user_id: i64, nagging_id: i64) -> Result<bool> {
        self.done_at(user_id, nagging_id, Utc::now()).await
    }

    pub(crate) async fn done_at(
        &self,
        user_id: i64,
        nagging_id: i64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(self
            .repository
            .complete_nagging(user_id, nagging_id, now)
            .await?)
    }

    /// Stops naggings by reminder id, for buttons sent by the .NET bot.
    pub(crate) async fn done_legacy(&self, user_id: i64, reminder_id: i64) -> Result<bool> {
        Ok(self
            .repository
            .acknowledge_reminder(user_id, reminder_id)
            .await?)
    }

    /// Postpones a nagging; returns when it will nag again, or `None` if it was already done.
    pub(crate) async fn snooze(
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
}
