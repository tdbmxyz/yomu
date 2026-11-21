use leptos::prelude::*;

/// Selector sources use full URLs as keys; other keys and local filenames
/// are opaque. Never hand a custom scheme or embedded credentials to Android.
fn source_href(key: &str) -> Option<String> {
    let url = url::Url::parse(key).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.into())
}

#[component]
pub(super) fn SourceLink(source_key: String) -> impl IntoView {
    source_href(&source_key).map(|href| {
        let external = href.clone();
        view! {
            <a class="button" href=href target="_blank" rel="external noopener noreferrer"
                on:click=move |ev| {
                    if crate::offline::shell_available() && crate::auth::open_external(&external).is_ok() {
                        ev.prevent_default();
                    }
                }>
                "Open on source site"
            </a>
        }
    })
}

#[cfg(test)]
mod tests {
    use super::source_href;

    #[test]
    fn opens_web_links_without_credentials_only() {
        let title = "https://asurascans.com/comics/nano-machine-7ba78ec7";
        assert_eq!(source_href(title).as_deref(), Some(title));
        for unsafe_key in [
            "comic-key",
            "/books/comic.cbz",
            "file:///books/comic.cbz",
            "intent://comic",
            "javascript:alert(1)",
            "https://user:password@example.org/comic",
        ] {
            assert_eq!(source_href(unsafe_key), None, "{unsafe_key}");
        }
    }
}
