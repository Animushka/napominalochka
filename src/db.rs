//! SQLite persistence.
//!
//! A single connection is shared behind a mutex; every call runs on tokio's blocking pool
//! so database I/O never stalls the async runtime.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::model::{
    Confirmation, Draft, Nagging, Preferences, Recurrence, Reminder, User, UserTz, UtcOffset,
};

/// Migrations are applied in order; `PRAGMA user_version` records how many have run.
/// The first one is the schema of the original .NET bot, so existing databases upgrade in place.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/001_initial.sql"),
    include_str!("../migrations/002_offsets_and_scheduling.sql"),
    include_str!("../migrations/003_timezones_and_drafts.sql"),
    include_str!("../migrations/004_calendar_feed.sql"),
    include_str!("../migrations/005_preferences_and_confirmations.sql"),
    include_str!("../migrations/006_yandex.sql"),
    include_str!("../migrations/007_recurrence_anchor.sql"),
];

const USER_COLUMNS: &str = "user_id, first_name, last_name, utc_offset_minutes, timezone";
const REMINDER_COLUMNS: &str =
    "reminder_id, user_id, description, due_at, recurrence, anchor_local";

/// Timestamps are stored as UTC text in the same format the .NET bot used (`DateTime.ToString("s")`).
const TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

/// Whether `upsert_user` created a new user or updated an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    Created,
    Updated,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating database directory {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> anyhow::Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        migrate(&mut conn).context("migrating database schema")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub(crate) async fn call<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        let result = tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut conn)
        })
        .await
        .context("database task panicked")?;
        Ok(result?)
    }

    /// Reading defaults does not register a user or overwrite Telegram's initial language.
    pub async fn preferences(&self, user_id: i64) -> anyhow::Result<Preferences> {
        self.call(move |conn| {
            Ok(conn.query_row(
                "SELECT language, nag_interval_minutes, confirmation_check_minutes FROM preferences WHERE user_id = ?1",
                [user_id], preferences_from_row,
            ).optional()?.unwrap_or_default())
        }).await
    }

    pub async fn initialize_language(&self, user_id: i64, language: String) -> anyhow::Result<()> {
        self.call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO preferences (user_id, language) VALUES (?1, ?2)",
                params![user_id, language],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn set_preferences(
        &self,
        user_id: i64,
        language: Option<String>,
        nag_interval_minutes: Option<i64>,
        confirmation_check_minutes: Option<i64>,
    ) -> anyhow::Result<Preferences> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("INSERT OR IGNORE INTO preferences (user_id) VALUES (?1)", [user_id])?;
            let settings = tx.query_row(
                "UPDATE preferences SET language = COALESCE(?1, language), nag_interval_minutes = COALESCE(?2, nag_interval_minutes), confirmation_check_minutes = COALESCE(?3, confirmation_check_minutes) WHERE user_id = ?4 RETURNING language, nag_interval_minutes, confirmation_check_minutes",
                params![language, nag_interval_minutes, confirmation_check_minutes, user_id], preferences_from_row,
            )?;
            if settings.confirmation_check_minutes == 0 {
                tx.execute("DELETE FROM confirmations WHERE user_id = ?1", [user_id])?;
            }
            tx.commit()?;
            Ok(settings)
        }).await
    }

    // ---- users -------------------------------------------------------------------------

    pub async fn upsert_user(&self, user: User) -> anyhow::Result<Registration> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let exists = tx
                .query_row("SELECT 1 FROM users WHERE user_id = ?1", [user.id], |_| {
                    Ok(())
                })
                .optional()?
                .is_some();
            tx.execute(
                "INSERT INTO users (user_id, first_name, last_name, utc_offset_minutes, timezone)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (user_id) DO UPDATE SET
                     first_name = excluded.first_name,
                     last_name = excluded.last_name,
                     utc_offset_minutes = excluded.utc_offset_minutes,
                     timezone = excluded.timezone",
                params![
                    user.id,
                    user.first_name,
                    user.last_name,
                    user.tz.offset_minutes(Utc::now()),
                    user.tz.name()
                ],
            )?;
            tx.commit()?;
            Ok(if exists {
                Registration::Updated
            } else {
                Registration::Created
            })
        })
        .await
    }

    pub async fn user(&self, user_id: i64) -> anyhow::Result<Option<User>> {
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE user_id = ?1"),
                [user_id],
                user_from_row,
            )
            .optional()
        })
        .await
    }

    /// The user's calendar feed token, created on first use. `reset` replaces it, which
    /// revokes the previous feed URL. Returns `None` if the user is not registered.
    pub async fn calendar_token(
        &self,
        user_id: i64,
        reset: bool,
    ) -> anyhow::Result<Option<String>> {
        let candidate = new_token()?;
        self.call(move |conn| {
            let sql = if reset {
                "UPDATE users SET calendar_token = ?1 WHERE user_id = ?2 RETURNING calendar_token"
            } else {
                "UPDATE users SET calendar_token = COALESCE(calendar_token, ?1) WHERE user_id = ?2
                 RETURNING calendar_token"
            };
            conn.query_row(sql, params![candidate, user_id], |row| row.get(0))
                .optional()
        })
        .await
    }

    pub async fn user_by_calendar_token(&self, token: String) -> anyhow::Result<Option<User>> {
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE calendar_token = ?1"),
                [token],
                user_from_row,
            )
            .optional()
        })
        .await
    }

    /// Removes preferences/drafts even before registration; owned reminder data cascades.
    pub async fn delete_user(&self, user_id: i64) -> anyhow::Result<bool> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM preferences WHERE user_id = ?1", [user_id])?;
            tx.execute("DELETE FROM drafts WHERE user_id = ?1", [user_id])?;
            tx.execute(
                "DELETE FROM yandex_connections WHERE user_id = ?1",
                [user_id],
            )?;
            let removed = tx.execute("DELETE FROM users WHERE user_id = ?1", [user_id])? > 0;
            tx.commit()?;
            Ok(removed)
        })
        .await
    }

    // ---- reminders ---------------------------------------------------------------------

    #[cfg(test)]
    pub async fn add_reminder(
        &self,
        user_id: i64,
        description: String,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
    ) -> anyhow::Result<Reminder> {
        self.add_reminder_with_anchor(user_id, description, due_at, recurrence, None)
            .await
    }

    pub async fn add_reminder_with_anchor(
        &self,
        user_id: i64,
        description: String,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> anyhow::Result<Reminder> {
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO reminders (user_id, description, due_at, recurrence, anchor_local) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![user_id, description, DbTime(due_at), recurrence.to_db(), anchor_local.map(DbLocalTime)],
            )?;
            Ok(Reminder { id: conn.last_insert_rowid(), user_id, description, due_at, recurrence, anchor_local })
        })
        .await
    }

    pub async fn reminders_for_user(&self, user_id: i64) -> anyhow::Result<Vec<Reminder>> {
        self.call(move |conn| {
            conn.prepare(
                "SELECT reminder_id, user_id, description, due_at, recurrence, anchor_local
                 FROM reminders WHERE user_id = ?1 ORDER BY due_at, reminder_id",
            )?
            .query_map([user_id], reminder_from_row)?
            .collect()
        })
        .await
    }

    pub async fn reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
    ) -> anyhow::Result<Option<Reminder>> {
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {REMINDER_COLUMNS} FROM reminders WHERE reminder_id = ?1 AND user_id = ?2"),
                [reminder_id, user_id],
                reminder_from_row,
            )
            .optional()
        })
        .await
    }

    /// Changes one of the user's reminders. The description is kept if `description` is `None`.
    #[cfg(test)]
    pub async fn update_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
        description: Option<String>,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
    ) -> anyhow::Result<Option<Reminder>> {
        self.update_reminder_with_anchor(
            user_id,
            reminder_id,
            description,
            due_at,
            recurrence,
            None,
        )
        .await
    }

    pub async fn update_reminder_with_anchor(
        &self,
        user_id: i64,
        reminder_id: i64,
        description: Option<String>,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> anyhow::Result<Option<Reminder>> {
        self.call(move |conn| {
            conn.query_row(
                &format!(
                    "UPDATE reminders
                     SET description = COALESCE(?1, description), due_at = ?2, recurrence = ?3,
                         anchor_local = ?6
                     WHERE reminder_id = ?4 AND user_id = ?5
                     RETURNING {REMINDER_COLUMNS}"
                ),
                params![
                    description,
                    DbTime(due_at),
                    recurrence.to_db(),
                    reminder_id,
                    user_id,
                    anchor_local.map(DbLocalTime)
                ],
                reminder_from_row,
            )
            .optional()
        })
        .await
    }

    /// A text-only edit must not reset the original calendar anchor or a schedule advanced
    /// concurrently by the scheduler.
    pub async fn update_reminder_description(
        &self,
        user_id: i64,
        reminder_id: i64,
        description: Option<String>,
    ) -> anyhow::Result<Option<Reminder>> {
        self.call(move |conn| {
            conn.query_row(
                &format!(
                    "UPDATE reminders SET description = COALESCE(?1, description)
                          WHERE reminder_id = ?2 AND user_id = ?3 RETURNING {REMINDER_COLUMNS}"
                ),
                params![description, reminder_id, user_id],
                reminder_from_row,
            )
            .optional()
        })
        .await
    }

    /// Deletes one of the user's reminders and stops any nagging it started.
    pub async fn delete_reminder(&self, user_id: i64, reminder_id: i64) -> anyhow::Result<bool> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM naggings WHERE reminder_id = ?1 AND user_id = ?2",
                [reminder_id, user_id],
            )?;
            let removed = tx.execute(
                "DELETE FROM reminders WHERE reminder_id = ?1 AND user_id = ?2",
                [reminder_id, user_id],
            )?;
            tx.commit()?;
            Ok(removed > 0)
        })
        .await
    }

    /// Deletes all of the user's reminders and naggings; returns the number of reminders removed.
    pub async fn delete_all_reminders(&self, user_id: i64) -> anyhow::Result<usize> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM naggings WHERE user_id = ?1", [user_id])?;
            tx.execute("DELETE FROM confirmations WHERE user_id = ?1", [user_id])?;
            let removed = tx.execute("DELETE FROM reminders WHERE user_id = ?1", [user_id])?;
            tx.commit()?;
            Ok(removed)
        })
        .await
    }

    pub async fn due_reminders(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Reminder>> {
        self.call(move |conn| {
            conn.prepare(
                "SELECT reminder_id, user_id, description, due_at, recurrence, anchor_local
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
    pub async fn fire_reminder(
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
                "INSERT INTO naggings (reminder_id, user_id, description, next_at) VALUES (?1, ?2, ?3, ?4)",
                params![reminder.id, reminder.user_id, reminder.description, DbTime(first_nag_at)],
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
                 AND requires_reconnect = 0 AND scenario_id IS NOT NULL",
                params![nagging.id, now.timestamp(), reminder.user_id],
            )?;

            let tz = user_tz(&tx, reminder.user_id)?;
            let anchor = reminder.anchor_local.or_else(|| tz.map(|tz| tz.local(reminder.due_at)));
            let next = tz.and_then(|tz| match reminder.anchor_local {
                Some(anchor) => reminder.recurrence.next_after_anchored(reminder.due_at, now, tz, anchor),
                None => reminder.recurrence.next_after(reminder.due_at, now, tz),
            });
            match next {
                Some(next) => tx.execute(
                    "UPDATE reminders SET due_at = ?1, anchor_local = ?3 WHERE reminder_id = ?2",
                    params![DbTime(next), reminder.id, anchor.map(DbLocalTime)],
                )?,
                None => tx.execute("DELETE FROM reminders WHERE reminder_id = ?1", [reminder.id])?,
            };

            tx.commit()?;
            Ok(Some(nagging))
        })
        .await
    }

    // ---- naggings ----------------------------------------------------------------------

    /// Pending occurrences remain visible after their one-off reminder has been consumed
    /// and while snoozed. Never join reminders here: one-off occurrences have no parent.
    pub async fn naggings_for_user(&self, user_id: i64) -> anyhow::Result<Vec<Nagging>> {
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

    pub async fn due_naggings(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Nagging>> {
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
    pub async fn claim_nagging(
        &self,
        observed: &Nagging,
        now: DateTime<Utc>,
        next_at: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
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
    pub async fn acknowledge_nagging(&self, user_id: i64, nagging_id: i64) -> anyhow::Result<bool> {
        self.complete_nagging(user_id, nagging_id, Utc::now()).await
    }

    /// Completion and its optional one-time check are recorded atomically.
    pub async fn complete_nagging(
        &self,
        user_id: i64,
        nagging_id: i64,
        now: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let completed: Option<(String, Option<i64>)> = tx.query_row(
                "DELETE FROM naggings WHERE nagging_id = ?1 AND user_id = ?2 RETURNING description, reminder_id",
                [nagging_id, user_id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let Some((description, reminder_id)) = completed else { return Ok(false) };
            let delay: i64 = tx.query_row("SELECT confirmation_check_minutes FROM preferences WHERE user_id = ?1", [user_id], |row| row.get(0)).optional()?.unwrap_or(0);
            if delay == 20 {
                tx.execute("INSERT INTO confirmations (source_nagging_id, reminder_id, user_id, description, due_at) VALUES (?1, ?2, ?3, ?4, ?5)", params![nagging_id, reminder_id, user_id, description, DbTime(now + chrono::Duration::minutes(delay))])?;
            }
            tx.commit()?;
            Ok(true)
        }).await
    }

    pub async fn due_confirmations(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Confirmation>> {
        self.call(move |conn| conn.prepare("SELECT confirmation_id, user_id, description, due_at FROM confirmations WHERE due_at <= ?1 ORDER BY due_at, confirmation_id")?
            .query_map([DbTime(now)], |row| Ok(Confirmation { id: row.get(0)?, user_id: row.get(1)?, description: row.get(2)?, due_at: row.get::<_, DbTime>(3)?.0 }))?.collect()).await
    }

    pub async fn delete_confirmation(&self, id: i64) -> anyhow::Result<()> {
        self.call(move |conn| {
            conn.execute("DELETE FROM confirmations WHERE confirmation_id = ?1", [id])?;
            Ok(())
        })
        .await
    }

    pub async fn postpone_confirmation(
        &self,
        id: i64,
        due_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.call(move |conn| {
            conn.execute(
                "UPDATE confirmations SET due_at = ?1 WHERE confirmation_id = ?2",
                params![DbTime(due_at), id],
            )?;
            Ok(())
        })
        .await
    }

    /// Postpones the next nag. Only the user being nagged can snooze it.
    pub async fn snooze_nagging(
        &self,
        user_id: i64,
        nagging_id: i64,
        next_at: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let changed = tx.execute(
                "UPDATE naggings SET next_at = ?1 WHERE nagging_id = ?2 AND user_id = ?3",
                params![DbTime(next_at), nagging_id, user_id],
            )? > 0;
            if changed {
                // A claimed worker rechecks the job immediately before its external request.
                // Removing it cancels queued work; an already dispatched request cannot be recalled.
                tx.execute(
                    "DELETE FROM yandex_jobs WHERE nagging_id = ?1 AND user_id = ?2
                     AND state IN ('pending', 'claimed')",
                    [nagging_id, user_id],
                )?;
            }
            tx.commit()?;
            Ok(changed)
        })
        .await
    }

    /// Stops naggings by reminder id. Handles "Acknowledge" buttons sent by the .NET bot.
    pub async fn acknowledge_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
    ) -> anyhow::Result<bool> {
        let due_at = Utc::now() + chrono::Duration::minutes(20);
        self.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO confirmations (source_nagging_id, reminder_id, user_id, description, due_at)
                 SELECT nagging_id, reminder_id, user_id, description, ?1 FROM naggings
                 WHERE reminder_id = ?2 AND user_id = ?3
                 AND EXISTS (SELECT 1 FROM preferences WHERE user_id = ?3 AND confirmation_check_minutes = 20)
                 ON CONFLICT (source_nagging_id) DO NOTHING",
                params![DbTime(due_at), reminder_id, user_id],
            )?;
            let removed = tx.execute("DELETE FROM naggings WHERE reminder_id = ?1 AND user_id = ?2", [reminder_id, user_id])? > 0;
            tx.commit()?;
            Ok(removed)
        }).await
    }

    /// Stops nagging a user, e.g. after they blocked the bot.
    pub async fn delete_naggings_for_user(&self, user_id: i64) -> anyhow::Result<usize> {
        self.call(move |conn| conn.execute("DELETE FROM naggings WHERE user_id = ?1", [user_id]))
            .await
    }

    // ---- drafts ------------------------------------------------------------------------

    /// Stores text waiting for a time. A user has at most one draft; older ones are replaced.
    pub async fn create_draft(
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

    pub async fn draft(&self, user_id: i64, draft_id: i64) -> anyhow::Result<Option<Draft>> {
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
    pub async fn pending_draft(
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

    pub async fn delete_draft(&self, user_id: i64, draft_id: i64) -> anyhow::Result<bool> {
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
    pub async fn save_draft(
        &self,
        user_id: i64,
        draft_id: i64,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
    ) -> anyhow::Result<Option<Reminder>> {
        self.save_draft_with_anchor(user_id, draft_id, due_at, recurrence, None)
            .await
    }

    pub async fn save_draft_with_anchor(
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
                params![user_id, description, DbTime(due_at), recurrence.to_db(), anchor_local.map(DbLocalTime)],
            )?;
            let reminder = Reminder { id: tx.last_insert_rowid(), user_id, description, due_at, recurrence, anchor_local };
            tx.commit()?;
            Ok(Some(reminder))
        })
        .await
    }

    pub async fn delete_drafts_before(&self, cutoff: DateTime<Utc>) -> anyhow::Result<usize> {
        self.call(move |conn| {
            conn.execute("DELETE FROM drafts WHERE created_at < ?1", [DbTime(cutoff)])
        })
        .await
    }

    // ---- maintenance -------------------------------------------------------------------

    pub async fn vacuum(&self) -> anyhow::Result<()> {
        self.call(|conn| conn.execute_batch("VACUUM;")).await
    }
}

/// 256 random bits, hex encoded.
fn new_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no secure randomness: {e}"))?;
    Ok(hex::encode(bytes))
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    // Table rebuilds in migrations require foreign key enforcement to be off.
    conn.pragma_update(None, "foreign_keys", false)?;
    let applied: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let applied = usize::try_from(applied).unwrap_or(0);
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        let version = i64::try_from(index + 1).expect("migration count fits in i64");
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        tracing::info!(version, "applied database migration");
    }
    Ok(())
}

fn reminder_by_id(tx: &Transaction<'_>, reminder_id: i64) -> rusqlite::Result<Option<Reminder>> {
    tx.query_row(
        &format!("SELECT {REMINDER_COLUMNS} FROM reminders WHERE reminder_id = ?1"),
        [reminder_id],
        reminder_from_row,
    )
    .optional()
}

fn user_tz(tx: &Transaction<'_>, user_id: i64) -> rusqlite::Result<Option<UserTz>> {
    tx.query_row(
        "SELECT utc_offset_minutes, timezone FROM users WHERE user_id = ?1",
        [user_id],
        |row| tz_from_columns(row, 0, 1),
    )
    .optional()
}

fn preferences_from_row(row: &Row<'_>) -> rusqlite::Result<Preferences> {
    Ok(Preferences {
        language: row.get(0)?,
        nag_interval_minutes: row.get(1)?,
        confirmation_check_minutes: row.get(2)?,
    })
}

fn user_from_row(row: &Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: row.get(0)?,
        first_name: row.get(1)?,
        last_name: row.get(2)?,
        tz: tz_from_columns(row, 3, 4)?,
    })
}

/// A named timezone takes precedence; the offset column is the fallback.
fn tz_from_columns(
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

fn draft_from_row(row: &Row<'_>) -> rusqlite::Result<Draft> {
    Ok(Draft {
        id: row.get(0)?,
        user_id: row.get(1)?,
        text: row.get(2)?,
        created_at: row.get::<_, DbTime>(3)?.0,
    })
}

fn reminder_from_row(row: &Row<'_>) -> rusqlite::Result<Reminder> {
    let recurrence: i64 = row.get(4)?;
    Ok(Reminder {
        id: row.get(0)?,
        user_id: row.get(1)?,
        description: row.get(2)?,
        due_at: row.get::<_, DbTime>(3)?.0,
        recurrence: Recurrence::from_db(recurrence)
            .ok_or_else(|| invalid_column(4, "unknown recurrence"))?,
        anchor_local: row.get::<_, Option<DbLocalTime>>(5)?.map(|time| time.0),
    })
}

fn nagging_from_row(row: &Row<'_>) -> rusqlite::Result<Nagging> {
    Ok(Nagging {
        id: row.get(0)?,
        reminder_id: row.get(1)?,
        user_id: row.get(2)?,
        description: row.get(3)?,
        next_at: row.get::<_, DbTime>(4)?.0,
    })
}

fn invalid_column(index: usize, message: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Integer, message.into())
}

/// A UTC timestamp stored as `TIME_FORMAT` text. That format sorts lexically, so SQL range
/// comparisons on these columns are correct.
struct DbTime(DateTime<Utc>);

/// Local wall-clock schedule anchor; deliberately has no UTC offset.
struct DbLocalTime(NaiveDateTime);

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn user(id: i64) -> User {
        User {
            id,
            first_name: "Ada".into(),
            last_name: String::new(),
            tz: UserTz::Fixed(UtcOffset::from_minutes(480).unwrap()),
        }
    }

    #[tokio::test]
    async fn calendar_anchors_survive_reopening_and_legacy_rows_gain_an_anchor() {
        let path = std::env::temp_dir().join(format!(
            "napominalochka-anchor-{}.sqlite",
            new_token().unwrap()
        ));
        let db = Db::open(&path).unwrap();
        let mut utc_user = user(1);
        utc_user.tz = UserTz::Fixed(UtcOffset::from_minutes(0).unwrap());
        db.upsert_user(utc_user).await.unwrap();
        let mut london_user = user(2);
        london_user.tz = UserTz::Named(chrono_tz::Tz::Europe__London);
        let london = london_user.tz;
        db.upsert_user(london_user).await.unwrap();

        // An unanchored legacy row is upgraded using its current due local date.
        let monthly = db
            .add_reminder(
                1,
                "monthly".into(),
                utc(2027, 1, 31, 9, 0),
                Recurrence::Monthly,
            )
            .await
            .unwrap();
        assert_eq!(monthly.anchor_local, None);
        let daily_anchor = utc(2026, 3, 28, 1, 30).naive_utc();
        let daily = db
            .add_reminder_with_anchor(
                2,
                "daily".into(),
                london.utc(daily_anchor),
                Recurrence::Daily,
                Some(daily_anchor),
            )
            .await
            .unwrap();
        let leap_anchor = utc(2028, 2, 29, 9, 0).naive_utc();
        let yearly = db
            .add_reminder_with_anchor(
                1,
                "yearly".into(),
                leap_anchor.and_utc(),
                Recurrence::Yearly,
                Some(leap_anchor),
            )
            .await
            .unwrap();
        for reminder in [&monthly, &daily, &yearly] {
            db.fire_reminder(reminder.id, reminder.due_at, reminder.due_at)
                .await
                .unwrap();
        }
        drop(db);

        let db = Db::open(&path).unwrap();
        let monthly = db.reminder(1, monthly.id).await.unwrap().unwrap();
        assert_eq!(monthly.due_at, utc(2027, 2, 28, 9, 0));
        assert_eq!(
            monthly.anchor_local,
            Some(utc(2027, 1, 31, 9, 0).naive_utc())
        );
        db.fire_reminder(monthly.id, monthly.due_at, monthly.due_at)
            .await
            .unwrap();
        assert_eq!(
            db.reminder(1, monthly.id).await.unwrap().unwrap().due_at,
            utc(2027, 3, 31, 9, 0)
        );

        let daily = db.reminder(2, daily.id).await.unwrap().unwrap();
        assert_eq!(
            london.local(daily.due_at),
            utc(2026, 3, 29, 2, 30).naive_utc()
        );
        assert_eq!(daily.anchor_local, Some(daily_anchor));
        db.fire_reminder(daily.id, daily.due_at, daily.due_at)
            .await
            .unwrap();
        assert_eq!(
            london.local(db.reminder(2, daily.id).await.unwrap().unwrap().due_at),
            utc(2026, 3, 30, 1, 30).naive_utc()
        );

        let mut yearly = db.reminder(1, yearly.id).await.unwrap().unwrap();
        for year in 2029..=2032 {
            assert_eq!(yearly.anchor_local, Some(leap_anchor));
            assert_eq!(
                yearly.due_at,
                utc(year, 2, if year == 2032 { 29 } else { 28 }, 9, 0)
            );
            db.fire_reminder(yearly.id, yearly.due_at, yearly.due_at)
                .await
                .unwrap();
            yearly = db.reminder(1, yearly.id).await.unwrap().unwrap();
        }
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn pending_one_offs_and_snoozes_survive_reopening_and_stay_private() {
        let path = std::env::temp_dir().join(format!(
            "napominalochka-pending-{}.sqlite",
            new_token().unwrap()
        ));
        let db = Db::open(&path).unwrap();
        db.upsert_user(user(1)).await.unwrap();
        db.upsert_user(user(2)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let later = due + ChronoDuration::minutes(10);
        let mut nags = Vec::new();
        for (owner, text) in [(1, "first"), (1, "second"), (2, "private")] {
            let reminder = db
                .add_reminder(owner, text.into(), due, Recurrence::Once)
                .await
                .unwrap();
            nags.push(
                db.fire_reminder(reminder.id, due, later)
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        assert!(db.reminders_for_user(1).await.unwrap().is_empty());
        assert!(db.due_naggings(due).await.unwrap().is_empty());
        assert_eq!(
            db.naggings_for_user(1)
                .await
                .unwrap()
                .iter()
                .map(|n| n.id)
                .collect::<Vec<_>>(),
            vec![nags[0].id, nags[1].id]
        );
        let snoozed_until = due + ChronoDuration::hours(1);
        db.snooze_nagging(1, nags[0].id, snoozed_until)
            .await
            .unwrap();
        drop(db);

        let db = Db::open(&path).unwrap();
        let pending = db.naggings_for_user(1).await.unwrap();
        assert_eq!(
            pending.iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![nags[1].id, nags[0].id]
        );
        assert!(
            pending
                .iter()
                .all(|n| n.user_id == 1 && n.reminder_id.is_none())
        );
        assert_eq!(pending[1].next_at, snoozed_until);
        assert_eq!(db.naggings_for_user(2).await.unwrap()[0].id, nags[2].id);
        assert!(db.naggings_for_user(3).await.unwrap().is_empty());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn nagging_claim_requires_a_current_due_snapshot_and_only_one_worker_wins() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let reminder = db
            .add_reminder(1, "task".into(), due, Recurrence::Once)
            .await
            .unwrap();
        db.fire_reminder(reminder.id, due, due).await.unwrap();
        let observed = db.due_naggings(due).await.unwrap().remove(0);
        let next = due + ChronoDuration::minutes(10);
        let mut wrong_owner = observed.clone();
        wrong_owner.user_id = 2;
        assert!(!db.claim_nagging(&wrong_owner, due, next).await.unwrap());
        let (first, second) = tokio::join!(
            db.claim_nagging(&observed, due, next),
            db.claim_nagging(&observed, due, next),
        );
        assert_ne!(first.unwrap(), second.unwrap());
        let rescheduled = db.naggings_for_user(1).await.unwrap().remove(0);
        assert!(
            !db.claim_nagging(&rescheduled, due, next).await.unwrap(),
            "future occurrence cannot be claimed"
        );

        let until = due + ChronoDuration::hours(1);
        db.snooze_nagging(1, observed.id, until).await.unwrap();
        assert!(
            !db.claim_nagging(&rescheduled, next, next + ChronoDuration::minutes(10))
                .await
                .unwrap()
        );
        assert_eq!(db.naggings_for_user(1).await.unwrap()[0].next_at, until);
        let snoozed = db.naggings_for_user(1).await.unwrap().remove(0);
        db.complete_nagging(1, observed.id, next).await.unwrap();
        assert!(
            !db.claim_nagging(&snoozed, until, until + ChronoDuration::minutes(10))
                .await
                .unwrap()
        );
        assert!(db.naggings_for_user(1).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn snoozing_cancels_only_its_owners_undispatched_yandex_job() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        db.upsert_user(user(2)).await.unwrap();
        db.call(|conn| conn.execute_batch(
            "INSERT INTO yandex_connections (user_id, generation, encrypted_tokens, enabled, scenario_id)
             VALUES (1, 'g1', X'01', 1, 'scenario-1'), (2, 'g2', X'02', 1, 'scenario-2');"
        )).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let later = due + ChronoDuration::hours(1);
        let mut nags = Vec::new();
        for owner in [1, 1, 1, 2] {
            let reminder = db
                .add_reminder(owner, "task".into(), due, Recurrence::Once)
                .await
                .unwrap();
            nags.push(
                db.fire_reminder(reminder.id, due, due)
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let claimed_id = nags[1].id;
        let delivered_id = nags[2].id;
        db.call(move |conn| {
            conn.execute(
                "UPDATE yandex_jobs SET state = 'claimed' WHERE nagging_id = ?1",
                [claimed_id],
            )?;
            conn.execute(
                "UPDATE yandex_jobs SET state = 'delivered' WHERE nagging_id = ?1",
                [delivered_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        assert!(!db.snooze_nagging(2, nags[0].id, later).await.unwrap());
        let count = || {
            db.call(|conn| {
                conn.query_row("SELECT COUNT(*) FROM yandex_jobs", [], |row| {
                    row.get::<_, i64>(0)
                })
            })
        };
        assert_eq!(count().await.unwrap(), 4);
        for nagging in &nags[..3] {
            assert!(db.snooze_nagging(1, nagging.id, later).await.unwrap());
        }
        let remaining = db
            .call(|conn| {
                conn.prepare("SELECT nagging_id, state FROM yandex_jobs ORDER BY job_id")?
                    .query_map([], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .await
            .unwrap();
        assert_eq!(
            remaining,
            vec![
                (nags[2].id, "delivered".into()),
                (nags[3].id, "pending".into())
            ]
        );
        assert_eq!(db.naggings_for_user(1).await.unwrap().len(), 3);
        assert_eq!(db.naggings_for_user(2).await.unwrap()[0].next_at, due);
        let snoozed = db.naggings_for_user(1).await.unwrap().remove(0);
        assert!(
            db.claim_nagging(&snoozed, later, later + ChronoDuration::minutes(10))
                .await
                .unwrap()
        );
        assert_eq!(
            count().await.unwrap(),
            2,
            "repeating never creates a new voice job"
        );
    }

    #[tokio::test]
    async fn upgrades_a_database_created_by_the_dotnet_bot() {
        let conn = Connection::open_in_memory().unwrap();
        // The .NET bot never enabled foreign keys, so its databases can contain orphaned rows.
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(
            "INSERT INTO users VALUES (1, 'Ada', '', 8);
             INSERT INTO reminders (user_id, description, datetime, recurring)
                 VALUES (1, 'stretch', '2026-01-01T00:00:00', 1);
             INSERT INTO reminders (user_id, description, datetime, recurring)
                 VALUES (99, 'orphan', '2026-01-01T00:00:00', 0);
             INSERT INTO naggings (reminder_id, user_id, description, datetime)
                 VALUES (1, 1, 'stretch', '2026-01-01T00:01:00');",
        )
        .unwrap();

        let db = Db::init(conn).unwrap();

        let migrated = db.user(1).await.unwrap().unwrap();
        assert_eq!(
            migrated.tz,
            UserTz::Fixed(UtcOffset::from_minutes(480).unwrap())
        );
        let reminders = db.reminders_for_user(1).await.unwrap();
        assert_eq!(reminders.len(), 1);
        assert_eq!(reminders[0].due_at, utc(2026, 1, 1, 0, 0));
        assert_eq!(reminders[0].recurrence, Recurrence::Daily);
        assert!(db.reminders_for_user(99).await.unwrap().is_empty());
        assert!(
            db.due_naggings(utc(2030, 1, 1, 0, 0))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn register_twice_updates_the_user() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(
            db.upsert_user(user(1)).await.unwrap(),
            Registration::Created
        );
        let mut updated = user(1);
        updated.tz = UserTz::Named(chrono_tz::Tz::Europe__London);
        assert_eq!(
            db.upsert_user(updated.clone()).await.unwrap(),
            Registration::Updated
        );
        assert_eq!(db.user(1).await.unwrap(), Some(updated));
    }

    #[tokio::test]
    async fn recurring_reminder_is_rescheduled_and_nags_until_acknowledged() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let reminder = db
            .add_reminder(1, "pills".into(), due, Recurrence::Daily)
            .await
            .unwrap();

        assert!(
            db.due_reminders(due - ChronoDuration::minutes(1))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(db.due_reminders(due).await.unwrap(), vec![reminder.clone()]);

        let nag_at = due + ChronoDuration::minutes(1);
        let nagging = db
            .fire_reminder(reminder.id, due, nag_at)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            db.reminders_for_user(1).await.unwrap()[0].due_at,
            utc(2026, 3, 2, 8, 0)
        );
        assert!(db.due_reminders(due).await.unwrap().is_empty());
        assert_eq!(
            db.due_naggings(nag_at).await.unwrap(),
            vec![nagging.clone()]
        );

        assert!(
            !db.acknowledge_nagging(2, nagging.id).await.unwrap(),
            "other users cannot acknowledge"
        );
        assert!(db.acknowledge_nagging(1, nagging.id).await.unwrap());
        assert!(db.due_naggings(nag_at).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn next_occurrence_replaces_an_unacknowledged_nagging() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let reminder = db
            .add_reminder(1, "pills".into(), due, Recurrence::Daily)
            .await
            .unwrap();

        db.fire_reminder(reminder.id, due, due).await.unwrap();
        let next_day = utc(2026, 3, 2, 8, 0);
        let second = db
            .fire_reminder(reminder.id, next_day, next_day)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(db.due_naggings(next_day).await.unwrap(), vec![second]);
    }

    #[tokio::test]
    async fn one_off_reminder_is_consumed_but_keeps_nagging() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let reminder = db
            .add_reminder(1, "call mum".into(), due, Recurrence::Once)
            .await
            .unwrap();

        let nagging = db
            .fire_reminder(reminder.id, due, due)
            .await
            .unwrap()
            .unwrap();
        assert!(db.reminders_for_user(1).await.unwrap().is_empty());
        let pending = db.due_naggings(due).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, nagging.id);
        assert_eq!(pending[0].reminder_id, None);
    }

    #[tokio::test]
    async fn overdue_reminder_skips_missed_occurrences() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        let reminder = db
            .add_reminder(
                1,
                "water plants".into(),
                utc(2026, 3, 1, 8, 0),
                Recurrence::Daily,
            )
            .await
            .unwrap();

        let now = utc(2026, 3, 5, 12, 0);
        db.fire_reminder(reminder.id, now, now).await.unwrap();
        assert_eq!(
            db.reminders_for_user(1).await.unwrap()[0].due_at,
            utc(2026, 3, 6, 8, 0)
        );
    }

    #[tokio::test]
    async fn removing_a_reminder_stops_its_nagging() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let reminder = db
            .add_reminder(1, "pills".into(), due, Recurrence::Daily)
            .await
            .unwrap();
        db.fire_reminder(reminder.id, due, due).await.unwrap();

        assert!(
            !db.delete_reminder(2, reminder.id).await.unwrap(),
            "other users cannot delete it"
        );
        assert!(db.delete_reminder(1, reminder.id).await.unwrap());
        assert!(db.due_naggings(due).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unregistering_removes_everything() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let reminder = db
            .add_reminder(1, "a".into(), due, Recurrence::Daily)
            .await
            .unwrap();
        db.add_reminder(1, "b".into(), due, Recurrence::Once)
            .await
            .unwrap();
        db.fire_reminder(reminder.id, due, due).await.unwrap();

        assert!(db.delete_user(1).await.unwrap());
        assert!(db.reminders_for_user(1).await.unwrap().is_empty());
        assert!(db.due_naggings(due).await.unwrap().is_empty());
        assert!(!db.delete_user(1).await.unwrap());
    }

    #[tokio::test]
    async fn legacy_acknowledgement_by_reminder_id() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let reminder = db
            .add_reminder(1, "pills".into(), due, Recurrence::Weekly)
            .await
            .unwrap();
        db.fire_reminder(reminder.id, due, due).await.unwrap();

        assert!(db.acknowledge_reminder(1, reminder.id).await.unwrap());
        assert!(db.due_naggings(due).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn draft_becomes_a_reminder_once() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        let now = utc(2026, 3, 1, 8, 0);
        let first = db.create_draft(1, "old".into(), now).await.unwrap();
        let draft = db.create_draft(1, "call mum".into(), now).await.unwrap();
        assert_eq!(
            db.draft(1, first.id).await.unwrap(),
            None,
            "only the latest draft is kept"
        );
        assert_eq!(
            db.draft(2, draft.id).await.unwrap(),
            None,
            "drafts are private"
        );
        assert_eq!(db.pending_draft(1, now).await.unwrap(), Some(draft.clone()));

        let due = utc(2026, 3, 2, 1, 0);
        let reminder = db
            .save_draft(1, draft.id, due, Recurrence::Weekdays)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reminder.description, "call mum");
        assert_eq!(reminder.recurrence, Recurrence::Weekdays);
        assert_eq!(
            db.save_draft(1, draft.id, due, Recurrence::Once)
                .await
                .unwrap(),
            None
        );
        assert_eq!(db.reminders_for_user(1).await.unwrap(), vec![reminder]);
    }

    #[tokio::test]
    async fn updates_and_snoozes() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let reminder = db
            .add_reminder(1, "pills".into(), due, Recurrence::Once)
            .await
            .unwrap();

        let later = utc(2026, 3, 3, 9, 0);
        assert_eq!(
            db.update_reminder(2, reminder.id, None, later, Recurrence::Daily)
                .await
                .unwrap(),
            None
        );
        let updated = db
            .update_reminder(
                1,
                reminder.id,
                Some("vitamins".into()),
                later,
                Recurrence::Daily,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                updated.description.as_str(),
                updated.due_at,
                updated.recurrence
            ),
            ("vitamins", later, Recurrence::Daily)
        );

        let nagging = db
            .fire_reminder(reminder.id, later, later)
            .await
            .unwrap()
            .unwrap();
        let snooze_until = later + ChronoDuration::hours(1);
        assert!(
            !db.snooze_nagging(2, nagging.id, snooze_until)
                .await
                .unwrap()
        );
        assert!(
            db.snooze_nagging(1, nagging.id, snooze_until)
                .await
                .unwrap()
        );
        assert!(db.due_naggings(later).await.unwrap().is_empty());
        assert_eq!(db.due_naggings(snooze_until).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn named_timezone_round_trips() {
        let db = Db::open_in_memory().unwrap();
        let mut u = user(1);
        u.tz = UserTz::Named(chrono_tz::Tz::America__New_York);
        db.upsert_user(u.clone()).await.unwrap();
        assert_eq!(db.user(1).await.unwrap(), Some(u));
    }

    #[tokio::test]
    async fn ids_of_deleted_rows_are_not_reused() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let first = db
            .add_reminder(1, "a".into(), due, Recurrence::Once)
            .await
            .unwrap();
        db.delete_reminder(1, first.id).await.unwrap();
        let second = db
            .add_reminder(1, "b".into(), due, Recurrence::Once)
            .await
            .unwrap();
        assert_ne!(first.id, second.id);

        let nag = db
            .fire_reminder(second.id, due, due)
            .await
            .unwrap()
            .unwrap();
        db.acknowledge_nagging(1, nag.id).await.unwrap();
        let third = db
            .add_reminder(1, "c".into(), due, Recurrence::Once)
            .await
            .unwrap();
        let next_nag = db.fire_reminder(third.id, due, due).await.unwrap().unwrap();
        assert_ne!(nag.id, next_nag.id);
    }

    #[tokio::test]
    async fn calendar_tokens() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(
            db.calendar_token(1, false).await.unwrap(),
            None,
            "unregistered"
        );
        db.upsert_user(user(1)).await.unwrap();
        db.upsert_user(user(2)).await.unwrap();

        let token = db.calendar_token(1, false).await.unwrap().unwrap();
        assert_eq!(token.len(), 64);
        assert_eq!(
            db.calendar_token(1, false).await.unwrap().as_ref(),
            Some(&token),
            "stable"
        );
        assert_ne!(
            db.calendar_token(2, false).await.unwrap().as_ref(),
            Some(&token)
        );
        assert_eq!(
            db.user_by_calendar_token(token.clone())
                .await
                .unwrap()
                .map(|u| u.id),
            Some(1)
        );

        let new = db.calendar_token(1, true).await.unwrap().unwrap();
        assert_ne!(new, token);
        assert_eq!(
            db.user_by_calendar_token(token).await.unwrap(),
            None,
            "old link revoked"
        );
        assert_eq!(
            db.user_by_calendar_token(new).await.unwrap().map(|u| u.id),
            Some(1)
        );
    }
    #[tokio::test]
    async fn preferences_and_completion_checks_survive_reopening() {
        let path = std::env::temp_dir().join(format!(
            "napominalochka-test-{}.sqlite",
            new_token().unwrap()
        ));
        let db = Db::open(&path).unwrap();
        db.set_preferences(7, Some("en".into()), Some(20), Some(20))
            .await
            .unwrap();
        db.upsert_user(user(7)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let reminder = db
            .add_reminder(7, "done".into(), due, Recurrence::Once)
            .await
            .unwrap();
        let nagging = db
            .fire_reminder(reminder.id, due, due)
            .await
            .unwrap()
            .unwrap();
        db.complete_nagging(7, nagging.id, due).await.unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(
            db.preferences(7).await.unwrap(),
            Preferences {
                language: "en".into(),
                nag_interval_minutes: 20,
                confirmation_check_minutes: 20
            }
        );
        assert!(
            db.due_confirmations(due + ChronoDuration::minutes(19))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.due_confirmations(due + ChronoDuration::minutes(20))
                .await
                .unwrap()
                .len(),
            1
        );
        db.set_preferences(8, Some("ru".into()), Some(5), Some(20))
            .await
            .unwrap();
        db.upsert_user(user(8)).await.unwrap();
        let reminder = db
            .add_reminder(
                8,
                "other user's completed task".into(),
                due,
                Recurrence::Once,
            )
            .await
            .unwrap();
        let nagging = db
            .fire_reminder(reminder.id, due, due)
            .await
            .unwrap()
            .unwrap();
        db.complete_nagging(8, nagging.id, due).await.unwrap();
        db.set_preferences(7, None, None, Some(0)).await.unwrap();
        let remaining = db
            .due_confirmations(due + ChronoDuration::days(1))
            .await
            .unwrap();
        assert_eq!(
            remaining.len(),
            1,
            "disabling cancels only the owner's pending checks"
        );
        assert_eq!(remaining[0].user_id, 8);
        db.delete_user(8).await.unwrap();
        assert!(
            db.due_confirmations(due + ChronoDuration::days(1))
                .await
                .unwrap()
                .is_empty()
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[tokio::test]
    async fn deleting_a_recurring_reminder_also_cancels_its_completion_check() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_user(user(1)).await.unwrap();
        db.set_preferences(1, None, None, Some(20)).await.unwrap();
        let due = utc(2026, 3, 1, 8, 0);
        let reminder = db
            .add_reminder(1, "daily".into(), due, Recurrence::Daily)
            .await
            .unwrap();
        let nagging = db
            .fire_reminder(reminder.id, due, due)
            .await
            .unwrap()
            .unwrap();
        db.complete_nagging(1, nagging.id, due).await.unwrap();
        assert_eq!(
            db.due_confirmations(due + ChronoDuration::minutes(20))
                .await
                .unwrap()
                .len(),
            1
        );
        db.delete_reminder(1, reminder.id).await.unwrap();
        assert!(
            db.due_confirmations(due + ChronoDuration::days(1))
                .await
                .unwrap()
                .is_empty()
        );
    }
}
