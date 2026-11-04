//! Yandex tokens use cases.
use super::types::{Connection, Error, Result, Tokens};
use super::{YandexService, validate_tokens};
use crate::domain::UserId;
use chrono::Utc;
use tokio_util::sync::CancellationToken;

impl YandexService {
    pub(super) async fn access_token(&self, user_id: UserId, c: &Connection) -> Result<String> {
        let result = async {
            let tokens = self.decrypt_tokens(user_id, c)?;
            if c.expires_at
                .is_some_and(|expiry| expiry > Utc::now().timestamp() + 60)
            {
                return Ok(tokens.access_token);
            }
            self.refresh_tokens(user_id, c, tokens).await
        }
        .await;
        if matches!(result, Err(Error::ReconnectRequired)) {
            self.record_failure(user_id, &c.generation, Error::ReconnectRequired)
                .await?;
        }
        result
    }

    pub(super) fn decrypt_tokens(&self, user_id: UserId, c: &Connection) -> Result<Tokens> {
        // Yandex's refresh grant expires together with the access token. After downtime
        // spanning that deadline, do not keep trying an expired grant indefinitely.
        if c.expires_at
            .is_none_or(|expiry| expiry <= Utc::now().timestamp())
        {
            return Err(Error::ReconnectRequired);
        }
        let decrypted = self.open(
            user_id,
            &c.generation,
            "tokens",
            c.encrypted_tokens.as_ref().ok_or(Error::NotConnected)?,
        )?;
        serde_json::from_slice(&decrypted).map_err(|_| Error::ReconnectRequired)
    }

    pub(super) async fn refresh_tokens(
        &self,
        user_id: UserId,
        c: &Connection,
        tokens: Tokens,
    ) -> Result<String> {
        let response = self
            .inner
            .provider
            .exchange(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &tokens.refresh_token),
            ])
            .await?;
        let (tokens, expires_at, refresh_after) =
            validate_tokens(response, Some(tokens.refresh_token))?;
        let encrypted = self.seal(
            user_id,
            &c.generation,
            "tokens",
            &serde_json::to_vec(&tokens).map_err(|_| Error::Internal)?,
        )?;
        let generation = c.generation.clone();
        let changed = self
            .inner
            .repository
            .update_tokens(user_id, generation, encrypted, expires_at, refresh_after)
            .await?;
        if !changed {
            return Err(Error::NotConnected);
        }
        Ok(tokens.access_token)
    }

    /// Refresh at most one due connection per worker tick, including disabled integrations
    /// and users with no reminders. Transient failures keep the still-valid grant usable.
    #[cfg(test)]
    pub(super) async fn refresh_tick(&self) -> Result<()> {
        self.refresh_tick_until(&CancellationToken::new()).await
    }

    pub(super) async fn refresh_tick_until(&self, shutdown: &CancellationToken) -> Result<()> {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let now = Utc::now().timestamp();
        let candidate = self.inner.repository.refresh_candidate(now).await?;
        let Some(user_id) = candidate else {
            return Ok(());
        };
        let _guard = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(()),
            guard = self.lock(user_id) => guard,
        };
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let Some(c) = self.connection(user_id).await? else {
            return Ok(());
        };
        if c.encrypted_tokens.is_none() || c.requires_reconnect {
            return Ok(());
        }
        // A request may have refreshed this connection while this worker awaited its lock.
        if c.expires_at.is_some_and(|expiry| expiry > now)
            && c.refresh_after.is_some_and(|at| at > now)
        {
            return Ok(());
        }
        let result = match self.decrypt_tokens(user_id, &c) {
            Ok(tokens) => self.refresh_tokens(user_id, &c, tokens).await.map(|_| ()),
            Err(error) => Err(error),
        };
        match result {
            Ok(()) | Err(Error::NotConnected) => Ok(()),
            Err(Error::ReconnectRequired) => {
                self.record_failure(user_id, &c.generation, Error::ReconnectRequired)
                    .await
            }
            Err(_) => {
                // Retry every 10..300 seconds, shrinking the delay near expiry. This is
                // persisted, so restarts cannot hammer a rate-limited token endpoint.
                let retry_at = Utc::now().timestamp()
                    + ((c.expires_at.unwrap_or(now) - now) / 4).clamp(10, 300);
                let generation = c.generation;
                self.inner
                    .repository
                    .defer_refresh(user_id, generation, retry_at)
                    .await?;
                Ok(())
            }
        }
    }
}
