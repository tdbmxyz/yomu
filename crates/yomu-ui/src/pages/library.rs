use leptos::prelude::*;

use crate::use_client;

#[component]
pub fn Library() -> impl IntoView {
    let client = use_client();
    let library = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.library().await }
        }
    });

    view! {
        <section>
            <h2>"Library"</h2>
            {move || match library.get() {
                None => view! { <p class="muted">"Loading library…"</p> }.into_any(),
                Some(Ok(list)) if list.is_empty() => {
                    view! {
                        <p class="muted">
                            "Nothing tracked yet — use " <a href="/search">"Add manga"</a>
                            " to search a source."
                        </p>
                    }
                        .into_any()
                }
                Some(Ok(list)) => {
                    let client = use_client();
                    view! {
                        <div class="manga-grid">
                            {list
                                .into_iter()
                                .map(|entry| {
                                    let cover = client.cover_url(entry.manga.id);
                                    let read_state = entry
                                        .position
                                        .as_ref()
                                        .map(|_| "continue")
                                        .unwrap_or("start");
                                    view! {
                                        <a
                                            class="manga-card"
                                            href=format!("/manga/{}", entry.manga.id)
                                        >
                                            {cover
                                                .map(|url| {
                                                    view! {
                                                        <img
                                                            class="manga-cover"
                                                            src=url.to_string()
                                                            loading="lazy"
                                                            alt=""
                                                        />
                                                    }
                                                })}
                                            <span class="manga-title">{entry.manga.title.clone()}</span>
                                            <span class="muted manga-meta">
                                                {format!(
                                                    "{} chapter{} · {}",
                                                    entry.chapter_count,
                                                    if entry.chapter_count == 1 { "" } else { "s" },
                                                    read_state,
                                                )}
                                            </span>
                                        </a>
                                    }
                                })
                                .collect_view()}
                        </div>
                    }
                        .into_any()
                }
                Some(Err(err)) => {
                    view! { <p class="error">"Could not reach yomu server: " {err.to_string()}</p> }
                        .into_any()
                }
            }}
        </section>
    }
}
