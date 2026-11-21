//! Reader images retry once with a fresh capability, then offer an explicit
//! retry rather than leaving an unexplained broken-image glyph.
use leptos::prelude::*;

#[component]
pub(super) fn ReaderImage(
    chapter: uuid::Uuid,
    n: u32,
    #[prop(optional)] strip: bool,
    #[prop(optional)] on_load: Option<Callback<leptos::ev::Event>>,
    #[prop(optional, into)] transform: Signal<String>,
) -> impl IntoView {
    let client = crate::use_client();
    let media = crate::media::use_media();
    let failed = RwSignal::new(false);
    let retried = StoredValue::new(false);
    let device = crate::offline::device_chapter_pages(chapter).is_some()
        && crate::offline::shell_page_url(chapter, n).is_some();
    let src_client = client.clone();
    let src = move || {
        let client = src_client.clone().with_media_token(media.token.get());
        let mut src = super::page_source(&client, chapter, n);
        let revision = media.revision.get();
        if !device && revision > 0 {
            src.push_str(if src.contains('?') { "&" } else { "?" });
            src.push_str(&format!("retry={revision}"));
        }
        src
    };
    Effect::new(move |_| {
        media.revision.track();
        failed.set(false);
    });
    let retry_client = client.clone();
    view! {
        <img
            class=if strip { "reader-strip-page" } else { "reader-page" }
            class:image-failed=move || failed.get()
            src=src
            style:transform=move || transform.get()
            data-chapter=chapter.to_string()
            data-page=n.to_string()
            loading=if !strip || n < 3 { "eager" } else { "lazy" }
            decoding="async"
            alt=""
            on:load=move |ev| {
                failed.set(false);
                retried.set_value(false);
                if let Some(callback) = on_load { callback.run(ev); }
            }
            on:error=move |_| {
                failed.set(true);
                if !device && !retried.get_value() {
                    retried.set_value(true);
                    media.refresh(client.clone());
                }
            }
        />
        <Show when=move || failed.get()>
            <button class="reader-image-retry" on:click={let client = retry_client.clone(); move |ev| {
                ev.stop_propagation();
                media.refresh(client.clone());
            }}>"Image unavailable — retry"</button>
        </Show>
    }
}
