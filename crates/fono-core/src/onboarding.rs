// SPDX-License-Identifier: GPL-3.0-only
//! Provider-neutral logic behind first-run onboarding, shared by the
//! browser welcome page and the terminal wizard so both offer the same
//! providers in the same order and recognise the same keys.
//!
//! Everything here is pure or close to it: no network, no config writes.
//! Callers test keys and persist them.

use sha2::{Digest, Sha256};

use crate::provider_catalog::{CloudProvider, FreeTier, CLOUD_PROVIDERS};

/// Shortest string accepted as a pasted API key. Every catalogued
/// provider's keys are well above this; it keeps a stray word that happens
/// to start with a prefix (`sk-…`) from being taken for a key.
const MIN_KEY_LEN: usize = 20;

/// Longest string accepted as a pasted API key. Real keys are under 200
/// characters; anything far longer is a document on the clipboard.
const MAX_KEY_LEN: usize = 512;

/// Whether a provider can run dictation on its own: speech-to-text plus
/// cleanup, with a key Fono can test. The unwired stubs carry no key
/// validation, so this also keeps them out.
fn runs_full_dictation(p: &CloudProvider) -> bool {
    p.stt.is_some() && p.polish.is_some() && p.key_validation.is_some()
}

/// Whether a provider is worth offering at all on the onboarding screen:
/// it covers at least one capability and its key can be tested.
fn offerable(p: &CloudProvider) -> bool {
    (p.stt.is_some() || p.polish.is_some()) && p.key_validation.is_some()
}

/// Providers for the "free" row: an ongoing free tier that covers full
/// dictation on Fono's default models. Catalogue order.
#[must_use]
pub fn free_dictation_providers() -> Vec<&'static CloudProvider> {
    CLOUD_PROVIDERS
        .iter()
        .filter(|p| p.onboarding.free_tier == FreeTier::Ongoing && runs_full_dictation(p))
        .collect()
}

/// Every other provider worth offering, for the collapsed "Other
/// services" list. Catalogue order.
#[must_use]
pub fn other_providers() -> Vec<&'static CloudProvider> {
    let free = free_dictation_providers();
    CLOUD_PROVIDERS.iter().filter(|p| offerable(p) && !free.iter().any(|f| f.id == p.id)).collect()
}

/// The free provider to badge as "Recommended": the first in catalogue
/// order, which lists the lowest-latency provider first. `None` when no
/// provider qualifies.
#[must_use]
pub fn recommended_free_provider() -> Option<&'static CloudProvider> {
    free_dictation_providers().into_iter().next()
}

/// Trim `text` and return it if it has the shape of an API key: one
/// token of printable ASCII, no whitespace, plausible length.
#[must_use]
pub fn key_candidate(text: &str) -> Option<&str> {
    let t = text.trim();
    let ok = (MIN_KEY_LEN..=MAX_KEY_LEN).contains(&t.len())
        && t.bytes().all(|b| b.is_ascii_graphic())
        && !t.contains("://");
    ok.then_some(t)
}

/// Recognise which provider a pasted key belongs to from its prefix.
///
/// The longest matching prefix wins, so `sk-or-…` resolves to OpenRouter
/// and `sk-ant-…` to Anthropic rather than to OpenAI's bare `sk-`, and
/// `sk_car_…` to Cartesia rather than ElevenLabs' `sk_`. Providers whose
/// keys have no distinctive prefix are never matched.
#[must_use]
pub fn detect_provider(text: &str) -> Option<&'static CloudProvider> {
    let key = key_candidate(text)?;
    CLOUD_PROVIDERS
        .iter()
        .filter(|p| offerable(p))
        .filter_map(|p| {
            p.onboarding
                .key_prefixes
                .iter()
                .filter(|pre| key.starts_with(**pre) && key.len() > pre.len())
                .map(|pre| pre.len())
                .max()
                .map(|len| (len, p))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, p)| p)
}

/// Whether `text` is a key for `provider` specifically. Used by the
/// clipboard watcher, which only ever accepts the provider the user is
/// connecting.
#[must_use]
pub fn is_key_for(text: &str, provider: &CloudProvider) -> bool {
    detect_provider(text).is_some_and(|p| p.id == provider.id)
}

/// A PKCE verifier and its S256 challenge (RFC 7636).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    /// Secret kept by Fono and sent only to the token exchange.
    pub verifier: String,
    /// `BASE64URL(SHA256(verifier))`, sent on the authorization URL.
    pub challenge: String,
}

impl Pkce {
    /// A fresh verifier from 32 random bytes (43 base64url characters,
    /// the RFC's recommended size).
    ///
    /// # Errors
    /// When the OS random source is unavailable.
    pub fn generate() -> Result<Self, getrandom::Error> {
        Ok(Self::from_verifier(random_token()?))
    }

    /// Build the pair for a known verifier.
    #[must_use]
    pub fn from_verifier(verifier: String) -> Self {
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge }
    }
}

/// 32 random bytes as base64url, for PKCE verifiers and OAuth `state`.
///
/// # Errors
/// When the OS random source is unavailable.
pub fn random_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)?;
    Ok(base64url(&bytes))
}

/// Unpadded base64url (RFC 4648 §5). Hand-rolled because the only users
/// are PKCE and `state`, which don't justify a dependency edge here.
#[must_use]
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            let idx = (n >> (18 - 6 * i)) & 0x3f;
            out.push(char::from(ALPHABET[idx as usize]));
        }
    }
    out
}

/// This computer's name, for labelling the keys Fono creates. Falls back
/// to `"computer"` when no source answers.
#[must_use]
pub fn host_name() -> String {
    system_host_name().unwrap_or_else(|| "computer".to_string())
}

/// This computer's host name as the system reports it, or `None` when no
/// source answers. Services started by systemd usually have no `HOSTNAME`
/// variable, and some systems have no `/etc/hostname`, so the kernel's own
/// name is tried as well.
#[must_use]
pub fn system_host_name() -> Option<String> {
    let files = ["/etc/hostname", "/proc/sys/kernel/hostname"];
    files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .chain(os_host_name())
        .chain(std::env::var("COMPUTERNAME").ok())
        .chain(std::env::var("HOSTNAME").ok())
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
fn os_host_name() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid for `buf.len()` bytes; gethostname writes at
    // most that many and the result is read only up to the first NUL.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

#[cfg(not(target_os = "macos"))]
fn os_host_name() -> Option<String> {
    None
}

/// The single-label form of `host` for an mDNS `<name>.local` record:
/// `box.example.com` and `box.local` both become `box`.
#[must_use]
pub fn short_host_name(host: &str) -> &str {
    host.split('.').next().unwrap_or(host)
}

/// The label for a key Fono creates: `fono-<host>`, lower-case, limited
/// to characters every provider accepts in a key name. Seeing this name
/// in a provider's key list tells the user which computer it belongs to.
#[must_use]
pub fn key_label(host: &str) -> String {
    let mut slug: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    slug.truncate(40);
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "fono".to_string()
    } else {
        format!("fono-{slug}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_host_name_keeps_the_first_label() {
        assert_eq!(short_host_name("box.example.com"), "box");
        assert_eq!(short_host_name("box.local"), "box");
        assert_eq!(short_host_name("box"), "box");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn system_host_name_is_found_without_the_environment() {
        // The kernel always has a name, even with no HOSTNAME variable.
        assert!(system_host_name().is_some());
    }

    fn id(text: &str) -> Option<&'static str> {
        detect_provider(text).map(|p| p.id)
    }

    #[test]
    fn free_row_is_groq_and_gemini() {
        let ids: Vec<_> = free_dictation_providers().iter().map(|p| p.id).collect();
        assert_eq!(ids, ["groq", "gemini"]);
        assert_eq!(recommended_free_provider().map(|p| p.id), Some("groq"));
    }

    #[test]
    fn other_row_excludes_free_and_stubs() {
        let ids: Vec<_> = other_providers().iter().map(|p| p.id).collect();
        assert!(ids.contains(&"openrouter"));
        assert!(ids.contains(&"cerebras"));
        for gone in ["groq", "gemini", "azure", "google", "nemotron"] {
            assert!(!ids.contains(&gone), "{gone} must not be in Other");
        }
    }

    #[test]
    fn every_free_provider_is_recognisable_and_reachable() {
        for p in free_dictation_providers() {
            assert!(!p.onboarding.key_prefixes.is_empty(), "{} needs a key prefix", p.id);
            assert!(!p.console_url.is_empty(), "{} needs a key page", p.id);
            assert!(!p.onboarding.card_line.is_empty(), "{} needs a card line", p.id);
        }
    }

    #[test]
    fn detects_each_prefix() {
        let tail = "A1b2C3d4E5f6G7h8I9j0K1l2";
        assert_eq!(id(&format!("gsk_{tail}")), Some("groq"));
        assert_eq!(id(&format!("AIza{tail}")), Some("gemini"));
        assert_eq!(id(&format!("sk-or-v1-{tail}")), Some("openrouter"));
        assert_eq!(id(&format!("sk-ant-api03-{tail}")), Some("anthropic"));
        assert_eq!(id(&format!("sk-proj-{tail}")), Some("openai"));
        assert_eq!(id(&format!("csk-{tail}")), Some("cerebras"));
        assert_eq!(id(&format!("sk_car_{tail}")), Some("cartesia"));
        assert_eq!(id(&format!("sk_{tail}")), Some("elevenlabs"));
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(id("  gsk_A1b2C3d4E5f6G7h8I9j0K1l2\n"), Some("groq"));
    }

    #[test]
    fn rejects_things_that_are_not_keys() {
        assert_eq!(id("gsk_short"), None);
        assert_eq!(id("gsk_A1b2C3d4E5f6 G7h8I9j0K1l2"), None, "inner space");
        assert_eq!(id("https://console.groq.com/keys?x=gsk_A1b2C3d4E5f6G7h8"), None);
        assert_eq!(id("hello world, this is a sentence"), None);
        assert_eq!(id("0123456789abcdef0123456789abcdef"), None, "bare hex has no owner");
        assert_eq!(id(&"gsk_".repeat(200)), None, "too long");
        assert_eq!(id("sk-"), None);
    }

    #[test]
    fn is_key_for_only_matches_the_named_provider() {
        let groq = crate::provider_catalog::find("groq").unwrap();
        let gemini = crate::provider_catalog::find("gemini").unwrap();
        let k = "gsk_A1b2C3d4E5f6G7h8I9j0K1l2";
        assert!(is_key_for(k, groq));
        assert!(!is_key_for(k, gemini));
    }

    /// RFC 7636 Appendix B test vector.
    #[test]
    fn pkce_matches_rfc_7636_vector() {
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_tokens_are_fresh_and_url_safe() {
        let a = Pkce::generate().unwrap();
        let b = Pkce::generate().unwrap();
        assert_ne!(a.verifier, b.verifier);
        assert_eq!(a.verifier.len(), 43);
        assert!(a.verifier.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
    }

    #[test]
    fn base64url_matches_rfc_4648_vectors() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn key_labels_are_safe() {
        assert_eq!(key_label("Laptop.local"), "fono-laptop-local");
        assert_eq!(key_label("  "), "fono");
        assert_eq!(key_label("DESKTOP-ABC123"), "fono-desktop-abc123");
    }
}
