//! Explicit recovery and non-destructive browser storage maintenance.
use crate::offline::{self, context};
use leptos::prelude::*;

#[component]
pub(super) fn OfflineStorage() -> impl IntoView {
    let status = RwSignal::new(None::<String>);
    let legacy = LocalResource::new(|| async {
        context::invoke("legacy", &js_sys::Array::new())
            .await
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    });
    let report = move |_| {
        leptos::task::spawn_local(async move {
            status.set(Some(
                match context::invoke("report", &js_sys::Array::new()).await {
                    Ok(value) => value.as_string().unwrap_or_default(),
                    Err(error) => error,
                },
            ));
        })
    };
    let clear_runtime = move |_| {
        leptos::task::spawn_local(async move {
            status.set(Some(match context::worker("runtime-clear", uuid::Uuid::nil(), None, None).await {
            Ok(_) => "Disposable cache cleared. Saved chapters and pending history were retained.".into(), Err(error) => error,
        }));
        })
    };
    let import = move |_| {
        if !web_sys::window().is_some_and(|window| window.confirm_with_message(
            "Import legacy offline history and downloads into THIS account on THIS server? Only continue if they belong to you. Original data will be retained."
        ).unwrap_or(false)) { return; }
        leptos::task::spawn_local(async move {
            match context::invoke("importLegacy", &js_sys::Array::new()).await {
                Ok(_) => crate::auth::reload(),
                Err(error) => status.set(Some(error)),
            }
        });
    };
    let export = move |_| {
        leptos::task::spawn_local(async move {
            let result = async {
                let json = context::invoke("exportRetained", &js_sys::Array::new())
                    .await?
                    .as_string()
                    .ok_or("Invalid offline export")?;
                super::more::download_json("yomu-offline-state.json", &json).await
            }
            .await;
            status.set(Some(match result {
                Ok(true) => {
                    "Offline state exported. Keep it private; it includes retained reading history."
                        .into()
                }
                Ok(false) => "Export cancelled.".into(),
                Err(error) => error,
            }));
        })
    };
    view! {
        <h3 class="shelf-title">"Offline storage"</h3>
        <p class="muted">"Pending history is retained by server and account. Signing out removes that account's browser-cached content, but not its pending history. Sign back into the same account to sync it."</p>
        {(!offline::shell_available()).then(|| view! {
            <div class="backup-actions">
                <button class="button" on:click=report>"Check browser storage"</button>
                <button class="button" on:click=clear_runtime>"Clear disposable cache"</button>
            </div>
        })}
        <button class="button" on:click=export>"Export retained offline state"</button>
        {move || legacy.get().unwrap_or(false).then(|| view! {
            <p class="muted">"Legacy offline data has no recorded owner. It has been kept unchanged and will not sync automatically. Export it before recovery; only import into its original server and account."</p>
            <button class="button" on:click=import>"Import legacy offline data into this account"</button>
        })}
        {move || status.get().map(|message| view! { <p role="status">{message}</p> })}
    }
}
