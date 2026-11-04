//! Preferences HTTP handlers.
use super::dto::{PreferencesInput, StateView, TimezoneInput};
use super::state::load_state;
use super::{ApiError, AuthUser, WebState};
use crate::application::{self as service, Profile};
use crate::domain::UserTz;
use axum::{Json, extract::State};
use chrono::Utc;
use std::sync::Arc;

pub(super) async fn set_preferences(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Json(input): Json<PreferencesInput>,
) -> Result<Json<StateView>, ApiError> {
    state
        .service
        .set_preferences(
            auth.id,
            input
                .language
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(|_| service::Error::InvalidPreferences("language"))?,
            input
                .nag_interval_minutes
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| service::Error::InvalidPreferences("nag_interval_minutes"))?,
            input
                .confirmation_check_minutes
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| service::Error::InvalidPreferences("confirmation_check_minutes"))?,
        )
        .await?;
    Ok(Json(load_state(&state.service, auth.id, Utc::now()).await?))
}

pub(super) async fn set_timezone(
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
