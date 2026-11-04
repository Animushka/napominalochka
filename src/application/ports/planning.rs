//! Persistence needed for planning; implementations own atomic occurrence creation.
use crate::application::ports::StorageResult;
use crate::domain::ReminderId;
use crate::domain::{Nagging, Reminder};
use chrono::{DateTime, Utc};
use std::future::Future;

pub(crate) trait PlanningRepository: Clone + Send + Sync + 'static {
    fn due_reminders(
        &self,
        now: DateTime<Utc>,
    ) -> impl Future<Output = StorageResult<Vec<Reminder>>> + Send;
    fn fire_reminder(
        &self,
        reminder_id: ReminderId,
        now: DateTime<Utc>,
        first_nag_at: DateTime<Utc>,
    ) -> impl Future<Output = StorageResult<Option<Nagging>>> + Send;
    fn delete_drafts_before(
        &self,
        cutoff: DateTime<Utc>,
    ) -> impl Future<Output = StorageResult<usize>> + Send;
    fn maintain_storage(&self) -> impl Future<Output = StorageResult<()>> + Send;
}
