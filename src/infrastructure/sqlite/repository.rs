//! SQLite adapter for the application repository port.

use super::Db;
use crate::application::ports::StorageResult;
use crate::application::ports::{
    CalendarRepository, CompletionRepository, DraftRepository, PendingRepository,
    PreferencesRepository, Registration, ReminderStore, StorageHealth, UserRepository,
};
use crate::domain::{
    Completion, Draft, HistoryPage, Nagging, Preferences, Recurrence, Reminder, ReminderChanges,
    ReminderWrite, UndoResult, User,
};
use crate::domain::{CompletionId, DraftId, NaggingId, ReminderId, UserId};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::future::BoxFuture;

impl StorageHealth for Db {
    fn ping(&self) -> BoxFuture<'_, StorageResult<()>> {
        Box::pin(async move {
            self.call(|conn| conn.query_row("SELECT 1", [], |_| Ok(())))
                .await
        })
    }
}

impl UserRepository for Db {
    fn user(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<Option<User>>> {
        Box::pin(Db::user(self, user_id))
    }

    fn upsert_user(&self, user: User) -> BoxFuture<'_, StorageResult<Registration>> {
        Box::pin(Db::upsert_user(self, user))
    }

    fn delete_user(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(Db::delete_user(self, user_id))
    }
}

impl PreferencesRepository for Db {
    fn preferences(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<Preferences>> {
        Box::pin(Db::preferences(self, user_id))
    }

    fn initialize_language(
        &self,
        user_id: UserId,
        language: crate::domain::Language,
    ) -> BoxFuture<'_, StorageResult<()>> {
        Box::pin(Db::initialize_language(self, user_id, language))
    }

    fn set_preferences(
        &self,
        user_id: UserId,
        language: Option<crate::domain::Language>,
        nag_interval_minutes: Option<crate::domain::NagInterval>,
        confirmation_check_minutes: Option<crate::domain::ConfirmationDelay>,
    ) -> BoxFuture<'_, StorageResult<Preferences>> {
        Box::pin(Db::set_preferences(
            self,
            user_id,
            language,
            nag_interval_minutes,
            confirmation_check_minutes,
        ))
    }
}

impl ReminderStore for Db {
    fn insert_reminder(
        &self,
        user_id: UserId,
        write: ReminderWrite,
    ) -> BoxFuture<'_, StorageResult<Reminder>> {
        Box::pin(Db::insert_reminder(self, user_id, write))
    }
    fn patch_reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
        changes: ReminderChanges,
    ) -> BoxFuture<'_, StorageResult<Option<Reminder>>> {
        Box::pin(Db::patch_reminder(self, user_id, reminder_id, changes))
    }
    fn reminders_for_user(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<Vec<Reminder>>> {
        Box::pin(Db::reminders_for_user(self, user_id))
    }

    fn reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
    ) -> BoxFuture<'_, StorageResult<Option<Reminder>>> {
        Box::pin(Db::reminder(self, user_id, reminder_id))
    }

    fn add_reminder_with_anchor(
        &self,
        user_id: UserId,
        description: String,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, StorageResult<Reminder>> {
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
        user_id: UserId,
        reminder_id: ReminderId,
        description: Option<String>,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, StorageResult<Option<Reminder>>> {
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
        user_id: UserId,
        reminder_id: ReminderId,
        description: Option<String>,
    ) -> BoxFuture<'_, StorageResult<Option<Reminder>>> {
        Box::pin(Db::update_reminder_description(
            self,
            user_id,
            reminder_id,
            description,
        ))
    }

    fn delete_reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
    ) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(Db::delete_reminder(self, user_id, reminder_id))
    }

    fn delete_all_reminders(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<usize>> {
        Box::pin(Db::delete_all_reminders(self, user_id))
    }
}

impl DraftRepository for Db {
    fn create_draft(
        &self,
        user_id: UserId,
        text: String,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, StorageResult<Draft>> {
        Box::pin(Db::create_draft(self, user_id, text, now))
    }

    fn draft(
        &self,
        user_id: UserId,
        draft_id: DraftId,
    ) -> BoxFuture<'_, StorageResult<Option<Draft>>> {
        Box::pin(Db::draft(self, user_id, draft_id))
    }

    fn pending_draft(
        &self,
        user_id: UserId,
        since: DateTime<Utc>,
    ) -> BoxFuture<'_, StorageResult<Option<Draft>>> {
        Box::pin(Db::pending_draft(self, user_id, since))
    }

    fn delete_draft(
        &self,
        user_id: UserId,
        draft_id: DraftId,
    ) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(Db::delete_draft(self, user_id, draft_id))
    }

    fn save_draft_with_anchor(
        &self,
        user_id: UserId,
        draft_id: DraftId,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
        anchor_local: Option<NaiveDateTime>,
    ) -> BoxFuture<'_, StorageResult<Option<Reminder>>> {
        Box::pin(Db::save_draft_with_anchor(
            self,
            user_id,
            draft_id,
            due_at,
            recurrence,
            anchor_local,
        ))
    }
}

impl PendingRepository for Db {
    fn naggings_for_user(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<Vec<Nagging>>> {
        Box::pin(Db::naggings_for_user(self, user_id))
    }

    fn complete_nagging(
        &self,
        user_id: UserId,
        nagging_id: NaggingId,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(Db::complete_nagging(self, user_id, nagging_id, now))
    }

    fn acknowledge_reminder(
        &self,
        user_id: UserId,
        reminder_id: ReminderId,
    ) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(Db::acknowledge_reminder(self, user_id, reminder_id))
    }

    fn snooze_nagging(
        &self,
        user_id: UserId,
        nagging_id: NaggingId,
        next_at: DateTime<Utc>,
    ) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(Db::snooze_nagging(self, user_id, nagging_id, next_at))
    }
}

impl CompletionRepository for Db {
    fn completion_history(
        &self,
        user_id: UserId,
        before: Option<CompletionId>,
        limit: usize,
    ) -> BoxFuture<'_, StorageResult<HistoryPage>> {
        Box::pin(Db::completion_history(self, user_id, before, limit))
    }
    fn completion_for_source(
        &self,
        user_id: UserId,
        nagging_id: NaggingId,
    ) -> BoxFuture<'_, StorageResult<Option<Completion>>> {
        Box::pin(Db::completion_for_source(self, user_id, nagging_id))
    }
    fn undo_completion(
        &self,
        user_id: UserId,
        completion_id: CompletionId,
        now: DateTime<Utc>,
    ) -> BoxFuture<'_, StorageResult<UndoResult>> {
        Box::pin(Db::undo_completion(self, user_id, completion_id, now))
    }
}

impl CalendarRepository for Db {
    fn calendar_token(
        &self,
        user_id: UserId,
        reset: bool,
    ) -> BoxFuture<'_, StorageResult<Option<String>>> {
        Box::pin(Db::calendar_token(self, user_id, reset))
    }

    fn user_by_calendar_token(&self, token: String) -> BoxFuture<'_, StorageResult<Option<User>>> {
        Box::pin(Db::user_by_calendar_token(self, token))
    }
}
