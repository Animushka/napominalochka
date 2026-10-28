//! HTTP server for the Telegram Mini App: the page itself and a small JSON API.
//!
//! Every API request carries the Mini App's `initData` (in `Authorization: tma <initData>`),
//! which Telegram signs with the bot token. The signature identifies the user, so there are
//! no separate accounts or sessions.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
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
use crate::model::{Recurrence, Reminder, UserTz};
use crate::panels;
use crate::service::{self, Profile, ReminderInput, Service};

const INDEX_HTML: &str = include_str!("../web/index.html");
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
        .route("/", get(index))
        .route(
            "/assets/app.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/assets/i18n.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/i18n.js"),
                )
            }),
        )
        .route(
            "/assets/styles.css",
            get(|| async { asset("text/css; charset=utf-8", include_str!("../web/styles.css")) }),
        )
        .route(
            "/assets/avatar.png",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png")],
                    include_bytes!("../web/avatar.png").as_slice(),
                )
            }),
        )
        .route(
            "/locales/ru.json",
            get(|| async {
                asset(
                    "application/json; charset=utf-8",
                    include_str!("../web/locales/ru.json"),
                )
            }),
        )
        .route(
            "/locales/en.json",
            get(|| async {
                asset(
                    "application/json; charset=utf-8",
                    include_str!("../web/locales/en.json"),
                )
            }),
        )
        .route("/health", get(|| async { "ok" }))
        .route("/api/state", get(get_state))
        .route("/api/preferences", patch(set_preferences))
        .route("/api/timezone", put(set_timezone))
        .route("/api/reminders", post(create_reminder))
        .route(
            "/api/reminders/{id}",
            put(update_reminder).delete(delete_reminder),
        )
        .route("/api/calendar", get(get_calendar_link))
        .route("/api/calendar/reset", post(reset_calendar_link))
        .route("/calendar/{file}", get(calendar_feed))
        .merge(crate::yandex_web::routes())
        .with_state(state)
}

async fn index() -> Response {
    let mut response = Html(INDEX_HTML).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
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
            repeat: lang.recurrence(reminder.recurrence, local),
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
struct ReminderBody {
    description: String,
    at: String,
    recurrence: String,
}

// ---- handlers -------------------------------------------------------------------------

async fn get_state(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<StateView>, ApiError> {
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
    let (at, recurrence) = input.schedule()?;
    let reminder = state
        .service
        .update(&user, id, Some(&input.description), at, recurrence, now)
        .await?;
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
    let base = state
        .web_app_url
        .as_ref()
        .ok_or_else(|| ApiError::BadRequest("The calendar feed is not enabled.".into()))?;
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
        });
    };
    let reminders = service.list(user_id).await?;
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
    })
}

impl ReminderBody {
    /// Parses the wire format; everything else is validated by the service.
    fn schedule(&self) -> Result<(NaiveDateTime, Recurrence), ApiError> {
        let at = NaiveDateTime::parse_from_str(&self.at, LOCAL_FORMAT)
            .map_err(|_| ApiError::BadRequest("Please pick a date and time.".into()))?;
        let recurrence = self.recurrence.parse().map_err(service::Error::from)?;
        Ok((at, recurrence))
    }
}

// ---- errors ---------------------------------------------------------------------------

pub enum ApiError {
    Unauthorized,
    NotFound,
    BadRequest(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        ApiError::Internal(error)
    }
}

impl From<service::Error> for ApiError {
    fn from(error: service::Error) -> Self {
        match error {
            service::Error::NotFound => ApiError::NotFound,
            service::Error::NotRegistered
            | service::Error::Input(_)
            | service::Error::InvalidPreferences(_) => ApiError::BadRequest(error.to_string()),
            service::Error::Internal(error) => ApiError::Internal(error),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "Please open this page from Telegram.".to_owned(),
            ),
            ApiError::NotFound => (
                StatusCode::NOT_FOUND,
                "That reminder no longer exists.".to_owned(),
            ),
            ApiError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            ApiError::Internal(error) => {
                tracing::error!(?error, "web request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong. Please try again.".to_owned(),
                )
            }
        };
        let code = match message.as_str() {
            "Please open this page from Telegram." => "unauthorized",
            "That reminder no longer exists." | "This reminder is no longer available." => {
                "reminder_not_found"
            }
            "Please set your timezone first." => "invalid_timezone",
            "Please pick a date and time." => "invalid_datetime",
            "Please enter what to remind you about." => "empty_description",
            "That time has already passed." => "past_time",
            "Something went wrong. Please try again." => "internal_error",
            "The calendar feed is not enabled." => "calendar_disabled",
            _ if message.starts_with("Invalid preference") => "invalid_preferences",
            _ if message.starts_with("I don't recognise that timezone") => "invalid_timezone",
            _ if message.starts_with("Repeat must be one of") => "invalid_recurrence",
            _ => "invalid_request",
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
}
