use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use url::Url;
use uuid::Uuid;
use yomu_domain::{
    BulkChaptersResponse, Chapter, DownloadChaptersRequest, DownloadState, MarkChaptersRequest,
    PagesResponse,
};

use super::ApiError;
use crate::auth::CurrentUser;
use crate::state::AppState;

/// Enqueue a chapter for download to the server's disk.
pub async fn download(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Chapter>), ApiError> {
    let chapter = state.db.get_chapter(id).await?;
    if matches!(
        chapter.download,
        DownloadState::Downloaded { .. } | DownloadState::Downloading | DownloadState::Pending
    ) {
        return Ok((StatusCode::OK, Json(chapter)));
    }
    state.db.mark_pending(&[id]).await?;
    state.download_notify.notify_one();
    Ok((StatusCode::ACCEPTED, Json(state.db.get_chapter(id).await?)))
}

/// Enqueue a batch of chapters. They join the same single-worker queue as
/// individual downloads, which drains sequentially with the source's
/// politeness delay between requests — a big selection downloads slowly on
/// purpose.
pub async fn download_many(
    State(state): State<AppState>,
    Json(req): Json<DownloadChaptersRequest>,
) -> Result<(StatusCode, Json<BulkChaptersResponse>), ApiError> {
    let queued = state.db.mark_pending(&req.chapter_ids).await?;
    if queued > 0 {
        state.download_notify.notify_one();
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(BulkChaptersResponse { affected: queued }),
    ))
}

/// Mark chapters read or unread for the current user.
pub async fn mark(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<MarkChaptersRequest>,
) -> Result<Json<BulkChaptersResponse>, ApiError> {
    let affected = if req.read {
        state.db.mark_read(user.id, &req.chapter_ids).await?
    } else {
        state.db.mark_unread(user.id, &req.chapter_ids).await?
    };
    Ok(Json(BulkChaptersResponse { affected }))
}

/// Page count for the reader. For non-downloaded chapters this resolves the
/// page list live from the source (cached with a TTL).
pub async fn pages(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<PagesResponse>, ApiError> {
    let chapter = state.db.get_chapter(id).await?;

    if let Some(files) = downloaded_files(&state, &chapter).await {
        return Ok(Json(PagesResponse {
            chapter_id: id,
            page_count: files.len() as u32,
            downloaded: true,
        }));
    }

    let urls = live_pages(&state, &chapter).await?;
    Ok(Json(PagesResponse {
        chapter_id: id,
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
    let chapter = state.db.get_chapter(id).await?;

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

    let manga = state.db.get_manga(chapter.manga_id).await?;
    let source = state
        .sources
        .get(&manga.source_id)
        .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;

    let urls = live_pages(&state, &chapter).await?;
    let url = urls.get(n as usize).ok_or(ApiError::NotFound)?;
    match source.image(url).await {
        Ok(image) => Ok(image_response(image.bytes.to_vec(), image.content_type)),
        // The cached page list may hold expired CDN URLs; re-resolve the
        // chapter once and retry before giving up.
        Err(_) => {
            state.live_pages.invalidate(chapter.id).await;
            let urls = live_pages(&state, &chapter).await?;
            let url = urls.get(n as usize).ok_or(ApiError::NotFound)?;
            let image = source.image(url).await?;
            Ok(image_response(image.bytes.to_vec(), image.content_type))
        }
    }
}

/// Page files of a downloaded chapter — `None` when the chapter isn't
/// downloaded *or* its directory vanished (wiped disk, moved data_dir), in
/// which case serving falls back to the live path instead of erroring.
async fn downloaded_files(state: &AppState, chapter: &Chapter) -> Option<Vec<std::path::PathBuf>> {
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
async fn live_pages(state: &AppState, chapter: &Chapter) -> Result<Vec<Url>, ApiError> {
    if let Some(urls) = state.live_pages.get(chapter.id).await {
        return Ok(urls);
    }

    let manga = state.db.get_manga(chapter.manga_id).await?;
    let source = state
        .sources
        .get(&manga.source_id)
        .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;
    let urls = source.pages(&chapter.source_key).await?;

    // Remember the count for the UI even after restart.
    let _ = state.db.set_page_count(chapter.id, urls.len() as u32).await;
    state.live_pages.put(chapter.id, urls.clone()).await;
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
