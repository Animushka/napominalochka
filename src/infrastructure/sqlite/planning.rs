//! Planning persistence implementation.
use super::Db;
use crate::application::ports::planning::PlanningRepository;
use crate::domain::{Nagging, Reminder};
use chrono::{DateTime, Utc};

impl PlanningRepository for Db {
    async fn due_reminders(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Reminder>> {
        Db::due_reminders(self, now).await
    }
    async fn fire_reminder(
        &self,
        reminder_id: i64,
        now: DateTime<Utc>,
        first_nag_at: DateTime<Utc>,
    ) -> anyhow::Result<Option<Nagging>> {
        Db::fire_reminder(self, reminder_id, now, first_nag_at).await
    }
    async fn delete_drafts_before(&self, cutoff: DateTime<Utc>) -> anyhow::Result<usize> {
        Db::delete_drafts_before(self, cutoff).await
    }
    async fn maintain_storage(&self) -> anyhow::Result<()> {
        Db::vacuum(self).await
    }
}
