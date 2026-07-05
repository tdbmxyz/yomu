//! Immersive reader: fullscreen overlay, chrome toggled by a center tap,
//! paged and vertical (webtoon) modes. Progress is reported per page turn;
//! when the server is unreachable the event lands in the offline outbox and
//! is merged back later (see `offline`).

use chrono::Utc;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::JsCast;
use leptos_router::hooks::use_query_map;
use yomu_domain::{ProgressEvent, SetPositionRequest};

use super::{NotFound, param_uuid};
use crate::offline::{self, ReaderMode};
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
    let mode = RwSignal::new(offline::reader_mode(manga_id));
    let chrome = RwSignal::new(true);

    let client = use_client();
    let pages = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.chapter_pages(chapter_id).await }
        }
    });
    let detail = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.manga(manga_id).await }
        }
    });

    // Report progress; offline failures append to the outbox so the journal
    // merge reconciles once we're back.
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
                if client.set_position(manga_id, &req).await.is_err() {
                    offline::outbox_push(ProgressEvent {
                        id: offline::uuid_v7_js(),
                        manga_id,
                        chapter_id,
                        page: p,
                        device: "web-offline".into(),
                        at: Utc::now(),
                    });
                }
            });
        }
    };

    let page_count = move || {
        pages
            .get()
            .and_then(|r| r.ok())
            .map(|p| p.page_count)
            .unwrap_or(0)
    };

    // Journal the opening position once the chapter is confirmed to exist —
    // not at mount, where a bad link would create an event for a chapter
    // that was never opened.
    let opened = StoredValue::new(false);
    Effect::new({
        let report = report.clone();
        move |_| {
            if page_count() > 0 && !opened.get_value() {
                opened.set_value(true);
                report(page.get_untracked());
            }
        }
    });
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

    let key_turn = turn.clone();
    let key_handle =
        window_event_listener(leptos::ev::keydown, move |ev| match ev.key().as_str() {
            "ArrowLeft" => key_turn(-1),
            "ArrowRight" => key_turn(1),
            _ => {}
        });
    on_cleanup(move || key_handle.remove());

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

    let toggle_mode = move |_| {
        let next = match mode.get_untracked() {
            ReaderMode::Paged => ReaderMode::Vertical,
            ReaderMode::Vertical => ReaderMode::Paged,
        };
        mode.set(next);
        offline::set_reader_mode(manga_id, next);
    };

    let turn_prev = turn.clone();
    let turn_next = turn.clone();
    let report_scroll = report.clone();
    let client_paged = use_client();
    let client_vertical = use_client();

    view! {
        <div class="reader-overlay" class:chrome-hidden=move || !chrome.get()>
            <div class="reader-chrome reader-top">
                <a href=format!("/manga/{manga_id}")>"← back"</a>
                <span class="reader-title">{chapter_title}</span>
                <button class="mode-btn" title="Toggle paged / vertical" on:click=toggle_mode>
                    {move || match mode.get() {
                        ReaderMode::Paged => "⇅ vertical",
                        ReaderMode::Vertical => "⇆ paged",
                    }}
                </button>
                <span class="muted">
                    {move || format!("{} / {}", page.get() + 1, page_count().max(1))}
                </span>
            </div>

            {move || {
                let prev = turn_prev.clone();
                let next = turn_next.clone();
                let report = report_scroll.clone();
                match pages.get() {
                    None => view! { <p class="muted reader-msg">"Loading pages…"</p> }.into_any(),
                    Some(Err(err)) => {
                        view! {
                            <p class="error reader-msg">"Cannot load chapter: " {err.to_string()}</p>
                        }
                            .into_any()
                    }
                    Some(Ok(meta)) => match mode.get() {
                        ReaderMode::Paged => {
                            let src = client_paged
                                .page_url(chapter_id, page.get())
                                .map(|u| u.to_string())
                                .unwrap_or_default();
                            view! {
                                <div class="reader-stage">
                                    <img class="reader-page" src=src alt=""/>
                                    <button
                                        class="page-zone left"
                                        aria-label="previous page"
                                        on:click=move |_| prev(-1)
                                    ></button>
                                    <button
                                        class="page-zone center"
                                        aria-label="toggle controls"
                                        on:click=move |_| chrome.update(|c| *c = !*c)
                                    ></button>
                                    <button
                                        class="page-zone right"
                                        aria-label="next page"
                                        on:click=move |_| next(1)
                                    ></button>
                                </div>
                            }
                                .into_any()
                        }
                        ReaderMode::Vertical => {
                            let strip = NodeRef::<leptos::html::Div>::new();
                            // Start at the current page, not the top: entering
                            // vertical mode (or "continue reading") must not
                            // rewind the saved position.
                            Effect::new(move |_| {
                                let Some(el) = strip.get() else { return };
                                if let Some(child) = el.children().item(page.get_untracked()) {
                                    child.scroll_into_view();
                                }
                            });
                            // Only user scrolling moves the journal: the
                            // programmatic positioning above also fires
                            // scroll events, and while images are still
                            // placeholder-height they would map to a wrong
                            // page and overwrite the saved position.
                            let interacted = RwSignal::new(false);
                            let on_scroll = move |ev: leptos::ev::Event| {
                                if !interacted.get_untracked() {
                                    return;
                                }
                                let el = event_target::<web_sys::Element>(&ev);
                                // The page under the viewport's midline; per
                                // element offsets, so uneven page heights and
                                // still-loading images don't skew the index.
                                let middle = el.scroll_top() as f64
                                    + el.client_height() as f64 / 2.0;
                                let children = el.children();
                                let mut index = 0;
                                for i in 0..children.length() {
                                    let Some(child) = children
                                        .item(i)
                                        .and_then(|c| c.dyn_into::<web_sys::HtmlElement>().ok())
                                    else {
                                        continue;
                                    };
                                    if (child.offset_top() as f64) <= middle {
                                        index = i;
                                    }
                                }
                                if index != page.get_untracked() {
                                    page.set(index);
                                    report(index);
                                }
                            };
                            view! {
                                <div
                                    class="reader-scroll"
                                    node_ref=strip
                                    on:scroll=on_scroll
                                    on:wheel=move |_| interacted.set(true)
                                    on:touchstart=move |_| interacted.set(true)
                                    on:pointerdown=move |_| interacted.set(true)
                                    on:click=move |_| chrome.update(|c| *c = !*c)
                                >
                                    {(0..meta.page_count)
                                        .map(|n| {
                                            let src = client_vertical
                                                .page_url(chapter_id, n)
                                                .map(|u| u.to_string())
                                                .unwrap_or_default();
                                            view! {
                                                <img
                                                    class="reader-strip-page"
                                                    src=src
                                                    loading=if n < 3 { "eager" } else { "lazy" }
                                                    alt=""
                                                />
                                            }
                                        })
                                        .collect_view()}
                                </div>
                            }
                                .into_any()
                        }
                    },
                }
            }}

            <div class="reader-chrome reader-bottom">
                {move || {
                    neighbours()
                        .and_then(|(previous, _)| previous)
                        .map(|prev| {
                            // rel=external: the reader reads its params once
                            // at mount; same-route SPA nav would keep the old
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
