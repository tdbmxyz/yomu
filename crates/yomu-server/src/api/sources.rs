use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use yomu_domain::{MangaSummary, SourceInfo};

use super::ApiError;
use crate::state::AppState;

pub async fn list(State(state): State<AppState>) -> Json<Vec<SourceInfo>> {
    Json(
        state
            .sources
            .iter()
            .map(|s| SourceInfo {
                id: s.id().to_string(),
                name: s.name().to_string(),
                base_url: s.base_url().clone(),
            })
            .collect(),
    )
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
}

pub async fn search(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<MangaSummary>>, ApiError> {
    let source = state.sources.get(&id).ok_or(ApiError::NotFound)?;
    Ok(Json(source.search(&query.q).await?))
}
