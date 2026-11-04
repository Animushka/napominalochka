//! Pending persistence contract.
use super::StorageResult;
use crate::domain::{Nagging, NaggingId, ReminderId, UserId};
use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;

pub(crate) trait PendingRepository: Send + Sync + 'static {
    fn naggings_for_user(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<Vec<Nagging>>>;
    fn complete_nagging(
        &self,
        user_id: UserId,
        nagging_id: NaggingId,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, StorageResult<bool>>;
    fn acknowledge_reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
    ) -> BoxFuture<'_, StorageResult<bool>>;
    fn snooze_nagging(
        &self,
        user_id: UserId,
        nagging_id: NaggingId,
        next_at: DateTime<Utc>,
    ) -> BoxFuture<'_, StorageResult<bool>>;
}
