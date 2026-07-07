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
use crate::offline::{self, ReaderDirection, ReaderFit, ReaderMode};
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

    let initial_page: u32 = use_query_map()
        .get_untracked()
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let page = RwSignal::new(initial_page);
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
                let key = format!("manga:{manga_id}");
                match client.manga(manga_id).await {
                    Ok(detail) => {
                        offline::cache_put(&key, &detail);
                        Ok(detail)
                    }
                    // offline: chapter title + prev/next come from the
                    // last-known-good copy the manga page stored
                    Err(err) => offline::cache_get(&key).ok_or(err),
                }
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

    let key_turn = turn.clone();
    let key_handle = window_event_listener(leptos::ev::keydown, move |ev| {
        // In RTL the previous page is on the right.
        let forward: i64 = match dir.get_untracked() {
            ReaderDirection::Ltr => 1,
            ReaderDirection::Rtl => -1,
        };
        match ev.key().as_str() {
            "ArrowLeft" => key_turn(-forward),
            "ArrowRight" => key_turn(forward),
            _ => {}
        }
    });
    on_cleanup(move || key_handle.remove());

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
    let toggle_mode = move |_| {
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
                navigate(
                    &format!("/read/{manga_id}/{current}?page={}", page.get_untracked()),
                    Default::default(),
                );
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
    let toggle_fullscreen = move |_| {
        let doc = document();
        if doc.fullscreen_element().is_some() {
            doc.exit_fullscreen();
        } else if let Some(root) = doc.document_element() {
            let _ = root.request_fullscreen();
        }
    };

    let turn_paged = turn.clone();
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
        let turn = turn.clone();
        move |delta: i64| match mode.get_untracked() {
            ReaderMode::Paged => turn(delta),
            ReaderMode::Vertical => {
                let chapter = current_chapter.get_untracked();
                let count = shown_count().max(1) as i64;
                let current = page.get_untracked() as i64;
                let target = (current + delta).clamp(0, count - 1);
                if target == current {
                    return;
                }
                let selector =
                    format!("img[data-chapter='{chapter}'][data-page='{target}']");
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
                let turn = turn_paged.clone();
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
                            let src = page_source(&client_paged, chapter_id, page.get());
                            let stage = NodeRef::<leptos::html::Div>::new();
                            // Width/original fits scroll; the next page must
                            // start at its top-left again.
                            Effect::new(move |_| {
                                page.get();
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
                                page.get();
                                zoom.set(1.0);
                                pan.set((0.0, 0.0));
                            });
                            let gesture = StoredValue::new(Gesture::default());
                            let clamp_pan = move |(x, y): (f64, f64)| -> (f64, f64) {
                                let z = zoom.get_untracked();
                                let Some(el) = stage.get_untracked() else {
                                    return (0.0, 0.0);
                                };
                                let max_x = (z - 1.0) * el.client_width() as f64 / 2.0;
                                let max_y = (z - 1.0) * el.client_height() as f64 / 2.0;
                                (x.clamp(-max_x, max_x), y.clamp(-max_y, max_y))
                            };
                            // forward = next page in reading order; which
                            // physical side that is depends on direction.
                            let go = {
                                let turn = turn.clone();
                                move |forward: bool| {
                                    turn(if forward { 1 } else { -1 });
                                }
                            };
                            // Tap zones by position instead of overlay
                            // buttons: buttons over the scroller would
                            // swallow touch panning once the page overflows.
                            let click_go = go.clone();
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
                                    click_go(rtl);
                                } else if x > width * 2.0 / 3.0 {
                                    click_go(!rtl);
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
                                    && let Some(pos) = touch_xy(&touches, 0)
                                {
                                    gesture.update_value(|g| {
                                        g.start = Some(pos);
                                        g.pan0 = pan.get_untracked();
                                        g.moved = false;
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
                                    }
                                }
                            };
                            let swipe_go = go.clone();
                            let on_touchend = move |ev: leptos::ev::TouchEvent| {
                                if ev.touches().length() > 0 {
                                    // finger lifted mid-pinch: wait for a
                                    // fresh touchstart before tracking again
                                    gesture.update_value(|g| g.start = None);
                                    return;
                                }
                                let (start, pinch, moved) =
                                    gesture.with_value(|g| (g.start, g.pinch0, g.moved));
                                gesture.update_value(|g| {
                                    g.start = None;
                                    g.pinch0 = None;
                                    if moved || pinch.is_some() {
                                        g.suppress_click = true;
                                    }
                                });
                                if pinch.is_some() || !moved || zoom.get_untracked() > 1.0 {
                                    return;
                                }
                                let (Some((sx, sy)), Some(touch)) =
                                    (start, ev.changed_touches().item(0))
                                else {
                                    return;
                                };
                                let dx = touch.client_x() as f64 - sx;
                                let dy = touch.client_y() as f64 - sy;
                                if dx.abs() > 60.0 && dx.abs() > 2.0 * dy.abs() {
                                    // swiping left pulls in the page that
                                    // sits on the right
                                    let rtl = dir.get_untracked() == ReaderDirection::Rtl;
                                    swipe_go(if dx < 0.0 { !rtl } else { rtl });
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
                            view! {
                                <div
                                    class="reader-stage"
                                    class:fit-screen=move || fit.get() == ReaderFit::Screen
                                    class:fit-width=move || fit.get() == ReaderFit::Width
                                    class:fit-original=move || fit.get() == ReaderFit::Original
                                    node_ref=stage
                                    on:click=on_click
                                    on:touchstart=on_touchstart
                                    on:touchmove=on_touchmove
                                    on:touchend=on_touchend
                                    on:wheel=on_wheel
                                >
                                    <img
                                        class="reader-page"
                                        src=src
                                        alt=""
                                        style:transform=move || {
                                            let z = zoom.get();
                                            let (x, y) = pan.get();
                                            if z > 1.0 {
                                                format!("translate({x}px, {y}px) scale({z})")
                                            } else {
                                                String::new()
                                            }
                                        }
                                    />
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
                                            // A lazily loaded image above the
                                            // viewport grows from its
                                            // placeholder height when it
                                            // arrives; shift the scroll by the
                                            // difference so the view doesn't
                                            // jump while reading backwards.
                                            let on_load = move |ev: leptos::ev::Event| {
                                                let Some(img) = ev
                                                    .target()
                                                    .and_then(|t| {
                                                        t.dyn_into::<web_sys::HtmlElement>().ok()
                                                    })
                                                else {
                                                    return;
                                                };
                                                let rect = img.get_bounding_client_rect();
                                                if rect.top() >= 0.0 {
                                                    return;
                                                }
                                                let placeholder = window()
                                                    .get_computed_style(&img)
                                                    .ok()
                                                    .flatten()
                                                    .and_then(|s| {
                                                        s.get_property_value("min-height").ok()
                                                    })
                                                    .and_then(|v| {
                                                        v.strip_suffix("px")?.parse::<f64>().ok()
                                                    })
                                                    .unwrap_or(0.0);
                                                let delta = rect.height() - placeholder;
                                                if delta.abs() > 1.0 {
                                                    window().scroll_by_with_x_and_y(0.0, delta);
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
                                                            loading=if chapter == chapter_id
                                                                && n < 3
                                                            {
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
                {move || {
                    neighbours()
                        .and_then(|(previous, _)| previous)
                        .map(|prev| {
                            view! {
                                <a
                                    class="pill-btn"
                                    title="Previous chapter"
                                    href=format!("/read/{manga_id}/{prev}")
                                >
                                    "|‹"
                                </a>
                            }
                        })
                }}
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
                {move || {
                    neighbours()
                        .and_then(|(_, next)| next)
                        .map(|next| {
                            view! {
                                <a
                                    class="pill-btn"
                                    title="Next chapter"
                                    href=format!("/read/{manga_id}/{next}")
                                >
                                    "›|"
                                </a>
                            }
                        })
                }}
                <span class="pill-sep"></span>
                <button
                    class="pill-btn"
                    title="Reader options"
                    on:click=move |_| menu_open.update(|o| *o = !*o)
                >
                    "⚙"
                </button>
                <button class="pill-btn" title="Fullscreen" on:click=toggle_fullscreen>
                    "⛶"
                </button>
            </div>
        </div>
    }
    .into_any()
}
