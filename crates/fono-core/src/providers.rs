// SPDX-License-Identifier: GPL-3.0-only
//! Canonical provider metadata: enum-string mapping + default API-key
//! environment-variable name. Single source of truth shared by:
//!
//! * `fono-stt` / `fono-llm` factories (resolving `cloud=None`),
//! * `fono` CLI (`fono use`, `fono keys`),
//! * `fono` wizard (key prompts, multi-provider opt-in),
//! * `fono` doctor (per-provider reachability).
//!
//! Provider-switching plan S2 — keep changes here in sync with the
//! `defaults` modules in `fono-stt` / `fono-llm` for model strings.

use crate::config::{LlmBackend, SttBackend};

/// Canonical lower-case identifier for a STT backend (matches serde
/// rename and what users type on the CLI: `fono use stt groq`).
#[must_use]
pub const fn stt_backend_str(b: &SttBackend) -> &'static str {
    match b {
        SttBackend::Local => "local",
        SttBackend::Groq => "groq",
        SttBackend::Deepgram => "deepgram",
        SttBackend::OpenAI => "openai",
        SttBackend::Cartesia => "cartesia",
        SttBackend::AssemblyAI => "assemblyai",
        SttBackend::Azure => "azure",
        SttBackend::Speechmatics => "speechmatics",
        SttBackend::Google => "google",
        SttBackend::Nemotron => "nemotron",
        SttBackend::Wyoming => "wyoming",
    }
}

/// Parse a CLI-style provider string into a `SttBackend`. Returns `None`
/// for unknown strings; caller surfaces a clear error.
#[must_use]
pub fn parse_stt_backend(s: &str) -> Option<SttBackend> {
    match s.to_ascii_lowercase().as_str() {
        "local" => Some(SttBackend::Local),
        "groq" => Some(SttBackend::Groq),
        "deepgram" => Some(SttBackend::Deepgram),
        "openai" => Some(SttBackend::OpenAI),
        "cartesia" => Some(SttBackend::Cartesia),
        "assemblyai" => Some(SttBackend::AssemblyAI),
        "azure" => Some(SttBackend::Azure),
        "speechmatics" => Some(SttBackend::Speechmatics),
        "google" => Some(SttBackend::Google),
        "nemotron" => Some(SttBackend::Nemotron),
        "wyoming" => Some(SttBackend::Wyoming),
        _ => None,
    }
}

/// Canonical lower-case identifier for a LLM backend.
#[must_use]
pub const fn llm_backend_str(b: &LlmBackend) -> &'static str {
    match b {
        LlmBackend::Local => "local",
        LlmBackend::None => "none",
        LlmBackend::OpenAI => "openai",
        LlmBackend::Anthropic => "anthropic",
        LlmBackend::Gemini => "gemini",
        LlmBackend::Groq => "groq",
        LlmBackend::Cerebras => "cerebras",
        LlmBackend::OpenRouter => "openrouter",
        LlmBackend::Ollama => "ollama",
    }
}

#[must_use]
pub fn parse_llm_backend(s: &str) -> Option<LlmBackend> {
    match s.to_ascii_lowercase().as_str() {
        "local" => Some(LlmBackend::Local),
        "none" | "off" | "skip" => Some(LlmBackend::None),
        "openai" => Some(LlmBackend::OpenAI),
        "anthropic" => Some(LlmBackend::Anthropic),
        "gemini" => Some(LlmBackend::Gemini),
        "groq" => Some(LlmBackend::Groq),
        "cerebras" => Some(LlmBackend::Cerebras),
        "openrouter" => Some(LlmBackend::OpenRouter),
        "ollama" => Some(LlmBackend::Ollama),
        _ => None,
    }
}

/// Canonical environment-variable name where the API key for a given
/// STT backend is read from. Returned even for `Local` (where it's
/// unused) to keep callers branch-free; check `requires_key` first.
#[must_use]
pub const fn stt_key_env(b: &SttBackend) -> &'static str {
    match b {
        SttBackend::Local => "",
        SttBackend::Groq => "GROQ_API_KEY",
        SttBackend::Deepgram => "DEEPGRAM_API_KEY",
        SttBackend::OpenAI => "OPENAI_API_KEY",
        SttBackend::Cartesia => "CARTESIA_API_KEY",
        SttBackend::AssemblyAI => "ASSEMBLYAI_API_KEY",
        SttBackend::Azure => "AZURE_API_KEY",
        SttBackend::Speechmatics => "SPEECHMATICS_API_KEY",
        SttBackend::Google => "GOOGLE_API_KEY",
        SttBackend::Nemotron => "NEMOTRON_API_KEY",
        // Wyoming v1 has no in-band auth; an optional pre-shared token
        // is configured via `[stt.wyoming].auth_token_ref` instead.
        SttBackend::Wyoming => "",
    }
}

#[must_use]
pub const fn llm_key_env(b: &LlmBackend) -> &'static str {
    match b {
        LlmBackend::Local | LlmBackend::None | LlmBackend::Ollama => "",
        LlmBackend::OpenAI => "OPENAI_API_KEY",
        LlmBackend::Anthropic => "ANTHROPIC_API_KEY",
        LlmBackend::Gemini => "GEMINI_API_KEY",
        LlmBackend::Groq => "GROQ_API_KEY",
        LlmBackend::Cerebras => "CEREBRAS_API_KEY",
        LlmBackend::OpenRouter => "OPENROUTER_API_KEY",
    }
}

#[must_use]
pub const fn stt_requires_key(b: &SttBackend) -> bool {
    !matches!(b, SttBackend::Local | SttBackend::Wyoming)
}

#[must_use]
pub const fn llm_requires_key(b: &LlmBackend) -> bool {
    !matches!(b, LlmBackend::Local | LlmBackend::None | LlmBackend::Ollama)
}

/// Paired cloud preset for `fono use cloud <name>`. Returns `(stt, llm)`
/// for the preset, or `None` if the name isn't a known pair.
#[must_use]
pub fn cloud_pair(name: &str) -> Option<(SttBackend, LlmBackend)> {
    match name.to_ascii_lowercase().as_str() {
        "groq" => Some((SttBackend::Groq, LlmBackend::Groq)),
        // Cerebras has no STT product — pair with Groq's whisper-turbo,
        // which is the de-facto fast cloud STT today.
        "cerebras" => Some((SttBackend::Groq, LlmBackend::Cerebras)),
        "openai" => Some((SttBackend::OpenAI, LlmBackend::OpenAI)),
        "anthropic" => Some((SttBackend::Groq, LlmBackend::Anthropic)),
        "openrouter" => Some((SttBackend::Groq, LlmBackend::OpenRouter)),
        "deepgram" => Some((SttBackend::Deepgram, LlmBackend::Cerebras)),
        "assemblyai" => Some((SttBackend::AssemblyAI, LlmBackend::Cerebras)),
        _ => None,
    }
}

/// Iterator over every STT backend (for doctor enumeration etc.).
#[must_use]
pub fn all_stt_backends() -> [SttBackend; 11] {
    [
        SttBackend::Local,
        SttBackend::Groq,
        SttBackend::OpenAI,
        SttBackend::Deepgram,
        SttBackend::AssemblyAI,
        SttBackend::Cartesia,
        SttBackend::Azure,
        SttBackend::Speechmatics,
        SttBackend::Google,
        SttBackend::Nemotron,
        SttBackend::Wyoming,
    ]
}

#[must_use]
pub fn all_llm_backends() -> [LlmBackend; 9] {
    [
        LlmBackend::None,
        LlmBackend::Local,
        LlmBackend::Cerebras,
        LlmBackend::Groq,
        LlmBackend::OpenAI,
        LlmBackend::Anthropic,
        LlmBackend::OpenRouter,
        LlmBackend::Ollama,
        LlmBackend::Gemini,
    ]
}

/// Subset of [`all_stt_backends`] the user can actually pick today,
/// given the loaded `Secrets`. `Local` is always included; cloud
/// backends are included iff their API key is **explicitly listed in
/// `secrets.toml`**. The process environment is intentionally
/// ignored so a stray `OPENAI_API_KEY` exported in the user's shell
/// doesn't clutter the tray submenu — to surface a backend the user
/// must run `fono keys add <NAME>`. `active` is always included even
/// if its key is missing, so the tray reflects the current selection.
#[must_use]
pub fn configured_stt_backends(secrets: &crate::Secrets, active: &SttBackend) -> Vec<SttBackend> {
    all_stt_backends()
        .into_iter()
        .filter(|b| {
            if std::mem::discriminant(b) == std::mem::discriminant(active) {
                return true;
            }
            // Wyoming has no API key — its opt-in is `[stt.wyoming].uri`
            // (manual config) or mDNS discovery (Slice 4 will inject
            // discovered peers separately). Hide it from the menu
            // until then to avoid a dead row.
            if matches!(b, SttBackend::Wyoming) {
                return false;
            }
            if !stt_requires_key(b) {
                return true;
            }
            secrets.has_in_file(stt_key_env(b))
        })
        .collect()
}

/// Same idea as [`configured_stt_backends`] but for LLM backends.
/// Always includes `None` and `Local` (no key required). Ollama is
/// included only if `OLLAMA_HOST` appears in `secrets.toml` (or it's
/// the active backend), so users without a local Ollama server don't
/// see it in the tray menu. Like its STT cousin, the process
/// environment is ignored — only keys saved in `secrets.toml` count.
#[must_use]
pub fn configured_llm_backends(secrets: &crate::Secrets, active: &LlmBackend) -> Vec<LlmBackend> {
    all_llm_backends()
        .into_iter()
        .filter(|b| {
            if std::mem::discriminant(b) == std::mem::discriminant(active) {
                return true;
            }
            // Ollama doesn't have an API key but still needs an explicit
            // opt-in so users without an Ollama server don't see it in
            // the tray menu. Treat OLLAMA_HOST in secrets.toml as the
            // opt-in marker.
            if matches!(b, LlmBackend::Ollama) {
                return secrets.has_in_file("OLLAMA_HOST");
            }
            if !llm_requires_key(b) {
                return true;
            }
            secrets.has_in_file(llm_key_env(b))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stt_roundtrip() {
        for b in all_stt_backends() {
            let s = stt_backend_str(&b);
            assert_eq!(parse_stt_backend(s).unwrap(), b);
        }
    }

    #[test]
    fn llm_roundtrip() {
        for b in all_llm_backends() {
            let s = llm_backend_str(&b);
            assert_eq!(parse_llm_backend(s).unwrap(), b);
        }
    }

    #[test]
    fn unknown_returns_none() {
        assert!(parse_stt_backend("nope").is_none());
        assert!(parse_llm_backend("nope").is_none());
    }

    #[test]
    fn key_env_matches_provider() {
        assert_eq!(stt_key_env(&SttBackend::Groq), "GROQ_API_KEY");
        assert_eq!(llm_key_env(&LlmBackend::Cerebras), "CEREBRAS_API_KEY");
        assert!(stt_key_env(&SttBackend::Local).is_empty());
        assert!(llm_key_env(&LlmBackend::None).is_empty());
    }

    #[test]
    fn requires_key_flags() {
        assert!(!stt_requires_key(&SttBackend::Local));
        assert!(stt_requires_key(&SttBackend::Groq));
        assert!(!llm_requires_key(&LlmBackend::None));
        assert!(!llm_requires_key(&LlmBackend::Local));
        assert!(!llm_requires_key(&LlmBackend::Ollama));
        assert!(llm_requires_key(&LlmBackend::Cerebras));
    }

    #[test]
    fn cloud_pairs() {
        let (s, l) = cloud_pair("groq").unwrap();
        assert!(matches!(s, SttBackend::Groq));
        assert!(matches!(l, LlmBackend::Groq));
        let (s, l) = cloud_pair("cerebras").unwrap();
        assert!(matches!(s, SttBackend::Groq));
        assert!(matches!(l, LlmBackend::Cerebras));
        assert!(cloud_pair("nope").is_none());
    }

    #[test]
    fn configured_filter_ignores_env() {
        // Env-fallback would have leaked OPENAI_API_KEY into the menu;
        // the new filter must read secrets.toml only.
        std::env::set_var("OPENAI_API_KEY", "leaky-env-value");
        std::env::set_var("CEREBRAS_API_KEY", "leaky-env-value");
        let secrets = crate::Secrets::default(); // empty file
        let stt = configured_stt_backends(&secrets, &SttBackend::Local);
        let llm = configured_llm_backends(&secrets, &LlmBackend::None);
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("CEREBRAS_API_KEY");
        // Only key-free backends + the active one should be present.
        assert_eq!(
            stt,
            vec![SttBackend::Local],
            "env vars should not expand the STT menu"
        );
        assert!(
            !llm.iter().any(|b| matches!(b, LlmBackend::OpenAI)),
            "env-only OPENAI_API_KEY should not show OpenAI in the LLM menu"
        );
        assert!(
            !llm.iter().any(|b| matches!(b, LlmBackend::Cerebras)),
            "env-only CEREBRAS_API_KEY should not show Cerebras in the LLM menu"
        );
    }

    #[test]
    fn configured_filter_includes_explicit_keys() {
        let mut secrets = crate::Secrets::default();
        secrets.insert("GROQ_API_KEY", "gsk-explicit");
        secrets.insert("CEREBRAS_API_KEY", "cs-explicit");
        let stt = configured_stt_backends(&secrets, &SttBackend::Local);
        let llm = configured_llm_backends(&secrets, &LlmBackend::None);
        assert!(stt.iter().any(|b| matches!(b, SttBackend::Groq)));
        assert!(llm.iter().any(|b| matches!(b, LlmBackend::Cerebras)));
        // Backends without explicit keys must remain hidden.
        assert!(!stt.iter().any(|b| matches!(b, SttBackend::OpenAI)));
        assert!(!llm.iter().any(|b| matches!(b, LlmBackend::Anthropic)));
    }

    #[test]
    fn configured_filter_hides_ollama_without_host() {
        // Ollama has no API key but must still be opt-in via OLLAMA_HOST.
        let secrets = crate::Secrets::default();
        let llm = configured_llm_backends(&secrets, &LlmBackend::None);
        assert!(
            !llm.iter().any(|b| matches!(b, LlmBackend::Ollama)),
            "Ollama must be hidden until OLLAMA_HOST is configured"
        );

        let mut with_host = crate::Secrets::default();
        with_host.insert("OLLAMA_HOST", "http://localhost:11434");
        let llm = configured_llm_backends(&with_host, &LlmBackend::None);
        assert!(
            llm.iter().any(|b| matches!(b, LlmBackend::Ollama)),
            "Ollama must appear once OLLAMA_HOST is configured"
        );
    }
}
