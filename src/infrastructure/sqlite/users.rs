//! SQLite operations for users.
use super::mapping::{new_token, preferences_from_row, user_from_row};
use super::{Db, USER_COLUMNS, reminder_settings};
use crate::application::ports::Registration;
use crate::application::ports::StorageResult;
use crate::domain::UserId;
use crate::domain::{Preferences, User};
use chrono::Utc;
use rusqlite::{OptionalExtension, params};

impl Db {
    /// Reading defaults does not register a user or overwrite Telegram's initial language.
    pub(crate) async fn preferences(&self, user_id: UserId) -> StorageResult<Preferences> {
        self.call(move |conn| {
            Ok(conn.query_row(
                "SELECT language, nag_interval_minutes, confirmation_check_minutes FROM preferences WHERE user_id = ?1",
                [user_id], preferences_from_row,
            ).optional()?.unwrap_or_default())
        }).await
    }

    pub(crate) async fn initialize_language(
        &self,
        user_id: UserId,
        language: crate::domain::Language,
    ) -> StorageResult<()> {
        self.call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO preferences (user_id, language) VALUES (?1, ?2)",
                params![user_id, language],
            )?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn set_preferences(
        &self,
        user_id: UserId,
        language: Option<crate::domain::Language>,
        nag_interval_minutes: Option<crate::domain::NagInterval>,
        confirmation_check_minutes: Option<crate::domain::ConfirmationDelay>,
    ) -> StorageResult<Preferences> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("INSERT OR IGNORE INTO preferences (user_id) VALUES (?1)", [user_id])?;
            let settings = tx.query_row(
                "UPDATE preferences SET language = COALESCE(?1, language), nag_interval_minutes = COALESCE(?2, nag_interval_minutes), confirmation_check_minutes = COALESCE(?3, confirmation_check_minutes) WHERE user_id = ?4 RETURNING language, nag_interval_minutes, confirmation_check_minutes",
                params![language, nag_interval_minutes, confirmation_check_minutes, user_id], preferences_from_row,
            )?;
            if settings.confirmation_check_minutes == crate::domain::ConfirmationDelay::Disabled {
                tx.execute(&format!("DELETE FROM confirmations WHERE user_id = ?1 AND ({}) <> 20", reminder_settings::EFFECTIVE_CONFIRMATION_CHECK), [user_id])?;
            }
            tx.commit()?;
            Ok(settings)
        }).await
    }

    pub(crate) async fn upsert_user(&self, user: User) -> StorageResult<Registration> {
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

    pub(crate) async fn user(&self, user_id: UserId) -> StorageResult<Option<User>> {
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
    pub(crate) async fn calendar_token(
        &self,
        user_id: UserId,
        reset: bool,
    ) -> StorageResult<Option<String>> {
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

    pub(crate) async fn user_by_calendar_token(
        &self,
        token: String,
    ) -> StorageResult<Option<User>> {
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
    pub(crate) async fn delete_user(&self, user_id: UserId) -> StorageResult<bool> {
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
}
