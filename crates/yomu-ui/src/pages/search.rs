//! Search: one search box across every source at once. Query-less catalog
//! browsing lives in the Sources tab (see `sources.rs`).

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use yomu_domain::MangaSummary;

use super::source_title::TrackResult;
use crate::cover::SourceCover;

use crate::use_client;

#[component]
pub fn Search() -> impl IntoView {
    let client = use_client();
    let params = use_query_map();
    let submitted = Memo::new(move |_| params.get().get("q").filter(|q| !q.is_empty()));
    let query = RwSignal::new(submitted.get_untracked().unwrap_or_default());
    Effect::new(move |_| query.set(submitted.get().unwrap_or_default()));
    let navigate = use_navigate();
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
        if submitted.get_untracked().as_ref() == Some(&q) {
            results.refetch();
        } else {
            navigate(&search_href(&q), Default::default());
        }
    };

    let clear_search = use_navigate();
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
                                    on:click={
                                        let navigate = clear_search.clone();
                                        move |_| {
                                            status.set(None);
                                            query.set(String::new());
                                            navigate("/search", Default::default());
                                        }
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
                                                        items=Signal::stored(group.results)
                                                        source=group.source_id
                                                        search_query=submitted.get_untracked()
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

pub(super) fn search_href(query: &str) -> String {
    format!(
        "/search?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", query)
            .finish()
    )
}

/// Cover grid of source results, each with track actions. Shared with the
/// Sources tab's catalog page.
#[component]
pub(crate) fn SummaryGrid(
    #[prop(into)] items: Signal<Vec<MangaSummary>>,
    source: String,
    #[prop(optional_no_strip)] search_query: Option<String>,
    status: RwSignal<Option<String>>,
) -> impl IntoView {
    view! {
        <div class="manga-grid browse-grid">
            <For
                each=move || items.get()
                key=|hit| hit.key.clone()
                children=move |hit| view! {
                    <SummaryCard hit source=source.clone() search_query=search_query.clone() status/>
                }
            />
        </div>
    }
}

#[component]
fn SummaryCard(
    hit: MangaSummary,
    source: String,
    #[prop(optional_no_strip)] search_query: Option<String>,
    status: RwSignal<Option<String>>,
) -> impl IntoView {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("key", &hit.key);
    if let Some(q) = search_query {
        query.append_pair("q", &q);
    }
    let href = format!("/sources/{source}/title?{}", query.finish());
    let in_library = hit.in_library;
    view! {
        <div class="manga-card browse-card">
            <a class="browse-title" href=href>
                <span class="cover-wrap">
                    <SourceCover path=hit.cover_url/>
                    {in_library.map(|_| view! {
                        <span class="in-library-badge" title="Already in the library" aria-hidden="true">"✓"</span>
                    })}
                </span>
                <span class="manga-title">{hit.title}</span>
            </a>
            <TrackResult source key=hit.key in_library status/>
        </div>
    }
}
