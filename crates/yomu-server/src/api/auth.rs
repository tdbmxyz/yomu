//! `/api/v1/auth/*`: OIDC sign-in (redirect flow), sign-out, whoami.
//!
//! With no `[auth]` configured there is nothing to sign into: `me` reports
//! the shared account in `single` mode and login/callback answer 404.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{Duration, Utc};
use serde::Deserialize;
use yomu_domain::{AuthMode, MeResponse};

use super::ApiError;
use crate::auth::{
    DEFAULT_SESSION_DAYS, OptionalUser, SESSION_COOKIE, new_token, request_token, token_hash,
};
use crate::oidc::OidcRuntime;
use crate::state::AppState;

pub async fn me(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
) -> Json<MeResponse> {
    let mode = if state.config.auth.oidc_enabled() {
        AuthMode::Oidc
    } else {
        AuthMode::Single
    };
    Json(MeResponse { mode, user })
}

/// Browser entrypoint: 302 to the identity provider.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let oidc = require_oidc(&state)?;
    let url = oidc
        .begin_login(&callback_uri(&state, &headers))
        .await
        .map_err(ApiError::UpstreamFailed)?;
    Ok(Redirect::temporary(url.as_str()).into_response())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: String,
    state: String,
}

/// The provider redirects here; on success the browser lands back on the
/// app with a session cookie set.
pub async fn callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let oidc = require_oidc(&state)?;
    let claims = oidc
        .complete_login(&query.state, &query.code, &callback_uri(&state, &headers))
        .await
        .map_err(ApiError::UpstreamFailed)?;

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
    let token = new_token();
    state
        .db
        .create_session(
            &token_hash(&token),
            user.id,
            Utc::now() + Duration::days(days),
        )
        .await?;
    tracing::info!(username = user.username, "oidc login");

    Ok((
        session_cookie_headers(&state, &token, days * 24 * 60 * 60),
        Redirect::temporary("/"),
    )
        .into_response())
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = request_token(&headers) {
        let _ = state.db.delete_session(&token_hash(&token)).await;
    }
    // Expire the cookie either way.
    (
        session_cookie_headers(&state, "", 0),
        Json(serde_json::json!({})),
    )
        .into_response()
}

fn require_oidc(state: &AppState) -> Result<&OidcRuntime, ApiError> {
    state.oidc.as_deref().ok_or(ApiError::NotFound)
}

/// The redirect URI registered with the provider: `<public_url>` when
/// configured, otherwise derived from the request (reverse-proxy aware).
fn callback_uri(state: &AppState, headers: &HeaderMap) -> String {
    let origin = match &state.config.auth.public_url {
        Some(url) => url.as_str().trim_end_matches('/').to_string(),
        None => {
            let get = |name: &str| {
                headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            let scheme = get("x-forwarded-proto").unwrap_or_else(|| "http".into());
            let host = get("x-forwarded-host")
                .or_else(|| get("host"))
                .unwrap_or_else(|| state.config.listen.to_string());
            format!("{scheme}://{host}")
        }
    };
    format!("{origin}/api/v1/auth/callback")
}

fn session_cookie_headers(state: &AppState, token: &str, max_age_secs: i64) -> HeaderMap {
    let secure = state
        .config
        .auth
        .public_url
        .as_ref()
        .is_some_and(|u| u.scheme() == "https");
    let cookie = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{}",
        if secure { "; Secure" } else { "" }
    );
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        headers.insert(header::SET_COOKIE, value);
    }
    headers
}
