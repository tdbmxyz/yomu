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
    /// Downloaded units live here: <data_dir>/<publication id>/<unit id>/.
    pub data_dir: PathBuf,
    /// Directory of source definitions (`*.toml`, see sources.d examples).
    pub sources_dir: PathBuf,
    pub updater: UpdaterConfig,
    /// Accepts the legacy 1.x section name `[local]` too; see [`extract`].
    pub books: BooksConfig,
    pub auth: AuthConfig,
    pub notify: Option<NotifyConfig>,
    pub catalog: CatalogConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdaterConfig {
    /// Master switch for the periodic new-chapter check.
    pub enabled: bool,
    /// Seconds between two library-wide checks (clamped to ≥ 60).
    pub interval_secs: u64,
}

/// The streamer's watched folder: user-supplied comic files as
/// `<dir>/<Series>/<Chapter>/*.png`, `<Series>/<Chapter>.cbz`, or
/// root-level `.cbz` / image dirs (see `crate::streamer`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BooksConfig {
    pub enabled: bool,
    /// Directory holding the files. Defaults to the 1.x local-source dir so
    /// nothing moves on disk for existing deployments.
    pub dir: PathBuf,
    /// Seconds between periodic rescans (clamped to ≥ 60).
    pub scan_interval_secs: u64,
}

impl Default for BooksConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: PathBuf::from("local"),
            scan_interval_secs: 60 * 60,
        }
    }
}

/// Source catalog cache (Sources tab listings).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogConfig {
    /// Cached browse pages older than this revalidate in the background
    /// on access; 0 disables cached reads (listings always live).
    pub ttl_secs: u64,
}

impl Default for CatalogConfig {
    fn default() -> Self {
        Self {
            ttl_secs: 6 * 60 * 60,
        }
    }
}

/// Push notifications for updater-found chapters, POSTed to an ntfy
/// topic. Absent section = feature off.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyConfig {
    /// ntfy topic URL, e.g. `https://ntfy.example.net/yomu`.
    pub url: url::Url,
    /// Optional ntfy access token (sent as `Authorization: Bearer`).
    #[serde(default)]
    pub token: Option<String>,
}

/// OIDC sign-in (authentik). Leave `issuer` unset for single-account mode:
/// no login, every reader is the shared "Everyone" account.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// OIDC issuer, e.g. `https://auth.example.com/application/o/yomu/`
    /// (its `/.well-known/openid-configuration` must resolve).
    pub issuer: Option<url::Url>,
    pub client_id: String,
    pub client_secret: String,
    /// Public origin of this server, used to build the OIDC redirect URI
    /// (`<public_url>/api/v1/auth/callback` — register it in authentik).
    /// Derived from the request's Host header when unset.
    pub public_url: Option<url::Url>,
    /// Session lifetime in days (0 = default 90).
    pub session_days: u32,
    /// Browser origins allowed to make credentialed cross-origin calls
    /// (a frontend served from a different host than this API). Empty =
    /// same-origin only, which is the served-frontend deployment. A
    /// wildcard is intentionally impossible here: credentialed CORS may
    /// not use `*`.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

impl AuthConfig {
    pub fn oidc_enabled(&self) -> bool {
        self.issuer.is_some()
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
            books: BooksConfig::default(),
            auth: AuthConfig::default(),
            notify: None,
            catalog: CatalogConfig::default(),
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
    let figment = Figment::from(Toml::file(&config_path)).merge(Env::prefixed("YOMU_").split("__"));
    extract(figment)
}

/// Extract a [`Config`] from merged providers, filling gaps with defaults.
///
/// Pre-2.x deployments named the books section `[local]` (and
/// `YOMU_LOCAL__*` in the environment). A serde alias can't absorb that
/// here — layering user keys over serialized defaults makes `local` and
/// `books` collide as a duplicate field — so the legacy key is folded in
/// at the figment level instead: `local` becomes `books` unless an
/// explicit `books` section is also present, in which case the new name
/// wins wholesale.
fn extract(figment: Figment) -> anyhow::Result<Config> {
    let mut root: figment::value::Dict = figment.extract()?;
    if let Some(local) = root.remove("local") {
        root.entry("books".to_owned()).or_insert(local);
    }
    let config = Figment::from(Serialized::defaults(Config::default()))
        .merge(Serialized::defaults(root))
        .extract::<Config>()?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_toml(toml: &str) -> anyhow::Result<Config> {
        extract(Figment::from(Toml::string(toml)))
    }

    #[test]
    fn legacy_local_section_maps_to_books() {
        let config = from_toml("[local]\ndir = \"/srv/media\"\n")
            .expect("legacy [local] section must not crash");
        assert_eq!(config.books.dir, PathBuf::from("/srv/media"));
        assert!(config.books.enabled);
        assert_eq!(config.books.scan_interval_secs, 3600);
    }

    #[test]
    fn books_section_parses() {
        let config = from_toml("[books]\ndir = \"/srv/media\"\n").expect("[books] section parses");
        assert_eq!(config.books.dir, PathBuf::from("/srv/media"));
        assert!(config.books.enabled);
        assert_eq!(config.books.scan_interval_secs, 3600);
    }

    #[test]
    fn missing_section_yields_defaults() {
        let config = from_toml("").expect("empty config parses");
        assert!(config.books.enabled);
        assert_eq!(config.books.dir, PathBuf::from("local"));
        assert_eq!(config.books.scan_interval_secs, 3600);
        // Unrelated keys keep their defaults through the same pipeline.
        assert_eq!(config.listen, "0.0.0.0:4700".parse().unwrap());
        assert_eq!(config.db_path, PathBuf::from("yomu.db"));
        assert_eq!(config.updater.interval_secs, 6 * 60 * 60);
        assert_eq!(config.catalog.ttl_secs, 6 * 60 * 60);
    }

    #[test]
    fn both_sections_present_books_wins() {
        let config = from_toml("[books]\ndir = \"/new\"\n\n[local]\ndir = \"/old\"\n")
            .expect("both sections parse");
        assert_eq!(config.books.dir, PathBuf::from("/new"));
    }

    #[test]
    #[allow(clippy::result_large_err)] // Jail closures return figment::Error
    fn legacy_local_env_var_maps_to_books() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("YOMU_LOCAL__DIR", "/srv/media");
            let figment = Figment::from(Env::prefixed("YOMU_").split("__"));
            let config = extract(figment).expect("legacy YOMU_LOCAL__DIR must not crash");
            assert_eq!(config.books.dir, PathBuf::from("/srv/media"));
            Ok(())
        });
    }

    #[test]
    #[allow(clippy::result_large_err)] // Jail closures return figment::Error
    fn books_env_var_maps() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("YOMU_BOOKS__DIR", "/srv/media");
            let figment = Figment::from(Env::prefixed("YOMU_").split("__"));
            let config = extract(figment).expect("YOMU_BOOKS__DIR parses");
            assert_eq!(config.books.dir, PathBuf::from("/srv/media"));
            Ok(())
        });
    }

    #[test]
    fn other_keys_still_merge_from_toml() {
        let config = from_toml("listen = \"127.0.0.1:9999\"\n\n[updater]\nenabled = false\n")
            .expect("partial sections parse");
        assert_eq!(config.listen, "127.0.0.1:9999".parse().unwrap());
        assert!(!config.updater.enabled);
        // Unset sibling key falls back to its default.
        assert_eq!(config.updater.interval_secs, 6 * 60 * 60);
    }
}
