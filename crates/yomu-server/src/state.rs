use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Notify, RwLock};
use url::Url;
use uuid::Uuid;
use yomu_source::registry::Registry;

use crate::config::Config;
use crate::db::Db;

/// Shared application state, cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Db,
    pub sources: Arc<Registry>,
    /// Wakes the download worker when chapters become pending.
    pub download_notify: Arc<Notify>,
    /// Page-URL lists of chapters read live (not downloaded), so a reading
    /// session doesn't re-scrape the chapter page for every image.
    pub live_pages: Arc<RwLock<HashMap<Uuid, Vec<Url>>>>,
}

impl AppState {
    pub fn new(config: Config, db: Db, sources: Registry) -> Self {
        Self {
            config: Arc::new(config),
            db,
            sources: Arc::new(sources),
            download_notify: Arc::new(Notify::new()),
            live_pages: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Directory holding a downloaded chapter's page files.
    pub fn chapter_dir(&self, manga_id: Uuid, chapter_id: Uuid) -> PathBuf {
        self.config
            .data_dir
            .join(manga_id.to_string())
            .join(chapter_id.to_string())
    }
}
