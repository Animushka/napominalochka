//! Authenticated Telegram initData verification.
use super::{ApiError, WebState};
use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
use chrono::{DateTime, Utc};
use hmac::{KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AuthUser {
    pub(crate) id: crate::domain::UserId,
    pub(crate) first_name: String,
    pub(crate) last_name: String,
    pub(crate) language_code: Option<String>,
}

impl FromRequestParts<Arc<WebState>> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<WebState>,
    ) -> Result<Self, Self::Rejection> {
        let init_data = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("tma "))
            .ok_or(ApiError::Unauthorized)?;
        let user = verify_init_data(init_data, &state.bot_token, Utc::now())
            .ok_or(ApiError::Unauthorized)?;
        state
            .service
            .initialize_language(user.id, user.language_code.as_deref())
            .await?;
        Ok(user)
    }
}

/// Validates Mini App `initData` as described in
/// <https://core.telegram.org/bots/webapps#validating-data-received-via-the-mini-app>.
pub(crate) fn verify_init_data(
    init_data: &str,
    bot_token: &str,
    now: DateTime<Utc>,
) -> Option<AuthUser> {
    let mut hash = None;
    let mut fields: Vec<(String, String)> = Vec::new();
    for (key, value) in form_urlencoded::parse(init_data.as_bytes()) {
        if key == "hash" {
            hash = Some(value.into_owned());
        } else {
            fields.push((key.into_owned(), value.into_owned()));
        }
    }
    let hash = hex::decode(hash?).ok()?;
    fields.sort();
    let data_check_string = fields
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut secret = HmacSha256::new_from_slice(b"WebAppData").ok()?;
    secret.update(bot_token.as_bytes());
    let secret_key = secret.finalize().into_bytes();
    let mut mac = HmacSha256::new_from_slice(&secret_key).ok()?;
    mac.update(data_check_string.as_bytes());
    mac.verify_slice(&hash).ok()?;

    let field = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let auth_date: i64 = field("auth_date")?.parse().ok()?;
    if now.timestamp() - auth_date > MAX_INIT_DATA_AGE_SECS || auth_date - now.timestamp() > 30 {
        return None;
    }

    #[derive(Deserialize)]
    struct TelegramUser {
        id: crate::domain::UserId,
        first_name: String,
        #[serde(default)]
        last_name: Option<String>,
        #[serde(default)]
        language_code: Option<String>,
    }
    let user: TelegramUser = serde_json::from_str(field("user")?).ok()?;
    Some(AuthUser {
        id: user.id,
        first_name: user.first_name,
        last_name: user.last_name.unwrap_or_default(),
        language_code: user.language_code,
    })
}

/// `initData` older than this is rejected, limiting how long a leaked copy is useful.
const MAX_INIT_DATA_AGE_SECS: i64 = 24 * 60 * 60;

type HmacSha256 = hmac::Hmac<Sha256>;
