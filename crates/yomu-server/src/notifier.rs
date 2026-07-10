//! Push notifications for new chapters, POSTed to an ntfy topic
//! (fire-and-forget: an unreachable ntfy must never fail a sync).

use yomu_domain::Chapter;

use crate::config::NotifyConfig;

pub struct Notifier {
    config: Option<NotifyConfig>,
    http: reqwest::Client,
}

impl Notifier {
    pub fn new(config: Option<NotifyConfig>) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// One push per manga per sync round. Failures are logged, never
    /// returned — a broken notifier must not stop the update sweep.
    pub async fn notify_new_chapters(&self, manga_title: &str, chapters: &[Chapter]) {
        let Some(config) = &self.config else {
            return;
        };
        if chapters.is_empty() {
            return;
        }
        let mut request = self
            .http
            .post(config.url.as_str())
            .header("X-Tags", "books");
        // HTTP header values are latin-1; a manga title beyond that moves
        // into the body instead of the X-Title header.
        match reqwest::header::HeaderValue::from_str(manga_title) {
            Ok(title) => {
                request = request.header("X-Title", title).body(message(chapters));
            }
            Err(_) => {
                request = request.body(format!("{manga_title}\n{}", message(chapters)));
            }
        }
        if let Some(token) = &config.token {
            request = request.bearer_auth(token);
        }
        match request.send().await {
            Ok(resp) if !resp.status().is_success() => {
                tracing::warn!(status = %resp.status(), "ntfy push rejected");
            }
            Err(err) => tracing::warn!(%err, "ntfy push failed"),
            Ok(_) => {}
        }
    }
}

/// Body: single chapter title, or "N new chapters — first … last" in the
/// order the listing produced them.
fn message(chapters: &[Chapter]) -> String {
    match chapters {
        [one] => one.title.clone(),
        many => format!(
            "{} new chapters — {} … {}",
            many.len(),
            many.first().expect("non-empty").title,
            many.last().expect("non-empty").title,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;
    use yomu_domain::DownloadState;

    fn chapter(title: &str) -> Chapter {
        Chapter {
            id: Uuid::now_v7(),
            manga_id: Uuid::now_v7(),
            source_key: format!("k-{title}"),
            title: title.into(),
            number: None,
            source_order: 0,
            scanlator: None,
            fetched_at: Utc::now(),
            published_at: None,
            download: DownloadState::None,
            page_count: None,
            read: false,
        }
    }

    #[test]
    fn message_single_chapter_is_its_title() {
        assert_eq!(message(&[chapter("Chapter 171")]), "Chapter 171");
    }

    #[test]
    fn message_many_chapters_shows_count_and_range() {
        let list = [
            chapter("Chapter 171"),
            chapter("Chapter 172"),
            chapter("Chapter 173"),
        ];
        assert_eq!(message(&list), "3 new chapters — Chapter 171 … Chapter 173");
    }

    /// End-to-end against a local mock: method, headers, body, auth.
    #[tokio::test]
    async fn posts_to_the_topic_with_title_and_token() {
        use axum::extract::State;
        use std::sync::{Arc, Mutex};

        type Seen = Arc<Mutex<Option<(String, String, String, String)>>>;
        let seen: Seen = Arc::new(Mutex::new(None));

        async fn capture(
            State(seen): State<Seen>,
            headers: axum::http::HeaderMap,
            body: String,
        ) -> &'static str {
            let get = |k: &str| {
                headers
                    .get(k)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string()
            };
            *seen.lock().unwrap() =
                Some((get("x-title"), get("x-tags"), get("authorization"), body));
            "ok"
        }

        let app = axum::Router::new()
            .route("/yomu", axum::routing::post(capture))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let notifier = Notifier::new(Some(NotifyConfig {
            url: format!("http://{addr}/yomu").parse().unwrap(),
            token: Some("tk_test".into()),
        }));
        notifier
            .notify_new_chapters("Some Manga", &[chapter("Chapter 5")])
            .await;

        let (title, tags, auth, body) = seen.lock().unwrap().clone().expect("request received");
        assert_eq!(title, "Some Manga");
        assert_eq!(tags, "books");
        assert_eq!(auth, "Bearer tk_test");
        assert_eq!(body, "Chapter 5");
    }

    #[tokio::test]
    async fn unconfigured_notifier_is_a_noop() {
        // Must return without any network activity or panic.
        Notifier::new(None)
            .notify_new_chapters("Some Manga", &[chapter("Chapter 5")])
            .await;
    }
}
