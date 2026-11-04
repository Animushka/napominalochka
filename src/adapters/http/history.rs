//! History HTTP handlers.
use super::dto::{CompletionView, HistoryQuery, HistoryView, StateView};
use super::state::load_state;
use super::{ApiError, AuthUser, WebState};
use axum::extract::{Path, Query};
use axum::{Json, extract::State};
use chrono::Utc;
use std::sync::Arc;

pub(super) async fn get_history(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryView>, ApiError> {
    if query.before.is_some_and(|before| before.0 <= 0) || query.limit == Some(0) {
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
pub(super) async fn undo_completion(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Path(id): Path<crate::domain::CompletionId>,
) -> Result<Json<StateView>, ApiError> {
    state
        .service
        .undo_completion(auth.id, id, Utc::now())
        .await?;
    Ok(Json(load_state(&state.service, auth.id, Utc::now()).await?))
}
