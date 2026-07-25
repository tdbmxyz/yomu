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
mod static_cache;
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
        // Restore takes a whole library export, which outgrows axum's 2 MB
        // default body limit at a few thousand chapters — the point where
        // a backup is worth having. The cap stays finite (an unbounded
        // body would be buffered into memory) but generous.
        .route(
            "/restore",
            axum::routing::post(backup::restore)
                .layer(axum::extract::DefaultBodyLimit::max(RESTORE_BODY_LIMIT)),
        )
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
        // Siblings are generated once at build time (see yomu-web-compressed
        // in flake.nix); ServeDir picks one by Accept-Encoding and falls back
        // to the identity file when none exists, so a plain local dist works
        // unchanged.
        let index = ServeFile::new(index)
            .precompressed_br()
            .precompressed_gzip();
        let files = ServeDir::new(dir)
            .precompressed_br()
            .precompressed_gzip()
            .fallback(index);
        // The cache layer goes on a router holding nothing but the static
        // service, which is then merged in: on the app itself it would stamp
        // cache headers on API responses too.
        let files = Router::new()
            .fallback_service(files)
            .layer(axum::middleware::from_fn(static_cache::cache_headers));
        app = app.merge(files);
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

/// Ceiling for an uploaded backup. Roughly 100× a 3k-chapter export, so it
/// is a runaway guard rather than a limit any real library meets.
const RESTORE_BODY_LIMIT: usize = 256 * 1024 * 1024;

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

    /// A fixture directory no other run can collide with. `tempfile` is
    /// deliberately not a dependency, and a fixed /tmp name is worse than no
    /// cleanup: every one of these tests starts by deleting the directory, so
    /// two concurrent runs (two worktrees, a CI matrix) delete each other's
    /// files mid-test.
    fn fixture_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("yomu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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

    /// A real library exports well past axum's 2 MB default body limit (a
    /// 3k-chapter library is ~2.3 MB), and restore is the *only* way that
    /// file is ever used — so the limit made the feature useless exactly
    /// where it matters. The body here is deliberately not valid JSON: what
    /// matters is that it was read to the end (a JSON error) instead of
    /// being cut off at 2 MB (413 Payload Too Large).
    #[tokio::test]
    async fn restore_accepts_a_backup_larger_than_the_default_body_limit() {
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(Config::default(), db, Registry::default(), None);
        let oversized = "x".repeat(4 * 1024 * 1024);
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/restore")
            .header("content-type", "application/json")
            .body(Body::from(oversized))
            .unwrap();
        let status = super::router(state).oneshot(req).await.unwrap().status();
        assert_ne!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "a full-size backup must still reach the handler"
        );
        assert_eq!(status, StatusCode::BAD_REQUEST);
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

    /// A dist with a precompressed sibling: the server must hand the sibling
    /// to a client that accepts brotli, and the plain file to one that does
    /// not. Without this the wasm ships uncompressed to every visitor.
    #[tokio::test]
    async fn static_files_prefer_a_precompressed_sibling() {
        let dir = fixture_dir("precompressed-test");
        std::fs::write(dir.join("app.wasm"), b"plain-wasm-bytes").unwrap();
        std::fs::write(dir.join("app.wasm.br"), b"brotli-wasm-bytes").unwrap();
        std::fs::write(dir.join("index.html"), b"<html></html>").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);
        let router = super::router(state);

        let req = Request::builder()
            .uri("/app.wasm")
            .header("accept-encoding", "br")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
            Some("br")
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"brotli-wasm-bytes");

        let req = Request::builder()
            .uri("/app.wasm")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert!(resp.headers().get("content-encoding").is_none());
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"plain-wasm-bytes");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The cache layer must sit on the static service and nowhere else: a
    /// fingerprinted asset is pinned for a year, `index.html` (which changes
    /// in place under the same URL) revalidates, and an API response carries
    /// no `cache-control` at all — the guard against layering it app-wide,
    /// which would let a stale library listing be served from a cache.
    #[tokio::test]
    async fn static_assets_carry_cache_control_and_api_responses_do_not() {
        let dir = fixture_dir("cache-control-test");
        std::fs::write(dir.join("yomu-web-9da5a24d4d3677cc_bg.wasm"), b"w").unwrap();
        std::fs::write(dir.join("index.html"), b"<html></html>").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);
        let router = super::router(state);

        let header_of = |resp: &axum::response::Response, name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let get = |uri: &'static str| {
            let router = router.clone();
            async move {
                let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
                router.oneshot(req).await.unwrap()
            }
        };

        let resp = get("/yomu-web-9da5a24d4d3677cc_bg.wasm").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            header_of(&resp, "cache-control").as_deref(),
            Some(super::static_cache::IMMUTABLE)
        );
        // ServeDir picks a body by Accept-Encoding but sets no Vary, so a
        // shared cache would otherwise reuse a brotli body for a client that
        // never asked for one — for a year, given `immutable`. (The CORS
        // layer contributes its own Vary values, hence the scan.)
        assert!(
            resp.headers()
                .get_all("vary")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .any(|v| v.contains("accept-encoding")),
            "static responses must vary on accept-encoding"
        );

        let resp = get("/index.html").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            header_of(&resp, "cache-control").as_deref(),
            Some(super::static_cache::REVALIDATE)
        );

        let resp = get("/api/v1/health").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            header_of(&resp, "cache-control"),
            None,
            "the cache layer must not reach API responses"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A local `trunk build` dist has no siblings at all; it must still serve.
    /// This is what keeps `just web` and a hand-built dist working.
    #[tokio::test]
    async fn static_files_fall_back_to_identity_without_a_sibling() {
        let dir = fixture_dir("no-sibling-test");
        std::fs::write(dir.join("app.js"), b"console.log(1)").unwrap();
        std::fs::write(dir.join("index.html"), b"<html></html>").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);

        let req = Request::builder()
            .uri("/app.js")
            .header("accept-encoding", "br, gzip")
            .body(Body::empty())
            .unwrap();
        let resp = super::router(state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get("content-encoding").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The SPA fallback is a separate `ServeFile`, so it needs its own
    /// `precompressed_*` — and it is the hottest path there is: every deep
    /// link and every cold navigation is served by it. Dropping the flag from
    /// the fallback leaves the per-file test above green, so this pins it.
    #[tokio::test]
    async fn the_spa_fallback_prefers_a_precompressed_index() {
        let dir = fixture_dir("spa-precompressed-test");
        std::fs::write(dir.join("index.html"), b"<html>plain</html>").unwrap();
        std::fs::write(dir.join("index.html.br"), b"<html>brotli</html>").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);

        // /library/42 matches no file and no API route: it falls through to
        // the index, the way a bookmarked reader URL does.
        let req = Request::builder()
            .uri("/library/42")
            .header("accept-encoding", "br")
            .body(Body::empty())
            .unwrap();
        let resp = super::router(state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
            Some("br"),
            "the SPA fallback must serve index.html.br"
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"<html>brotli</html>");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half: a hand-built `trunk build` dist has no `index.html.br`,
    /// and a deep link must still render rather than 404.
    #[tokio::test]
    async fn the_spa_fallback_serves_identity_without_a_sibling() {
        let dir = fixture_dir("spa-no-sibling-test");
        std::fs::write(dir.join("index.html"), b"<html>plain</html>").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);

        let req = Request::builder()
            .uri("/library/42")
            .header("accept-encoding", "br, gzip")
            .body(Body::empty())
            .unwrap();
        let resp = super::router(state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get("content-encoding").is_none());
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"<html>plain</html>");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The unclearable cache poisoning: `ServeDir` answers a miss with the SPA
    /// shell, so a request for a hashed asset that no longer exists returns
    /// 200 `text/html`. Stamped `immutable` that pins HTML under an asset URL
    /// for a year — and if that exact build is ever served again (a rollback,
    /// or a reverted frontend change reproducing the same trunk hash) the app
    /// fetches wasm, gets HTML, and never boots. `sw.js` makes it durable: its
    /// `refreshShell` sees `asset.ok` and caches the HTML under the asset URL.
    /// Nothing but a manual cache clear escapes.
    #[tokio::test]
    async fn a_missing_hashed_asset_falls_back_to_the_shell_and_is_not_immutable() {
        let dir = fixture_dir("missing-asset-test");
        std::fs::write(dir.join("index.html"), b"<html>SHELL</html>").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);

        let req = Request::builder()
            .uri("/yomu-web-0123456789abcdef_bg.wasm")
            .body(Body::empty())
            .unwrap();
        let resp = super::router(state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some(super::static_cache::REVALIDATE),
            "the SPA shell must never be pinned under an asset URL"
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"<html>SHELL</html>");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The second reachable path to the same bug: a source id may be 16 hex
    /// characters (`registry.rs` allows alphanumeric plus `-`/`_`), so an SPA
    /// route is indistinguishable from an asset by URL alone. `/manga/:id` and
    /// `/read/:m/:c` escape only because uuid groups are 8/4/4/4/12 — a
    /// convention living nowhere near the classifier. Asserted on served
    /// responses, not on classifier strings.
    #[tokio::test]
    async fn spa_routes_revalidate_even_when_they_look_fingerprinted() {
        let dir = fixture_dir("spa-cache-control-test");
        std::fs::write(dir.join("index.html"), b"<html>SHELL</html>").unwrap();
        std::fs::write(dir.join("styles-dcb9e8dca193296c.css"), b"body{}").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);
        let router = super::router(state);

        let cache_control_of = |uri: &'static str| {
            let router = router.clone();
            async move {
                let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
                let resp = router.oneshot(req).await.unwrap();
                assert_eq!(resp.status(), StatusCode::OK, "{uri}");
                resp.headers()
                    .get("cache-control")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            }
        };

        for route in ["/sources/deadbeefdeadbeef", "/library"] {
            assert_eq!(
                cache_control_of(route).await.as_deref(),
                Some(super::static_cache::REVALIDATE),
                "{route}"
            );
        }

        // The positive must survive the fix: a hashed asset that actually
        // exists is still pinned, which is the whole point of the feature.
        assert_eq!(
            cache_control_of("/styles-dcb9e8dca193296c.css")
                .await
                .as_deref(),
            Some(super::static_cache::IMMUTABLE)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `Vary` has two independent contributors: the cache layer adds
    /// `accept-encoding`, the CORS layer adds the origin/preflight triple.
    /// Setting either with `insert` erases the other, and which one loses
    /// depends only on layer order — so both must be present on one response.
    #[tokio::test]
    async fn static_responses_vary_on_both_encoding_and_cors_headers() {
        let dir = fixture_dir("vary-test");
        std::fs::write(dir.join("index.html"), b"<html></html>").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);

        let req = Request::builder()
            .uri("/index.html")
            .header("origin", "https://tauri.localhost")
            .body(Body::empty())
            .unwrap();
        let resp = super::router(state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let vary: String = resp
            .headers()
            .get_all("vary")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>()
            .join(", ")
            .to_ascii_lowercase();
        for expected in [
            "accept-encoding",
            "origin",
            "access-control-request-method",
            "access-control-request-headers",
        ] {
            assert!(
                vary.contains(expected),
                "vary `{vary}` is missing {expected}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
