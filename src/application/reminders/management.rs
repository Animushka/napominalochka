//! Reminder creation, editing and deletion use cases.
use super::{Error, ReminderInput, ReminderService, Result, description, first_due, first_instant};
use crate::domain::{
    Completion, HistoryPage, Recurrence, Reminder, ReminderChanges, ReminderSchedule,
    ReminderSettings, ReminderWrite, UndoResult, User,
};
use crate::domain::{CompletionId, NaggingId, ReminderId, UserId};
use chrono::{DateTime, NaiveDateTime, Utc};

impl ReminderService {
    pub(crate) async fn list(&self, user_id: UserId) -> Result<Vec<Reminder>> {
        Ok(self.repository.reminders_for_user(user_id).await?)
    }

    pub(crate) async fn reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
    ) -> Result<Reminder> {
        self.repository
            .reminder(user_id, reminder_id)
            .await?
            .ok_or(Error::NotFound)
    }

    pub(crate) async fn create(
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

    pub(crate) async fn create_with_settings(
        &self,
        user: &User,
        input: ReminderInput<'_>,
        settings: ReminderSettings,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
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
    pub(crate) async fn edit(
        &self,
        user: &User,
        reminder_id: ReminderId,
        text: &str,
        schedule: Option<(NaiveDateTime, Recurrence)>,
        settings: Option<ReminderSettings>,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let description = Some(description(text)?);
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

    pub(crate) async fn history(
        &self,
        user_id: UserId,
        before: Option<CompletionId>,
        limit: usize,
    ) -> Result<HistoryPage> {
        Ok(self
            .repository
            .completion_history(user_id, before, limit.clamp(1, 50))
            .await?)
    }

    pub(crate) async fn completion_for_source(
        &self,
        user_id: UserId,
        nagging_id: NaggingId,
    ) -> Result<Option<Completion>> {
        Ok(self
            .repository
            .completion_for_source(user_id, nagging_id)
            .await?)
    }

    pub(crate) async fn undo_completion(
        &self,
        user_id: UserId,
        completion_id: CompletionId,
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
    pub(crate) async fn update(
        &self,
        user: &User,
        reminder_id: ReminderId,
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
    pub(crate) async fn update_at(
        &self,
        user: &User,
        reminder_id: ReminderId,
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
    pub(crate) async fn rename(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
        text: &str,
    ) -> Result<Reminder> {
        self.repository
            .update_reminder_description(user_id, reminder_id, Some(description(text)?))
            .await?
            .ok_or(Error::NotFound)
    }

    /// Deletes a reminder and stops its nagging; returns what was deleted.
    pub(crate) async fn delete(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
    ) -> Result<Reminder> {
        let reminder = self.reminder(user_id, reminder_id).await?;
        self.repository
            .delete_reminder(user_id, reminder_id)
            .await?;
        Ok(reminder)
    }

    pub(crate) async fn delete_all(&self, user_id: UserId) -> Result<usize> {
        Ok(self.repository.delete_all_reminders(user_id).await?)
    }
}
