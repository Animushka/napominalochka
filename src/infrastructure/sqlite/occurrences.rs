//! SQLite operations for occurrences.
use super::Db;
use super::mapping::{DbLocalTime, DbTime, reminder_by_id, reminder_from_row, user_tz};
use crate::domain::{Nagging, Reminder};
use chrono::{DateTime, Utc};
use rusqlite::params;

impl Db {
    pub(crate) async fn due_reminders(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Reminder>> {
        self.call(move |conn| {
            conn.prepare(
                "SELECT reminder_id, user_id, description, due_at, recurrence, anchor_local, nag_interval_minutes, confirmation_check_minutes, voice_enabled
                 FROM reminders WHERE due_at <= ?1 ORDER BY due_at, reminder_id",
            )?
            .query_map([DbTime(now)], reminder_from_row)?
            .collect()
        })
        .await
    }

    /// Consumes a due reminder: starts a nagging for it (replacing one left over from the
    /// previous occurrence) and either reschedules the reminder or, if it is one-off, deletes it.
    ///
    /// Returns `None` if the reminder no longer exists or is no longer due.
    pub(crate) async fn fire_reminder(
        &self,
        reminder_id: i64,
        now: DateTime<Utc>,
        first_nag_at: DateTime<Utc>,
    ) -> anyhow::Result<Option<Nagging>> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let Some(reminder) = reminder_by_id(&tx, reminder_id)? else {
                return Ok(None);
            };
            // Another scheduler pass may already have rescheduled this occurrence.
            if reminder.due_at > now {
                return Ok(None);
            }

            tx.execute("DELETE FROM naggings WHERE reminder_id = ?1", [reminder.id])?;
            tx.execute(
                "INSERT INTO naggings (reminder_id, user_id, description, next_at, scheduled_at, nag_interval_minutes, confirmation_check_minutes, voice_enabled) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![reminder.id, reminder.user_id, reminder.description, DbTime(first_nag_at), DbTime(reminder.due_at), reminder.settings.nag_interval_minutes, reminder.settings.confirmation_check_minutes, reminder.settings.voice_enabled],
            )?;
            let nagging = Nagging {
                id: tx.last_insert_rowid(),
                reminder_id: Some(reminder.id),
                user_id: reminder.user_id,
                description: reminder.description.clone(),
                next_at: first_nag_at,
            };

            tx.execute(
                "INSERT INTO yandex_jobs (nagging_id, user_id, generation, scenario_id, created_at)
                 SELECT ?1, user_id, generation, scenario_id, ?2 FROM yandex_connections
                 WHERE user_id = ?3 AND encrypted_tokens IS NOT NULL AND enabled = 1
                 AND requires_reconnect = 0 AND scenario_id IS NOT NULL AND ?4 = 1",
                params![nagging.id, now.timestamp(), reminder.user_id, reminder.settings.voice_enabled],
            )?;

            let tz = user_tz(&tx, reminder.user_id)?;
            let anchor = reminder.anchor_local.or_else(|| tz.map(|tz| tz.local(reminder.due_at)));
            let next = tz.and_then(|tz| match reminder.anchor_local {
                Some(anchor) => reminder.recurrence.next_after_anchored(reminder.due_at, now, tz, anchor),
                None => reminder.recurrence.next_after(reminder.due_at, now, tz),
            });
            match next {
                Some(next) => tx.execute(
                    "UPDATE reminders SET due_at = ?1, anchor_local = ?3, last_fired_nagging_id = ?4 WHERE reminder_id = ?2",
                    params![DbTime(next), reminder.id, anchor.map(DbLocalTime), nagging.id],
                )?,
                None => tx.execute("DELETE FROM reminders WHERE reminder_id = ?1", [reminder.id])?,
            };

            tx.commit()?;
            Ok(Some(nagging))
        })
        .await
    }
}
