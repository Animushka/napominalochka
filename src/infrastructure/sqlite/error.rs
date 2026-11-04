//! Translate driver failures once, at the persistence boundary.
use crate::application::ports::StorageError;
use rusqlite::{Error, ErrorCode};

impl From<Error> for StorageError {
    fn from(error: Error) -> Self {
        match &error {
            Error::SqliteFailure(inner, _) => match inner.code {
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => Self::Busy(Box::new(error)),
                ErrorCode::ConstraintViolation => Self::Conflict(Box::new(error)),
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => {
                    Self::InvalidData(Box::new(error))
                }
                ErrorCode::CannotOpen | ErrorCode::ReadOnly | ErrorCode::SystemIoFailure => {
                    Self::Unavailable(Box::new(error))
                }
                _ => Self::Internal(Box::new(error)),
            },
            Error::FromSqlConversionFailure(..)
            | Error::InvalidColumnType(..)
            | Error::IntegralValueOutOfRange(..) => Self::InvalidData(Box::new(error)),
            _ => Self::Internal(Box::new(error)),
        }
    }
}
impl From<anyhow::Error> for StorageError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error.into_boxed_dyn_error())
    }
}

#[cfg(test)]
mod tests {
    use super::StorageError;
    use crate::domain::UserId;
    use crate::infrastructure::sqlite::Db;

    #[tokio::test]
    async fn storage_preserves_constraint_and_decoding_failures() {
        let db = Db::open_in_memory().unwrap();
        let failure = db
            .call(|conn| {
                conn.execute(
                    "INSERT INTO preferences(user_id,language) VALUES (1,'invalid')",
                    [],
                )
            })
            .await
            .unwrap_err();
        assert!(matches!(failure, StorageError::Conflict(_)));
        db.call(|conn| conn.execute_batch(
            "PRAGMA ignore_check_constraints=ON; INSERT INTO preferences(user_id,nag_interval_minutes) VALUES (1,99);"
        )).await.unwrap();
        assert!(matches!(
            db.preferences(UserId(1)).await,
            Err(StorageError::InvalidData(_))
        ));
    }
}
