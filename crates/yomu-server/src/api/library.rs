use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;
use yomu_domain::{
    AddPublicationRequest, Origin, Publication, PublicationDetailResponse, PublicationWithLocator,
    RefreshResponse, UpdatePublicationRequest,
};

use super::ApiError;
use crate::auth::{CurrentUser, OptionalUser};
use crate::state::AppState;
use crate::sync;

pub async fn add(
    State(state): State<AppState>,
    _user: CurrentUser,
    Json(req): Json<AddPublicationRequest>,
) -> Result<(StatusCode, Json<Publication>), ApiError> {
    let source = state.sources.get(&req.source_id).ok_or_else(|| {
        ApiError::Unprocessable(format!("source {:?} is not configured", req.source_id))
    })?;
    let details = source.manga(&req.source_key).await?;
    let publication = state
        .db
        .insert_publication(&req.source_id, &details, req.auto_download)
        .await?;

    if req.auto_download {
        let units = state.db.list_units(publication.id).await?;
        let ids: Vec<_> = units.iter().map(|c| c.id).collect();
        state.db.mark_pending(&ids).await?;
        state.download_notify.notify_one();
    }
    Ok((StatusCode::CREATED, Json(publication)))
}

/// The library is server-wide; reading positions are per user (absent when
/// signed out in OIDC mode).
pub async fn list(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
) -> Result<Json<Vec<PublicationWithLocator>>, ApiError> {
    // Three queries total, not 3N+1: unit rollups (counts + latest) and
    // per-publication locators come back grouped, keyed by publication id.
    let rollup_scope = user.as_ref().map(|u| u.id.to_string()).unwrap_or_default();
    let mut rollups = state.db.library_rollups(&rollup_scope).await?;
    let mut positions = match &user {
        Some(user) => state.db.latest_positions(user.id).await?,
        None => Default::default(),
    };

    let out = state
        .db
        .list_publications()
        .await?
        .into_iter()
        .map(|publication| {
            let rollup = rollups.remove(&publication.id).unwrap_or_default();
            let (locator, locator_unit_title) = match positions.remove(&publication.id) {
                Some((locator, title)) => (Some(locator), title),
                None => (None, None),
            };
            PublicationWithLocator {
                locator,
                unit_count: rollup.unit_count,
                unread_count: rollup.unread_count,
                downloaded_count: rollup.downloaded_count,
                latest_unit_at: rollup.latest_unit_at,
                locator_unit_title,
                publication,
            }
        })
        .collect();
    Ok(Json(out))
}

pub async fn detail(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
    Path(id): Path<Uuid>,
) -> Result<Json<PublicationDetailResponse>, ApiError> {
    let publication = state.db.get_publication(id).await?;
    let mut units = state.db.list_units(id).await?;
    let locator = match &user {
        Some(user) => state.db.latest_position(user.id, id).await?,
        None => None,
    };
    if let Some(user) = &user {
        let read = state.db.read_ids(user.id, id).await?;
        for unit in &mut units {
            unit.read = read.contains(&unit.id);
        }
    }
    Ok(Json(PublicationDetailResponse {
        publication,
        units,
        locator,
    }))
}

pub async fn update(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePublicationRequest>,
) -> Result<Json<Publication>, ApiError> {
    let mut publication = state.db.set_auto_download(id, req.auto_download).await?;
    if let Some(category) = &req.category {
        publication = state.db.set_category(id, category).await?;
    }
    Ok(Json(publication))
}

pub async fn delete(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let unit_ids: Vec<Uuid> = state
        .db
        .list_units(id)
        .await?
        .iter()
        .map(|c| c.id)
        .collect();
    state.db.delete_publication(id).await?;
    // Downloaded pages, cached cover and live page lists go with the
    // publication.
    let _ = tokio::fs::remove_dir_all(state.config.data_dir.join(id.to_string())).await;
    let _ = remove_cover_cache(&state, id).await;
    state.live_pages.invalidate_many(&unit_ids).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn refresh(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<RefreshResponse>, ApiError> {
    let publication = state.db.get_publication(id).await?;
    let new_units = sync::refresh_publication(&state, &publication).await?.len() as u32;
    Ok(Json(RefreshResponse {
        new_units,
        checked_at: chrono::Utc::now(),
    }))
}

/// Cover image, proxied from the source once and cached on disk (scan sites
/// often reject hotlinking, and the LAN client shouldn't need the site).
pub async fn cover(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let covers_dir = state.config.data_dir.join("covers");

    for ext in ["jpg", "png", "webp", "gif", "avif"] {
        let path = covers_dir.join(format!("{id}.{ext}"));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(cover_response(
                bytes,
                crate::downloader::content_type_for(&path),
            ));
        }
    }

    let publication = state.db.get_publication(id).await?;
    let cover_url = publication.cover_url.ok_or(ApiError::NotFound)?;
    // LocalFile covers are extracted and served by the streamer once it lands.
    let Origin::Source { source_id, .. } = &publication.origin else {
        return Err(ApiError::Unprocessable(
            "publication is not source-backed".into(),
        ));
    };
    let source = state
        .sources
        .get(source_id)
        .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;
    let image = source.image(&cover_url).await?;

    let ext = crate::downloader::extension_for(&image.content_type, &cover_url);
    let _ = tokio::fs::create_dir_all(&covers_dir).await;
    let path = covers_dir.join(format!("{id}.{ext}"));
    let _ = tokio::fs::write(&path, &image.bytes).await;

    Ok(cover_response(
        image.bytes.to_vec(),
        crate::downloader::content_type_for(&path),
    ))
}

pub(crate) fn cover_response(bytes: Vec<u8>, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            ),
        ],
        bytes,
    )
        .into_response()
}

async fn remove_cover_cache(state: &AppState, id: Uuid) -> std::io::Result<()> {
    let covers_dir = state.config.data_dir.join("covers");
    for ext in ["jpg", "png", "webp", "gif", "avif"] {
        let _ = tokio::fs::remove_file(covers_dir.join(format!("{id}.{ext}"))).await;
    }
    Ok(())
}
