//! Cache headers for the static frontend. Trunk emits content-hashed
//! filenames, which can be pinned for a year because a change always
//! arrives under a new URL; everything else must revalidate.

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

pub(crate) const IMMUTABLE: &str = "public, max-age=31536000, immutable";
pub(crate) const REVALIDATE: &str = "public, max-age=0, must-revalidate";

/// Trunk's fingerprint is a 16-hex-character segment of the filename
/// (`yomu-web-9da5a24d4d3677cc_bg.wasm`, `styles-<hash>.css`). Split on `-`,
/// `_` and `.` so the `_bg` suffix doesn't hide it.
///
/// This looks at the URL only, so it is half an answer: an SPA route can wear
/// a 16-hex last segment (`/sources/deadbeefdeadbeef` — source ids are
/// alphanumeric plus `-`/`_`), and a *missing* asset URL is answered by the
/// SPA fallback. See [`cache_headers`] for the other half.
pub(crate) fn fingerprinted(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split(['-', '_', '.'])
        .any(|seg| seg.len() == 16 && seg.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The invariant: a response whose body is the SPA shell must never be
/// `immutable`, whatever the URL looked like.
///
/// `ServeDir` falls back to `index.html` on a miss, so a request for a hashed
/// asset that does not exist returns 200 with `text/html`. Pinning that for a
/// year is unclearable: deploy v2, a stale tab or service worker asks for a v1
/// asset URL, the browser caches HTML under it forever — and if v1 is ever
/// restored (a rollback, or a reverted frontend change reproducing the same
/// trunk hash) the app fetches wasm, receives HTML, and never boots. Only a
/// manual cache clear escapes. Trunk never fingerprints an HTML file, so
/// "the content type is HTML" is exactly "this is the fallback".
fn serves_its_own_bytes(response: &Response) -> bool {
    let served = response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED;
    let is_shell = response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"text/html"));
    served && !is_shell
}

/// Applied to the static service only — never the whole app, or API
/// responses would get cache headers too.
pub(crate) async fn cache_headers(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let mut response = next.run(request).await;
    let cache_control = if fingerprinted(&path) && serves_its_own_bytes(&response) {
        IMMUTABLE
    } else {
        REVALIDATE
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    // ServeDir sets Content-Encoding but not Vary; `immutable` without Vary
    // lets a shared cache hand a brotli body to a client that never asked.
    // `append`, not `insert`: the CORS layer sets its own Vary (origin and the
    // two access-control-request-* headers), and replacing it would make a
    // shared cache reuse one origin's preflight answer for another. Which
    // layer runs first must not decide whether that bug exists.
    headers.append(header::VARY, HeaderValue::from_static("accept-encoding"));
    response
}

#[cfg(test)]
mod tests {
    use super::fingerprinted;

    #[test]
    fn fingerprinted_assets_are_recognised() {
        assert!(fingerprinted("/yomu-web-9da5a24d4d3677cc_bg.wasm"));
        assert!(fingerprinted("/styles-dcb9e8dca193296c.css"));
    }

    /// The failure that users cannot clear: a year-long pin on something that
    /// changes in place.
    #[test]
    fn everything_else_is_not() {
        for path in [
            "/index.html",
            "/",
            "/sw.js",
            "/manifest.webmanifest",
            "/favicon.svg",
            "/icon-192.png",
            "/library",
            // uuid groups are 8/4/4/4/12 hex — never 16, so an API path
            // cannot be mistaken for a fingerprint
            "/api/v1/manga/019f4921-3946-7c20-9a67-d84d46072fe6",
        ] {
            assert!(!fingerprinted(path), "{path}");
        }
    }

    /// The URL classifier cannot save this one: a source id may legally be 16
    /// hex characters, so an SPA route reaches here looking exactly like an
    /// asset. Only the response check keeps it out of `immutable`.
    #[test]
    fn an_spa_route_can_look_fingerprinted() {
        assert!(fingerprinted("/sources/deadbeefdeadbeef"));
    }

    /// With the layers as they stand the CORS `Vary` is written *after* this
    /// middleware, so an `insert` here happens to survive. That is a property
    /// of the ordering, not of the code — this pins the middleware itself by
    /// handing it a response that already carries the CORS values, the shape
    /// a reorder would produce. Losing them makes a shared cache answer one
    /// origin's preflight from another's cached response.
    #[tokio::test]
    async fn vary_is_appended_to_values_already_on_the_response() {
        use axum::body::Body;
        use axum::http::{HeaderValue, Request, header};
        use axum::routing::get;
        use tower::ServiceExt;

        let app = axum::Router::new()
            .route(
                "/index.html",
                get(|| async {
                    let mut resp = axum::response::Response::new(Body::empty());
                    resp.headers_mut().insert(
                        header::VARY,
                        HeaderValue::from_static(
                            "origin, access-control-request-method, \
                             access-control-request-headers",
                        ),
                    );
                    resp
                }),
            )
            .layer(axum::middleware::from_fn(super::cache_headers));

        let req = Request::builder()
            .uri("/index.html")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        let vary = resp
            .headers()
            .get_all(header::VARY)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>()
            .join(", ");
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
    }
}
