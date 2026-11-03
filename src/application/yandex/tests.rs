use super::*;
use crate::application::yandex::YandexService as Yandex;
use crate::bootstrap::config::YandexConfig;
use crate::bootstrap::services::yandex_with_endpoints;
use crate::domain::{Recurrence, User, UserTz, UtcOffset};
use crate::infrastructure::sqlite::Db;
use axum::{
    Form, Json, Router,
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use reqwest::{Client, StatusCode};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicUsize, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Default)]
struct Mock {
    actions: AtomicUsize,
    refreshes: AtomicUsize,
    token_requests: AtomicUsize,
    token_lifetime: AtomicI64,
    post_status: AtomicU16,
    token_status: AtomicU16,
    info_status: AtomicU16,
    info_requests: AtomicUsize,
    block_info: AtomicBool,
    info_entered: tokio::sync::Notify,
    info_release: tokio::sync::Notify,
    block_action: AtomicBool,
    action_entered: tokio::sync::Notify,
    action_release: tokio::sync::Notify,
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
    mock.info_requests.fetch_add(1, Ordering::SeqCst);
    if mock.block_info.load(Ordering::SeqCst) {
        mock.info_entered.notify_one();
        mock.info_release.notified().await;
    }
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
    if mock.block_action.load(Ordering::SeqCst) {
        mock.action_entered.notify_one();
        mock.action_release.notified().await;
    }
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
    config: YandexConfig,
    base: Url,
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
            callback_url: Url::parse("https://reminders.example.com/integrations/yandex/callback")
                .unwrap(),
        };
        let yandex =
            yandex_with_endpoints(config.clone(), db.clone(), base.clone(), base.clone()).unwrap();
        Self {
            yandex,
            config,
            base,
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

    async fn fire(&self, user_id: i64) -> crate::domain::Nagging {
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
    assert!(status.connected && !status.requires_reconnect && status.enabled);
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
    let mut provider = crate::adapters::yandex::provider::Provider::new(
        f.config.client_id.clone(),
        f.config.client_secret.clone(),
        f.config.callback_url.clone(),
        f.base.clone(),
        f.base.clone(),
    )
    .unwrap();
    provider.http = Client::builder()
        .timeout(Duration::from_millis(500))
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    f.yandex = Yandex::from_dependencies(
        f.db.clone(),
        provider,
        crate::adapters::yandex::crypto::TokenCipher::new(&f.config.token_key).unwrap(),
    );
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
    f.yandex = yandex_with_endpoints(
        f.config.clone(),
        f.db.clone(),
        f.base.clone(),
        f.base.clone(),
    )
    .unwrap();
    f.ready(1, "alice").await;
    f.fire(1).await;
    f.db = Db::open(&path).unwrap();
    f.yandex = yandex_with_endpoints(
        f.config.clone(),
        f.db.clone(),
        f.base.clone(),
        f.base.clone(),
    )
    .unwrap();
    f.yandex.worker_tick().await.unwrap();
    assert_eq!(f.mock.actions.load(Ordering::SeqCst), 1);
    f.db = Db::open(&path).unwrap();
    f.yandex = yandex_with_endpoints(
        f.config.clone(),
        f.db.clone(),
        f.base.clone(),
        f.base.clone(),
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
    let restarted = yandex_with_endpoints(
        f.config.clone(),
        f.db.clone(),
        f.base.clone(),
        f.base.clone(),
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
#[tokio::test]
async fn scenario_read_outage_preserves_enabled_jobs_and_recovers_without_reenable() {
    let f = Fixture::new().await;
    f.ready(1, "alice").await;
    f.fire(1).await;
    f.mock.info_status.store(503, Ordering::SeqCst);
    assert_eq!(f.yandex.scenarios(1).await.unwrap_err(), Error::Unavailable);
    let status = f.yandex.status(1).await.unwrap();
    assert!(status.connected && status.enabled && !status.requires_reconnect);
    assert_eq!(status.last_error.as_deref(), Some("yandex_unavailable"));
    assert_eq!(f.count("yandex_jobs").await, 1);
    let requests = f.mock.info_requests.load(Ordering::SeqCst);
    // The retry deadline is persisted, so a fresh integration also observes it.
    let restarted = yandex_with_endpoints(
        f.config.clone(),
        f.db.clone(),
        f.base.clone(),
        f.base.clone(),
    )
    .unwrap();
    restarted.worker_tick().await.unwrap();
    assert_eq!(f.mock.info_requests.load(Ordering::SeqCst), requests);
    assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
    f.mock.info_status.store(0, Ordering::SeqCst);
    assert!(restarted.scenarios(1).await.is_ok());
    assert!(restarted.status(1).await.unwrap().last_error.is_none());
    restarted.worker_tick().await.unwrap();
    assert_eq!(f.mock.actions.load(Ordering::SeqCst), 1);
    assert!(restarted.status(1).await.unwrap().enabled);
}

#[tokio::test]
async fn preparation_failure_stays_pending_and_restart_never_marks_it_uncertain() {
    let f = Fixture::new().await;
    f.ready(1, "alice").await;
    f.fire(1).await;
    f.mock.info_status.store(503, Ordering::SeqCst);
    f.yandex.worker_tick().await.unwrap();
    let state: String =
        f.db.call(|conn| conn.query_row("SELECT state FROM yandex_jobs", [], |row| row.get(0)))
            .await
            .unwrap();
    assert_eq!(state, "pending");
    f.yandex.recover_claims().await.unwrap();
    assert!(f.yandex.status(1).await.unwrap().enabled);
    let requests = f.mock.info_requests.load(Ordering::SeqCst);
    f.yandex.worker_tick().await.unwrap();
    assert_eq!(f.mock.info_requests.load(Ordering::SeqCst), requests);
    f.mock.info_status.store(0, Ordering::SeqCst);
    f.db.call(|conn| {
        conn.execute("UPDATE yandex_connections SET retry_after = 0", [])?;
        Ok(())
    })
    .await
    .unwrap();
    f.yandex.worker_tick().await.unwrap();
    f.yandex.worker_tick().await.unwrap();
    assert_eq!(f.mock.actions.load(Ordering::SeqCst), 1);
    assert!(f.yandex.status(1).await.unwrap().last_error.is_none());
}

#[tokio::test]
async fn shutdown_during_scenario_read_leaves_unclaimed_work_for_restart() {
    let f = Fixture::new().await;
    f.ready(1, "alice").await;
    f.fire(1).await;
    f.mock.block_info.store(true, Ordering::SeqCst);
    let yandex = f.yandex.clone();
    let shutdown = CancellationToken::new();
    let worker_shutdown = shutdown.clone();
    let task = tokio::spawn(async move { yandex.worker_tick_until(&worker_shutdown).await });
    tokio::time::timeout(Duration::from_secs(2), f.mock.info_entered.notified())
        .await
        .unwrap();
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    f.mock.info_release.notify_one();
    let state: String =
        f.db.call(|conn| conn.query_row("SELECT state FROM yandex_jobs", [], |row| row.get(0)))
            .await
            .unwrap();
    assert_eq!(state, "pending");
    assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
    f.yandex.recover_claims().await.unwrap();
    assert!(f.yandex.status(1).await.unwrap().enabled);
}

#[tokio::test]
async fn shutdown_finishes_token_refresh_without_starting_a_scenario_action() {
    let f = Fixture::new().await;
    f.ready(1, "alice").await;
    f.fire(1).await;
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
    let requests = f.mock.info_requests.load(Ordering::SeqCst);
    let yandex = f.yandex.clone();
    let shutdown = CancellationToken::new();
    let worker_shutdown = shutdown.clone();
    let task = tokio::spawn(async move { yandex.worker_tick_until(&worker_shutdown).await });
    tokio::time::timeout(Duration::from_secs(2), f.mock.token_entered.notified())
        .await
        .unwrap();
    shutdown.cancel();
    f.mock.token_release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(f.mock.actions.load(Ordering::SeqCst), 0);
    assert_eq!(f.mock.info_requests.load(Ordering::SeqCst), requests);
    assert!(
        f.yandex
            .connection(1)
            .await
            .unwrap()
            .unwrap()
            .expires_at
            .unwrap()
            > Utc::now().timestamp() + 60
    );
    f.yandex.recover_claims().await.unwrap();
    assert!(f.yandex.status(1).await.unwrap().enabled);
}

#[tokio::test]
async fn shutdown_allows_a_dispatched_action_to_finish_without_replay() {
    let f = Fixture::new().await;
    f.ready(1, "alice").await;
    f.fire(1).await;
    f.mock.block_action.store(true, Ordering::SeqCst);
    let yandex = f.yandex.clone();
    let shutdown = CancellationToken::new();
    let worker_shutdown = shutdown.clone();
    let mut task = tokio::spawn(async move { yandex.worker_tick_until(&worker_shutdown).await });
    tokio::time::timeout(Duration::from_secs(2), f.mock.action_entered.notified())
        .await
        .unwrap();
    shutdown.cancel();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut task)
            .await
            .is_err()
    );
    f.mock.action_release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    f.yandex.recover_claims().await.unwrap();
    f.yandex.worker_tick().await.unwrap();
    assert_eq!(f.mock.actions.load(Ordering::SeqCst), 1);
    assert!(f.yandex.status(1).await.unwrap().enabled);
}
