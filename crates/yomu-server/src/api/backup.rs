//! `/api/v1/backup` (export) and `/api/v1/restore` (import): a portable,
//! self-contained JSON snapshot of the shared library plus the requesting
//! user's reading state. See `yomu_domain::backup`.

use axum::Json;
use axum::extract::State;
use yomu_domain::{BACKUP_VERSION, Backup, RestoreSummary};

use super::ApiError;
use crate::auth::CurrentUser;
use crate::state::AppState;

/// Dump the library, its chapters, and this user's read marks and progress.
pub async fn export(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Backup>, ApiError> {
    Ok(Json(Backup {
        version: BACKUP_VERSION,
        exported_at: chrono::Utc::now(),
        categories: state.db.list_categories().await?,
        manga: state.db.list_manga().await?,
        chapters: state.db.export_chapters().await?,
        read_chapter_ids: state.db.read_all_ids(user.id).await?,
        progress: state.db.export_events(user.id).await?,
    }))
}

/// Merge a backup into this instance (additive; existing rows are kept).
pub async fn restore(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(backup): Json<Backup>,
) -> Result<Json<RestoreSummary>, ApiError> {
    if backup.version != BACKUP_VERSION {
        return Err(ApiError::Unprocessable(format!(
            "unsupported backup version {} (this server reads {BACKUP_VERSION})",
            backup.version
        )));
    }
    Ok(Json(state.db.import_backup(user.id, &backup).await?))
}
