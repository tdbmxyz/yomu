//! `/api/v1/updates`: updater-found new chapters since a watermark —
//! what shell notifications announce. Read-only, so `OptionalUser`.

use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use yomu_domain::UpdatesResponse;

use super::ApiError;
use crate::auth::OptionalUser;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct UpdatesQuery {
    /// Notification clients supply their watermark; the UI omits it to list
    /// the retained recent feed.
    since: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn list(
    State(state): State<AppState>,
    OptionalUser(_user): OptionalUser,
    Query(q): Query<UpdatesQuery>,
) -> Result<Json<UpdatesResponse>, ApiError> {
    let since = q
        .since
        .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::days(30));
    let updates = state.db.updates_since(since, 100).await?;
    Ok(Json(UpdatesResponse { updates }))
}
