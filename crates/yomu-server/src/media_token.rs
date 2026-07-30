//! Short-lived tokens for the two routes an `<img>` loads.
//!
//! Covers and page images are fetched by the browser's image loader,
//! which sends no `Authorization` header — and the shells' cookies never
//! apply from `tauri://localhost`. So those two routes accept a signed,
//! expiring token in the query string instead. Everything else the UI
//! reads goes through `yomu-client`, which can send a header.
//!
//! Stateless: the key is generated at startup, so a restart invalidates
//! outstanding tokens. They last an hour and clients refetch, which is
//! cheaper than persisting a secret.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

/// Signing key for `<user>.<expiry>.<hmac>` tokens. The user and expiry
/// are readable; the mac is what makes them binding.
pub struct Key([u8; 32]);

impl Key {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        // Two v4 UUIDs: the same OS randomness `auth::new_token` uses.
        bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        Self(bytes)
    }

    /// A token for `user`, valid for `ttl_secs` from now.
    pub fn mint(&self, user: Uuid, ttl_secs: u64) -> String {
        let payload = format!("{}.{}", user.simple(), now() + ttl_secs);
        format!("{payload}.{}", hex::encode(self.sign(&payload)))
    }

    /// The user this token speaks for, or `None` if it is malformed,
    /// forged or expired. `skew_secs` is for the tests; production
    /// passes 0.
    pub fn verify(&self, token: &str, skew_secs: u64) -> Option<Uuid> {
        let (payload, mac) = token.rsplit_once('.')?;
        let (user, expiry) = payload.split_once('.')?;
        let user: Uuid = user.parse().ok()?;
        let expiry: u64 = expiry.parse().ok()?;
        if !self.check(payload, mac) {
            return None;
        }
        (now() + skew_secs < expiry).then_some(user)
    }

    fn sign(&self, payload: &str) -> Vec<u8> {
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }

    /// `verify_slice` rather than comparing bytes: a MAC check that
    /// short-circuits on the first wrong byte leaks how much of a forgery
    /// was right.
    fn check(&self, payload: &str, mac_hex: &str) -> bool {
        let Ok(expected) = hex::decode(mac_hex) else {
            return false;
        };
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        mac.verify_slice(&expected).is_ok()
    }

    fn mac(&self) -> Hmac<Sha256> {
        Hmac::new_from_slice(&self.0).expect("hmac accepts any key length")
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Key {
        Key::generate()
    }

    #[test]
    fn a_fresh_token_verifies_for_its_own_user() {
        let key = key();
        let user = Uuid::from_u128(7);
        let token = key.mint(user, 3600);
        assert_eq!(key.verify(&token, 0), Some(user));
    }

    /// The signature is the whole point: someone who can read a token —
    /// from a URL bar, a log, a referrer — must not be able to make one
    /// for another user.
    #[test]
    fn a_tampered_token_is_rejected() {
        let key = key();
        let token = key.mint(Uuid::from_u128(7), 3600);
        let forged = token.replace(
            &Uuid::from_u128(7).simple().to_string(),
            &Uuid::from_u128(8).simple().to_string(),
        );
        assert_ne!(forged, token);
        assert_eq!(key.verify(&forged, 0), None);
    }

    /// Short-lived is the other half of why this is safe to put in a URL.
    #[test]
    fn an_expired_token_is_rejected() {
        let key = key();
        let token = key.mint(Uuid::from_u128(7), 60);
        assert_eq!(key.verify(&token, 61), None);
    }

    /// A key is per-process: a restart invalidates outstanding tokens
    /// rather than accepting anything signed by anyone.
    #[test]
    fn another_key_does_not_verify_this_token() {
        let token = key().mint(Uuid::from_u128(7), 3600);
        assert_eq!(key().verify(&token, 0), None);
    }

    #[test]
    fn malformed_tokens_are_rejected_rather_than_panicking() {
        let key = key();
        for bad in ["", ".", "a.b", "a.b.c", "....", "zz.1.2"] {
            assert_eq!(key.verify(bad, 0), None, "{bad:?}");
        }
    }
}
