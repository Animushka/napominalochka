//! SQLite adapter for the application repository port.

use super::Db;
use crate::application::ports::{Registration, ReminderRepository};
use crate::model::{
    Completion, Draft, HistoryPage, Nagging, Preferences, Recurrence, Reminder, ReminderChanges,
    ReminderWrite, UndoResult, User,
};
use chrono::{DateTime, NaiveDateTime, Utc};

impl ReminderRepository for Db {
    async fn insert_reminder(
        &self,
        user_id: i64,
        write: ReminderWrite,
    ) -> anyhow::Result<Reminder> {
        Db::insert_reminder(self, user_id, write).await
    }
    async fn patch_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
        changes: ReminderChanges,
    ) -> anyhow::Result<Option<Reminder>> {
        Db::patch_reminder(self, user_id, reminder_id, changes).await
    }
    async fn completion_history(
        &self,
        user_id: i64,
        before: Option<i64>,
        limit: usize,
    ) -> anyhow::Result<HistoryPage> {
        Db::completion_history(self, user_id, before, limit).await
    }
    async fn completion_for_source(
        &self,
        user_id: i64,
        nagging_id: i64,
    ) -> anyhow::Result<Option<Completion>> {
        Db::completion_for_source(self, user_id, nagging_id).await
    }
    async fn undo_completion(
        &self,
        user_id: i64,
        completion_id: i64,
        now: DateTime<Utc>,
    ) -> anyhow::Result<UndoResult> {
        Db::undo_completion(self, user_id, completion_id, now).await
    }

    async fn ping(&self) -> anyhow::Result<()> {
        self.call(|conn| conn.query_row("SELECT 1", [], |_| Ok(())))
            .await
    }

    async fn preferences(&self, user_id: i64) -> anyhow::Result<Preferences> {
        Db::preferences(self, user_id).await
    }

    async fn initialize_language(&self, user_id: i64, language: String) -> anyhow::Result<()> {
        Db::initialize_language(self, user_id, language).await
    }

    async fn set_preferences(
        &self,
        user_id: i64,
        language: Option<String>,
        nag_interval_minutes: Option<i64>,
        confirmation_check_minutes: Option<i64>,
    ) -> anyhow::Result<Preferences> {
        Db::set_preferences(
            self,
            user_id,
            language,
            nag_interval_minutes,
            confirmation_check_minutes,
        )
        .await
    }

    async fn user(&self, user_id: i64) -> anyhow::Result<Option<User>> {
        Db::user(self, user_id).await
    }

    async fn upsert_user(&self, user: User) -> anyhow::Result<Registration> {
        Db::upsert_user(self, user).await
    }

    async fn delete_user(&self, user_id: i64) -> anyhow::Result<bool> {
        Db::delete_user(self, user_id).await
    }

    async fn reminders_for_user(&self, user_id: i64) -> anyhow::Result<Vec<Reminder>> {
        Db::reminders_for_user(self, user_id).await
    }

    async fn reminder(&self, user_id: i64, reminder_id: i64) -> anyhow::Result<Option<Reminder>> {
        Db::reminder(self, user_id, reminder_id).await
    }

    async fn add_reminder_with_anchor(
        &self,
        user_id: i64,
        description: String,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> anyhow::Result<Reminder> {
        Db::add_reminder_with_anchor(self, user_id, description, due_at, recurrence, anchor_local)
            .await
    }

    async fn update_reminder_with_anchor(
        &self,
        user_id: i64,
        reminder_id: i64,
        description: Option<String>,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> anyhow::Result<Option<Reminder>> {
        Db::update_reminder_with_anchor(
            self,
            user_id,
            reminder_id,
            description,
            due_at,
            recurrence,
            anchor_local,
        )
        .await
    }

    async fn update_reminder_description(
        &self,
        user_id: i64,
        reminder_id: i64,
        description: Option<String>,
    ) -> anyhow::Result<Option<Reminder>> {
        Db::update_reminder_description(self, user_id, reminder_id, description).await
    }

    async fn delete_reminder(&self, user_id: i64, reminder_id: i64) -> anyhow::Result<bool> {
        Db::delete_reminder(self, user_id, reminder_id).await
    }

    async fn delete_all_reminders(&self, user_id: i64) -> anyhow::Result<usize> {
        Db::delete_all_reminders(self, user_id).await
    }

    async fn create_draft(
        &self,
        user_id: i64,
        text: String,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Draft> {
        Db::create_draft(self, user_id, text, now).await
    }

    async fn draft(&self, user_id: i64, draft_id: i64) -> anyhow::Result<Option<Draft>> {
        Db::draft(self, user_id, draft_id).await
    }

    async fn pending_draft(
        &self,
        user_id: i64,
        since: DateTime<Utc>,
    ) -> anyhow::Result<Option<Draft>> {
        Db::pending_draft(self, user_id, since).await
    }

    async fn delete_draft(&self, user_id: i64, draft_id: i64) -> anyhow::Result<bool> {
        Db::delete_draft(self, user_id, draft_id).await
    }

    async fn save_draft_with_anchor(
        &self,
        user_id: i64,
        draft_id: i64,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> anyhow::Result<Option<Reminder>> {
        Db::save_draft_with_anchor(self, user_id, draft_id, due_at, recurrence, anchor_local).await
    }

    async fn naggings_for_user(&self, user_id: i64) -> anyhow::Result<Vec<Nagging>> {
        Db::naggings_for_user(self, user_id).await
    }

    async fn complete_nagging(
        &self,
        user_id: i64,
        nagging_id: i64,
        now: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        Db::complete_nagging(self, user_id, nagging_id, now).await
    }

    async fn acknowledge_reminder(&self, user_id: i64, reminder_id: i64) -> anyhow::Result<bool> {
        Db::acknowledge_reminder(self, user_id, reminder_id).await
    }

    async fn snooze_nagging(
        &self,
        user_id: i64,
        nagging_id: i64,
        next_at: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        Db::snooze_nagging(self, user_id, nagging_id, next_at).await
    }

    async fn calendar_token(&self, user_id: i64, reset: bool) -> anyhow::Result<Option<String>> {
        Db::calendar_token(self, user_id, reset).await
    }

    async fn user_by_calendar_token(&self, token: String) -> anyhow::Result<Option<User>> {
        Db::user_by_calendar_token(self, token).await
    }
}
