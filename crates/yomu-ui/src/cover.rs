//! Manga cover that survives offline in the shells. Online it renders the
//! server's cover route (on the web the service worker caches it); offline
//! in a shell it renders the device-saved copy over the `yomudev` protocol
//! (see the library page's cover sweep). Whatever errors falls back to the
//! empty-cover placeholder instead of a broken image.

use leptos::prelude::*;

use crate::{Connectivity, offline, use_client, use_connectivity};

#[component]
pub fn Cover(manga_id: uuid::Uuid, #[prop(optional)] large: bool) -> impl IntoView {
    let conn = use_connectivity();
    let server = use_client().cover_url(manga_id).map(|u| u.to_string());
    let failed = RwSignal::new(false);
    // A connectivity flip changes the source: give it a fresh attempt.
    Effect::new(move |_| {
        conn.track();
        failed.set(false);
    });
    let class = if large {
        "manga-cover large"
    } else {
        "manga-cover"
    };
    let empty = if large {
        "manga-cover large cover-empty"
    } else {
        "manga-cover cover-empty"
    };
    let src = move || -> Option<String> {
        if conn.get() != Connectivity::Online
            && offline::shell_available()
            && offline::device_cover_saved(manga_id)
            && let Some(url) = offline::shell_cover_url(manga_id)
        {
            return Some(url);
        }
        server.clone()
    };
    view! {
        {move || match (failed.get(), src()) {
            (false, Some(url)) => view! {
                <img
                    class=class
                    src=url
                    loading="lazy"
                    alt=""
                    on:error=move |_| failed.set(true)
                />
            }
                .into_any(),
            _ => view! { <span class=empty></span> }.into_any(),
        }}
    }
}
