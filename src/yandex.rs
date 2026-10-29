//! Optional, per-user official Yandex OAuth and Smart Home scenarios.
//!
//! Tokens and PKCE verifiers are encrypted at rest. No upstream response body, token,
//! authorization URL or OAuth code is included in an error or log. Scenario POSTs are
//! at-most-once: the durable claim is committed before the request and is never retried.

use std::sync::Arc;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::db::Db;

mod crypto;
mod provider;
mod worker;
use crypto::TokenCipher;
use provider::{Provider, TokenResponse};

const AUTH_LIFETIME: i64 = 600;
const ACTION_COOLDOWN: i64 = 60;
const MAX_JOB_AGE: i64 = 300;
const MAX_REFRESH_DELAY: i64 = 90 * 24 * 60 * 60;
const CONNECTION_COLUMNS: &str = "generation, encrypted_tokens, expires_at, enabled, requires_reconnect, scenario_id, scenario_name, last_error, refresh_after";

#[derive(Clone)]
pub struct YandexConfig {
    pub client_id: String,
    pub client_secret: String,
    pub token_key: [u8; 32],
    pub callback_url: Url,
}

// Config is printable without disclosing credentials or encryption material.
impl std::fmt::Debug for YandexConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YandexConfig")
            .field("callback_url", &self.callback_url)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("yandex_not_connected")]
    NotConnected,
    #[error("yandex_reconnect_required")]
    ReconnectRequired,
    #[error("yandex_scenario_missing")]
    ScenarioMissing,
    #[error("yandex_rate_limited")]
    RateLimited,
    #[error("yandex_unavailable")]
    Unavailable,
    #[error("yandex_invalid_state")]
    InvalidState,
    #[error("yandex_invalid_selection")]
    InvalidSelection,
    #[error("yandex_delivery_uncertain")]
    DeliveryUncertain,
    #[error("yandex_internal")]
    Internal,
}

impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotConnected => "yandex_not_connected",
            Self::ReconnectRequired => "yandex_reconnect_required",
            Self::ScenarioMissing => "yandex_scenario_missing",
            Self::RateLimited => "yandex_rate_limited",
            Self::Unavailable => "yandex_unavailable",
            Self::InvalidState => "yandex_invalid_state",
            Self::InvalidSelection => "yandex_invalid_selection",
            Self::DeliveryUncertain => "yandex_delivery_uncertain",
            Self::Internal => "yandex_internal",
        }
    }

    pub fn status_code(self) -> u16 {
        match self {
            Self::NotConnected | Self::ReconnectRequired | Self::ScenarioMissing => 409,
            Self::RateLimited => 429,
            Self::InvalidState | Self::InvalidSelection => 400,
            Self::Unavailable | Self::DeliveryUncertain => 502,
            Self::Internal => 500,
        }
    }
}

// Deliberately discard SQL/HTTP/body errors at this boundary. Only stable codes leave it.
impl From<anyhow::Error> for Error {
    fn from(_: anyhow::Error) -> Self {
        Self::Internal
    }
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Serialize)]
pub struct Status {
    pub available: bool,
    pub connected: bool,
    pub enabled: bool,
    pub requires_reconnect: bool,
    pub scenario_id: Option<String>,
    pub scenario_name: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub name: String,
    pub is_active: bool,
}

pub struct PendingAuth {
    pub language: String,
    pub completion_token: String,
}

impl std::fmt::Debug for PendingAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingAuth")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Yandex {
    inner: Arc<Inner>,
}

struct Inner {
    config: YandexConfig,
    db: Db,
    provider: Provider,
    cipher: TokenCipher,
    // Bounded lock set serializes refresh, settings and deliveries per user. DB generation
    // guards additionally prevent an in-flight response from resurrecting a deleted user.
    locks: [Mutex<()>; 64],
}

struct Connection {
    generation: String,
    encrypted_tokens: Option<Vec<u8>>,
    expires_at: Option<i64>,
    refresh_after: Option<i64>,
    enabled: bool,
    requires_reconnect: bool,
    scenario_id: Option<String>,
    scenario_name: Option<String>,
    last_error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
}

struct AuthState {
    user_id: i64,
    state_hash: String,
    encrypted_verifier: Vec<u8>,
    language: String,
}

enum ActionResult {
    Delivered,
    Skipped,
    Cancelled,
}

#[derive(Clone)]
struct Job {
    id: i64,
    user_id: i64,
    generation: String,
    scenario_id: String,
}

impl Yandex {
    pub fn new(config: YandexConfig, db: Db) -> anyhow::Result<Self> {
        Self::with_endpoints(
            config,
            db,
            Url::parse("https://oauth.yandex.ru/")?,
            Url::parse("https://api.iot.yandex.net/")?,
        )
    }

    fn with_endpoints(
        config: YandexConfig,
        db: Db,
        oauth_base: Url,
        api_base: Url,
    ) -> anyhow::Result<Self> {
        let provider = Provider::new(&config, oauth_base, api_base)?;
        let cipher = TokenCipher::new(&config.token_key)?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                db,
                provider,
                cipher,
                locks: std::array::from_fn(|_| Mutex::new(())),
            }),
        })
    }

    async fn lock(&self, user_id: i64) -> MutexGuard<'_, ()> {
        self.inner.locks[(user_id as u64 % 64) as usize]
            .lock()
            .await
    }

    pub async fn status(&self, user_id: i64) -> Result<Status> {
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

    pub async fn start_auth(&self, user_id: i64, language: &str) -> Result<String> {
        let _guard = self.lock(user_id).await;
        let state = random_string()?;
        let state_hash = state_digest(&state);
        let verifier = random_string()?;
        let encrypted_verifier = self.seal(user_id, &state_hash, "pkce", verifier.as_bytes())?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let generation = random_string()?;
        let now = Utc::now().timestamp();
        let language = if language == "en" { "en" } else { "ru" }.to_owned();
        let hash = state_hash.clone();
        self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM yandex_oauth_states WHERE expires_at <= ?1", [now])?;
            tx.execute("INSERT INTO yandex_connections (user_id, generation, auth_nonce) VALUES (?1, ?2, ?3) ON CONFLICT(user_id) DO UPDATE SET auth_nonce = excluded.auth_nonce", params![user_id, generation, hash])?;
            tx.execute("DELETE FROM yandex_oauth_states WHERE user_id = ?1", [user_id])?;
            tx.execute("DELETE FROM yandex_pending_grants WHERE user_id = ?1 OR expires_at <= ?2", params![user_id, now])?;
            tx.execute("INSERT INTO yandex_oauth_states (state_hash, user_id, encrypted_verifier, expires_at, language) VALUES (?1, ?2, ?3, ?4, ?5)", params![hash, user_id, encrypted_verifier, now + AUTH_LIFETIME, language])?;
            tx.commit()
        }).await?;
        let mut url = self
            .inner
            .provider
            .oauth_base
            .join("authorize")
            .map_err(|_| Error::Internal)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.inner.config.client_id)
            .append_pair("redirect_uri", self.inner.config.callback_url.as_str())
            .append_pair("scope", "iot:view iot:control")
            .append_pair("force_confirm", "yes")
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url.into())
    }

    pub async fn cancel_auth(&self, state: &str) -> Result<String> {
        let auth = self.consume_state(state).await?;
        let hash = auth.state_hash;
        self.inner.db.call(move |conn| {
            conn.execute("UPDATE yandex_connections SET auth_nonce = NULL WHERE user_id = ?1 AND auth_nonce = ?2", params![auth.user_id, hash])?;
            Ok(())
        }).await?;
        Ok(auth.language)
    }

    /// Exchanges a callback into a tentative grant. The callback receipt must be redeemed
    /// by the same authenticated Telegram user; state and PKCE alone do not prevent an
    /// attacker from sharing their own authorization URL with another Yandex account owner.
    pub async fn prepare_auth(&self, state: &str, code: &str) -> Result<PendingAuth> {
        let auth = self.consume_state(state).await?;
        let _guard = self.lock(auth.user_id).await;
        if code.is_empty() || code.len() > 2048 {
            return Err(Error::InvalidState);
        }
        let verifier = String::from_utf8(self.open(
            auth.user_id,
            &auth.state_hash,
            "pkce",
            &auth.encrypted_verifier,
        )?)
        .map_err(|_| Error::Internal)?;
        let response = self
            .inner
            .provider
            .exchange(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("code_verifier", &verifier),
            ])
            .await?;
        let (tokens, expires_at, refresh_after) = validate_tokens(response, None)?;
        // Prove the grant can access this user's official Smart Home API before storing it.
        self.inner.provider.scenarios(&tokens.access_token).await?;
        let generation = random_string()?;
        let encrypted = self.seal(
            auth.user_id,
            &generation,
            "tokens",
            &serde_json::to_vec(&tokens).map_err(|_| Error::Internal)?,
        )?;
        let mut receipt = [0; 24];
        getrandom::fill(&mut receipt).map_err(|_| Error::Internal)?;
        let completion_token = hex::encode(receipt);
        let completion_hash = state_digest(&completion_token);
        let pending_expires = Utc::now().timestamp() + AUTH_LIFETIME;
        let updated = self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            let n = tx.execute("INSERT INTO yandex_pending_grants (completion_hash, user_id, generation, encrypted_tokens, expires_at, token_expires_at, refresh_after, auth_nonce) SELECT ?1, user_id, ?2, ?3, ?4, ?5, ?6, auth_nonce FROM yandex_connections WHERE user_id = ?7 AND auth_nonce = ?8", params![completion_hash, generation, encrypted, pending_expires, expires_at, refresh_after, auth.user_id, auth.state_hash])?;
            tx.commit()?;
            Ok(n > 0)
        }).await?;
        if !updated {
            return Err(Error::InvalidState);
        }
        Ok(PendingAuth {
            language: auth.language,
            completion_token,
        })
    }

    pub async fn confirm_auth(&self, user_id: i64, completion_token: &str) -> Result<Status> {
        if completion_token.len() != 48 || !completion_token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::InvalidState);
        }
        let _guard = self.lock(user_id).await;
        let hash = state_digest(completion_token);
        let now = Utc::now().timestamp();
        let confirmed = self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM yandex_pending_grants WHERE expires_at <= ?1 OR token_expires_at <= ?1", [now])?;
            let grant: Option<(String, Vec<u8>, i64, String, i64)> = tx.query_row("DELETE FROM yandex_pending_grants WHERE completion_hash = ?1 AND user_id = ?2 RETURNING generation, encrypted_tokens, token_expires_at, auth_nonce, refresh_after", params![hash, user_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).optional()?;
            let mut confirmed = false;
            if let Some((generation, encrypted, expires_at, nonce, refresh_after)) = grant {
                confirmed = tx.execute("UPDATE yandex_connections SET generation = ?1, encrypted_tokens = ?2, expires_at = ?3, refresh_after = ?4, enabled = 0, requires_reconnect = 0, scenario_id = NULL, scenario_name = NULL, last_error = NULL, retry_after = NULL, auth_nonce = NULL WHERE user_id = ?5 AND auth_nonce = ?6", params![generation, encrypted, expires_at, refresh_after, user_id, nonce])? > 0;
                if confirmed { tx.execute("DELETE FROM yandex_jobs WHERE user_id = ?1", [user_id])?; }
            }
            tx.commit()?;
            Ok(confirmed)
        }).await?;
        if !confirmed {
            return Err(Error::InvalidState);
        }
        self.status(user_id).await
    }

    async fn consume_state(&self, state: &str) -> Result<AuthState> {
        if state.len() != 43
            || !state
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(Error::InvalidState);
        }
        let hash = state_digest(state);
        let now = Utc::now().timestamp();
        let auth = self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            // DELETE RETURNING consumes even an expired state, preventing replay.
            let row: Option<(i64, Vec<u8>, i64, String)> = tx.query_row("DELETE FROM yandex_oauth_states WHERE state_hash = ?1 RETURNING user_id, encrypted_verifier, expires_at, language", [&hash], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()?;
            tx.commit()?;
            Ok(row.filter(|row| row.2 > now).map(|(user_id, encrypted_verifier, _, language)| AuthState { user_id, state_hash: hash, encrypted_verifier, language }))
        }).await?;
        auth.ok_or(Error::InvalidState)
    }

    pub async fn scenarios(&self, user_id: i64) -> Result<Vec<Scenario>> {
        let _guard = self.lock(user_id).await;
        let c = self.require_connection(user_id).await?;
        let result = async {
            let token = self.access_token(user_id, &c).await?;
            self.inner.provider.scenarios(&token).await
        }
        .await;
        match &result {
            Err(error) => self.record_failure(user_id, &c.generation, *error).await?,
            Ok(_) => self.clear_temporary_failure(user_id, &c.generation).await?,
        }
        result
    }

    pub async fn configure(
        &self,
        user_id: i64,
        scenario_id: &str,
        enabled: bool,
    ) -> Result<Status> {
        let _guard = self.lock(user_id).await;
        let c = self.require_connection(user_id).await?;
        let selected = if !enabled && scenario_id.is_empty() {
            None
        } else if !enabled && c.scenario_id.as_deref() == Some(scenario_id) {
            // Switching off must work even when the provider is unavailable.
            Some(Scenario {
                id: scenario_id.into(),
                name: c.scenario_name.clone().unwrap_or_default(),
                is_active: true,
            })
        } else {
            let token = self.access_token(user_id, &c).await?;
            let scenarios = match self.inner.provider.scenarios(&token).await {
                Ok(scenarios) => scenarios,
                Err(error) => {
                    self.record_failure(user_id, &c.generation, error).await?;
                    return Err(error);
                }
            };
            Some(
                scenarios
                    .into_iter()
                    .find(|s| s.id == scenario_id && s.is_active)
                    .ok_or(Error::InvalidSelection)?,
            )
        };
        let generation = c.generation;
        let changed = self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            let n = tx.execute("UPDATE yandex_connections SET scenario_id = ?1, scenario_name = ?2, enabled = ?3, last_error = NULL, retry_after = NULL WHERE user_id = ?4 AND generation = ?5 AND encrypted_tokens IS NOT NULL AND requires_reconnect = 0", params![selected.as_ref().map(|s| &s.id), selected.as_ref().map(|s| &s.name), enabled, user_id, generation])?;
            // A new setting applies to future occurrences, never an old queued reminder.
            tx.execute("DELETE FROM yandex_jobs WHERE user_id = ?1 AND state = 'pending'", [user_id])?;
            tx.commit()?;
            Ok(n > 0)
        }).await?;
        if !changed {
            return Err(Error::NotConnected);
        }
        self.status(user_id).await
    }

    pub async fn disconnect(&self, user_id: i64) -> Result<()> {
        let _guard = self.lock(user_id).await;
        self.inner
            .db
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM yandex_connections WHERE user_id = ?1",
                    [user_id],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn test(&self, user_id: i64) -> Result<()> {
        let _guard = self.lock(user_id).await;
        let c = self.require_connection(user_id).await?;
        let scenario_id = c.scenario_id.clone().ok_or(Error::ScenarioMissing)?;
        self.reserve_action(user_id, &c.generation).await?;
        let result = self.execute(user_id, &c, &scenario_id, None).await;
        if let Err(error) = result {
            self.record_failure(user_id, &c.generation, error).await?;
        }
        result
    }

    /// Tests use the same preparation/dispatch boundary as queued delivery.
    async fn execute(
        &self,
        user_id: i64,
        c: &Connection,
        scenario_id: &str,
        job: Option<&Job>,
    ) -> Result<()> {
        let shutdown = CancellationToken::new();
        let Some(token) = self
            .prepare_action(user_id, c, scenario_id, &shutdown)
            .await?
        else {
            return Ok(());
        };
        self.dispatch_action(user_id, c, scenario_id, job, &token, &shutdown)
            .await
            .map(|_| ())
    }

    /// Preparation is safe to retry: it may refresh credentials and read scenarios, but
    /// does not execute a device action. No durable action claim exists at this stage.
    async fn prepare_action(
        &self,
        user_id: i64,
        c: &Connection,
        scenario_id: &str,
        shutdown: &CancellationToken,
    ) -> Result<Option<String>> {
        if shutdown.is_cancelled() {
            return Ok(None);
        }
        // Do not interrupt a token refresh halfway through: a provider may rotate the grant.
        let token = self.access_token(user_id, c).await?;
        if shutdown.is_cancelled() {
            return Ok(None);
        }
        let scenarios = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(None),
            result = self.inner.provider.scenarios(&token) => result?,
        };
        if !scenarios.iter().any(|s| s.id == scenario_id && s.is_active) {
            return Err(Error::ScenarioMissing);
        }
        Ok(Some(token))
    }

    /// Check cancellation/ownership immediately before the external side effect. Once
    /// POST begins, allow its bounded HTTP deadline to finish; never cancel and replay it.
    async fn dispatch_action(
        &self,
        user_id: i64,
        c: &Connection,
        scenario_id: &str,
        job: Option<&Job>,
        token: &str,
        shutdown: &CancellationToken,
    ) -> Result<ActionResult> {
        if let Some(job) = job {
            if !self.job_valid(job.clone()).await? {
                return Ok(ActionResult::Skipped);
            }
        } else {
            let current = self.require_connection(user_id).await?;
            if current.generation != c.generation
                || current.scenario_id.as_deref() != Some(scenario_id)
            {
                return Err(Error::NotConnected);
            }
        }
        if shutdown.is_cancelled() {
            return Ok(ActionResult::Cancelled);
        }
        self.inner.provider.action(token, scenario_id).await?;
        self.clear_temporary_failure(user_id, &c.generation).await?;
        Ok(ActionResult::Delivered)
    }

    async fn require_connection(&self, user_id: i64) -> Result<Connection> {
        let c = self.connection(user_id).await?.ok_or(Error::NotConnected)?;
        if c.requires_reconnect {
            return Err(Error::ReconnectRequired);
        }
        if c.encrypted_tokens.is_none() {
            return Err(Error::NotConnected);
        }
        Ok(c)
    }

    async fn connection(&self, user_id: i64) -> Result<Option<Connection>> {
        Ok(self
            .inner
            .db
            .call(move |conn| {
                conn.query_row(
                    &format!(
                        "SELECT {CONNECTION_COLUMNS} FROM yandex_connections WHERE user_id = ?1"
                    ),
                    [user_id],
                    connection_from_row,
                )
                .optional()
            })
            .await?)
    }

    async fn access_token(&self, user_id: i64, c: &Connection) -> Result<String> {
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

    fn decrypt_tokens(&self, user_id: i64, c: &Connection) -> Result<Tokens> {
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

    async fn refresh_tokens(&self, user_id: i64, c: &Connection, tokens: Tokens) -> Result<String> {
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
        let changed = self.inner.db.call(move |conn| Ok(conn.execute("UPDATE yandex_connections SET encrypted_tokens = ?1, expires_at = ?2, refresh_after = ?3 WHERE user_id = ?4 AND generation = ?5 AND encrypted_tokens IS NOT NULL AND requires_reconnect = 0", params![encrypted, expires_at, refresh_after, user_id, generation])? > 0)).await?;
        if !changed {
            return Err(Error::NotConnected);
        }
        Ok(tokens.access_token)
    }

    /// Refresh at most one due connection per worker tick, including disabled integrations
    /// and users with no reminders. Transient failures keep the still-valid grant usable.
    #[cfg(test)]
    async fn refresh_tick(&self) -> Result<()> {
        self.refresh_tick_until(&CancellationToken::new()).await
    }

    async fn refresh_tick_until(&self, shutdown: &CancellationToken) -> Result<()> {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let now = Utc::now().timestamp();
        let candidate = self.inner.db.call(move |conn| {
            conn.query_row("SELECT user_id FROM yandex_connections WHERE encrypted_tokens IS NOT NULL AND requires_reconnect = 0 AND (expires_at IS NULL OR expires_at <= ?1 OR refresh_after IS NULL OR refresh_after <= ?1) ORDER BY COALESCE(refresh_after, 0), user_id LIMIT 1", [now], |row| row.get::<_, i64>(0)).optional()
        }).await?;
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
                self.inner.db.call(move |conn| {
                    conn.execute("UPDATE yandex_connections SET refresh_after = ?1 WHERE user_id = ?2 AND generation = ?3 AND encrypted_tokens IS NOT NULL AND requires_reconnect = 0", params![retry_at, user_id, generation])?;
                    Ok(())
                }).await?;
                Ok(())
            }
        }
    }

    async fn reserve_action(&self, user_id: i64, generation: &str) -> Result<()> {
        let generation = generation.to_owned();
        let now = Utc::now().timestamp();
        let reserved = self.inner.db.call(move |conn| Ok(conn.execute("UPDATE yandex_connections SET last_action_at = ?1 WHERE user_id = ?2 AND generation = ?3 AND encrypted_tokens IS NOT NULL AND requires_reconnect = 0 AND (last_action_at IS NULL OR last_action_at <= ?4)", params![now, user_id, generation, now - ACTION_COOLDOWN])? > 0)).await?;
        if reserved {
            Ok(())
        } else {
            Err(Error::RateLimited)
        }
    }

    /// Availability errors during reads/preparation do not revoke the user's choice
    /// to enable voice. Only a lost grant, missing scenario or uncertain action stops it.
    async fn record_failure(&self, user_id: i64, generation: &str, error: Error) -> Result<()> {
        if error == Error::NotConnected {
            return Ok(());
        }
        let generation = generation.to_owned();
        let terminal = matches!(
            error,
            Error::ReconnectRequired | Error::ScenarioMissing | Error::DeliveryUncertain
        );
        let reconnect = error == Error::ReconnectRequired;
        let retry_after =
            Utc::now().timestamp() + if error == Error::RateLimited { 60 } else { 10 };
        self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            if terminal {
                tx.execute("UPDATE yandex_connections SET enabled = 0, last_error = ?1, requires_reconnect = ?2, encrypted_tokens = CASE WHEN ?2 THEN NULL ELSE encrypted_tokens END, expires_at = CASE WHEN ?2 THEN NULL ELSE expires_at END, refresh_after = CASE WHEN ?2 THEN NULL ELSE refresh_after END, retry_after = NULL WHERE user_id = ?3 AND generation = ?4", params![error.code(), reconnect, user_id, generation])?;
                tx.execute("DELETE FROM yandex_jobs WHERE user_id = ?1 AND generation = ?2 AND state = 'pending'", params![user_id, generation])?;
            } else {
                tx.execute("UPDATE yandex_connections SET last_error = ?1, retry_after = ?2 WHERE user_id = ?3 AND generation = ?4", params![error.code(), retry_after, user_id, generation])?;
            }
            tx.commit()
        }).await?;
        Ok(())
    }

    async fn clear_temporary_failure(&self, user_id: i64, generation: &str) -> Result<()> {
        let generation = generation.to_owned();
        self.inner.db.call(move |conn| {
            conn.execute("UPDATE yandex_connections SET last_error = NULL, retry_after = NULL WHERE user_id = ?1 AND generation = ?2 AND last_error IN ('yandex_unavailable', 'yandex_rate_limited', 'yandex_internal')", params![user_id, generation])?;
            Ok(())
        }).await?;
        Ok(())
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

fn connection_from_row(row: &Row<'_>) -> rusqlite::Result<Connection> {
    Ok(Connection {
        generation: row.get(0)?,
        encrypted_tokens: row.get(1)?,
        expires_at: row.get(2)?,
        enabled: row.get(3)?,
        requires_reconnect: row.get(4)?,
        scenario_id: row.get(5)?,
        scenario_name: row.get(6)?,
        last_error: row.get(7)?,
        refresh_after: row.get(8)?,
    })
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
