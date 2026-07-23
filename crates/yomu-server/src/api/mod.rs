//! HTTP API (`/api/v1`) and static frontend serving.

mod auth;
mod backup;
mod categories;
mod chapters;
mod downloads;
mod error;
mod library;
mod progress;
mod sources;
mod updates;

use axum::http::{HeaderValue, Method, header};
use axum::routing::get;
use axum::{Json, Router};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use yomu_domain::HealthResponse;

pub use error::ApiError;

use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/auth/me", get(auth::me))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", axum::routing::post(auth::logout))
        .route("/sources", get(sources::list))
        .route("/sources/{id}/search", get(sources::search))
        .route("/sources/{id}/browse", get(sources::browse))
        .route("/covers", get(sources::cover))
        .route("/search", get(sources::search_all))
        .route("/library", get(library::list).post(library::add))
        .route("/library/rescan", axum::routing::post(library::rescan))
        .route("/categories", get(categories::list))
        .route("/categories/{id}", axum::routing::put(categories::update))
        .route(
            "/manga/{id}",
            get(library::detail)
                .put(library::update)
                .delete(library::delete),
        )
        .route("/manga/{id}/refresh", axum::routing::post(library::refresh))
        .route("/manga/{id}/cover", get(library::cover))
        .route(
            "/manga/{id}/position",
            axum::routing::put(progress::set_position),
        )
        .route(
            "/chapters/{id}/download",
            axum::routing::post(chapters::download),
        )
        .route(
            "/chapters/download",
            axum::routing::post(chapters::download_many),
        )
        .route(
            "/chapters/remove-downloads",
            axum::routing::post(chapters::remove_downloads),
        )
        .route("/chapters/mark", axum::routing::post(chapters::mark))
        .route("/chapters/{id}/pages", get(chapters::pages))
        .route("/chapters/{id}/pages/{n}", get(chapters::page_image))
        .route(
            "/progress/events",
            get(progress::events).post(progress::push_events),
        )
        .route("/backup", get(backup::export))
        .route("/restore", axum::routing::post(backup::restore))
        .route("/updates", get(updates::list))
        .route("/downloads", get(downloads::list))
        .route("/downloads/retry", axum::routing::post(downloads::retry))
        .route(
            "/downloads/dismiss",
            axum::routing::post(downloads::dismiss),
        )
        .with_state(state.clone());

    let mut app = Router::new().nest("/api/v1", api);

    if let Some(dir) = &state.config.static_dir {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)));
    }

    app.layer(cors_layer(&state.config.auth.allowed_origins))
        .layer(TraceLayer::new_for_http())
}

/// CORS policy. Default (no `allowed_origins`) is permissive: any origin,
/// no credentials. That's what the native shells (Android/desktop pointed at
/// a LAN server) and a separately-hosted web frontend rely on, and it's safe
/// now that every mutating route requires a session — a credentialed request
/// may not use a wildcard `Access-Control-Allow-Origin`, so `*` cannot ride a
/// user's cookie. Set `allowed_origins` to switch to a credentialed allowlist
/// (for a cross-origin frontend that authenticates with cookies). An invalid
/// origin string is dropped with a warning rather than failing boot.
fn cors_layer(allowed_origins: &[String]) -> CorsLayer {
    let origins: Vec<HeaderValue> = allowed_origins
        .iter()
        .filter_map(|o| match o.trim_end_matches('/').parse::<HeaderValue>() {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!(origin = %o, "ignoring unparseable allowed_origin");
                None
            }
        })
        .collect();
    if origins.is_empty() {
        return CorsLayer::permissive();
    }
    CorsLayer::new()
        .allow_credentials(true)
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        commit: option_env!("YOMU_BUILD_COMMIT").map(Into::into),
    })
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    use yomu_source::registry::Registry;

    use crate::config::Config;
    use crate::db::Db;
    use crate::state::AppState;

    /// Router in OIDC mode: `oidc_enabled()` is true, but no session is
    /// presented, so `CurrentUser`-gated routes must reject.
    async fn oidc_router() -> axum::Router {
        let mut config = Config::default();
        config.auth.issuer = Some("https://auth.example.test/".parse().unwrap());
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);
        super::router(state)
    }

    async fn status_of(method: &str, path: &str) -> StatusCode {
        let router = oidc_router().await;
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        router.oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn mutating_routes_require_a_session_in_oidc_mode() {
        // Every write must reject an anonymous request with 401, not act on it.
        assert_eq!(
            status_of("POST", "/api/v1/library").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_of("POST", "/api/v1/chapters/download").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_of("POST", "/api/v1/chapters/remove-downloads").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_of("POST", "/api/v1/downloads/retry").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_of("POST", "/api/v1/downloads/dismiss").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_of("POST", "/api/v1/library/rescan").await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn rescan_rejects_when_books_folder_is_disabled() {
        // The background scan loop gates on `books.enabled`; the manual
        // endpoint must too, or it would scan the default dir anyway.
        let mut config = Config::default();
        config.books.enabled = false;
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/library/rescan")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = super::router(state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn health_stays_open() {
        assert_eq!(status_of("GET", "/api/v1/health").await, StatusCode::OK);
    }

    #[tokio::test]
    async fn default_cors_is_permissive_for_cross_origin_clients() {
        // No allowed_origins configured (the default): a cross-origin request
        // — the native shells and PWAs pointed at a LAN server — must get a
        // permissive `Access-Control-Allow-Origin`, or their fetches are
        // blocked. This is the 1.8.0 → 1.8.1 regression guard.
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(Config::default(), db, Registry::default(), None);
        let router = super::router(state);
        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/health")
            .header("origin", "https://tauri.localhost")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let acao = resp
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok());
        assert_eq!(acao, Some("*"), "default CORS must allow any origin");
    }
}
