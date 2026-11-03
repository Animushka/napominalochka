//! Yandex auth use cases.
use super::types::{AuthState, Error, NewAuth, PendingAuth, PendingGrant, Result, Status};
use super::{AUTH_LIFETIME, YandexService, random_string, state_digest, validate_tokens};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use sha2::{Digest, Sha256};

impl YandexService {
    pub(crate) async fn start_auth(&self, user_id: i64, language: &str) -> Result<String> {
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
        self.inner
            .repository
            .save_auth(NewAuth {
                user_id,
                generation,
                hash,
                encrypted_verifier,
                now,
                expires_at: now + AUTH_LIFETIME,
                language,
            })
            .await?;
        self.inner.provider.authorization_url(&state, &challenge)
    }

    pub(crate) async fn cancel_auth(&self, state: &str) -> Result<String> {
        let auth = self.consume_state(state).await?;
        let hash = auth.state_hash;
        self.inner
            .repository
            .cancel_auth(auth.user_id, hash)
            .await?;
        Ok(auth.language)
    }

    /// Exchanges a callback into a tentative grant. The callback receipt must be redeemed
    /// by the same authenticated Telegram user; state and PKCE alone do not prevent an
    /// attacker from sharing their own authorization URL with another Yandex account owner.
    pub(crate) async fn prepare_auth(&self, state: &str, code: &str) -> Result<PendingAuth> {
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
        let updated = self
            .inner
            .repository
            .save_grant(PendingGrant {
                completion_hash,
                generation,
                encrypted,
                pending_expires,
                expires_at,
                refresh_after,
                user_id: auth.user_id,
                state_hash: auth.state_hash,
            })
            .await?;
        if !updated {
            return Err(Error::InvalidState);
        }
        Ok(PendingAuth {
            language: auth.language,
            completion_token,
        })
    }

    pub(crate) async fn confirm_auth(
        &self,
        user_id: i64,
        completion_token: &str,
    ) -> Result<Status> {
        if completion_token.len() != 48 || !completion_token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::InvalidState);
        }
        let _guard = self.lock(user_id).await;
        let hash = state_digest(completion_token);
        let now = Utc::now().timestamp();
        let confirmed = self
            .inner
            .repository
            .confirm_auth(user_id, hash, now)
            .await?;
        if !confirmed {
            return Err(Error::InvalidState);
        }
        self.status(user_id).await
    }

    pub(super) async fn consume_state(&self, state: &str) -> Result<AuthState> {
        if state.len() != 43
            || !state
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(Error::InvalidState);
        }
        let hash = state_digest(state);
        let now = Utc::now().timestamp();
        let auth = self.inner.repository.consume_state(hash, now).await?;
        auth.ok_or(Error::InvalidState)
    }
}
