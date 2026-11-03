//! Optional, per-user official Yandex OAuth and Smart Home scenarios.
//!
//! Tokens and PKCE verifiers are encrypted at rest. No upstream response body, token,
//! authorization URL or OAuth code is included in an error or log. Scenario POSTs are
//! at-most-once: the durable claim is committed before the request and is never retried.

use std::sync::Arc;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, MutexGuard};

const AUTH_LIFETIME: i64 = 600;
const MAX_REFRESH_DELAY: i64 = 90 * 24 * 60 * 60;

mod auth;
mod failures;
pub(crate) mod ports;
mod scenarios;
mod tokens;
pub(crate) mod types;
mod worker;

use ports::{TokenVault, YandexProvider, YandexRepository};
pub(crate) use types::Error;
use types::{Result, Status, TokenResponse, Tokens};

#[derive(Clone)]
pub(crate) struct YandexService {
    inner: Arc<Inner>,
}

struct Inner {
    repository: Arc<dyn YandexRepository>,
    provider: Arc<dyn YandexProvider>,
    cipher: Arc<dyn TokenVault>,
    // Account locks plus persistent generation checks protect against stale responses.
    locks: [Mutex<()>; 64],
}

impl YandexService {
    pub(crate) fn from_dependencies(
        repository: impl YandexRepository,
        provider: impl YandexProvider,
        cipher: impl TokenVault,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                repository: Arc::new(repository),
                provider: Arc::new(provider),
                cipher: Arc::new(cipher),
                locks: std::array::from_fn(|_| Mutex::new(())),
            }),
        }
    }

    async fn lock(&self, user_id: i64) -> MutexGuard<'_, ()> {
        self.inner.locks[(user_id as u64 % 64) as usize]
            .lock()
            .await
    }

    pub(crate) async fn status(&self, user_id: i64) -> Result<Status> {
        let c = self.connection(user_id).await?;
        Ok(Status {
            available: true,
            connected: c
                .as_ref()
                .is_some_and(|c| c.encrypted_tokens.is_some() && !c.requires_reconnect),
            enabled: c.as_ref().is_some_and(|c| c.enabled),
            requires_reconnect: c.as_ref().is_some_and(|c| c.requires_reconnect),
            scenario_id: c.as_ref().and_then(|c| c.scenario_id.clone()),
            scenario_name: c.as_ref().and_then(|c| c.scenario_name.clone()),
            last_error: c.and_then(|c| c.last_error),
        })
    }

    fn seal(
        &self,
        user_id: i64,
        generation: &str,
        kind: &str,
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        self.inner.cipher.seal(user_id, generation, kind, plaintext)
    }

    fn open(&self, user_id: i64, generation: &str, kind: &str, envelope: &[u8]) -> Result<Vec<u8>> {
        self.inner.cipher.open(user_id, generation, kind, envelope)
    }
}

fn random_string() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Internal)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn state_digest(state: &str) -> String {
    hex::encode(Sha256::digest(state.as_bytes()))
}

fn validate_tokens(
    response: TokenResponse,
    old_refresh: Option<String>,
) -> Result<(Tokens, i64, i64)> {
    if !response.token_type.eq_ignore_ascii_case("bearer")
        || response.access_token.is_empty()
        || response.expires_in <= 0
    {
        return Err(Error::ReconnectRequired);
    }
    if let Some(scope) = &response.scope
        && !["iot:view", "iot:control"]
            .iter()
            .all(|required| scope.split_whitespace().any(|scope| scope == *required))
    {
        return Err(Error::ReconnectRequired);
    }
    let refresh_token = response
        .refresh_token
        .or(old_refresh)
        .filter(|s| !s.is_empty())
        .ok_or(Error::ReconnectRequired)?;
    let issued_at = Utc::now().timestamp();
    let refresh_delay =
        (response.expires_in / 4 * 3 + response.expires_in % 4 * 3 / 4).clamp(1, MAX_REFRESH_DELAY);
    let expires_at = issued_at
        .checked_add(response.expires_in)
        .ok_or(Error::ReconnectRequired)?;
    Ok((
        Tokens {
            access_token: response.access_token,
            refresh_token,
        },
        expires_at,
        issued_at + refresh_delay,
    ))
}

#[cfg(test)]
mod tests;
