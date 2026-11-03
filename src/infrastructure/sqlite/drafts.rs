//! SQLite operations for drafts.
use super::Db;
use super::mapping::{DbLocalTime, DbTime, draft_from_row};
use crate::domain::{Draft, Recurrence, Reminder, ReminderSettings};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{OptionalExtension, params};

impl Db {
    /// Stores text waiting for a time. A user has at most one draft; older ones are replaced.
    pub(crate) async fn create_draft(
        &self,
        user_id: i64,
        text: String,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Draft> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM drafts WHERE user_id = ?1", [user_id])?;
            tx.execute(
                "INSERT INTO drafts (user_id, text, created_at) VALUES (?1, ?2, ?3)",
                params![user_id, text, DbTime(now)],
            )?;
            let draft = Draft {
                id: tx.last_insert_rowid(),
                user_id,
                text,
                created_at: now,
            };
            tx.commit()?;
            Ok(draft)
        })
        .await
    }

    pub(crate) async fn draft(&self, user_id: i64, draft_id: i64) -> anyhow::Result<Option<Draft>> {
        self.call(move |conn| {
            conn.query_row(
                "SELECT draft_id, user_id, text, created_at FROM drafts WHERE draft_id = ?1 AND user_id = ?2",
                [draft_id, user_id],
                draft_from_row,
            )
            .optional()
        })
        .await
    }

    /// The user's draft, if it was created after `since`.
    pub(crate) async fn pending_draft(
        &self,
        user_id: i64,
        since: DateTime<Utc>,
    ) -> anyhow::Result<Option<Draft>> {
        self.call(move |conn| {
            conn.query_row(
                "SELECT draft_id, user_id, text, created_at FROM drafts
                 WHERE user_id = ?1 AND created_at >= ?2
                 ORDER BY draft_id DESC LIMIT 1",
                params![user_id, DbTime(since)],
                draft_from_row,
            )
            .optional()
        })
        .await
    }

    pub(crate) async fn delete_draft(&self, user_id: i64, draft_id: i64) -> anyhow::Result<bool> {
        self.call(move |conn| {
            Ok(conn.execute(
                "DELETE FROM drafts WHERE draft_id = ?1 AND user_id = ?2",
                [draft_id, user_id],
            )? > 0)
        })
        .await
    }

    /// Turns a draft into a reminder. Returns `None` if the draft no longer exists
    /// (already saved or cancelled).
    #[cfg(test)]
    pub(crate) async fn save_draft(
        &self,
        user_id: i64,
        draft_id: i64,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
    ) -> anyhow::Result<Option<Reminder>> {
        self.save_draft_with_anchor(user_id, draft_id, due_at, recurrence, None)
            .await
    }

    pub(crate) async fn save_draft_with_anchor(
        &self,
        user_id: i64,
        draft_id: i64,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> anyhow::Result<Option<Reminder>> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let text: Option<String> = tx
                .query_row(
                    "DELETE FROM drafts WHERE draft_id = ?1 AND user_id = ?2 RETURNING text",
                    [draft_id, user_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(description) = text else {
                return Ok(None);
            };
            tx.execute(
                "INSERT INTO reminders (user_id, description, due_at, recurrence, anchor_local) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![user_id, description, DbTime(due_at), recurrence.code(), anchor_local.map(DbLocalTime)],
            )?;
            let reminder = Reminder { id: tx.last_insert_rowid(), user_id, description, due_at, recurrence, anchor_local, settings: ReminderSettings::default() };
            tx.commit()?;
            Ok(Some(reminder))
        })
        .await
    }

    pub(crate) async fn delete_drafts_before(
        &self,
        cutoff: DateTime<Utc>,
    ) -> anyhow::Result<usize> {
        self.call(move |conn| {
            conn.execute("DELETE FROM drafts WHERE created_at < ?1", [DbTime(cutoff)])
        })
        .await
    }
}
