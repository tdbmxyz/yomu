mod api;
mod config;
mod db;
mod downloader;
mod state;
mod sync;
mod updater;

use anyhow::Context;
use tracing_subscriber::EnvFilter;
use yomu_source::registry::Registry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = config::load().context("loading configuration")?;

    let sources = Registry::load(&config.sources_dir).map_err(|e| {
        anyhow::anyhow!("loading sources from {}: {e}", config.sources_dir.display())
    })?;
    if sources.is_empty() {
        tracing::warn!(
            dir = %config.sources_dir.display(),
            "no sources configured; drop a *.toml definition there to add a scan site"
        );
    } else {
        for source in sources.iter() {
            tracing::info!(id = source.id(), name = source.name(), "source loaded");
        }
    }

    let db = db::Db::connect(&config.db_path)
        .await
        .with_context(|| format!("opening database {}", config.db_path.display()))?;

    let state = state::AppState::new(config, db, sources);
    downloader::spawn(state.clone());
    updater::spawn(state.clone());

    let app = api::router(state.clone());
    let listener = tokio::net::TcpListener::bind(state.config.listen)
        .await
        .with_context(|| format!("binding {}", state.config.listen))?;
    tracing::info!("listening on http://{}", state.config.listen);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl-c");
    tracing::info!("shutting down");
}
