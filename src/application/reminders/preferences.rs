//! Reminder preferences use cases.
use super::{ReminderService, Result};
use crate::domain::Preferences;
use crate::domain::UserId;

impl ReminderService {
    pub(crate) async fn preferences(&self, user_id: UserId) -> Result<Preferences> {
        Ok(self.repository.preferences(user_id).await?)
    }

    /// A manually chosen language is never overwritten by Telegram's language code.
    pub(crate) async fn initialize_language(
        &self,
        user_id: UserId,
        telegram_language: Option<&str>,
    ) -> Result<()> {
        let language = crate::domain::Language::from_telegram(telegram_language);
        self.repository
            .initialize_language(user_id, language)
            .await?;
        Ok(())
    }

    pub(crate) async fn set_preferences(
        &self,
        user_id: UserId,
        language: Option<crate::domain::Language>,
        nag_interval_minutes: Option<crate::domain::NagInterval>,
        confirmation_check_minutes: Option<crate::domain::ConfirmationDelay>,
    ) -> Result<Preferences> {
        Ok(self
            .repository
            .set_preferences(
                user_id,
                language,
                nag_interval_minutes,
                confirmation_check_minutes,
            )
            .await?)
    }
}
