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

    view! {
        <div
            class="reader-overlay"
            class:chrome-hidden=move || !chrome.get()
            class:flow=move || mode.get() == ReaderMode::Vertical
        >
            <div class="reader-chrome reader-top">
                <a href=format!("/manga/{manga_id}")>"← back"</a>
                <span class="reader-title">{chapter_title}</span>
                <div class="reader-tools">
                    <button class="mode-btn" title="Toggle paged / vertical" on:click=toggle_mode>
                        {move || match mode.get() {
                            ReaderMode::Paged => "⇅ vertical",
                            ReaderMode::Vertical => "⇆ paged",
                        }}
                    </button>
                    {move || {
                        (mode.get() == ReaderMode::Paged)
                            .then(|| {
                                view! {
                                    <button class="mode-btn" title="Page fit" on:click=cycle_fit>
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
                    <button class="mode-btn" title="Fullscreen" on:click=toggle_fullscreen>
                        "⛶"
                    </button>
                </div>
                <span class="muted">
                    {move || {
                        let count = segments
                            .get()
                            .iter()
                            .find(|(id, _)| *id == current_chapter.get())
                            .map(|(_, c)| *c)
                            .unwrap_or_else(page_count);
                        format!("{} / {}", page.get() + 1, count.max(1))
                    }}
                </span>
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
                            let strip = NodeRef::<leptos::html::Div>::new();
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
                            });
                            // Only user scrolling moves the journal: the
                            // programmatic positioning above also fires
                            // scroll events, and while images are still
                            // placeholder-height they would map to a wrong
                            // page and overwrite the saved position.
                            let interacted = RwSignal::new(false);
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
                                loading_next.set_value(true);
                                let client = client_next.clone();
                                spawn_local(async move {
                                    let count = match client.chapter_pages(next).await {
                                        Ok(meta) => Some(meta.page_count),
                                        Err(_) => offline::device_chapter_pages(next),
                                    };
                                    if let Some(count) = count {
                                        segments.update(|s| s.push((next, count)));
                                    }
                                    loading_next.set_value(false);
                                });
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
                                    if let Ok(imgs) = el.query_selector_all("img[data-page]") {
                                        for i in 0..imgs.length() {
                                            let Some(img) = imgs
                                                .item(i)
                                                .and_then(|n| {
                                                    n.dyn_into::<web_sys::Element>().ok()
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
                                            }
                                        }
                                    }
                                    if interacted.get_untracked()
                                        && let Some((chapter, p)) = at
                                        && (chapter != current_chapter.get_untracked()
                                            || p != page.get_untracked())
                                    {
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
                                    }
                                    // near the bottom: extend the strip
                                    if el.get_bounding_client_rect().bottom()
                                        < viewport * 3.0
                                    {
                                        load_next();
                                    }
                                },
                            );
                            on_cleanup(move || scroll_handle.remove());
                            let client = client_vertical.clone();
                            view! {
                                <div
                                    class="reader-scroll"
                                    node_ref=strip
                                    on:wheel=move |_| interacted.set(true)
                                    on:touchstart=move |_| interacted.set(true)
                                    on:pointerdown=move |_| interacted.set(true)
                                    on:click=move |_| chrome.update(|c| *c = !*c)
                                >
                                    <For
                                        each=move || {
                                            segments.get().into_iter().enumerate().collect::<Vec<_>>()
                                        }
                                        key=|(_, (chapter, _))| *chapter
                                        children=move |(i, (chapter, count))| {
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
                                            let images = (0..count)
                                                .map(|n| {
                                                    let src = page_source(&client, chapter, n);
                                                    view! {
                                                        <img
                                                            class="reader-strip-page"
                                                            src=src
                                                            data-chapter=chapter.to_string()
                                                            data-page=n.to_string()
                                                            loading=if i == 0 && n < 3 {
                                                                "eager"
                                                            } else {
                                                                "lazy"
                                                            }
                                                            alt=""
                                                        />
                                                    }
                                                })
                                                .collect_view();
                                            view! {
                                                {(i > 0)
                                                    .then(|| {
                                                        view! {
                                                            <div class="strip-chapter-break">
                                                                {title}
                                                            </div>
                                                        }
                                                    })}
                                                {images}
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

            <div class="reader-chrome reader-bottom">
                {move || {
                    neighbours()
                        .and_then(|(previous, _)| previous)
                        .map(|prev| {
                            view! {
                                <a class="button" href=format!("/read/{manga_id}/{prev}")>
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
