use leptos::prelude::*;
use url::Url;
use yomu_ui::{App, AppConfig};

/// Same-origin API in the browser (trunk proxies /api in dev); fall back to
/// the default local server under non-http origins (future desktop shell).
fn api_base() -> Url {
    let fallback = Url::parse("http://127.0.0.1:4700").expect("valid fallback url");
    let Some(origin) = web_sys::window().and_then(|w| w.location().origin().ok()) else {
        return fallback;
    };
    match Url::parse(&origin) {
        Ok(url) if url.scheme() == "http" || url.scheme() == "https" => url,
        _ => fallback,
    }
}

fn main() {
    console_error_panic_hook::set_once();

    // Offline support: the service worker caches the app shell, page images
    // and API responses (see sw.js). Registration is fire-and-forget.
    if let Some(window) = web_sys::window() {
        let _ = window.navigator().service_worker().register("/sw.js");
    }

    let config = AppConfig {
        api_base: api_base(),
    };
    mount_to_body(move || view! { <App config=config.clone()/> });
}
