//! A title from a source's catalog, before (or independent of) tracking it.
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_params_map, use_query_map};
use yomu_domain::{AddPublicationRequest, SourcePreview};

use super::source_link::SourceLink;
use crate::cover::{Cover, SourceCover};
use crate::use_client;

#[component]
pub fn SourceTitle() -> impl IntoView {
    let params = use_params_map();
    let query = use_query_map();
    view! {
        {move || match (params.get().get("source"), query.get().get("key")) {
            (Some(source), Some(key)) if !key.is_empty() => {
                let search_query = query.get_untracked().get("q");
                view! { <TitlePage source key search_query/> }.into_any()
            }
            _ => view! { <super::NotFound/> }.into_any(),
        }}
    }
}

#[component]
fn TitlePage(source: String, key: String, search_query: Option<String>) -> impl IntoView {
    let client = use_client();
    let retry = RwSignal::new(0u32);
    let preview = LocalResource::new({
        let source = source.clone();
        move || {
            retry.track();
            let (client, source, key) = (client.clone(), source.clone(), key.clone());
            async move { client.source_preview(&source, &key).await }
        }
    });
    let (back, back_label) = match search_query {
        Some(q) => (super::search::search_href(&q), "Back to search"),
        None => (format!("/sources/{source}"), "Back to catalog"),
    };
    view! {
        <section class="source-title">
            <a class="button source-title-back" href=back>{back_label}</a>
            {move || match preview.get() {
                None => view! { <p class="muted" role="status">"Loading title details…"</p> }.into_any(),
                Some(Err(error)) => {
                    let message = match error {
                        yomu_client::ClientError::Api { status: 404, .. } => "This title could not be found in the catalog.",
                        _ => "Could not load the title. Check the connection to your Yomu server and try again.",
                    };
                    view! {
                        <p class="error" role="alert">{message}</p>
                        <button on:click=move |_| retry.update(|n| *n += 1)>"Retry details"</button>
                    }.into_any()
                }
                Some(Ok(preview)) => view! { <TitleDetails preview source=source.clone() retry/> }.into_any(),
            }}
        </section>
    }
}

#[component]
fn TitleDetails(preview: SourcePreview, source: String, retry: RwSignal<u32>) -> impl IntoView {
    let status = RwSignal::new(None::<String>);
    let in_library = preview.summary.in_library;
    view! {
        <div class="source-title-header">
            {match in_library {
                Some(id) => view! { <Cover manga_id=id large=true/> }.into_any(),
                None => view! { <SourceCover path=preview.summary.cover_url.clone() large=true/> }.into_any(),
            }}
            <div class="source-title-heading">
                <h2>{preview.summary.title}</h2>
                <p class="muted">{preview.source_name}</p>
                <dl class="source-title-facts">
                    <dt>"Chapter count"</dt>
                    <dd>{preview.chapter_count.map(|n| n.to_string()).unwrap_or_else(|| "Unavailable".into())}</dd>
                    <dt>"Last chapter date"</dt>
                    <dd>{preview.latest_chapter_at.map(|date| date.date_label()).unwrap_or_else(|| "Unavailable".into())}</dd>
                </dl>
                <TrackResult source key=preview.summary.key.clone() in_library status/>
                {move || status.get().map(|s| view! { <p class="status" role="status">{s}</p> })}
            </div>
        </div>
        {preview.live_unavailable.then(|| view! {
            <div class="source-title-notice">
                <p role="status">
                    {if preview.browser_challenge {
                        "The source requires a browser check; Yomu can't fetch live details. "
                    } else {
                        "The source couldn't provide live details. "
                    }}
                    {if in_library.is_some() { "Showing saved library information." } else { "Showing available catalog information." }}
                </p>
                <div class="browse-actions">
                    <SourceLink source_key=preview.summary.key/>
                    <button on:click=move |_| retry.update(|n| *n += 1)>"Retry details"</button>
                </div>
            </div>
        })}
        <h3>"Summary"</h3>
        <p class="source-title-summary">{preview.description.unwrap_or_else(|| "No summary available.".into())}</p>
        {(!preview.genres.is_empty()).then(|| view! {
            <h3>"Genres"</h3>
            <p>{preview.genres.join(" · ")}</p>
        })}
    }
}

#[component]
pub(super) fn TrackResult(
    source: String,
    key: String,
    in_library: Option<uuid::Uuid>,
    status: RwSignal<Option<String>>,
) -> impl IntoView {
    let client = use_client();
    let library_cache = crate::cache::use_library_cache();
    let detail_cache = crate::cache::use_detail_cache();
    let tracked = RwSignal::new(in_library);
    let adding = RwSignal::new(false);
    let add = move |auto_download: bool| {
        if adding.get_untracked() {
            return;
        }
        adding.set(true);
        status.set(None);
        let client = client.clone();
        let req = AddPublicationRequest {
            source_id: source.clone(),
            source_key: key.clone(),
            auto_download,
        };
        spawn_local(async move {
            match client.add_publication(&req).await {
                Ok(publication) => {
                    crate::cache::mark_publication_stale(library_cache, detail_cache);
                    tracked.try_set(Some(publication.id));
                    status.try_set(Some(format!(
                        "Added \"{}\" to the library",
                        publication.title
                    )));
                }
                Err(err) => {
                    status.try_set(Some(format!("Add failed: {err}")));
                }
            }
            adding.try_set(false);
        });
    };
    view! {
        <div class="browse-actions">
            {move || match tracked.get() {
                Some(id) => view! { <a class="button" href=format!("/publications/{id}")>"open"</a> }.into_any(),
                None => {
                    let (add_plain, add_auto) = (add.clone(), add.clone());
                    view! {
                        <button disabled=adding title="Track in the library" on:click=move |_| add_plain(false)>"track"</button>
                        <button disabled=adding title="Track and auto-download new chapters" on:click=move |_| add_auto(true)>"+ auto"</button>
                    }.into_any()
                }
            }}
        </div>
    }
}
