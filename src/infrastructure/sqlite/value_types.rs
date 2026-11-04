//! SQLite representations of domain values; domain code never imports the SQL driver.
use crate::domain::{
    CompletionId, ConfirmationId, DraftId, NaggingId, ReminderId, UserId, VoiceJobId,
};
use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
macro_rules! identity_sql {
    ($($name:ident),+ $(,)?) => { $(
        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> { self.0.to_sql() }
        }
        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> { i64::column_result(value).map(Self) }
        }
    )+ };
}
identity_sql!(
    UserId,
    ReminderId,
    DraftId,
    NaggingId,
    CompletionId,
    ConfirmationId,
    VoiceJobId
);

use crate::domain::{ConfirmationDelay, Language, NagInterval};
use rusqlite::types::FromSqlError;
impl ToSql for Language {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.code().to_sql()
    }
}
impl FromSql for Language {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|error| FromSqlError::Other(Box::new(error)))
    }
}
macro_rules! interval_sql {
    ($($ty:ty),+) => { $(
        impl ToSql for $ty {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> { Ok(self.minutes().into()) }
        }
        impl FromSql for $ty {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                i64::column_result(value)?.try_into().map_err(|error| FromSqlError::Other(Box::new(error)))
            }
        }
    )+ };
}
interval_sql!(NagInterval, ConfirmationDelay);
