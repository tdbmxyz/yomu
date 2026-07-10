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
    let results = source.search(&query.q).await?;
    remember(&state, &id, &results).await;
    Ok(Json(results))
}

/// Feed summaries into the catalog cache (warms titles and the cover
/// proxy's allow-list). Caching must never fail a listing.
async fn remember(state: &AppState, source_id: &str, items: &[MangaSummary]) {
    if let Err(err) = state
        .db
        .upsert_catalog_entries(source_id, items, chrono::Utc::now())
        .await
    {
        tracing::warn!(source = source_id, %err, "catalog upsert failed");
    }
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
        let state = state.clone();
        async move {
            let (results, error) = match source.search(&q).await {
                Ok(results) => (results, None),
                Err(err) => (Vec::new(), Some(err.to_string())),
            };
            remember(&state, source.id(), &results).await;
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
/// Served from the catalog cache when possible: a fresh page costs no
/// source request, a stale one is answered immediately and refreshed in
/// the background, only a never-seen page fetches live.
pub async fn browse(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<BrowseQuery>,
) -> Result<Json<Vec<MangaSummary>>, ApiError> {
    let source = state.sources.get(&id).ok_or(ApiError::NotFound)?;
    let sort_key = query.sort.key();
    let cached = state.db.read_catalog_page(&id, sort_key, query.page).await?;
    let plan = crate::catalog::CachePlan::decide(
        cached.as_ref().map(|(_, at)| *at),
        state.config.catalog.ttl_secs,
        chrono::Utc::now(),
    );
    match plan {
        crate::catalog::CachePlan::Fresh => {
            let (items, _) = cached.expect("Fresh implies cached");
            Ok(Json(items))
        }
        crate::catalog::CachePlan::Revalidate => {
            let (items, _) = cached.expect("Revalidate implies cached");
            let flight_key = format!("{id}/{sort_key}/{}", query.page);
            if state.catalog_inflight.start(&flight_key) {
                let state = state.clone();
                let source = source.clone();
                let (sort, page) = (query.sort, query.page);
                tokio::spawn(async move {
                    match source.browse(sort, page).await {
                        Ok(fresh) => {
                            if let Err(err) =
                                store_page(&state, source.id(), sort.key(), page, &fresh).await
                            {
                                tracing::warn!(source = source.id(), %err, "catalog store failed");
                            }
                        }
                        // The stale page stays; it self-heals when the
                        // source answers again.
                        Err(err) => {
                            tracing::warn!(source = source.id(), %err, "catalog revalidation failed");
                        }
                    }
                    state.catalog_inflight.finish(&flight_key);
                });
            }
            Ok(Json(items))
        }
        crate::catalog::CachePlan::Live => {
            let items = source.browse(query.sort, query.page).await?;
            store_page(&state, &id, sort_key, query.page, &items).await?;
            Ok(Json(items))
        }
    }
}

/// Upsert entries then record the page composition.
async fn store_page(
    state: &AppState,
    source_id: &str,
    sort: &str,
    page: u32,
    items: &[MangaSummary],
) -> Result<(), crate::db::DbError> {
    let now = chrono::Utc::now();
    state.db.upsert_catalog_entries(source_id, items, now).await?;
    let keys: Vec<String> = items.iter().map(|s| s.key.clone()).collect();
    state.db.write_catalog_page(source_id, sort, page, &keys, now).await
}
