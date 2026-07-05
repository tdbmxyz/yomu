//! OIDC login against authentik (or any standard provider).
//!
//! Authorization-code flow with PKCE; the callback exchanges the code and
//! reads the **userinfo endpoint** (no JWT validation needed — the answer
//! comes straight from the provider over TLS). A successful callback upserts
//! the user by `sub` and mints a plain yomu session (`auth.rs`) — nothing
//! downstream knows where a session came from.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::auth::new_token;
use crate::config::AuthConfig;

/// A login begun but not yet called back; expires quickly.
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);

pub struct OidcRuntime {
    issuer: Url,
    client_id: String,
    client_secret: String,
    discovery: tokio::sync::OnceCell<Discovery>,
    pending: Mutex<HashMap<String, Pending>>,
    http: reqwest::Client,
}

struct Pending {
    verifier: String,
    created_at: Instant,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    authorization_endpoint: Url,
    token_endpoint: Url,
    userinfo_endpoint: Url,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// Claims yomu cares about, via the userinfo endpoint.
#[derive(Debug, Deserialize)]
pub struct UserInfo {
    pub sub: String,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

impl OidcRuntime {
    /// `None` when `[auth]` has no issuer — single-account mode.
    pub fn from_config(config: &AuthConfig) -> anyhow::Result<Option<Self>> {
        let Some(issuer) = &config.issuer else {
            return Ok(None);
        };
        if config.client_id.is_empty() || config.client_secret.is_empty() {
            anyhow::bail!("[auth] issuer is set but client_id/client_secret are empty");
        }
        Ok(Some(Self {
            issuer: issuer.clone(),
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            discovery: tokio::sync::OnceCell::new(),
            pending: Mutex::new(HashMap::new()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
        }))
    }

    /// Provider endpoints, fetched once (lazily — the server must boot even
    /// when the IdP is down).
    async fn discovery(&self) -> Result<&Discovery, String> {
        self.discovery
            .get_or_try_init(|| async {
                let url = format!(
                    "{}/.well-known/openid-configuration",
                    self.issuer.as_str().trim_end_matches('/')
                );
                self.http
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| format!("fetching {url}: {e}"))?
                    .error_for_status()
                    .map_err(|e| format!("fetching {url}: {e}"))?
                    .json::<Discovery>()
                    .await
                    .map_err(|e| format!("parsing {url}: {e}"))
            })
            .await
    }

    /// Start a login: returns the provider URL to redirect the browser to.
    pub async fn begin_login(&self, redirect_uri: &str) -> Result<Url, String> {
        let discovery = self.discovery().await?;
        let state = new_token();
        let verifier = new_token(); // 64 hex chars — valid PKCE charset
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));

        {
            let mut pending = self.pending.lock().expect("pending lock");
            pending.retain(|_, p| p.created_at.elapsed() < PENDING_TTL);
            pending.insert(
                state.clone(),
                Pending {
                    verifier,
                    created_at: Instant::now(),
                },
            );
        }

        let mut url = discovery.authorization_endpoint.clone();
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", "openid profile")
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url)
    }

    /// Finish a login: validate `state`, exchange the code, read userinfo.
    pub async fn complete_login(
        &self,
        state: &str,
        code: &str,
        redirect_uri: &str,
    ) -> Result<UserInfo, String> {
        let verifier = {
            let mut pending = self.pending.lock().expect("pending lock");
            let entry = pending
                .remove(state)
                .ok_or("unknown or expired login state")?;
            if entry.created_at.elapsed() >= PENDING_TTL {
                return Err("login took too long, try again".into());
            }
            entry.verifier
        };

        let discovery = self.discovery().await?;
        let token: TokenResponse = self
            .http
            .post(discovery.token_endpoint.clone())
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("code_verifier", &verifier),
            ])
            .send()
            .await
            .map_err(|e| format!("token exchange: {e}"))?
            .error_for_status()
            .map_err(|e| format!("token exchange: {e}"))?
            .json()
            .await
            .map_err(|e| format!("token exchange response: {e}"))?;

        self.http
            .get(discovery.userinfo_endpoint.clone())
            .bearer_auth(&token.access_token)
            .send()
            .await
            .map_err(|e| format!("userinfo: {e}"))?
            .error_for_status()
            .map_err(|e| format!("userinfo: {e}"))?
            .json()
            .await
            .map_err(|e| format!("userinfo response: {e}"))
    }
}
