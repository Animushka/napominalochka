//! Users persistence contract.
use super::StorageResult;
use crate::domain::{User, UserId};
use futures_util::future::BoxFuture;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Registration {
    Created,
    Updated,
}

pub(crate) trait UserRepository: Send + Sync + 'static {
    fn user(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<Option<User>>>;
    fn upsert_user(&self, user: User) -> BoxFuture<'_, StorageResult<Registration>>;
    fn delete_user(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<bool>>;
}
