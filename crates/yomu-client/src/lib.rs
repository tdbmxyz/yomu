//! Typed client for the yomu HTTP API. Compiles on native and wasm; all UI
//! crates (and the future offline store) go through this client.

use url::Url;
use uuid::Uuid;
use yomu_domain::{
    AddMangaRequest, ApiErrorBody, Backup, BrowseSort, BulkChaptersResponse, Category, Chapter,
    DownloadChaptersRequest, EventsResponse, HealthResponse, Manga, MangaDetailResponse,
    MangaSummary, MangaWithPosition, MarkChaptersRequest, MeResponse, PagesResponse, Position,
    PushEventsRequest, PushEventsResponse, RefreshResponse, RestoreSummary, SetPositionRequest,
    SourceInfo, SourceSearchResults, UpdateCategoryRequest, UpdateMangaRequest,
};

#[derive(Debug, Clone, thiserror::Error)]
pub enum ClientError {
    #[error("request failed: {0}")]
    Transport(String),
    #[error("server returned {status}: {message}")]
    Api { status: u16, message: String },
    #[error("invalid response body: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// On wasm, `fetch` defaults to same-origin credentials, so a frontend
/// pointed at a *different* host than the API (remote `yomu-api-base`)
/// never sends the session cookie and every authenticated call 401s.
/// Force-include credentials. No-op on native (reqwest carries its own
/// cookie handling there).
fn with_credentials(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    #[cfg(target_arch = "wasm32")]
    {
        req.fetch_credentials_include()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        req
    }
}

#[derive(Debug, Clone)]
pub struct YomuClient {
    base: Url,
    http: reqwest::Client,
}

impl YomuClient {
    /// `base` is the server origin (no `/api/v1` suffix). A non-root path
    /// (subpath deployment) is kept, but needs a trailing slash for
    /// `Url::join` not to drop its last segment — normalize here so callers
    /// can't get it subtly wrong.
    pub fn new(mut base: Url) -> Self {
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        Self {
            base,
            http: reqwest::Client::new(),
        }
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    pub async fn health(&self) -> Result<HealthResponse> {
        self.get("api/v1/health").await
    }

    // ---- auth ----

    /// Auth mode + current user (never a 401; `user` is `None` signed out).
    /// Sign-in itself is a browser redirect to `<base>/api/v1/auth/login`.
    pub async fn me(&self) -> Result<MeResponse> {
        self.get("api/v1/auth/me").await
    }

    pub async fn logout(&self) -> Result<()> {
        let req = self.http.post(self.url("api/v1/auth/logout")?);
        self.send_no_content(req).await
    }

    // ---- sources ----

    pub async fn sources(&self) -> Result<Vec<SourceInfo>> {
        self.get("api/v1/sources").await
    }

    pub async fn search(&self, source_id: &str, query: &str) -> Result<Vec<MangaSummary>> {
        let req = self
            .http
            .get(self.url(&format!("api/v1/sources/{source_id}/search"))?)
            .query(&[("q", query)]);
        self.send(req).await
    }

    /// One query against every configured source; one entry per source.
    pub async fn search_all(&self, query: &str) -> Result<Vec<SourceSearchResults>> {
        let req = self
            .http
            .get(self.url("api/v1/search")?)
            .query(&[("q", query)]);
        self.send(req).await
    }

    /// A source's catalog listing (`sort` = popular/latest), 1-based pages.
    pub async fn browse(
        &self,
        source_id: &str,
        sort: BrowseSort,
        page: u32,
    ) -> Result<Vec<MangaSummary>> {
        let req = self
            .http
            .get(self.url(&format!("api/v1/sources/{source_id}/browse"))?)
            .query(&[("sort", sort.key()), ("page", &page.to_string())]);
        self.send(req).await
    }

    // ---- library ----

    pub async fn add_manga(&self, req: &AddMangaRequest) -> Result<Manga> {
        let req = self.http.post(self.url("api/v1/library")?).json(req);
        self.send(req).await
    }

    pub async fn library(&self) -> Result<Vec<MangaWithPosition>> {
        self.get("api/v1/library").await
    }

    pub async fn manga(&self, id: Uuid) -> Result<MangaDetailResponse> {
        self.get(&format!("api/v1/manga/{id}")).await
    }

    pub async fn update_manga(&self, id: Uuid, req: &UpdateMangaRequest) -> Result<Manga> {
        let req = self
            .http
            .put(self.url(&format!("api/v1/manga/{id}"))?)
            .json(req);
        self.send(req).await
    }

    pub async fn delete_manga(&self, id: Uuid) -> Result<()> {
        let req = self.http.delete(self.url(&format!("api/v1/manga/{id}"))?);
        self.send_no_content(req).await
    }

    pub async fn refresh_manga(&self, id: Uuid) -> Result<RefreshResponse> {
        let req = self
            .http
            .post(self.url(&format!("api/v1/manga/{id}/refresh"))?);
        self.send(req).await
    }

    /// Server-cached cover image URL (for `<img src>`).
    pub fn cover_url(&self, id: Uuid) -> Option<Url> {
        self.base.join(&format!("api/v1/manga/{id}/cover")).ok()
    }

    // ---- backup / restore ----

    pub async fn backup(&self) -> Result<Backup> {
        self.get("api/v1/backup").await
    }

    pub async fn restore(&self, backup: &Backup) -> Result<RestoreSummary> {
        let req = self.http.post(self.url("api/v1/restore")?).json(backup);
        self.send(req).await
    }

    // ---- categories ----

    pub async fn categories(&self) -> Result<Vec<Category>> {
        self.get("api/v1/categories").await
    }

    pub async fn update_category(&self, id: &str, req: &UpdateCategoryRequest) -> Result<Category> {
        let req = self
            .http
            .put(self.url(&format!("api/v1/categories/{id}"))?)
            .json(req);
        self.send(req).await
    }

    // ---- chapters & pages ----

    pub async fn download_chapter(&self, id: Uuid) -> Result<Chapter> {
        let req = self
            .http
            .post(self.url(&format!("api/v1/chapters/{id}/download"))?);
        self.send(req).await
    }

    /// Queue several chapters; the server's single download worker drains
    /// them with the source's politeness delay.
    pub async fn download_chapters(&self, ids: &[Uuid]) -> Result<BulkChaptersResponse> {
        let req =
            self.http
                .post(self.url("api/v1/chapters/download")?)
                .json(&DownloadChaptersRequest {
                    chapter_ids: ids.to_vec(),
                });
        self.send(req).await
    }

    /// Remove the server copies of these chapters.
    pub async fn remove_downloads(&self, ids: &[Uuid]) -> Result<BulkChaptersResponse> {
        let req = self
            .http
            .post(self.url("api/v1/chapters/remove-downloads")?)
            .json(&DownloadChaptersRequest {
                chapter_ids: ids.to_vec(),
            });
        self.send(req).await
    }

    pub async fn mark_chapters(&self, ids: &[Uuid], read: bool) -> Result<BulkChaptersResponse> {
        let req = self
            .http
            .post(self.url("api/v1/chapters/mark")?)
            .json(&MarkChaptersRequest {
                chapter_ids: ids.to_vec(),
                read,
            });
        self.send(req).await
    }

    pub async fn chapter_pages(&self, id: Uuid) -> Result<PagesResponse> {
        self.get(&format!("api/v1/chapters/{id}/pages")).await
    }

    /// Image URL of one page (for `<img src>`); served from disk or proxied
    /// live by the server.
    pub fn page_url(&self, chapter_id: Uuid, page: u32) -> Option<Url> {
        self.base
            .join(&format!("api/v1/chapters/{chapter_id}/pages/{page}"))
            .ok()
    }

    /// Fetch a page image and discard the body — used to warm caches
    /// (browser service worker) for offline reading.
    pub async fn fetch_page(&self, chapter_id: Uuid, page: u32) -> Result<()> {
        let url = self
            .page_url(chapter_id, page)
            .ok_or_else(|| ClientError::Transport("invalid page url".into()))?;
        Self::check_status(self.http.get(url)).await.map(|_| ())
    }

    // ---- progress ----

    pub async fn set_position(&self, manga_id: Uuid, req: &SetPositionRequest) -> Result<Position> {
        let req = self
            .http
            .put(self.url(&format!("api/v1/manga/{manga_id}/position"))?)
            .json(req);
        self.send(req).await
    }

    /// Journal sync for offline clients. Events for manga the server no
    /// longer knows are consumed (`skipped`), not errors — see
    /// [`PushEventsResponse`].
    pub async fn push_events(&self, req: &PushEventsRequest) -> Result<PushEventsResponse> {
        let req = self
            .http
            .post(self.url("api/v1/progress/events")?)
            .json(req);
        self.send(req).await
    }

    /// `since` is the `next_since` cursor of the previous page (server
    /// arrival order), not an event id.
    pub async fn events_since(&self, since: Option<i64>) -> Result<EventsResponse> {
        let mut req = self.http.get(self.url("api/v1/progress/events")?);
        if let Some(since) = since {
            req = req.query(&[("since", since.to_string())]);
        }
        self.send(req).await
    }

    // ---- plumbing ----

    fn url(&self, path: &str) -> Result<Url> {
        self.base
            .join(path)
            .map_err(|e| ClientError::Transport(e.to_string()))
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(self.http.get(self.url(path)?)).await
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<T> {
        let resp = Self::check_status(req).await?;
        resp.json::<T>()
            .await
            .map_err(|e| ClientError::Decode(e.to_string()))
    }

    async fn send_no_content(&self, req: reqwest::RequestBuilder) -> Result<()> {
        Self::check_status(req).await.map(|_| ())
    }

    async fn check_status(req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let resp = with_credentials(req)
            .send()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let message = match resp.text().await {
                Ok(body) => serde_json::from_str::<ApiErrorBody>(&body)
                    .map(|b| b.message)
                    .unwrap_or(body),
                Err(_) => String::from("<no body>"),
            };
            return Err(ClientError::Api {
                status: status.as_u16(),
                message,
            });
        }
        Ok(resp)
    }
}
