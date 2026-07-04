//! Chapter re-sync shared by the add flow, the refresh endpoint and the
//! periodic updater: fetch the source listing, merge new chapters, queue
//! downloads when the manga wants them.

use chrono::Utc;
use yomu_domain::Manga;

use crate::state::AppState;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("source {0:?} is not configured")]
    UnknownSource(String),
    #[error(transparent)]
    Source(#[from] yomu_source::SourceError),
    #[error(transparent)]
    Db(#[from] crate::db::DbError),
}

/// Returns the number of newly discovered chapters.
pub async fn refresh_manga(state: &AppState, manga: &Manga) -> Result<u32, SyncError> {
    let source = state
        .sources
        .get(&manga.source_id)
        .ok_or_else(|| SyncError::UnknownSource(manga.source_id.clone()))?;

    let details = source.manga(&manga.source_key).await?;
    let new_chapters = state.db.sync_chapters(manga.id, &details.chapters).await?;
    state.db.set_last_checked(manga.id, Utc::now()).await?;

    if manga.auto_download && !new_chapters.is_empty() {
        let ids: Vec<_> = new_chapters.iter().map(|c| c.id).collect();
        state.db.mark_pending(&ids).await?;
        state.download_notify.notify_one();
    }

    if !new_chapters.is_empty() {
        tracing::info!(
            manga = %manga.title,
            new = new_chapters.len(),
            auto_download = manga.auto_download,
            "new chapters found"
        );
    }
    Ok(new_chapters.len() as u32)
}
