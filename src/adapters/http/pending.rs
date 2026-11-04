//! Pending HTTP handlers.
use super::dto::{SnoozeInput, StateView};
use super::state::load_state;
use super::{ApiError, AuthUser, WebState};
use crate::domain::Snooze;
use axum::extract::Path;
use axum::{Json, extract::State};
use chrono::Utc;
use std::sync::Arc;

pub(super) async fn complete_pending(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Path(id): Path<crate::domain::NaggingId>,
) -> Result<Json<StateView>, ApiError> {
    state.service.done_at(auth.id, id, Utc::now()).await?;
    Ok(Json(load_state(&state.service, auth.id, Utc::now()).await?))
}
pub(super) async fn snooze_pending(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Path(id): Path<crate::domain::NaggingId>,
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
