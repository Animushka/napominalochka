//! HTTP server for the Telegram Mini App: the page itself and a small JSON API.
//!
//! Every API request carries the Mini App's `initData` (in `Authorization: tma <initData>`),
//! which Telegram signs with the bot token. The signature identifies the user, so there are
//! no separate accounts or sessions.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{FromRequestParts, Path, Query, Request, State};
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
use crate::model::{
    Completion, InputError, Nagging, Recurrence, Reminder, ReminderSettings, Snooze, UserTz,
};
use crate::panels;
use crate::service::{self, Profile, ReminderInput, Service};

/// `initData` older than this is rejected, limiting how long a leaked copy is useful.
const MAX_INIT_DATA_AGE_SECS: i64 = 24 * 60 * 60;
const LOCAL_FORMAT: &str = "%Y-%m-%dT%H:%M";

type HmacSha256 = hmac::Hmac<Sha256>;

pub struct WebState {
    pub service: Service,
    pub bot_token: String,
    pub bot_username: String,
    pub health: crate::health::Health,
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
        .route("/health", get(health))
        .route("/api/state", get(get_state))
        .route("/api/history", get(get_history))
        .route("/api/history/{id}/undo", post(undo_completion))
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

async fn health(State(state): State<Arc<WebState>>) -> Response {
    if !state.health.is_ready() {
        return (StatusCode::SERVICE_UNAVAILABLE, "unavailable").into_response();
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.service.check_storage(),
    )
    .await
    {
        Ok(Ok(())) => (StatusCode::OK, "ok").into_response(),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "unavailable").into_response(),
    }
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
    settings: ReminderSettings,
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
            settings: reminder.settings.clone(),
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
    settings: Option<ReminderSettings>,
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
    let settings = input.settings.clone().unwrap_or_default();
    let input = ReminderInput {
        description: &input.description,
        at,
        recurrence,
    };
    let reminder = state
        .service
        .create_with_settings(&user, input, settings, now)
        .await?;
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
    let schedule = if input.preserve_schedule {
        None
    } else {
        Some(input.schedule()?)
    };
    let reminder = state
        .service
        .edit(&user, id, &input.description, schedule, input.settings, now)
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    before: Option<i64>,
    limit: Option<usize>,
}
#[derive(Serialize)]
struct CompletionView {
    id: i64,
    description: String,
    scheduled_at: Option<String>,
    completed_at: String,
    undone_at: Option<String>,
    can_undo: bool,
}
impl CompletionView {
    fn new(item: Completion, tz: UserTz) -> Self {
        let local = |at| tz.local(at).format(LOCAL_FORMAT).to_string();
        Self {
            id: item.id,
            description: item.description,
            scheduled_at: item.scheduled_at.map(local),
            completed_at: local(item.completed_at),
            undone_at: item.undone_at.map(local),
            can_undo: item.can_undo,
        }
    }
}
#[derive(Serialize)]
struct HistoryView {
    items: Vec<CompletionView>,
    next_cursor: Option<i64>,
}
async fn get_history(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryView>, ApiError> {
    if query.before.is_some_and(|before| before <= 0) || query.limit == Some(0) {
        return Err(ApiError::bad_request(
            "invalid_request",
            "Choose a valid history page.",
        ));
    }
    let user = state.service.registered(auth.id).await?;
    let page = state
        .service
        .history(auth.id, query.before, query.limit.unwrap_or(20))
        .await?;
    Ok(Json(HistoryView {
        items: page
            .items
            .into_iter()
            .map(|item| CompletionView::new(item, user.tz))
            .collect(),
        next_cursor: page.next_cursor,
    }))
}
async fn undo_completion(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Result<Json<StateView>, ApiError> {
    state
        .service
        .undo_completion(auth.id, id, Utc::now())
        .await?;
    Ok(Json(load_state(&state.service, auth.id, Utc::now()).await?))
}

// ---- errors ---------------------------------------------------------------------------

pub enum ApiError {
    Unauthorized,
    NotFound,
    Conflict { code: &'static str, message: String },
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
            service::Error::CompletionUnavailable => ApiError::Conflict {
                code: "completion_unavailable",
                message: "This completion can no longer be undone.".into(),
            },
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
            ApiError::Conflict { code, message } => (StatusCode::CONFLICT, code, message),
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
mod tests;
