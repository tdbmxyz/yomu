//! Native authorization-code + PKCE flow for desktop/mobile shells.
//!
//! Authentik returns to a custom-scheme deep link. The shell validates the
//! state, sends the code and verifier to yomu, and stores the resulting opaque
//! yomu session outside the WebView.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

pub const REDIRECT_URI: &str = "xyz.tdbm.yomu://auth/callback";

const STORE_FILE: &str = "auth.json";
const KEY_TOKEN: &str = "session_token";
const KEY_EXPIRES: &str = "expires_at";
const KEY_PENDING: &str = "pending";
const KEY_STATUS: &str = "last_status";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pending {
    pub verifier: String,
    pub state: String,
    pub server: String,
    pub issuer: String,
    pub client_id: String,
}

#[derive(Debug, Deserialize)]
struct Discovery {
    authorization_endpoint: String,
}

#[derive(Debug, Serialize)]
struct ExchangeRequest<'a> {
    code: &'a str,
    verifier: &'a str,
}

#[derive(Debug, Deserialize)]
struct SessionResponse {
    token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct AuthStatus {
    pub token: Option<String>,
    pub status: Option<String>,
}

pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn authorize_url(endpoint: &str, pending: &Pending) -> String {
    let challenge = code_challenge(&pending.verifier);
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", &pending.client_id)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", "openid profile email")
        .append_pair("state", &pending.state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .finish();
    format!("{endpoint}?{query}")
}

pub fn parse_callback(raw: &str) -> Option<(String, String)> {
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "xyz.tdbm.yomu"
        || url.host_str() != Some("auth")
        || url.path() != "/callback"
    {
        return None;
    }
    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    Some((code?, state?))
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_default()
}

async fn discover(issuer: &str) -> Result<Discovery, String> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    http()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("cannot reach the identity provider: {e}"))?
        .error_for_status()
        .map_err(|e| format!("identity-provider discovery failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("unexpected discovery document: {e}"))
}

#[tauri::command]
pub async fn auth_start<R: Runtime>(
    app: AppHandle<R>,
    server: String,
    issuer: String,
    client_id: String,
) -> Result<String, String> {
    let discovery = discover(&issuer).await?;
    let pending = Pending {
        verifier: random_token(),
        state: random_token(),
        server,
        issuer,
        client_id,
    };
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.set(
        KEY_PENDING,
        serde_json::to_value(&pending).map_err(|e| e.to_string())?,
    );
    store.set(KEY_STATUS, "waiting for the browser to come back");
    store.save().map_err(|e| e.to_string())?;
    Ok(authorize_url(&discovery.authorization_endpoint, &pending))
}

#[tauri::command]
pub async fn auth_status<R: Runtime>(app: AppHandle<R>) -> Result<AuthStatus, String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    let _ = store.reload();
    let expires = store
        .get(KEY_EXPIRES)
        .and_then(|v| v.as_str().and_then(|s| s.parse::<DateTime<Utc>>().ok()));
    let token = if expires.is_some_and(|at| at > Utc::now()) {
        store
            .get(KEY_TOKEN)
            .and_then(|v| v.as_str().map(String::from))
    } else {
        store.delete(KEY_TOKEN);
        store.delete(KEY_EXPIRES);
        let _ = store.save();
        None
    };
    Ok(AuthStatus {
        token,
        status: store
            .get(KEY_STATUS)
            .and_then(|v| v.as_str().map(String::from)),
    })
}

pub fn record_status<R: Runtime>(app: &AppHandle<R>, status: &str) {
    if let Ok(store) = app.store(STORE_FILE) {
        store.set(KEY_STATUS, status);
        let _ = store.save();
    }
}

pub async fn finish<R: Runtime>(app: &AppHandle<R>, code: &str, state: &str) -> Result<(), String> {
    record_status(app, "callback received, exchanging it for a yomu session…");
    let result = exchange(app, code, state).await;
    match &result {
        Ok(()) => record_status(app, "signed in"),
        Err(err) => record_status(app, &format!("sign-in failed: {err}")),
    }
    result
}

async fn exchange<R: Runtime>(app: &AppHandle<R>, code: &str, state: &str) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    let pending: Pending = store
        .get(KEY_PENDING)
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or("no sign-in is in progress")?;
    if pending.state != state {
        return Err("sign-in state did not match; ignoring callback".into());
    }
    let mut server = url::Url::parse(&pending.server).map_err(|e| e.to_string())?;
    if !server.path().ends_with('/') {
        server.set_path(&format!("{}/", server.path()));
    }
    let endpoint = server
        .join("api/v1/auth/exchange")
        .map_err(|e| e.to_string())?;
    let response = http()
        .post(endpoint)
        .json(&ExchangeRequest {
            code,
            verifier: &pending.verifier,
        })
        .send()
        .await
        .map_err(|e| format!("yomu exchange request failed: {e}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("yomu rejected the exchange ({status}): {body}"));
    }
    let session: SessionResponse = response
        .json()
        .await
        .map_err(|e| format!("unexpected yomu exchange response: {e}"))?;
    store.set(KEY_TOKEN, session.token);
    store.set(KEY_EXPIRES, session.expires_at.to_rfc3339());
    store.delete(KEY_PENDING);
    store.save().map_err(|e| e.to_string())
}

fn clear<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.delete(KEY_TOKEN);
    store.delete(KEY_EXPIRES);
    store.delete(KEY_PENDING);
    store.delete(KEY_STATUS);
    store.save().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn auth_sign_out<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    clear(&app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_rfc_7636() {
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn callback_is_strict() {
        assert_eq!(
            parse_callback("xyz.tdbm.yomu://auth/callback?code=c&state=s"),
            Some(("c".into(), "s".into()))
        );
        assert_eq!(parse_callback("https://example.test/?code=c&state=s"), None);
    }
}
