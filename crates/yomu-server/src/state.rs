use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Notify, RwLock};
use url::Url;
use uuid::Uuid;
use yomu_source::registry::Registry;

use crate::config::Config;
use crate::db::Db;

/// How long a live-read chapter's page-URL list stays valid. Scan sites
/// commonly serve expiring CDN image URLs; a stale list would 502 every
/// page until restart.
const LIVE_PAGES_TTL: Duration = Duration::from_secs(30 * 60);
/// Cap on cached live chapters (a reading session touches a handful).
const LIVE_PAGES_MAX: usize = 64;

struct LiveEntry {
    urls: Vec<Url>,
    resolved_at: Instant,
}

/// Page-URL lists of chapters read live (not downloaded), so a reading
/// session doesn't re-scrape the chapter page for every image. Entries
/// expire (TTL), are bounded, and are dropped when the chapter is
/// downloaded, fails to serve, or its manga is deleted.
#[derive(Default)]
pub struct LivePages {
    entries: RwLock<HashMap<Uuid, LiveEntry>>,
}

impl LivePages {
    pub async fn get(&self, chapter_id: Uuid) -> Option<Vec<Url>> {
        let entries = self.entries.read().await;
        let entry = entries.get(&chapter_id)?;
        (entry.resolved_at.elapsed() < LIVE_PAGES_TTL).then(|| entry.urls.clone())
    }

    pub async fn put(&self, chapter_id: Uuid, urls: Vec<Url>) {
        let mut entries = self.entries.write().await;
        if entries.len() >= LIVE_PAGES_MAX && !entries.contains_key(&chapter_id) {
            // Evict the oldest entry; the map stays small enough to scan.
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, e)| e.resolved_at)
                .map(|(id, _)| *id)
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            chapter_id,
            LiveEntry {
                urls,
                resolved_at: Instant::now(),
            },
        );
    }

    pub async fn invalidate(&self, chapter_id: Uuid) {
        self.entries.write().await.remove(&chapter_id);
    }

    pub async fn invalidate_many(&self, chapter_ids: &[Uuid]) {
        let mut entries = self.entries.write().await;
        for id in chapter_ids {
            entries.remove(id);
        }
    }
}

/// Shared application state, cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Db,
    pub sources: Arc<Registry>,
    /// Wakes the download worker when chapters become pending.
    pub download_notify: Arc<Notify>,
    pub live_pages: Arc<LivePages>,
}

impl AppState {
    pub fn new(config: Config, db: Db, sources: Registry) -> Self {
        Self {
            config: Arc::new(config),
            db,
            sources: Arc::new(sources),
            download_notify: Arc::new(Notify::new()),
            live_pages: Arc::new(LivePages::default()),
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
