//! Manga cover that survives offline in the shells. Online it renders the
//! server's cover route (on the web the service worker caches it); offline
//! in a shell it renders the device-saved copy over the `yomudev` protocol
//! (see the library page's cover sweep). Whatever errors falls back to the
//! empty-cover placeholder instead of a broken image.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::{Connectivity, offline, use_client, use_connectivity};

thread_local! {
    static SWEEPING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Save any of these manga's covers that device storage is missing —
/// no-op outside the shells, while offline, or while a sweep is already
/// running. Called by the pages that load the library (Home, Library), so
/// covers are stored no matter where the app lands first.
pub fn sweep_device_covers(
    conn: RwSignal<Connectivity>,
    client: &yomu_client::YomuClient,
    ids: Vec<uuid::Uuid>,
) {
    if conn.get_untracked() != Connectivity::Online || !offline::shell_available() {
        return;
    }
    if ids.is_empty() || SWEEPING.get() {
        return;
    }
    SWEEPING.set(true);
    let client = client.clone();
    spawn_local(async move {
        // the shell short-circuits covers it already stores
        for id in ids {
            if let Err(err) = offline::shell_save_cover(&client, id).await {
                leptos::logging::warn!("cover save failed for {id}: {err}");
            }
        }
        SWEEPING.set(false);
    });
}

/// Covers from search/catalog only use the authenticated Yomu proxy. A
/// refused or undecodable image leaves the same-sized placeholder behind.
#[component]
pub fn SourceCover(path: Option<String>, #[prop(optional)] large: bool) -> impl IntoView {
    let client = use_client();
    let media = crate::media::use_media();
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
    view! {
        {move || {
            let client = client.clone().with_media_token(media.token.get());
            match path.as_ref().and_then(|path| client.source_cover_url(path)) {
                Some(src) => {
                    let failed = RwSignal::new(false);
                    view! {
                        <img class=class src=src.to_string() alt="" loading=if large { "eager" } else { "lazy" }
                            decoding="async" hidden=move || failed.get() on:error=move |_| failed.set(true)/>
                        <span class=empty aria-hidden="true" hidden=move || !failed.get()></span>
                    }.into_any()
                }
                None => view! { <span class=empty aria-hidden="true"></span> }.into_any(),
            }
        }}
    }
}

#[component]
pub fn Cover(manga_id: uuid::Uuid, #[prop(optional)] large: bool) -> impl IntoView {
    let conn = use_connectivity();
    let server = use_client().cover_url(manga_id).map(|u| u.to_string());
    let failed = RwSignal::new(false);
    let device_failed = RwSignal::new(false);
    // A connectivity flip changes the source: give the remote fallback a
    // fresh attempt; a later navigation will try a newly saved device copy.
    Effect::new(move |_| {
        conn.track();
        failed.set(false);
        device_failed.set(false);
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
        // Device copies are the shell's real cover cache. Prefer them even
        // online: otherwise every navigation recreates a remote image fetch.
        if !device_failed.get()
            && offline::shell_available()
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
                    on:error=move |_| {
                        if offline::shell_available() && !device_failed.get_untracked() {
                            device_failed.set(true);
                        } else {
                            failed.set(true);
                        }
                    }
                />
            }
                .into_any(),
            _ => view! { <span class=empty></span> }.into_any(),
        }}
    }
}
