//! State HTTP handlers.
use super::dto::{LOCAL_FORMAT, PendingView, ReminderView, StateView};
use super::{ApiError, AuthUser, WebState};
use crate::application::{self as service, ReminderService};
use crate::domain::{UserId, UserTz};
use axum::{Json, extract::State};
use chrono::DateTime;
use chrono::Utc;
use std::sync::Arc;

pub(super) async fn get_state(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<StateView>, ApiError> {
    Ok(Json(load_state(&state.service, auth.id, Utc::now()).await?))
}

pub(super) async fn load_state(
    service: &ReminderService,
    user_id: UserId,
    now: DateTime<Utc>,
) -> service::Result<StateView> {
    let preferences = service.preferences(user_id).await?;
    let lang = preferences.language;
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
