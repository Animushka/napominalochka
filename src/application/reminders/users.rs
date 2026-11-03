//! Reminder users use cases.
use super::{Error, Profile, ReminderService, Result};
use crate::application::ports::Registration;
use crate::domain::{User, UserTz};

impl ReminderService {
    pub(crate) async fn user(&self, user_id: i64) -> Result<Option<User>> {
        Ok(self.repository.user(user_id).await?)
    }

    pub(crate) async fn registered(&self, user_id: i64) -> Result<User> {
        self.user(user_id).await?.ok_or(Error::NotRegistered)
    }

    /// Registers the user or changes their timezone.
    pub(crate) async fn set_timezone(
        &self,
        profile: Profile,
        tz: UserTz,
    ) -> Result<(User, Registration)> {
        let user = User {
            id: profile.id,
            first_name: profile.first_name,
            last_name: profile.last_name,
            tz,
        };
        let registration = self.repository.upsert_user(user.clone()).await?;
        Ok((user, registration))
    }

    /// Deletes the user and all their data. Returns whether they were registered.
    pub(crate) async fn unregister(&self, user_id: i64) -> Result<bool> {
        Ok(self.repository.delete_user(user_id).await?)
    }
}
