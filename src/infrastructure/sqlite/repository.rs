//! SQLite adapter for the application repository port.

use super::Db;
use crate::application::ports::{Registration, ReminderRepository};
use crate::domain::{
    Completion, Draft, HistoryPage, Nagging, Preferences, Recurrence, Reminder, ReminderChanges,
    ReminderWrite, UndoResult, User,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::future::BoxFuture;

impl ReminderRepository for Db {
    fn insert_reminder(
        &self,
        user_id: i64,
        write: ReminderWrite,
    ) -> BoxFuture<'_, anyhow::Result<Reminder>> {
        Box::pin(Db::insert_reminder(self, user_id, write))
    }
    fn patch_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
        changes: ReminderChanges,
    ) -> BoxFuture<'_, anyhow::Result<Option<Reminder>>> {
        Box::pin(Db::patch_reminder(self, user_id, reminder_id, changes))
    }
    fn completion_history(
        &self,
        user_id: i64,
        before: Option<i64>,
        limit: usize,
    ) -> BoxFuture<'_, anyhow::Result<HistoryPage>> {
        Box::pin(Db::completion_history(self, user_id, before, limit))
    }
    fn completion_for_source(
        &self,
        user_id: i64,
        nagging_id: i64,
    ) -> BoxFuture<'_, anyhow::Result<Option<Completion>>> {
        Box::pin(Db::completion_for_source(self, user_id, nagging_id))
    }
    fn undo_completion(
        &self,
        user_id: i64,
        completion_id: i64,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, anyhow::Result<UndoResult>> {
        Box::pin(Db::undo_completion(self, user_id, completion_id, now))
    }

    fn ping(&self) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move {
            self.call(|conn| conn.query_row("SELECT 1", [], |_| Ok(())))
                .await
        })
    }

    fn preferences(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<Preferences>> {
        Box::pin(Db::preferences(self, user_id))
    }

    fn initialize_language(
        &self,
        user_id: i64,
        language: String,
    ) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(Db::initialize_language(self, user_id, language))
    }

    fn set_preferences(
        &self,
        user_id: i64,
        language: Option<String>,
        nag_interval_minutes: Option<i64>,
        confirmation_check_minutes: Option<i64>,
    ) -> BoxFuture<'_, anyhow::Result<Preferences>> {
        Box::pin(Db::set_preferences(
            self,
            user_id,
            language,
            nag_interval_minutes,
            confirmation_check_minutes,
        ))
    }

    fn user(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<Option<User>>> {
        Box::pin(Db::user(self, user_id))
    }

    fn upsert_user(&self, user: User) -> BoxFuture<'_, anyhow::Result<Registration>> {
        Box::pin(Db::upsert_user(self, user))
    }

    fn delete_user(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<bool>> {
        Box::pin(Db::delete_user(self, user_id))
    }

    fn reminders_for_user(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<Vec<Reminder>>> {
        Box::pin(Db::reminders_for_user(self, user_id))
    }

    fn reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
    ) -> BoxFuture<'_, anyhow::Result<Option<Reminder>>> {
        Box::pin(Db::reminder(self, user_id, reminder_id))
    }

    fn add_reminder_with_anchor(
        &self,
        user_id: i64,
        description: String,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, anyhow::Result<Reminder>> {
        Box::pin(Db::add_reminder_with_anchor(
            self,
            user_id,
            description,
            due_at,
            recurrence,
            anchor_local,
        ))
    }

    fn update_reminder_with_anchor(
        &self,
        user_id: i64,
        reminder_id: i64,
        description: Option<String>,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, anyhow::Result<Option<Reminder>>> {
        Box::pin(Db::update_reminder_with_anchor(
            self,
            user_id,
            reminder_id,
            description,
            due_at,
            recurrence,
            anchor_local,
        ))
    }

    fn update_reminder_description(
        &self,
        user_id: i64,
        reminder_id: i64,
        description: Option<String>,
    ) -> BoxFuture<'_, anyhow::Result<Option<Reminder>>> {
        Box::pin(Db::update_reminder_description(
            self,
            user_id,
            reminder_id,
            description,
        ))
    }

    fn delete_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
    ) -> BoxFuture<'_, anyhow::Result<bool>> {
        Box::pin(Db::delete_reminder(self, user_id, reminder_id))
    }

    fn delete_all_reminders(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<usize>> {
        Box::pin(Db::delete_all_reminders(self, user_id))
    }

    fn create_draft(
        &self,
        user_id: i64,
        text: String,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, anyhow::Result<Draft>> {
        Box::pin(Db::create_draft(self, user_id, text, now))
    }

    fn draft(&self, user_id: i64, draft_id: i64) -> BoxFuture<'_, anyhow::Result<Option<Draft>>> {
        Box::pin(Db::draft(self, user_id, draft_id))
    }

    fn pending_draft(
        &self,
        user_id: i64,
        since: DateTime<Utc>,
    ) -> BoxFuture<'_, anyhow::Result<Option<Draft>>> {
        Box::pin(Db::pending_draft(self, user_id, since))
    }

    fn delete_draft(&self, user_id: i64, draft_id: i64) -> BoxFuture<'_, anyhow::Result<bool>> {
        Box::pin(Db::delete_draft(self, user_id, draft_id))
    }

    fn save_draft_with_anchor(
        &self,
        user_id: i64,
        draft_id: i64,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, anyhow::Result<Option<Reminder>>> {
        Box::pin(Db::save_draft_with_anchor(
            self,
            user_id,
            draft_id,
            due_at,
            recurrence,
            anchor_local,
        ))
    }

    fn naggings_for_user(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<Vec<Nagging>>> {
        Box::pin(Db::naggings_for_user(self, user_id))
    }

    fn complete_nagging(
        &self,
        user_id: i64,
        nagging_id: i64,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, anyhow::Result<bool>> {
        Box::pin(Db::complete_nagging(self, user_id, nagging_id, now))
    }

    fn acknowledge_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
    ) -> BoxFuture<'_, anyhow::Result<bool>> {
        Box::pin(Db::acknowledge_reminder(self, user_id, reminder_id))
    }

    fn snooze_nagging(
        &self,
        user_id: i64,
        nagging_id: i64,
        next_at: DateTime<Utc>,
    ) -> BoxFuture<'_, anyhow::Result<bool>> {
        Box::pin(Db::snooze_nagging(self, user_id, nagging_id, next_at))
    }

    fn calendar_token(
        &self,
        user_id: i64,
        reset: bool,
    ) -> BoxFuture<'_, anyhow::Result<Option<String>>> {
        Box::pin(Db::calendar_token(self, user_id, reset))
    }

    fn user_by_calendar_token(&self, token: String) -> BoxFuture<'_, anyhow::Result<Option<User>>> {
        Box::pin(Db::user_by_calendar_token(self, token))
    }
}
