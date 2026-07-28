//! HTTP API (`/api/v1`) and static frontend serving.

mod auth;
mod backup;
mod categories;
mod chapters;
mod downloads;
mod error;
mod fingerprints;
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
        .route("/manga/{id}/fingerprints", get(fingerprints::list))
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
        .with_state(state.clone())
        // Anything under /api/v1 this build does not serve is an error, not a
        // page. Without this it reaches the SPA fallback below and comes back
        // as 200 index.html, so a client calling a route older than itself
        // gets a JSON decode error rather than a plain 404, and an API typo
        // answers HTML.
        .fallback(api_not_found);

    let mut app = Router::new()
        .nest("/api/v1", api)
        // The same for prefixes the nest never sees: a future /api/v2, or
        // /api/health from a client that dropped the version.
        .route("/api", axum::routing::any(api_not_found))
        .route("/api/{*rest}", axum::routing::any(api_not_found));

    if let Some(dir) = &state.config.static_dir {
        let index = dir.join("index.html");
        // Siblings are generated once at build time (see yomu-web-compressed
        // in flake.nix); ServeDir picks one by Accept-Encoding and falls back
        // to the identity file when none exists, so a plain local dist works
        // unchanged.
        let index = ServeFile::new(index)
            .precompressed_br()
            .precompressed_gzip();
        // The SPA shell answers every URL that names no file, including a
        // hashed asset URL from a previous deploy — and it changes in place,
        // so pinning it under such a URL is a cache poisoning the user cannot
        // clear. That guarantee is attached to the *service*, not inferred
        // afterwards from a header: a 304 carries no content-type, so no
        // amount of inspecting the response can recognise the shell, and a
        // 304's headers overwrite the stored ones (RFC 9111 §4.3.4). Every
        // response this service produces — 200, 304, 206, compressed or not —
        // leaves here already marked revalidate-always, and the outer
        // `cache_headers` fills in `Cache-Control` only where it is absent.
        let shell = Router::new()
            .fallback_service(index)
            .layer(axum::middleware::from_fn(static_cache::shell_cache_headers));
        let files = ServeDir::new(dir)
            .precompressed_br()
            .precompressed_gzip()
            .fallback(shell);
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

/// The answer for every unrouted `/api` path: a 404 in the same JSON shape
/// every other API error uses, so a client can read it.
async fn api_not_found() -> ApiError {
    ApiError::NotFound
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

    /// The same poisoning one round trip later, which the content-type check
    /// could never see. `must-revalidate` is *itself* the instruction that
    /// makes the browser re-ask conditionally, so the shell-under-an-asset-URL
    /// response always comes back as a `304` on its second use — and a 304
    /// carries no `content-type`, so "is the body HTML?" cannot be asked of
    /// it. RFC 9111 §4.3.4 has the client fold a 304's headers into its stored
    /// response, so an `immutable` here rewrites the stored HTML to immutable
    /// for a year: identical unclearable failure, arrived at through the fix's
    /// own header. Both requests are made, because the second only exists
    /// because of the first.
    #[tokio::test]
    async fn a_revalidated_shell_under_an_asset_url_is_still_not_immutable() {
        let dir = fixture_dir("shell-304-test");
        std::fs::write(dir.join("index.html"), b"<html>SHELL</html>").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);
        let router = super::router(state);

        let asset = "/yomu-web-0123456789abcdef_bg.wasm";
        let req = Request::builder().uri(asset).body(Body::empty()).unwrap();
        let first = router.clone().oneshot(req).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(
            first
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some(super::static_cache::REVALIDATE)
        );
        let last_modified = first
            .headers()
            .get("last-modified")
            .expect("the shell must carry a validator, or no 304 is possible")
            .to_str()
            .unwrap()
            .to_owned();

        // Exactly what a browser sends next, having been told to revalidate.
        let req = Request::builder()
            .uri(asset)
            .header("if-modified-since", &last_modified)
            .body(Body::empty())
            .unwrap();
        let second = router.oneshot(req).await.unwrap();
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            second
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some(super::static_cache::REVALIDATE),
            "a 304 rewrites the stored response's headers: `immutable` here \
             pins the shell under an asset URL just as surely as on the 200"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other side of the same coin, so the fix is not an over-correction:
    /// a hashed asset that really exists must stay `immutable` on its own 304.
    /// Losing that makes every reload pay a round trip for a file that can
    /// never change.
    #[tokio::test]
    async fn a_real_assets_own_304_stays_immutable() {
        let dir = fixture_dir("asset-304-test");
        std::fs::write(dir.join("index.html"), b"<html>SHELL</html>").unwrap();
        std::fs::write(dir.join("styles-dcb9e8dca193296c.css"), b"body{}").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);
        let router = super::router(state);

        let asset = "/styles-dcb9e8dca193296c.css";
        let req = Request::builder().uri(asset).body(Body::empty()).unwrap();
        let first = router.clone().oneshot(req).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let last_modified = first
            .headers()
            .get("last-modified")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();

        let req = Request::builder()
            .uri(asset)
            .header("if-modified-since", &last_modified)
            .body(Body::empty())
            .unwrap();
        let second = router.oneshot(req).await.unwrap();
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            second
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some(super::static_cache::IMMUTABLE)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Trunk drops leading zeros from the hex fingerprint, so its length
    /// varies — this exact filename came off a real build of this repo and is
    /// 14 characters. A classifier demanding 16 rejected it and shipped the
    /// 1.45 MB wasm with `must-revalidate`, so the whole feature was a no-op
    /// for that build and no test noticed, every fixture being 16 characters.
    #[tokio::test]
    async fn a_short_fingerprint_is_still_pinned() {
        let dir = fixture_dir("short-fingerprint-test");
        std::fs::write(dir.join("index.html"), b"<html>SHELL</html>").unwrap();
        std::fs::write(dir.join("yomu-web-ae4beb7cab1d74_bg.wasm"), b"w").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);

        let req = Request::builder()
            .uri("/yomu-web-ae4beb7cab1d74_bg.wasm")
            .body(Body::empty())
            .unwrap();
        let resp = super::router(state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some(super::static_cache::IMMUTABLE),
            "a short trunk hash is a hash; rejecting it silently disables \
             caching for the single largest asset we ship"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The second reachable path to the same bug: a source id may be hex
    /// (`registry.rs` allows alphanumeric plus `-`/`_`) and a uuid's groups
    /// are hex too, so an SPA route is indistinguishable from an asset by URL
    /// alone — the classifier reads both as fingerprinted and is meant to.
    /// What keeps them out of `immutable` is that they are served by the shell
    /// service, which marks its own responses. Asserted on served responses,
    /// not on classifier strings, because the classifier is not where the
    /// guarantee lives.
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

        for route in [
            "/sources/deadbeefdeadbeef",
            // A uuid's groups are 8/4/4/4/12 hex and the classifier now
            // accepts runs from 8 up, so this reads as an asset. Harmless:
            // it is not a file, so the shell answers it.
            "/manga/019f4921-3946-7c20-9a67-d84d46072fe6",
            "/library",
        ] {
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

    /// The rest of what a browser and a CDN actually send, on one dist, so a
    /// later change to the cache layer cannot quietly break one of them: a
    /// HEAD (what a cache issues to freshen metadata), a Range request (what a
    /// resumed download and some wasm loaders send), a gzip-only client, and a
    /// deep link into a dist with no `index.html` at all — the last must 404
    /// rather than pin anything.
    #[tokio::test]
    async fn the_cache_layer_holds_across_head_range_and_gzip() {
        let dir = fixture_dir("cache-attack-surface-test");
        std::fs::write(dir.join("index.html"), b"<html>SHELL</html>").unwrap();
        std::fs::write(dir.join("styles-dcb9e8dca193296c.css"), b"body{color:red}").unwrap();
        std::fs::write(dir.join("styles-dcb9e8dca193296c.css.gz"), b"gzipped").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);
        let router = super::router(state);

        let asset = "/styles-dcb9e8dca193296c.css";
        let cache_control = |resp: &axum::response::Response| {
            resp.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };

        let req = Request::builder()
            .method("HEAD")
            .uri(asset)
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            cache_control(&resp).as_deref(),
            Some(super::static_cache::IMMUTABLE)
        );

        let req = Request::builder()
            .uri(asset)
            .header("range", "bytes=0-3")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            cache_control(&resp).as_deref(),
            Some(super::static_cache::IMMUTABLE),
            "a 206 is a slice of an immutable body, not a different body"
        );

        let req = Request::builder()
            .uri(asset)
            .header("accept-encoding", "gzip")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        assert_eq!(
            cache_control(&resp).as_deref(),
            Some(super::static_cache::IMMUTABLE)
        );

        // The errors `ServeDir` answers itself, without consulting the
        // fallback — so the shell service never sees them and the status
        // guard is the only thing standing between them and a year-long pin
        // on a failure. An unsatisfiable range is what a resumed download
        // sends against a file that shrank; a method it does not implement is
        // what a probe or a misrouted client sends.
        let req = Request::builder()
            .uri(asset)
            .header("range", "bytes=9999-")
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            cache_control(&resp).as_deref(),
            Some(super::static_cache::REVALIDATE),
            "pinning an error answer makes it permanent for that client"
        );

        let req = Request::builder()
            .method("POST")
            .uri(asset)
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            cache_control(&resp).as_deref(),
            Some(super::static_cache::REVALIDATE)
        );

        let _ = std::fs::remove_dir_all(&dir);

        // A dist with no index.html: the fallback has nothing to serve, and a
        // 404 must never be pinned — the URL may well exist in the next
        // deploy.
        let empty = fixture_dir("cache-no-index-test");
        let config = Config {
            static_dir: Some(empty.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);
        let req = Request::builder()
            .uri("/yomu-web-0123456789abcdef_bg.wasm")
            .body(Body::empty())
            .unwrap();
        let resp = super::router(state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            cache_control(&resp).as_deref(),
            Some(super::static_cache::REVALIDATE)
        );

        let _ = std::fs::remove_dir_all(&empty);
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

    /// The recovery this endpoint exists for matches a device directory
    /// against the server by content, so the fingerprint must come from the
    /// bytes actually on disk — and a unit the server never downloaded has no
    /// bytes to match, so it must not appear at all rather than appear with a
    /// hash of nothing.
    ///
    /// Both units get a full page directory, so the *only* thing that can keep
    /// the second one out of the answer is its download state. (With pages on
    /// disk for the downloaded unit alone, deleting the state check left the
    /// test green: the missing-directory guard was silently doing its work.)
    /// The downloaded unit gets more files than its recorded `page_count`, and
    /// more than two of them, so a count read from the database and a listing
    /// left unsorted are both visible from the response.
    #[tokio::test]
    async fn fingerprints_describe_downloaded_units_and_omit_the_rest() {
        use sha2::{Digest, Sha256};
        use yomu_domain::{ChapterRef, MangaDetails, MangaSummary};

        let dir = fixture_dir("fingerprints-test");
        let details = MangaDetails {
            summary: MangaSummary {
                key: "m1".into(),
                title: "Publication m1".into(),
                cover_url: None,
                in_library: None,
            },
            description: None,
            genres: Vec::new(),
            chapters: ["c1", "c2"]
                .iter()
                .enumerate()
                .map(|(i, key)| ChapterRef {
                    key: (*key).into(),
                    title: format!("Chapter {key}"),
                    number: Some(i as f64 + 1.0),
                    source_order: i as u32,
                    scanlator: None,
                    published_at: None,
                })
                .collect(),
        };

        let db = Db::in_memory().await.unwrap();
        let publication = db
            .insert_publication("fixture", &details, false)
            .await
            .unwrap();
        let units = db.list_units(publication.id).await.unwrap();
        // Only the first is downloaded; the second stays untouched.
        db.finish_download(units[0].id, Ok(2)).await.unwrap();

        let config = Config {
            data_dir: dir.clone(),
            ..Config::default()
        };
        let state = AppState::new(config, db, Registry::default(), None);

        // Written newest-name-first: on a filesystem that hands back entries
        // in creation order, an unsorted read starts at the last page, so the
        // page-0 hash below catches it outright. Elsewhere the order is a hash
        // of the names and eight pages make an accidental match unlikely.
        let pages: [(&str, &[u8]); 8] = [
            ("0007.jpg", b"page-seven-bytes"),
            ("0006.gif", b"page-six-bytes"),
            ("0005.avif", b"page-five-bytes"),
            ("0004.jpg", b"page-four-bytes"),
            ("0003.webp", b"page-three-bytes"),
            ("0002.png", b"page-two-bytes"),
            ("0001.jpg", b"page-one-bytes"),
            ("0000.jpg", b"page-zero-bytes"),
        ];
        let unit_dir = state.unit_dir(publication.id, units[0].id);
        std::fs::create_dir_all(&unit_dir).unwrap();
        for (name, bytes) in pages {
            std::fs::write(unit_dir.join(name), bytes).unwrap();
        }
        // The undownloaded unit has pages on disk too — a leftover directory
        // from a removed download is exactly this — so nothing but its state
        // can keep it out of the answer.
        let other_dir = state.unit_dir(publication.id, units[1].id);
        std::fs::create_dir_all(&other_dir).unwrap();
        for (name, bytes) in pages {
            std::fs::write(other_dir.join(name), bytes).unwrap();
        }

        let req = Request::builder()
            .uri(format!("/api/v1/manga/{}/fingerprints", publication.id))
            .body(Body::empty())
            .unwrap();
        let resp = super::router(state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entries = body["fingerprints"].as_array().unwrap();

        assert_eq!(
            entries.len(),
            1,
            "only the downloaded unit is fingerprinted"
        );
        assert_eq!(entries[0]["unit_id"], units[0].id.to_string());
        assert_eq!(
            entries[0]["page_count"], 8,
            "the count must be the files on disk, not the recorded page_count"
        );
        assert_eq!(
            entries[0]["page0_sha256"],
            hex::encode(Sha256::digest(b"page-zero-bytes")),
            "the hash must be of the lowest-numbered page file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A publication that does not exist is not a publication with nothing
    /// downloaded. Answering both with an empty list has a recovery client
    /// report zero matches for a library entry that is simply gone, so this
    /// 404s like `GET /manga/{id}` next door.
    #[tokio::test]
    async fn fingerprints_404_for_an_unknown_publication() {
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(Config::default(), db, Registry::default(), None);
        let req = Request::builder()
            .uri(format!(
                "/api/v1/manga/{}/fingerprints",
                uuid::Uuid::now_v7()
            ))
            .body(Body::empty())
            .unwrap();
        let resp = super::router(state).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// An `/api` path this build does not serve must say so. Unmatched paths
    /// used to reach the static fallback, so a client calling a route its
    /// server predates got 200 `text/html` and a JSON decode error instead of
    /// a clean "not supported" — and any API typo answered with the SPA shell.
    #[tokio::test]
    async fn an_unknown_api_path_is_404_json_and_never_the_spa_shell() {
        let dir = fixture_dir("api-404-test");
        std::fs::write(dir.join("index.html"), b"<html>SHELL</html>").unwrap();

        let config = Config {
            static_dir: Some(dir.clone()),
            ..Config::default()
        };
        let db = Db::in_memory().await.unwrap();
        let state = AppState::new(config, db, Registry::default(), None);
        let router = super::router(state);

        for uri in [
            // A route a later version adds, called against this one.
            "/api/v1/manga/019f4921-3946-7c20-9a67-d84d46072fe6/fingerprints/extra",
            "/api/v1/not-a-route",
            // A version prefix that does not exist at all.
            "/api/v2/health",
            "/api/health",
        ] {
            let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
            let resp = router.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
            let content_type = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            assert!(
                !content_type.contains("text/html"),
                "{uri} answered with the SPA shell ({content_type})"
            );
            let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body)
                .unwrap_or_else(|e| panic!("{uri} did not answer JSON: {e}"));
            assert!(body["message"].is_string(), "{uri}");
        }

        // The SPA still owns everything else, deep links included.
        let req = Request::builder()
            .uri("/library/42")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"<html>SHELL</html>");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
