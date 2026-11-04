//! HTTP server for the Telegram Mini App: the page itself and a small JSON API.
//!
//! Every API request carries the Mini App's `initData` (in `Authorization: tma <initData>`),
//! which Telegram signs with the bot token. The signature identifies the user, so there are
//! no separate accounts or sessions.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::application::ReminderService;

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

mod calendar;
mod dto;
mod history;
mod pending;
mod preferences;
mod reminders;
mod state;
use calendar::{calendar_feed, get_calendar_link, reset_calendar_link};
use history::{get_history, undo_completion};
use pending::{complete_pending, snooze_pending};
use preferences::{set_preferences, set_timezone};
use reminders::{create_reminder, delete_reminder, update_reminder};
use state::get_state;
