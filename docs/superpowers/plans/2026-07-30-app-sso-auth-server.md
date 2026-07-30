# App SSO Auth — Server Half, Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `[auth]` actually protect yomu, and give the app a way to turn an authentik login into a yomu session.

**Architecture:** A middleware over `/api/v1` resolves the session once and rejects anything not explicitly public, so a route is protected unless someone exempts it. Two new endpoints — an introspection-checked token exchange for the app, and short-lived media tokens for the `<img>` routes that cannot carry a header. `/health` advertises how to sign in, so the app self-configures from a server address alone.

**Tech Stack:** Rust, axum 0.8, sqlx/SQLite, reqwest, hmac-sha256.

**Spec:** `docs/superpowers/specs/2026-07-30-app-sso-auth-design.md` (§1–§4)

**Scope:** the server only. The shell and UI are a second plan; this half deploys inert — in single-account mode (`[auth]` absent) every request still resolves to `SHARED_USER` and nothing changes.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/yomu-server/src/auth.rs` (modify) | `is_public()`, the `require_auth` middleware, `CurrentUser` reading from extensions |
| `crates/yomu-server/src/media_token.rs` (create) | mint/verify the stateless image tokens; pure, no axum |
| `crates/yomu-server/src/oidc.rs` (modify) | `introspection_endpoint` in discovery; `introspect()` |
| `crates/yomu-server/src/api/auth.rs` (modify) | `POST /auth/exchange`, `GET /auth/media-token` |
| `crates/yomu-server/src/api/mod.rs` (modify) | apply the layer, register the routes, advertise on `/health` |
| `crates/yomu-server/src/config.rs` (modify) | `app_client_id` |
| `crates/yomu-server/src/state.rs` (modify) | the media-token key |
| `crates/yomu-domain/src/api.rs` (modify) | `AuthAdvertisement`, `ExchangeRequest/Response`, `MediaTokenResponse` |

**Commands:** `cargo test -p yomu-server`, `just check`.

---

## Task 1: Media tokens

Pure logic first, with no axum in the way.

**Files:**
- Create: `crates/yomu-server/src/media_token.rs`
- Modify: `crates/yomu-server/src/lib.rs` (module declaration)

- [ ] **Step 1: Declare the module**

In `crates/yomu-server/src/lib.rs`, add `pub mod media_token;` in alphabetical order among the existing `mod` lines.

- [ ] **Step 2: Write the failing tests**

Create `crates/yomu-server/src/media_token.rs` containing only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn key() -> Key {
        Key::generate()
    }

    #[test]
    fn a_fresh_token_verifies_for_its_own_user() {
        let key = key();
        let user = Uuid::from_u128(7);
        let token = key.mint(user, 3600);
        assert_eq!(key.verify(&token, 0), Some(user));
    }

    /// The signature is the whole point: an attacker who can read a token
    /// must not be able to make one for another user.
    #[test]
    fn a_tampered_token_is_rejected() {
        let key = key();
        let token = key.mint(Uuid::from_u128(7), 3600);
        let forged = token.replace(
            &Uuid::from_u128(7).simple().to_string(),
            &Uuid::from_u128(8).simple().to_string(),
        );
        assert_ne!(forged, token);
        assert_eq!(key.verify(&forged, 0), None);
    }

    /// Short-lived is the other half of why this is safe to put in a URL.
    #[test]
    fn an_expired_token_is_rejected() {
        let key = key();
        let token = key.mint(Uuid::from_u128(7), 60);
        assert_eq!(key.verify(&token, 61), None);
    }

    /// A key is per-process: a restart invalidates outstanding tokens
    /// rather than accepting anything signed by anyone.
    #[test]
    fn another_key_does_not_verify_this_token() {
        let token = key().mint(Uuid::from_u128(7), 3600);
        assert_eq!(key().verify(&token, 0), None);
    }

    #[test]
    fn malformed_tokens_are_rejected_rather_than_panicking() {
        let key = key();
        for bad in ["", ".", "a.b", "a.b.c", "....", "zz.1.2"] {
            assert_eq!(key.verify(bad, 0), None, "{bad:?}");
        }
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p yomu-server media_token::`
Expected: FAIL to compile — `cannot find type Key in this scope`.

- [ ] **Step 4: Implement**

Prepend to `crates/yomu-server/src/media_token.rs`:

```rust
//! Short-lived tokens for the two routes an `<img>` loads.
//!
//! Covers and page images are fetched by the browser's image loader,
//! which sends no `Authorization` header — and the shells' cookies never
//! apply from `tauri://localhost`. So those two routes accept a signed,
//! expiring token in the query string instead.
//!
//! Stateless: the key is generated at startup, so a restart invalidates
//! outstanding tokens. They last an hour and clients refetch, which is
//! cheaper than persisting a secret.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

/// `<user>.<expiry>.<hmac>` — user and expiry are readable, the mac is
/// what makes them binding.
pub struct Key([u8; 32]);

impl Key {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        // Two v4 UUIDs: the same OS randomness `auth::new_token` uses.
        bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        Self(bytes)
    }

    /// A token for `user`, valid for `ttl_secs` from now.
    pub fn mint(&self, user: Uuid, ttl_secs: u64) -> String {
        self.mint_at(user, now() + ttl_secs)
    }

    fn mint_at(&self, user: Uuid, expiry: u64) -> String {
        let payload = format!("{}.{expiry}", user.simple());
        format!("{payload}.{}", hex::encode(self.sign(&payload)))
    }

    /// The user this token speaks for, or `None` if it is malformed,
    /// forged or expired. `skew_secs` exists for the tests; production
    /// passes 0.
    pub fn verify(&self, token: &str, skew_secs: u64) -> Option<Uuid> {
        let (payload, mac) = token.rsplit_once('.')?;
        let (user, expiry) = payload.split_once('.')?;
        let user: Uuid = user.parse().ok()?;
        let expiry: u64 = expiry.parse().ok()?;
        if hex::decode(mac).ok()? != self.sign(payload) {
            return None;
        }
        (now() + skew_secs < expiry).then_some(user)
    }

    fn sign(&self, payload: &str) -> Vec<u8> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).expect("hmac accepts any key length");
        mac.update(payload.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
```

Note `verify` compares with `!=` on a `Vec<u8>`; that is a non-constant-time comparison of a MAC. Use `mac.verify_slice(&expected).is_ok()` instead, which is constant time:

```rust
    fn check(&self, payload: &str, mac_hex: &str) -> bool {
        let Ok(expected) = hex::decode(mac_hex) else {
            return false;
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).expect("hmac accepts any key length");
        mac.update(payload.as_bytes());
        mac.verify_slice(&expected).is_ok()
    }
```

and call `self.check(payload, mac)` from `verify`, keeping `sign` for `mint` only.

- [ ] **Step 5: Add the dependency**

`crates/yomu-server/Cargo.toml` — add `hmac = "0.12"` under `[dependencies]` (sha2, hex and uuid are already there; confirm with `grep -n 'sha2\|hex\|uuid' crates/yomu-server/Cargo.toml`).

- [ ] **Step 6: Run the tests**

Run: `cargo test -p yomu-server media_token::`
Expected: PASS, 5 tests.

- [ ] **Step 7: Commit**

```bash
git add crates/yomu-server/src/media_token.rs crates/yomu-server/src/lib.rs crates/yomu-server/Cargo.toml Cargo.lock
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(server): signed short-lived tokens for image routes

Covers and pages are loaded by <img>, which sends no Authorization
header, and the shells get no cookie from tauri://localhost. Requiring a
session on those two routes would blank every cover and every page, so
they take a signed expiring token in the query instead.

Stateless and per-process: a restart invalidates outstanding tokens,
which costs a refetch and saves persisting a secret.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 2: `is_public` — what stays reachable without a session

**Files:**
- Modify: `crates/yomu-server/src/auth.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/yomu-server/src/auth.rs`:

```rust
    /// The four routes that must answer without a session, and why:
    /// health tells an app the server is there and how to sign in, /auth
    /// carries the sign-in itself. Everything else is content.
    #[test]
    fn only_the_sign_in_surface_is_public() {
        assert!(is_public("/health"));
        assert!(is_public("/auth/me"));
        assert!(is_public("/auth/login"));
        assert!(is_public("/auth/callback"));
        assert!(is_public("/auth/exchange"));
        // Signing out with an expired session must not 401 into a corner.
        assert!(is_public("/auth/logout"));

        assert!(!is_public("/library"));
        assert!(!is_public("/manga/0199-abc"));
        assert!(!is_public("/chapters/0199-abc/pages/3"));
        assert!(!is_public("/sources"));
        assert!(!is_public("/auth/media-token"));
    }

    /// Prefix matching would make /healthz or /auth/../library public.
    #[test]
    fn public_paths_match_exactly() {
        assert!(!is_public("/health/../library"));
        assert!(!is_public("/healthz"));
        assert!(!is_public("/auth/me/library"));
        assert!(!is_public(""));
    }

    /// The image routes are not public — they are reachable *with a media
    /// token*, which is a different thing and is decided elsewhere.
    #[test]
    fn image_routes_are_not_public() {
        assert!(!is_public("/manga/0199-abc/cover"));
        assert!(takes_media_token("/manga/0199-abc/cover"));
        assert!(takes_media_token("/chapters/0199-abc/pages/12"));
        assert!(!takes_media_token("/chapters/0199-abc/pages"));
        assert!(!takes_media_token("/library"));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p yomu-server auth::`
Expected: FAIL — `cannot find function is_public`.

- [ ] **Step 3: Implement**

Add to `crates/yomu-server/src/auth.rs`:

```rust
/// Paths under `/api/v1` reachable with no session.
///
/// Exact matches only. A prefix test would make `/healthz` public, and
/// worse, anything a future route names with a public prefix. Everything
/// absent from this list is content, and content requires identity —
/// which is the point: a new route is protected because someone has to
/// come here to exempt it.
pub fn is_public(path: &str) -> bool {
    matches!(
        path,
        "/health"
            | "/auth/me"
            | "/auth/login"
            | "/auth/callback"
            | "/auth/exchange"
            // Signing out with a session the server already dropped must
            // not 401: the client's goal is to have no session.
            | "/auth/logout"
    )
}

/// The two routes an `<img>` loads, which may present a media token
/// instead of a session (see `media_token.rs`). Not public: without a
/// valid token they still 401.
pub fn takes_media_token(path: &str) -> bool {
    let cover = path.starts_with("/manga/") && path.ends_with("/cover");
    let page = path.starts_with("/chapters/")
        && path
            .split('/')
            .nth(3)
            .is_some_and(|segment| segment == "pages")
        && path.split('/').nth(4).is_some_and(|n| !n.is_empty());
    cover || page
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p yomu-server auth::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/yomu-server/src/auth.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(server): name the routes that stay reachable without a session

Exact matches, not prefixes: a prefix test makes /healthz public, and
any future route that happens to share a public prefix.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 3: The gate middleware

**Files:**
- Modify: `crates/yomu-server/src/auth.rs`, `crates/yomu-server/src/state.rs`, `crates/yomu-server/src/api/mod.rs`

- [ ] **Step 1: Put the media-token key in state**

In `crates/yomu-server/src/state.rs`, add to `AppState`:

```rust
    /// Signing key for image-route tokens, generated per process (see
    /// media_token.rs).
    pub media_key: Arc<crate::media_token::Key>,
```

and in `AppState::new`, `media_key: Arc::new(crate::media_token::Key::generate()),`.

- [ ] **Step 2: Write the middleware**

In `crates/yomu-server/src/auth.rs`:

```rust
/// Default-deny for `/api/v1`.
///
/// Resolves the session once and puts the `User` in request extensions,
/// so `CurrentUser` is a read rather than a second database hit. Anything
/// not named in [`is_public`] needs that user; the two image routes may
/// present a media token instead, because an `<img>` cannot send a
/// header.
///
/// In single-account mode `resolve` returns the shared user for everyone,
/// so this layer changes nothing for a deployment without `[auth]`.
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = request.uri().path().to_string();
    let (parts, body) = request.into_parts();
    let user = resolve(&parts, &state).await;
    request = axum::extract::Request::from_parts(parts, body);

    if let Some(user) = user {
        request.extensions_mut().insert(user);
        return next.run(request).await;
    }
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
```

Add the imports it needs at the top of the file: `axum::extract::State`, `axum::response::IntoResponse`.

- [ ] **Step 3: Make `CurrentUser` read the extension**

Replace the two extractor impls' bodies in `crates/yomu-server/src/auth.rs`:

```rust
impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        // The layer already resolved this request; falling back keeps the
        // extractor correct for any router built without it (the tests).
        match parts.extensions.get::<User>() {
            Some(user) => Ok(CurrentUser(user.clone())),
            None => resolve(parts, state).await.map(CurrentUser).ok_or(ApiError::Unauthorized),
        }
    }
}
```

and the same shape for `OptionalUser`, returning `OptionalUser(None)` instead of an error.

- [ ] **Step 4: Apply the layer**

In `crates/yomu-server/src/api/mod.rs`, on the `api` router, after `.with_state(state.clone())` and before `.fallback(api_not_found)`:

```rust
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_auth,
        ))
```

- [ ] **Step 5: Write the coverage test**

Add to the `tests` module in `crates/yomu-server/src/api/mod.rs`:

```rust
    /// Every route the router serves, called with no credentials against a
    /// server that has [auth] configured. Anything not on the sign-in
    /// surface must be 401 — including the reads, which is the gap this
    /// closes: the library, the chapter lists and the page images used to
    /// answer anyone who asked.
    #[tokio::test]
    async fn no_route_answers_without_a_session() {
        let routes: &[(&str, &str)] = &[
            ("GET", "/api/v1/library"),
            ("POST", "/api/v1/library"),
            ("POST", "/api/v1/library/rescan"),
            ("GET", "/api/v1/categories"),
            ("PUT", "/api/v1/categories/reading"),
            ("GET", "/api/v1/manga/00000000-0000-0000-0000-000000000001"),
            ("PUT", "/api/v1/manga/00000000-0000-0000-0000-000000000001"),
            ("DELETE", "/api/v1/manga/00000000-0000-0000-0000-000000000001"),
            ("POST", "/api/v1/manga/00000000-0000-0000-0000-000000000001/refresh"),
            ("GET", "/api/v1/manga/00000000-0000-0000-0000-000000000001/cover"),
            ("GET", "/api/v1/manga/00000000-0000-0000-0000-000000000001/fingerprints"),
            ("PUT", "/api/v1/manga/00000000-0000-0000-0000-000000000001/position"),
            ("GET", "/api/v1/chapters/00000000-0000-0000-0000-000000000001/pages"),
            ("GET", "/api/v1/chapters/00000000-0000-0000-0000-000000000001/pages/0"),
            ("POST", "/api/v1/chapters/00000000-0000-0000-0000-000000000001/download"),
            ("POST", "/api/v1/chapters/download"),
            ("POST", "/api/v1/chapters/remove-downloads"),
            ("POST", "/api/v1/chapters/mark"),
            ("GET", "/api/v1/sources"),
            ("GET", "/api/v1/sources/x/search"),
            ("GET", "/api/v1/sources/x/browse"),
            ("GET", "/api/v1/search"),
            ("GET", "/api/v1/covers"),
            ("GET", "/api/v1/updates"),
            ("GET", "/api/v1/downloads"),
            ("POST", "/api/v1/downloads/retry"),
            ("POST", "/api/v1/downloads/dismiss"),
            ("GET", "/api/v1/progress/events"),
            ("POST", "/api/v1/progress/events"),
            ("GET", "/api/v1/backup"),
            ("POST", "/api/v1/restore"),
            ("GET", "/api/v1/auth/media-token"),
        ];
        for (method, path) in routes {
            assert_eq!(
                status_with_auth(method, path).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path} answered without a session"
            );
        }
    }

    /// The sign-in surface itself: an app with no session must be able to
    /// ask where it is and how to sign in, or "not signed in" is
    /// indistinguishable from "server down".
    #[tokio::test]
    async fn the_sign_in_surface_stays_reachable() {
        for (method, path) in [
            ("GET", "/api/v1/health"),
            ("GET", "/api/v1/auth/me"),
            ("POST", "/api/v1/auth/logout"),
        ] {
            assert_ne!(
                status_with_auth(method, path).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path} must answer without a session"
            );
        }
    }
```

with the helper, alongside the existing `status_of`:

```rust
    /// A router whose config has `[auth]` configured, so `resolve` demands
    /// a session rather than returning the shared user.
    async fn status_with_auth(method: &str, path: &str) -> StatusCode {
        let db = Db::in_memory().await.unwrap();
        let mut config = Config::default();
        config.auth.issuer = Some("https://auth.example.test/".parse().unwrap());
        config.auth.client_id = "yomu".into();
        config.auth.client_secret = "secret".into();
        let state = AppState::new(config, db, Registry::default(), None);
        let router = super::router(state);
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        router.oneshot(request).await.unwrap().status()
    }
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p yomu-server api::tests`
Expected: PASS. If a route answers 200, that route is a hole — fix the layer, not the test.

- [ ] **Step 7: Verify the mutation**

Temporarily add `"/library"` to `is_public` and re-run; `no_route_answers_without_a_session` must fail. Remove it again.

- [ ] **Step 8: Commit**

```bash
git add crates/yomu-server/src
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(server): require a session for everything but the sign-in surface

[auth] gated every mutation and no read: GET /library, GET /manga/{id}
and the page images answered anyone who asked. The content was the part
left open.

The gate is a layer rather than an extractor on 28 handlers, because
axum exposes no way to enumerate a Router's routes — no test could have
caught the next one added. Now a route is protected unless someone comes
to is_public() and exempts it.

Resolving once into request extensions also stops CurrentUser being a
second database hit per request.

Single-account mode is unaffected: with no [auth], resolve() returns the
shared user for everyone, exactly as before.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 4: Token introspection

**Files:**
- Modify: `crates/yomu-server/src/oidc.rs`

- [ ] **Step 1: Extend discovery**

In `crates/yomu-server/src/oidc.rs`, add to `struct Discovery`:

```rust
    /// RFC 7662. authentik publishes it; a provider that does not means
    /// app sign-in cannot be offered, and `introspect` says so.
    #[serde(default)]
    introspection_endpoint: Option<Url>,
```

- [ ] **Step 2: Write the failing test**

Add to `crates/yomu-server/src/oidc.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The audience check is the reason this endpoint exists. Without it
    /// any access token from any application on the same IdP could be
    /// traded for a yomu session.
    #[test]
    fn only_an_active_token_for_our_client_is_accepted() {
        let ours = "yomu-app";
        let active = |client: &str| Introspection {
            active: true,
            client_id: Some(client.into()),
            sub: Some("user-1".into()),
            preferred_username: Some("tibo".into()),
            name: None,
        };
        assert!(active(ours).identity(ours).is_ok());
        assert_eq!(
            active("some-other-app").identity(ours),
            Err(IntrospectionError::WrongClient)
        );
        let mut inactive = active(ours);
        inactive.active = false;
        assert_eq!(inactive.identity(ours), Err(IntrospectionError::Inactive));
        let mut anonymous = active(ours);
        anonymous.sub = None;
        assert_eq!(anonymous.identity(ours), Err(IntrospectionError::Inactive));
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p yomu-server oidc::`
Expected: FAIL — `cannot find type Introspection`.

- [ ] **Step 4: Implement**

Add to `crates/yomu-server/src/oidc.rs`:

```rust
/// RFC 7662 response, plus the profile claims authentik includes.
#[derive(Debug, Deserialize)]
pub struct Introspection {
    pub active: bool,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub sub: Option<String>,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum IntrospectionError {
    /// Expired, revoked, or not a token at all.
    Inactive,
    /// Valid — for a different application on the same provider. Trading
    /// it for a yomu session would let any app on this IdP in.
    WrongClient,
}

impl Introspection {
    /// The identity this token speaks for, if it is ours to accept.
    pub fn identity(&self, app_client_id: &str) -> Result<UserInfo, IntrospectionError> {
        if !self.active {
            return Err(IntrospectionError::Inactive);
        }
        if self.client_id.as_deref() != Some(app_client_id) {
            return Err(IntrospectionError::WrongClient);
        }
        let sub = self.sub.clone().ok_or(IntrospectionError::Inactive)?;
        Ok(UserInfo {
            sub,
            preferred_username: self.preferred_username.clone(),
            name: self.name.clone(),
        })
    }
}

impl OidcRuntime {
    /// Ask the provider about an access token an app presented.
    /// Authenticated with yomu's own confidential credentials — no new
    /// secret, and the provider will only answer a client it knows.
    pub async fn introspect(&self, access_token: &str) -> Result<Introspection, String> {
        let discovery = self.discovery().await?;
        let endpoint = discovery
            .introspection_endpoint
            .clone()
            .ok_or("this provider publishes no introspection endpoint")?;
        self.http
            .post(endpoint)
            .form(&[
                ("token", access_token),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
            ])
            .send()
            .await
            .map_err(|e| format!("introspection: {e}"))?
            .error_for_status()
            .map_err(|e| format!("introspection: {e}"))?
            .json()
            .await
            .map_err(|e| format!("introspection response: {e}"))
    }
}
```

`UserInfo` already exists in this file; it gains no fields.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p yomu-server oidc::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/yomu-server/src/oidc.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(server): introspect the access token an app presents

Verifying by signature would mean a JWKS fetch and algorithm
negotiation, and authentik's proxy providers sign HS256 while publishing
an empty key set — verification that verifies nothing.

Introspection answers the question that actually matters here: which
client was this token issued to. Without that check, an access token
minted for any other application on the same provider could be traded
for a yomu session.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 5: `/auth/exchange` and `/auth/media-token`

**Files:**
- Modify: `crates/yomu-domain/src/api.rs`, `crates/yomu-server/src/config.rs`, `crates/yomu-server/src/api/auth.rs`, `crates/yomu-server/src/api/mod.rs`

- [ ] **Step 1: Wire types**

`crates/yomu-domain/src/api.rs`:

```rust
/// What an app posts to trade an IdP access token for a yomu session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeRequest {
    pub access_token: String,
}

/// A yomu session, for a client that holds it as a bearer token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResponse {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// Short-lived credential for the image routes (see the server's
/// media_token module).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaTokenResponse {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}
```

`crates/yomu-server/src/config.rs`, in `AuthConfig`:

```rust
    /// Client id of the *public* provider the native shells use. Empty
    /// disables app sign-in: `/auth/exchange` answers 404 and only the
    /// browser flow exists.
    #[serde(default)]
    pub app_client_id: String,
```

and next to `oidc_enabled`:

```rust
    pub fn app_sign_in_enabled(&self) -> bool {
        self.oidc_enabled() && !self.app_client_id.is_empty()
    }
```

- [ ] **Step 2: Implement the handlers**

In `crates/yomu-server/src/api/auth.rs`:

```rust
/// Trade an IdP access token for a yomu session. The app holds the
/// result as a bearer, so there is no refresh dance on the client: when
/// it expires, the app signs in again.
pub async fn exchange(
    State(state): State<AppState>,
    Json(req): Json<ExchangeRequest>,
) -> Result<Json<SessionResponse>, ApiError> {
    if !state.config.auth.app_sign_in_enabled() {
        return Err(ApiError::NotFound);
    }
    let oidc = require_oidc(&state)?;
    let introspection = oidc
        .introspect(&req.access_token)
        .await
        // The provider is unreachable or refused us: not the caller's
        // fault, and a different message than "sign in again".
        .map_err(ApiError::UpstreamFailed)?;

    let claims = introspection
        .identity(&state.config.auth.app_client_id)
        .map_err(|err| match err {
            IntrospectionError::Inactive => ApiError::Unauthorized,
            IntrospectionError::WrongClient => {
                ApiError::Forbidden("this token was not issued for yomu".into())
            }
        })?;

    let username = claims.preferred_username.as_deref().unwrap_or(&claims.sub);
    let display_name = claims.name.as_deref().unwrap_or(username);
    let user = state
        .db
        .upsert_oidc_user(&claims.sub, username, display_name)
        .await?;

    let days = match state.config.auth.session_days {
        0 => DEFAULT_SESSION_DAYS,
        days => days,
    } as i64;
    let expires_at = Utc::now() + Duration::days(days);
    let token = new_token();
    state
        .db
        .create_session(&token_hash(&token), user.id, expires_at)
        .await?;
    tracing::info!(username = user.username, "app sign-in");
    Ok(Json(SessionResponse { token, expires_at }))
}

/// A short-lived credential for the two routes an `<img>` loads.
pub async fn media_token(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Json<MediaTokenResponse> {
    const TTL_SECS: u64 = 60 * 60;
    Json(MediaTokenResponse {
        token: state.media_key.mint(user.id, TTL_SECS),
        expires_at: Utc::now() + Duration::seconds(TTL_SECS as i64),
    })
}
```

If `ApiError` has no `Forbidden` variant, add one returning `403` alongside `Unauthorized` in `crates/yomu-server/src/api/error.rs`; check with `grep -n "enum ApiError" -A 20 crates/yomu-server/src/api/error.rs`.

- [ ] **Step 3: Register the routes**

In `crates/yomu-server/src/api/mod.rs`, beside the other auth routes:

```rust
        .route("/auth/exchange", axum::routing::post(auth::exchange))
        .route("/auth/media-token", get(auth::media_token))
```

- [ ] **Step 4: Write the tests**

Add to the `tests` module in `crates/yomu-server/src/api/mod.rs`:

```rust
    /// With no app_client_id there is no app sign-in to offer, and the
    /// endpoint must not look like a broken one.
    #[tokio::test]
    async fn exchange_is_absent_until_an_app_client_is_configured() {
        let db = Db::in_memory().await.unwrap();
        let mut config = Config::default();
        config.auth.issuer = Some("https://auth.example.test/".parse().unwrap());
        config.auth.client_id = "yomu".into();
        config.auth.client_secret = "secret".into();
        let state = AppState::new(config, db, Registry::default(), None);
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/exchange")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"access_token":"x"}"#))
            .unwrap();
        let status = super::router(state).oneshot(request).await.unwrap().status();
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
```

- [ ] **Step 5: Run and commit**

Run: `cargo test -p yomu-server`
Expected: PASS.

```bash
git add crates/yomu-server/src crates/yomu-domain/src
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(server): trade an IdP token for a session, and mint media tokens

/auth/exchange is how a native app signs in: it holds an authentik
access token, and gets back an opaque yomu session it can carry as a
bearer for 90 days. No refresh dance on the client, and no JWT
verification on the server.

The three failure modes stay distinct on purpose, because they need
different words on screen: expired token 401, token for another
application 403, provider unreachable 502.

/auth/media-token covers the routes an <img> loads, and is itself
authenticated — the short-lived token is a delegation of a session the
caller already has.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 6: The advertisement on `/health`

**Files:**
- Modify: `crates/yomu-domain/src/api.rs`, `crates/yomu-server/src/api/mod.rs`

- [ ] **Step 1: Extend the wire, additively**

In `crates/yomu-domain/src/api.rs`:

```rust
/// How to sign in to this server, for a client that holds no session.
/// Absent when the server runs single-account, which is how an app knows
/// to skip sign-in entirely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthAdvertisement {
    pub issuer: Url,
    /// The *public* client the native apps use, not the confidential one
    /// the browser flow uses.
    pub client_id: String,
}
```

and in `HealthResponse`:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthAdvertisement>,
```

- [ ] **Step 2: Fill it in**

`crates/yomu-server/src/api/mod.rs` — `health` needs state:

```rust
async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        commit: option_env!("YOMU_BUILD_COMMIT").map(Into::into),
        // Only when there is an app sign-in to advertise: an app pointed
        // at a server without SSO must not show a sign-in button it can
        // never satisfy.
        auth: state.config.auth.app_sign_in_enabled().then(|| {
            AuthAdvertisement {
                issuer: state.config.auth.issuer.clone().expect("checked"),
                client_id: state.config.auth.app_client_id.clone(),
            }
        }),
    })
}
```

- [ ] **Step 3: Write the tests**

```rust
    /// A server without app sign-in advertises nothing, so an app pointed
    /// at it never shows a sign-in button.
    #[tokio::test]
    async fn health_advertises_sign_in_only_when_there_is_one() {
        let body = health_body(Config::default()).await;
        assert!(body.get("auth").is_none());

        let mut config = Config::default();
        config.auth.issuer = Some("https://auth.example.test/".parse().unwrap());
        config.auth.client_id = "yomu".into();
        config.auth.client_secret = "secret".into();
        config.auth.app_client_id = "yomu-app".into();
        let body = health_body(config).await;
        assert_eq!(body["auth"]["client_id"], "yomu-app");
        // The 1.x fields are still there, in place: an old client parses
        // this response exactly as before.
        assert_eq!(body["status"], "ok");
        assert!(body["version"].is_string());
    }
```

with a helper that calls `/api/v1/health` and parses the body as
`serde_json::Value`, following the shape of `status_with_auth`.

- [ ] **Step 4: Run and commit**

Run: `cargo test -p yomu-server`

```bash
git add crates/yomu-server/src crates/yomu-domain/src
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(server,domain): health says how to sign in

An app cannot tell "you are not signed in" from "the server is
unreachable" when a proxy answers with a redirect to an origin that
sends no CORS header — a webview reports that as a network error, and
the redirect is invisible to it. One endpoint that always answers, and
that carries the issuer and client id, is what gives the question an
answer.

Nothing about the IdP is compiled into the app: it self-configures from
a server address. Additive and skipped when absent, so the frozen 1.x
wire is unchanged for existing clients.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 7: `yomu-client` carries a session

**Files:**
- Modify: `crates/yomu-client/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

```rust
    /// The client is rebuilt per call site rather than cloned once at
    /// startup, so a token that arrives later is actually used — a client
    /// captured before sign-in stays anonymous forever, and behind a
    /// proxy that failure arrives as a transport error, which the UI
    /// reads as "go offline" rather than "sign in".
    #[test]
    fn image_urls_carry_a_media_token_only_when_one_is_held() {
        let base: Url = "http://localhost:4700/".parse().unwrap();
        let id = Uuid::from_u128(1);
        let plain = YomuClient::new(base.clone());
        assert!(!plain.cover_url(id).unwrap().query().is_some());

        let with_media = YomuClient::new(base).with_media_token(Some("mt-1".into()));
        assert_eq!(
            with_media.cover_url(id).unwrap().query(),
            Some("mt=mt-1")
        );
        assert_eq!(
            with_media.page_url(id, 3).unwrap().query(),
            Some("mt=mt-1")
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p yomu-client`
Expected: FAIL — no method `with_media_token`.

- [ ] **Step 3: Implement**

In `crates/yomu-client/src/lib.rs`, add to the struct and its builder:

```rust
    /// yomu session, presented as a bearer. `None` in single-account
    /// mode, and before sign-in.
    token: Option<String>,
    /// Short-lived credential appended to image URLs, which an `<img>`
    /// loads without any header of ours.
    media_token: Option<String>,
```

```rust
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
    }

    pub fn with_media_token(mut self, token: Option<String>) -> Self {
        self.media_token = token;
        self
    }

    /// Append `?mt=` when we hold one; used by the two image URLs.
    fn media_url(&self, url: Option<Url>) -> Option<Url> {
        let mut url = url?;
        if let Some(token) = &self.media_token {
            url.query_pairs_mut().append_pair("mt", token);
        }
        Some(url)
    }
```

Every request builder gains the bearer. Find the one place requests are
constructed (`fn get`, `fn send`, `fn url`) with
`grep -n "fn get\|fn send\|self.http" crates/yomu-client/src/lib.rs` and
add, wherever a `RequestBuilder` is created:

```rust
        let req = match &self.token {
            Some(token) => req.bearer_auth(token),
            None => req,
        };
```

`cover_url` and `page_url` wrap their result in `self.media_url(...)`.

- [ ] **Step 4: Run and commit**

Run: `cargo test -p yomu-client`

```bash
git add crates/yomu-client/src/lib.rs
git -c commit.gpgsign=false commit -m "$(cat <<'MSG'
feat(client): carry a session, and sign image URLs

The client had no notion of a token at all. It now sends a bearer when
it holds one, and appends a media token to the two URLs an <img> loads,
which cannot carry a header of ours.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_011ev4TEM29CmkC2Rj1c4nTX
MSG
)"
```

---

## Task 8: Full gate

- [ ] **Step 1:** `just check` — fmt, clippy `-D warnings`, wasm.
- [ ] **Step 2:** `cargo test --workspace --exclude yomu-shell`. Existing server tests that call routes without a session will need the shared-user default (no `[auth]`), which is the pre-existing behaviour; any that configured `[auth]` and expected 200 are asserting the hole this plan closes and should be updated to sign in first.
- [ ] **Step 3:** `nix develop .#tauri --command just check-shell`.
- [ ] **Step 4:** Commit any formatting fallout.

---

## Manual verification (after deploy, before the app half)

With `[auth]` **not** configured — i.e. every existing deployment:

1. The web UI works exactly as before; no sign-in appears.
2. `curl -s localhost:4700/api/v1/health | grep -c auth` → 0.

With `[auth]` configured but no `app_client_id`:

3. `curl -s .../api/v1/library` → 401.
4. The browser flow (`/api/v1/auth/login`) still signs in and sets a cookie, and the web UI works through it.
5. `curl -s -X POST .../api/v1/auth/exchange` → 404.
