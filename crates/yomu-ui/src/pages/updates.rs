//! Recent updater discoveries, newest first.

use leptos::prelude::*;

use crate::use_client;

#[component]
pub fn Updates() -> impl IntoView {
    let client = use_client();
    let updates = LocalResource::new(move || {
        let client = client.clone();
        async move { client.recent_updates().await }
    });

    view! {
        <section class="updates-page">
            <h2>"Recent updates"</h2>
            <p class="muted">"New units found in the last 30 days."</p>
            {move || match updates.get() {
                None => view! { <p class="muted">"Loading…"</p> }.into_any(),
                Some(Err(err)) => view! { <p class="error">{err.to_string()}</p> }.into_any(),
                Some(Ok(feed)) if feed.updates.is_empty() => {
                    view! { <p class="muted">"No recent updates."</p> }.into_any()
                }
                Some(Ok(feed)) => view! {
                    <div class="updates-list">
                        {feed.updates.into_iter().map(|event| {
                            let title = event.publication_title;
                            let units = if event.unit_count == 1 {
                                event.first_title
                            } else {
                                format!("{} new units: {} – {}", event.unit_count, event.first_title, event.last_title)
                            };
                            let when = crate::format::published_label(event.created_at, chrono::Utc::now());
                            let href = format!("/publications/{}", event.publication_id);
                            view! {
                                <a class="update-row" href=href>
                                    <strong>{title}</strong>
                                    <span>{units}</span>
                                    <time class="muted">{when}</time>
                                </a>
                            }
                        }).collect_view()}
                    </div>
                }.into_any(),
            }}
        </section>
    }
}
