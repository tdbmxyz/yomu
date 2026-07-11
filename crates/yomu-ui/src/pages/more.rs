//! More: theme picker, account, server details, backup/restore.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::{JsCast, JsValue};

use crate::offline::{self, Theme};
use crate::use_client;

/// Trigger a browser download of `json` as `filename`.
fn download_json(filename: &str, json: &str) -> Result<(), JsValue> {
    let parts = js_sys::Array::new();
    parts.push(&JsValue::from_str(json));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("application/json");
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let anchor = document()
        .create_element("a")?
        .dyn_into::<web_sys::HtmlAnchorElement>()?;
    anchor.set_href(&url);
    anchor.set_download(filename);
    anchor.click();
    web_sys::Url::revoke_object_url(&url)?;
    Ok(())
}

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

    // Backup: export downloads a JSON snapshot; restore reads one back and
    // merges it (additive — nothing already present is overwritten).
    let backup_status = RwSignal::new(None::<String>);
    let export = {
        let client = client.clone();
        move |_| {
            let client = client.clone();
            backup_status.set(Some("Preparing backup…".into()));
            spawn_local(async move {
                match client.backup().await {
                    Ok(backup) => match serde_json::to_string(&backup) {
                        Ok(json) => {
                            if download_json("yomu-backup.json", &json).is_ok() {
                                backup_status
                                    .set(Some(format!("Exported {} manga.", backup.manga.len())));
                            } else {
                                backup_status.set(Some("Could not start the download.".into()));
                            }
                        }
                        Err(e) => backup_status.set(Some(format!("Export failed: {e}"))),
                    },
                    Err(e) => backup_status.set(Some(format!("Export failed: {e}"))),
                }
            });
        }
    };
    let import = {
        let client = client.clone();
        move |ev: leptos::ev::Event| {
            let Some(input) = ev
                .target()
                .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
            else {
                return;
            };
            let Some(file) = input.files().and_then(|f| f.get(0)) else {
                return;
            };
            // Let the same file be re-picked after a failed attempt.
            input.set_value("");
            let client = client.clone();
            backup_status.set(Some("Restoring…".into()));
            spawn_local(async move {
                let text = match wasm_bindgen_futures::JsFuture::from(file.text()).await {
                    Ok(v) => v.as_string().unwrap_or_default(),
                    Err(_) => {
                        backup_status.set(Some("Could not read the file.".into()));
                        return;
                    }
                };
                let backup = match serde_json::from_str::<yomu_domain::Backup>(&text) {
                    Ok(b) => b,
                    Err(e) => {
                        backup_status.set(Some(format!("Not a valid backup: {e}")));
                        return;
                    }
                };
                match client.restore(&backup).await {
                    Ok(s) => backup_status.set(Some(format!(
                        "Restored {} manga, {} chapters, {} read marks.",
                        s.manga, s.chapters, s.read_marks
                    ))),
                    Err(e) => backup_status.set(Some(format!("Restore failed: {e}"))),
                }
            });
        }
    };

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

            <h3 class="shelf-title">"Backup"</h3>
            <p class="muted">
                "Export your library, reading progress and read marks as a "
                "file, or restore one. Restoring merges — nothing you already "
                "have is overwritten."
            </p>
            <div class="backup-actions">
                <button class="button" on:click=export>"Export backup"</button>
                <label class="button">
                    "Restore backup"
                    <input
                        type="file"
                        accept="application/json,.json"
                        class="visually-hidden"
                        on:change=import
                    />
                </label>
            </div>
            {move || {
                backup_status
                    .get()
                    .map(|msg| view! { <p class="muted backup-status">{msg}</p> })
            }}

            <p class="home-more">
                <a href="/about">"About yomu →"</a>
            </p>
        </section>
    }
}
