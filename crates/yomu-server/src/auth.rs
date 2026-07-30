//! Sessions and the user extractors (same session shape as chaos).
//!
//! Sessions are opaque tokens, sha256-hashed at rest, presented as an
//! HttpOnly cookie by browsers or `Authorization: Bearer` by native
//! clients. Identity is either the OIDC provider (`oidc.rs`) or — when no
//! `[auth]` is configured — the built-in shared account: every request
//! resolves to [`SHARED_USER`], no login involved.

use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
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
        // /manga/{id}/cover
        (Some("manga"), Some(_), Some("cover")) => segments.next().is_none(),
        // /chapters/{id}/pages/{n} — but not the page list, which is JSON
        // the client fetches with a header.
        (Some("chapters"), Some(_), Some("pages")) => {
            segments.next().is_some_and(|n| !n.is_empty()) && segments.next().is_none()
        }
        _ => false,
    }
}

async fn resolve(parts: &Parts, state: &AppState) -> Option<User> {
    if !state.config.auth.oidc_enabled() {
        return state.db.user_by_id(SHARED_USER).await.ok();
    }
    let token = request_token(&parts.headers)?;
    state.db.user_by_session(&token_hash(&token)).await.ok()
}

/// Extractor for handlers that need a user (progress reads/writes). In
/// single-account mode this is always the shared user; in OIDC mode it
/// requires a valid session.
pub struct CurrentUser(pub User);

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
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
        assert!(!is_public("/manga/0199-abc"));
        assert!(!is_public("/chapters/0199-abc/pages/3"));
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
        assert!(!is_public("/manga/0199-abc/cover"));
        assert!(takes_media_token("/manga/0199-abc/cover"));
        assert!(takes_media_token("/chapters/0199-abc/pages/12"));
        // The page *list* is JSON, fetched by the client with a header.
        assert!(!takes_media_token("/chapters/0199-abc/pages"));
        assert!(!takes_media_token("/library"));
        assert!(!takes_media_token("/manga/0199-abc"));
    }

    #[test]
    fn tokens_hash_stably() {
        let token = new_token();
        assert_eq!(token.len(), 64);
        assert_eq!(token_hash(&token), token_hash(&token));
        assert_ne!(token_hash(&token), token_hash("other"));
    }
}
