//! SQLite persistence.
//!
//! A single connection is shared behind a mutex; every call runs on tokio's blocking pool
//! so database I/O never stalls the async runtime.

mod delivery;
mod history;
mod reminder_settings;
mod repository;

#[cfg(test)]
mod migration_tests;

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::Context;
use rusqlite::Connection;

const USER_COLUMNS: &str = "user_id, first_name, last_name, utc_offset_minutes, timezone";
const REMINDER_COLUMNS: &str = "reminder_id, user_id, description, due_at, recurrence, anchor_local, nag_interval_minutes, confirmation_check_minutes, voice_enabled";

/// Timestamps are stored as UTC text in the same format the .NET bot used (`DateTime.ToString("s")`).
const TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

#[derive(Clone)]
pub(crate) struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub(crate) fn open(path: &Path) -> anyhow::Result<Self> {
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
    pub(crate) fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> anyhow::Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        schema::migrate(&mut conn).context("migrating database schema")?;
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

    pub(crate) async fn vacuum(&self) -> anyhow::Result<()> {
        self.call(|conn| conn.execute_batch("VACUUM;")).await
    }
}

mod drafts;
mod mapping;
mod occurrences;
mod pending;
mod planning;
mod reminders;
mod schema;
mod users;
mod yandex;

#[cfg(test)]
mod tests;
