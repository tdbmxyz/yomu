//! About: what's running, exactly — app and server versions with their
//! build commits, so a bug report can say which build misbehaved.

use leptos::prelude::*;

use crate::use_client;

/// "1.0.0 (2f6c29f)" — or just the version when the commit is unknown.
fn build_label(version: &str, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => format!("{version} ({commit})"),
        None => version.to_string(),
    }
}

#[component]
pub fn About() -> impl IntoView {
    let client = use_client();
    let health = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.health().await }
        }
    });
    let base = client.base().to_string();

    let app = build_label(env!("CARGO_PKG_VERSION"), option_env!("YOMU_BUILD_COMMIT"));

    view! {
        <section class="about">
            <h2>"About"</h2>

            <h3 class="shelf-title">"App"</h3>
            <p>"yomu " {app}</p>

            <h3 class="shelf-title">"Server"</h3>
            <p>
                <span class="muted">{base}</span>
                <br/>
                {move || match health.get() {
                    Some(Ok(h)) => build_label(&h.version, h.commit.as_deref()).into_any(),
                    Some(Err(_)) => view! { <span class="muted">"unreachable"</span> }.into_any(),
                    None => view! { <span class="muted">"checking…"</span> }.into_any(),
                }}
            </p>

            <h3 class="shelf-title">"Project"</h3>
            <p>
                <a href="https://github.com/tdbmxyz/yomu" rel="external" target="_blank">
                    "github.com/tdbmxyz/yomu"
                </a>
                <br/>
                <span class="muted">
                    "AGPL-3.0-or-later. A self-hosted reader: it ships no content "
                    "and no site-specific source definitions."
                </span>
            </p>
        </section>
    }
}
