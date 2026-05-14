// SPDX-License-Identifier: GPL-3.0-only
//! Upstream request-id capture.

use reqwest::header::HeaderMap;

/// Return the upstream provider's request-id header value if present.
///
/// Probes `x-request-id` first (used by OpenAI, Groq, OpenRouter, and
/// every OpenAI-compat host we've seen), then `request-id` (used by
/// Anthropic). Returns the first header that exists and decodes as
/// UTF-8. Returns `None` when neither header is present or the value
/// isn't valid UTF-8 — both are non-fatal: every error path through
/// the instrumentation helpers logs `request_id = "<none>"` rather
/// than crashing.
///
/// Keep this list synchronised with new providers: any host we add
/// that emits an upstream request id under yet another header name
/// should be added below so users have a single grep-target for
/// support tickets across the whole Fono provider matrix.
#[must_use]
pub fn provider_request_id(headers: &HeaderMap) -> Option<&str> {
    for name in ["x-request-id", "request-id"] {
        if let Some(v) = headers.get(name) {
            if let Ok(s) = v.to_str() {
                return Some(s);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn picks_x_request_id_first() {
        let mut h = HeaderMap::new();
        h.insert("x-request-id", HeaderValue::from_static("or-abc"));
        h.insert("request-id", HeaderValue::from_static("anth-xyz"));
        assert_eq!(provider_request_id(&h), Some("or-abc"));
    }

    #[test]
    fn falls_back_to_request_id() {
        let mut h = HeaderMap::new();
        h.insert("request-id", HeaderValue::from_static("anth-xyz"));
        assert_eq!(provider_request_id(&h), Some("anth-xyz"));
    }

    #[test]
    fn returns_none_when_absent() {
        let h = HeaderMap::new();
        assert_eq!(provider_request_id(&h), None);
    }

    #[test]
    fn returns_none_on_non_utf8() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-request-id",
            HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );
        assert_eq!(provider_request_id(&h), None);
    }
}
