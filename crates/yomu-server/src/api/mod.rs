//! HTTP API (`/api/v1`) and static frontend serving.

mod categories;
mod chapters;
mod error;
mod library;
mod progress;
mod sources;

use axum::routing::get;
use axum::{Json, Router};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use yomu_domain::HealthResponse;

pub use error::ApiError;

use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/sources", get(sources::list))
        .route("/sources/{id}/search", get(sources::search))
        .route("/library", get(library::list).post(library::add))
        .route("/categories", get(categories::list))
        .route("/categories/{id}", axum::routing::put(categories::update))
        .route(
            "/manga/{id}",
            get(library::detail)
                .put(library::update)
                .delete(library::delete),
        )
        .route("/manga/{id}/refresh", axum::routing::post(library::refresh))
        .route("/manga/{id}/cover", get(library::cover))
        .route(
            "/manga/{id}/position",
            axum::routing::put(progress::set_position),
        )
        .route(
            "/chapters/{id}/download",
            axum::routing::post(chapters::download),
        )
        .route("/chapters/{id}/pages", get(chapters::pages))
        .route("/chapters/{id}/pages/{n}", get(chapters::page_image))
        .route(
            "/progress/events",
            get(progress::events).post(progress::push_events),
        )
        .with_state(state.clone());

    let mut app = Router::new().nest("/api/v1", api);

    if let Some(dir) = &state.config.static_dir {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)));
    }

    app
        // LAN-only posture, like chaos; revisit with auth.
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    })
}
