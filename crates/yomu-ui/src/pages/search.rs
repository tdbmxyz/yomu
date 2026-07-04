use leptos::prelude::*;
use leptos::task::spawn_local;
use yomu_domain::{AddMangaRequest, MangaSummary};

use crate::use_client;

#[component]
pub fn Search() -> impl IntoView {
    let client = use_client();
    let query = RwSignal::new(String::new());
    let source_id = RwSignal::new(String::new());
    // (source_id, query) of the search currently displayed.
    let submitted = RwSignal::new(None::<(String, String)>);
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
                    Some((source, q)) => client.search(&source, &q).await.map(Some),
                    None => Ok(None),
                }
            }
        }
    });

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let q = query.get_untracked().trim().to_string();
        let source = source_id.get_untracked();
        if q.is_empty() || source.is_empty() {
            return;
        }
        status.set(None);
        submitted.set(Some((source, q)));
    };

    view! {
        <section>
            <h2>"Add manga"</h2>
            {move || match sources.get() {
                Some(Ok(list)) if list.is_empty() => {
                    view! {
                        <p class="muted">
                            "No sources configured. Drop a *.toml definition in the server's "
                            <code>"sources.d/"</code> " directory (see the sample file) and restart."
                        </p>
                    }
                        .into_any()
                }
                Some(Ok(list)) => {
                    if source_id.get_untracked().is_empty()
                        && let Some(first) = list.first()
                    {
                        source_id.set(first.id.clone());
                    }
                    view! {
                        <form class="search-form" on:submit=submit>
                            <select on:change=move |ev| source_id.set(event_target_value(&ev))>
                                {list
                                    .into_iter()
                                    .map(|s| {
                                        view! { <option value=s.id.clone()>{s.name.clone()}</option> }
                                    })
                                    .collect_view()}
                            </select>
                            <input
                                type="search"
                                class="grow"
                                placeholder="Search title…"
                                prop:value=query
                                on:input=move |ev| query.set(event_target_value(&ev))
                            />
                            <button type="submit" class="primary">
                                "Search"
                            </button>
                        </form>
                    }
                        .into_any()
                }
                Some(Err(err)) => view! { <p class="error">{err.to_string()}</p> }.into_any(),
                None => view! { <p class="muted">"Loading sources…"</p> }.into_any(),
            }}
            {move || status.get().map(|s| view! { <p class="status">{s}</p> })}
            {move || match results.get() {
                Some(Ok(Some(hits))) if hits.is_empty() => {
                    view! { <p class="muted">"No results."</p> }.into_any()
                }
                Some(Ok(Some(hits))) => {
                    let source = submitted
                        .get_untracked()
                        .map(|(source, _)| source)
                        .unwrap_or_default();
                    view! {
                        <ul class="result-list">
                            {hits
                                .into_iter()
                                .map(|hit| {
                                    view! { <ResultItem hit source=source.clone() status/> }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }
                Some(Err(err)) => view! { <p class="error">{err.to_string()}</p> }.into_any(),
                _ => ().into_any(),
            }}
        </section>
    }
}

#[component]
fn ResultItem(
    hit: MangaSummary,
    source: String,
    status: RwSignal<Option<String>>,
) -> impl IntoView {
    let client = use_client();

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
        <li class="result-item">
            <span class="result-title">{hit.title.clone()}</span>
            <span class="grow"></span>
            <button on:click=move |_| add_plain(false)>"Track"</button>
            <button class="primary" on:click=move |_| add(true)>
                "Track + auto-download"
            </button>
        </li>
    }
}
