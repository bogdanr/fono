// SPDX-License-Identifier: GPL-3.0-only
//! Shared inbound-auth primitives for Fono's HTTP servers.
//!
//! Both the LLM inference server (`llm_server`) and the web settings
//! server verify presented bearer tokens against the same
//! [`fono_core::api_keys::ApiKeyStore`] and record bounded per-interval
//! usage. To keep the servers decoupled from the store (and from
//! `rusqlite`'s non-`Sync` `Connection`), the daemon injects two small
//! closures:
//!
//! * an [`AuthVerifier`] that maps a presented token to the matching key
//!   id (or `None`), and
//! * a [`UsageSink`] that records one authenticated hit against a key id
//!   — typically by pushing onto a bounded channel drained by a
//!   background writer, keeping SQLite writes off the request hot path.

use std::sync::Arc;

/// Identifier of a stored inbound API key (its `api_keys.id`).
pub type KeyId = i64;

/// Verifies a presented bearer token, returning the matching key id when
/// the token is valid and the key is neither revoked nor expired.
pub type AuthVerifier = Arc<dyn Fn(&str) -> Option<KeyId> + Send + Sync>;

/// Records one authenticated request against a key id. Implementations
/// should be cheap and non-blocking (e.g. a channel send).
pub type UsageSink = Arc<dyn Fn(KeyId) + Send + Sync>;

/// Outcome of an inbound-auth check.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthDecision {
    /// Admit the request. `Some(id)` when a stored key authorised it (the
    /// caller should record one usage hit against that key); `None` when
    /// admitted without a key (loopback owner, or auth disabled).
    Allow(Option<KeyId>),
    /// Reject the request with `401`.
    Deny,
}

/// Decide whether an inbound HTTP request is authorised, shared by the LLM
/// inference server and the web settings server so both enforce identical
/// rules:
///
/// * auth off ⇒ everyone is admitted;
/// * a *presented* bearer token is always verified — even from loopback —
///   so a wrong key is rejected (`Deny`) and a valid key's id is returned
///   so its usage is recorded (`Allow(Some(id))`);
/// * when *no* token is presented, a loopback caller is trusted as the
///   local owner (`Allow(None)`; this avoids a bootstrap lockout where the
///   first key can't be created), and a non-loopback caller is denied.
///
/// Fails closed: with auth on and no verifier, any presented token and any
/// non-loopback caller is denied. An empty token counts as "not presented"
/// so a bare `Authorization: Bearer ` header still hits the loopback path.
pub fn decide(
    auth_enabled: bool,
    is_loopback: bool,
    presented: Option<&str>,
    verifier: Option<&AuthVerifier>,
) -> AuthDecision {
    if !auth_enabled {
        return AuthDecision::Allow(None);
    }
    // A presented credential is authoritative: verify it regardless of
    // origin so localhost tools that send a key are validated and metered,
    // and a bad key is rejected instead of silently waved through.
    if let Some(tok) = presented.filter(|t| !t.is_empty()) {
        return verifier
            .and_then(|v| v(tok))
            .map_or(AuthDecision::Deny, |id| AuthDecision::Allow(Some(id)));
    }
    // No credential presented: trust the loopback owner, else deny.
    if is_loopback {
        AuthDecision::Allow(None)
    } else {
        AuthDecision::Deny
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verifier_accepting(expected: &'static str, id: KeyId) -> AuthVerifier {
        Arc::new(move |tok: &str| (tok == expected).then_some(id))
    }

    #[test]
    fn auth_off_admits_everyone() {
        assert_eq!(decide(false, false, None, None), AuthDecision::Allow(None));
    }

    #[test]
    fn loopback_without_token_is_trusted() {
        // With auth on and no token, a loopback caller is admitted (bootstrap).
        assert_eq!(decide(true, true, None, None), AuthDecision::Allow(None));
    }

    #[test]
    fn loopback_with_empty_token_is_trusted() {
        // A bare `Authorization: Bearer ` (empty token) counts as no token.
        let v = verifier_accepting("good", 7);
        assert_eq!(decide(true, true, Some(""), Some(&v)), AuthDecision::Allow(None));
    }

    #[test]
    fn loopback_with_valid_token_is_metered() {
        // A valid token presented from loopback is verified and its id
        // returned so usage (last-used) is recorded.
        let v = verifier_accepting("good", 7);
        assert_eq!(decide(true, true, Some("good"), Some(&v)), AuthDecision::Allow(Some(7)));
    }

    #[test]
    fn loopback_with_bad_token_is_denied() {
        // A wrong token is rejected even from loopback — presenting a
        // credential means it must be valid.
        let v = verifier_accepting("good", 7);
        assert_eq!(decide(true, true, Some("bad"), Some(&v)), AuthDecision::Deny);
    }

    #[test]
    fn non_loopback_without_token_is_denied() {
        let v = verifier_accepting("good", 7);
        assert_eq!(decide(true, false, None, Some(&v)), AuthDecision::Deny);
    }

    #[test]
    fn non_loopback_with_bad_token_is_denied() {
        let v = verifier_accepting("good", 7);
        assert_eq!(decide(true, false, Some("bad"), Some(&v)), AuthDecision::Deny);
    }

    #[test]
    fn non_loopback_with_valid_token_is_admitted_with_key_id() {
        let v = verifier_accepting("good", 7);
        assert_eq!(decide(true, false, Some("good"), Some(&v)), AuthDecision::Allow(Some(7)));
    }

    #[test]
    fn fails_closed_without_verifier() {
        assert_eq!(decide(true, false, Some("anything"), None), AuthDecision::Deny);
    }
}
