//! Browse: one search box across every source at once, and query-less
//! catalog browsing (popular / latest) per source.

use leptos::prelude::*;
use leptos::task::spawn_local;
use yomu_domain::{AddMangaRequest, BrowseSort, MangaSummary, SourceInfo};

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
            <h2>"Browse"</h2>
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
                    // browse mode
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
                        Some(Ok(list)) => view! { <SourceBrowser list status/> }.into_any(),
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

/// Query-less catalog browsing: pick a source, pick a listing, page through
/// its catalog.
#[component]
fn SourceBrowser(list: Vec<SourceInfo>, status: RwSignal<Option<String>>) -> impl IntoView {
    let browsable: Vec<SourceInfo> = list.into_iter().filter(|s| !s.browse.is_empty()).collect();
    if browsable.is_empty() {
        return view! {
            <p class="muted">
                "No source offers catalog browsing — add a " <code>"[browse.popular]"</code>
                " or " <code>"[browse.latest]"</code>
                " listing to a source definition, or use the search above."
            </p>
        }
        .into_any();
    }

    let first = browsable[0].clone();
    let selected = RwSignal::new(first.id.clone());
    let sort = RwSignal::new(*first.browse.first().unwrap_or(&BrowseSort::Popular));
    let sorts_of = {
        let browsable = browsable.clone();
        move |id: &str| {
            browsable
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.browse.clone())
                .unwrap_or_default()
        }
    };

    let items = RwSignal::new(Vec::<MangaSummary>::new());
    let next_page = StoredValue::new(1u32);
    let loading = RwSignal::new(false);
    let exhausted = RwSignal::new(false);

    let load_more = {
        let client = use_client();
        move || {
            if loading.get_untracked() || exhausted.get_untracked() {
                return;
            }
            loading.set(true);
            let client = client.clone();
            let source = selected.get_untracked();
            let requested_sort = sort.get_untracked();
            let page = next_page.get_value();
            spawn_local(async move {
                match client.browse(&source, requested_sort, page).await {
                    Ok(batch) => {
                        // stale answer (source/sort switched meanwhile)?
                        if selected.get_untracked() == source
                            && sort.get_untracked() == requested_sort
                        {
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

    // Reset and load the first page whenever the source or listing changes.
    Effect::new({
        let load_more = load_more.clone();
        move |_| {
            selected.track();
            sort.track();
            items.set(Vec::new());
            next_page.set_value(1);
            exhausted.set(false);
            load_more();
        }
    });

    let sorts_view = {
        let sorts_of = sorts_of.clone();
        move || {
            sorts_of(&selected.get())
                .into_iter()
                .map(|s| {
                    view! {
                        <button
                            class:active=move || sort.get() == s
                            on:click=move |_| sort.set(s)
                        >
                            {s.label()}
                        </button>
                    }
                })
                .collect_view()
        }
    };

    view! {
        <div class="category-tabs">
            {browsable
                .iter()
                .map(|source| {
                    let id = source.id.clone();
                    let is_active = {
                        let id = id.clone();
                        move || selected.get() == id
                    };
                    let switch = {
                        let sorts_of = sorts_of.clone();
                        let id = id.clone();
                        move |_| {
                            let sorts = sorts_of(&id);
                            if !sorts.contains(&sort.get_untracked())
                                && let Some(first) = sorts.first()
                            {
                                sort.set(*first);
                            }
                            selected.set(id.clone());
                        }
                    };
                    view! {
                        <button class:active=is_active on:click=switch>
                            {source.name.clone()}
                        </button>
                    }
                })
                .collect_view()}
            <span class="grow"></span>
            {sorts_view}
        </div>
        {move || {
            let list = items.get();
            view! {
                <SummaryGrid items=list source=selected.get_untracked() status/>
            }
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
    .into_any()
}

/// Cover grid of source results, each with track actions.
#[component]
fn SummaryGrid(
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

    let add = move |auto_download: bool| {
        let client = client.clone();
        let req = AddMangaRequest {
            source_id: source.clone(),
            source_key: hit.key.clone(),
            auto_download,
        };
        spawn_local(async move {
            match client.add_manga(&req).await {
                Ok(manga) => status.set(Some(format!("Added \"{}\" to the library", manga.title))),
                Err(err) => status.set(Some(format!("Add failed: {err}"))),
            }
        });
    };
    let add_plain = add.clone();

    view! {
        <div class="manga-card browse-card">
            <span class="cover-wrap">
                {match hit.cover_url.clone() {
                    // Covers come straight from the site here (only library
                    // manga get the server-side cover proxy); some sites
                    // block hotlinking, leaving the placeholder background.
                    Some(url) => view! {
                        <img class="manga-cover" src=url.to_string() loading="lazy" alt=""/>
                    }
                        .into_any(),
                    None => view! { <span class="manga-cover cover-empty"></span> }.into_any(),
                }}
            </span>
            <span class="manga-title">{title}</span>
            <span class="browse-actions">
                <button title="Track in the library" on:click=move |_| add_plain(false)>
                    "track"
                </button>
                <button
                    title="Track and auto-download new chapters"
                    on:click=move |_| add(true)
                >
                    "+ auto"
                </button>
            </span>
        </div>
    }
}
