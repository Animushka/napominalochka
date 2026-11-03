//! Reminder preferences use cases.
use super::{Error, ReminderService, Result};
use crate::domain::Preferences;

impl ReminderService {
    pub(crate) async fn preferences(&self, user_id: i64) -> Result<Preferences> {
        Ok(self.repository.preferences(user_id).await?)
    }

    /// A manually chosen language is never overwritten by Telegram's language code.
    pub(crate) async fn initialize_language(
        &self,
        user_id: i64,
        telegram_language: Option<&str>,
    ) -> Result<()> {
        let language = if telegram_language.is_some_and(|code| {
            code.eq_ignore_ascii_case("en") || code.to_ascii_lowercase().starts_with("en-")
        }) {
            "en"
        } else {
            "ru"
        };
        self.repository
            .initialize_language(user_id, language.into())
            .await?;
        Ok(())
    }

    pub(crate) async fn set_preferences(
        &self,
        user_id: i64,
        language: Option<&str>,
        nag_interval_minutes: Option<i64>,
        confirmation_check_minutes: Option<i64>,
    ) -> Result<Preferences> {
        if language.is_some_and(|value| !matches!(value, "ru" | "en")) {
            return Err(Error::InvalidPreferences("language"));
        }
        if nag_interval_minutes.is_some_and(|value| ![1, 5, 10, 20, 30, 60].contains(&value)) {
            return Err(Error::InvalidPreferences("nag_interval_minutes"));
        }
        if confirmation_check_minutes.is_some_and(|value| ![0, 20].contains(&value)) {
            return Err(Error::InvalidPreferences("confirmation_check_minutes"));
        }
        Ok(self
            .repository
            .set_preferences(
                user_id,
                language.map(str::to_owned),
                nag_interval_minutes,
                confirmation_check_minutes,
            )
            .await?)
    }
}
