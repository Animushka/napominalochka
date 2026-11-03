//! Persistence required by reminder use cases; implementations own transaction boundaries.

use crate::domain::{
    Completion, Draft, HistoryPage, Nagging, Preferences, Recurrence, Reminder, ReminderChanges,
    ReminderWrite, UndoResult, User,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::future::BoxFuture;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Registration {
    Created,
    Updated,
}

pub(crate) trait ReminderRepository: Send + Sync + 'static {
    fn insert_reminder(
        &self,
        user_id: i64,
        write: ReminderWrite,
    ) -> BoxFuture<'_, anyhow::Result<Reminder>>;
    fn patch_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
        changes: ReminderChanges,
    ) -> BoxFuture<'_, anyhow::Result<Option<Reminder>>>;
    fn completion_history(
        &self,
        user_id: i64,
        before: Option<i64>,
        limit: usize,
    ) -> BoxFuture<'_, anyhow::Result<HistoryPage>>;
    fn completion_for_source(
        &self,
        user_id: i64,
        nagging_id: i64,
    ) -> BoxFuture<'_, anyhow::Result<Option<Completion>>>;
    fn undo_completion(
        &self,
        user_id: i64,
        completion_id: i64,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, anyhow::Result<UndoResult>>;

    fn ping(&self) -> BoxFuture<'_, anyhow::Result<()>>;

    fn preferences(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<Preferences>>;

    fn initialize_language(
        &self,
        user_id: i64,
        language: String,
    ) -> BoxFuture<'_, anyhow::Result<()>>;

    fn set_preferences(
        &self,
        user_id: i64,
        language: Option<String>,
        nag_interval_minutes: Option<i64>,
        confirmation_check_minutes: Option<i64>,
    ) -> BoxFuture<'_, anyhow::Result<Preferences>>;

    fn user(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<Option<User>>>;

    fn upsert_user(&self, user: User) -> BoxFuture<'_, anyhow::Result<Registration>>;

    fn delete_user(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<bool>>;

    fn reminders_for_user(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<Vec<Reminder>>>;

    fn reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
    ) -> BoxFuture<'_, anyhow::Result<Option<Reminder>>>;

    fn add_reminder_with_anchor(
        &self,
        user_id: i64,
        description: String,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, anyhow::Result<Reminder>>;

    fn update_reminder_with_anchor(
        &self,
        user_id: i64,
        reminder_id: i64,
        description: Option<String>,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, anyhow::Result<Option<Reminder>>>;

    fn update_reminder_description(
        &self,
        user_id: i64,
        reminder_id: i64,
        description: Option<String>,
    ) -> BoxFuture<'_, anyhow::Result<Option<Reminder>>>;

    fn delete_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
    ) -> BoxFuture<'_, anyhow::Result<bool>>;

    fn delete_all_reminders(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<usize>>;

    fn create_draft(
        &self,
        user_id: i64,
        text: String,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, anyhow::Result<Draft>>;

    fn draft(&self, user_id: i64, draft_id: i64) -> BoxFuture<'_, anyhow::Result<Option<Draft>>>;

    fn pending_draft(
        &self,
        user_id: i64,
        since: DateTime<Utc>,
    ) -> BoxFuture<'_, anyhow::Result<Option<Draft>>>;

    fn delete_draft(&self, user_id: i64, draft_id: i64) -> BoxFuture<'_, anyhow::Result<bool>>;

    fn save_draft_with_anchor(
        &self,
        user_id: i64,
        draft_id: i64,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, anyhow::Result<Option<Reminder>>>;

    fn naggings_for_user(&self, user_id: i64) -> BoxFuture<'_, anyhow::Result<Vec<Nagging>>>;

    fn complete_nagging(
        &self,
        user_id: i64,
        nagging_id: i64,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, anyhow::Result<bool>>;

    fn acknowledge_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
    ) -> BoxFuture<'_, anyhow::Result<bool>>;

    fn snooze_nagging(
        &self,
        user_id: i64,
        nagging_id: i64,
        next_at: DateTime<Utc>,
    ) -> BoxFuture<'_, anyhow::Result<bool>>;

    fn calendar_token(
        &self,
        user_id: i64,
        reset: bool,
    ) -> BoxFuture<'_, anyhow::Result<Option<String>>>;

    fn user_by_calendar_token(&self, token: String) -> BoxFuture<'_, anyhow::Result<Option<User>>>;
}
