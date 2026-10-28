//! HTTP server for the Telegram Mini App: the page itself and a small JSON API.
//!
//! Every API request carries the Mini App's `initData` (in `Authorization: tma <initData>`),
//! which Telegram signs with the bot token. The signature identifies the user, so there are
//! no separate accounts or sessions.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDateTime, Utc};
use hmac::{KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::calendar;
use crate::i18n::Lang;
use crate::model::{InputError, Nagging, Recurrence, Reminder, Snooze, UserTz};
use crate::panels;
use crate::service::{self, Profile, ReminderInput, Service};

/// `initData` older than this is rejected, limiting how long a leaked copy is useful.
const MAX_INIT_DATA_AGE_SECS: i64 = 24 * 60 * 60;
const LOCAL_FORMAT: &str = "%Y-%m-%dT%H:%M";

type HmacSha256 = hmac::Hmac<Sha256>;

pub struct WebState {
    pub service: Service,
    pub bot_token: String,
    /// Public address; calendar feed links are built from it.
    pub web_app_url: Option<Url>,
    pub yandex: Option<crate::yandex::Yandex>,
}

pub async fn serve(
    addr: SocketAddr,
    state: Arc<WebState>,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding web server to {addr}"))?;
    tracing::info!(%addr, "web server listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await
        .context("web server failed")
}

pub fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/", get(crate::frontend::index))
        .route("/assets/{*path}", get(crate::frontend::asset))
        .route("/avatar.jpg", get(crate::frontend::avatar))
        .route("/avatar.png", get(crate::frontend::avatar))
        .route("/appearance.js", get(crate::frontend::appearance))
        .fallback(crate::frontend::not_found)
        .route("/health", get(|| async { "ok" }))
        .route("/api/state", get(get_state))
        .route("/api/preferences", patch(set_preferences))
        .route("/api/timezone", put(set_timezone))
        .route("/api/reminders", post(create_reminder))
        .route("/api/pending/{id}/done", post(complete_pending))
        .route("/api/pending/{id}/snooze", post(snooze_pending))
        .route(
            "/api/reminders/{id}",
            put(update_reminder).delete(delete_reminder),
        )
        .route("/api/calendar", get(get_calendar_link))
        .route("/api/calendar/reset", post(reset_calendar_link))
        .route("/calendar/{file}", get(calendar_feed))
        .merge(crate::yandex_web::routes())
        .layer(middleware::from_fn(protect_api_cache))
        .with_state(state)
}

async fn protect_api_cache(request: Request, next: Next) -> Response {
    let sensitive = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    if sensitive {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

// ---- API types ------------------------------------------------------------------------

#[derive(Serialize)]
struct StateView {
    language: String,
    nag_interval_minutes: i64,
    confirmation_check_minutes: i64,
    /// Display name, e.g. "Asia/Singapore (UTC+08:00)"; `None` until a timezone is chosen.
    timezone: Option<String>,
    /// IANA name or offset, comparable with the device's timezone.
    timezone_id: Option<String>,
    /// Current local time in the user's timezone, `YYYY-MM-DDTHH:MM`.
    now: Option<String>,
    reminders: Vec<ReminderView>,
    pending: Vec<PendingView>,
}

#[derive(Serialize)]
struct PendingView {
    id: i64,
    reminder_id: Option<i64>,
    description: String,
    next_at: String,
    next_when: String,
    relative: String,
}
impl PendingView {
    fn new(pending: Nagging, tz: UserTz, now: DateTime<Utc>, lang: Lang) -> Self {
        let local = tz.local(pending.next_at);
        Self {
            id: pending.id,
            reminder_id: pending.reminder_id,
            description: pending.description,
            next_at: local.format(LOCAL_FORMAT).to_string(),
            next_when: lang.date(local),
            relative: panels::relative_lang(now, pending.next_at, lang),
        }
    }
}

#[derive(Serialize)]
struct ReminderView {
    id: i64,
    description: String,
    /// Local time in the user's timezone, `YYYY-MM-DDTHH:MM` (for `<input type=date/time>`).
    at: String,
    recurrence: &'static str,
    when: String,
    relative: String,
    repeat: String,
}

impl ReminderView {
    fn new(reminder: &Reminder, tz: UserTz, now: DateTime<Utc>, lang: Lang) -> Self {
        let local = tz.local(reminder.due_at);
        Self {
            id: reminder.id,
            description: reminder.description.clone(),
            at: local.format(LOCAL_FORMAT).to_string(),
            recurrence: reminder.recurrence.as_str(),
            when: lang.date(local),
            relative: panels::relative_lang(now, reminder.due_at, lang),
            repeat: lang.recurrence(reminder.recurrence, reminder.anchor_local.unwrap_or(local)),
        }
    }
}

#[derive(Deserialize)]
struct TimezoneInput {
    timezone: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferencesInput {
    language: Option<String>,
    nag_interval_minutes: Option<i64>,
    confirmation_check_minutes: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReminderBody {
    description: String,
    at: String,
    recurrence: String,
    #[serde(default)]
    preserve_schedule: bool,
}

// ---- handlers -------------------------------------------------------------------------

async fn get_state(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<StateView>, ApiError> {
    Ok(Json(load_state(&state.service, auth.id, Utc::now()).await?))
}

async fn complete_pending(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Result<Json<StateView>, ApiError> {
    state.service.done_at(auth.id, id, Utc::now()).await?;
    Ok(Json(load_state(&state.service, auth.id, Utc::now()).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnoozeInput {
    duration: String,
}
async fn snooze_pending(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(input): Json<SnoozeInput>,
) -> Result<Json<StateView>, ApiError> {
    let duration = match input.duration.as_str() {
        "10m" => Snooze::TenMinutes,
        "1h" => Snooze::OneHour,
        "tomorrow" => Snooze::TomorrowMorning,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_request",
                "Choose a snooze duration.",
            ));
        }
    };
    let user = state.service.registered(auth.id).await?;
    if state
        .service
        .snooze(&user, id, duration, Utc::now())
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound);
    }
    Ok(Json(load_state(&state.service, auth.id, Utc::now()).await?))
}

async fn set_preferences(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Json(input): Json<PreferencesInput>,
) -> Result<Json<StateView>, ApiError> {
    state
        .service
        .set_preferences(
            auth.id,
            input.language.as_deref(),
            input.nag_interval_minutes,
            input.confirmation_check_minutes,
        )
        .await?;
    Ok(Json(load_state(&state.service, auth.id, Utc::now()).await?))
}

async fn set_timezone(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Json(input): Json<TimezoneInput>,
) -> Result<Json<StateView>, ApiError> {
    let tz = UserTz::parse(&input.timezone).map_err(service::Error::from)?;
    let profile = Profile {
        id: auth.id,
        first_name: auth.first_name,
        last_name: auth.last_name,
    };
    state.service.set_timezone(profile, tz).await?;
    Ok(Json(load_state(&state.service, auth.id, Utc::now()).await?))
}

async fn create_reminder(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Json(input): Json<ReminderBody>,
) -> Result<(StatusCode, Json<ReminderView>), ApiError> {
    let now = Utc::now();
    let user = state.service.registered(auth.id).await?;
    let (at, recurrence) = input.schedule()?;
    let input = ReminderInput {
        description: &input.description,
        at,
        recurrence,
    };
    let reminder = state.service.create(&user, input, now).await?;
    let lang = Lang::parse(&state.service.preferences(auth.id).await?.language);
    Ok((
        StatusCode::CREATED,
        Json(ReminderView::new(&reminder, user.tz, now, lang)),
    ))
}

async fn update_reminder(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(input): Json<ReminderBody>,
) -> Result<Json<ReminderView>, ApiError> {
    let now = Utc::now();
    let user = state.service.registered(auth.id).await?;
    let reminder = if input.preserve_schedule {
        state
            .service
            .rename(auth.id, id, &input.description)
            .await?
    } else {
        let (at, recurrence) = input.schedule()?;
        state
            .service
            .update(&user, id, Some(&input.description), at, recurrence, now)
            .await?
    };
    let lang = Lang::parse(&state.service.preferences(auth.id).await?.language);
    Ok(Json(ReminderView::new(&reminder, user.tz, now, lang)))
}

async fn delete_reminder(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    state.service.delete(auth.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct CalendarLink {
    url: String,
    webcal: String,
}

async fn get_calendar_link(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<CalendarLink>, ApiError> {
    calendar_link(&state, auth.id, false).await.map(Json)
}

async fn reset_calendar_link(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<CalendarLink>, ApiError> {
    calendar_link(&state, auth.id, true).await.map(Json)
}

async fn calendar_link(
    state: &WebState,
    user_id: i64,
    reset: bool,
) -> Result<CalendarLink, ApiError> {
    let base = state.web_app_url.as_ref().ok_or_else(|| {
        ApiError::bad_request("calendar_disabled", "The calendar feed is not enabled.")
    })?;
    let token = state.service.calendar_token(user_id, reset).await?;
    let url =
        calendar::feed_url(base, &token).ok_or_else(|| anyhow::anyhow!("invalid WEB_APP_URL"))?;
    Ok(CalendarLink {
        webcal: calendar::webcal(&url),
        url: url.into(),
    })
}

/// `GET /calendar/<token>.ics`: the feed itself. The unguessable token is the credential,
/// as calendar apps cannot send Telegram's signed data.
async fn calendar_feed(
    State(state): State<Arc<WebState>>,
    Path(file): Path<String>,
) -> Result<Response, ApiError> {
    let token = file.strip_suffix(".ics").unwrap_or(&file);
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::NotFound);
    }
    let (user, reminders) = state
        .service
        .calendar_feed(token)
        .await?
        .ok_or(ApiError::NotFound)?;
    let lang = Lang::parse(&state.service.preferences(user.id).await?.language);
    let body = calendar::feed_lang(&reminders, user.tz, Utc::now(), lang);
    Ok((
        [
            (header::CONTENT_TYPE, "text/calendar; charset=utf-8"),
            (header::CACHE_CONTROL, "private, max-age=300"),
            (
                header::CONTENT_DISPOSITION,
                "inline; filename=\"reminders.ics\"",
            ),
        ],
        body,
    )
        .into_response())
}

async fn load_state(
    service: &Service,
    user_id: i64,
    now: DateTime<Utc>,
) -> service::Result<StateView> {
    let preferences = service.preferences(user_id).await?;
    let lang = Lang::parse(&preferences.language);
    let Some(user) = service.user(user_id).await? else {
        return Ok(StateView {
            language: preferences.language,
            nag_interval_minutes: preferences.nag_interval_minutes,
            confirmation_check_minutes: preferences.confirmation_check_minutes,
            timezone: None,
            timezone_id: None,
            now: None,
            reminders: Vec::new(),
            pending: Vec::new(),
        });
    };
    let reminders = service.list(user_id).await?;
    let pending = service.pending(user_id).await?;
    let timezone_id = match user.tz {
        UserTz::Named(tz) => tz.name().to_owned(),
        UserTz::Fixed(offset) => offset.to_string(),
    };
    Ok(StateView {
        language: preferences.language,
        nag_interval_minutes: preferences.nag_interval_minutes,
        confirmation_check_minutes: preferences.confirmation_check_minutes,
        timezone: Some(user.tz.to_string()),
        timezone_id: Some(timezone_id),
        now: Some(user.tz.local(now).format(LOCAL_FORMAT).to_string()),
        reminders: reminders
            .iter()
            .map(|r| ReminderView::new(r, user.tz, now, lang))
            .collect(),
        pending: pending
            .into_iter()
            .map(|item| PendingView::new(item, user.tz, now, lang))
            .collect(),
    })
}

impl ReminderBody {
    /// Parses the wire format; everything else is validated by the service.
    fn schedule(&self) -> Result<(NaiveDateTime, Recurrence), ApiError> {
        let at = NaiveDateTime::parse_from_str(&self.at, LOCAL_FORMAT).map_err(|_| {
            ApiError::bad_request("invalid_datetime", "Please pick a date and time.")
        })?;
        let recurrence = self.recurrence.parse().map_err(service::Error::from)?;
        Ok((at, recurrence))
    }
}

// ---- errors ---------------------------------------------------------------------------

pub enum ApiError {
    Unauthorized,
    NotFound,
    BadRequest { code: &'static str, message: String },
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        ApiError::Internal(error)
    }
}

impl ApiError {
    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::BadRequest {
            code,
            message: message.into(),
        }
    }
}
impl From<service::Error> for ApiError {
    fn from(error: service::Error) -> Self {
        match error {
            service::Error::NotFound => ApiError::NotFound,
            service::Error::NotRegistered => {
                ApiError::bad_request("invalid_timezone", "Please set your timezone first.")
            }
            service::Error::InvalidPreferences(_) => {
                ApiError::bad_request("invalid_preferences", "Check your reminder preferences.")
            }
            service::Error::Input(error) => {
                let code = match error {
                    InputError::InvalidTimezone => "invalid_timezone",
                    InputError::InvalidRecurrence => "invalid_recurrence",
                    InputError::EmptyDescription => "empty_description",
                    InputError::DescriptionTooLong => "description_too_long",
                    InputError::InPast => "past_time",
                };
                ApiError::bad_request(code, error.to_string())
            }
            service::Error::Internal(error) => ApiError::Internal(error),
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Please open this page from Telegram.".to_owned(),
            ),
            ApiError::NotFound => (
                StatusCode::NOT_FOUND,
                "reminder_not_found",
                "That reminder no longer exists.".to_owned(),
            ),
            ApiError::BadRequest { code, message } => (StatusCode::BAD_REQUEST, code, message),
            ApiError::Internal(error) => {
                tracing::error!(?error, "web request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "Something went wrong. Please try again.".to_owned(),
                )
            }
        };
        (
            status,
            Json(serde_json::json!({ "error": message, "code": code })),
        )
            .into_response()
    }
}

// ---- authentication -------------------------------------------------------------------

/// The Telegram user who opened the Mini App, taken from verified `initData`.
#[derive(Debug, PartialEq, Eq)]
pub struct AuthUser {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub language_code: Option<String>,
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
pub fn verify_init_data(init_data: &str, bot_token: &str, now: DateTime<Utc>) -> Option<AuthUser> {
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
        id: i64,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const TOKEN: &str = "123456:test-token";

    /// Builds `initData` signed the way Telegram does.
    fn sign(fields: &[(&str, &str)], token: &str) -> String {
        let mut sorted: Vec<_> = fields.to_vec();
        sorted.sort();
        let check = sorted
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut secret = HmacSha256::new_from_slice(b"WebAppData").unwrap();
        secret.update(token.as_bytes());
        let mut mac = HmacSha256::new_from_slice(&secret.finalize().into_bytes()).unwrap();
        mac.update(check.as_bytes());
        let hash = hex::encode(mac.finalize().into_bytes());

        let mut serializer = form_urlencoded::Serializer::new(String::new());
        for (k, v) in fields {
            serializer.append_pair(k, v);
        }
        serializer.append_pair("hash", &hash);
        serializer.finish()
    }

    fn init_data(user_id: i64, now: DateTime<Utc>) -> String {
        let user = format!(r#"{{"id":{user_id},"first_name":"Ada","last_name":"Lovelace"}}"#);
        sign(
            &[
                ("auth_date", &now.timestamp().to_string()),
                ("query_id", "AAH"),
                ("user", &user),
            ],
            TOKEN,
        )
    }

    fn init_data_with_language(user_id: i64, language: &str, now: DateTime<Utc>) -> String {
        let user =
            serde_json::json!({ "id": user_id, "first_name": "Ada", "language_code": language })
                .to_string();
        sign(
            &[
                ("auth_date", &now.timestamp().to_string()),
                ("query_id", "AAH"),
                ("user", &user),
            ],
            TOKEN,
        )
    }

    #[test]
    fn accepts_valid_init_data() {
        let now = Utc::now();
        assert_eq!(
            verify_init_data(&init_data(42, now), TOKEN, now),
            Some(AuthUser {
                id: 42,
                first_name: "Ada".into(),
                last_name: "Lovelace".into(),
                language_code: None,
            })
        );
    }

    #[test]
    fn rejects_forged_or_stale_init_data() {
        let now = Utc::now();
        let data = init_data(42, now);
        assert_eq!(verify_init_data(&data, "999:other-token", now), None);
        assert_eq!(
            verify_init_data(&data.replace("42", "43"), TOKEN, now),
            None
        );
        assert_eq!(
            verify_init_data(&data, TOKEN, now + chrono::Duration::days(2)),
            None
        );
        assert_eq!(verify_init_data("user=%7B%7D", TOKEN, now), None);
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        auth: Option<&str>,
        body: Option<&str>,
    ) -> (StatusCode, String) {
        let mut request = Request::builder().method(method).uri(uri);
        if let Some(auth) = auth {
            request = request.header(header::AUTHORIZATION, format!("tma {auth}"));
        }
        if body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        let response = app
            .clone()
            .oneshot(
                request
                    .body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn api_flow() {
        let app = router(Arc::new(WebState {
            service: Service::new(Db::open_in_memory().unwrap()),
            bot_token: TOKEN.into(),
            web_app_url: Some(Url::parse("https://reminders.example.com").unwrap()),
            yandex: None,
        }));
        let ada = init_data_with_language(42, "en", Utc::now());
        let eve = init_data(66, Utc::now());

        assert_eq!(
            call(&app, "GET", "/api/state", None, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, body) = call(&app, "GET", "/api/state", Some(&ada), None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(r#""timezone":null"#), "{body}");

        let reminder =
            r#"{"description":"call mum","at":"2099-01-02T09:30","recurrence":"weekly"}"#;
        assert_eq!(
            call(&app, "POST", "/api/reminders", Some(&ada), Some(reminder))
                .await
                .0,
            StatusCode::BAD_REQUEST,
            "timezone is required first"
        );

        let (status, body) = call(
            &app,
            "PUT",
            "/api/timezone",
            Some(&ada),
            Some(r#"{"timezone":"Asia/Singapore"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Asia/Singapore"), "{body}");

        let (status, body) = call(&app, "POST", "/api/reminders", Some(&ada), Some(reminder)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body.contains(r#""at":"2099-01-02T09:30""#), "{body}");
        assert!(body.contains(r#""repeat":"every Fri""#), "{body}");
        let id: i64 = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
            .as_i64()
            .unwrap();

        let past = r#"{"description":"x","at":"2000-01-01T09:00","recurrence":"once"}"#;
        assert_eq!(
            call(&app, "POST", "/api/reminders", Some(&ada), Some(past))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );

        let edited = r#"{"description":"call dad","at":"2099-01-03T10:00","recurrence":"once"}"#;
        let uri = format!("/api/reminders/{id}");
        assert_eq!(
            call(&app, "PUT", &uri, Some(&eve), Some(edited)).await.0,
            StatusCode::BAD_REQUEST
        );
        call(
            &app,
            "PUT",
            "/api/timezone",
            Some(&eve),
            Some(r#"{"timezone":"+0"}"#),
        )
        .await;
        assert_eq!(
            call(&app, "PUT", &uri, Some(&eve), Some(edited)).await.0,
            StatusCode::NOT_FOUND,
            "not eve's"
        );
        assert_eq!(
            call(&app, "DELETE", &uri, Some(&eve), None).await.0,
            StatusCode::NOT_FOUND,
            "not eve's"
        );

        let (status, body) = call(&app, "PUT", &uri, Some(&ada), Some(edited)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("call dad"), "{body}");

        assert_eq!(
            call(&app, "DELETE", &uri, Some(&ada), None).await.0,
            StatusCode::NO_CONTENT
        );
        let (_, body) = call(&app, "GET", "/api/state", Some(&ada), None).await;
        assert!(body.contains(r#""reminders":[]"#), "{body}");
    }

    #[tokio::test]
    async fn preferences_persist_before_registration_and_remain_isolated() {
        let service = Service::new(Db::open_in_memory().unwrap());
        let app = router(Arc::new(WebState {
            service: service.clone(),
            bot_token: TOKEN.into(),
            web_app_url: None,
            yandex: None,
        }));
        let ada = init_data_with_language(42, "en", Utc::now());
        let eve = init_data_with_language(66, "ru", Utc::now());
        assert_eq!(
            call(
                &app,
                "PATCH",
                "/api/preferences",
                None,
                Some(r#"{"language":"ru"}"#)
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        let (status, body) = call(&app, "GET", "/api/state", Some(&ada), None).await;
        assert_eq!(status, StatusCode::OK);
        let state: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(state["language"], "en");
        assert_eq!(state["nag_interval_minutes"], 10);
        assert_eq!(state["confirmation_check_minutes"], 0);
        assert!(state["timezone"].is_null());
        let patch =
            r#"{"language":"ru","nag_interval_minutes":30,"confirmation_check_minutes":20}"#;
        let (status, body) = call(&app, "PATCH", "/api/preferences", Some(&ada), Some(patch)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let state: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(state["language"], "ru");
        assert_eq!(state["nag_interval_minutes"], 30);
        assert_eq!(state["confirmation_check_minutes"], 20);
        assert!(
            service.user(42).await.unwrap().is_none(),
            "preferences do not register a timezone"
        );
        let (_, body) = call(&app, "GET", "/api/state", Some(&ada), None).await;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["language"],
            "ru",
            "manual selection wins over Telegram language"
        );
        let (_, body) = call(&app, "GET", "/api/state", Some(&eve), None).await;
        let other: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(other["language"], "ru");
        assert_eq!(other["nag_interval_minutes"], 10);
        assert_eq!(other["confirmation_check_minutes"], 0);
        call(
            &app,
            "PUT",
            "/api/timezone",
            Some(&ada),
            Some(r#"{"timezone":"Europe/Moscow"}"#),
        )
        .await;
        let (_, body) = call(&app, "GET", "/api/state", Some(&ada), None).await;
        let registered: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            registered["nag_interval_minutes"], 30,
            "timezone registration preserves preferences"
        );
    }

    #[tokio::test]
    async fn preferences_validate_whole_patch_before_mutation() {
        let service = Service::new(Db::open_in_memory().unwrap());
        let app = router(Arc::new(WebState {
            service: service.clone(),
            bot_token: TOKEN.into(),
            web_app_url: None,
            yandex: None,
        }));
        let auth = init_data(42, Utc::now());
        for invalid in [
            r#"{"language":"fr"}"#,
            r#"{"language":"en","nag_interval_minutes":2}"#,
            r#"{"confirmation_check_minutes":10}"#,
        ] {
            let (status, body) = call(
                &app,
                "PATCH",
                "/api/preferences",
                Some(&auth),
                Some(invalid),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}: {body}");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"],
                "invalid_preferences"
            );
            assert_eq!(
                service.preferences(42).await.unwrap(),
                crate::model::Preferences::default()
            );
        }
        assert!(
            !call(
                &app,
                "PATCH",
                "/api/preferences",
                Some(&auth),
                Some(r#"{"user_id":66,"language":"en"}"#)
            )
            .await
            .0
            .is_success(),
            "unknown fields cannot redirect a patch"
        );
        assert_eq!(
            service.preferences(66).await.unwrap(),
            crate::model::Preferences::default()
        );
    }

    #[tokio::test]
    async fn serves_page_and_health() {
        let app = router(Arc::new(WebState {
            service: Service::new(Db::open_in_memory().unwrap()),
            bot_token: TOKEN.into(),
            web_app_url: Some(Url::parse("https://reminders.example.com").unwrap()),
            yandex: None,
        }));
        let (status, body) = call(&app, "GET", "/", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("telegram-web-app.js"));
        assert_eq!(
            call(&app, "GET", "/health", None, None).await,
            (StatusCode::OK, "ok".into())
        );
    }

    #[tokio::test]
    async fn calendar_feed_flow() {
        let app = router(Arc::new(WebState {
            service: Service::new(Db::open_in_memory().unwrap()),
            bot_token: TOKEN.into(),
            web_app_url: Some(Url::parse("https://reminders.example.com").unwrap()),
            yandex: None,
        }));
        let ada = init_data(42, Utc::now());

        assert_eq!(
            call(&app, "GET", "/api/calendar", Some(&ada), None).await.0,
            StatusCode::BAD_REQUEST
        );
        call(
            &app,
            "PUT",
            "/api/timezone",
            Some(&ada),
            Some(r#"{"timezone":"Europe/London"}"#),
        )
        .await;
        let reminder = r#"{"description":"stretch","at":"2099-01-02T09:30","recurrence":"daily"}"#;
        call(&app, "POST", "/api/reminders", Some(&ada), Some(reminder)).await;

        let (status, body) = call(&app, "GET", "/api/calendar", Some(&ada), None).await;
        assert_eq!(status, StatusCode::OK);
        let link: serde_json::Value = serde_json::from_str(&body).unwrap();
        let url = link["url"].as_str().unwrap();
        assert!(
            url.starts_with("https://reminders.example.com/calendar/"),
            "{url}"
        );
        assert!(
            link["webcal"]
                .as_str()
                .unwrap()
                .starts_with("webcal://reminders.example.com/")
        );
        let path = url.trim_start_matches("https://reminders.example.com");

        let (status, ics) = call(&app, "GET", path, None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(ics.contains("SUMMARY:stretch"), "{ics}");
        assert!(
            ics.contains("DTSTART;TZID=Europe/London:20990102T093000"),
            "{ics}"
        );

        let (_, body) = call(&app, "POST", "/api/calendar/reset", Some(&ada), None).await;
        let new_url: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_ne!(new_url["url"], link["url"]);
        assert_eq!(
            call(&app, "GET", path, None, None).await.0,
            StatusCode::NOT_FOUND,
            "old link revoked"
        );
        assert_eq!(
            call(&app, "GET", "/calendar/nope.ics", None, None).await.0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn pending_api_keeps_fired_items_isolated_and_completes_once() {
        let db = Db::open_in_memory().unwrap();
        let service = Service::new(db.clone());
        for id in [42, 66] {
            service
                .set_timezone(
                    Profile {
                        id,
                        first_name: "Test".into(),
                        last_name: String::new(),
                    },
                    UserTz::parse("+3").unwrap(),
                )
                .await
                .unwrap();
        }
        service
            .set_preferences(42, None, None, Some(20))
            .await
            .unwrap();
        let now = Utc::now();
        let reminder = db
            .add_reminder(
                42,
                "Water plants".into(),
                now - chrono::Duration::minutes(1),
                Recurrence::Once,
            )
            .await
            .unwrap();
        let nag = db
            .fire_reminder(reminder.id, now, now + chrono::Duration::minutes(10))
            .await
            .unwrap()
            .unwrap();
        let app = router(Arc::new(WebState {
            service: service.clone(),
            bot_token: TOKEN.into(),
            web_app_url: None,
            yandex: None,
        }));
        let owner = init_data(42, now);
        let other = init_data(66, now);
        let (_, body) = call(&app, "GET", "/api/state", Some(&owner), None).await;
        let state: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(state["reminders"].as_array().unwrap().len(), 0);
        assert_eq!(state["pending"].as_array().unwrap().len(), 1);
        assert_eq!(state["pending"][0]["id"], nag.id);
        assert!(state["pending"][0]["reminder_id"].is_null());
        let (_, body) = call(&app, "GET", "/api/state", Some(&other), None).await;
        assert!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["pending"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let done = format!("/api/pending/{}/done", nag.id);
        let snooze = format!("/api/pending/{}/snooze", nag.id);
        assert_eq!(
            call(&app, "POST", &done, None, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "POST", &done, Some(&other), None).await.0,
            StatusCode::OK
        );
        assert_eq!(service.pending(42).await.unwrap().len(), 1);
        assert_eq!(
            call(
                &app,
                "POST",
                &snooze,
                Some(&other),
                Some(r#"{"duration":"10m"}"#)
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(
                &app,
                "POST",
                &snooze,
                Some(&owner),
                Some(r#"{"duration":"bad"}"#)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &app,
                "POST",
                &snooze,
                Some(&owner),
                Some(r#"{"duration":"10m"}"#)
            )
            .await
            .0,
            StatusCode::OK
        );
        let pending = service.pending(42).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].next_at >= now + chrono::Duration::minutes(9));
        assert!(pending[0].next_at <= Utc::now() + chrono::Duration::minutes(10));
        for _ in 0..2 {
            let (status, body) = call(&app, "POST", &done, Some(&owner), None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                serde_json::from_str::<serde_json::Value>(&body).unwrap()["pending"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(
            db.due_confirmations(now + chrono::Duration::minutes(25))
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            call(
                &app,
                "POST",
                &snooze,
                Some(&owner),
                Some(r#"{"duration":"1h"}"#)
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn text_only_http_edit_preserves_advanced_recurrence_and_anchor() {
        let db = Db::open_in_memory().unwrap();
        let service = Service::new(db.clone());
        let (user, _) = service
            .set_timezone(
                Profile {
                    id: 42,
                    first_name: "Test".into(),
                    last_name: String::new(),
                },
                UserTz::parse("UTC").unwrap(),
            )
            .await
            .unwrap();
        let initial: DateTime<Utc> = "2027-01-31T09:00:00Z".parse().unwrap();
        let reminder = service
            .create(
                &user,
                ReminderInput {
                    description: "Monthly",
                    at: initial.naive_utc(),
                    recurrence: Recurrence::Monthly,
                },
                initial - chrono::Duration::days(1),
            )
            .await
            .unwrap();
        db.fire_reminder(reminder.id, initial, initial)
            .await
            .unwrap();
        let app = router(Arc::new(WebState {
            service: service.clone(),
            bot_token: TOKEN.into(),
            web_app_url: None,
            yandex: None,
        }));
        let owner = init_data_with_language(42, "en", Utc::now());
        let other = init_data(66, Utc::now());
        let uri = format!("/api/reminders/{}", reminder.id);
        let body = r#"{"description":"Renamed","at":"2027-01-31T09:00","recurrence":"monthly","preserve_schedule":true}"#;
        let (status, response) = call(&app, "PUT", &uri, Some(&owner), Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let changed = service.reminder(42, reminder.id).await.unwrap();
        assert_eq!(
            changed.due_at,
            "2027-02-28T09:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(changed.anchor_local, Some(initial.naive_utc()));
        assert!(
            serde_json::from_str::<serde_json::Value>(&response).unwrap()["repeat"]
                .as_str()
                .unwrap()
                .contains("31")
        );
        assert_ne!(
            call(&app, "PUT", &uri, Some(&other), Some(body)).await.0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn too_long_descriptions_have_stable_code_and_private_api_is_not_cached() {
        let app = router(Arc::new(WebState {
            service: Service::new(Db::open_in_memory().unwrap()),
            bot_token: TOKEN.into(),
            web_app_url: None,
            yandex: None,
        }));
        let owner = init_data(42, Utc::now());
        call(
            &app,
            "PUT",
            "/api/timezone",
            Some(&owner),
            Some(r#"{"timezone":"UTC"}"#),
        )
        .await;
        let body = serde_json::json!({ "description": "🌿".repeat(1001), "at": "2099-01-01T09:00", "recurrence": "once" }).to_string();
        let (status, response) =
            call(&app, "POST", "/api/reminders", Some(&owner), Some(&body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response).unwrap()["code"],
            "description_too_long"
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
    }
}
