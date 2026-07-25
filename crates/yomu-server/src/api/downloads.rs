//! `/api/v1/downloads`: a view of the download queue (pending / downloading
//! / failed) with live per-page progress, plus retry/dismiss actions and a
//! server-storage summary. See `yomu_domain::api`.

use axum::Json;
use axum::extract::State;
use yomu_domain::{DownloadProgress, DownloadQueueEntry, DownloadUnitsRequest, DownloadsResponse};

use super::ApiError;
use crate::auth::{CurrentUser, OptionalUser};
use crate::state::AppState;

/// The queue plus a server-storage summary. Read-only, so `OptionalUser`.
pub async fn list(
    State(state): State<AppState>,
    OptionalUser(_user): OptionalUser,
) -> Result<Json<DownloadsResponse>, ApiError> {
    let chapters = state.db.download_queue().await?;
    let publication_ids: Vec<_> = chapters.iter().map(|c| c.publication_id).collect();
    let titles = state.db.publication_titles(&publication_ids).await?;
    let active = *state.download_progress.read().await;

    let queue = chapters
        .into_iter()
        .map(|chapter| {
            let progress = active
                .filter(|a| a.unit_id == chapter.id)
                .map(|a| DownloadProgress {
                    page: a.page,
                    total: a.total,
                });
            DownloadQueueEntry {
                publication_title: titles
                    .get(&chapter.publication_id)
                    .cloned()
                    .unwrap_or_default(),
                unit_id: chapter.id,
                publication_id: chapter.publication_id,
                unit_title: chapter.title,
                state: chapter.download,
                progress,
            }
        })
        .collect();

    let (server_downloaded_chapters, server_downloaded_pages) =
        state.db.downloaded_summary().await?;

    Ok(Json(DownloadsResponse {
        queue,
        server_downloaded_chapters,
        server_downloaded_pages,
    }))
}

/// Re-queue failed chapters and wake the worker.
pub async fn retry(
    State(state): State<AppState>,
    _user: CurrentUser,
    Json(req): Json<DownloadUnitsRequest>,
) -> Result<Json<yomu_domain::BulkUnitsResponse>, ApiError> {
    let affected = state.db.retry_failed(&req.unit_ids).await?;
    if affected > 0 {
        state.download_notify.notify_one();
    }
    Ok(Json(yomu_domain::BulkUnitsResponse { affected }))
}

/// Drop pending/failed chapters from the queue.
pub async fn dismiss(
    State(state): State<AppState>,
    _user: CurrentUser,
    Json(req): Json<DownloadUnitsRequest>,
) -> Result<Json<yomu_domain::BulkUnitsResponse>, ApiError> {
    let affected = state.db.dismiss_downloads(&req.unit_ids).await?;
    Ok(Json(yomu_domain::BulkUnitsResponse { affected }))
}
