use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;
use yomu_domain::{EventsResponse, Position, ProgressEvent, PushEventsRequest, SetPositionRequest};

use super::ApiError;
use crate::state::AppState;

/// Online client reporting its position: the server wraps it into a journal
/// event with a server-side timestamp.
pub async fn set_position(
    State(state): State<AppState>,
    Path(manga_id): Path<Uuid>,
    Json(req): Json<SetPositionRequest>,
) -> Result<Json<Position>, ApiError> {
    // The chapter must belong to the manga; catches stale/foreign ids.
    let chapter = state.db.get_chapter(req.chapter_id).await?;
    if chapter.manga_id != manga_id {
        return Err(ApiError::Unprocessable(
            "chapter does not belong to this manga".into(),
        ));
    }

    let event = ProgressEvent {
        id: Uuid::now_v7(),
        manga_id,
        chapter_id: req.chapter_id,
        page: req.page,
        device: req.device,
        at: Utc::now(),
    };
    state.db.append_event(&event).await?;
    Ok(Json(Position {
        chapter_id: event.chapter_id,
        page: event.page,
        at: event.at,
    }))
}

/// Offline client pushing its journal on reconnect. Idempotent: events are
/// keyed by their client-generated ids.
pub async fn push_events(
    State(state): State<AppState>,
    Json(req): Json<PushEventsRequest>,
) -> Result<StatusCode, ApiError> {
    for event in &req.events {
        state.db.append_event(event).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct EventsQuery {
    /// Return events with id strictly greater than this cursor.
    since: Option<Uuid>,
}

pub async fn events(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<EventsResponse>, ApiError> {
    Ok(Json(EventsResponse {
        events: state.db.events_since(query.since).await?,
    }))
}
