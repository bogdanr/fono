// SPDX-License-Identifier: GPL-3.0-only
//! Thin accessor over the cloud-provider catalogue for the LLM
//! factory + wizard. Mirrors `fono_stt::defaults`.
//!
//! The literal model strings live in
//! [`fono_core::provider_catalog::CLOUD_PROVIDERS`] — that array is the
//! single source of truth. To change the default cleanup model for a
//! cloud provider, edit its `PolishDefaults` entry there.
//!
//! Ollama has no catalogue entry (it's a self-hosted local server, not
//! a cloud provider) so its default is hard-coded here.

use fono_core::provider_catalog;

/// Default cloud polish model for `provider`. Reads the catalogue
/// for cloud providers; hard-codes Ollama (no catalogue entry); falls
/// back to `llama3.1-8b` for unknown ids.
#[must_use]
pub fn default_cloud_model(provider: &str) -> &'static str {
    if provider == "ollama" {
        return "llama3.2";
    }
    provider_catalog::find(provider).and_then(|p| p.polish).map_or("llama3.1-8b", |l| l.model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_resolve_via_catalogue() {
        assert_eq!(default_cloud_model("cerebras"), "gpt-oss-120b");
        assert_eq!(default_cloud_model("groq"), "openai/gpt-oss-20b");
        assert_eq!(default_cloud_model("openai"), "gpt-5.4-nano");
        assert_eq!(default_cloud_model("anthropic"), "claude-haiku-4-5-20251001");
        assert_eq!(default_cloud_model("openrouter"), "openai/gpt-5.4-nano");
        assert_eq!(default_cloud_model("gemini"), "gemini-1.5-flash");
    }

    #[test]
    fn ollama_is_special_cased() {
        assert_eq!(default_cloud_model("ollama"), "llama3.2");
    }

    #[test]
    fn unknown_falls_back() {
        assert_eq!(default_cloud_model("nope"), "llama3.1-8b");
    }
}
