//! Cache headers for the static frontend. Trunk emits content-hashed
//! filenames, which can be pinned for a year because a change always
//! arrives under a new URL; everything else must revalidate.

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

pub(crate) const IMMUTABLE: &str = "public, max-age=31536000, immutable";
pub(crate) const REVALIDATE: &str = "public, max-age=0, must-revalidate";

/// Shortest and longest fingerprint we will pin. Trunk's fingerprint is a
/// `u64` rendered as hex with leading zeros dropped, so its length *varies*:
/// a real build produced `yomu-web-ae4beb7cab1d74_bg.wasm`, 14 characters.
/// Demanding exactly 16 rejected it, and the 1.45 MB wasm shipped with
/// `must-revalidate` — the feature silently did nothing for that build. One
/// build in 4096 loses three or more leading nibbles, so this is not a corner
/// worth ignoring. The floor is a hedge against nothing in particular: below
/// eight nibbles the odds are 1 in 4 billion, and a short lower bound only
/// widens the set of *non*-asset URLs that look hashed.
///
/// Widening is safe because the SPA shell no longer depends on this function
/// being right. The fallback service stamps its own `Cache-Control` before
/// this middleware ever sees the response (see [`shell_cache_headers`]), so a
/// route misread as an asset — an id-bearing SPA path, a uuid — is answered by
/// the shell and keeps the shell's `must-revalidate`. The two changes are one
/// change: only a service-level guarantee made a loose URL guess affordable.
const FINGERPRINT_LEN: std::ops::RangeInclusive<usize> = 8..=16;

/// Whether the URL *looks* like a trunk-fingerprinted asset: a hex run of
/// [`FINGERPRINT_LEN`] characters in the filename
/// (`yomu-web-ae4beb7cab1d74_bg.wasm`, `styles-<hash>.css`). Split on `-`,
/// `_` and `.` so the `_bg` suffix doesn't hide it.
///
/// A guess about a URL, never a claim about what was served.
pub(crate) fn fingerprinted(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split(['-', '_', '.']).any(|seg| {
        FINGERPRINT_LEN.contains(&seg.len()) && seg.chars().all(|c| c.is_ascii_hexdigit())
    })
}

/// Whether a year-long pin could ever be correct for this status. A 404 or a
/// 405 describes today's dist, not tomorrow's, and pinning it makes the miss
/// permanent. `304` is included on purpose: it is the successful answer for a
/// body the client already holds, and a real hashed asset revalidating must
/// stay `immutable` or every reload pays a round trip.
fn status_may_be_pinned(response: &Response) -> bool {
    response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED
}

/// Wraps the SPA fallback service, so *everything that service answers* is
/// marked revalidate-always — 200, 304, 206, any content type, compressed or
/// not.
///
/// The failure this exists to prevent is unclearable by the user. `ServeDir`
/// answers a miss with `index.html`, so a request for a hashed asset that no
/// longer exists returns the shell. Pin that for a year and the browser holds
/// HTML under an asset URL forever: deploy v2, a stale tab or the service
/// worker asks for a v1 asset, and if v1 is ever served again (a rollback, or
/// a reverted frontend change reproducing the same trunk hash) the app fetches
/// wasm, receives HTML, and never boots. Nothing short of a manual cache clear
/// escapes.
///
/// Answering "is this the shell?" by inspecting the response is what failed
/// before, and it could not have worked: the old check keyed on
/// `content-type: text/html`, and a `304` carries no `content-type` at all.
/// The `must-revalidate` we set is precisely what makes the browser send a
/// conditional request, so the shell-under-an-asset-URL response *always*
/// comes back as a 304 on its second use — and RFC 9111 §4.3.4 has the client
/// merge a 304's headers into its stored response, so stamping `immutable` on
/// that 304 rewrote the stored HTML to immutable one round trip later. Here
/// the question is answered by *which service ran*, which no response header
/// can hide.
pub(crate) async fn shell_cache_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(REVALIDATE));
    response
}

/// Applied to the static service only — never the whole app, or API
/// responses would get cache headers too.
pub(crate) async fn cache_headers(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let mut response = next.run(request).await;
    let cache_control = if fingerprinted(&path) && status_may_be_pinned(&response) {
        IMMUTABLE
    } else {
        REVALIDATE
    };
    let headers = response.headers_mut();
    // Only if absent. A value already on the response was put there by the
    // inner service, which knows what it served; this middleware knows only
    // the URL, and the URL is the thing that lies.
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static(cache_control));
    // ServeDir sets Content-Encoding but not Vary; `immutable` without Vary
    // lets a shared cache hand a brotli body to a client that never asked.
    // Appended here for both paths — the shell's responses pass through this
    // middleware too, so the inner layer does not repeat it.
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

    /// The build that shipped uncompressed-cache: trunk renders the
    /// fingerprint as hex with leading zeros dropped, so a hash whose top
    /// nibbles are zero is shorter than 16. `ae4beb7cab1d74` is 14 and came
    /// off a real build of this repo; requiring exactly 16 sent the 1.45 MB
    /// wasm out with `must-revalidate` and nothing noticed, because every
    /// fixture in this file happened to be a full-length hash.
    #[test]
    fn a_short_fingerprint_is_still_a_fingerprint() {
        assert!(fingerprinted("/yomu-web-ae4beb7cab1d74_bg.wasm"));
        assert!(fingerprinted("/yomu-web-ae4beb7cab1d74.js"));
        // 15, 12 and 8 nibbles: one, four and eight leading zeros dropped.
        assert!(fingerprinted("/styles-dcb9e8dca19329.css"));
        assert!(fingerprinted("/styles-dcb9e8dca193.css"));
        assert!(fingerprinted("/styles-dcb9e8dc.css"));
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
        ] {
            assert!(!fingerprinted(path), "{path}");
        }
    }

    /// What the URL classifier cannot do, stated so nobody trusts it. A source
    /// id may legally be hex (`registry.rs` allows alphanumeric plus `-`/`_`),
    /// and a uuid's groups are 8/4/4/4/12 hex — both land inside the accepted
    /// length range. Neither is ever pinned, because neither is a file: both
    /// are answered by the SPA fallback service, which sets its own
    /// `must-revalidate`. Asserting the misreads here keeps the guarantee
    /// where it actually lives instead of pretending this function provides
    /// it. The served-response proof is in `api::tests`.
    #[test]
    fn non_assets_may_look_fingerprinted_and_it_does_not_matter() {
        assert!(fingerprinted("/sources/deadbeefdeadbeef"));
        assert!(fingerprinted(
            "/api/v1/publications/019f4921-3946-7c20-9a67-d84d46072fe6"
        ));
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
