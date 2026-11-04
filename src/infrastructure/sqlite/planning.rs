//! Planning persistence implementation.
use super::Db;
use crate::application::ports::StorageResult;
use crate::application::ports::planning::PlanningRepository;
use crate::domain::ReminderId;
use crate::domain::{Nagging, Reminder};
use chrono::{DateTime, Utc};

impl PlanningRepository for Db {
    async fn due_reminders(&self, now: DateTime<Utc>) -> StorageResult<Vec<Reminder>> {
        Db::due_reminders(self, now).await
    }
    async fn fire_reminder(
        &self,
        reminder_id: ReminderId,
        now: DateTime<Utc>,
        first_nag_at: DateTime<Utc>,
    ) -> StorageResult<Option<Nagging>> {
        Db::fire_reminder(self, reminder_id, now, first_nag_at).await
    }
    async fn delete_drafts_before(&self, cutoff: DateTime<Utc>) -> StorageResult<usize> {
        Db::delete_drafts_before(self, cutoff).await
    }
    async fn maintain_storage(&self) -> StorageResult<()> {
        Db::vacuum(self).await
    }
}
