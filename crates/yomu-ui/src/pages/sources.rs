//! Sources: the configured sources as a catalog — pick one, then browse
//! its query-less listings (popular / latest) page by page. Cross-source
//! search lives in the Search tab (`search.rs`).

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_params_map;
use yomu_domain::{MangaSummary, SourceInfo};

use super::search::SummaryGrid;
use crate::offline;
use crate::use_client;

/// The list of configured sources; browsable ones link to their catalog.
#[component]
pub fn Sources() -> impl IntoView {
    let client = use_client();
    let conn = crate::use_connectivity();
    let sources = LocalResource::new(move || {
        conn.track();
        let client = client.clone();
        async move {
            offline::cached(conn, "sources", || client.sources())
                .await
                .map(|(value, _)| value)
        }
    });

    view! {
        <section class="sources">
            <h2>"Sources"</h2>
            {move || match sources.get() {
                None => view! { <p class="muted">"Loading sources…"</p> }.into_any(),
                Some(Err(err)) => view! { <p class="error">{err.to_string()}</p> }.into_any(),
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
                Some(Ok(list)) => {
                    view! {
                        <div class="source-list">
                            {list.into_iter().map(source_card).collect_view()}
                        </div>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}

fn source_card(source: SourceInfo) -> impl IntoView {
    let host = source.base_url.host_str().unwrap_or_default().to_string();
    let sorts = source
        .browse
        .iter()
        .map(|s| s.label())
        .collect::<Vec<_>>()
        .join(" · ");
    let body = view! {
        <span class="source-card-name">{source.name.clone()}</span>
        <span class="muted source-card-host">{host}</span>
        <span class="grow"></span>
        {if sorts.is_empty() {
            view! { <span class="muted source-card-sorts">"search only"</span> }.into_any()
        } else {
            view! { <span class="source-card-sorts">{sorts}</span> }.into_any()
        }}
    };
    if source.browse.is_empty() {
        // Nothing to list: reachable through the Search tab only.
        view! { <div class="source-card">{body}</div> }.into_any()
    } else {
        view! {
            <a class="source-card" href=format!("/sources/{}", source.id)>
                {body}
            </a>
        }
        .into_any()
    }
}

/// One source's catalog: listing tabs (popular / latest) plus an endless
/// "load more" grid.
#[component]
pub fn SourceCatalog() -> impl IntoView {
    let id = use_params_map()
        .get_untracked()
        .get("source")
        .unwrap_or_default();

    let client = use_client();
    let sources = LocalResource::new(move || {
        let client = client.clone();
        async move { client.sources().await }
    });

    view! {
        <section class="sources">
            {move || {
                let id = id.clone();
                match sources.get() {
                    None => view! { <p class="muted">"Loading…"</p> }.into_any(),
                    Some(Err(err)) => view! { <p class="error">{err.to_string()}</p> }.into_any(),
                    Some(Ok(list)) => match list.into_iter().find(|s| s.id == id) {
                        Some(info) if !info.browse.is_empty() => {
                            view! { <Catalog info/> }.into_any()
                        }
                        Some(info) => {
                            view! {
                                <h2>{info.name}</h2>
                                <p class="muted">
                                    "This source has no catalog listings — find its titles "
                                    "through the " <a href="/search">"Search"</a> " tab."
                                </p>
                            }
                                .into_any()
                        }
                        None => {
                            view! { <p class="error">"No such source: " {id.clone()}</p> }
                                .into_any()
                        }
                    },
                }
            }}
        </section>
    }
}

#[component]
fn Catalog(info: SourceInfo) -> impl IntoView {
    let status = RwSignal::new(None::<String>);
    let source_id = info.id.clone();
    let sort = RwSignal::new(*info.browse.first().expect("catalog needs a listing"));

    let items = RwSignal::new(Vec::<MangaSummary>::new());
    let next_page = StoredValue::new(1u32);
    let loading = RwSignal::new(false);
    let exhausted = RwSignal::new(false);

    let load_more = {
        let client = use_client();
        let source_id = source_id.clone();
        move || {
            if loading.get_untracked() || exhausted.get_untracked() {
                return;
            }
            loading.set(true);
            let client = client.clone();
            let source = source_id.clone();
            let requested_sort = sort.get_untracked();
            let page = next_page.get_value();
            spawn_local(async move {
                match client.browse(&source, requested_sort, page).await {
                    Ok(batch) => {
                        // stale answer (sort switched meanwhile)?
                        if sort.get_untracked() == requested_sort {
                            if batch.is_empty() {
                                exhausted.set(true);
                            } else {
                                next_page.set_value(page + 1);
                                items.update(|all| all.extend(batch));
                            }
                        }
                    }
                    Err(err) => status.set(Some(format!("Browse failed: {err}"))),
                }
                loading.set(false);
            });
        }
    };

    // Reset and load the first page whenever the listing changes.
    Effect::new({
        let load_more = load_more.clone();
        move |_| {
            sort.track();
            items.set(Vec::new());
            next_page.set_value(1);
            exhausted.set(false);
            load_more();
        }
    });

    let grid_source = source_id.clone();
    view! {
        <h2>
            <a class="muted catalog-back" href="/sources">"‹"</a>
            " " {info.name.clone()}
        </h2>
        {move || status.get().map(|s| view! { <p class="status">{s}</p> })}
        <div class="category-tabs">
            {info
                .browse
                .iter()
                .map(|&s| {
                    view! {
                        <button
                            class:active=move || sort.get() == s
                            on:click=move |_| sort.set(s)
                        >
                            {s.label()}
                        </button>
                    }
                })
                .collect_view()}
        </div>
        {move || {
            let list = items.get();
            view! { <SummaryGrid items=list source=grid_source.clone() status/> }
        }}
        <p class="browse-more">
            {move || {
                if exhausted.get() && items.get().is_empty() {
                    view! { <span class="muted">"Nothing listed."</span> }.into_any()
                } else if exhausted.get() {
                    view! { <span class="muted">"End of the listing."</span> }.into_any()
                } else {
                    let load = load_more.clone();
                    view! {
                        <button disabled=move || loading.get() on:click=move |_| load()>
                            {move || if loading.get() { "Loading…" } else { "Load more" }}
                        </button>
                    }
                        .into_any()
                }
            }}
        </p>
    }
}
