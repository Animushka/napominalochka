//! Maps application failures to stable HTTP responses.
use crate::application as service;
use crate::domain::InputError;
use axum::Json;
use serde::Serialize;
use std::borrow::Cow;

#[derive(Serialize)]
pub(super) struct ErrorBody {
    pub(super) error: Cow<'static, str>,
    pub(super) code: &'static str,
}

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

pub(crate) enum ApiError {
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
    pub(super) fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
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
            service::Error::Storage(error) => ApiError::Internal(error.into()),
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
            Json(ErrorBody {
                error: message.into(),
                code,
            }),
        )
            .into_response()
    }
}
