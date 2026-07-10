mod api;
mod auth;
mod config;
mod db;
mod downloader;
mod notifier;
mod oidc;
mod state;
mod sync;
mod updater;

use anyhow::Context;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use yomu_source::local::LocalSource;
use yomu_source::registry::Registry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = config::load().context("loading configuration")?;

    let mut sources = Registry::load(&config.sources_dir).map_err(|e| {
        anyhow::anyhow!("loading sources from {}: {e}", config.sources_dir.display())
    })?;
    if config.local.enabled {
        sources
            .insert(Arc::new(LocalSource::new(
                "local",
                "Local series",
                config.local.dir.clone(),
            )))
            .map_err(|e| anyhow::anyhow!("registering local source: {e}"))?;
    }
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

    let oidc = oidc::OidcRuntime::from_config(&config.auth).context("configuring [auth]")?;
    match &oidc {
        Some(_) => tracing::info!(
            issuer = %config.auth.issuer.as_ref().expect("issuer set").as_str(),
            "auth: OIDC sign-in enabled"
        ),
        None => tracing::info!("auth: single-account mode (no [auth] issuer configured)"),
    }

    let state = state::AppState::new(config, db, sources, oidc);
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

/// Ctrl-c *and* SIGTERM: under systemd (the nix module) stop sends SIGTERM,
/// and graceful shutdown must run there too, not just in a dev terminal.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM");
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await.expect("failed to listen for ctrl-c");
    tracing::info!("shutting down");
}
