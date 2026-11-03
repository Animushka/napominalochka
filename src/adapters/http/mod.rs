//! HTTP server for the Telegram Mini App: the page itself and a small JSON API.
//!
//! Every API request carries the Mini App's `initData` (in `Authorization: tma <initData>`),
//! which Telegram signs with the bot token. The signature identifies the user, so there are
//! no separate accounts or sessions.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::adapters::calendar;
use crate::adapters::i18n::Lang;
use crate::adapters::telegram::panels;
use crate::application as service;
use crate::application::{Profile, ReminderInput, ReminderService};
use crate::domain::{Completion, Nagging, Recurrence, Reminder, ReminderSettings, Snooze, UserTz};

const LOCAL_FORMAT: &str = "%Y-%m-%dT%H:%M";

pub(crate) struct WebState {
    pub(crate) service: ReminderService,
    pub(crate) bot_token: String,
    pub(crate) bot_username: String,
    pub(crate) health: crate::application::health::Health,
    /// Public address; calendar feed links are built from it.
    pub(crate) web_app_url: Option<Url>,
    pub(crate) yandex: Option<crate::application::yandex::YandexService>,
}

pub(crate) async fn serve(
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

pub(crate) fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/", get(crate::adapters::http::frontend::index))
        .route(
            "/assets/{*path}",
            get(crate::adapters::http::frontend::asset),
        )
        .route("/avatar.jpg", get(crate::adapters::http::frontend::avatar))
        .route("/avatar.png", get(crate::adapters::http::frontend::avatar))
        .route(
            "/appearance.js",
            get(crate::adapters::http::frontend::appearance),
        )
        .fallback(crate::adapters::http::frontend::not_found)
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
        .merge(crate::adapters::http::yandex::routes())
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
    service: &ReminderService,
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

#[cfg(test)]
mod tests;

pub(crate) mod frontend;
pub(crate) mod yandex;

mod auth;
mod error;
pub(crate) use auth::AuthUser;
#[cfg(test)]
use auth::verify_init_data;
pub(crate) use error::ApiError;
