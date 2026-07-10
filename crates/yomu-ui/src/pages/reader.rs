//! Immersive reader: fullscreen overlay, chrome toggled by a center tap,
//! paged and vertical (webtoon) modes. Progress is reported per page turn;
//! when the server is unreachable the event lands in the offline outbox and
//! is merged back later (see `offline`).

use chrono::Utc;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::JsCast;
use leptos_router::NavigateOptions;
use leptos_router::hooks::use_query_map;
use yomu_domain::{ProgressEvent, SetPositionRequest};

use super::{NotFound, param_uuid};
use crate::offline::{self, ReaderDirection, ReaderFit, ReaderMode};
use crate::pager;
use crate::use_client;

/// Touch-gesture bookkeeping for the paged stage (swipe / pinch / pan).
#[derive(Default)]
struct Gesture {
    /// First finger's start position, while a one-finger gesture is live.
    start: Option<(f64, f64)>,
    /// Pan offset when the drag started.
    pan0: (f64, f64),
    /// (finger distance, zoom) at the moment a pinch started.
    pinch0: Option<(f64, f64)>,
    /// The finger travelled: not a tap anymore.
    moved: bool,
    /// Horizontal intent at zoom 1: the drag drives the sliding track.
    h_capture: bool,
    /// Eat the synthetic click that follows a swipe/pinch/pan.
    suppress_click: bool,
}

fn touch_xy(touches: &web_sys::TouchList, index: u32) -> Option<(f64, f64)> {
    let touch = touches.item(index)?;
    Some((touch.client_x() as f64, touch.client_y() as f64))
}

fn touch_distance(touches: &web_sys::TouchList) -> Option<f64> {
    let (ax, ay) = touch_xy(touches, 0)?;
    let (bx, by) = touch_xy(touches, 1)?;
    Some(((ax - bx).powi(2) + (ay - by).powi(2)).sqrt())
}

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
    let pages = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move {
                match client.chapter_pages(chapter_id).await {
                    Ok(meta) => Ok(meta),
                    // Saved on this device: enough metadata to read with
                    // the server unreachable.
                    Err(err) => match offline::device_chapter_pages(chapter_id) {
                        Some(page_count) => Ok(yomu_domain::PagesResponse {
                            chapter_id,
                            page_count,
                            downloaded: false,
                        }),
                        None => Err(err),
                    },
                }
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
                offline::with_cache(&format!("manga:{manga_id}"), client.manga(manga_id).await)
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
                let req = SetPositionRequest {
                    chapter_id: chapter,
                    page: p,
                    device: "web".into(),
                };
                if client.set_position(manga_id, &req).await.is_err() {
                    offline::outbox_push(ProgressEvent {
                        id: offline::uuid_v7_js(),
                        manga_id,
                        chapter_id: chapter,
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
                if wants_end {
                    let last = page_count().saturating_sub(1);
                    page.set(last);
                    pos.set(last as i64);
                }
                report(chapter_id, page.get_untracked());
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
                report(chapter_id, next);
            }
        }
    };

    let neighbours = move || {
        let chapters = detail.get().and_then(|r| r.ok()).map(|d| d.chapters)?;
        let index = chapters
            .iter()
            .position(|c| c.id == current_chapter.get())?;
        let previous = index.checked_sub(1).map(|i| chapters[i].id);
        let next = chapters.get(index + 1).map(|c| c.id);
        Some((previous, next))
    };
    let chapter_title = move || {
        detail
            .get()
            .and_then(|r| r.ok())
            .and_then(|d| {
                d.chapters
                    .into_iter()
                    .find(|c| c.id == current_chapter.get())
            })
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

    let key_turn = turn.clone();
    let key_request = request_turn.clone();
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
        match mode.get_untracked() {
            ReaderMode::Paged => key_request(delta),
            ReaderMode::Vertical => key_turn(delta),
        }
    });
    on_cleanup(move || key_handle.remove());

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
    let commit_view = commit_pos.clone();
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
                let commit = commit_view.clone();
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
                        ReaderMode::Paged => {
                            let stage = NodeRef::<leptos::html::Div>::new();
                            // Width/original fits scroll; the next page must
                            // start at its top-left again.
                            Effect::new(move |_| {
                                pos.get();
                                if let Some(el) = stage.get() {
                                    el.set_scroll_top(0);
                                    el.set_scroll_left(0);
                                }
                            });
                            // Zoom (pinch / ctrl+wheel) and pan, reset on
                            // page turn.
                            let zoom = RwSignal::new(1.0_f64);
                            let pan = RwSignal::new((0.0_f64, 0.0_f64));
                            Effect::new(move |_| {
                                pos.get();
                                zoom.set(1.0);
                                pan.set((0.0, 0.0));
                            });
                            let gesture = StoredValue::new(Gesture::default());
                            let flick = StoredValue::new(pager::Flick::default());
                            let clamp_pan = move |(x, y): (f64, f64)| -> (f64, f64) {
                                let z = zoom.get_untracked();
                                let Some(el) = stage.get_untracked() else {
                                    return (0.0, 0.0);
                                };
                                let max_x = (z - 1.0) * el.client_width() as f64 / 2.0;
                                let max_y = (z - 1.0) * el.client_height() as f64 / 2.0;
                                (x.clamp(-max_x, max_x), y.clamp(-max_y, max_y))
                            };
                            // Tap zones by position instead of overlay
                            // buttons: buttons over the scroller would
                            // swallow touch panning once the page overflows.
                            let click_request = request.clone();
                            let on_click = move |ev: leptos::ev::MouseEvent| {
                                if gesture.with_value(|g| g.suppress_click) {
                                    gesture.update_value(|g| g.suppress_click = false);
                                    return;
                                }
                                let width = window()
                                    .inner_width()
                                    .ok()
                                    .and_then(|w| w.as_f64())
                                    .unwrap_or(0.0);
                                if width <= 0.0 {
                                    return;
                                }
                                let rtl = dir.get_untracked() == ReaderDirection::Rtl;
                                let x = ev.client_x() as f64;
                                if x < width / 3.0 {
                                    // left side: back in LTR, forward in RTL
                                    click_request(if rtl { 1 } else { -1 });
                                } else if x > width * 2.0 / 3.0 {
                                    click_request(if rtl { -1 } else { 1 });
                                } else {
                                    chrome.update(|c| *c = !*c);
                                }
                            };
                            let on_touchstart = move |ev: leptos::ev::TouchEvent| {
                                let touches = ev.touches();
                                if touches.length() == 2 {
                                    if let Some(d) = touch_distance(&touches) {
                                        gesture.update_value(|g| {
                                            g.pinch0 = Some((d, zoom.get_untracked()));
                                            g.start = None;
                                            g.moved = true;
                                        });
                                    }
                                } else if touches.length() == 1
                                    && let Some((x, y)) = touch_xy(&touches, 0)
                                {
                                    gesture.update_value(|g| {
                                        g.start = Some((x, y));
                                        g.pan0 = pan.get_untracked();
                                        g.moved = false;
                                        g.h_capture = false;
                                    });
                                    flick.update_value(|f| {
                                        f.clear();
                                        f.push(x, ev.time_stamp());
                                    });
                                }
                            };
                            let on_touchmove = move |ev: leptos::ev::TouchEvent| {
                                let touches = ev.touches();
                                if touches.length() == 2 {
                                    let Some((d0, z0)) = gesture.with_value(|g| g.pinch0)
                                    else {
                                        return;
                                    };
                                    let Some(d) = touch_distance(&touches) else {
                                        return;
                                    };
                                    ev.prevent_default();
                                    let z = (z0 * d / d0).clamp(1.0, 5.0);
                                    zoom.set(z);
                                    if z <= 1.0 {
                                        pan.set((0.0, 0.0));
                                    } else {
                                        pan.set(clamp_pan(pan.get_untracked()));
                                    }
                                } else if touches.length() == 1 {
                                    let Some((sx, sy)) = gesture.with_value(|g| g.start) else {
                                        return;
                                    };
                                    let Some((x, y)) = touch_xy(&touches, 0) else {
                                        return;
                                    };
                                    let (dx, dy) = (x - sx, y - sy);
                                    if dx.abs() > 10.0 || dy.abs() > 10.0 {
                                        gesture.update_value(|g| g.moved = true);
                                    }
                                    if zoom.get_untracked() > 1.0 {
                                        // drag pans the zoomed page
                                        ev.prevent_default();
                                        let (px, py) = gesture.with_value(|g| g.pan0);
                                        pan.set(clamp_pan((px + dx, py + dy)));
                                        return;
                                    }
                                    // horizontal intent captures the drag:
                                    // the track follows the finger
                                    if !gesture.with_value(|g| g.h_capture)
                                        && dx.abs() > 10.0
                                        && dx.abs() > dy.abs()
                                    {
                                        gesture.update_value(|g| g.h_capture = true);
                                    }
                                    if gesture.with_value(|g| g.h_capture)
                                        && snap.get_untracked().is_none()
                                    {
                                        ev.prevent_default();
                                        flick.update_value(|f| f.push(x, ev.time_stamp()));
                                        let rtl =
                                            dir.get_untracked() == ReaderDirection::Rtl;
                                        let (prev, next) = neighbour_ids();
                                        let target =
                                            pos.get_untracked() + pager::step(dx, rtl);
                                        // nothing (or a dead end) to reveal:
                                        // rubber-band
                                        let free = !matches!(
                                            pager::panel(
                                                target,
                                                page_count(),
                                                prev.is_some(),
                                                next.is_some(),
                                            ),
                                            pager::Panel::Empty | pager::Panel::DeadEnd
                                        );
                                        drag.set(if free { dx } else { pager::damp(dx) });
                                    }
                                }
                            };
                            let end_request = request.clone();
                            let on_touchend = move |ev: leptos::ev::TouchEvent| {
                                if ev.touches().length() > 0 {
                                    // finger lifted mid-pinch: wait for a
                                    // fresh touchstart before tracking again
                                    gesture.update_value(|g| g.start = None);
                                    return;
                                }
                                let (start, pinch, moved, h_capture) = gesture
                                    .with_value(|g| (g.start, g.pinch0, g.moved, g.h_capture));
                                gesture.update_value(|g| {
                                    g.start = None;
                                    g.pinch0 = None;
                                    g.h_capture = false;
                                    if moved || pinch.is_some() {
                                        g.suppress_click = true;
                                    }
                                });
                                if pinch.is_some()
                                    || !moved
                                    || !h_capture
                                    || zoom.get_untracked() > 1.0
                                {
                                    return;
                                }
                                let (Some((sx, _)), Some(touch)) =
                                    (start, ev.changed_touches().item(0))
                                else {
                                    return;
                                };
                                let dx = touch.client_x() as f64 - sx;
                                let width = window()
                                    .inner_width()
                                    .ok()
                                    .and_then(|w| w.as_f64())
                                    .unwrap_or(0.0)
                                    .max(1.0);
                                let velocity =
                                    flick.with_value(|f| f.velocity(ev.time_stamp()));
                                let rtl = dir.get_untracked() == ReaderDirection::Rtl;
                                if pager::verdict(dx, width, velocity)
                                    == pager::Verdict::Commit
                                {
                                    end_request(pager::step(dx, rtl));
                                }
                                // refused (dead end / navigation) or
                                // cancelled: spring back to center — but a
                                // sub-pixel offset may not transition (no
                                // transitionend), so reset it directly
                                if snap.get_untracked().is_none() {
                                    if drag.get_untracked().abs() > 1.0 {
                                        snap.set(Some(0));
                                    } else {
                                        drag.set(0.0);
                                    }
                                }
                            };
                            let on_wheel = move |ev: leptos::ev::WheelEvent| {
                                if !ev.ctrl_key() {
                                    return;
                                }
                                ev.prevent_default();
                                let z = (zoom.get_untracked() * (1.0 - ev.delta_y() * 0.002))
                                    .clamp(1.0, 5.0);
                                zoom.set(z);
                                if z <= 1.0 {
                                    pan.set((0.0, 0.0));
                                } else {
                                    pan.set(clamp_pan(pan.get_untracked()));
                                }
                            };
                            // titles for the transition panels
                            let title_of = move |id: uuid::Uuid| {
                                detail
                                    .get()
                                    .and_then(|r| r.ok())
                                    .and_then(|d| {
                                        d.chapters.into_iter().find(|c| c.id == id)
                                    })
                                    .map(|c| c.title)
                                    .unwrap_or_default()
                            };
                            // What one slot shows. The zoom transform reads
                            // shared signals, but neighbours sit at zoom 1
                            // whenever they are visible (drags don't turn
                            // while zoomed).
                            let panel_view = {
                                let client = client_paged.clone();
                                move |position: i64| {
                                    let (prev, next) = neighbour_ids();
                                    match pager::panel(
                                        position,
                                        page_count(),
                                        prev.is_some(),
                                        next.is_some(),
                                    ) {
                                        pager::Panel::Page(n) => {
                                            let src = page_source(&client, chapter_id, n);
                                            view! {
                                                <img
                                                    class="reader-page"
                                                    src=src
                                                    alt=""
                                                    style:transform=move || {
                                                        let z = zoom.get();
                                                        let (x, y) = pan.get();
                                                        if z > 1.0 {
                                                            format!(
                                                                "translate({x}px, {y}px) scale({z})"
                                                            )
                                                        } else {
                                                            String::new()
                                                        }
                                                    }
                                                />
                                            }
                                                .into_any()
                                        }
                                        pager::Panel::TransitionNext => {
                                            let next_title =
                                                next.map(title_of).unwrap_or_default();
                                            view! {
                                                <div class="reader-transition">
                                                    <span>"Finished:"</span>
                                                    <strong>{chapter_title}</strong>
                                                    <span>"Next up — keep turning:"</span>
                                                    <strong>{next_title}</strong>
                                                </div>
                                            }
                                                .into_any()
                                        }
                                        pager::Panel::TransitionPrev => {
                                            let prev_title =
                                                prev.map(title_of).unwrap_or_default();
                                            view! {
                                                <div class="reader-transition">
                                                    <span>"Start of:"</span>
                                                    <strong>{chapter_title}</strong>
                                                    <span>"Previous — keep turning:"</span>
                                                    <strong>{prev_title}</strong>
                                                </div>
                                            }
                                                .into_any()
                                        }
                                        pager::Panel::DeadEnd => view! {
                                            <div class="reader-transition">
                                                <span>"No more chapters this way"</span>
                                                <a
                                                    class="button"
                                                    href=format!("/manga/{manga_id}")
                                                >
                                                    "Back to the chapter list"
                                                </a>
                                            </div>
                                        }
                                            .into_any(),
                                        pager::Panel::Empty => ().into_any(),
                                    }
                                }
                            };
                            let pv_left = panel_view.clone();
                            let pv_center = panel_view.clone();
                            let pv_right = panel_view;
                            // DOM slots are physical (left / center /
                            // right); RTL puts the next position on the left.
                            let slot = move |physical: i64| {
                                let rtl = dir.get() == ReaderDirection::Rtl;
                                pos.get() + if rtl { -physical } else { physical }
                            };
                            let track_transform = move || {
                                let rtl = dir.get() == ReaderDirection::Rtl;
                                match snap.get() {
                                    Some(delta) => {
                                        let shift =
                                            if rtl { -delta } else { delta } as f64;
                                        format!(
                                            "translateX({}%)",
                                            -33.3333 - shift * 33.3333
                                        )
                                    }
                                    None => {
                                        let px = drag.get();
                                        if px == 0.0 {
                                            "translateX(-33.3333%)".to_string()
                                        } else {
                                            format!("translateX(calc(-33.3333% + {px}px))")
                                        }
                                    }
                                }
                            };
                            let on_transitionend = {
                                let commit = commit.clone();
                                move |ev: leptos::ev::TransitionEvent| {
                                    if ev.property_name() != "transform" {
                                        return;
                                    }
                                    if let Some(delta) = snap.get_untracked() {
                                        snap.set(None);
                                        drag.set(0.0);
                                        commit(pos.get_untracked() + delta);
                                    }
                                }
                            };
                            view! {
                                <div
                                    class="reader-pager"
                                    on:click=on_click
                                    on:touchstart=on_touchstart
                                    on:touchmove=on_touchmove
                                    on:touchend=on_touchend
                                    on:wheel=on_wheel
                                >
                                    <div
                                        class="reader-track"
                                        class:snap=move || snap.get().is_some()
                                        class:fit-screen=move || {
                                            fit.get() == ReaderFit::Screen
                                        }
                                        class:fit-width=move || {
                                            fit.get() == ReaderFit::Width
                                        }
                                        class:fit-original=move || {
                                            fit.get() == ReaderFit::Original
                                        }
                                        style:transform=track_transform
                                        on:transitionend=on_transitionend
                                    >
                                        <div class="reader-panel">
                                            {move || pv_left(slot(-1))}
                                        </div>
                                        <div class="reader-panel" node_ref=stage>
                                            {move || pv_center(slot(0))}
                                        </div>
                                        <div class="reader-panel">
                                            {move || pv_right(slot(1))}
                                        </div>
                                    </div>
                                </div>
                            }
                                .into_any()
                        }
                        ReaderMode::Vertical => {
                            // Continuous strip: starts with the routed chapter
                            // and appends the next one when the reader nears
                            // the bottom. The document itself scrolls (`.flow`
                            // in styles.css) — mobile browsers only collapse
                            // their address bar for the root scroller.
                            if segments.with_untracked(|s| s.is_empty()) {
                                segments.set(vec![(chapter_id, meta.page_count)]);
                            }
                            page_counts.update_value(|counts| {
                                counts.insert(chapter_id, meta.page_count);
                            });
                            let strip = NodeRef::<leptos::html::Div>::new();
                            // (sum, count) of loaded page heights: keeps
                            // --strip-placeholder at the average page.
                            // Seeded from the last session so the opening
                            // geometry is realistic before any load.
                            let loaded_avg = StoredValue::new(
                                match offline::page_height_hint(manga_id) {
                                    Some(h) => (h, 1_u32),
                                    None => (0.0_f64, 0_u32),
                                },
                            );
                            // Glue deferral: a compensation scrollBy landing
                            // mid-fling cancels the fling on touch devices
                            // (the "snap after release"). While scroll events
                            // are streaming, deltas accumulate and land on
                            // scrollend / the next idle load instead.
                            let last_scroll_ts = StoredValue::new(0.0_f64);
                            let pending_glue = StoredValue::new(0.0_f64);
                            // Scroll events only count once the programmatic
                            // opening scroll below has landed; before that
                            // they would map placeholder-height images to a
                            // wrong page and overwrite the saved position.
                            // Deliberately NOT gated on pointer/wheel input:
                            // desktop mice scroll with the scrollbar and
                            // keyboards page with the arrows, neither of
                            // which touches the strip element.
                            let positioned = StoredValue::new(false);
                            // Start at the current page, not the top: entering
                            // vertical mode (or "continue reading") must not
                            // rewind the saved position.
                            Effect::new(move |_| {
                                let Some(el) = strip.get() else { return };
                                // placeholders at the learned page height
                                // before positioning against them
                                let (sum, n) = loaded_avg.get_value();
                                if n > 0 {
                                    let _ = web_sys::HtmlElement::style(&el).set_property(
                                        "--strip-placeholder",
                                        &format!("{}px", sum / n as f64),
                                    );
                                }
                                let selector = format!(
                                    "img[data-chapter='{}'][data-page='{}']",
                                    current_chapter.get_untracked(),
                                    page.get_untracked(),
                                );
                                if let Ok(Some(child)) = el.query_selector(&selector) {
                                    child.scroll_into_view();
                                }
                                positioned.set_value(true);
                            });
                            let loading_next = StoredValue::new(false);
                            let client_next = client_vertical.clone();
                            let load_next = move || {
                                if loading_next.get_value() {
                                    return;
                                }
                                let Some(chapters) =
                                    detail.get_untracked().and_then(|r| r.ok()).map(|d| d.chapters)
                                else {
                                    return;
                                };
                                let Some(last) =
                                    segments.with_untracked(|s| s.last().map(|(id, _)| *id))
                                else {
                                    return;
                                };
                                let Some(index) = chapters.iter().position(|c| c.id == last)
                                else {
                                    return;
                                };
                                let Some(next) = chapters.get(index + 1).map(|c| c.id) else {
                                    return; // already at the last chapter
                                };
                                if let Some(count) =
                                    page_counts.with_value(|counts| counts.get(&next).copied())
                                {
                                    segments.update(|s| s.push((next, count)));
                                    return;
                                }
                                loading_next.set_value(true);
                                let client = client_next.clone();
                                spawn_local(async move {
                                    let count = match client.chapter_pages(next).await {
                                        Ok(meta) => Some(meta.page_count),
                                        Err(_) => offline::device_chapter_pages(next),
                                    };
                                    if let Some(count) = count {
                                        page_counts.update_value(|counts| {
                                            counts.insert(next, count);
                                        });
                                        segments.update(|s| s.push((next, count)));
                                    }
                                    loading_next.set_value(false);
                                });
                            };
                            // Scrolling up past the strip's start pulls the
                            // previous chapter in *above* the viewport, so
                            // the scroll position must be pushed down by the
                            // new segment's height to keep the view still.
                            // Native scroll anchoring would do the same
                            // (doubling the shift where supported), so it is
                            // disabled on the strip (styles.css) and all
                            // compensation is done by hand.
                            let loading_prev = StoredValue::new(false);
                            let client_prev = client_vertical.clone();
                            let load_prev = move || {
                                if loading_prev.get_value() {
                                    return;
                                }
                                let Some(chapters) =
                                    detail.get_untracked().and_then(|r| r.ok()).map(|d| d.chapters)
                                else {
                                    return;
                                };
                                let Some(first) =
                                    segments.with_untracked(|s| s.first().map(|(id, _)| *id))
                                else {
                                    return;
                                };
                                let Some(index) = chapters.iter().position(|c| c.id == first)
                                else {
                                    return;
                                };
                                let Some(prev) = index.checked_sub(1).map(|i| chapters[i].id)
                                else {
                                    return; // already at the first chapter
                                };
                                let prepend = move |count: u32| {
                                    segments.update(|s| s.insert(0, (prev, count)));
                                    request_animation_frame(move || {
                                        let Some(el) = strip.get_untracked() else { return };
                                        if let Ok(Some(wrap)) = el.query_selector(&format!(
                                            ".strip-chapter[data-chapter='{prev}']"
                                        )) && let Ok(wrap) =
                                            wrap.dyn_into::<web_sys::HtmlElement>()
                                        {
                                            window().scroll_by_with_x_and_y(
                                                0.0,
                                                wrap.offset_height() as f64,
                                            );
                                        }
                                    });
                                };
                                if let Some(count) =
                                    page_counts.with_value(|counts| counts.get(&prev).copied())
                                {
                                    prepend(count);
                                    return;
                                }
                                loading_prev.set_value(true);
                                let client = client_prev.clone();
                                spawn_local(async move {
                                    let count = match client.chapter_pages(prev).await {
                                        Ok(meta) => Some(meta.page_count),
                                        Err(_) => offline::device_chapter_pages(prev),
                                    };
                                    if let Some(count) = count {
                                        page_counts.update_value(|counts| {
                                            counts.insert(prev, count);
                                        });
                                        prepend(count);
                                    }
                                    loading_prev.set_value(false);
                                });
                            };
                            // A wheel-up with the strip already at scroll
                            // position 0 produces no scroll event, so the
                            // intent to read backwards would go unseen —
                            // catch it on the wheel event itself.
                            let wheel_prev = {
                                let load_prev = load_prev.clone();
                                move |ev: leptos::ev::WheelEvent| {
                                    if ev.delta_y() >= 0.0 {
                                        return;
                                    }
                                    let Some(el) = strip.get_untracked() else { return };
                                    let viewport = window()
                                        .inner_height()
                                        .ok()
                                        .and_then(|h| h.as_f64())
                                        .unwrap_or(0.0);
                                    if el.get_bounding_client_rect().top() > -viewport * 3.0 {
                                        load_prev();
                                    }
                                }
                            };
                            let scroll_handle = window_event_listener(
                                leptos::ev::scroll,
                                move |_| {
                                    last_scroll_ts.set_value(js_sys::Date::now());
                                    let Some(el) = strip.get_untracked() else { return };
                                    let viewport = window()
                                        .inner_height()
                                        .ok()
                                        .and_then(|h| h.as_f64())
                                        .unwrap_or(0.0);
                                    // the page under the viewport's midline
                                    let middle = viewport / 2.0;
                                    let mut at: Option<(uuid::Uuid, u32)> = None;
                                    // A placeholder image spans a fraction of
                                    // its real height, so a position computed
                                    // from one would be pages off — only
                                    // report positions read off loaded images.
                                    let mut at_loaded = false;
                                    if let Ok(imgs) = el.query_selector_all("img[data-page]") {
                                        for i in 0..imgs.length() {
                                            let Some(img) = imgs
                                                .item(i)
                                                .and_then(|n| {
                                                    n.dyn_into::<web_sys::HtmlImageElement>().ok()
                                                })
                                            else {
                                                continue;
                                            };
                                            if img.get_bounding_client_rect().top() > middle {
                                                break;
                                            }
                                            let position = img
                                                .get_attribute("data-chapter")
                                                .and_then(|c| c.parse::<uuid::Uuid>().ok())
                                                .zip(
                                                    img.get_attribute("data-page")
                                                        .and_then(|p| p.parse::<u32>().ok()),
                                                );
                                            if position.is_some() {
                                                at = position;
                                                at_loaded = img.complete();
                                            }
                                        }
                                    }
                                    if positioned.get_value()
                                        && at_loaded
                                        && let Some((chapter, p)) = at
                                        && (chapter != current_chapter.get_untracked()
                                            || p != page.get_untracked())
                                    {
                                        let chapter_changed =
                                            chapter != current_chapter.get_untracked();
                                        current_chapter.set(chapter);
                                        page.set(p);
                                        report(chapter, p);
                                        // keep the URL honest for a refresh,
                                        // without waking the router (which
                                        // would remount the reader and lose
                                        // the strip)
                                        if let Ok(history) = window().history() {
                                            let _ = history.replace_state_with_url(
                                                &leptos::wasm_bindgen::JsValue::NULL,
                                                "",
                                                Some(&format!(
                                                    "/read/{manga_id}/{chapter}?page={p}"
                                                )),
                                            );
                                        }
                                        // Bound the strip to two chapters
                                        // either side of the one being read;
                                        // evicted chapters come back through
                                        // load_prev/load_next and the browser
                                        // HTTP cache when scrolled towards
                                        // again. Segments removed *above* the
                                        // viewport shift it up, so their
                                        // measured height is scrolled away.
                                        if chapter_changed {
                                            let (front, back) =
                                                segments.with_untracked(|s| {
                                                    let Some(idx) = s
                                                        .iter()
                                                        .position(|(id, _)| *id == chapter)
                                                    else {
                                                        return (Vec::new(), Vec::new());
                                                    };
                                                    let evicted = |i: usize| {
                                                        s[i].0
                                                    };
                                                    let front: Vec<uuid::Uuid> = (0..idx
                                                        .saturating_sub(2))
                                                        .map(evicted)
                                                        .collect();
                                                    let back: Vec<uuid::Uuid> =
                                                        (idx + 3..s.len()).map(evicted).collect();
                                                    (front, back)
                                                });
                                            if !front.is_empty() || !back.is_empty() {
                                                let mut above = 0.0;
                                                for id in &front {
                                                    if let Ok(Some(wrap)) = el.query_selector(
                                                        &format!(
                                                            ".strip-chapter[data-chapter='{id}']"
                                                        ),
                                                    ) && let Ok(wrap) =
                                                        wrap.dyn_into::<web_sys::HtmlElement>()
                                                    {
                                                        above += wrap.offset_height() as f64;
                                                    }
                                                }
                                                let drop: std::collections::HashSet<uuid::Uuid> =
                                                    front.into_iter().chain(back).collect();
                                                segments.update(|s| {
                                                    s.retain(|(id, _)| !drop.contains(id));
                                                });
                                                if above > 0.0 {
                                                    request_animation_frame(move || {
                                                        window()
                                                            .scroll_by_with_x_and_y(0.0, -above);
                                                    });
                                                }
                                            }
                                        }
                                    }
                                    let rect = el.get_bounding_client_rect();
                                    // near the bottom: extend the strip
                                    if rect.bottom() < viewport * 3.0 {
                                        load_next();
                                    }
                                    // near the top (and past the programmatic
                                    // positioning scroll): pull the previous
                                    // chapter in
                                    if positioned.get_value() && rect.top() > -viewport * 3.0 {
                                        load_prev();
                                    }
                                },
                            );
                            on_cleanup(move || scroll_handle.remove());
                            // The fling is over: land any parked glue. On
                            // engines without scrollend the next idle load
                            // flushes it instead.
                            let scrollend_handle = window_event_listener(
                                leptos::ev::Custom::<web_sys::Event>::new("scrollend"),
                                move |_| {
                                    let parked = pending_glue.get_value();
                                    if parked.abs() > 1.0 {
                                        pending_glue.set_value(0.0);
                                        window().scroll_by_with_x_and_y(0.0, parked);
                                    }
                                },
                            );
                            on_cleanup(move || scrollend_handle.remove());
                            let client = client_vertical.clone();
                            view! {
                                <div
                                    class="reader-scroll"
                                    node_ref=strip
                                    on:wheel=wheel_prev
                                    on:click=move |_| chrome.update(|c| *c = !*c)
                                >
                                    <For
                                        each=move || segments.get()
                                        key=|(chapter, _)| *chapter
                                        children=move |(chapter, count)| {
                                            let title = move || {
                                                detail
                                                    .get()
                                                    .and_then(|r| r.ok())
                                                    .and_then(|d| {
                                                        d.chapters
                                                            .into_iter()
                                                            .find(|c| c.id == chapter)
                                                    })
                                                    .map(|c| c.title)
                                                    .unwrap_or_default()
                                            };
                                            // A lazily loaded image is held at
                                            // the placeholder height by CSS
                                            // until data-loaded is set, so the
                                            // handler can measure the true
                                            // pre-growth geometry, reveal the
                                            // natural size, and measure again.
                                            // When the placeholder sat fully
                                            // above the reading line (the
                                            // viewport midline, where the
                                            // scroll handler reads positions)
                                            // its growth pushes the line
                                            // content away — the scroll
                                            // follows by the same amount.
                                            // Deciding on pre-growth geometry
                                            // matters: pages can run several
                                            // viewports tall, so a page loaded
                                            // out of order grows from "fully
                                            // above" into "straddling", and
                                            // the old post-growth check read
                                            // that as the page being read and
                                            // skipped it — a several-page jump
                                            // back at chapter transitions. A
                                            // placeholder at or under the line
                                            // grows below it: nothing to do.
                                            let on_load = move |ev: leptos::ev::Event| {
                                                let Some(img) = ev
                                                    .target()
                                                    .and_then(|t| {
                                                        t.dyn_into::<web_sys::HtmlElement>().ok()
                                                    })
                                                else {
                                                    return;
                                                };
                                                let viewport = window()
                                                    .inner_height()
                                                    .ok()
                                                    .and_then(|h| h.as_f64())
                                                    .unwrap_or(0.0);
                                                let middle = viewport / 2.0;
                                                let before = img.get_bounding_client_rect();
                                                // Placeholders still waiting,
                                                // fully above the line — counted
                                                // BEFORE anything moves (the
                                                // reveal below shifts them).
                                                let mut above = 0.0;
                                                if let Some(el) = strip.get_untracked()
                                                    && let Ok(pending) = el.query_selector_all(
                                                        "img.reader-strip-page:not([data-loaded])",
                                                    )
                                                {
                                                    for i in 0..pending.length() {
                                                        let Some(other) = pending
                                                            .item(i)
                                                            .and_then(|n| {
                                                                n.dyn_into::<web_sys::Element>()
                                                                    .ok()
                                                            })
                                                        else {
                                                            continue;
                                                        };
                                                        if !other.is_same_node(Some(&img))
                                                            && other
                                                                .get_bounding_client_rect()
                                                                .bottom()
                                                                <= middle
                                                        {
                                                            above += 1.0;
                                                        }
                                                    }
                                                }
                                                let _ =
                                                    img.set_attribute("data-loaded", "1");
                                                let after = img.get_bounding_client_rect();
                                                // Own growth, when the
                                                // placeholder sat fully above
                                                // the line.
                                                let mut delta = if before.bottom() <= middle {
                                                    after.height() - before.height()
                                                } else {
                                                    0.0
                                                };
                                                // Keep the waiting placeholders
                                                // near the average loaded page —
                                                // and compensate that re-size
                                                // too: every placeholder still
                                                // fully above the line grows by
                                                // the average's change, which
                                                // would otherwise shove the
                                                // line content down by pages.
                                                let old_avg = loaded_avg.with_value(
                                                    |(sum, n)| {
                                                        if *n > 0 {
                                                            *sum / *n as f64
                                                        } else {
                                                            // CSS fallback height
                                                            viewport * 0.85
                                                        }
                                                    },
                                                );
                                                loaded_avg.update_value(|(sum, n)| {
                                                    *sum += after.height();
                                                    *n += 1;
                                                });
                                                let new_avg = loaded_avg
                                                    .with_value(|(sum, n)| *sum / *n as f64);
                                                offline::set_page_height_hint(
                                                    manga_id, new_avg,
                                                );
                                                if let Some(el) = strip.get_untracked() {
                                                    let _ = web_sys::HtmlElement::style(&el)
                                                        .set_property(
                                                            "--strip-placeholder",
                                                            &format!("{new_avg}px"),
                                                        );
                                                }
                                                delta += above * (new_avg - old_avg);
                                                // Mid-fling, a scrollBy would
                                                // cancel the fling ("snap
                                                // after release" on phones):
                                                // park the correction until
                                                // the scroll settles.
                                                if js_sys::Date::now()
                                                    - last_scroll_ts.get_value()
                                                    < 150.0
                                                {
                                                    pending_glue
                                                        .update_value(|p| *p += delta);
                                                } else {
                                                    let delta =
                                                        delta + pending_glue.get_value();
                                                    pending_glue.set_value(0.0);
                                                    if delta.abs() > 1.0 {
                                                        window()
                                                            .scroll_by_with_x_and_y(0.0, delta);
                                                    }
                                                }
                                            };
                                            let images = (0..count)
                                                .map(|n| {
                                                    let src = page_source(&client, chapter, n);
                                                    view! {
                                                        <img
                                                            class="reader-strip-page"
                                                            src=src
                                                            data-chapter=chapter.to_string()
                                                            data-page=n.to_string()
                                                            // every segment's opening
                                                            // pages load eagerly, so a
                                                            // chapter crossing lands on
                                                            // real content instead of
                                                            // racing placeholders
                                                            loading=if n < 3 {
                                                                "eager"
                                                            } else {
                                                                "lazy"
                                                            }
                                                            on:load=on_load
                                                            alt=""
                                                        />
                                                    }
                                                })
                                                .collect_view();
                                            view! {
                                                <div
                                                    class="strip-chapter"
                                                    data-chapter=chapter.to_string()
                                                >
                                                    <div class="strip-chapter-break">{title}</div>
                                                    {images}
                                                </div>
                                            }
                                        }
                                    />
                                </div>
                            }
                                .into_any()
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
