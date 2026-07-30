//! Identity resolved by a trusted reverse proxy's forward-auth.
//!
//! With authentik's outpost in front, the browser is already signed in
//! before yomu sees the request, and the outpost stamps who it is. Asking
//! for a second sign-in would be theatre — but believing a header is only
//! safe under a condition, and the condition is the whole design here:
//!
//! traefik's `authResponseHeaders` overwrites client-sent `X-authentik-*`
//! copies on the proxied route, so on *that* route the headers are
//! trustworthy. A request arriving on the direct LAN route never passes
//! through the outpost at all, and could carry anything. So yomu accepts
//! the identity only when the request also carries a shared secret that
//! the proxy stamps and a client cannot know.
//!
//! No secret configured means no header path: this fails closed.

use std::path::Path;

/// Header traefik stamps with the shared secret (see the yomu router in
/// `modules/roles/server/traefik.nix`).
pub const PROXY_SECRET_HEADER: &str = "x-yomu-proxy-secret";
/// Stable per-user id from authentik, preferred over the username, which
/// a rename would change.
pub const UID_HEADER: &str = "x-authentik-uid";
pub const USERNAME_HEADER: &str = "x-authentik-username";
pub const NAME_HEADER: &str = "x-authentik-name";

/// Who the proxy says this is.
#[derive(Debug, PartialEq, Eq)]
pub struct ProxyUser {
    /// Used as the OIDC `sub`, so a proxy sign-in and a native sign-in
    /// land on the same yomu account.
    pub sub: String,
    pub username: String,
    pub display_name: String,
}

/// The shared secret, from the environment-supplied value or the file.
///
/// `None` — neither set, unreadable, or empty — turns the header path
/// off, which is the safe direction.
pub fn load_secret(inline: &str, path: Option<&Path>) -> Option<String> {
    let inline = inline.trim();
    if !inline.is_empty() {
        return Some(inline.to_string());
    }
    let contents = std::fs::read_to_string(path?).ok()?;
    let secret = contents.trim().to_string();
    if secret.is_empty() {
        tracing::warn!("[auth] proxy_secret_file is empty; proxy identity is disabled");
        return None;
    }
    Some(secret)
}

/// The identity a request carries, if it is one to believe.
///
/// `get` reads a header by lowercase name. Kept as a closure so this is
/// testable without building an http request.
pub fn identify(
    expected_secret: Option<&str>,
    get: impl Fn(&str) -> Option<String>,
) -> Option<ProxyUser> {
    let expected = expected_secret?;
    let presented = get(PROXY_SECRET_HEADER)?;
    // Constant-time-ish: compare lengths first, then bytes without an
    // early exit. The secret is long and random, but there is no reason
    // to leak a prefix.
    if presented.len() != expected.len()
        || presented
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
    {
        return None;
    }

    let username = get(USERNAME_HEADER)?;
    if username.is_empty() {
        return None;
    }
    // authentik always sends a uid; falling back to the username keeps a
    // provider that does not from being locked out, at the cost of a
    // rename creating a new account.
    let sub = get(UID_HEADER)
        .filter(|uid| !uid.is_empty())
        .unwrap_or_else(|| username.clone());
    let display_name = get(NAME_HEADER)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| username.clone());
    Some(ProxyUser {
        sub,
        username,
        display_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn headers(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    fn signed(secret: &str) -> Vec<(&'static str, String)> {
        vec![
            (PROXY_SECRET_HEADER, secret.to_string()),
            (UID_HEADER, "uid-1".to_string()),
            (USERNAME_HEADER, "tibo".to_string()),
            (NAME_HEADER, "Tibo".to_string()),
        ]
    }

    fn get_from(pairs: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> =
            pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn a_correctly_stamped_request_is_believed() {
        let user = identify(Some("s3cret"), get_from(signed("s3cret"))).unwrap();
        assert_eq!(user.sub, "uid-1");
        assert_eq!(user.username, "tibo");
        assert_eq!(user.display_name, "Tibo");
    }

    /// The header path exists only because the proxy stamps a secret. A
    /// request that forges the identity headers without it — anything
    /// arriving on the direct LAN route — must be ignored.
    #[test]
    fn identity_headers_alone_prove_nothing() {
        let forged = headers(&[
            (UID_HEADER, "uid-1"),
            (USERNAME_HEADER, "tibo"),
            (NAME_HEADER, "Tibo"),
        ]);
        assert_eq!(identify(Some("s3cret"), forged), None);
        assert_eq!(identify(Some("s3cret"), get_from(signed("wrong"))), None);
        // Same length, different bytes: exercises the comparison itself
        // rather than the length shortcut.
        assert_eq!(identify(Some("s3cret"), get_from(signed("s3crat"))), None);
    }

    /// No secret configured means the whole path is off, even for a
    /// request that presents one.
    #[test]
    fn without_a_configured_secret_nothing_is_believed() {
        assert_eq!(identify(None, get_from(signed("s3cret"))), None);
    }

    /// A stamped request with no identity is not an anonymous session —
    /// it is nothing.
    #[test]
    fn a_stamped_request_without_a_username_is_not_an_identity() {
        assert_eq!(
            identify(
                Some("s3cret"),
                headers(&[(PROXY_SECRET_HEADER, "s3cret"), (USERNAME_HEADER, "")])
            ),
            None
        );
        assert_eq!(
            identify(Some("s3cret"), headers(&[(PROXY_SECRET_HEADER, "s3cret")])),
            None
        );
    }

    /// A provider that sends no uid still works; the username stands in.
    #[test]
    fn a_missing_uid_falls_back_to_the_username() {
        let user = identify(
            Some("s3cret"),
            headers(&[(PROXY_SECRET_HEADER, "s3cret"), (USERNAME_HEADER, "tibo")]),
        )
        .unwrap();
        assert_eq!(user.sub, "tibo");
        assert_eq!(user.display_name, "tibo");
    }

    #[test]
    fn an_empty_secret_file_disables_the_path() {
        let dir = std::env::temp_dir().join(format!("yomu-proxy-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty");
        std::fs::write(&empty, "   \n").unwrap();
        assert_eq!(load_secret("", Some(&empty)), None);

        let real = dir.join("secret");
        std::fs::write(&real, "s3cret\n").unwrap();
        assert_eq!(load_secret("", Some(&real)).as_deref(), Some("s3cret"));

        assert_eq!(load_secret("", None), None);
        assert_eq!(load_secret("  ", None), None);
        // The environment-supplied value wins, so one age file can feed
        // both traefik and yomu without them drifting apart.
        assert_eq!(
            load_secret("from-env", Some(&real)).as_deref(),
            Some("from-env")
        );
        assert_eq!(load_secret("", Some(&dir.join("missing"))), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
