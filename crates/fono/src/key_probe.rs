// SPDX-License-Identifier: GPL-3.0-only
//! Live API-key reachability probes shared by `fono keys check` and
//! `fono doctor`.
//!
//! Both surfaces used to report only whether a key was *present* in
//! `secrets.toml` / the environment — never whether it actually *works*.
//! An expired or revoked key looked identical to a good one. This module
//! closes that gap: for every key that has [`KeyValidation`] metadata in
//! the provider catalogue, it issues the same authenticated `GET` the
//! wizard uses at setup time and classifies the response.
//!
//! Probes run **concurrently** ([`futures::future::join_all`]) so the
//! total wait is the slowest single provider, not their sum.
//!
//! [`KeyValidation`]: fono_core::provider_catalog::KeyValidation

use std::collections::BTreeMap;
use std::time::Duration;

use fono_core::provider_catalog::build_auth_get;
use fono_core::Secrets;

/// Overall per-key timeout for a single liveness probe. Kept short so
/// `fono keys check` / `fono doctor` stay responsive even when one
/// provider is slow or unreachable. A healthy API answers well under a
/// second, so this only ever bites on genuinely stuck providers.
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Connect-phase timeout. A dead host (bad DNS / firewalled port) fails
/// here rather than burning the full [`PROBE_TIMEOUT`], so an offline
/// machine reports "unreachable" in a few seconds instead of five.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Every canonical API-key env-var name across the key-requiring
/// backends. STT + polish is the full union — TTS and assistant reuse
/// the same env vars — so enumerating those two covers every probeable
/// key. Callers filter by which are actually configured before probing.
#[must_use]
pub fn all_key_envs() -> Vec<String> {
    use fono_core::providers::{
        all_polish_backends, all_stt_backends, polish_key_env, polish_requires_key, stt_key_env,
        stt_requires_key,
    };
    let mut set = std::collections::BTreeSet::new();
    for b in all_stt_backends() {
        if stt_requires_key(&b) {
            set.insert(stt_key_env(&b).to_string());
        }
    }
    for b in all_polish_backends() {
        if polish_requires_key(&b) {
            set.insert(polish_key_env(&b).to_string());
        }
    }
    set.into_iter().collect()
}

/// Outcome of a single live key probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyReachability {
    /// Provider accepted the key (HTTP 2xx).
    Valid,
    /// Provider rejected the key — expired, revoked, or wrong
    /// (HTTP 401 / 403). Carries the status code for the detail line.
    Rejected(u16),
    /// The provider answered, but with an unexpected non-success status
    /// (e.g. 429 rate-limit, 5xx). The key is probably fine; the probe
    /// just couldn't confirm it right now.
    Unexpected(u16),
    /// Could not reach the provider at all (DNS / TCP / TLS / timeout).
    Unreachable(String),
    /// The catalogue has no validation endpoint for this key, so it
    /// cannot be probed (e.g. the unwired azure/google/nemotron stubs).
    NoProbe,
}

impl KeyReachability {
    /// Whether this outcome means the key definitely does not work.
    #[must_use]
    pub fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }

    /// A short human summary for the CLI / doctor detail column.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Valid => "works".to_string(),
            Self::Rejected(code) => {
                format!("REJECTED (HTTP {code} — expired or invalid)")
            }
            Self::Unexpected(code) => {
                format!("unverified (provider returned HTTP {code})")
            }
            Self::Unreachable(e) => format!("unreachable ({e})"),
            Self::NoProbe => "not verifiable (no probe endpoint)".to_string(),
        }
    }
}

/// Probe every env-var name in `key_envs` that resolves to a value,
/// concurrently. The returned map is keyed by env-var name; names that
/// resolve to no key are omitted. Values are resolved via
/// [`Secrets::resolve`], so both `secrets.toml` and the process
/// environment are honoured (matching the daemon's own key lookup).
pub async fn probe_keys(
    key_envs: &[String],
    secrets: &Secrets,
) -> BTreeMap<String, KeyReachability> {
    let client = match reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("fono-doctor/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        // If the HTTP stack itself won't build, report every configured
        // key as unreachable rather than silently dropping the probes.
        Err(e) => {
            return key_envs
                .iter()
                .filter(|env| secrets.resolve(env).is_some())
                .map(|env| (env.clone(), KeyReachability::Unreachable(e.to_string())))
                .collect();
        }
    };

    let probes = key_envs.iter().filter_map(|env| {
        let key = secrets.resolve(env)?;
        let client = client.clone();
        let env = env.clone();
        Some(async move {
            let status = probe_one(&client, &env, &key).await;
            (env, status)
        })
    });

    futures::future::join_all(probes).await.into_iter().collect()
}

/// Probe a single key against its catalogue validation endpoint.
async fn probe_one(client: &reqwest::Client, key_env: &str, key: &str) -> KeyReachability {
    let Some(entry) = fono_core::provider_catalog::find_by_key_env(key_env) else {
        return KeyReachability::NoProbe;
    };
    let Some(validation) = entry.key_validation else {
        return KeyReachability::NoProbe;
    };
    let (url, headers) =
        build_auth_get(validation.url, validation.auth, key, validation.extra_headers);
    let mut req = client.get(url);
    for (h, v) in headers {
        req = req.header(h, v);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            // Drain the body so the connection returns to the pool.
            let _ = resp.bytes().await;
            let code = status.as_u16();
            if status.is_success() {
                KeyReachability::Valid
            } else if code == 401 || code == 403 {
                KeyReachability::Rejected(code)
            } else {
                KeyReachability::Unexpected(code)
            }
        }
        Err(e) => KeyReachability::Unreachable(short_err(&e)),
    }
}

/// Condense a reqwest error into a short, user-facing phrase (the full
/// error chain is too noisy for a status column).
fn short_err(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".to_string()
    } else if e.is_connect() {
        "connection failed".to_string()
    } else {
        "network error".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_are_distinct_and_human() {
        assert_eq!(KeyReachability::Valid.summary(), "works");
        assert!(KeyReachability::Rejected(401).summary().contains("401"));
        assert!(KeyReachability::Rejected(401).summary().contains("expired"));
        assert!(KeyReachability::Unexpected(429).summary().contains("429"));
        assert!(KeyReachability::Unreachable("timed out".into()).summary().contains("timed out"));
        assert!(KeyReachability::NoProbe.summary().contains("no probe"));
    }

    #[test]
    fn only_rejection_counts_as_definitely_broken() {
        assert!(KeyReachability::Rejected(403).is_rejected());
        assert!(!KeyReachability::Valid.is_rejected());
        assert!(!KeyReachability::Unexpected(500).is_rejected());
        assert!(!KeyReachability::Unreachable("x".into()).is_rejected());
        assert!(!KeyReachability::NoProbe.is_rejected());
    }

    /// `all_key_envs` returns a de-duplicated, sorted set that includes
    /// providers sharing an env var only once.
    #[test]
    fn key_envs_are_sorted_and_deduped() {
        let envs = all_key_envs();
        let mut sorted = envs.clone();
        sorted.sort();
        assert_eq!(envs, sorted, "must be sorted");
        let unique: std::collections::BTreeSet<_> = envs.iter().collect();
        assert_eq!(unique.len(), envs.len(), "must be de-duplicated");
        assert!(envs.iter().any(|e| e == "GROQ_API_KEY"));
    }

    /// Keys that resolve to no value are omitted; there is nothing to
    /// probe and we must not fabricate an entry for them.
    #[tokio::test]
    async fn absent_keys_are_skipped() {
        let secrets = Secrets::default();
        let out = probe_keys(&["DEFINITELY_UNSET_KEY_ENV".to_string()], &secrets).await;
        assert!(out.is_empty());
    }
}
