//! Persistence required by reminder use cases; implementations own transaction boundaries.

use crate::model::{
    Completion, Draft, HistoryPage, Nagging, Preferences, Recurrence, Reminder, ReminderChanges,
    ReminderWrite, UndoResult, User,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use std::future::Future;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    Created,
    Updated,
}

pub trait ReminderRepository: Clone + Send + Sync + 'static {
    fn insert_reminder(
        &self,
        user_id: i64,
        write: ReminderWrite,
    ) -> impl Future<Output = anyhow::Result<Reminder>> + Send;
    fn patch_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
        changes: ReminderChanges,
    ) -> impl Future<Output = anyhow::Result<Option<Reminder>>> + Send;
    fn completion_history(
        &self,
        user_id: i64,
        before: Option<i64>,
        limit: usize,
    ) -> impl Future<Output = anyhow::Result<HistoryPage>> + Send;
    fn completion_for_source(
        &self,
        user_id: i64,
        nagging_id: i64,
    ) -> impl Future<Output = anyhow::Result<Option<Completion>>> + Send;
    fn undo_completion(
        &self,
        user_id: i64,
        completion_id: i64,
        now: DateTime<Utc>,
    ) -> impl Future<Output = anyhow::Result<UndoResult>> + Send;

    fn ping(&self) -> impl Future<Output = anyhow::Result<()>> + Send;

    fn preferences(&self, user_id: i64)
    -> impl Future<Output = anyhow::Result<Preferences>> + Send;

    fn initialize_language(
        &self,
        user_id: i64,
        language: String,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;

    fn set_preferences(
        &self,
        user_id: i64,
        language: Option<String>,
        nag_interval_minutes: Option<i64>,
        confirmation_check_minutes: Option<i64>,
    ) -> impl Future<Output = anyhow::Result<Preferences>> + Send;

    fn user(&self, user_id: i64) -> impl Future<Output = anyhow::Result<Option<User>>> + Send;

    fn upsert_user(&self, user: User) -> impl Future<Output = anyhow::Result<Registration>> + Send;

    fn delete_user(&self, user_id: i64) -> impl Future<Output = anyhow::Result<bool>> + Send;

    fn reminders_for_user(
        &self,
        user_id: i64,
    ) -> impl Future<Output = anyhow::Result<Vec<Reminder>>> + Send;

    fn reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
    ) -> impl Future<Output = anyhow::Result<Option<Reminder>>> + Send;

    fn add_reminder_with_anchor(
        &self,
        user_id: i64,
        description: String,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> impl Future<Output = anyhow::Result<Reminder>> + Send;

    fn update_reminder_with_anchor(
        &self,
        user_id: i64,
        reminder_id: i64,
        description: Option<String>,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> impl Future<Output = anyhow::Result<Option<Reminder>>> + Send;

    fn update_reminder_description(
        &self,
        user_id: i64,
        reminder_id: i64,
        description: Option<String>,
    ) -> impl Future<Output = anyhow::Result<Option<Reminder>>> + Send;

    fn delete_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
    ) -> impl Future<Output = anyhow::Result<bool>> + Send;

    fn delete_all_reminders(
        &self,
        user_id: i64,
    ) -> impl Future<Output = anyhow::Result<usize>> + Send;

    fn create_draft(
        &self,
        user_id: i64,
        text: String,
        now: DateTime<Utc>,
    ) -> impl Future<Output = anyhow::Result<Draft>> + Send;

    fn draft(
        &self,
        user_id: i64,
        draft_id: i64,
    ) -> impl Future<Output = anyhow::Result<Option<Draft>>> + Send;

    fn pending_draft(
        &self,
        user_id: i64,
        since: DateTime<Utc>,
    ) -> impl Future<Output = anyhow::Result<Option<Draft>>> + Send;

    fn delete_draft(
        &self,
        user_id: i64,
        draft_id: i64,
    ) -> impl Future<Output = anyhow::Result<bool>> + Send;

    fn save_draft_with_anchor(
        &self,
        user_id: i64,
        draft_id: i64,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> impl Future<Output = anyhow::Result<Option<Reminder>>> + Send;

    fn naggings_for_user(
        &self,
        user_id: i64,
    ) -> impl Future<Output = anyhow::Result<Vec<Nagging>>> + Send;

    fn complete_nagging(
        &self,
        user_id: i64,
        nagging_id: i64,
        now: DateTime<Utc>,
    ) -> impl Future<Output = anyhow::Result<bool>> + Send;

    fn acknowledge_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
    ) -> impl Future<Output = anyhow::Result<bool>> + Send;

    fn snooze_nagging(
        &self,
        user_id: i64,
        nagging_id: i64,
        next_at: DateTime<Utc>,
    ) -> impl Future<Output = anyhow::Result<bool>> + Send;

    fn calendar_token(
        &self,
        user_id: i64,
        reset: bool,
    ) -> impl Future<Output = anyhow::Result<Option<String>>> + Send;

    fn user_by_calendar_token(
        &self,
        token: String,
    ) -> impl Future<Output = anyhow::Result<Option<User>>> + Send;
}
