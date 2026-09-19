//! Connection-time SSRF policy. Reqwest connects to these vetted addresses;
//! it must not resolve a checked hostname a second time or use ambient proxies.
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
};
use url::Url;

#[derive(Clone)]
pub(crate) struct NetworkPolicy {
    private_hosts: Vec<String>,
}
impl NetworkPolicy {
    pub fn new(private_hosts: Vec<String>) -> Self {
        Self { private_hosts }
    }
    fn explicitly_allowed(&self, host: &str) -> bool {
        self.private_hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
    }
    pub fn check_url(&self, url: &Url) -> Result<(), String> {
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("source URLs must be http(s) without credentials".into());
        }
        let host = url.host_str().ok_or("source URL has no host")?;
        if self.explicitly_allowed(host) {
            return Ok(());
        }
        let ip = match url.host() {
            Some(url::Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
            Some(url::Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
            _ => None,
        };
        if ip.is_some_and(|ip| !is_public(ip)) {
            return Err("source URL targets a non-public address (private hosts require explicit operator configuration)".into());
        }
        Ok(())
    }
    fn check_addresses(
        &self,
        host: &str,
        addresses: &[std::net::SocketAddr],
    ) -> Result<(), String> {
        if addresses.is_empty() {
            return Err("source hostname resolved to no addresses".into());
        }
        if !self.explicitly_allowed(host) && addresses.iter().any(|a| !is_public(a.ip())) {
            return Err("source hostname resolves to a non-public address".into());
        }
        Ok(())
    }
    pub fn resolver(&self) -> Arc<Self> {
        Arc::new(self.clone())
    }
}
impl Resolve for NetworkPolicy {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = self.clone();
        Box::pin(async move {
            let addresses: Vec<_> = tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            policy
                .check_addresses(name.as_str(), &addresses)
                .map_err(std::io::Error::other)?;
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}
fn public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || o[0] >= 240
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)))
}
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_v4(v4);
            }
            let s = ip.segments();
            // Only global unicast, excluding special-purpose, documentation,
            // transition (6to4), and benchmarking allocations.
            (s[0] & 0xe000) == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn denies_special_addresses_and_mixed_dns_answers() {
        let policy = NetworkPolicy::new(vec![]);
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "224.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "::1",
            "::ffff:169.254.169.254",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        let public = "93.184.216.34:0".parse().unwrap();
        let private = "127.0.0.1:0".parse().unwrap();
        assert!(policy.check_addresses("site.test", &[public]).is_ok());
        assert!(
            policy
                .check_addresses("site.test", &[public, private])
                .is_err()
        );
        // Rebinding on a later connection is checked again, not trusted by name.
        assert!(policy.check_addresses("site.test", &[private]).is_err());
        assert!(is_public("2606:4700:4700::1111".parse().unwrap()));
    }
    #[test]
    fn private_exception_is_exact_and_does_not_disable_url_validation() {
        let policy = NetworkPolicy::new(vec!["localhost".into()]);
        let private = "127.0.0.1:0".parse().unwrap();
        assert!(policy.check_addresses("localhost", &[private]).is_ok());
        assert!(
            policy
                .check_addresses("localhost.evil.test", &[private])
                .is_err()
        );
        assert!(
            policy
                .check_url(&"http://127.0.0.1/".parse().unwrap())
                .is_err()
        );
        assert!(
            policy
                .check_url(&"http://localhost/".parse().unwrap())
                .is_ok()
        );
        assert!(
            policy
                .check_url(&"file://localhost/etc/passwd".parse().unwrap())
                .is_err()
        );
    }
}
