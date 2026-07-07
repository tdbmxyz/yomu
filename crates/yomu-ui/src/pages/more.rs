//! More: theme picker, account, server details.

use leptos::prelude::*;

use crate::offline::{self, Theme};
use crate::use_client;

#[component]
pub fn More() -> impl IntoView {
    let current = RwSignal::new(offline::theme());
    let pick = move |theme: Theme| {
        offline::set_theme(theme);
        current.set(theme);
    };

    let client = use_client();
    let health = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.health().await }
        }
    });
    let base = client.base().to_string();

    view! {
        <section class="more">
            <h2>"Settings"</h2>

            <h3 class="shelf-title">"Theme"</h3>
            <div class="theme-grid">
                {Theme::ALL
                    .into_iter()
                    .map(|theme| {
                        view! {
                            <button
                                class="theme-choice"
                                class:active=move || current.get() == theme
                                data-swatch=theme.key()
                                on:click=move |_| pick(theme)
                            >
                                <span class="swatch">
                                    <span class="swatch-accent"></span>
                                </span>
                                {theme.name()}
                            </button>
                        }
                    })
                    .collect_view()}
            </div>

            <h3 class="shelf-title">"Account"</h3>
            <p><crate::Account/></p>

            <h3 class="shelf-title">"Server"</h3>
            <p class="muted">
                {base} {" · "}
                {move || match health.get() {
                    Some(Ok(h)) => format!("yomu {} · {}", h.version, h.status),
                    Some(Err(_)) => "unreachable".into(),
                    None => "checking…".into(),
                }}
            </p>
            <crate::ConnectForm/>
        </section>
    }
}
