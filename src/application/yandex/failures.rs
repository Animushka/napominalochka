//! Yandex failures use cases.
use super::YandexService;
use super::types::{ConnectionFailure, DispatchWindow, Error, FailureDisposition, Result};
use crate::domain::UserId;
use chrono::Utc;

impl YandexService {
    pub(super) async fn reserve_action(&self, user_id: UserId, generation: &str) -> Result<()> {
        let generation = generation.to_owned();
        let now = Utc::now().timestamp();
        let reserved = self
            .inner
            .repository
            .reserve_action(user_id, generation, DispatchWindow::at(now))
            .await?;
        if reserved {
            Ok(())
        } else {
            Err(Error::RateLimited)
        }
    }

    /// Availability errors during reads/preparation do not revoke the user's choice
    /// to enable voice. Only a lost grant, missing scenario or uncertain action stops it.
    pub(super) async fn record_failure(
        &self,
        user_id: UserId,
        generation: &str,
        error: Error,
    ) -> Result<()> {
        if error == Error::NotConnected {
            return Ok(());
        }
        let generation = generation.to_owned();
        let disposition = match error {
            Error::ReconnectRequired => FailureDisposition::Reconnect,
            Error::ScenarioMissing | Error::DeliveryUncertain => FailureDisposition::Disable,
            _ => {
                let delay = if error == Error::RateLimited { 60 } else { 10 };
                FailureDisposition::RetryAt(Utc::now().timestamp() + delay)
            }
        };
        self.inner
            .repository
            .record_failure(ConnectionFailure {
                user_id,
                generation,
                error,
                disposition,
            })
            .await?;
        Ok(())
    }

    pub(super) async fn clear_temporary_failure(
        &self,
        user_id: UserId,
        generation: &str,
    ) -> Result<()> {
        let generation = generation.to_owned();
        self.inner
            .repository
            .clear_temporary_failure(user_id, generation)
            .await?;
        Ok(())
    }
}
