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
    !matches!(b, SttBackend::Local)
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
pub fn all_stt_backends() -> [SttBackend; 10] {
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
}
