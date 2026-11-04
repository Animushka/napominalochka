//! SQLite operations for pending.
use super::mapping::{DbTime, nagging_from_row};
use super::{Db, history};
use crate::application::ports::StorageResult;
#[cfg(test)]
use crate::domain::Confirmation;
use crate::domain::Nagging;
use crate::domain::{NaggingId, ReminderId, UserId};
use chrono::{DateTime, Utc};
use rusqlite::params;

impl Db {
    /// Pending occurrences remain visible after their one-off reminder has been consumed
    /// and while snoozed. Never join reminders here: one-off occurrences have no parent.
    pub(crate) async fn naggings_for_user(&self, user_id: UserId) -> StorageResult<Vec<Nagging>> {
        self.call(move |conn| {
            conn.prepare(
                "SELECT nagging_id, reminder_id, user_id, description, next_at
                 FROM naggings WHERE user_id = ?1 ORDER BY next_at, nagging_id",
            )?
            .query_map([user_id], nagging_from_row)?
            .collect()
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn due_naggings(&self, now: DateTime<Utc>) -> StorageResult<Vec<Nagging>> {
        self.call(move |conn| {
            conn.prepare(
                "SELECT nagging_id, reminder_id, user_id, description, next_at
                 FROM naggings WHERE next_at <= ?1 ORDER BY next_at, nagging_id",
            )?
            .query_map([DbTime(now)], nagging_from_row)?
            .collect()
        })
        .await
    }

    /// Claims one due delivery only while its observed schedule is still current. A stale
    /// scheduler snapshot must not overwrite a snooze or send a completed/deleted occurrence.
    #[cfg(test)]
    pub(crate) async fn claim_nagging(
        &self,
        observed: &Nagging,
        now: DateTime<Utc>,
        next_at: DateTime<Utc>,
    ) -> StorageResult<bool> {
        let (nagging_id, user_id, observed_at) = (observed.id, observed.user_id, observed.next_at);
        self.call(move |conn| {
            Ok(conn.execute(
                "UPDATE naggings SET next_at = ?1
                 WHERE nagging_id = ?2 AND user_id = ?3 AND next_at = ?4 AND next_at <= ?5",
                params![
                    DbTime(next_at),
                    nagging_id,
                    user_id,
                    DbTime(observed_at),
                    DbTime(now)
                ],
            )? > 0)
        })
        .await
    }

    /// Test helper for the acknowledgement contract; service calls use an explicit clock.
    #[cfg(test)]
    pub(crate) async fn acknowledge_nagging(
        &self,
        user_id: UserId,
        nagging_id: NaggingId,
    ) -> StorageResult<bool> {
        self.complete_nagging(user_id, nagging_id, Utc::now()).await
    }

    /// Completion history and its optional one-time check are recorded atomically.
    pub(crate) async fn complete_nagging(
        &self,
        user_id: UserId,
        nagging_id: NaggingId,
        now: DateTime<Utc>,
    ) -> StorageResult<bool> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let changed = history::complete(&tx, user_id, nagging_id, now)?;
            tx.commit()?;
            Ok(changed)
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn due_confirmations(
        &self,
        now: DateTime<Utc>,
    ) -> StorageResult<Vec<Confirmation>> {
        self.call(move |conn| conn.prepare("SELECT confirmation_id, user_id, description, due_at, completion_id FROM confirmations WHERE due_at <= ?1 ORDER BY due_at, confirmation_id")?
            .query_map([DbTime(now)], |row| Ok(Confirmation { id: row.get(0)?, user_id: row.get(1)?, description: row.get(2)?, due_at: row.get::<_, DbTime>(3)?.0, completion_id: row.get(4)? }))?.collect()).await
    }

    /// Postpones the next nag. Only the user being nagged can snooze it.
    pub(crate) async fn snooze_nagging(
        &self,
        user_id: UserId,
        nagging_id: NaggingId,
        next_at: DateTime<Utc>,
    ) -> StorageResult<bool> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let changed = tx.execute(
                "UPDATE naggings SET next_at = ?1, delivery_claim = NULL, delivery_lease_until = NULL, delivery_attempts = 0 WHERE nagging_id = ?2 AND user_id = ?3",
                params![DbTime(next_at), nagging_id, user_id],
            )? > 0;
            if changed {
                // A claimed worker rechecks the job immediately before its external request.
                // Removing it cancels queued work; an already dispatched request cannot be recalled.
                tx.execute(
                    "DELETE FROM yandex_jobs WHERE nagging_id = ?1 AND user_id = ?2
                     AND state IN ('pending', 'claimed')",
                    params![nagging_id, user_id],
                )?;
            }
            tx.commit()?;
            Ok(changed)
        })
        .await
    }

    /// Stops naggings by reminder id. Handles "Acknowledge" buttons sent by the .NET bot.
    pub(crate) async fn acknowledge_reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
    ) -> StorageResult<bool> {
        let now = Utc::now();
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let ids = tx.prepare("SELECT nagging_id FROM naggings WHERE reminder_id=?1 AND user_id=?2 ORDER BY nagging_id")?
                .query_map(params![reminder_id,user_id], |row| row.get::<_,NaggingId>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut changed = false;
            for id in ids { changed |= history::complete(&tx,user_id,id,now)?; }
            tx.commit()?;
            Ok(changed)
        }).await
    }
}
