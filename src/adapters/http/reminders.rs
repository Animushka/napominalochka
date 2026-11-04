//! Reminders HTTP handlers.
use super::dto::{ReminderBody, ReminderView};
use super::{ApiError, AuthUser, WebState};
use crate::application::ReminderInput;
use crate::domain::ReminderSettings;
use axum::{Json, extract::State};
use axum::{extract::Path, http::StatusCode};
use chrono::Utc;
use std::sync::Arc;

pub(super) async fn create_reminder(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Json(input): Json<ReminderBody>,
) -> Result<(StatusCode, Json<ReminderView>), ApiError> {
    let now = Utc::now();
    let user = state.service.registered(auth.id).await?;
    let (at, recurrence) = input.schedule()?;
    let settings = input
        .settings
        .clone()
        .map(ReminderSettings::try_from)
        .transpose()?
        .unwrap_or_default();
    let input = ReminderInput {
        description: &input.description,
        at,
        recurrence,
    };
    let reminder = state
        .service
        .create_with_settings(&user, input, settings, now)
        .await?;
    let lang = state.service.preferences(auth.id).await?.language;
    Ok((
        StatusCode::CREATED,
        Json(ReminderView::new(&reminder, user.tz, now, lang)),
    ))
}

pub(super) async fn update_reminder(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Path(id): Path<crate::domain::ReminderId>,
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
        .edit(
            &user,
            id,
            &input.description,
            schedule,
            input.settings.map(ReminderSettings::try_from).transpose()?,
            now,
        )
        .await?;

    let lang = state.service.preferences(auth.id).await?.language;
    Ok(Json(ReminderView::new(&reminder, user.tz, now, lang)))
}

pub(super) async fn delete_reminder(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Path(id): Path<crate::domain::ReminderId>,
) -> Result<StatusCode, ApiError> {
    state.service.delete(auth.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
