use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_query_map;
use yomu_domain::SetPositionRequest;

use super::{NotFound, param_uuid};
use crate::use_client;

#[component]
pub fn Reader() -> impl IntoView {
    let (Some(manga_id), Some(chapter_id)) = (param_uuid("manga"), param_uuid("chapter")) else {
        return view! { <NotFound/> }.into_any();
    };

    let initial_page: u32 = use_query_map()
        .get_untracked()
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let page = RwSignal::new(initial_page);

    let client = use_client();
    let pages = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.chapter_pages(chapter_id).await }
        }
    });
    // Chapter list for title + next/previous chapter navigation.
    let detail = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.manga(manga_id).await }
        }
    });

    // Report the position on open and on every page turn. Fire-and-forget:
    // reading must not stutter on a slow network.
    let report = {
        let client = client.clone();
        move |p: u32| {
            let client = client.clone();
            spawn_local(async move {
                let req = SetPositionRequest {
                    chapter_id,
                    page: p,
                    device: "web".into(),
                };
                if let Err(err) = client.set_position(manga_id, &req).await {
                    leptos::logging::warn!("position not saved: {err}");
                }
            });
        }
    };
    report(initial_page);

    let page_count = move || {
        pages
            .get()
            .and_then(|r| r.ok())
            .map(|p| p.page_count)
            .unwrap_or(0)
    };
    let turn = {
        let report = report.clone();
        move |delta: i64| {
            let count = page_count();
            if count == 0 {
                return;
            }
            let current = page.get_untracked() as i64;
            let next = (current + delta).clamp(0, count as i64 - 1) as u32;
            if next != current as u32 {
                page.set(next);
                report(next);
            }
        }
    };

    // Arrow keys turn pages.
    let key_turn = turn.clone();
    let key_handle =
        window_event_listener(leptos::ev::keydown, move |ev| match ev.key().as_str() {
            "ArrowLeft" => key_turn(-1),
            "ArrowRight" => key_turn(1),
            _ => {}
        });
    on_cleanup(move || key_handle.remove());

    // Neighbouring chapters in reading order.
    let neighbours = move || {
        let chapters = detail.get().and_then(|r| r.ok()).map(|d| d.chapters)?;
        let index = chapters.iter().position(|c| c.id == chapter_id)?;
        let previous = index.checked_sub(1).map(|i| chapters[i].id);
        let next = chapters.get(index + 1).map(|c| c.id);
        Some((previous, next))
    };
    let chapter_title = move || {
        detail
            .get()
            .and_then(|r| r.ok())
            .and_then(|d| d.chapters.into_iter().find(|c| c.id == chapter_id))
            .map(|c| c.title)
            .unwrap_or_default()
    };

    let turn_click_prev = turn.clone();
    let turn_click_next = turn.clone();
    let client_pages = use_client();

    view! {
        <div class="reader">
            <div class="reader-bar">
                <a href=format!("/manga/{manga_id}")>"← back"</a>
                <span class="reader-title">{chapter_title}</span>
                <span class="muted">
                    {move || format!("{} / {}", page.get() + 1, page_count().max(1))}
                </span>
            </div>
            {move || {
                let prev = turn_click_prev.clone();
                let next = turn_click_next.clone();
                match pages.get() {
                    None => view! { <p class="muted">"Loading pages…"</p> }.into_any(),
                    Some(Err(err)) => {
                        view! { <p class="error">"Cannot load chapter: " {err.to_string()}</p> }
                            .into_any()
                    }
                    Some(Ok(_)) => {
                        let src = client_pages
                            .page_url(chapter_id, page.get())
                            .map(|u| u.to_string())
                            .unwrap_or_default();
                        view! {
                            <div class="reader-stage">
                                <button
                                    class="page-zone left"
                                    aria-label="previous page"
                                    on:click=move |_| prev(-1)
                                ></button>
                                <img class="reader-page" src=src alt=""/>
                                <button
                                    class="page-zone right"
                                    aria-label="next page"
                                    on:click=move |_| next(1)
                                ></button>
                            </div>
                        }
                            .into_any()
                    }
                }
            }}
            <div class="reader-nav">
                {move || {
                    neighbours()
                        .and_then(|(previous, _)| previous)
                        .map(|prev| {
                            // rel=external forces a real navigation: the
                            // reader reads its params once at mount, so
                            // same-route SPA navigation would keep the old
                            // chapter.
                            view! {
                                <a
                                    class="button"
                                    rel="external"
                                    href=format!("/read/{manga_id}/{prev}")
                                >
                                    "← previous chapter"
                                </a>
                            }
                        })
                }}
                <span class="grow"></span>
                {move || {
                    neighbours()
                        .and_then(|(_, next)| next)
                        .map(|next| {
                            view! {
                                <a
                                    class="button primary"
                                    rel="external"
                                    href=format!("/read/{manga_id}/{next}")
                                >
                                    "next chapter →"
                                </a>
                            }
                        })
                }}
            </div>
        </div>
    }
    .into_any()
}
