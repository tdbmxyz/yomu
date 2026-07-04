//! Shared Leptos UI, mounted by the web bundle (and later by a desktop
//! shell). Platform specifics are injected via [`AppConfig`], same seam as
//! chaos.

mod pages;

use leptos::prelude::*;
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
    provide_context(config);

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
