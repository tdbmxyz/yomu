//! WebView side of native sign-in. Long-lived credentials stay in the Tauri
//! store; only Yomu's opaque session is mirrored here so clients can attach it
//! synchronously when they are built.

use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;
use yomu_domain::AuthAdvertisement;

const SESSION_KEY: &str = "yomu-session";
const MEDIA_KEY: &str = "yomu-media-token";

#[derive(Clone, Copy)]
pub(crate) struct AuthContext {
    pub advertisement: RwSignal<Option<AuthAdvertisement>>,
    pub status: RwSignal<Option<String>>,
}

fn storage_get(key: &str) -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .get_item(key)
        .ok()?
        .filter(|value| !value.is_empty())
}

fn storage_set(key: &str, value: Option<&str>) {
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return;
    };
    match value {
        Some(value) if !value.is_empty() => {
            let _ = storage.set_item(key, value);
        }
        _ => {
            let _ = storage.remove_item(key);
        }
    }
}

pub(crate) fn session_token() -> Option<String> {
    storage_get(SESSION_KEY)
}

pub(crate) fn media_token() -> Option<String> {
    storage_get(MEDIA_KEY)
}

pub(crate) fn clear_local() {
    storage_set(SESSION_KEY, None);
    storage_set(MEDIA_KEY, None);
}

/// Mirror the shell's durable session into WebView storage. Returns true when
/// a usable session exists afterwards.
pub(crate) async fn sync_status(status_signal: RwSignal<Option<String>>, server: &str) -> bool {
    let args = js_sys::Object::new();
    match crate::offline::shell_invoke("auth_status", args).await {
        Ok(value) => {
            let token = js_sys::Reflect::get(&value, &"token".into())
                .ok()
                .and_then(|v| v.as_string());
            let status = js_sys::Reflect::get(&value, &"status".into())
                .ok()
                .and_then(|v| v.as_string());
            storage_set(SESSION_KEY, token.as_deref());
            status_signal.set(status);
            if let Some(token) = token {
                refresh_media_token(server, &token).await;
                true
            } else {
                storage_set(MEDIA_KEY, None);
                false
            }
        }
        Err(err) => {
            status_signal.set(Some(err));
            session_token().is_some()
        }
    }
}

async fn refresh_media_token(server: &str, token: &str) {
    let Ok(base) = url::Url::parse(server) else {
        return;
    };
    let client = yomu_client::YomuClient::new(base).with_token(Some(token.to_string()));
    if let Ok(response) = client.media_token().await {
        storage_set(MEDIA_KEY, Some(&response.token));
    }
}

pub(crate) async fn start_sign_in(
    server: &str,
    auth: &AuthAdvertisement,
    status: RwSignal<Option<String>>,
) -> Result<(), String> {
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &"server".into(), &server.into());
    let _ = js_sys::Reflect::set(&args, &"issuer".into(), &auth.issuer.to_string().into());
    let _ = js_sys::Reflect::set(&args, &"clientId".into(), &auth.client_id.clone().into());
    let url = crate::offline::shell_invoke("auth_start", args)
        .await?
        .as_string()
        .ok_or("the shell returned no authorization URL")?;
    status.set(Some("waiting for the browser to come back".into()));
    open_external(&url).await
}

async fn open_external(url: &str) -> Result<(), String> {
    // Android's JS bridge launches an ACTION_VIEW intent. Tauri commands run
    // off the activity thread and are not a reliable system-browser opener.
    if let Some(window) = web_sys::window()
        && let Ok(bridge) = js_sys::Reflect::get(&window, &"YomuAndroid".into())
        && let Ok(method) = js_sys::Reflect::get(&bridge, &"openUrl".into())
        && let Ok(method) = method.dyn_into::<js_sys::Function>()
    {
        method
            .call1(&bridge, &url.into())
            .map_err(|e| format!("{e:?}"))?;
        return Ok(());
    }
    let args = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&args, &"url".into(), &url.into());
    crate::offline::shell_invoke("open_external", args)
        .await
        .map(|_| ())
}

pub(crate) async fn sign_out() {
    let _ = crate::offline::shell_invoke("auth_sign_out", js_sys::Object::new()).await;
    clear_local();
}

pub(crate) fn reload() {
    if let Some(window) = web_sys::window() {
        let _ = window.location().reload();
    }
}
