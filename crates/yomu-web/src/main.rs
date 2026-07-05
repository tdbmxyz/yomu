use leptos::prelude::*;
use url::Url;
use yomu_ui::{App, AppConfig};

/// Where the yomu server lives, resolved in order:
///
/// 1. `window.YOMU_API_BASE` — set by a hosting shell (Tauri injects it, a
///    reverse proxy can inline a `<script>`); wins over everything.
/// 2. `localStorage["yomu-api-base"]` — user override, survives reloads.
/// 3. The page origin, when it's http(s) — the served-by-yomu-server case
///    (and trunk's dev proxy). Note Windows Tauri uses `https://tauri.localhost`,
///    which is *not* the API — shells must set YOMU_API_BASE.
/// 4. The default local server, as a last resort for non-http origins.
fn api_base() -> Url {
    let fallback = Url::parse("http://127.0.0.1:4700").expect("valid fallback url");
    let Some(window) = web_sys::window() else {
        return fallback;
    };

    let global = js_sys::Reflect::get(&window, &"YOMU_API_BASE".into())
        .ok()
        .and_then(|v| v.as_string());
    let stored = window
        .local_storage()
        .ok()
        .flatten()
        .and_then(|s| s.get_item("yomu-api-base").ok().flatten());
    if let Some(url) = [global, stored]
        .into_iter()
        .flatten()
        .find_map(|raw| Url::parse(&raw).ok())
    {
        return url;
    }

    // `tauri.localhost` is the app bundle's origin, not the server — only
    // trust the origin when it can actually be the yomu server.
    match window.location().origin().ok().map(|o| Url::parse(&o)) {
        Some(Ok(url))
            if (url.scheme() == "http" || url.scheme() == "https")
                && url.host_str() != Some("tauri.localhost") =>
        {
            url
        }
        _ => fallback,
    }
}

fn main() {
    console_error_panic_hook::set_once();

    // Offline support: the service worker caches the app shell, page images
    // and API responses (see sw.js). Registered from index.html so it's in
    // place before this bundle runs; this is a fallback for exotic loads.
    if let Some(window) = web_sys::window() {
        let sw = window.navigator().service_worker();
        if !sw.is_undefined() {
            let _ = sw.register("/sw.js");
        }
    }

    let config = AppConfig {
        api_base: api_base(),
    };
    mount_to_body(move || view! { <App config=config.clone()/> });
}
