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
    since: chrono::DateTime<chrono::Utc>,
}

pub async fn list(
    State(state): State<AppState>,
    OptionalUser(_user): OptionalUser,
    Query(q): Query<UpdatesQuery>,
) -> Result<Json<UpdatesResponse>, ApiError> {
    let updates = state.db.updates_since(q.since, 100).await?;
    Ok(Json(UpdatesResponse { updates }))
}
