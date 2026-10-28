//! Optional, per-user official Yandex OAuth and Smart Home scenarios.
//!
//! Tokens and PKCE verifiers are encrypted at rest. No upstream response body, token,
//! authorization URL or OAuth code is included in an error or log. Scenario POSTs are
//! at-most-once: the durable claim is committed before the request and is never retried.

use std::sync::Arc;
use std::time::Duration;

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use reqwest::{Client, StatusCode};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::db::Db;

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
    http: Client,
    cipher: Aes256Gcm,
    oauth_base: Url,
    api_base: Url,
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

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    token_type: String,
    expires_in: i64,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct UserInfo {
    status: String,
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize)]
struct ActionResponse {
    status: String,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
}

struct AuthState {
    user_id: i64,
    state_hash: String,
    encrypted_verifier: Vec<u8>,
    language: String,
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
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(6))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()?;
        let cipher = Aes256Gcm::new_from_slice(&config.token_key)
            .map_err(|_| anyhow::anyhow!("invalid encryption key"))?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                db,
                http,
                cipher,
                oauth_base,
                api_base,
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
            .exchange(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("code_verifier", &verifier),
            ])
            .await?;
        let (tokens, expires_at, refresh_after) = validate_tokens(response, None)?;
        // Prove the grant can access this user's official Smart Home API before storing it.
        self.fetch_scenarios(&tokens.access_token).await?;
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
                confirmed = tx.execute("UPDATE yandex_connections SET generation = ?1, encrypted_tokens = ?2, expires_at = ?3, refresh_after = ?4, enabled = 0, requires_reconnect = 0, scenario_id = NULL, scenario_name = NULL, last_error = NULL, auth_nonce = NULL WHERE user_id = ?5 AND auth_nonce = ?6", params![generation, encrypted, expires_at, refresh_after, user_id, nonce])? > 0;
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
            self.fetch_scenarios(&token).await
        }
        .await;
        if let Err(error) = result {
            self.record_failure(user_id, &c.generation, error).await?;
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
            let scenarios = match self.fetch_scenarios(&token).await {
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
            let n = tx.execute("UPDATE yandex_connections SET scenario_id = ?1, scenario_name = ?2, enabled = ?3, last_error = NULL WHERE user_id = ?4 AND generation = ?5 AND encrypted_tokens IS NOT NULL AND requires_reconnect = 0", params![selected.as_ref().map(|s| &s.id), selected.as_ref().map(|s| &s.name), enabled, user_id, generation])?;
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

    async fn execute(
        &self,
        user_id: i64,
        c: &Connection,
        scenario_id: &str,
        job: Option<&Job>,
    ) -> Result<()> {
        let token = self.access_token(user_id, c).await?;
        let scenarios = self.fetch_scenarios(&token).await?;
        if !scenarios.iter().any(|s| s.id == scenario_id && s.is_active) {
            return Err(Error::ScenarioMissing);
        }
        if let Some(job) = job {
            if !self.job_valid(job.clone()).await? {
                return Ok(());
            }
        } else {
            let current = self.require_connection(user_id).await?;
            if current.generation != c.generation
                || current.scenario_id.as_deref() != Some(scenario_id)
            {
                return Err(Error::NotConnected);
            }
        }
        let mut url = self.inner.api_base.clone();
        url.path_segments_mut()
            .map_err(|_| Error::Internal)?
            .extend(["v1.0", "scenarios", scenario_id, "actions"]);
        // No retry, even for a network error or server failure: the action might have run.
        let response = self
            .inner
            .http
            .post(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| Error::DeliveryUncertain)?;
        match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(Error::ReconnectRequired);
            }
            StatusCode::NOT_FOUND => return Err(Error::ScenarioMissing),
            StatusCode::TOO_MANY_REQUESTS => return Err(Error::RateLimited),
            status if !status.is_success() => return Err(Error::DeliveryUncertain),
            _ => {}
        }
        let body: ActionResponse = response
            .json()
            .await
            .map_err(|_| Error::DeliveryUncertain)?;
        if body.status != "ok" {
            return Err(Error::DeliveryUncertain);
        }
        Ok(())
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
    async fn refresh_tick(&self) -> Result<()> {
        let now = Utc::now().timestamp();
        let candidate = self.inner.db.call(move |conn| {
            conn.query_row("SELECT user_id FROM yandex_connections WHERE encrypted_tokens IS NOT NULL AND requires_reconnect = 0 AND (expires_at IS NULL OR expires_at <= ?1 OR refresh_after IS NULL OR refresh_after <= ?1) ORDER BY COALESCE(refresh_after, 0), user_id LIMIT 1", [now], |row| row.get::<_, i64>(0)).optional()
        }).await?;
        let Some(user_id) = candidate else {
            return Ok(());
        };
        let _guard = self.lock(user_id).await;
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

    async fn exchange(&self, fields: &[(&str, &str)]) -> Result<TokenResponse> {
        let url = self
            .inner
            .oauth_base
            .join("token")
            .map_err(|_| Error::Internal)?;
        let response = self
            .inner
            .http
            .post(url)
            .basic_auth(
                &self.inner.config.client_id,
                Some(&self.inner.config.client_secret),
            )
            .form(fields)
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(Error::RateLimited);
        }
        if response.status().is_client_error() {
            // A rate limit, malformed request or app-level problem does not prove the
            // user's refresh grant is revoked. Preserve their credentials in those cases.
            let error: TokenError = response.json().await.map_err(|_| Error::Unavailable)?;
            return Err(match error.error.as_str() {
                "invalid_grant" | "invalid_scope" => Error::ReconnectRequired,
                _ => Error::Unavailable,
            });
        }
        if !response.status().is_success() {
            return Err(Error::Unavailable);
        }
        response.json().await.map_err(|_| Error::Unavailable)
    }

    async fn fetch_scenarios(&self, token: &str) -> Result<Vec<Scenario>> {
        let url = self
            .inner
            .api_base
            .join("v1.0/user/info")
            .map_err(|_| Error::Internal)?;
        let response = self
            .inner
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(Error::ReconnectRequired);
            }
            StatusCode::TOO_MANY_REQUESTS => return Err(Error::RateLimited),
            status if !status.is_success() => return Err(Error::Unavailable),
            _ => {}
        }
        let info: UserInfo = response.json().await.map_err(|_| Error::Unavailable)?;
        if info.status != "ok" {
            return Err(Error::Unavailable);
        }
        Ok(info.scenarios)
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

    async fn record_failure(&self, user_id: i64, generation: &str, error: Error) -> Result<()> {
        if error == Error::NotConnected {
            return Ok(());
        }
        let generation = generation.to_owned();
        let reconnect = error == Error::ReconnectRequired;
        self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("UPDATE yandex_connections SET enabled = 0, last_error = ?1, requires_reconnect = ?2, encrypted_tokens = CASE WHEN ?2 THEN NULL ELSE encrypted_tokens END, expires_at = CASE WHEN ?2 THEN NULL ELSE expires_at END, refresh_after = CASE WHEN ?2 THEN NULL ELSE refresh_after END WHERE user_id = ?3 AND generation = ?4", params![error.code(), reconnect, user_id, generation])?;
            tx.execute("DELETE FROM yandex_jobs WHERE user_id = ?1 AND generation = ?2 AND state = 'pending'", params![user_id, generation])?;
            tx.commit()
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
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).map_err(|_| Error::Internal)?;
        let aad = format!("napominalochka:yandex:v1:{user_id}:{generation}:{kind}");
        let encrypted = self
            .inner
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| Error::Internal)?;
        let mut envelope = vec![1];
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&encrypted);
        Ok(envelope)
    }

    fn open(&self, user_id: i64, generation: &str, kind: &str, envelope: &[u8]) -> Result<Vec<u8>> {
        if envelope.len() < 29 || envelope[0] != 1 {
            return Err(Error::ReconnectRequired);
        }
        let aad = format!("napominalochka:yandex:v1:{user_id}:{generation}:{kind}");
        self.inner
            .cipher
            .decrypt(
                Nonce::from_slice(&envelope[1..13]),
                Payload {
                    msg: &envelope[13..],
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| Error::ReconnectRequired)
    }

    pub async fn run_worker(&self, shutdown: CancellationToken) {
        if self.recover_claims().await.is_err() {
            tracing::error!("Yandex queue recovery failed");
        }
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = timer.tick() => {
                    if self.worker_tick().await.is_err() { tracing::warn!("Yandex queue operation failed"); }
                    if self.refresh_tick().await.is_err() { tracing::warn!("Yandex token maintenance failed"); }
                }
            }
        }
    }

    async fn recover_claims(&self) -> Result<()> {
        self.inner.db.call(|conn| {
            let tx = conn.transaction()?;
            tx.execute("UPDATE yandex_connections SET enabled = 0, last_error = 'yandex_delivery_uncertain' WHERE EXISTS (SELECT 1 FROM yandex_jobs j WHERE j.user_id = yandex_connections.user_id AND j.generation = yandex_connections.generation AND j.state = 'claimed')", [])?;
            tx.execute("UPDATE yandex_jobs SET state = 'failed', last_error = 'yandex_delivery_uncertain' WHERE state = 'claimed'", [])?;
            tx.commit()
        }).await?;
        Ok(())
    }

    async fn worker_tick(&self) -> Result<()> {
        let Some(job) = self.next_job().await? else {
            return Ok(());
        };
        let _guard = self.lock(job.user_id).await;
        // Recheck under the same user lock used by disconnect, test and configure.
        if !self.claim_job(job.clone()).await? {
            return Ok(());
        }
        let result = match self.require_connection(job.user_id).await {
            Ok(c) => {
                self.execute(job.user_id, &c, &job.scenario_id, Some(&job))
                    .await
            }
            Err(e) => Err(e),
        };
        if let Err(error) = result {
            self.record_failure(job.user_id, &job.generation, error)
                .await?;
        }
        self.inner.db.call(move |conn| {
            conn.execute("UPDATE yandex_jobs SET state = ?1, last_error = ?2 WHERE job_id = ?3 AND state = 'claimed'", params![if result.is_ok() { "delivered" } else { "failed" }, result.err().map(Error::code), job.id])?;
            Ok(())
        }).await?;
        Ok(())
    }

    async fn next_job(&self) -> Result<Option<Job>> {
        let now = Utc::now().timestamp();
        Ok(self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("UPDATE yandex_jobs SET state = 'skipped' WHERE state = 'pending' AND (created_at < ?1 OR NOT EXISTS (SELECT 1 FROM yandex_connections c WHERE c.user_id = yandex_jobs.user_id AND c.generation = yandex_jobs.generation AND c.scenario_id = yandex_jobs.scenario_id AND c.enabled = 1 AND c.requires_reconnect = 0 AND c.encrypted_tokens IS NOT NULL))", [now - MAX_JOB_AGE])?;
            // Terminal records are operational metadata, retained for at most one day.
            tx.execute("DELETE FROM yandex_jobs WHERE state IN ('delivered', 'failed', 'skipped') AND created_at < ?1", [now - 86400])?;
            tx.execute("DELETE FROM yandex_oauth_states WHERE expires_at <= ?1", [now])?;
            tx.execute("DELETE FROM yandex_pending_grants WHERE expires_at <= ?1", [now])?;
            let job = tx.query_row("SELECT j.job_id, j.user_id, j.generation, j.scenario_id FROM yandex_jobs j JOIN yandex_connections c ON c.user_id = j.user_id WHERE j.state = 'pending' AND (c.last_action_at IS NULL OR c.last_action_at <= ?1) ORDER BY j.created_at, j.job_id LIMIT 1", [now - ACTION_COOLDOWN], |row| Ok(Job { id: row.get(0)?, user_id: row.get(1)?, generation: row.get(2)?, scenario_id: row.get(3)? })).optional()?;
            tx.commit()?;
            Ok(job)
        }).await?)
    }

    async fn claim_job(&self, job: Job) -> Result<bool> {
        let now = Utc::now().timestamp();
        Ok(self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            let changed = tx.execute("UPDATE yandex_jobs SET state = 'claimed' WHERE job_id = ?1 AND state = 'pending' AND created_at >= ?2 AND EXISTS (SELECT 1 FROM yandex_connections c WHERE c.user_id = yandex_jobs.user_id AND c.generation = yandex_jobs.generation AND c.scenario_id = yandex_jobs.scenario_id AND c.enabled = 1 AND c.requires_reconnect = 0 AND c.encrypted_tokens IS NOT NULL AND (c.last_action_at IS NULL OR c.last_action_at <= ?3))", params![job.id, now - MAX_JOB_AGE, now - ACTION_COOLDOWN])? > 0;
            if changed { tx.execute("UPDATE yandex_connections SET last_action_at = ?1 WHERE user_id = ?2 AND generation = ?3", params![now, job.user_id, job.generation])?; }
            tx.commit()?;
            Ok(changed)
        }).await?)
    }

    async fn job_valid(&self, job: Job) -> Result<bool> {
        let now = Utc::now().timestamp();
        Ok(self.inner.db.call(move |conn| conn.query_row("SELECT EXISTS (SELECT 1 FROM yandex_jobs j JOIN yandex_connections c ON j.user_id = c.user_id WHERE j.job_id = ?1 AND j.state = 'claimed' AND j.created_at >= ?2 AND j.generation = c.generation AND j.scenario_id = c.scenario_id AND c.enabled = 1 AND c.requires_reconnect = 0 AND c.encrypted_tokens IS NOT NULL)", params![job.id, now - MAX_JOB_AGE], |row| row.get(0))).await?)
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
mod tests {
    use super::*;
    use crate::model::{Recurrence, User, UserTz, UtcOffset};
    use axum::{
        Form, Json, Router,
        extract::{Path, State},
        http::HeaderMap,
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicUsize, Ordering};

    #[derive(Default)]
    struct Mock {
        actions: AtomicUsize,
        refreshes: AtomicUsize,
        token_requests: AtomicUsize,
        token_lifetime: AtomicI64,
        post_status: AtomicU16,
        token_status: AtomicU16,
        info_status: AtomicU16,
        omit_scope: AtomicBool,
        reduced_scope: AtomicBool,
        block_token: AtomicBool,
        token_entered: tokio::sync::Notify,
        token_release: tokio::sync::Notify,
    }

    async fn mock_token(
        State(mock): State<Arc<Mock>>,
        headers: HeaderMap,
        Form(fields): Form<HashMap<String, String>>,
    ) -> Response {
        mock.token_requests.fetch_add(1, Ordering::SeqCst);
        assert!(
            headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("Basic ")
        );
        let status = mock.token_status.load(Ordering::SeqCst);
        if status != 0 {
            return (
                StatusCode::from_u16(status).unwrap(),
                Json(serde_json::json!({ "error": "temporarily_unavailable" })),
            )
                .into_response();
        }
        if mock.block_token.load(Ordering::SeqCst) {
            mock.token_entered.notify_one();
            mock.token_release.notified().await;
        }
        let tenant = if fields.get("grant_type").unwrap() == "refresh_token" {
            mock.refreshes.fetch_add(1, Ordering::SeqCst);
            fields
                .get("refresh_token")
                .unwrap()
                .strip_prefix("refresh-")
                .unwrap()
                .to_owned()
        } else {
            assert_eq!(fields.get("code_verifier").unwrap().len(), 43);
            fields.get("code").unwrap().clone()
        };
        let lifetime = match mock.token_lifetime.load(Ordering::SeqCst) {
            0 => 3600,
            value => value,
        };
        let mut token = serde_json::json!({ "token_type": "bearer", "access_token": format!("access-{tenant}"), "refresh_token": format!("refresh-{tenant}"), "expires_in": lifetime, "scope": if mock.reduced_scope.load(Ordering::SeqCst) { "iot:view" } else { "iot:view iot:control" } });
        if mock.omit_scope.load(Ordering::SeqCst) {
            token.as_object_mut().unwrap().remove("scope");
        }
        Json(token).into_response()
    }

    async fn mock_info(State(mock): State<Arc<Mock>>, headers: HeaderMap) -> Response {
        let status = mock.info_status.load(Ordering::SeqCst);
        if status != 0 {
            return (
                StatusCode::from_u16(status).unwrap(),
                Json(serde_json::json!({ "status": "error" })),
            )
                .into_response();
        }
        let authorization = headers.get("authorization").unwrap().to_str().unwrap();
        let tenant = authorization.strip_prefix("Bearer access-").unwrap();
        Json(serde_json::json!({ "status": "ok", "scenarios": [{ "id": format!("scenario-{tenant}"), "name": format!("Scenario {tenant}"), "is_active": true }, { "id": format!("inactive-{tenant}"), "name": "Inactive", "is_active": false }] })).into_response()
    }

    async fn mock_action(
        State(mock): State<Arc<Mock>>,
        headers: HeaderMap,
        Path(scenario): Path<String>,
    ) -> Response {
        let tenant = headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("Bearer access-")
            .unwrap();
        assert_eq!(scenario, format!("scenario-{tenant}"));
        mock.actions.fetch_add(1, Ordering::SeqCst);
        let status = mock.post_status.load(Ordering::SeqCst);
        if status == 599 {
            // The provider accepted the action but the response never reaches us.
            return std::future::pending().await;
        }
        (
            if status == 0 {
                StatusCode::OK
            } else {
                StatusCode::from_u16(status).unwrap()
            },
            Json(serde_json::json!({ "status": if status == 0 { "ok" } else { "error" } })),
        )
            .into_response()
    }

    struct Fixture {
        yandex: Yandex,
        db: Db,
        mock: Arc<Mock>,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    impl Fixture {
        async fn new() -> Self {
            let db = Db::open_in_memory().unwrap();
            let mock = Arc::new(Mock::default());
            let app = Router::new()
                .route("/token", post(mock_token))
                .route("/v1.0/user/info", get(mock_info))
                .route("/v1.0/scenarios/{id}/actions", post(mock_action))
                .with_state(mock.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let config = YandexConfig {
                client_id: "test-client".into(),
                client_secret: "test-secret".into(),
                token_key: [7; 32],
                callback_url: Url::parse(
                    "https://reminders.example.com/integrations/yandex/callback",
                )
                .unwrap(),
            };
            let yandex = Yandex::with_endpoints(config, db.clone(), base.clone(), base).unwrap();
            Self {
                yandex,
                db,
                mock,
                server,
            }
        }

        async fn auth(&self, user_id: i64, tenant: &str) {
            let url = self.yandex.start_auth(user_id, "en").await.unwrap();
            let pending = self
                .yandex
                .prepare_auth(&state(&url), tenant)
                .await
                .unwrap();
            assert_eq!(pending.language, "en");
            self.yandex
                .confirm_auth(user_id, &pending.completion_token)
                .await
                .unwrap();
        }

        async fn ready(&self, user_id: i64, tenant: &str) {
            self.auth(user_id, tenant).await;
            self.yandex
                .configure(user_id, &format!("scenario-{tenant}"), true)
                .await
                .unwrap();
            self.db
                .upsert_user(User {
                    id: user_id,
                    first_name: "Ada".into(),
                    last_name: String::new(),
                    tz: UserTz::Fixed(UtcOffset::from_minutes(0).unwrap()),
                })
                .await
                .unwrap();
        }

        async fn fire(&self, user_id: i64) -> crate::model::Nagging {
            let now = Utc::now();
            let reminder = self
                .db
                .add_reminder(user_id, "Test reminder".into(), now, Recurrence::Once)
                .await
                .unwrap();
            self.db
                .fire_reminder(reminder.id, now, now)
                .await
                .unwrap()
                .unwrap()
        }

        async fn count(&self, table: &'static str) -> i64 {
            self.db
                .call(move |conn| {
                    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                })
                .await
                .unwrap()
        }
    }

    fn state(url: &str) -> String {
        Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned()
    }

    #[tokio::test]
    async fn oauth_pkce_state_is_opaque_one_use_and_expires() {
        let f = Fixture::new().await;
        let url = f.yandex.start_auth(12345, "en").await.unwrap();
        let query: HashMap<String, String> = Url::parse(&url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(query["scope"], "iot:view iot:control");
        assert_eq!(query["code_challenge_method"], "S256");
        let s = state(&url);
        assert!(!s.contains("12345"));
        let auth = f.yandex.consume_state(&s).await.unwrap();
        assert_eq!(auth.user_id, 12345);
        let verifier = f
            .yandex
            .open(
                auth.user_id,
                &auth.state_hash,
                "pkce",
                &auth.encrypted_verifier,
            )
            .unwrap();
        assert_eq!(
            query["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier))
        );
        assert!(matches!(
            f.yandex.consume_state(&s).await,
            Err(Error::InvalidState)
        ));
        let old = f.yandex.start_auth(1, "ru").await.unwrap();
        let new = f.yandex.start_auth(1, "ru").await.unwrap();
        assert_eq!(
            f.yandex.cancel_auth(&state(&old)).await.unwrap_err(),
            Error::InvalidState
        );
        f.db.call(|conn| {
            conn.execute("UPDATE yandex_oauth_states SET expires_at = 0", [])?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            f.yandex.cancel_auth(&state(&new)).await.unwrap_err(),
            Error::InvalidState
        );
        assert_eq!(f.count("yandex_oauth_states").await, 0);
    }

    #[tokio::test]
    async fn token_envelope_rejects_tampering_other_tenants_and_other_generations() {
        let f = Fixture::new().await;
        let a = f.yandex.seal(1, "gen", "tokens", b"secret").unwrap();
        let b = f.yandex.seal(1, "gen", "tokens", b"secret").unwrap();
        assert_ne!(a, b);
        assert!(!a.windows(6).any(|s| s == b"secret"));
        assert_eq!(f.yandex.open(1, "gen", "tokens", &a).unwrap(), b"secret");
        assert_eq!(
            f.yandex.open(2, "gen", "tokens", &a).unwrap_err(),
            Error::ReconnectRequired
        );
        assert_eq!(
            f.yandex.open(1, "other", "tokens", &a).unwrap_err(),
            Error::ReconnectRequired
        );
        assert_eq!(
            f.yandex.open(1, "gen", "pkce", &a).unwrap_err(),
            Error::ReconnectRequired
        );
        let mut changed = a;
        changed[15] ^= 1;
        assert_eq!(
            f.yandex.open(1, "gen", "tokens", &changed).unwrap_err(),
            Error::ReconnectRequired
        );
    }

    #[tokio::test]
    async fn oauth_defaults_disabled_and_selection_is_owned_active_scenario() {
        let f = Fixture::new().await;
        f.auth(1, "alice").await;
        f.auth(2, "bob").await;
        let status = f.yandex.status(1).await.unwrap();
        assert!(status.connected);
        assert!(!status.enabled);
        assert!(status.scenario_id.is_none());
        assert_eq!(
            f.yandex
                .configure(1, "scenario-bob", true)
                .await
                .unwrap_err(),
            Error::InvalidSelection
        );
        assert_eq!(
            f.yandex
                .configure(1, "inactive-alice", true)
                .await
                .unwrap_err(),
            Error::InvalidSelection
        );
        f.yandex.configure(1, "scenario-alice", true).await.unwrap();
        assert!(f.yandex.status(2).await.unwrap().scenario_id.is_none());
        let json = serde_json::to_string(&f.yandex.status(1).await.unwrap()).unwrap();
        assert!(
            !json.contains("access_token")
                && !json.contains("refresh_token")
                && !json.contains("user_id")
        );
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn grant_scopes_are_checked_when_present_and_omission_is_valid() {
        let f = Fixture::new().await;
        f.mock.reduced_scope.store(true, Ordering::SeqCst);
        let url = f.yandex.start_auth(1, "ru").await.unwrap();
        assert_eq!(
            f.yandex
                .prepare_auth(&state(&url), "alice")
                .await
                .unwrap_err(),
            Error::ReconnectRequired
        );
        assert!(!f.yandex.status(1).await.unwrap().connected);
        f.mock.omit_scope.store(true, Ordering::SeqCst);
        f.auth(1, "alice").await;
        assert!(f.yandex.status(1).await.unwrap().connected);
    }

    #[tokio::test]
    async fn near_expiry_token_refreshes_and_disconnect_removes_secrets_jobs_and_states() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        f.fire(1).await;
        f.yandex.start_auth(1, "ru").await.unwrap();
        f.db.call(|conn| {
            conn.execute(
                "UPDATE yandex_connections SET expires_at = unixepoch() + 30",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let results = tokio::join!(f.yandex.scenarios(1), f.yandex.scenarios(1));
        assert!(results.0.is_ok() && results.1.is_ok());
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
        f.yandex.disconnect(1).await.unwrap();
        assert_eq!(f.count("yandex_connections").await, 0);
        assert_eq!(f.count("yandex_jobs").await, 0);
        assert_eq!(f.count("yandex_oauth_states").await, 0);
        assert!(f.db.user(1).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn unregister_during_oauth_exchange_cannot_restore_integration() {
        let f = Fixture::new().await;
        let url = f.yandex.start_auth(99, "ru").await.unwrap();
        f.mock.block_token.store(true, Ordering::SeqCst);
        let yandex = f.yandex.clone();
        let handle = tokio::spawn(async move { yandex.prepare_auth(&state(&url), "alice").await });
        f.mock.token_entered.notified().await;
        assert!(!f.db.delete_user(99).await.unwrap());
        f.mock.token_release.notify_one();
        assert_eq!(handle.await.unwrap().unwrap_err(), Error::InvalidState);
        assert_eq!(f.count("yandex_connections").await, 0);
        assert_eq!(f.count("yandex_oauth_states").await, 0);
    }

    #[tokio::test]
    async fn unregister_during_refresh_cannot_restore_tokens_or_run_scenario() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        f.db.call(|conn| {
            conn.execute(
                "UPDATE yandex_connections SET expires_at = unixepoch() + 30",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        f.mock.block_token.store(true, Ordering::SeqCst);
        let yandex = f.yandex.clone();
        let handle = tokio::spawn(async move { yandex.test(1).await });
        f.mock.token_entered.notified().await;
        f.db.delete_user(1).await.unwrap();
        f.mock.token_release.notify_one();
        assert_eq!(handle.await.unwrap().unwrap_err(), Error::NotConnected);
        assert_eq!(f.count("yandex_connections").await, 0);
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn only_first_fire_enqueues_and_completion_cancels_delivery() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        let now = Utc::now();
        let reminder =
            f.db.add_reminder(1, "Daily".into(), now, Recurrence::Daily)
                .await
                .unwrap();
        let nag =
            f.db.fire_reminder(reminder.id, now, now)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(f.count("yandex_jobs").await, 1);
        assert!(
            f.db.fire_reminder(reminder.id, now, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            f.db.claim_nagging(&nag, now, now + chrono::Duration::minutes(1))
                .await
                .unwrap()
        );
        assert_eq!(f.count("yandex_jobs").await, 1);
        f.db.snooze_nagging(1, nag.id, now).await.unwrap();
        assert_eq!(f.count("yandex_jobs").await, 0);
        f.yandex.worker_tick().await.unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
        f.db.set_preferences(1, None, None, Some(20)).await.unwrap();
        f.db.complete_nagging(1, nag.id, now).await.unwrap();
        assert_eq!(f.count("yandex_jobs").await, 0);
        assert_eq!(
            f.db.due_confirmations(now + chrono::Duration::minutes(21))
                .await
                .unwrap()
                .len(),
            1
        );
        f.yandex.worker_tick().await.unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn normal_job_runs_once_and_test_shares_durable_cooldown() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        f.fire(1).await;
        f.yandex.worker_tick().await.unwrap();
        f.yandex.worker_tick().await.unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 1);
        assert_eq!(f.yandex.test(1).await.unwrap_err(), Error::RateLimited);
        assert!(f.yandex.status(1).await.unwrap().enabled);
        assert_eq!(f.db.due_naggings(Utc::now()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn ambiguous_post_failure_is_never_retried_and_does_not_remove_telegram_nag() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        f.fire(1).await;
        f.mock.post_status.store(503, Ordering::SeqCst);
        f.yandex.worker_tick().await.unwrap();
        f.db.call(|conn| {
            conn.execute("UPDATE yandex_connections SET last_action_at = 0", [])?;
            Ok(())
        })
        .await
        .unwrap();
        f.yandex.worker_tick().await.unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 1);
        let status = f.yandex.status(1).await.unwrap();
        assert!(!status.enabled && status.connected && !status.requires_reconnect);
        assert_eq!(
            status.last_error.as_deref(),
            Some("yandex_delivery_uncertain")
        );
        assert_eq!(f.db.due_naggings(Utc::now()).await.unwrap().len(), 1);
        assert!(f.db.user(1).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn unauthorized_provider_clears_tokens_and_requires_reconnect_only_for_that_user() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        f.auth(2, "bob").await;
        f.mock.info_status.store(401, Ordering::SeqCst);
        assert_eq!(
            f.yandex.scenarios(1).await.unwrap_err(),
            Error::ReconnectRequired
        );
        let status = f.yandex.status(1).await.unwrap();
        assert!(status.requires_reconnect && !status.connected && !status.enabled);
        assert!(f.yandex.status(2).await.unwrap().connected);
        assert!(f.db.user(1).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn expired_generation_changed_and_deleted_jobs_never_run() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        let nag = f.fire(1).await;
        f.db.call(|conn| {
            conn.execute("UPDATE yandex_jobs SET created_at = 0", [])?;
            Ok(())
        })
        .await
        .unwrap();
        f.yandex.worker_tick().await.unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
        f.db.complete_nagging(1, nag.id, Utc::now()).await.unwrap();
        f.fire(1).await;
        f.db.call(|conn| {
            conn.execute("UPDATE yandex_jobs SET generation = 'old-account'", [])?;
            Ok(())
        })
        .await
        .unwrap();
        f.yandex.worker_tick().await.unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
        f.db.delete_all_reminders(1).await.unwrap();
        assert_eq!(f.count("yandex_jobs").await, 0);
        f.fire(1).await;
        f.yandex
            .configure(1, "scenario-alice", false)
            .await
            .unwrap();
        assert_eq!(f.count("yandex_jobs").await, 0);
        f.yandex.worker_tick().await.unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn disabled_connection_does_not_enqueue_and_can_be_disabled_offline() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        f.mock.info_status.store(503, Ordering::SeqCst);
        f.yandex
            .configure(1, "scenario-alice", false)
            .await
            .unwrap();
        f.fire(1).await;
        assert_eq!(f.count("yandex_jobs").await, 0);
    }

    #[tokio::test]
    async fn snoozing_a_claimed_job_cancels_its_action_before_dispatch() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        let nagging = f.fire(1).await;
        let job = f.yandex.next_job().await.unwrap().unwrap();
        assert!(f.yandex.claim_job(job.clone()).await.unwrap());
        let connection = f.yandex.require_connection(1).await.unwrap();
        assert!(
            f.db.snooze_nagging(1, nagging.id, Utc::now() + chrono::Duration::hours(1))
                .await
                .unwrap()
        );
        f.yandex
            .execute(1, &connection, &job.scenario_id, Some(&job))
            .await
            .unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
        assert_eq!(f.count("yandex_jobs").await, 0);
        assert!(f.yandex.status(1).await.unwrap().enabled);
    }

    #[tokio::test]
    async fn claimed_job_after_restart_is_uncertain_and_is_never_replayed() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        f.fire(1).await;
        let job = f.yandex.next_job().await.unwrap().unwrap();
        assert!(f.yandex.claim_job(job).await.unwrap());
        f.yandex.recover_claims().await.unwrap();
        f.yandex.worker_tick().await.unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
        let status = f.yandex.status(1).await.unwrap();
        assert!(!status.enabled);
        assert_eq!(
            status.last_error.as_deref(),
            Some("yandex_delivery_uncertain")
        );
    }

    #[tokio::test]
    async fn refresh_rate_limit_preserves_credentials_and_reconnect_is_not_required() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        f.db.call(|conn| {
            conn.execute(
                "UPDATE yandex_connections SET expires_at = unixepoch() + 30",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        f.mock.token_status.store(429, Ordering::SeqCst);
        assert_eq!(f.yandex.scenarios(1).await.unwrap_err(), Error::RateLimited);
        let status = f.yandex.status(1).await.unwrap();
        assert!(status.connected && !status.requires_reconnect && !status.enabled);
        f.mock.token_status.store(0, Ordering::SeqCst);
        assert!(f.yandex.scenarios(1).await.is_ok());
    }

    #[tokio::test]
    async fn callback_receipt_requires_originating_telegram_user_and_is_one_use() {
        let f = Fixture::new().await;
        let url = f.yandex.start_auth(1, "en").await.unwrap();
        let pending = f.yandex.prepare_auth(&state(&url), "alice").await.unwrap();
        assert_eq!(pending.completion_token.len(), 48);
        assert!(
            pending
                .completion_token
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        );
        assert!(!f.yandex.status(1).await.unwrap().connected);
        assert_eq!(
            f.yandex.scenarios(1).await.unwrap_err(),
            Error::NotConnected
        );
        // Sharing the initial authorize URL cannot bind a victim's Yandex account to
        // another Telegram account: the callback-only receipt is unusable by the victim.
        assert_eq!(
            f.yandex
                .confirm_auth(2, &pending.completion_token)
                .await
                .unwrap_err(),
            Error::InvalidState
        );
        assert!(!f.yandex.status(1).await.unwrap().connected);
        assert!(
            f.yandex
                .confirm_auth(1, &pending.completion_token)
                .await
                .unwrap()
                .connected
        );
        assert_eq!(
            f.yandex
                .confirm_auth(1, &pending.completion_token)
                .await
                .unwrap_err(),
            Error::InvalidState
        );
        assert_eq!(f.count("yandex_pending_grants").await, 0);
    }

    #[tokio::test]
    async fn pending_grants_expire_and_disconnect_unregister_and_restart_invalidate_them() {
        let f = Fixture::new().await;
        let url = f.yandex.start_auth(1, "ru").await.unwrap();
        let first = f.yandex.prepare_auth(&state(&url), "alice").await.unwrap();
        f.db.call(|conn| {
            conn.execute("UPDATE yandex_pending_grants SET expires_at = 0", [])?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            f.yandex
                .confirm_auth(1, &first.completion_token)
                .await
                .unwrap_err(),
            Error::InvalidState
        );
        let url = f.yandex.start_auth(1, "ru").await.unwrap();
        let second = f.yandex.prepare_auth(&state(&url), "alice").await.unwrap();
        f.yandex.disconnect(1).await.unwrap();
        assert_eq!(f.count("yandex_pending_grants").await, 0);
        assert_eq!(
            f.yandex
                .confirm_auth(1, &second.completion_token)
                .await
                .unwrap_err(),
            Error::InvalidState
        );
        let url = f.yandex.start_auth(1, "ru").await.unwrap();
        let third = f.yandex.prepare_auth(&state(&url), "alice").await.unwrap();
        f.db.delete_user(1).await.unwrap();
        assert_eq!(f.count("yandex_pending_grants").await, 0);
        assert_eq!(
            f.yandex
                .confirm_auth(1, &third.completion_token)
                .await
                .unwrap_err(),
            Error::InvalidState
        );
        let url = f.yandex.start_auth(1, "ru").await.unwrap();
        let fourth = f.yandex.prepare_auth(&state(&url), "alice").await.unwrap();
        f.yandex.start_auth(1, "ru").await.unwrap();
        assert_eq!(
            f.yandex
                .confirm_auth(1, &fourth.completion_token)
                .await
                .unwrap_err(),
            Error::InvalidState
        );
    }

    #[tokio::test]
    async fn response_timeout_after_action_is_claimed_never_retries() {
        let mut f = Fixture::new().await;
        f.ready(1, "alice").await;
        f.fire(1).await;
        Arc::get_mut(&mut f.yandex.inner).unwrap().http = Client::builder()
            .timeout(Duration::from_millis(500))
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        f.mock.post_status.store(599, Ordering::SeqCst);
        f.yandex.worker_tick().await.unwrap();
        f.yandex.worker_tick().await.unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 1);
        assert_eq!(
            f.yandex.status(1).await.unwrap().last_error.as_deref(),
            Some("yandex_delivery_uncertain")
        );
    }

    #[tokio::test]
    async fn queued_job_and_its_completed_claim_survive_database_reopening() {
        let path = std::env::temp_dir().join(format!(
            "napominalochka-yandex-{}.db",
            random_string().unwrap()
        ));
        let mut f = Fixture::new().await;
        f.db = Db::open(&path).unwrap();
        f.yandex = Yandex::with_endpoints(
            f.yandex.inner.config.clone(),
            f.db.clone(),
            f.yandex.inner.oauth_base.clone(),
            f.yandex.inner.api_base.clone(),
        )
        .unwrap();
        f.ready(1, "alice").await;
        f.fire(1).await;
        f.db = Db::open(&path).unwrap();
        f.yandex = Yandex::with_endpoints(
            f.yandex.inner.config.clone(),
            f.db.clone(),
            f.yandex.inner.oauth_base.clone(),
            f.yandex.inner.api_base.clone(),
        )
        .unwrap();
        f.yandex.worker_tick().await.unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 1);
        f.db = Db::open(&path).unwrap();
        f.yandex = Yandex::with_endpoints(
            f.yandex.inner.config.clone(),
            f.db.clone(),
            f.yandex.inner.oauth_base.clone(),
            f.yandex.inner.api_base.clone(),
        )
        .unwrap();
        f.yandex.worker_tick().await.unwrap();
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 1);
        drop(f);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn idle_connection_refreshes_well_before_expiry_and_reschedules_from_granted_lifetime() {
        let f = Fixture::new().await;
        f.auth(1, "alice").await;
        let initial = f.yandex.connection(1).await.unwrap().unwrap();
        assert_eq!(
            initial.expires_at.unwrap() - initial.refresh_after.unwrap(),
            900
        );
        assert!(!initial.enabled);
        assert_eq!(f.count("reminders").await, 0);
        f.db.call(|conn| {
            conn.execute("UPDATE yandex_connections SET refresh_after = 0", [])?;
            Ok(())
        })
        .await
        .unwrap();
        assert!(initial.expires_at.unwrap() > Utc::now().timestamp() + 60);
        // The next grant may have a different (including much shorter) lifetime.
        f.mock.token_lifetime.store(120, Ordering::SeqCst);
        f.yandex.refresh_tick().await.unwrap();
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
        let refreshed = f.yandex.connection(1).await.unwrap().unwrap();
        assert_eq!(
            refreshed.expires_at.unwrap() - refreshed.refresh_after.unwrap(),
            30
        );
        assert!(refreshed.refresh_after.unwrap() > Utc::now().timestamp() + 60);
        f.yandex.refresh_tick().await.unwrap();
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn proactive_refresh_rate_limit_preserves_valid_grant_with_persisted_backoff() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        f.db.call(|conn| {
            conn.execute("UPDATE yandex_connections SET refresh_after = 0", [])?;
            Ok(())
        })
        .await
        .unwrap();
        let previous = f
            .yandex
            .connection(1)
            .await
            .unwrap()
            .unwrap()
            .encrypted_tokens;
        f.mock.token_status.store(429, Ordering::SeqCst);
        f.yandex.refresh_tick().await.unwrap();
        let delayed = f.yandex.connection(1).await.unwrap().unwrap();
        assert_eq!(delayed.encrypted_tokens, previous);
        assert!(delayed.enabled && !delayed.requires_reconnect);
        assert!(delayed.refresh_after.unwrap() >= Utc::now().timestamp() + 290);
        let requests = f.mock.token_requests.load(Ordering::SeqCst);
        let restarted = Yandex::with_endpoints(
            f.yandex.inner.config.clone(),
            f.db.clone(),
            f.yandex.inner.oauth_base.clone(),
            f.yandex.inner.api_base.clone(),
        )
        .unwrap();
        restarted.refresh_tick().await.unwrap();
        assert_eq!(f.mock.token_requests.load(Ordering::SeqCst), requests);
        f.mock.token_status.store(0, Ordering::SeqCst);
        f.db.call(|conn| {
            conn.execute("UPDATE yandex_connections SET refresh_after = 0", [])?;
            Ok(())
        })
        .await
        .unwrap();
        restarted.refresh_tick().await.unwrap();
        assert_eq!(f.mock.token_requests.load(Ordering::SeqCst), requests + 1);
        assert!(f.yandex.status(1).await.unwrap().enabled);
    }

    #[tokio::test]
    async fn expired_grant_requires_reconnect_without_attempting_expired_refresh() {
        let f = Fixture::new().await;
        f.ready(1, "alice").await;
        f.fire(1).await;
        f.db.call(|conn| {
            conn.execute("UPDATE yandex_connections SET expires_at = 0", [])?;
            Ok(())
        })
        .await
        .unwrap();
        let requests = f.mock.token_requests.load(Ordering::SeqCst);
        f.yandex.refresh_tick().await.unwrap();
        let status = f.yandex.status(1).await.unwrap();
        assert!(status.requires_reconnect && !status.connected && !status.enabled);
        assert_eq!(f.count("yandex_jobs").await, 0);
        assert_eq!(f.mock.token_requests.load(Ordering::SeqCst), requests);
        assert_eq!(f.db.due_naggings(Utc::now()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unregister_during_proactive_refresh_cannot_restore_credentials() {
        let f = Fixture::new().await;
        f.auth(1, "alice").await;
        f.db.call(|conn| {
            conn.execute("UPDATE yandex_connections SET refresh_after = 0", [])?;
            Ok(())
        })
        .await
        .unwrap();
        f.mock.block_token.store(true, Ordering::SeqCst);
        let yandex = f.yandex.clone();
        let refreshing = tokio::spawn(async move { yandex.refresh_tick().await });
        f.mock.token_entered.notified().await;
        f.db.delete_user(1).await.unwrap();
        f.mock.token_release.notify_one();
        refreshing.await.unwrap().unwrap();
        assert!(!f.yandex.status(1).await.unwrap().connected);
        assert_eq!(f.count("yandex_connections").await, 0);
    }
}
