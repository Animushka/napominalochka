//! Storage failures classified without exposing a concrete SQL driver to use cases.
use std::error::Error;
use thiserror::Error;
type Cause = Box<dyn Error + Send + Sync>;

#[derive(Debug, Error)]
pub(crate) enum StorageError {
    #[error("storage is busy")]
    Busy(#[source] Cause),
    #[error("storage constraint conflict")]
    Conflict(#[source] Cause),
    #[error("invalid stored data")]
    InvalidData(#[source] Cause),
    #[error("storage unavailable")]
    Unavailable(#[source] Cause),
    #[error("storage worker failed")]
    Worker(#[source] Cause),
    #[error("storage operation failed")]
    Internal(#[source] Cause),
}
pub(crate) type StorageResult<T> = Result<T, StorageError>;
