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
                <span class="grow"></span>
                <Account/>
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

/// Topbar account widget. Only relevant when the server runs an OIDC
/// provider: single-account mode shows nothing (there is no one to sign
/// in as). Sign-in is a full-page redirect through the provider.
#[component]
fn Account() -> impl IntoView {
    let client = use_client();
    let me = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.me().await }
        }
    });

    let logout_client = client.clone();
    let sign_out = move |_| {
        let client = logout_client.clone();
        spawn_local(async move {
            let _ = client.logout().await;
            if let Some(window) = web_sys::window() {
                let _ = window.location().set_href("/");
            }
        });
    };

    view! {
        {move || {
            let sign_out = sign_out.clone();
            let me = me.get().and_then(|r| r.ok())?;
            match (me.mode, me.user) {
                (yomu_domain::AuthMode::Oidc, Some(user)) => {
                    Some(
                        view! {
                            <span class="account">
                                <span class="muted">{user.display_name}</span>
                                <button class="account-btn" on:click=sign_out>
                                    "sign out"
                                </button>
                            </span>
                        }
                            .into_any(),
                    )
                }
                (yomu_domain::AuthMode::Oidc, None) => {
                    // Full document navigation (the endpoint 302s to the
                    // provider), against the API base so a desktop shell
                    // pointing at a remote server signs in there.
                    let href = use_client()
                        .base()
                        .join("api/v1/auth/login")
                        .map(|u| u.to_string())
                        .unwrap_or_else(|_| "/api/v1/auth/login".into());
                    Some(
                        view! {
                            <a class="button" rel="external" href=href>
                                "Sign in"
                            </a>
                        }
                            .into_any(),
                    )
                }
                (yomu_domain::AuthMode::Single, _) => None,
            }
        }}
    }
}
