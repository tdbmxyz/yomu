//! Immersive reader: fullscreen overlay, chrome toggled by a center tap,
//! paged and vertical (webtoon) modes. Progress is reported per page turn;
//! when the server is unreachable the event lands in the offline outbox and
//! is merged back later (see `offline`).

use chrono::Utc;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::NavigateOptions;
use leptos_router::hooks::use_query_map;
use yomu_domain::{ProgressEvent, SetLocatorRequest};

use super::{NotFound, param_uuid};
use crate::offline::{self, ReaderDirection, ReaderFit, ReaderMode};
use crate::pager;
use crate::use_client;

mod gesture;
mod stages;

use stages::{ReaderCtx, paged_stage, vertical_strip};

/// Page image URL: chapters saved to the device inside the Tauri shell are
/// served by the shell's own protocol; everything else comes from the
/// server (where the browser's service worker may still answer offline).
fn page_source(client: &yomu_client::YomuClient, chapter_id: uuid::Uuid, n: u32) -> String {
    if offline::device_chapter_pages(chapter_id).is_some()
        && let Some(url) = offline::shell_page_url(chapter_id, n)
    {
        return url;
    }
    client
        .page_url(chapter_id, n)
        .map(|u| u.to_string())
        .unwrap_or_default()
}

/// Routed wrapper: re-creates the reader whenever the chapter param
/// changes, so prev/next chapter can be a plain SPA link. A full-document
/// reload would need a server-side SPA fallback, which the Tauri shell's
/// asset protocol doesn't have.
#[component]
pub fn Reader() -> impl IntoView {
    let params = leptos_router::hooks::use_params_map();
    view! {
        {move || {
            params.track();
            view! { <ReaderInner/> }
        }}
    }
}

#[component]
fn ReaderInner() -> impl IntoView {
    let (Some(manga_id), Some(chapter_id)) = (param_uuid("manga"), param_uuid("chapter")) else {
        return view! { <NotFound/> }.into_any();
    };

    let page_query = use_query_map().get_untracked().get("page");
    // "?page=end" (arriving backward through a chapter transition)
    // resolves to the last page once the page count is known.
    let wants_end = page_query.as_deref() == Some("end");
    let initial_page: u32 = page_query.and_then(|p| p.parse().ok()).unwrap_or(0);
    let page = RwSignal::new(initial_page);
    // Paged mode's virtual position: -1 and page_count are the chapter
    // transition panels; real pages mirror into `page` (which keeps
    // driving the counter, the progress bar, and progress reports).
    let pos = RwSignal::new(initial_page as i64);
    // Live drag offset (px) and the turn being animated (pos + delta).
    let drag = RwSignal::new(0.0_f64);
    let snap = RwSignal::new(None::<i64>);
    let mode = RwSignal::new(offline::reader_mode(manga_id));
    let fit = RwSignal::new(offline::reader_fit(manga_id));
    let dir = RwSignal::new(offline::reader_direction(manga_id));
    let chrome = RwSignal::new(true);
    // Vertical mode reads continuously across chapters: `segments` lists the
    // (chapter, page count) pairs currently in the strip, and
    // `current_chapter` follows the reader through it (in paged mode it
    // stays the routed chapter).
    let current_chapter = RwSignal::new(chapter_id);
    let segments: RwSignal<Vec<(uuid::Uuid, u32)>> = RwSignal::new(Vec::new());
    // Page counts of every chapter the strip has visited: segments evicted
    // to keep the DOM small re-enter without re-asking the server (their
    // images come back through the browser HTTP cache).
    let page_counts: StoredValue<std::collections::HashMap<uuid::Uuid, u32>> =
        StoredValue::new(std::collections::HashMap::new());

    let client = use_client();
    let conn = crate::use_connectivity();
    // NB: the reader's resources deliberately do NOT track `conn` — a
    // badge retry mid-read would refetch them and rebuild the strip at the
    // mount-time page. They read it untracked for the offline shortcut.
    let pages = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move {
                // A chapter saved on this device is read entirely from the
                // device: the images already come from the shell/worker
                // copy unconditionally (see page_source), so its page
                // count must too — otherwise opening a saved chapter
                // waits on a server round-trip that adds nothing (and, on
                // a bad link, seconds).
                if let Some(page_count) = offline::device_chapter_pages(chapter_id) {
                    return Ok(yomu_domain::PagesResponse {
                        unit_id: chapter_id,
                        page_count,
                        downloaded: false,
                    });
                }
                client.unit_pages(chapter_id).await
            }
        }
    });
    let detail = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move {
                // offline: chapter title + prev/next come from the
                // last-known-good copy the manga page stored
                offline::cached(conn, &format!("manga:{manga_id}"), || {
                    client.publication(manga_id)
                })
                .await
                .map(|(value, _)| value)
            }
        }
    });

    // Report progress; offline failures append to the outbox so the journal
    // merge reconciles once we're back.
    let report = {
        let client = client.clone();
        move |chapter: uuid::Uuid, p: u32| {
            let client = client.clone();
            spawn_local(async move {
                let req = SetLocatorRequest {
                    unit_id: chapter,
                    page: p,
                    device: "web".into(),
                };
                if client.set_locator(manga_id, &req).await.is_err() {
                    offline::outbox_push(ProgressEvent {
                        id: offline::uuid_v7_js(),
                        publication_id: manga_id,
                        unit_id: chapter,
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
                let last = page_count().saturating_sub(1);
                // "?page=end" resolves to the last page; any other opening
                // page is clamped so a stale or hand-edited ?page= (or a
                // chapter that shrank server-side) can't open a blank panel
                // and journal an out-of-range position.
                if wants_end || page.get_untracked() > last {
                    page.set(last);
                    pos.set(last as i64);
                }
                report(chapter_id, page.get_untracked());
            }
        }
    });

    let neighbours = move || {
        let units = detail.get().and_then(|r| r.ok()).map(|d| d.units)?;
        let index = units.iter().position(|c| c.id == current_chapter.get())?;
        let previous = index.checked_sub(1).map(|i| units[i].id);
        let next = units.get(index + 1).map(|c| c.id);
        Some((previous, next))
    };
    let chapter_title = move || {
        detail
            .get()
            .and_then(|r| r.ok())
            .and_then(|d| d.units.into_iter().find(|c| c.id == current_chapter.get()))
            .map(|c| c.title)
            .unwrap_or_default()
    };

    let navigate = leptos_router::hooks::use_navigate();
    // Chapter-to-chapter moves inside the reader replace the history
    // entry instead of pushing one: system back must leave the reader
    // (to the chapter list), not walk back through every chapter read.
    let reroute = move |url: String| {
        navigate(
            &url,
            NavigateOptions {
                replace: true,
                ..Default::default()
            },
        )
    };
    let neighbour_ids = move || neighbours().unwrap_or((None, None));

    // Paged-mode turn requester shared by keys, tap zones, the ‹ › pill
    // and drag commits. Turns land on the virtual range [-1, count]
    // (pager::bounds trims missing neighbours); one more turn on a
    // transition panel crosses into the neighbouring chapter.
    let request_turn = {
        let reroute = reroute.clone();
        move |delta: i64| {
            if snap.get_untracked().is_some() {
                return; // mid-animation
            }
            let count = page_count();
            if count == 0 {
                return;
            }
            let (prev, next) = neighbour_ids();
            let (lo, hi) = pager::bounds(count, prev.is_some(), next.is_some());
            let current = pos.get_untracked();
            let target = current + delta;
            if target > hi {
                if current == count as i64
                    && let Some(next) = next
                {
                    reroute(format!("/read/{manga_id}/{next}"));
                }
                return;
            }
            if target < lo {
                if current == -1
                    && let Some(prev) = prev
                {
                    reroute(format!("/read/{manga_id}/{prev}?page=end"));
                }
                return;
            }
            snap.set(Some(delta));
        }
    };
    // Lands an animated turn; transition panels don't touch `page`.
    let commit_pos = {
        let report = report.clone();
        move |target: i64| {
            pos.set(target);
            if (0..page_count() as i64).contains(&target) {
                let p = target as u32;
                if p != page.get_untracked() {
                    page.set(p);
                    report(chapter_id, p);
                }
            }
        }
    };

    // Land an armed turn: what the track's `transitionend` normally does.
    // Reads the pending delta from `snap` so it is safe to call from either
    // the transition handler or the deadlock fallback below.
    let finish_snap = {
        let commit = commit_pos.clone();
        move || {
            if let Some(delta) = snap.get_untracked() {
                snap.set(None);
                drag.set(0.0);
                commit(pos.get_untracked() + delta);
            }
        }
    };
    // Deadlock guard: a committed turn clears `snap` on the track's
    // `transitionend`. When that event never fires — prefers-reduced-motion,
    // a user `transition: none`, or a transform equal to the current one —
    // `snap` would stay `Some` forever and every further turn early-returns.
    // Each time a snap is armed, schedule a fallback that lands it; a
    // generation tag makes a superseded timer a no-op so it can't commit a
    // later turn early.
    let snap_gen = StoredValue::new(0_u64);
    Effect::new({
        let finish_snap = finish_snap.clone();
        move |_| {
            if snap.get().is_none() {
                return;
            }
            snap_gen.update_value(|g| *g += 1);
            let generation = snap_gen.get_value();
            let finish = finish_snap.clone();
            set_timeout(
                move || {
                    if snap_gen.get_value() == generation && snap.get_untracked().is_some() {
                        finish();
                    }
                },
                std::time::Duration::from_millis(400),
            );
        }
    });

    let toggle_mode = {
        let reroute = reroute.clone();
        move |_| {
            let next = match mode.get_untracked() {
                ReaderMode::Paged => ReaderMode::Vertical,
                ReaderMode::Vertical => ReaderMode::Paged,
            };
            mode.set(next);
            offline::set_reader_mode(manga_id, next);
            // Leaving a continuous strip that wandered into another chapter:
            // paged mode is bound to the routed chapter, so re-route there.
            if next == ReaderMode::Paged {
                let current = current_chapter.get_untracked();
                if current != chapter_id {
                    reroute(format!(
                        "/read/{manga_id}/{current}?page={}",
                        page.get_untracked()
                    ));
                }
            }
        }
    };
    let cycle_fit = move |_| {
        let next = match fit.get_untracked() {
            ReaderFit::Screen => ReaderFit::Width,
            ReaderFit::Width => ReaderFit::Original,
            ReaderFit::Original => ReaderFit::Screen,
        };
        fit.set(next);
        offline::set_reader_fit(manga_id, next);
    };
    let toggle_dir = move |_| {
        let next = match dir.get_untracked() {
            ReaderDirection::Ltr => ReaderDirection::Rtl,
            ReaderDirection::Rtl => ReaderDirection::Ltr,
        };
        dir.set(next);
        offline::set_reader_direction(manga_id, next);
    };

    // Android shell: the reader runs edge-to-edge (bars overlay the page
    // instead of resizing the webview — no shift on chrome toggle) and
    // the system bars follow the reader chrome (no-ops elsewhere — see
    // offline::set_reading / set_immersive). Cleanup restores everything
    // however the reader is left, back gesture included.
    offline::set_reading(true);
    Effect::new(move |_| {
        offline::set_immersive(!chrome.get());
    });
    on_cleanup(|| {
        offline::set_immersive(false);
        offline::set_reading(false);
    });

    let request_view = request_turn.clone();
    let finish_view = finish_snap.clone();
    let report_scroll = report.clone();
    let client_paged = use_client();
    let client_vertical = use_client();

    // Page count of the chapter under the reader (the routed chapter until
    // a vertical strip wanders further).
    let shown_count = move || {
        segments
            .get()
            .iter()
            .find(|(id, _)| *id == current_chapter.get())
            .map(|(_, c)| *c)
            .unwrap_or_else(page_count)
    };
    let menu_open = RwSignal::new(false);

    // ‹ › on the pill turn a single page in both modes. In the vertical
    // strip a "turn" is a scroll to the adjacent page image — the scroll
    // handler then journals the new position like any user scroll.
    let go_page = {
        let request_turn = request_turn.clone();
        move |delta: i64| match mode.get_untracked() {
            ReaderMode::Paged => request_turn(delta),
            ReaderMode::Vertical => {
                let chapter = current_chapter.get_untracked();
                let count = shown_count().max(1) as i64;
                let current = page.get_untracked() as i64;
                let target = (current + delta).clamp(0, count - 1);
                if target == current {
                    return;
                }
                let selector = format!("img[data-chapter='{chapter}'][data-page='{target}']");
                if let Ok(Some(img)) = document().query_selector(&selector) {
                    img.scroll_into_view();
                }
            }
        }
    };

    // Arrow keys drive `go_page`, which routes per mode: paged turns the
    // pager, vertical scrolls the strip to the adjacent page (so the scroll
    // handler journals the position). Calling the paged-only `turn` here
    // used to move the counter and report a page the strip never scrolled to.
    let key_go_page = go_page.clone();
    let key_handle = window_event_listener(leptos::ev::keydown, move |ev| {
        // In RTL the previous page is on the right.
        let forward: i64 = match dir.get_untracked() {
            ReaderDirection::Ltr => 1,
            ReaderDirection::Rtl => -1,
        };
        let delta = match ev.key().as_str() {
            "ArrowLeft" => -forward,
            "ArrowRight" => forward,
            _ => return,
        };
        key_go_page(delta);
    });
    on_cleanup(move || key_handle.remove());

    // The Copy reactive state both stages read, copied into each stage
    // call so the moved bodies keep their original bindings.
    let cx = ReaderCtx {
        manga_id,
        chapter_id,
        initial_page,
        page,
        pos,
        drag,
        snap,
        fit,
        dir,
        chrome,
        current_chapter,
        segments,
        page_counts,
        detail,
    };

    view! {
        <div
            class="reader-overlay"
            class:chrome-hidden=move || !chrome.get()
            class:flow=move || mode.get() == ReaderMode::Vertical
        >
            <div class="reader-progress">
                <div
                    class="reader-progress-fill"
                    style:width=move || {
                        let count = shown_count().max(1) as f64;
                        format!("{}%", (page.get() + 1) as f64 / count * 100.0)
                    }
                ></div>
            </div>
            <div class="reader-chrome reader-top">
                <a href=format!("/manga/{manga_id}")>"← back"</a>
                <span class="reader-title">{chapter_title}</span>
            </div>

            {move || {
                let request = request_view.clone();
                let finish = finish_view.clone();
                let report = report_scroll.clone();
                match pages.get() {
                    None => view! { <p class="muted reader-msg">"Loading pages…"</p> }.into_any(),
                    Some(Err(err)) => {
                        // A chapter that isn't stored on this device needs
                        // the server — when that's what failed, say so and
                        // point at the connect form instead of dumping a
                        // bare transport error.
                        view! {
                            <div class="reader-msg reader-unavailable">
                                <p class="error">"This chapter isn't available"</p>
                                <p class="muted">
                                    "It isn't stored on this device and the server "
                                    "couldn't be reached (" {err.to_string()} ")."
                                </p>
                                <p class="reader-unavailable-actions">
                                    <a class="button" href=format!("/manga/{manga_id}")>
                                        "Back to the chapter list"
                                    </a>
                                    <a class="button primary" href="/more">
                                        "Connect to a server"
                                    </a>
                                </p>
                            </div>
                        }
                            .into_any()
                    }
                    Some(Ok(meta)) => match mode.get() {
                        ReaderMode::Paged => paged_stage(
                            cx,
                            client_paged.clone(),
                            request,
                            finish,
                            neighbour_ids,
                            page_count,
                            chapter_title,
                        ),
                        ReaderMode::Vertical => {
                            vertical_strip(cx, client_vertical.clone(), report, meta)
                        }
                    },
                }
            }}

            // Reader options, one layer under the pill's gear.
            {move || {
                let toggle_mode = toggle_mode.clone();
                menu_open.get()
                    .then(move || {
                        view! {
                            <div class="reader-chrome reader-menu">
                                <button
                                    class="mode-btn"
                                    title="Toggle paged / vertical"
                                    on:click=toggle_mode
                                >
                                    {move || match mode.get() {
                                        ReaderMode::Paged => "⇅ vertical",
                                        ReaderMode::Vertical => "⇆ paged",
                                    }}
                                </button>
                                {move || {
                                    (mode.get() == ReaderMode::Paged)
                                        .then(|| {
                                            view! {
                                                <button
                                                    class="mode-btn"
                                                    title="Page fit"
                                                    on:click=cycle_fit
                                                >
                                                    {move || match fit.get() {
                                                        ReaderFit::Screen => "fit: screen",
                                                        ReaderFit::Width => "fit: width",
                                                        ReaderFit::Original => "fit: 1:1",
                                                    }}
                                                </button>
                                                <button
                                                    class="mode-btn"
                                                    title="Reading direction (which side is the next page)"
                                                    on:click=toggle_dir
                                                >
                                                    {move || match dir.get() {
                                                        ReaderDirection::Ltr => "ltr →",
                                                        ReaderDirection::Rtl => "← rtl",
                                                    }}
                                                </button>
                                            }
                                        })
                                }}
                            </div>
                        }
                    })
            }}

            // ‹ › turn pages; the bar-style |‹ ›| jump chapters, so the
            // controls around the page counter actually act on pages.
            <div class="reader-chrome reader-bottom">
                {
                    let reroute = reroute.clone();
                    move || {
                        let reroute = reroute.clone();
                        neighbours()
                            .and_then(|(previous, _)| previous)
                            .map(|prev| {
                                view! {
                                    <button
                                        class="pill-btn pill-jump"
                                        title="Previous chapter"
                                        on:click=move |_| reroute(
                                            format!("/read/{manga_id}/{prev}"),
                                        )
                                    >
                                        "|‹"
                                    </button>
                                }
                            })
                    }
                }
                <button
                    class="pill-btn"
                    title="Previous page"
                    on:click={
                        let go_page = go_page.clone();
                        move |_| go_page(-1)
                    }
                >
                    "‹"
                </button>
                <span class="pill-counter">
                    {move || format!("{} / {}", page.get() + 1, shown_count().max(1))}
                </span>
                <button
                    class="pill-btn"
                    title="Next page"
                    on:click={
                        let go_page = go_page.clone();
                        move |_| go_page(1)
                    }
                >
                    "›"
                </button>
                {
                    let reroute = reroute.clone();
                    move || {
                        let reroute = reroute.clone();
                        neighbours()
                            .and_then(|(_, next)| next)
                            .map(|next| {
                                view! {
                                    <button
                                        class="pill-btn pill-jump"
                                        title="Next chapter"
                                        on:click=move |_| reroute(
                                            format!("/read/{manga_id}/{next}"),
                                        )
                                    >
                                        "›|"
                                    </button>
                                }
                            })
                    }
                }
                <span class="pill-sep"></span>
                <button
                    class="pill-btn"
                    title="Reader options"
                    on:click=move |_| menu_open.update(|o| *o = !*o)
                >
                    "⚙"
                </button>
            </div>
        </div>
    }
    .into_any()
}
