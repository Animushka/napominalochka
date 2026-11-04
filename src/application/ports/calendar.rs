//! Calendar persistence contract.
use super::StorageResult;
use crate::domain::{User, UserId};
use futures_util::future::BoxFuture;

pub(crate) trait CalendarRepository: Send + Sync + 'static {
    fn calendar_token(
        &self,
        user_id: UserId,
        reset: bool,
    ) -> BoxFuture<'_, StorageResult<Option<String>>>;
    fn user_by_calendar_token(&self, token: String) -> BoxFuture<'_, StorageResult<Option<User>>>;
}
