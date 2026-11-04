//! Health persistence contract.
use super::StorageResult;
use futures_util::future::BoxFuture;

pub(crate) trait StorageHealth: Send + Sync + 'static {
    fn ping(&self) -> BoxFuture<'_, StorageResult<()>>;
}
