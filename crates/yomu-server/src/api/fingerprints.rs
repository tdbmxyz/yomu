//! Content fingerprints for downloaded units.
//!
//! A device keeps its downloads in a directory named after the unit id, so a
//! source that re-keys its URLs orphans every one of them and no mapping
//! survives to undo it. What does survive is the bytes: a device page is a
//! byte-for-byte copy of the server's stored page. So `(page_count, sha256 of
//! the first page)` names a chapter that both sides can compute, and a client
//! can re-key its own storage from it.

use axum::Json;
use axum::extract::{Path, State};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use yomu_domain::{DownloadState, FingerprintsResponse, UnitFingerprint};

use super::ApiError;
use crate::state::AppState;

/// One entry per downloaded unit of this publication. Units that are not
/// downloaded are omitted rather than reported empty: there is nothing on
/// disk to match against, and an entry without content would only invite a
/// false match.
///
/// This reads the first page of every downloaded unit, which is a few hundred
/// files for a large publication — acceptable for a call made once, by hand,
/// to repair a library.
pub async fn list(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<FingerprintsResponse>, ApiError> {
    // A publication that is gone must not read as one with nothing
    // downloaded: this is the call a client makes to decide what its own
    // storage still corresponds to, and an empty list would have it conclude
    // "no matches" instead of "no such publication". `list_units` is happy to
    // return nothing for an unknown id, so ask first.
    state.db.get_publication(id).await?;
    let units = state.db.list_units(id).await?;
    let mut fingerprints = Vec::new();

    for unit in units {
        if !matches!(unit.download, DownloadState::Downloaded { .. }) {
            continue;
        }
        // A unit can be marked downloaded and have lost its directory (wiped
        // disk, moved data_dir). Nothing to fingerprint, so skip it the way
        // page serving skips it, rather than failing the whole call.
        let files = match crate::downloader::page_files(&state, &unit).await {
            Ok(files) if !files.is_empty() => files,
            _ => continue,
        };
        // `page_files` returns reading order, so the first entry is the
        // lowest-numbered page — the same one the device stored as page 0 —
        // and the last entry is its last page. Both ends are hashed: a shared
        // first page is common enough (credits, a site splash) that it cannot
        // carry the identity of a chapter on its own.
        let (Ok(first), Ok(last)) = (
            tokio::fs::read(&files[0]).await,
            tokio::fs::read(files.last().expect("non-empty")).await,
        ) else {
            continue;
        };
        fingerprints.push(UnitFingerprint {
            unit_id: unit.id,
            page_count: files.len() as u32,
            page0_sha256: hex::encode(Sha256::digest(&first)),
            page_last_sha256: hex::encode(Sha256::digest(&last)),
        });
    }

    Ok(Json(FingerprintsResponse { fingerprints }))
}
