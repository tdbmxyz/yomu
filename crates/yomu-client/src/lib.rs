//! Typed client for the yomu HTTP API. Compiles on native and wasm; all UI
//! crates (and the future offline store) go through this client.

use url::Url;
use uuid::Uuid;
use yomu_domain::{
    AddMangaRequest, ApiErrorBody, Chapter, EventsResponse, HealthResponse, Manga,
    MangaDetailResponse, MangaSummary, MangaWithPosition, PagesResponse, Position,
    PushEventsRequest, RefreshResponse, SetPositionRequest, SourceInfo, UpdateMangaRequest,
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

#[derive(Debug, Clone)]
pub struct YomuClient {
    base: Url,
    http: reqwest::Client,
}

impl YomuClient {
    /// `base` is the server origin (no `/api/v1` suffix).
    pub fn new(base: Url) -> Self {
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

    // ---- chapters & pages ----

    pub async fn download_chapter(&self, id: Uuid) -> Result<Chapter> {
        let req = self
            .http
            .post(self.url(&format!("api/v1/chapters/{id}/download"))?);
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

    /// Journal sync for offline clients.
    pub async fn push_events(&self, req: &PushEventsRequest) -> Result<()> {
        let req = self
            .http
            .post(self.url("api/v1/progress/events")?)
            .json(req);
        self.send_no_content(req).await
    }

    pub async fn events_since(&self, since: Option<Uuid>) -> Result<EventsResponse> {
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
        let resp = req
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
