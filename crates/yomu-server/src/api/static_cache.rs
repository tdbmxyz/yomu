//! Cache headers for the static frontend. Trunk emits content-hashed
//! filenames, which can be pinned for a year because a change always
//! arrives under a new URL; everything else must revalidate.

use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

pub(crate) const IMMUTABLE: &str = "public, max-age=31536000, immutable";
pub(crate) const REVALIDATE: &str = "public, max-age=0, must-revalidate";

/// Trunk's fingerprint is a 16-hex-character segment of the filename
/// (`yomu-web-9da5a24d4d3677cc_bg.wasm`, `styles-<hash>.css`). Split on `-`,
/// `_` and `.` so the `_bg` suffix doesn't hide it.
pub(crate) fn cache_control_for(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let hashed = name
        .split(['-', '_', '.'])
        .any(|seg| seg.len() == 16 && seg.chars().all(|c| c.is_ascii_hexdigit()));
    if hashed { IMMUTABLE } else { REVALIDATE }
}

/// Applied to the static service only — never the whole app, or API
/// responses would get cache headers too.
pub(crate) async fn cache_headers(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control_for(&path)),
    );
    // ServeDir sets Content-Encoding but not Vary; `immutable` without Vary
    // lets a shared cache hand a brotli body to a client that never asked.
    headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    response
}

#[cfg(test)]
mod tests {
    use super::cache_control_for;

    #[test]
    fn fingerprinted_assets_are_immutable() {
        assert_eq!(
            cache_control_for("/yomu-web-9da5a24d4d3677cc_bg.wasm"),
            super::IMMUTABLE
        );
        assert_eq!(
            cache_control_for("/styles-dcb9e8dca193296c.css"),
            super::IMMUTABLE
        );
    }

    /// The failure that users cannot clear: a year-long pin on something that
    /// changes in place.
    #[test]
    fn everything_else_revalidates() {
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
            assert_eq!(cache_control_for(path), super::REVALIDATE, "{path}");
        }
    }
}
