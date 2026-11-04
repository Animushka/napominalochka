//! Calendar HTTP handlers.
use super::dto::CalendarLink;
use super::{ApiError, AuthUser, WebState};
use crate::adapters::calendar;
use crate::domain::UserId;
use axum::{Json, extract::State};
use axum::{
    extract::Path,
    http::header,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use std::sync::Arc;

pub(super) async fn get_calendar_link(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<CalendarLink>, ApiError> {
    calendar_link(&state, auth.id, false).await.map(Json)
}

pub(super) async fn reset_calendar_link(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<CalendarLink>, ApiError> {
    calendar_link(&state, auth.id, true).await.map(Json)
}

pub(super) async fn calendar_link(
    state: &WebState,
    user_id: UserId,
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
pub(super) async fn calendar_feed(
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
    let lang = state.service.preferences(user.id).await?.language;
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
