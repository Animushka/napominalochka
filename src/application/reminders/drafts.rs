//! Reminder drafts use cases.
use super::{Error, ReminderService, Result, description, first_due, first_instant};
use crate::domain::{Draft, Recurrence, Reminder, User};
use crate::domain::{DraftId, UserId};
use chrono::{DateTime, Duration, NaiveDateTime, Utc};

impl ReminderService {
    pub(crate) async fn start_draft(
        &self,
        user_id: UserId,
        text: &str,
        now: DateTime<Utc>,
    ) -> Result<Draft> {
        Ok(self
            .repository
            .create_draft(user_id, description(text)?, now)
            .await?)
    }

    pub(crate) async fn draft(&self, user_id: UserId, draft_id: DraftId) -> Result<Draft> {
        self.repository
            .draft(user_id, draft_id)
            .await?
            .ok_or(Error::NotFound)
    }

    /// The user's draft if it was started within `window`.
    pub(crate) async fn pending_draft(
        &self,
        user_id: UserId,
        window: Duration,
        now: DateTime<Utc>,
    ) -> Result<Option<Draft>> {
        Ok(self.repository.pending_draft(user_id, now - window).await?)
    }

    pub(crate) async fn cancel_draft(&self, user_id: UserId, draft_id: DraftId) -> Result<()> {
        self.repository.delete_draft(user_id, draft_id).await?;
        Ok(())
    }

    /// Turns a draft into a reminder. `NotFound` if it was already saved or cancelled.
    pub(crate) async fn save_draft(
        &self,
        user: &User,
        draft_id: DraftId,
        at: NaiveDateTime,
        recurrence: Recurrence,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let due_at = first_due(at, recurrence, user.tz, now)?;
        self.repository
            .save_draft_with_anchor(user.id, draft_id, due_at, recurrence, Some(at))
            .await?
            .ok_or(Error::NotFound)
    }

    /// Keep the exact first instant and anchor future repeats in the local timezone.
    pub(crate) async fn save_draft_at(
        &self,
        user: &User,
        draft_id: DraftId,
        at: DateTime<Utc>,
        recurrence: Recurrence,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let due_at = first_instant(at, recurrence, user.tz, now)?;
        self.repository
            .save_draft_with_anchor(
                user.id,
                draft_id,
                due_at,
                recurrence,
                Some(user.tz.local(at)),
            )
            .await?
            .ok_or(Error::NotFound)
    }
}
