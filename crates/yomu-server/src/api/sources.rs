use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use yomu_domain::{BrowseSort, MangaSummary, SourceInfo, SourceSearchResults};

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
                browse: s.browse_sorts(),
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

/// One query against every configured source at once. Sources answer
/// concurrently; a failing one contributes its error, not a failed request.
pub async fn search_all(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Json<Vec<SourceSearchResults>> {
    let searches = state.sources.iter().map(|source| {
        let source = source.clone();
        let q = query.q.clone();
        async move {
            let (results, error) = match source.search(&q).await {
                Ok(results) => (results, None),
                Err(err) => (Vec::new(), Some(err.to_string())),
            };
            SourceSearchResults {
                source_id: source.id().to_string(),
                source_name: source.name().to_string(),
                results,
                error,
            }
        }
    });
    Json(futures::future::join_all(searches).await)
}

#[derive(Deserialize)]
pub struct BrowseQuery {
    sort: BrowseSort,
    #[serde(default = "first_page")]
    page: u32,
}

fn first_page() -> u32 {
    1
}

/// A source's query-less catalog listing (popular / latest), paged.
pub async fn browse(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<BrowseQuery>,
) -> Result<Json<Vec<MangaSummary>>, ApiError> {
    let source = state.sources.get(&id).ok_or(ApiError::NotFound)?;
    Ok(Json(source.browse(query.sort, query.page).await?))
}
