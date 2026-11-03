//! Reminder calendar use cases.
use super::{Error, ReminderService, Result};
use crate::domain::{Reminder, User};

impl ReminderService {
    /// The user's feed token; `reset` replaces it, revoking the old feed URL.
    pub(crate) async fn calendar_token(&self, user_id: i64, reset: bool) -> Result<String> {
        self.repository
            .calendar_token(user_id, reset)
            .await?
            .ok_or(Error::NotRegistered)
    }

    /// The feed owner and their reminders, if the token is valid.
    pub(crate) async fn calendar_feed(&self, token: &str) -> Result<Option<(User, Vec<Reminder>)>> {
        let Some(user) = self
            .repository
            .user_by_calendar_token(token.to_owned())
            .await?
        else {
            return Ok(None);
        };
        let reminders = self.repository.reminders_for_user(user.id).await?;
        Ok(Some((user, reminders)))
    }
}
