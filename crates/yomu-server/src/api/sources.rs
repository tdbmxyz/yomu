use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use serde::Deserialize;
use yomu_domain::{BrowseSort, MangaSummary, SourceInfo, SourceSearchResults};

use super::ApiError;
use crate::state::AppState;

/// Point result covers at the proxy so clients never touch the site CDN
/// (some sites block hotlinking anyway).
fn proxy_covers(items: &mut [MangaSummary]) {
    for item in items {
        if let Some(cover) = item.cover_url.take() {
            item.cover_url = Some(format!(
                "/api/v1/covers?src={}",
                percent_encoding::utf8_percent_encode(&cover, percent_encoding::NON_ALPHANUMERIC),
            ));
        }
    }
}

/// Some sites append a volatile hash to slug URLs (`…/foo-bar-1a2b3c4d`)
/// and rotate it over time, so a key stored at add-time drifts from the
/// live listing. Strip a trailing `-<hex>` (≥6 hex chars, short enough to
/// spare hex-looking words) so the stable stem still matches.
fn slug_stem(key: &str) -> &str {
    if let Some(pos) = key.rfind('-') {
        let suffix = &key[pos + 1..];
        if suffix.len() >= 6 && suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return &key[..pos];
        }
    }
    key
}

/// Mark results that are already tracked. Exact `source_key` match first;
/// failing that, fall back to the slug stem so a rotated suffix still
/// resolves — and heal the drifted key in place when it does. Decoration
/// only: never fails the listing.
async fn annotate_in_library(state: &AppState, source_id: &str, items: &mut [MangaSummary]) {
    let keys = match state.db.library_keys(source_id).await {
        Ok(keys) => keys,
        Err(err) => {
            tracing::warn!(%err, "in-library annotation failed");
            return;
        }
    };
    // Stem → id, keeping only unambiguous stems: a collision means we can't
    // safely say which tracked title an item belongs to, so we don't guess
    // (and, crucially, never heal the wrong row).
    let mut stems: std::collections::HashMap<&str, Option<uuid::Uuid>> =
        std::collections::HashMap::new();
    for (key, id) in &keys {
        stems
            .entry(slug_stem(key))
            .and_modify(|slot| *slot = None)
            .or_insert(Some(*id));
    }

    let mut heals: Vec<(uuid::Uuid, String)> = Vec::new();
    for item in items {
        if let Some(id) = keys.get(&item.key).copied() {
            item.in_library = Some(id);
        } else if let Some(Some(id)) = stems.get(slug_stem(&item.key)).copied() {
            item.in_library = Some(id);
            heals.push((id, item.key.clone()));
        }
    }

    for (id, key) in heals {
        if let Err(err) = state.db.update_source_key(id, &key).await {
            tracing::warn!(%err, %id, "in-library key heal failed");
        }
    }
}

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
    let mut results = source.search(&query.q).await?;
    remember(&state, &id, &results).await;
    annotate_in_library(&state, &id, &mut results).await;
    proxy_covers(&mut results);
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
            let (mut results, error) = match source.search(&q).await {
                Ok(results) => (results, None),
                Err(err) => (Vec::new(), Some(err.to_string())),
            };
            remember(&state, source.id(), &results).await;
            annotate_in_library(&state, source.id(), &mut results).await;
            proxy_covers(&mut results);
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
    let cached = state
        .db
        .read_catalog_page(&id, sort_key, query.page)
        .await?;
    let plan = crate::catalog::CachePlan::decide(
        cached.as_ref().map(|(_, at)| *at),
        state.config.catalog.ttl_secs,
        chrono::Utc::now(),
    );
    match plan {
        crate::catalog::CachePlan::Fresh => {
            let (mut items, _) = cached.expect("Fresh implies cached");
            annotate_in_library(&state, &id, &mut items).await;
            proxy_covers(&mut items);
            Ok(Json(items))
        }
        crate::catalog::CachePlan::Revalidate => {
            let (mut items, _) = cached.expect("Revalidate implies cached");
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
            annotate_in_library(&state, &id, &mut items).await;
            proxy_covers(&mut items);
            Ok(Json(items))
        }
        crate::catalog::CachePlan::Live => {
            let mut items = source.browse(query.sort, query.page).await?;
            store_page(&state, &id, sort_key, query.page, &items).await?;
            annotate_in_library(&state, &id, &mut items).await;
            proxy_covers(&mut items);
            Ok(Json(items))
        }
    }
}

#[derive(Deserialize)]
pub struct CoverQuery {
    src: String,
}

/// Proxied, disk-cached catalog cover. Only covers the catalog knows
/// about are fetched — this is not an open proxy.
pub async fn cover(
    State(state): State<AppState>,
    Query(query): Query<CoverQuery>,
) -> Result<Response, ApiError> {
    use sha2::{Digest, Sha256};
    let hash = format!("{:x}", Sha256::digest(query.src.as_bytes()));
    let dir = state.config.data_dir.join("covers/by-url");

    for ext in ["jpg", "png", "webp", "gif", "avif"] {
        let path = dir.join(format!("{hash}.{ext}"));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(super::library::cover_response(
                bytes,
                crate::downloader::content_type_for(&path),
            ));
        }
    }

    let source_id = state
        .db
        .catalog_source_for_cover(&query.src)
        .await?
        .ok_or(ApiError::NotFound)?;
    let source = state
        .sources
        .get(&source_id)
        .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;
    let url: url::Url = query
        .src
        .parse()
        .map_err(|_| ApiError::Unprocessable("invalid cover url".into()))?;
    let image = source.image(&url).await?;
    let ext = crate::downloader::extension_for(&image.content_type, &url);
    let _ = tokio::fs::create_dir_all(&dir).await;
    let path = dir.join(format!("{hash}.{ext}"));
    let _ = tokio::fs::write(&path, &image.bytes).await;
    Ok(super::library::cover_response(
        image.bytes.to_vec(),
        crate::downloader::content_type_for(&path),
    ))
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
    state
        .db
        .upsert_catalog_entries(source_id, items, now)
        .await?;
    let keys: Vec<String> = items.iter().map(|s| s.key.clone()).collect();
    state
        .db
        .write_catalog_page(source_id, sort, page, &keys, now)
        .await
}

#[cfg(test)]
mod tests {
    use super::slug_stem;

    #[test]
    fn strips_rotating_hex_suffix() {
        // Same title, different rotated suffix → same stem.
        let a = "https://example.test/comics/return-of-the-mount-hua-sect-30e93729";
        let b = "https://example.test/comics/return-of-the-mount-hua-sect-f886a8af";
        assert_eq!(slug_stem(a), slug_stem(b));
        assert_eq!(
            slug_stem(a),
            "https://example.test/comics/return-of-the-mount-hua-sect"
        );
    }

    #[test]
    fn spares_short_and_non_hex_trailing_tokens() {
        // A trailing word that isn't ≥6 hex chars must be left intact, so
        // stable slugs (and hex-looking words like "cafe"/"dead") still
        // match only their exact selves.
        for key in [
            "https://example.test/manga/solo-leveling",
            "https://example.test/manga/the-100",
            "https://example.test/manga/cafe-dead",
            "https://example.test/manga/chapter-house",
        ] {
            assert_eq!(slug_stem(key), key, "should not strip {key}");
        }
    }

    #[test]
    fn keyless_string_is_returned_whole() {
        assert_eq!(slug_stem("noseparators"), "noseparators");
    }
}
