// SPDX-License-Identifier: GPL-3.0-only
//! Thin accessor over the cloud-provider catalogue for the STT
//! factory + wizard.
//!
//! The literal model strings live in
//! [`fono_core::provider_catalog::CLOUD_PROVIDERS`] — that array is the
//! single source of truth. To change the default STT model for a
//! provider, edit its `SttDefaults` entry there.
//!
//! This wrapper exists only because the call sites
//! (`crate::factory::resolve_cloud`, the wizard, and `fono doctor`)
//! pre-date the catalogue and still expect a `&'static str` keyed by
//! provider id. It also supplies a generic Whisper fallback for
//! catalogue stubs that declare no STT capability (impossible in
//! practice for the providers wired into the factory, but safer than
//! panicking).

use fono_core::provider_catalog;

/// Default cloud STT model for `provider`. Looks up the catalogue
/// entry; falls back to `whisper-large-v3` when the provider is
/// unknown or has no STT capability declared.
#[must_use]
pub fn default_cloud_model(provider: &str) -> &'static str {
    provider_catalog::find(provider).and_then(|p| p.stt).map_or("whisper-large-v3", |s| s.model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_resolve_via_catalogue() {
        assert_eq!(default_cloud_model("groq"), "whisper-large-v3-turbo");
        assert_eq!(default_cloud_model("openai"), "whisper-1");
        assert_eq!(default_cloud_model("deepgram"), "nova-2");
        assert_eq!(default_cloud_model("openrouter"), "openai/whisper-large-v3-turbo");
    }

    #[test]
    fn unknown_falls_back() {
        assert_eq!(default_cloud_model("nope"), "whisper-large-v3");
    }
}
