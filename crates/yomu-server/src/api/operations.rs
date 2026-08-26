//! Operational endpoints: dependency readiness and basic Prometheus metrics.

use std::path::Path;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use yomu_domain::{ReadinessCheck, ReadinessResponse};

use crate::state::AppState;

fn ok() -> ReadinessCheck {
    ReadinessCheck {
        ok: true,
        error: None,
    }
}

fn failed(error: impl ToString) -> ReadinessCheck {
    ReadinessCheck {
        ok: false,
        error: Some(error.to_string()),
    }
}

fn probe_writable_dir(path: &Path) -> ReadinessCheck {
    if let Err(err) = std::fs::create_dir_all(path) {
        return failed(format!("creating {}: {err}", path.display()));
    }
    let probe = path.join(format!(
        ".yomu-readiness-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
    {
        Ok(_) => match std::fs::remove_file(&probe) {
            Ok(()) => ok(),
            Err(err) => failed(format!("removing probe in {}: {err}", path.display())),
        },
        Err(err) => failed(format!("writing {}: {err}", path.display())),
    }
}

fn probe_readable_dir(path: &Path) -> ReadinessCheck {
    match std::fs::read_dir(path) {
        Ok(_) => ok(),
        Err(err) => failed(format!("reading {}: {err}", path.display())),
    }
}

pub async fn readiness(State(state): State<AppState>) -> impl IntoResponse {
    let database = match state.db.probe_write().await {
        Ok(()) => ok(),
        Err(err) => failed(err),
    };
    let data_dir = probe_writable_dir(&state.config.data_dir);
    let books_dir = if state.config.books.enabled {
        probe_readable_dir(&state.config.books.dir)
    } else {
        ok()
    };
    let free_bytes = fs2::available_space(&state.config.data_dir).ok();
    let floor = state.config.operations.minimum_free_bytes;
    let space_ok = free_bytes.is_some_and(|bytes| floor == 0 || bytes >= floor);
    let ready = database.ok && data_dir.ok && books_dir.ok && space_ok;
    let body = ReadinessResponse {
        status: if ready { "ready" } else { "not_ready" }.into(),
        database,
        data_dir,
        books_dir,
        free_bytes,
        minimum_free_bytes: floor,
    };
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(body),
    )
}

pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let requests = state.metrics.requests.load(Ordering::Relaxed);
    let cleaned = state.metrics.sessions_cleaned.load(Ordering::Relaxed);
    let uptime = state.metrics.started_at.elapsed().as_secs_f64();
    let free = fs2::available_space(&state.config.data_dir).unwrap_or(0);
    let body = format!(
        "# TYPE yomu_uptime_seconds gauge\n\
         yomu_uptime_seconds {uptime:.3}\n\
         # TYPE yomu_http_requests_total counter\n\
         yomu_http_requests_total {requests}\n\
         # TYPE yomu_db_pool_connections gauge\n\
         yomu_db_pool_connections {}\n\
         # TYPE yomu_db_pool_idle gauge\n\
         yomu_db_pool_idle {}\n\
         # TYPE yomu_data_free_bytes gauge\n\
         yomu_data_free_bytes {free}\n\
         # TYPE yomu_expired_sessions_cleaned_total counter\n\
         yomu_expired_sessions_cleaned_total {cleaned}\n",
        state.db.pool_size(),
        state.db.pool_idle(),
    );
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body)
}

pub async fn count_request(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    state.metrics.requests.fetch_add(1, Ordering::Relaxed);
    next.run(request).await
}
