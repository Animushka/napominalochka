//! Durable completion journal and transactional restoration of accidental completion.

use crate::application::ports::StorageResult;
use crate::domain::{CompletionId, NaggingId, UserId};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, Transaction, params};

use super::Db;
use super::mapping::DbTime;
use crate::domain::{Completion, HistoryPage, ReminderSettings, UndoResult};

// Reuse this predicate for rendering availability and for the mutation itself. A
// historical occurrence must never replace an already newer recurring occurrence.
const CAN_UNDO: &str = "h.undone_at IS NULL AND (
    h.reminder_id IS NULL OR (
        EXISTS (SELECT 1 FROM reminders r WHERE r.reminder_id = h.reminder_id AND r.user_id = h.user_id
                AND (r.last_fired_nagging_id IS NULL OR r.last_fired_nagging_id <= h.source_nagging_id))
        AND NOT EXISTS (SELECT 1 FROM naggings n WHERE n.reminder_id = h.reminder_id AND n.user_id = h.user_id)
        AND NOT EXISTS (SELECT 1 FROM completion_history newer WHERE newer.reminder_id = h.reminder_id
                        AND newer.user_id = h.user_id AND newer.source_nagging_id > h.source_nagging_id)
    )
)";

/// Journal insertion, optional follow-up and removal of the pending occurrence are
/// one transaction. It is shared by modern occurrence buttons and legacy buttons.
pub(super) fn complete(
    tx: &Transaction<'_>,
    user_id: UserId,
    nagging_id: NaggingId,
    now: DateTime<Utc>,
) -> rusqlite::Result<bool> {
    let completed = tx.query_row(
        "DELETE FROM naggings WHERE nagging_id=?1 AND user_id=?2 RETURNING description,reminder_id,scheduled_at,nag_interval_minutes,confirmation_check_minutes,voice_enabled",
        params![nagging_id,user_id],
        |row| Ok((row.get::<_,String>(0)?,row.get::<_,Option<crate::domain::ReminderId>>(1)?,row.get::<_,Option<DbTime>>(2)?,
            ReminderSettings { nag_interval_minutes: row.get(3)?,confirmation_check_minutes:row.get(4)?,voice_enabled:row.get(5)? })),
    ).optional()?;
    let Some((description, reminder_id, scheduled_at, settings)) = completed else {
        return Ok(false);
    };
    tx.execute(
        "INSERT INTO completion_history(user_id,source_nagging_id,reminder_id,description,scheduled_at,completed_at,nag_interval_minutes,confirmation_check_minutes,voice_enabled)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![user_id,nagging_id,reminder_id,description,scheduled_at,DbTime(now),settings.nag_interval_minutes,settings.confirmation_check_minutes,settings.voice_enabled],
    )?;
    let completion_id = tx.last_insert_rowid();
    let inherited: crate::domain::ConfirmationDelay = tx
        .query_row(
            "SELECT confirmation_check_minutes FROM preferences WHERE user_id=?1",
            [user_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(crate::domain::ConfirmationDelay::Disabled);
    let delay = settings.confirmation_check_minutes.unwrap_or(inherited);
    if delay == crate::domain::ConfirmationDelay::TwentyMinutes {
        tx.execute(
            "INSERT INTO confirmations(source_nagging_id,reminder_id,user_id,description,due_at,completion_id)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![nagging_id,reminder_id,user_id,description,DbTime(now+Duration::minutes(delay.minutes())),completion_id],
        )?;
    }
    Ok(true)
}

fn completion_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Completion> {
    Ok(Completion {
        id: row.get(0)?,
        user_id: row.get(1)?,
        description: row.get(2)?,
        scheduled_at: row.get::<_, Option<DbTime>>(3)?.map(|time| time.0),
        completed_at: row.get::<_, DbTime>(4)?.0,
        undone_at: row.get::<_, Option<DbTime>>(5)?.map(|time| time.0),
        can_undo: row.get(6)?,
    })
}

impl Db {
    pub(crate) async fn completion_history(
        &self,
        user_id: UserId,
        before: Option<CompletionId>,
        limit: usize,
    ) -> StorageResult<HistoryPage> {
        let limit = limit.clamp(1, 100);
        self.call(move |conn| {
            let sql = format!(
                "SELECT h.completion_id,h.user_id,h.description,h.scheduled_at,h.completed_at,h.undone_at,({CAN_UNDO})
                 FROM completion_history h WHERE h.user_id=?1 AND (?2 IS NULL OR h.completion_id < ?2)
                 ORDER BY h.completion_id DESC LIMIT ?3"
            );
            let mut items: Vec<Completion> = conn.prepare(&sql)?.query_map(
                params![user_id,before,i64::try_from(limit+1).unwrap_or(101)],
                |row| Ok(Completion {
                    id: row.get(0)?, user_id: row.get(1)?, description: row.get(2)?,
                    scheduled_at: row.get::<_, Option<DbTime>>(3)?.map(|at| at.0),
                    completed_at: row.get::<_, DbTime>(4)?.0,
                    undone_at: row.get::<_, Option<DbTime>>(5)?.map(|at| at.0),
                    can_undo: row.get(6)?,
                }),
            )?.collect::<rusqlite::Result<_>>()?;
            let has_more = items.len() > limit;
            items.truncate(limit);
            let next_cursor = if has_more { items.last().map(|item|item.id) } else { None };
            Ok(HistoryPage { items, next_cursor })
        }).await
    }

    pub(crate) async fn completion_for_source(
        &self,
        user_id: UserId,
        nagging_id: NaggingId,
    ) -> StorageResult<Option<Completion>> {
        self.call(move |conn| {
            let sql = format!("SELECT h.completion_id,h.user_id,h.description,h.scheduled_at,h.completed_at,h.undone_at,({CAN_UNDO}) FROM completion_history h WHERE h.user_id=?1 AND h.source_nagging_id=?2");
            conn.query_row(&sql,params![user_id,nagging_id],completion_from_row).optional()
        }).await
    }

    pub(crate) async fn undo_completion(
        &self,
        user_id: UserId,
        completion_id: CompletionId,
        now: DateTime<Utc>,
    ) -> StorageResult<UndoResult> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let sql = format!(
                "SELECT h.description,h.reminder_id,h.scheduled_at,h.undone_at,({CAN_UNDO}),h.nag_interval_minutes,h.confirmation_check_minutes,h.voice_enabled
                 FROM completion_history h WHERE h.user_id=?1 AND h.completion_id=?2"
            );
            let entry = tx.query_row(&sql, params![user_id,completion_id], |row| {
                Ok((row.get::<_,String>(0)?,row.get::<_,Option<crate::domain::ReminderId>>(1)?,row.get::<_,Option<DbTime>>(2)?,
                    row.get::<_,Option<DbTime>>(3)?,row.get::<_,bool>(4)?,ReminderSettings { nag_interval_minutes:row.get(5)?,confirmation_check_minutes:row.get(6)?,voice_enabled:row.get(7)? }))
            }).optional()?;
            let Some((description,reminder_id,scheduled_at,undone_at,allowed,settings)) = entry else {
                return Ok(UndoResult::NotFound);
            };
            if undone_at.is_some() { return Ok(UndoResult::AlreadyRestored); }
            if !allowed { return Ok(UndoResult::Unavailable); }
            // A surviving recurring reminder uses the user's current settings.
            // Consumed one-offs have no parent, so their journal snapshot is authoritative.
            let settings = match reminder_id {
                Some(reminder_id) => tx.query_row(
                    "SELECT nag_interval_minutes,confirmation_check_minutes,voice_enabled FROM reminders WHERE user_id=?1 AND reminder_id=?2",
                    params![user_id,reminder_id],
                    |row| Ok(ReminderSettings { nag_interval_minutes:row.get(0)?,confirmation_check_minutes:row.get(1)?,voice_enabled:row.get(2)? }),
                )?,
                None => settings,
            };
            tx.execute("DELETE FROM confirmations WHERE completion_id=?1 AND user_id=?2", params![completion_id,user_id])?;
            tx.execute(
                "INSERT INTO naggings(reminder_id,user_id,description,next_at,scheduled_at,nag_interval_minutes,confirmation_check_minutes,voice_enabled) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![reminder_id,user_id,description,DbTime(now+Duration::minutes(10)),scheduled_at,settings.nag_interval_minutes,settings.confirmation_check_minutes,settings.voice_enabled],
            )?;
            let restored_id = tx.last_insert_rowid();
            if let Some(reminder_id) = reminder_id {
                tx.execute(
                    "UPDATE reminders SET last_fired_nagging_id=?1 WHERE user_id=?2 AND reminder_id=?3",
                    params![restored_id,user_id,reminder_id],
                )?;
            }
            tx.execute(
                "UPDATE completion_history SET undone_at=?1,restored_nagging_id=?2 WHERE completion_id=?3 AND user_id=?4",
                params![DbTime(now),restored_id,completion_id,user_id],
            )?;
            tx.commit()?;
            Ok(UndoResult::Restored)
        }).await
    }
}

#[cfg(test)]
mod tests;
