//! Server configuration: figment defaults ← TOML (`$YOMU_CONFIG` or
//! ./yomu.toml) ← `YOMU_*` env vars, same scheme as chaos.

use std::net::SocketAddr;
use std::path::PathBuf;

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub listen: SocketAddr,
    /// Built web frontend; served with SPA fallback when set.
    pub static_dir: Option<PathBuf>,
    pub db_path: PathBuf,
    /// Downloaded chapters live here: <data_dir>/<manga id>/<chapter id>/.
    pub data_dir: PathBuf,
    /// Directory of source definitions (`*.toml`, see sources.d examples).
    pub sources_dir: PathBuf,
    pub updater: UpdaterConfig,
    pub local: LocalConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdaterConfig {
    /// Master switch for the periodic new-chapter check.
    pub enabled: bool,
    /// Seconds between two library-wide checks (clamped to ≥ 60).
    pub interval_secs: u64,
}

/// The built-in "local" source: series that already live on the server's
/// disk as `<dir>/<Series>/<Chapter>/*.png` or `<Series>/<Chapter>.cbz`
/// (see `yomu_source::local`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    pub enabled: bool,
    /// Directory holding the local series.
    pub dir: PathBuf,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: PathBuf::from("local"),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:4700".parse().expect("valid default listen addr"),
            static_dir: None,
            db_path: PathBuf::from("yomu.db"),
            data_dir: PathBuf::from("data"),
            sources_dir: PathBuf::from("sources.d"),
            updater: UpdaterConfig::default(),
            local: LocalConfig::default(),
        }
    }
}

impl Default for UpdaterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 6 * 60 * 60,
        }
    }
}

pub fn load() -> anyhow::Result<Config> {
    let config_path = std::env::var("YOMU_CONFIG").unwrap_or_else(|_| "yomu.toml".into());
    let config = Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(&config_path))
        .merge(Env::prefixed("YOMU_").split("__"))
        .extract::<Config>()?;
    Ok(config)
}
