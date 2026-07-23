use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use url::Url;
use uuid::Uuid;
use yomu_domain::{
    BulkUnitsResponse, DownloadState, DownloadUnitsRequest, MarkUnitsRequest, Origin,
    PagesResponse, ReadingUnit,
};

use super::ApiError;
use crate::auth::CurrentUser;
use crate::state::AppState;

/// Enqueue a chapter for download to the server's disk.
pub async fn download(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<ReadingUnit>), ApiError> {
    let chapter = state.db.get_unit(id).await?;
    if matches!(
        chapter.download,
        DownloadState::Downloaded { .. } | DownloadState::Downloading | DownloadState::Pending
    ) {
        return Ok((StatusCode::OK, Json(chapter)));
    }
    state.db.mark_pending(&[id]).await?;
    state.download_notify.notify_one();
    Ok((StatusCode::ACCEPTED, Json(state.db.get_unit(id).await?)))
}

/// Enqueue a batch of chapters. They join the same single-worker queue as
/// individual downloads, which drains sequentially with the source's
/// politeness delay between requests — a big selection downloads slowly on
/// purpose.
pub async fn download_many(
    State(state): State<AppState>,
    _user: CurrentUser,
    Json(req): Json<DownloadUnitsRequest>,
) -> Result<(StatusCode, Json<BulkUnitsResponse>), ApiError> {
    let queued = state.db.mark_pending(&req.unit_ids).await?;
    if queued > 0 {
        state.download_notify.notify_one();
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(BulkUnitsResponse { affected: queued }),
    ))
}

/// Remove the server copies of the given chapters: rows reset, page
/// directories deleted. Chapters not currently downloaded are skipped.
pub async fn remove_downloads(
    State(state): State<AppState>,
    _user: CurrentUser,
    Json(req): Json<DownloadUnitsRequest>,
) -> Result<Json<BulkUnitsResponse>, ApiError> {
    let removed = state.db.remove_downloads(&req.unit_ids).await?;
    for id in &removed {
        if let Ok(chapter) = state.db.get_unit(*id).await {
            let _ = tokio::fs::remove_dir_all(state.unit_dir(chapter.publication_id, *id)).await;
        }
    }
    state.live_pages.invalidate_many(&removed).await;
    Ok(Json(BulkUnitsResponse {
        affected: removed.len() as u32,
    }))
}

/// Mark chapters read or unread for the current user.
pub async fn mark(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<MarkUnitsRequest>,
) -> Result<Json<BulkUnitsResponse>, ApiError> {
    let affected = if req.read {
        state.db.mark_read(user.id, &req.unit_ids).await?
    } else {
        state.db.mark_unread(user.id, &req.unit_ids).await?
    };
    Ok(Json(BulkUnitsResponse { affected }))
}

/// Page count for the reader. For non-downloaded chapters this resolves the
/// page list live from the source (cached with a TTL).
pub async fn pages(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<PagesResponse>, ApiError> {
    let chapter = state.db.get_unit(id).await?;

    if let Some(files) = downloaded_files(&state, &chapter).await {
        return Ok(Json(PagesResponse {
            unit_id: id,
            page_count: files.len() as u32,
            downloaded: true,
        }));
    }

    let urls = live_pages(&state, &chapter).await?;
    Ok(Json(PagesResponse {
        unit_id: id,
        page_count: urls.len() as u32,
        downloaded: false,
    }))
}

/// One page image: from disk when downloaded, proxied live otherwise
/// (nothing is stored in the live case — "read from server, no local save").
pub async fn page_image(
    State(state): State<AppState>,
    Path((id, n)): Path<(Uuid, u32)>,
) -> Result<Response, ApiError> {
    let chapter = state.db.get_unit(id).await?;

    if let Some(files) = downloaded_files(&state, &chapter).await {
        let path = files.get(n as usize).ok_or(ApiError::NotFound)?;
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|_| ApiError::NotFound)?;
        return Ok(image_response(
            bytes,
            crate::downloader::content_type_for(path).to_string(),
        ));
    }

    let publication = state.db.get_publication(chapter.publication_id).await?;
    let urls = live_pages(&state, &chapter).await?;
    let url = urls.get(n as usize).ok_or(ApiError::NotFound)?;
    match &publication.origin {
        // No CDN-expiry retry for local files: `local:` URLs don't expire.
        Origin::LocalFile { .. } => {
            let image = state.streamer.image(url).await?;
            Ok(image_response(image.bytes.to_vec(), image.content_type))
        }
        Origin::Source { source_id, .. } => {
            let source = state
                .sources
                .get(source_id)
                .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;
            match source.image(url).await {
                Ok(image) => Ok(image_response(image.bytes.to_vec(), image.content_type)),
                // The cached page list may hold expired CDN URLs; re-resolve
                // the chapter once and retry before giving up.
                Err(_) => {
                    state.live_pages.invalidate(chapter.id).await;
                    let urls = live_pages(&state, &chapter).await?;
                    let url = urls.get(n as usize).ok_or(ApiError::NotFound)?;
                    let image = source.image(url).await?;
                    Ok(image_response(image.bytes.to_vec(), image.content_type))
                }
            }
        }
    }
}

/// Page files of a downloaded chapter — `None` when the chapter isn't
/// downloaded *or* its directory vanished (wiped disk, moved data_dir), in
/// which case serving falls back to the live path instead of erroring.
async fn downloaded_files(
    state: &AppState,
    chapter: &ReadingUnit,
) -> Option<Vec<std::path::PathBuf>> {
    if !matches!(chapter.download, DownloadState::Downloaded { .. }) {
        return None;
    }
    match crate::downloader::page_files(state, chapter).await {
        Ok(files) if !files.is_empty() => Some(files),
        _ => {
            tracing::warn!(
                chapter = %chapter.id,
                "chapter marked downloaded but files are missing; serving live"
            );
            None
        }
    }
}

/// Page-URL list for a live-read chapter, resolved once per TTL window.
async fn live_pages(state: &AppState, unit: &ReadingUnit) -> Result<Vec<Url>, ApiError> {
    if let Some(urls) = state.live_pages.get(unit.id).await {
        return Ok(urls);
    }

    let publication = state.db.get_publication(unit.publication_id).await?;
    let urls = match &publication.origin {
        Origin::LocalFile { .. } => state.streamer.pages(&unit.source_key).await?,
        Origin::Source { source_id, .. } => {
            let source = state
                .sources
                .get(source_id)
                .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;
            source.pages(&unit.source_key).await?
        }
    };

    // Remember the count for the UI even after restart.
    let _ = state.db.set_page_count(unit.id, urls.len() as u32).await;
    state.live_pages.put(unit.id, urls.clone()).await;
    Ok(urls)
}

fn image_response(bytes: Vec<u8>, content_type: String) -> Response {
    let content_type =
        HeaderValue::from_str(&content_type).unwrap_or(HeaderValue::from_static("image/jpeg"));
    (
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=3600"),
            ),
        ],
        bytes,
    )
        .into_response()
}
