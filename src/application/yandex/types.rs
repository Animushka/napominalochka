//! Yandex use-case contracts; no persistence or HTTP client types.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
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
    pub(crate) fn code(self) -> &'static str {
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
}

// Deliberately discard SQL/HTTP/body errors at this boundary. Only stable codes leave it.
impl From<anyhow::Error> for Error {
    fn from(_: anyhow::Error) -> Self {
        Self::Internal
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Serialize)]
pub(crate) struct Status {
    pub(crate) available: bool,
    pub(crate) connected: bool,
    pub(crate) enabled: bool,
    pub(crate) requires_reconnect: bool,
    pub(crate) scenario_id: Option<String>,
    pub(crate) scenario_name: Option<String>,
    pub(crate) last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Scenario {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) is_active: bool,
}

pub(crate) struct PendingAuth {
    pub(crate) language: String,
    pub(crate) completion_token: String,
}

impl std::fmt::Debug for PendingAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingAuth")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

pub(crate) struct Connection {
    pub(crate) generation: String,
    pub(crate) encrypted_tokens: Option<Vec<u8>>,
    pub(crate) expires_at: Option<i64>,
    pub(crate) refresh_after: Option<i64>,
    pub(crate) enabled: bool,
    pub(crate) requires_reconnect: bool,
    pub(crate) scenario_id: Option<String>,
    pub(crate) scenario_name: Option<String>,
    pub(crate) last_error: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Tokens {
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
}

pub(crate) struct AuthState {
    pub(crate) user_id: i64,
    pub(crate) state_hash: String,
    pub(crate) encrypted_verifier: Vec<u8>,
    pub(crate) language: String,
}

pub(crate) enum ActionResult {
    Delivered,
    Skipped,
    Cancelled,
}

#[derive(Clone)]
pub(crate) struct Job {
    pub(crate) id: i64,
    pub(crate) user_id: i64,
    pub(crate) generation: String,
    pub(crate) scenario_id: String,
}

pub(crate) struct NewAuth {
    pub(crate) user_id: i64,
    pub(crate) generation: String,
    pub(crate) hash: String,
    pub(crate) encrypted_verifier: Vec<u8>,
    pub(crate) now: i64,
    pub(crate) expires_at: i64,
    pub(crate) language: String,
}

pub(crate) struct PendingGrant {
    pub(crate) completion_hash: String,
    pub(crate) generation: String,
    pub(crate) encrypted: Vec<u8>,
    pub(crate) pending_expires: i64,
    pub(crate) expires_at: i64,
    pub(crate) refresh_after: i64,
    pub(crate) user_id: i64,
    pub(crate) state_hash: String,
}

/// A failed action either preserves the connection for a retry or disables it.
pub(crate) enum FailureDisposition {
    RetryAt(i64),
    Disable,
    Reconnect,
}

pub(crate) struct ConnectionFailure {
    pub(crate) user_id: i64,
    pub(crate) generation: String,
    pub(crate) error: Error,
    pub(crate) disposition: FailureDisposition,
}

#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    pub(crate) token_type: String,
    pub(crate) expires_in: i64,
    pub(crate) scope: Option<String>,
}

/// Policy deadlines supplied to atomic queue operations; SQLite chooses no intervals.
pub(crate) struct DispatchWindow {
    pub(crate) now: i64,
    pub(crate) oldest_created_at: i64,
    pub(crate) last_action_before: i64,
    pub(crate) terminal_before: i64,
}
impl DispatchWindow {
    pub(crate) fn at(now: i64) -> Self {
        Self {
            now,
            oldest_created_at: now - 300,
            last_action_before: now - 60,
            terminal_before: now - 86400,
        }
    }
}
