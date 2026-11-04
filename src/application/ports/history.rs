//! History persistence contract.
use super::StorageResult;
use crate::domain::{Completion, CompletionId, HistoryPage, NaggingId, UndoResult, UserId};
use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;

pub(crate) trait CompletionRepository: Send + Sync + 'static {
    fn completion_history(
        &self,
        user_id: UserId,
        before: Option<CompletionId>,
        limit: usize,
    ) -> BoxFuture<'_, StorageResult<HistoryPage>>;
    fn completion_for_source(
        &self,
        user_id: UserId,
        nagging_id: NaggingId,
    ) -> BoxFuture<'_, StorageResult<Option<Completion>>>;
    fn undo_completion(
        &self,
        user_id: UserId,
        completion_id: CompletionId,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, StorageResult<UndoResult>>;
}
