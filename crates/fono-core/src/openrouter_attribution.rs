// SPDX-License-Identifier: GPL-3.0-only
//! OpenRouter app-attribution headers — the single source of truth.
//!
//! OpenRouter aggregates request volume by the `HTTP-Referer` header
//! into a public app page, leaderboard entry, and per-model "Apps"
//! tab; see <https://openrouter.ai/docs/app-attribution>. Fono
//! identifies itself on every outbound request to `openrouter.ai`
//! using the constants below.
//!
//! The values are baked into the binary and are identical across all
//! installations: no per-user, per-machine, or per-request identifier
//! is ever embedded. The attribution payload is exactly three static
//! HTTP headers; nothing about the request body (audio, transcript,
//! prompt, etc.) is affected.
//!
//! ## Header reference
//!
//! - `HTTP-Referer` (required) — primary identifier; becomes the URL
//!   key of the public app page at
//!   `https://openrouter.ai/apps?url=<REFERER>`.
//! - `X-OpenRouter-Title` — display name shown in rankings and
//!   analytics. `X-Title` is the legacy synonym and is intentionally
//!   not sent: we only send the canonical header.
//! - `X-OpenRouter-Categories` — comma-separated list (≤ 2 per
//!   request, ≤ 10 merged over an app's lifetime). Values must match
//!   the published vocabulary; unknown ones are silently dropped by
//!   OpenRouter, so a stale value is a no-op rather than a hard
//!   failure.

/// Canonical project homepage. Becomes the OpenRouter app-page key.
pub const REFERER: &str = "https://fono.page";

/// Display name shown in rankings and on individual model pages.
pub const TITLE: &str = "Fono";

/// Comma-separated marketplace category list. Fono is a desktop
/// dictation tool and voice assistant, so both `personal-agent` and
/// `writing-assistant` apply (≤ 2 categories per request is the
/// per-request cap documented at <https://openrouter.ai/docs/app-attribution>).
pub const CATEGORIES: &str = "personal-agent,writing-assistant";

/// Host substring used by call sites that share a client across
/// multiple providers (e.g. the OpenAI-compatible TTS client) to gate
/// attribution headers so they don't leak into requests targeting
/// non-OpenRouter providers.
pub const HOST_SUFFIX: &str = "openrouter.ai";

/// Returns the three `(name, value)` header tuples that every
/// outbound OpenRouter request must carry. Consumer crates that
/// already depend on `reqwest` apply them via:
///
/// ```ignore
/// let mut req = client.post(url).bearer_auth(&key);
/// for (name, value) in fono_core::openrouter_attribution::headers() {
///     req = req.header(name, value);
/// }
/// ```
///
/// This shape keeps `fono-core` free of any `reqwest` dependency while
/// still centralising the wire format.
#[must_use]
pub const fn headers() -> [(&'static str, &'static str); 3] {
    [
        ("HTTP-Referer", REFERER),
        ("X-OpenRouter-Title", TITLE),
        ("X-OpenRouter-Categories", CATEGORIES),
    ]
}

/// Returns `true` when the given URL targets OpenRouter and therefore
/// should carry attribution headers. Used by shared OpenAI-compatible
/// clients that route to multiple providers from a single struct.
#[must_use]
pub fn is_openrouter_url(url: &str) -> bool {
    url.contains(HOST_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::const_is_empty)]
    fn constants_are_well_formed() {
        // Referer must be an https URL: OpenRouter normalises and
        // displays it verbatim on the public app page.
        assert!(REFERER.starts_with("https://"));
        // Title is short and human-readable — not an internal slug.
        assert!(!TITLE.is_empty());
        assert!(TITLE.len() < 64);
        // Categories: ≤ 2 entries, lowercase, hyphen-separated, each
        // ≤ 30 chars. Unknown categories are silently dropped by
        // OpenRouter, so a malformed value here would degrade
        // marketplace placement to nothing without any runtime error.
        let parts: Vec<&str> = CATEGORIES.split(',').collect();
        assert!(
            parts.len() <= 2,
            "OpenRouter caps categories at 2 per request"
        );
        for part in parts {
            assert!(!part.is_empty());
            assert!(part.len() <= 30, "category {part} exceeds 30 chars");
            assert!(
                part.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "category {part} must be lowercase + hyphens only"
            );
        }
    }

    #[test]
    fn headers_match_constants() {
        let h = headers();
        assert_eq!(h[0], ("HTTP-Referer", REFERER));
        assert_eq!(h[1], ("X-OpenRouter-Title", TITLE));
        assert_eq!(h[2], ("X-OpenRouter-Categories", CATEGORIES));
    }

    #[test]
    fn url_gating_picks_only_openrouter() {
        assert!(is_openrouter_url(
            "https://openrouter.ai/api/v1/chat/completions"
        ));
        assert!(is_openrouter_url(
            "https://openrouter.ai/api/v1/audio/speech"
        ));
        assert!(!is_openrouter_url(
            "https://api.openai.com/v1/chat/completions"
        ));
        assert!(!is_openrouter_url(
            "https://api.groq.com/openai/v1/chat/completions"
        ));
        assert!(!is_openrouter_url(
            "https://api.cerebras.ai/v1/chat/completions"
        ));
        assert!(!is_openrouter_url(
            "http://localhost:11434/v1/chat/completions"
        ));
    }
}
