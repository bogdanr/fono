// SPDX-License-Identifier: GPL-3.0-only
//! Provider-keyed default model strings for LLM cleanup. Mirrors
//! `fono_stt::defaults` for symmetry; kept in sync with the wizard.

#[must_use]
pub fn default_cloud_model(provider: &str) -> &'static str {
    match provider {
        "cerebras" => "llama-3.3-70b",
        "groq" => "llama-3.3-70b-versatile",
        "openai" => "gpt-4o-mini",
        "anthropic" => "claude-3-5-haiku-latest",
        "openrouter" => "openai/gpt-4o-mini",
        "ollama" => "llama3.2",
        "gemini" => "gemini-1.5-flash",
        _ => "llama-3.3-70b",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_resolve() {
        assert_eq!(default_cloud_model("cerebras"), "llama-3.3-70b");
        assert_eq!(default_cloud_model("openai"), "gpt-4o-mini");
        assert_eq!(default_cloud_model("anthropic"), "claude-3-5-haiku-latest");
    }
}
