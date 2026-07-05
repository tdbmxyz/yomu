//! Users and sessions.
//!
//! Same identity/session split as chaos: a *session* is an opaque bearer
//! token / HttpOnly cookie presented on every request; *identity* is how a
//! session is minted. yomu has no passwords at all — identity is either the
//! OIDC provider (authentik) when `[auth]` is configured, or the built-in
//! shared account when it isn't (one central account for everyone; same
//! tracking for all readers).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
}

/// How this server authenticates (told to clients via `GET /auth/me`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// No IdP configured: everyone is the shared account, no login UI.
    Single,
    /// OIDC provider configured: sign in via `GET /auth/login` (redirect).
    Oidc,
}

/// `GET /auth/me` — never a 401: `user` is `None` when signed out (only
/// possible in [`AuthMode::Oidc`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeResponse {
    pub mode: AuthMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
}
