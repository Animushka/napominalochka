//! SQLite operations for reminders.
use super::mapping::reminder_from_row;
use super::{Db, REMINDER_COLUMNS};
use crate::application::ports::StorageResult;
use crate::domain::{
    Recurrence, Reminder, ReminderChanges, ReminderSchedule, ReminderSettings, ReminderWrite,
};
use crate::domain::{ReminderId, UserId};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::OptionalExtension;
use rusqlite::params;

impl Db {
    #[cfg(test)]
    pub(crate) async fn add_reminder(
        &self,
        user_id: UserId,
        description: String,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
    ) -> StorageResult<Reminder> {
        self.add_reminder_with_anchor(user_id, description, due_at, recurrence, None)
            .await
    }

    pub(crate) async fn add_reminder_with_anchor(
        &self,
        user_id: UserId,
        description: String,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> StorageResult<Reminder> {
        self.insert_reminder(
            user_id,
            ReminderWrite {
                description,
                due_at,
                recurrence,
                anchor_local,
                settings: ReminderSettings::default(),
            },
        )
        .await
    }

    pub(crate) async fn reminders_for_user(&self, user_id: UserId) -> StorageResult<Vec<Reminder>> {
        self.call(move |conn| {
            conn.prepare(
                "SELECT reminder_id, user_id, description, due_at, recurrence, anchor_local, nag_interval_minutes, confirmation_check_minutes, voice_enabled
                 FROM reminders WHERE user_id = ?1 ORDER BY due_at, reminder_id",
            )?
            .query_map([user_id], reminder_from_row)?
            .collect()
        })
        .await
    }

    pub(crate) async fn reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
    ) -> StorageResult<Option<Reminder>> {
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {REMINDER_COLUMNS} FROM reminders WHERE reminder_id = ?1 AND user_id = ?2"),
                params![reminder_id, user_id],
                reminder_from_row,
            )
            .optional()
        })
        .await
    }

    /// Changes one of the user's reminders. The description is kept if `description` is `None`.
    #[cfg(test)]
    pub(crate) async fn update_reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
        description: Option<String>,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
    ) -> StorageResult<Option<Reminder>> {
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

    pub(crate) async fn update_reminder_with_anchor(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
        description: Option<String>,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> StorageResult<Option<Reminder>> {
        self.patch_reminder(
            user_id,
            reminder_id,
            ReminderChanges {
                description,
                schedule: Some(ReminderSchedule {
                    due_at,
                    recurrence,
                    anchor_local,
                }),
                settings: None,
            },
        )
        .await
    }

    /// A text-only edit must not reset the original calendar anchor or a schedule advanced
    /// concurrently by the scheduler.
    pub(crate) async fn update_reminder_description(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
        description: Option<String>,
    ) -> StorageResult<Option<Reminder>> {
        self.patch_reminder(
            user_id,
            reminder_id,
            ReminderChanges {
                description,
                schedule: None,
                settings: None,
            },
        )
        .await
    }

    /// Deletes one of the user's reminders and stops any nagging it started.
    pub(crate) async fn delete_reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
    ) -> StorageResult<bool> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM naggings WHERE reminder_id = ?1 AND user_id = ?2",
                params![reminder_id, user_id],
            )?;
            let removed = tx.execute(
                "DELETE FROM reminders WHERE reminder_id = ?1 AND user_id = ?2",
                params![reminder_id, user_id],
            )?;
            tx.commit()?;
            Ok(removed > 0)
        })
        .await
    }

    /// Deletes all of the user's reminders and naggings; returns the number of reminders removed.
    pub(crate) async fn delete_all_reminders(&self, user_id: UserId) -> StorageResult<usize> {
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
}
