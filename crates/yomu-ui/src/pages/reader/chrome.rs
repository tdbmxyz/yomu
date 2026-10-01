//! Shared reader shell and navigation chrome. Format navigators own content,
//! not their own fullscreen/back-button/immersive-mode implementations.
use leptos::prelude::*;

use crate::offline;

#[component]
pub(super) fn ReaderShell(
    chrome: RwSignal<bool>,
    #[prop(into)] flow: Signal<bool>,
    #[prop(optional)] publication: bool,
    children: Children,
) -> impl IntoView {
    offline::set_reading(true);
    Effect::new(move |_| offline::set_immersive(!chrome.get()));
    let keyboard = window_event_listener(leptos::ev::keydown, move |event| {
        if event.key() == "Escape" {
            chrome.set(true);
        }
    });
    on_cleanup(move || {
        keyboard.remove();
        offline::set_immersive(false);
        offline::set_reading(false);
    });
    view! {
        <div class="reader-overlay" class:chrome-hidden=move || !chrome.get()
            class:flow=move || flow.get() class:publication-reader=publication>
            {children()}
        </div>
    }
}

#[component]
pub(super) fn ReaderTop(publication_id: uuid::Uuid, children: Children) -> impl IntoView {
    view! {
        <header class="reader-chrome reader-top">
            <a class="reader-back" href=format!("/publications/{publication_id}")
                aria-label="Back to publication" title="Back to publication">
                <svg viewBox="0 0 24 24" aria-hidden="true" width="20" height="20">
                    <path d="m14 6-6 6 6 6" fill="none" stroke="currentColor"
                        stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
                <span>"Back"</span>
            </a>
            <div class="reader-heading">{children()}</div>
        </header>
    }
}
