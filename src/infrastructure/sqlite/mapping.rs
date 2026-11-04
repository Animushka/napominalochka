//! SQLite row and timestamp conversion and transaction helpers.
use super::{REMINDER_COLUMNS, TIME_FORMAT};
use crate::application::ports::StorageResult;
use crate::domain::{
    Draft, Nagging, Preferences, Recurrence, Reminder, ReminderSettings, User, UserTz, UtcOffset,
};
use crate::domain::{ReminderId, UserId};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{OptionalExtension, Row, Transaction};

/// 256 random bits, hex encoded.
pub(super) fn new_token() -> StorageResult<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no secure randomness: {e}"))?;
    Ok(hex::encode(bytes))
}

pub(super) fn reminder_by_id(
    tx: &Transaction<'_>,
    reminder_id: ReminderId,
) -> rusqlite::Result<Option<Reminder>> {
    tx.query_row(
        &format!("SELECT {REMINDER_COLUMNS} FROM reminders WHERE reminder_id = ?1"),
        [reminder_id],
        reminder_from_row,
    )
    .optional()
}

pub(super) fn user_tz(tx: &Transaction<'_>, user_id: UserId) -> rusqlite::Result<Option<UserTz>> {
    tx.query_row(
        "SELECT utc_offset_minutes, timezone FROM users WHERE user_id = ?1",
        [user_id],
        |row| tz_from_columns(row, 0, 1),
    )
    .optional()
}

pub(super) fn preferences_from_row(row: &Row<'_>) -> rusqlite::Result<Preferences> {
    Ok(Preferences {
        language: row.get(0)?,
        nag_interval_minutes: row.get(1)?,
        confirmation_check_minutes: row.get(2)?,
    })
}

pub(super) fn user_from_row(row: &Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: row.get(0)?,
        first_name: row.get(1)?,
        last_name: row.get(2)?,
        tz: tz_from_columns(row, 3, 4)?,
    })
}

/// A named timezone takes precedence; the offset column is the fallback.
pub(super) fn tz_from_columns(
    row: &Row<'_>,
    offset_index: usize,
    name_index: usize,
) -> rusqlite::Result<UserTz> {
    let name: Option<String> = row.get(name_index)?;
    if let Some(tz) = name.and_then(|name| name.parse().ok()) {
        return Ok(UserTz::Named(tz));
    }
    let minutes: i32 = row.get(offset_index)?;
    UtcOffset::from_minutes(minutes)
        .map(UserTz::Fixed)
        .ok_or_else(|| invalid_column(offset_index, "UTC offset out of range"))
}

pub(super) fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<Draft> {
    Ok(Draft {
        id: row.get(0)?,
        user_id: row.get(1)?,
        text: row.get(2)?,
        created_at: row.get::<_, DbTime>(3)?.0,
    })
}

pub(super) fn reminder_from_row(row: &Row<'_>) -> rusqlite::Result<Reminder> {
    let recurrence: i64 = row.get(4)?;
    Ok(Reminder {
        id: row.get(0)?,
        user_id: row.get(1)?,
        description: row.get(2)?,
        due_at: row.get::<_, DbTime>(3)?.0,
        recurrence: Recurrence::from_code(recurrence)
            .ok_or_else(|| invalid_column(4, "unknown recurrence"))?,
        anchor_local: row.get::<_, Option<DbLocalTime>>(5)?.map(|time| time.0),
        settings: ReminderSettings {
            nag_interval_minutes: row.get(6)?,
            confirmation_check_minutes: row.get(7)?,
            voice_enabled: row.get(8)?,
        },
    })
}

pub(super) fn nagging_from_row(row: &Row<'_>) -> rusqlite::Result<Nagging> {
    Ok(Nagging {
        id: row.get(0)?,
        reminder_id: row.get(1)?,
        user_id: row.get(2)?,
        description: row.get(3)?,
        next_at: row.get::<_, DbTime>(4)?.0,
    })
}

pub(super) fn invalid_column(index: usize, message: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Integer, message.into())
}

/// A UTC timestamp stored as `TIME_FORMAT` text. That format sorts lexically, so SQL range
/// comparisons on these columns are correct.
pub(super) struct DbTime(pub(super) DateTime<Utc>);

/// Local wall-clock schedule anchor; deliberately has no UTC offset.
pub(super) struct DbLocalTime(pub(super) NaiveDateTime);

impl ToSql for DbLocalTime {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.format(TIME_FORMAT).to_string()))
    }
}

impl FromSql for DbLocalTime {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        NaiveDateTime::parse_from_str(value.as_str()?, TIME_FORMAT)
            .map(DbLocalTime)
            .map_err(|error| FromSqlError::Other(Box::new(error)))
    }
}

impl ToSql for DbTime {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.format(TIME_FORMAT).to_string()))
    }
}

impl FromSql for DbTime {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        NaiveDateTime::parse_from_str(text, TIME_FORMAT)
            .map(|naive| DbTime(naive.and_utc()))
            .map_err(|e| FromSqlError::Other(Box::new(e)))
    }
}
