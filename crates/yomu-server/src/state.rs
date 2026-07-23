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
use crate::oidc::OidcRuntime;

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
/// downloaded, fails to serve, or its publication is deleted.
#[derive(Default)]
pub struct LivePages {
    entries: RwLock<HashMap<Uuid, LiveEntry>>,
}

impl LivePages {
    pub async fn get(&self, unit_id: Uuid) -> Option<Vec<Url>> {
        let entries = self.entries.read().await;
        let entry = entries.get(&unit_id)?;
        (entry.resolved_at.elapsed() < LIVE_PAGES_TTL).then(|| entry.urls.clone())
    }

    pub async fn put(&self, unit_id: Uuid, urls: Vec<Url>) {
        let mut entries = self.entries.write().await;
        if entries.len() >= LIVE_PAGES_MAX && !entries.contains_key(&unit_id) {
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
            unit_id,
            LiveEntry {
                urls,
                resolved_at: Instant::now(),
            },
        );
    }

    pub async fn invalidate(&self, unit_id: Uuid) {
        self.entries.write().await.remove(&unit_id);
    }

    pub async fn invalidate_many(&self, unit_ids: &[Uuid]) {
        let mut entries = self.entries.write().await;
        for id in unit_ids {
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
    /// Page progress of the chapter the single download worker is fetching
    /// (`None` when idle). In-memory and best-effort: lost on restart, which
    /// only drops a transient progress bar — the chapter re-queues normally.
    pub download_progress: Arc<RwLock<Option<ActiveDownload>>>,
    /// `Some` when `[auth]` configures an OIDC provider; `None` runs the
    /// single-account mode (everyone is the shared user).
    pub oidc: Option<Arc<OidcRuntime>>,
    /// Browse pages currently revalidating in the background.
    pub catalog_inflight: Arc<crate::catalog::Inflight>,
}

/// The chapter the download worker is fetching, and how far along.
#[derive(Clone, Copy)]
pub struct ActiveDownload {
    pub unit_id: Uuid,
    /// Pages written so far (1-based).
    pub page: u32,
    pub total: u32,
}

impl AppState {
    pub fn new(config: Config, db: Db, sources: Registry, oidc: Option<OidcRuntime>) -> Self {
        Self {
            config: Arc::new(config),
            db,
            sources: Arc::new(sources),
            download_notify: Arc::new(Notify::new()),
            live_pages: Arc::new(LivePages::default()),
            download_progress: Arc::new(RwLock::new(None)),
            oidc: oidc.map(Arc::new),
            catalog_inflight: Arc::new(crate::catalog::Inflight::default()),
        }
    }

    /// Directory holding a downloaded unit's page files.
    pub fn unit_dir(&self, publication_id: Uuid, unit_id: Uuid) -> PathBuf {
        self.config
            .data_dir
            .join(publication_id.to_string())
            .join(unit_id.to_string())
    }
}
