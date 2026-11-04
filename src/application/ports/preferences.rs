//! Preferences persistence contract.
use super::StorageResult;
use crate::domain::{Preferences, UserId};
use futures_util::future::BoxFuture;

pub(crate) trait PreferencesRepository: Send + Sync + 'static {
    fn preferences(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<Preferences>>;
    fn initialize_language(
        &self,
        user_id: UserId,
        language: crate::domain::Language,
    ) -> BoxFuture<'_, StorageResult<()>>;
    fn set_preferences(
        &self,
        user_id: UserId,
        language: Option<crate::domain::Language>,
        nag_interval_minutes: Option<crate::domain::NagInterval>,
        confirmation_check_minutes: Option<crate::domain::ConfirmationDelay>,
    ) -> BoxFuture<'_, StorageResult<Preferences>>;
}
