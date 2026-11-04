//! Reminders persistence contract.
use super::StorageResult;
use crate::domain::{Recurrence, Reminder, ReminderChanges, ReminderId, ReminderWrite, UserId};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::future::BoxFuture;

pub(crate) trait ReminderStore: Send + Sync + 'static {
    fn insert_reminder(
        &self,
        user_id: UserId,
        write: ReminderWrite,
    ) -> BoxFuture<'_, StorageResult<Reminder>>;
    fn patch_reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
        changes: ReminderChanges,
    ) -> BoxFuture<'_, StorageResult<Option<Reminder>>>;
    fn reminders_for_user(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<Vec<Reminder>>>;
    fn reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
    ) -> BoxFuture<'_, StorageResult<Option<Reminder>>>;
    fn add_reminder_with_anchor(
        &self,
        user_id: UserId,
        description: String,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, StorageResult<Reminder>>;
    fn update_reminder_with_anchor(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
        description: Option<String>,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, StorageResult<Option<Reminder>>>;
    fn update_reminder_description(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
        description: Option<String>,
    ) -> BoxFuture<'_, StorageResult<Option<Reminder>>>;
    fn delete_reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
    ) -> BoxFuture<'_, StorageResult<bool>>;
    fn delete_all_reminders(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<usize>>;
}
