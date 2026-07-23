//! Unit re-sync shared by the add flow, the refresh endpoint and the
//! periodic updater: fetch the source listing, merge new units, queue
//! downloads when the publication wants them.

use chrono::Utc;
use yomu_domain::{Origin, Publication, ReadingUnit};

use crate::db::UnitFileOp;
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

/// Returns the newly discovered units (twins merged into re-uploads
/// excluded — see `UnitSync::new_units`).
pub async fn refresh_publication(
    state: &AppState,
    publication: &Publication,
) -> Result<Vec<ReadingUnit>, SyncError> {
    // LocalFile publications have nothing to scrape; their refresh arrives
    // with the streamer.
    let Origin::Source {
        source_id,
        source_key,
    } = &publication.origin
    else {
        return Err(SyncError::UnknownSource("local".into()));
    };
    let source = state
        .sources
        .get(source_id)
        .ok_or_else(|| SyncError::UnknownSource(source_id.clone()))?;

    let details = source.manga(source_key).await?;
    let sync = state
        .db
        .sync_units(publication.id, &details.chapters)
        .await?;
    state.db.set_genres(publication.id, &details.genres).await?;
    state
        .db
        .set_last_checked(publication.id, Utc::now())
        .await?;

    // The DB layer only moves rows; apply its page-directory follow-ups
    // (dropped duplicates, downloads handed over to a re-uploaded twin).
    // Best-effort like the downloader's cleanup: a leftover directory is
    // harmless, a missing one falls back to live reading.
    for op in &sync.file_ops {
        match op {
            UnitFileOp::Remove { unit } => {
                let _ = tokio::fs::remove_dir_all(state.unit_dir(publication.id, *unit)).await;
            }
            UnitFileOp::Rename { from, to } => {
                let to_dir = state.unit_dir(publication.id, *to);
                let _ = tokio::fs::remove_dir_all(&to_dir).await;
                let _ = tokio::fs::rename(state.unit_dir(publication.id, *from), to_dir).await;
            }
        }
    }
    let new_units = sync.new_units;

    if publication.auto_download && !new_units.is_empty() {
        let ids: Vec<_> = new_units.iter().map(|c| c.id).collect();
        state.db.mark_pending(&ids).await?;
        state.download_notify.notify_one();
    }

    if !new_units.is_empty() {
        tracing::info!(
            publication = %publication.title,
            new = new_units.len(),
            auto_download = publication.auto_download,
            "new units found"
        );
    }
    Ok(new_units)
}
