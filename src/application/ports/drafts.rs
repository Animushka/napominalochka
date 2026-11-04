//! Drafts persistence contract.
use super::StorageResult;
use crate::domain::{Draft, DraftId, Recurrence, Reminder, UserId};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::future::BoxFuture;

pub(crate) trait DraftRepository: Send + Sync + 'static {
    fn create_draft(
        &self,
        user_id: UserId,
        text: String,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, StorageResult<Draft>>;
    fn draft(
        &self,
        user_id: UserId,
        draft_id: DraftId,
    ) -> BoxFuture<'_, StorageResult<Option<Draft>>>;
    fn pending_draft(
        &self,
        user_id: UserId,
        since: DateTime<Utc>,
    ) -> BoxFuture<'_, StorageResult<Option<Draft>>>;
    fn delete_draft(
        &self,
        user_id: UserId,
        draft_id: DraftId,
    ) -> BoxFuture<'_, StorageResult<bool>>;
    fn save_draft_with_anchor(
        &self,
        user_id: UserId,
        draft_id: DraftId,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, StorageResult<Option<Reminder>>>;
}
