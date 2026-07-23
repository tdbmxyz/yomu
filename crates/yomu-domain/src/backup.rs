//! Portable backup of a yomu instance: the tracked library, its chapters,
//! and the requesting user's reading state. Deliberately self-contained —
//! it carries chapter rows so a restore does not depend on re-reaching the
//! sources. Downloaded page files are not included (they re-download); a
//! restored chapter comes back with its download state reset.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Category, ProgressEvent, Publication, ReadingUnit};

/// Bumped when the shape changes incompatibly; a restore refuses a version
/// it does not understand.
pub const BACKUP_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    pub version: u32,
    pub exported_at: DateTime<Utc>,
    pub categories: Vec<Category>,
    #[serde(rename = "manga")]
    pub publications: Vec<Publication>,
    #[serde(rename = "chapters")]
    pub units: Vec<ReadingUnit>,
    /// Chapter ids the exporting user has marked read.
    #[serde(rename = "read_chapter_ids")]
    pub read_unit_ids: Vec<Uuid>,
    /// The exporting user's progress journal (merged back idempotently).
    pub progress: Vec<ProgressEvent>,
}

/// Summary returned after a restore, so the UI can report what landed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreSummary {
    #[serde(rename = "manga")]
    pub publications: u32,
    #[serde(rename = "chapters")]
    pub units: u32,
    pub categories: u32,
    pub read_marks: u32,
    pub progress_events: u32,
}
