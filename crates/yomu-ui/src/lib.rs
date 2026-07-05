//! Shared Leptos UI, mounted by the web bundle (and later by a desktop
//! shell). Platform specifics are injected via [`AppConfig`], same seam as
//! chaos.

pub mod offline;
mod pages;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{A, Route, Router, Routes};
use leptos_router::path;
use url::Url;
use yomu_client::YomuClient;

#[derive(Clone)]
pub struct AppConfig {
    pub api_base: Url,
}

pub fn use_client() -> YomuClient {
    let config = use_context::<AppConfig>().expect("AppConfig provided by the shell");
    YomuClient::new(config.api_base)
}

#[component]
pub fn App(config: AppConfig) -> impl IntoView {
    provide_context(config.clone());

    // Sync any progress recorded while offline: once at startup, and again
    // whenever the browser reports connectivity is back.
    let flush_client = YomuClient::new(config.api_base.clone());
    spawn_local({
        let client = flush_client.clone();
        async move { offline::flush_outbox(&client).await }
    });
    let online_handle = window_event_listener(leptos::ev::online, move |_| {
        let client = flush_client.clone();
        spawn_local(async move { offline::flush_outbox(&client).await });
    });
    on_cleanup(move || online_handle.remove());

    view! {
        <Router>
            <nav class="topbar">
                <span class="brand">"yomu"</span>
                <A href="/">"Library"</A>
                <A href="/search">"Add manga"</A>
            </nav>
            <main>
                <Routes fallback=|| view! { <p class="muted">"Page not found"</p> }>
                    <Route path=path!("/") view=pages::Library/>
                    <Route path=path!("/search") view=pages::Search/>
                    <Route path=path!("/manga/:id") view=pages::MangaPage/>
                    <Route path=path!("/read/:manga/:chapter") view=pages::Reader/>
                </Routes>
            </main>
        </Router>
    }
}
