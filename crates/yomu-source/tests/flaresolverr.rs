//! Deterministic browser-fallback tests; no external solver or site involved.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;
use yomu_source::{
    Source, SourceError,
    flaresolverr::{FlareSolverr, FlareSolverrConfig},
    selector::{SelectorSource, SelectorSpec},
};

async fn server(
    handler: impl Fn(&str, &str) -> (u16, String, String) + Send + Sync + 'static,
) -> (Url, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url: Url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let handler = Arc::new(handler);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let (head, body) = loop {
                    let mut chunk = [0; 4096];
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    assert!(bytes.len() < 65536);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&bytes[..end]).to_string();
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|s| s.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break (
                                head,
                                String::from_utf8_lossy(&bytes[end + 4..end + 4 + length])
                                    .to_string(),
                            );
                        }
                    }
                };
                let (status, headers, body) = handler(&head, &body);
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (url, task)
}

fn html() -> String {
    "<h1>Test title</h1><p>Summary</p><a href='/chapter/1'>Chapter 1</a>".into()
}
fn spec(url: &Url, private: bool) -> SelectorSpec {
    toml::from_str(&format!(
        r#"
id = "fixture"
name = "Fixture"
base_url = "{url}"
min_delay_ms = 0
allowed_private_hosts = {hosts}
[flaresolverr]
allowed_hosts = ["127.0.0.1"]
[search]
url = "{{base}}?q={{query}}"
item = "a"
link = "@href"
[manga]
title = "h1"
description = "p"
chapter_item = "a"
chapter_link = "@href"
[pages]
image = "img@src"
"#,
        hosts = if private { "['127.0.0.1']" } else { "[]" }
    ))
    .unwrap()
}
fn config(url: Url) -> FlareSolverrConfig {
    FlareSolverrConfig {
        url,
        timeout_secs: 5,
    }
}

#[tokio::test]
async fn challenge_html_and_images_reuse_browser_credentials_without_repeated_solves() {
    let (origin, origin_task) = server(|head, _| {
        let lower = head.to_ascii_lowercase();
        if head.starts_with("GET /forbidden ") {
            return (403, String::new(), "Not allowed".into());
        }
        if head.starts_with("GET /plain ") {
            return (200, String::new(), html());
        }
        if !lower.contains("user-agent: fixturebrowser")
            || !lower.contains("cookie: clearance=secret")
        {
            return (
                403,
                "cf-mitigated: challenge\r\n".into(),
                "Browser check".into(),
            );
        }
        if head.starts_with("GET /image ") {
            (
                200,
                "content-type: image/png\r\n".into(),
                "PNG fixture bytes".into(),
            )
        } else {
            (200, String::new(), html())
        }
    })
    .await;
    let count = Arc::new(AtomicUsize::new(0));
    let count_copy = count.clone();
    let (solver_url, solver_task) = server(move |head, body| {
        assert!(head.starts_with("POST /v1 "));
        assert!(!head.to_ascii_lowercase().contains("authorization:"));
        let command: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(command["cmd"], "request.get");
        assert_eq!(command["maxTimeout"], 5000);
        count_copy.fetch_add(1, Ordering::SeqCst);
        let reply = serde_json::json!({"status":"ok", "solution": {
            "url": command["url"], "status":200, "response":html(), "userAgent":"FixtureBrowser",
            "cookies":[{"name":"clearance", "value":"secret", "domain":"127.0.0.1", "path":"/", "expires":-1}]
        }});
        (200, "content-type: application/json\r\n".into(), reply.to_string())
    }).await;
    let source = Arc::new(
        SelectorSource::new(spec(&origin, true))
            .unwrap()
            .with_flaresolverr(Arc::new(FlareSolverr::new(config(solver_url)).unwrap())),
    );
    source
        .manga(origin.join("plain").unwrap().as_str())
        .await
        .unwrap();
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "successful HTTP must not launch a browser"
    );
    assert!(matches!(
        source
            .manga(origin.join("forbidden").unwrap().as_str())
            .await,
        Err(SourceError::Http(_))
    ));
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "ordinary 403 is not a browser challenge"
    );
    let (first, second) =
        tokio::join!(source.manga(origin.as_str()), source.manga(origin.as_str()));
    assert_eq!(first.unwrap().chapters.len(), 1);
    assert_eq!(second.unwrap().summary.title, "Test title");
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "concurrent challenge requests share renewal"
    );
    let image = source.image(&origin.join("image").unwrap()).await.unwrap();
    assert_eq!(image.content_type, "image/png");
    assert_eq!(image.bytes.as_ref(), b"PNG fixture bytes");
    source.manga(origin.as_str()).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    origin_task.abort();
    solver_task.abort();
}

#[tokio::test]
async fn fallback_requires_service_and_source_opt_in_and_keeps_private_target_checks() {
    let (origin, origin_task) =
        server(|_, _| (403, "cf-mitigated: challenge\r\n".into(), String::new())).await;
    let (solver_url, solver_task) = server(|_, _| panic!("solver must not be called")).await;
    let source = SelectorSource::new(spec(&origin, true)).unwrap();
    assert!(matches!(
        source.manga(origin.as_str()).await,
        Err(SourceError::BrowserChallenge { .. })
    ));
    let mut without_opt_in = spec(&origin, true);
    without_opt_in.flaresolverr = None;
    let source = SelectorSource::new(without_opt_in)
        .unwrap()
        .with_flaresolverr(Arc::new(
            FlareSolverr::new(config(solver_url.clone())).unwrap(),
        ));
    assert!(matches!(
        source.manga(origin.as_str()).await,
        Err(SourceError::BrowserChallenge { .. })
    ));
    let source = SelectorSource::new(spec(&origin, false))
        .unwrap()
        .with_flaresolverr(Arc::new(FlareSolverr::new(config(solver_url)).unwrap()));
    assert!(
        source
            .manga(origin.as_str())
            .await
            .unwrap_err()
            .to_string()
            .contains("non-public")
    );
    origin_task.abort();
    solver_task.abort();
}

#[tokio::test]
async fn solver_failures_are_errors_not_empty_chapter_lists() {
    let (origin, origin_task) =
        server(|_, _| (403, "cf-mitigated: challenge\r\n".into(), String::new())).await;
    for body in [
        "not JSON".to_string(),
        serde_json::json!({"status":"error", "message":"sensitive solver details"}).to_string(),
        serde_json::json!({"status":"ok", "solution":{"url":"http://169.254.169.254/", "status":200, "response":html(), "userAgent":"Browser"}}).to_string(),
        serde_json::json!({"status":"ok", "solution":{"url":origin.as_str(), "status":403, "response":html(), "userAgent":"Browser"}}).to_string(),
    ] {
        let (solver_url, solver_task) = server(move |_, _| (200, "content-type: application/json\r\n".into(), body.clone())).await;
        let source = SelectorSource::new(spec(&origin, true)).unwrap().with_flaresolverr(Arc::new(FlareSolverr::new(config(solver_url)).unwrap()));
        let error = source.manga(origin.as_str()).await.unwrap_err().to_string();
        assert!(error.contains("FlareSolverr"), "{error}");
        assert!(!error.contains("sensitive"));
        solver_task.abort();
    }
    origin_task.abort();
}
