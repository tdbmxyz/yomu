//! Sessions and the user extractors (same session shape as chaos).
//!
//! Sessions are opaque tokens, sha256-hashed at rest, presented as an
//! HttpOnly cookie by browsers or `Authorization: Bearer` by native
//! clients. Identity is either the OIDC provider (`oidc.rs`) or — when no
//! `[auth]` is configured — the built-in shared account: every request
//! resolves to [`SHARED_USER`], no login involved.

use axum::extract::{FromRequestParts, State};
use axum::http::header;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use yomu_domain::User;

use crate::api::ApiError;
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "yomu_session";
pub const DEFAULT_SESSION_DAYS: u32 = 90;

/// The single-account-mode user, seeded by migration 0004.
pub const SHARED_USER: Uuid = Uuid::nil();

/// Opaque session token: 244 bits of OS randomness, hex-encoded.
pub fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// What is stored in the sessions table (the raw token never touches disk).
pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// The session token presented by this request, from `Authorization:
/// Bearer …` (native clients) or the session cookie (browsers).
pub fn request_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string());
    if bearer.is_some() {
        return bearer;
    }
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())?
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.to_string())
}

/// Paths under `/api/v1` reachable with no session.
///
/// Exact matches only. A prefix test would make `/healthz` public, and
/// worse, any future route naming a public prefix. Everything absent from
/// this list is content, and content requires identity — which is the
/// point: a new route is protected because someone has to come here to
/// exempt it.
pub fn is_public(path: &str) -> bool {
    matches!(
        path,
        "/health"
            | "/auth/me"
            | "/auth/login"
            | "/auth/callback"
            | "/auth/exchange"
            // Signing out with a session the server already dropped must
            // not 401: the caller's goal is to have no session.
            | "/auth/logout"
    )
}

/// The two routes an `<img>` loads, which may present a media token
/// instead of a session (see `media_token.rs`). Not public: without a
/// valid token they still 401.
pub fn takes_media_token(path: &str) -> bool {
    let mut segments = path.split('/').skip(1);
    match (segments.next(), segments.next(), segments.next()) {
        // /publications/{id}/cover
        (Some("publications"), Some(_), Some("cover")) => segments.next().is_none(),
        // /units/{id}/pages/{n} — but not the page list, which is JSON
        // the client fetches with a header.
        (Some("units"), Some(_), Some("pages")) => {
            segments.next().is_some_and(|n| !n.is_empty()) && segments.next().is_none()
        }
        _ => false,
    }
}

async fn resolve(parts: &Parts, state: &AppState) -> Option<User> {
    if !state.config.auth.oidc_enabled() {
        return state.db.user_by_id(SHARED_USER).await.ok();
    }
    if let Some(token) = request_token(&parts.headers)
        && let Ok(user) = state.db.user_by_session(&token_hash(&token)).await
    {
        return Some(user);
    }
    // No session, but a trusted proxy may already have signed this
    // browser in. Only believed when the request carries the shared
    // secret (see proxy_identity): the identity headers alone prove
    // nothing, since a request on the direct LAN route never passes
    // through the outpost that would overwrite them.
    let proxy = crate::proxy_identity::identify(state.proxy_secret.as_deref(), |name| {
        parts
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    })?;
    state
        .db
        .upsert_oidc_user(&proxy.sub, &proxy.username, &proxy.display_name)
        .await
        .ok()
}

/// Default-deny for everything under `/api/v1`.
///
/// Resolves the session once and puts the `User` in request extensions,
/// so `CurrentUser` becomes a read rather than a second database hit.
/// Anything not named in [`is_public`] needs that user; the two image
/// routes may present a media token instead, because an `<img>` cannot
/// send a header.
///
/// This is a layer rather than an extractor on every handler because
/// axum offers no way to enumerate a router's routes: with per-handler
/// extractors, the next route added is open again and no test can catch
/// it. Here a route is reachable only because someone exempted it.
///
/// In single-account mode `resolve` returns the shared user for
/// everyone, so a deployment without `[auth]` is unaffected.
pub async fn require_auth(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let (parts, body) = request.into_parts();
    let user = resolve(&parts, &state).await;
    let mut request = axum::extract::Request::from_parts(parts, body);

    if let Some(user) = user {
        request.extensions_mut().insert(user);
        return next.run(request).await;
    }

    let path = request.uri().path().to_string();
    if is_public(&path) {
        return next.run(request).await;
    }
    if takes_media_token(&path)
        && let Some(token) = media_token_param(request.uri().query())
        && let Some(id) = state.media_key.verify(&token, 0)
        && let Ok(user) = state.db.user_by_id(id).await
    {
        request.extensions_mut().insert(user);
        return next.run(request).await;
    }
    ApiError::Unauthorized.into_response()
}

fn media_token_param(query: Option<&str>) -> Option<String> {
    query?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == "mt")
        .map(|(_, value)| value.to_string())
}

/// Extractor for handlers that need a user (progress reads/writes). In
/// single-account mode this is always the shared user; in OIDC mode it
/// requires a valid session.
pub struct CurrentUser(pub User);

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        // `require_auth` already resolved this request. The fallback
        // keeps the extractor correct on its own, for a router built
        // without the layer.
        if let Some(user) = parts.extensions.get::<User>() {
            return Ok(CurrentUser(user.clone()));
        }
        resolve(parts, state)
            .await
            .map(CurrentUser)
            .ok_or(ApiError::Unauthorized)
    }
}

/// Extractor for handlers that *enrich* their response with per-user data
/// (library positions) but stay usable signed-out. Never rejects.
pub struct OptionalUser(pub Option<User>);

impl FromRequestParts<AppState> for OptionalUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(user) = parts.extensions.get::<User>() {
            return Ok(OptionalUser(Some(user.clone())));
        }
        Ok(OptionalUser(resolve(parts, state).await))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_token_prefers_bearer_over_cookie() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "a=b; yomu_session=cookie-tok".parse().unwrap(),
        );
        assert_eq!(request_token(&headers).as_deref(), Some("cookie-tok"));
        headers.insert(header::AUTHORIZATION, "Bearer bearer-tok".parse().unwrap());
        assert_eq!(request_token(&headers).as_deref(), Some("bearer-tok"));
    }

    /// The routes that must answer without a session, and why: health
    /// tells an app the server is there and how to sign in, /auth carries
    /// the sign-in itself. Everything else is content.
    #[test]
    fn only_the_sign_in_surface_is_public() {
        assert!(is_public("/health"));
        assert!(is_public("/auth/me"));
        assert!(is_public("/auth/login"));
        assert!(is_public("/auth/callback"));
        assert!(is_public("/auth/exchange"));
        // Signing out with a session the server already dropped must not
        // 401: the caller's goal is to have no session.
        assert!(is_public("/auth/logout"));

        assert!(!is_public("/library"));
        assert!(!is_public("/publications/0199-abc"));
        assert!(!is_public("/units/0199-abc/pages/3"));
        assert!(!is_public("/sources"));
        assert!(!is_public("/auth/media-token"));
    }

    /// Prefix matching would make /healthz public, and any future route
    /// that happens to share a public prefix.
    #[test]
    fn public_paths_match_exactly() {
        assert!(!is_public("/health/../library"));
        assert!(!is_public("/healthz"));
        assert!(!is_public("/auth/me/library"));
        assert!(!is_public(""));
    }

    /// The image routes are not public — they are reachable *with a media
    /// token*, which is a different thing, decided elsewhere.
    #[test]
    fn image_routes_take_a_media_token_but_are_not_public() {
        assert!(!is_public("/publications/0199-abc/cover"));
        assert!(takes_media_token("/publications/0199-abc/cover"));
        assert!(takes_media_token("/units/0199-abc/pages/12"));
        // The page *list* is JSON, fetched by the client with a header.
        assert!(!takes_media_token("/units/0199-abc/pages"));
        assert!(!takes_media_token("/library"));
        assert!(!takes_media_token("/publications/0199-abc"));
    }

    #[test]
    fn tokens_hash_stably() {
        let token = new_token();
        assert_eq!(token.len(), 64);
        assert_eq!(token_hash(&token), token_hash(&token));
        assert_ne!(token_hash(&token), token_hash("other"));
    }
}
