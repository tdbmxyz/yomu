//! Readium-inspired format navigators. The host owns chrome and persistence;
//! each navigator reports locations only after displaying the content.
use chrono::Utc;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::JsCast;
use yomu_domain::{
    Locations, Locator, ProgressEvent, PublicationDetailResponse, SetLocatorRequest,
};

use super::chrome::{ReaderShell, ReaderTop};
use crate::{offline, use_client};

type Frame = NodeRef<leptos::html::Iframe>;

fn reporter(
    publication_id: uuid::Uuid,
    unit_id: uuid::Uuid,
    title: String,
) -> impl Fn(Locations, Option<u32>) + Clone + 'static {
    let library = crate::cache::use_library_cache();
    let details = crate::cache::use_detail_cache();
    let client = use_client();
    move |locations, page_count| {
        let locator = Locator {
            unit_id,
            locations,
            at: Utc::now(),
        };
        crate::cache::patch_locator(library, publication_id, &locator, Some(title.clone()));
        crate::cache::patch_detail_locator(details, publication_id, &locator);
        let client = client.clone();
        let event = ProgressEvent {
            id: offline::uuid_v7_js(),
            publication_id,
            unit_id,
            page: locator.page(),
            progression: locator.progression(),
            device: "web".into(),
            at: locator.at,
        };
        let request = SetLocatorRequest {
            unit_id,
            page: event.page,
            progression: event.progression,
            page_count,
            device: "web".into(),
        };
        spawn_local(async move {
            if client.set_locator(publication_id, &request).await.is_err() {
                offline::outbox_push(event);
            }
        });
        if let Ok(history) = window().history() {
            let _ = history.replace_state_with_url(
                &leptos::wasm_bindgen::JsValue::NULL,
                "",
                Some(&format!(
                    "/read/{publication_id}/{unit_id}?{}",
                    locator.query()
                )),
            );
        }
    }
}

fn initial_location(
    detail: &PublicationDetailResponse,
    unit_id: uuid::Uuid,
    reflowable: bool,
) -> Locations {
    let query = leptos_router::hooks::use_query_map().get_untracked();
    let locator = offline::effective_position(
        detail.publication.id,
        detail.locator.clone(),
        &offline::outbox(),
    )
    .filter(|l| l.unit_id == unit_id);
    if reflowable {
        let value = query
            .get("progression")
            .and_then(|v| v.parse::<f64>().ok())
            .or_else(|| locator.as_ref().and_then(Locator::progression))
            .filter(|v| v.is_finite())
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        Locations::Progression {
            progression: value,
            page: 0,
        }
    } else {
        let page = query
            .get("page")
            .and_then(|v| v.parse().ok())
            .or_else(|| locator.as_ref().map(Locator::page))
            .unwrap_or(0);
        Locations::Page { page }
    }
}

fn resource_suffix(path: &str) -> Option<&str> {
    path.split_once("/resources/")?
        .1
        .split_once('/')
        .map(|(_, suffix)| suffix)
}

fn from_frame(event: &web_sys::MessageEvent, frame: Frame) -> bool {
    let Some(expected) = frame.get_untracked().and_then(|f| f.content_window()) else {
        return false;
    };
    event
        .source()
        .is_some_and(|source| js_sys::Object::is(source.as_ref(), expected.as_ref()))
}

fn command(frame: Frame, message: &str) {
    if let Some(target) = frame.get_untracked().and_then(|f| f.content_window()) {
        let _ = target.post_message(&leptos::wasm_bindgen::JsValue::from_str(message), "*");
    }
}

fn navigation_key(event: &web_sys::KeyboardEvent) -> bool {
    !event.ctrl_key()
        && !event.alt_key()
        && !event.meta_key()
        && !event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|element| {
                element
                    .closest("input, select, textarea, [contenteditable]")
                    .ok()
                    .flatten()
                    .is_some()
            })
}

fn chrome_message(message: &str, chrome: RwSignal<bool>) -> bool {
    match message {
        "yomu-chrome:toggle" => chrome.update(|visible| *visible = !*visible),
        "yomu-chrome:show" => chrome.set(true),
        _ => return false,
    }
    true
}

pub(super) fn epub_reader(
    detail: PublicationDetailResponse,
    unit_id: uuid::Uuid,
    resource: String,
) -> AnyView {
    let publication_id = detail.publication.id;
    let Some(unit) = detail.units.iter().find(|u| u.id == unit_id).cloned() else {
        return view! { <p class="error">"Section not found"</p> }.into_any();
    };
    let chrome = RwSignal::new(true);
    let menu_open = RwSignal::new(false);
    let width = RwSignal::new(offline::epub_width());
    let initial = match initial_location(&detail, unit_id, true) {
        Locations::Progression { progression, .. } => progression,
        _ => 0.0,
    };
    let progression = RwSignal::new(initial);
    let palette = RwSignal::new(offline::epub_palette());
    let index = detail
        .units
        .iter()
        .position(|u| u.id == unit_id)
        .unwrap_or(0);
    let previous = index.checked_sub(1).map(|i| detail.units[i].id);
    let next = detail.units.get(index + 1).map(|u| u.id);
    let frame = Frame::new();
    let report = reporter(publication_id, unit_id, unit.title.clone());
    let last = StoredValue::new(None::<f64>);
    let navigate = leptos_router::hooks::use_navigate();
    let link_navigate = navigate.clone();
    let turn_navigate = navigate.clone();
    let client = use_client();
    let links: Vec<_> = detail
        .units
        .iter()
        .filter_map(|unit| {
            let url = client.publication_resource_url(publication_id, &unit.source_key)?;
            let suffix = resource_suffix(url.path())?.to_string();
            Some((suffix, unit.id))
        })
        .collect();
    let origin = url::Url::parse(&resource).ok().map(|url| url.origin());
    let message = window_event_listener(leptos::ev::message, move |event| {
        if !from_frame(&event, frame) {
            return;
        }
        let Some(message) = event.data().as_string() else {
            return;
        };
        if chrome_message(&message, chrome) {
            return;
        }
        let turn = match message.as_str() {
            "yomu-epub-turn:1" => next,
            "yomu-epub-turn:-1" => previous,
            _ => None,
        };
        if let Some(id) = turn {
            turn_navigate(
                &format!("/read/{publication_id}/{id}"),
                leptos_router::NavigateOptions {
                    replace: true,
                    ..Default::default()
                },
            );
            return;
        }
        if let Some(target) = message
            .strip_prefix("yomu-link:")
            .and_then(|url| url::Url::parse(url).ok())
        {
            if Some(target.origin()) != origin {
                return;
            }
            let Some(suffix) = resource_suffix(target.path()) else {
                return;
            };
            if let Some((_, id)) = links.iter().find(|(path, _)| path == suffix) {
                let mut route = url::Url::parse(&format!(
                    "https://reader.invalid/read/{publication_id}/{id}"
                ))
                .unwrap();
                if let Some(fragment) = target.fragment() {
                    route.query_pairs_mut().append_pair("fragment", fragment);
                }
                let query = route.query().map(|q| format!("?{q}")).unwrap_or_default();
                link_navigate(
                    &format!("{}{query}", route.path()),
                    leptos_router::NavigateOptions {
                        replace: true,
                        ..Default::default()
                    },
                );
            }
            return;
        }
        let Some(value) = message
            .strip_prefix("yomu-location:")
            .and_then(|v| v.parse::<u32>().ok())
        else {
            return;
        };
        let value = f64::from(value.min(1000)) / 1000.0;
        progression.set(value);
        if last.get_value() != Some(value) {
            last.set_value(Some(value));
            report(
                Locations::Progression {
                    progression: value,
                    page: 0,
                },
                None,
            );
        }
    });
    on_cleanup(move || message.remove());
    let keyboard_navigate = navigate.clone();
    let keyboard = window_event_listener(leptos::ev::keydown, move |event| {
        if !navigation_key(&event) {
            return;
        }
        let target = match event.key().as_str() {
            "ArrowLeft" => previous,
            "ArrowRight" => next,
            _ => None,
        };
        if let Some(id) = target {
            event.prevent_default();
            keyboard_navigate(
                &format!("/read/{publication_id}/{id}"),
                leptos_router::NavigateOptions {
                    replace: true,
                    ..Default::default()
                },
            );
        }
    });
    on_cleanup(move || keyboard.remove());
    let fragment = leptos_router::hooks::use_query_map()
        .get_untracked()
        .get("fragment");
    let source = url::Url::parse(&resource)
        .ok()
        .map(|mut url| {
            url.query_pairs_mut()
                .append_pair("yomu-theme", &palette.get_untracked())
                .append_pair("yomu-width", &width.get_untracked());
            url.set_fragment(Some(
                &fragment
                    .clone()
                    .unwrap_or_else(|| format!("yomu={}", (initial * 1000.0).round())),
            ));
            url.to_string()
        })
        .unwrap_or_default();
    view! {
        <ReaderShell chrome flow=Signal::derive(|| false) publication=true>
            <ReaderTop publication_id>
                <select title="Contents" on:change=move |event| {
                    navigate(&format!("/read/{publication_id}/{}", event_target_value(&event)),
                        leptos_router::NavigateOptions { replace: true, ..Default::default() });
                }>
                    {detail.units.into_iter().map(|u| view! {
                        <option value=u.id.to_string() selected=u.id == unit_id>{u.title}</option>
                    }).collect_view()}
                </select>
            </ReaderTop>
            <iframe class="epub-navigator" title="EPUB reader" sandbox="allow-scripts"
                referrerpolicy="no-referrer" src=source node_ref=frame></iframe>
            {move || menu_open.get().then(|| view! {
                <div class="reader-chrome reader-menu publication-reader-options">
                    <label>"Reading colors"
                        <select title="Reading colors" aria-label="Reading colors" prop:value=move || palette.get() on:change=move |event| {
                            let value = event_target_value(&event);
                            if matches!(value.as_str(), "paper" | "night") {
                                offline::set_epub_palette(&value);
                                command(frame, &format!("yomu-epub-theme:{value}"));
                                palette.set(value);
                            }
                        }>
                            <option value="night">"Night"</option>
                            <option value="paper">"Paper"</option>
                        </select>
                    </label>
                    <div class="reader-text-size" role="group" aria-label="Text size">
                        <span>"Text size"</span>
                        <button class="pill-btn" title="Smaller text" on:click=move |_| command(frame, "yomu-epub-font:-1")>"A−"</button>
                        <button class="pill-btn" title="Larger text" on:click=move |_| command(frame, "yomu-epub-font:1")>"A+"</button>
                    </div>
                    <label>"Reading width"
                        <select aria-label="Reading width" prop:value=move || width.get() on:change=move |event| {
                            let value = event_target_value(&event);
                            if matches!(value.as_str(), "narrow" | "comfortable" | "wide" | "full") {
                                offline::set_epub_width(&value);
                                command(frame, &format!("yomu-epub-width:{value}"));
                                width.set(value);
                            }
                        }>
                            <option value="narrow">"Narrow"</option>
                            <option value="comfortable">"Comfortable"</option>
                            <option value="wide">"Wide"</option>
                            <option value="full">"Full width"</option>
                        </select>
                    </label>
                </div>
            })}
            <footer class="reader-chrome reader-bottom">
                {previous.map(|id| view! { <a class="pill-btn" aria-label="Previous section" title="Previous section" href=format!("/read/{publication_id}/{id}")>"‹"</a> })}
                <span class="pill-counter">{move || format!("{:.0}%", progression.get() * 100.0)}</span>
                {next.map(|id| view! { <a class="pill-btn" aria-label="Next section" title="Next section" href=format!("/read/{publication_id}/{id}")>"›"</a> })}
                <span class="pill-sep"></span>
                <button class="pill-btn" aria-label="Reader options" title="Reader options" aria-expanded=move || menu_open.get().to_string() on:click=move |_| menu_open.update(|open| *open = !*open)>"⚙"</button>
            </footer>
        </ReaderShell>
    }.into_any()
}

pub(super) fn pdf_reader(
    detail: PublicationDetailResponse,
    unit_id: uuid::Uuid,
    resource: String,
) -> AnyView {
    let publication_id = detail.publication.id;
    let Some(unit) = detail.units.iter().find(|u| u.id == unit_id).cloned() else {
        return view! { <p class="error">"Document not found"</p> }.into_any();
    };
    let chrome = RwSignal::new(true);
    let initial = match initial_location(&detail, unit_id, false) {
        Locations::Page { page } => page,
        _ => 0,
    };
    let count = RwSignal::new(unit.page_count.unwrap_or(1).max(1));
    let page = RwSignal::new(initial.min(count.get_untracked() - 1));
    let error = RwSignal::new(None::<String>);
    let zoom_mode = RwSignal::new("fit-width".to_string());
    let zoom_scale = RwSignal::new(1.0_f64);
    let frame = Frame::new();
    let report = reporter(publication_id, unit_id, unit.title.clone());
    let last = StoredValue::new(None::<u32>);
    let message = window_event_listener(leptos::ev::message, move |event| {
        if !from_frame(&event, frame) {
            return;
        }
        let Some(message) = event.data().as_string() else {
            return;
        };
        if chrome_message(&message, chrome) {
            return;
        }
        if let Some((mode, scale)) = message
            .strip_prefix("yomu-pdf-zoom:")
            .and_then(|s| s.split_once(':'))
        {
            if matches!(mode, "fit-width" | "fit-page" | "custom")
                && let Ok(scale) = scale.parse::<f64>()
                && scale.is_finite()
                && scale > 0.0
                && scale <= 4.0
            {
                zoom_mode.set(mode.into());
                zoom_scale.set(scale);
            }
            return;
        }
        if let Some(message) = message.strip_prefix("yomu-reader-error:") {
            error.set(Some(message.into()));
            return;
        }
        let Some((p, total)) = message
            .strip_prefix("yomu-pdf-location:")
            .and_then(|s| s.split_once(':'))
        else {
            return;
        };
        let (Ok(p), Ok(total)) = (p.parse::<u32>(), total.parse::<u32>()) else {
            return;
        };
        if total == 0 || p >= total {
            return;
        }
        page.set(p);
        count.set(total);
        if last.get_value() != Some(p) {
            last.set_value(Some(p));
            report(Locations::Page { page: p }, Some(total));
        }
    });
    on_cleanup(move || message.remove());

    // Resolve against the UI's own origin, not the API origin. Native shells
    // ship the same viewer/assets and may read a server on another origin.
    let mut viewer = url::Url::parse(&window().location().href().unwrap_or_default()).unwrap();
    viewer.set_path("/pdf-reader.html");
    viewer.set_query(None);
    viewer.set_fragment(None);
    viewer
        .query_pairs_mut()
        .append_pair("file", &resource)
        // The scanner count can be wrong. Let the PDF renderer clamp the
        // requested location against its own count, not stale host metadata.
        .append_pair("page", &initial.to_string());
    let keyboard = window_event_listener(leptos::ev::keydown, move |event| {
        if !navigation_key(&event) {
            return;
        }
        let message = match event.key().as_str() {
            "ArrowLeft" | "PageUp" => "yomu-pdf-turn:-1",
            "ArrowRight" | "PageDown" => "yomu-pdf-turn:1",
            _ => return,
        };
        event.prevent_default();
        command(frame, message);
    });
    on_cleanup(move || keyboard.remove());
    view! {
        <ReaderShell chrome flow=Signal::derive(|| false) publication=true>
            <ReaderTop publication_id>
                <span class="reader-title">{detail.publication.title}</span>
            </ReaderTop>
            <iframe class="pdf-navigator" title="PDF reader" referrerpolicy="no-referrer" src=viewer.to_string() node_ref=frame></iframe>
            {move || error.get().map(|error| view! { <p class="error publication-reader-error" role="alert">{error}</p> })}
            <footer class="reader-chrome reader-bottom">
                <button class="pill-btn" aria-label="Previous page" title="Previous page" on:click=move |_| command(frame, "yomu-pdf-turn:-1") disabled=move || page.get() == 0>"‹"</button>
                <span class="pill-counter">{move || format!("{} / {}", page.get() + 1, count.get())}</span>
                <button class="pill-btn" aria-label="Next page" title="Next page" on:click=move |_| command(frame, "yomu-pdf-turn:1") disabled=move || { page.get() + 1 >= count.get() }>"›"</button>
                <span class="pill-sep"></span>
                <div class="publication-zoom" role="group" aria-label="PDF zoom controls">
                    <button class="pill-btn" aria-label="Zoom out" title="Zoom out" on:click=move |_| command(frame, "yomu-pdf-zoom:out") disabled=move || { zoom_scale.get() <= 0.25 }>"−"</button>
                    <select title="PDF zoom" aria-label="PDF zoom" prop:value=move || zoom_mode.get() on:change=move |event| command(frame, &format!("yomu-pdf-zoom:{}", event_target_value(&event)))>
                        <option value="fit-width">"Fit width"</option>
                        <option value="fit-page">"Fit page"</option>
                        <option value="custom" disabled>{move || format!("{:.0}%", zoom_scale.get() * 100.0)}</option>
                        <option value="0.5">"50%"</option>
                        <option value="0.75">"75%"</option>
                        <option value="1">"100%"</option>
                        <option value="1.5">"150%"</option>
                        <option value="2">"200%"</option>
                        <option value="3">"300%"</option>
                        <option value="4">"400%"</option>
                    </select>
                    <button class="pill-btn" aria-label="Zoom in" title="Zoom in" on:click=move |_| command(frame, "yomu-pdf-zoom:in") disabled=move || { zoom_scale.get() >= 4.0 }>"+"</button>
                </div>
            </footer>
        </ReaderShell>
    }.into_any()
}
