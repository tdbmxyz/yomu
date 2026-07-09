use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;
use yomu_domain::{
    EventsResponse, Position, ProgressEvent, PushEventsRequest, PushEventsResponse,
    SetPositionRequest,
};

use super::ApiError;
use crate::auth::CurrentUser;
use crate::state::AppState;

/// Online client reporting its position: the server wraps it into a journal
/// event with a server-side timestamp. Per-user (the shared account in
/// single mode).
pub async fn set_position(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
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
    state.db.append_event(user.id, &event).await?;
    auto_mark_read(&state, user.id, std::slice::from_ref(&event)).await;
    Ok(Json(Position {
        chapter_id: event.chapter_id,
        page: event.page,
        at: event.at,
    }))
}

/// Fold position events into read marks: everything strictly before the
/// event's chapter in reading order is read, and the chapter itself once
/// its last known page is reached. Only ever adds marks — re-reading an old
/// chapter doesn't unmark later ones, and explicit "mark unread" survives
/// unrelated reading. Best-effort: a failure here must not fail the
/// position write.
async fn auto_mark_read(state: &AppState, user_id: Uuid, events: &[ProgressEvent]) {
    let mut by_manga: std::collections::HashMap<Uuid, Vec<&ProgressEvent>> = Default::default();
    for event in events {
        by_manga.entry(event.manga_id).or_default().push(event);
    }
    for (manga_id, events) in by_manga {
        // Deleted manga: no chapters, nothing to mark.
        let Ok(chapters) = state.db.list_chapters(manga_id).await else {
            continue;
        };
        let mut ids = std::collections::HashSet::new();
        for event in events {
            let Some(idx) = chapters.iter().position(|c| c.id == event.chapter_id) else {
                continue;
            };
            ids.extend(chapters[..idx].iter().map(|c| c.id));
            if chapters[idx]
                .page_count
                .is_some_and(|n| event.page.saturating_add(1) >= n)
            {
                ids.insert(event.chapter_id);
            }
        }
        if ids.is_empty() {
            continue;
        }
        let ids: Vec<Uuid> = ids.into_iter().collect();
        if let Err(err) = state.db.mark_read(user_id, &ids).await {
            tracing::warn!(%err, %manga_id, "auto-marking chapters read");
        }
    }
}

/// Offline client pushing its journal on reconnect. Idempotent: events are
/// keyed by their client-generated ids. Events for manga deleted meanwhile
/// are skipped (reported in the response) rather than failing the batch —
/// a permanently failing batch would wedge the client's outbox forever.
pub async fn push_events(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<PushEventsRequest>,
) -> Result<Json<PushEventsResponse>, ApiError> {
    let (accepted, skipped) = state.db.append_events(user.id, &req.events).await?;
    if skipped > 0 {
        tracing::debug!(accepted, skipped, "journal push skipped stale events");
    }
    auto_mark_read(&state, user.id, &req.events).await;
    Ok(Json(PushEventsResponse { accepted, skipped }))
}

#[derive(Deserialize)]
pub struct EventsQuery {
    /// Server-assigned arrival cursor (`next_since` of the previous page).
    since: Option<i64>,
}

pub async fn events(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(query): Query<EventsQuery>,
) -> Result<Json<EventsResponse>, ApiError> {
    let (events, next_since) = state.db.events_since(user.id, query.since).await?;
    Ok(Json(EventsResponse { events, next_since }))
}
