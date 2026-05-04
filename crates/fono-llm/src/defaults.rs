// SPDX-License-Identifier: GPL-3.0-only
//! Provider-keyed default model strings for LLM cleanup. Mirrors
//! `fono_stt::defaults` for symmetry; kept in sync with the wizard.

#[must_use]
pub fn default_cloud_model(provider: &str) -> &'static str {
    match provider {
        // Cerebras retired llama-3.3-70b — `llama3.1-8b` is their
        // current cleanup-class recommendation (small, fast).
        "cerebras" => "llama3.1-8b",
        // Groq + OpenAI: use the smallest of the OpenAI-compat
        // family for cleanup; the assistant gets the larger sibling.
        // Groq exposes OpenAI's open-weight gpt-oss models under
        // an `openai/` namespace prefix.
        "groq" => "openai/gpt-oss-20b",
        "openai" => "gpt-5.4-nano",
        "anthropic" => "claude-haiku-4-5-20251001",
        "openrouter" => "openai/gpt-5.4-nano",
        "ollama" => "llama3.2",
        "gemini" => "gemini-1.5-flash",
        _ => "llama3.1-8b",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_resolve() {
        assert_eq!(default_cloud_model("cerebras"), "llama3.1-8b");
        assert_eq!(default_cloud_model("groq"), "openai/gpt-oss-20b");
        assert_eq!(default_cloud_model("openai"), "gpt-5.4-nano");
        assert_eq!(
            default_cloud_model("anthropic"),
            "claude-haiku-4-5-20251001"
        );
    }
}
