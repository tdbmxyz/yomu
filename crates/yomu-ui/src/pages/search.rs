//! Search: one search box across every source at once. Query-less catalog
//! browsing lives in the Sources tab (see `sources.rs`).

use leptos::prelude::*;
use leptos::task::spawn_local;
use yomu_domain::{AddPublicationRequest, MangaSummary};

use crate::use_client;

#[component]
pub fn Search() -> impl IntoView {
    let client = use_client();
    let query = RwSignal::new(String::new());
    // Query of the search currently displayed; None = browse mode.
    let submitted = RwSignal::new(None::<String>);
    let status = RwSignal::new(None::<String>);

    let sources = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.sources().await }
        }
    });
    let results = LocalResource::new({
        let client = client.clone();
        move || {
            let submitted = submitted.get();
            let client = client.clone();
            async move {
                match submitted {
                    Some(q) => client.search_all(&q).await.map(Some),
                    None => Ok(None),
                }
            }
        }
    });

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let q = query.get_untracked().trim().to_string();
        if q.is_empty() {
            return;
        }
        status.set(None);
        submitted.set(Some(q));
    };

    view! {
        <section class="browse">
            <h2>"Search"</h2>
            <form class="search-form" on:submit=submit>
                <input
                    type="search"
                    class="grow"
                    placeholder="Search every source…"
                    prop:value=query
                    on:input=move |ev| query.set(event_target_value(&ev))
                />
                <button type="submit" class="primary">
                    "Search"
                </button>
                {move || {
                    submitted
                        .get()
                        .map(|_| {
                            view! {
                                <button
                                    type="button"
                                    on:click=move |_| {
                                        submitted.set(None);
                                        query.set(String::new());
                                    }
                                >
                                    "Clear"
                                </button>
                            }
                        })
                }}
            </form>
            {move || status.get().map(|s| view! { <p class="status">{s}</p> })}

            {move || match results.get() {
                Some(Ok(Some(groups))) => {
                    view! {
                        {groups
                            .into_iter()
                            .map(|group| {
                                view! {
                                    <div class="source-group">
                                        <h3 class="shelf-title">{group.source_name.clone()}</h3>
                                        {match group.error {
                                            Some(err) => {
                                                view! { <p class="error">{err}</p> }.into_any()
                                            }
                                            None if group.results.is_empty() => {
                                                view! { <p class="muted">"No results."</p> }
                                                    .into_any()
                                            }
                                            None => {
                                                view! {
                                                    <SummaryGrid
                                                        items=group.results
                                                        source=group.source_id
                                                        status
                                                    />
                                                }
                                                    .into_any()
                                            }
                                        }}
                                    </div>
                                }
                            })
                            .collect_view()}
                    }
                        .into_any()
                }
                Some(Err(err)) => view! { <p class="error">{err.to_string()}</p> }.into_any(),
                Some(Ok(None)) => {
                    // nothing searched yet
                    match sources.get() {
                        Some(Ok(list)) if list.is_empty() => {
                            view! {
                                <p class="muted">
                                    "No sources configured. Drop a *.toml definition in the server's "
                                    <code>"sources.d/"</code>
                                    " directory (see the sample file) and restart."
                                </p>
                            }
                                .into_any()
                        }
                        Some(Ok(_)) => {
                            view! {
                                <p class="muted">
                                    "Search every source at once, or browse their catalogs from the "
                                    <a href="/sources">"Sources"</a> " tab."
                                </p>
                            }
                                .into_any()
                        }
                        Some(Err(err)) => {
                            view! { <p class="error">{err.to_string()}</p> }.into_any()
                        }
                        None => view! { <p class="muted">"Loading sources…"</p> }.into_any(),
                    }
                }
                None => view! { <p class="muted">"Searching every source…"</p> }.into_any(),
            }}
        </section>
    }
}

/// Cover grid of source results, each with track actions. Shared with the
/// Sources tab's catalog page.
#[component]
pub(crate) fn SummaryGrid(
    items: Vec<MangaSummary>,
    source: String,
    status: RwSignal<Option<String>>,
) -> impl IntoView {
    view! {
        <div class="manga-grid browse-grid">
            {items
                .into_iter()
                .map(|hit| view! { <SummaryCard hit source=source.clone() status/> })
                .collect_view()}
        </div>
    }
}

#[component]
fn SummaryCard(
    hit: MangaSummary,
    source: String,
    status: RwSignal<Option<String>>,
) -> impl IntoView {
    let client = use_client();
    let title = hit.title.clone();
    let in_library = hit.in_library;

    let add = move |auto_download: bool| {
        let client = client.clone();
        let req = AddPublicationRequest {
            source_id: source.clone(),
            source_key: hit.key.clone(),
            auto_download,
        };
        spawn_local(async move {
            match client.add_publication(&req).await {
                Ok(publication) => status.set(Some(format!(
                    "Added \"{}\" to the library",
                    publication.title
                ))),
                Err(err) => status.set(Some(format!("Add failed: {err}"))),
            }
        });
    };
    let add_plain = add.clone();

    view! {
        <div class="manga-card browse-card">
            <span class="cover-wrap">
                {match hit.cover_url.clone() {
                    // Covers arrive through the server's cover proxy as
                    // relative URLs — resolve them against the configured
                    // server, not the page origin: in the shells the page
                    // origin is the app itself, not the server.
                    Some(url) => {
                        // joined base-relative, like every client call
                        let src = url
                            .strip_prefix('/')
                            .and_then(|path| use_client().base().join(path).ok())
                            .map(|u| u.to_string())
                            .unwrap_or(url);
                        view! {
                            <img class="manga-cover" src=src loading="lazy" alt=""/>
                        }
                            .into_any()
                    }
                    None => view! { <span class="manga-cover cover-empty"></span> }.into_any(),
                }}
                {in_library
                    .map(|_| {
                        view! {
                            <span class="in-library-badge" title="Already in the library">
                                "✓"
                            </span>
                        }
                    })}
            </span>
            <span class="manga-title">{title}</span>
            <span class="browse-actions">
                {match in_library {
                    Some(id) => view! {
                        <a class="button" href=format!("/manga/{id}")>"open"</a>
                    }
                        .into_any(),
                    None => view! {
                        <button title="Track in the library" on:click=move |_| add_plain(false)>
                            "track"
                        </button>
                        <button
                            title="Track and auto-download new chapters"
                            on:click=move |_| add(true)
                        >
                            "+ auto"
                        </button>
                    }
                        .into_any(),
                }}
            </span>
        </div>
    }
}
