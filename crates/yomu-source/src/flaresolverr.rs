//! Optional, explicitly trusted browser fallback. Credentials stay in memory
//! per source; normal requests never send them outside the operator allowlist.
use std::{
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::header::{COOKIE, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use url::Url;

use crate::{Result, SourceError, network::NetworkPolicy};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlareSolverrConfig {
    /// Trusted service origin, e.g. http://127.0.0.1:8191/ (not /v1).
    pub url: Url,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}
fn default_timeout() -> u64 {
    60
}

/// Source-local opt-in. Keep this table in the operator-managed source file,
/// independently of the server's generic service URL and timeout settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlareSolverrSourceConfig {
    pub allowed_hosts: Vec<String>,
}

impl FlareSolverrSourceConfig {
    pub(crate) fn validate(&self, base: &Url) -> Result<()> {
        if self.allowed_hosts.is_empty()
            || !self.allows(base)
            || self.allowed_hosts.iter().any(|host| {
                url::Host::parse(host).map_or(true, |parsed| {
                    parsed.to_string() != *host || host.contains('*')
                })
            })
        {
            return Err(SourceError::Definition("source flaresolverr.allowed_hosts must contain its base_url host and exact site/CDN hosts (no wildcards)".into()));
        }
        Ok(())
    }

    fn allows(&self, url: &Url) -> bool {
        url.host_str()
            .is_some_and(|host| self.allowed_hosts.iter().any(|h| h == host))
    }
}

pub struct FlareSolverr {
    config: FlareSolverrConfig,
    http: reqwest::Client,
    /// Bound browser load even when many sources fail simultaneously.
    gate: Mutex<()>,
}
impl FlareSolverr {
    pub fn new(config: FlareSolverrConfig) -> Result<Self> {
        if !matches!(config.url.scheme(), "http" | "https")
            || config.url.host().is_none()
            || !config.url.username().is_empty()
            || config.url.password().is_some()
            || config.url.path() != "/"
            || config.url.query().is_some()
            || config.url.fragment().is_some()
            || !(5..=120).contains(&config.timeout_secs)
        {
            return Err(SourceError::Definition(
                "flaresolverr needs an http(s) service origin and timeout_secs between 5 and 120"
                    .into(),
            ));
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(config.timeout_secs + 5))
            .build()
            .map_err(|e| SourceError::Http(e.to_string()))?;
        Ok(Self {
            config,
            http,
            gate: Mutex::new(()),
        })
    }
}

pub(crate) struct SolverSession {
    solver: Arc<FlareSolverr>,
    scope: FlareSolverrSourceConfig,
    gate: Mutex<()>,
    state: RwLock<Clearance>,
}
#[derive(Default)]
struct Clearance {
    generation: u64,
    user_agent: Option<HeaderValue>,
    cookies: Vec<Cookie>,
    page_url: Option<Url>,
}
#[derive(Deserialize)]
struct Reply {
    status: String,
    solution: Option<Solution>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Solution {
    url: Url,
    status: u16,
    response: String,
    user_agent: String,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    cookies: Vec<Cookie>,
}
#[derive(Deserialize)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    #[serde(default = "root_path")]
    path: String,
    #[serde(default)]
    secure: bool,
    #[serde(default)]
    expires: f64,
}
fn root_path() -> String {
    "/".into()
}
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

impl Cookie {
    fn usable(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let domain = self.domain.trim_start_matches('.');
        let domain_match = host == domain
            || (self.domain.starts_with('.') && host.ends_with(&format!(".{domain}")));
        let path_match = url.path() == self.path
            || (url.path().starts_with(&self.path)
                && (self.path.ends_with('/')
                    || url.path().as_bytes().get(self.path.len()) == Some(&b'/')));
        domain_match
            && path_match
            && (!self.secure || url.scheme() == "https")
            && (self.expires <= 0.0 || self.expires > now())
    }
    fn safe(&self, solved: &Url, allowed_hosts: &[String]) -> bool {
        let domain = self.domain.trim_start_matches('.');
        let host = solved.host_str().unwrap_or_default();
        // No public-suffix cookies or credentials for an unrelated source.
        allowed_hosts.iter().any(|h| h == domain)
            && (host == domain || host.ends_with(&format!(".{domain}")))
            && !self.name.is_empty()
            && self.name.len() + self.value.len() < 8192
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            && self.value.bytes().all(|b| {
                b == 0x21
                    || (0x23..=0x2b).contains(&b)
                    || (0x2d..=0x3a).contains(&b)
                    || (0x3c..=0x5b).contains(&b)
                    || (0x5d..=0x7e).contains(&b)
            })
            && self.path.starts_with('/')
    }
}

impl SolverSession {
    pub fn new(solver: Arc<FlareSolverr>, scope: FlareSolverrSourceConfig) -> Self {
        Self {
            solver,
            scope,
            gate: Mutex::new(()),
            state: RwLock::new(Clearance::default()),
        }
    }
    pub fn generation(&self) -> u64 {
        self.state.read().expect("clearance lock").generation
    }
    pub fn headers(&self, url: &Url) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if !self.scope.allows(url) {
            return headers;
        }
        let state = self.state.read().expect("clearance lock");
        if let Some(agent) = &state.user_agent {
            headers.insert(USER_AGENT, agent.clone());
        }
        let cookies = state
            .cookies
            .iter()
            .filter(|c| c.usable(url))
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ");
        if !cookies.is_empty()
            && cookies.len() <= 16384
            && let Ok(cookie) = HeaderValue::from_str(&cookies)
        {
            headers.insert(COOKIE, cookie);
        }
        headers
    }
    pub fn page_url(&self, fallback: &Url) -> Url {
        self.state
            .read()
            .expect("clearance lock")
            .page_url
            .clone()
            .unwrap_or_else(|| fallback.clone())
    }

    /// None means another request already renewed our credentials: retry HTTP.
    pub async fn solve(
        &self,
        url: &Url,
        network: &NetworkPolicy,
        generation: u64,
    ) -> Result<Option<String>> {
        if !self.scope.allows(url) {
            return Err(SourceError::Http(
                "FlareSolverr target is not in allowed_hosts".into(),
            ));
        }
        tokio::time::timeout(Duration::from_secs(5), network.check_resolved_url(url))
            .await
            .map_err(|_| SourceError::Http("source DNS check timed out".into()))?
            .map_err(SourceError::Http)?;
        let wait = Duration::from_secs(self.solver.config.timeout_secs);
        let _source_guard = tokio::time::timeout(wait, self.gate.lock())
            .await
            .map_err(|_| SourceError::Http("FlareSolverr is busy; retry later".into()))?;
        if self.generation() != generation {
            return Ok(None);
        }
        let _global_guard = tokio::time::timeout(wait, self.solver.gate.lock())
            .await
            .map_err(|_| SourceError::Http("FlareSolverr is busy; retry later".into()))?;
        tracing::info!(
            host = url.host_str().unwrap_or_default(),
            "requesting FlareSolverr fallback"
        );
        // The solver is an operator-selected privileged service. Do not follow
        // redirects of the API itself or forward caller auth/ambient proxies.
        let endpoint = self.solver.config.url.join("v1").expect("validated origin");
        let mut response = self
            .solver
            .http
            .post(endpoint)
            .json(&serde_json::json!({
                "cmd": "request.get", "url": url.as_str(),
                "maxTimeout": self.solver.config.timeout_secs * 1000,
            }))
            .send()
            .await
            .map_err(|_| {
                SourceError::Http("FlareSolverr service is unreachable or timed out".into())
            })?;
        if !response.status().is_success() {
            return Err(SourceError::Http(format!(
                "FlareSolverr returned HTTP {}",
                response.status()
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| SourceError::Http("FlareSolverr response interrupted".into()))?
        {
            if body.len().saturating_add(chunk.len()) > 16 * 1024 * 1024 {
                return Err(SourceError::Http(
                    "FlareSolverr response exceeds 16 MiB".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        let reply: Reply = serde_json::from_slice(&body)
            .map_err(|_| SourceError::Http("invalid FlareSolverr response".into()))?;
        let solution = reply
            .solution
            .filter(|_| reply.status == "ok")
            .ok_or_else(|| {
                SourceError::Http("FlareSolverr could not solve the browser challenge".into())
            })?;
        if solution.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("cf-mitigated") && value.eq_ignore_ascii_case("challenge")
        }) {
            return Err(SourceError::Http(
                "FlareSolverr returned an unsolved browser challenge".into(),
            ));
        }
        if !(200..300).contains(&solution.status) || solution.response.len() > 8 * 1024 * 1024 {
            return Err(SourceError::Http(format!(
                "FlareSolverr upstream response rejected (HTTP {})",
                solution.status
            )));
        }
        if !self.scope.allows(&solution.url) {
            return Err(SourceError::Http(
                "FlareSolverr redirected outside allowed_hosts".into(),
            ));
        }
        tokio::time::timeout(
            Duration::from_secs(5),
            network.check_resolved_url(&solution.url),
        )
        .await
        .map_err(|_| SourceError::Http("source DNS check timed out".into()))?
        .map_err(SourceError::Http)?;
        let agent = HeaderValue::from_str(&solution.user_agent)
            .ok()
            .filter(|_| solution.user_agent.len() <= 2048)
            .ok_or_else(|| SourceError::Http("invalid FlareSolverr user-agent".into()))?;
        let cookies = solution
            .cookies
            .into_iter()
            .filter(|c| c.safe(&solution.url, &self.scope.allowed_hosts))
            .take(100)
            .collect();
        let mut state = self.state.write().expect("clearance lock");
        *state = Clearance {
            generation: state.generation.wrapping_add(1),
            user_agent: Some(agent),
            cookies,
            page_url: Some(solution.url),
        };
        Ok(Some(solution.response))
    }
}

#[cfg(test)]
mod tests;
