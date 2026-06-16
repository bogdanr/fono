// SPDX-License-Identifier: GPL-3.0-only
//! Canonical provider metadata: enum-string mapping + default API-key
//! environment-variable name. Single source of truth shared by:
//!
//! * `fono-stt` / `fono-polish` factories (resolving `cloud=None`),
//! * `fono` CLI (`fono use`, `fono keys`),
//! * `fono` wizard (key prompts, multi-provider opt-in),
//! * `fono` doctor (per-provider reachability).
//!
//! Provider-switching plan S2 — keep changes here in sync with the
//! `defaults` modules in `fono-stt` / `fono-polish` for model strings.

use crate::config::{AssistantBackend, PolishBackend, SttBackend, TtsBackend};

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
        SttBackend::ElevenLabs => "elevenlabs",
        SttBackend::OpenRouter => "openrouter",
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
        "elevenlabs" => Some(SttBackend::ElevenLabs),
        "openrouter" => Some(SttBackend::OpenRouter),
        "wyoming" => Some(SttBackend::Wyoming),
        _ => None,
    }
}

/// Canonical lower-case identifier for a polish backend.
#[must_use]
pub const fn polish_backend_str(b: &PolishBackend) -> &'static str {
    match b {
        PolishBackend::Local => "local",
        PolishBackend::None => "none",
        PolishBackend::OpenAI => "openai",
        PolishBackend::Anthropic => "anthropic",
        PolishBackend::Gemini => "gemini",
        PolishBackend::Groq => "groq",
        PolishBackend::Cerebras => "cerebras",
        PolishBackend::OpenRouter => "openrouter",
        PolishBackend::Ollama => "ollama",
    }
}

#[must_use]
pub fn parse_polish_backend(s: &str) -> Option<PolishBackend> {
    match s.to_ascii_lowercase().as_str() {
        "local" => Some(PolishBackend::Local),
        "none" | "off" | "skip" => Some(PolishBackend::None),
        "openai" => Some(PolishBackend::OpenAI),
        "anthropic" => Some(PolishBackend::Anthropic),
        "gemini" => Some(PolishBackend::Gemini),
        "groq" => Some(PolishBackend::Groq),
        "cerebras" => Some(PolishBackend::Cerebras),
        "openrouter" => Some(PolishBackend::OpenRouter),
        "ollama" => Some(PolishBackend::Ollama),
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
        SttBackend::ElevenLabs => "ELEVENLABS_API_KEY",
        SttBackend::OpenRouter => "OPENROUTER_API_KEY",
        // Wyoming v1 has no in-band auth; an optional pre-shared token
        // is configured via `[stt.wyoming].auth_token_ref` instead.
        SttBackend::Wyoming => "",
    }
}

#[must_use]
pub const fn polish_key_env(b: &PolishBackend) -> &'static str {
    match b {
        PolishBackend::Local | PolishBackend::None | PolishBackend::Ollama => "",
        PolishBackend::OpenAI => "OPENAI_API_KEY",
        PolishBackend::Anthropic => "ANTHROPIC_API_KEY",
        PolishBackend::Gemini => "GEMINI_API_KEY",
        PolishBackend::Groq => "GROQ_API_KEY",
        PolishBackend::Cerebras => "CEREBRAS_API_KEY",
        PolishBackend::OpenRouter => "OPENROUTER_API_KEY",
    }
}

#[must_use]
pub const fn stt_requires_key(b: &SttBackend) -> bool {
    !matches!(b, SttBackend::Local | SttBackend::Wyoming)
}

#[must_use]
pub const fn polish_requires_key(b: &PolishBackend) -> bool {
    !matches!(b, PolishBackend::Local | PolishBackend::None | PolishBackend::Ollama)
}

/// Canonical lower-case identifier for a TTS backend.
#[must_use]
pub const fn tts_backend_str(b: &TtsBackend) -> &'static str {
    match b {
        TtsBackend::None => "none",
        TtsBackend::Wyoming => "wyoming",
        TtsBackend::OpenAI => "openai",
        TtsBackend::Groq => "groq",
        TtsBackend::OpenRouter => "openrouter",
        TtsBackend::Cartesia => "cartesia",
        TtsBackend::Deepgram => "deepgram",
        TtsBackend::Speechmatics => "speechmatics",
        TtsBackend::ElevenLabs => "elevenlabs",
        TtsBackend::Local => "local",
    }
}

#[must_use]
pub fn parse_tts_backend(s: &str) -> Option<TtsBackend> {
    match s.to_ascii_lowercase().as_str() {
        "none" | "off" | "skip" => Some(TtsBackend::None),
        "wyoming" => Some(TtsBackend::Wyoming),
        "openai" => Some(TtsBackend::OpenAI),
        "groq" => Some(TtsBackend::Groq),
        "openrouter" => Some(TtsBackend::OpenRouter),
        "cartesia" => Some(TtsBackend::Cartesia),
        "deepgram" => Some(TtsBackend::Deepgram),
        "speechmatics" => Some(TtsBackend::Speechmatics),
        "elevenlabs" => Some(TtsBackend::ElevenLabs),
        "local" => Some(TtsBackend::Local),
        _ => None,
    }
}

/// Canonical environment-variable name for the API key of a cloud
/// TTS backend. Returned even for `None`/`Wyoming` (where it's
/// unused) for branch-free callers; check [`tts_requires_key`] first.
#[must_use]
pub const fn tts_key_env(b: &TtsBackend) -> &'static str {
    match b {
        TtsBackend::None | TtsBackend::Wyoming | TtsBackend::Local => "",
        TtsBackend::OpenAI => "OPENAI_API_KEY",
        TtsBackend::Groq => "GROQ_API_KEY",
        TtsBackend::OpenRouter => "OPENROUTER_API_KEY",
        TtsBackend::Cartesia => "CARTESIA_API_KEY",
        TtsBackend::Deepgram => "DEEPGRAM_API_KEY",
        TtsBackend::Speechmatics => "SPEECHMATICS_API_KEY",
        TtsBackend::ElevenLabs => "ELEVENLABS_API_KEY",
    }
}

#[must_use]
pub const fn tts_requires_key(b: &TtsBackend) -> bool {
    matches!(
        b,
        TtsBackend::OpenAI
            | TtsBackend::Groq
            | TtsBackend::OpenRouter
            | TtsBackend::Cartesia
            | TtsBackend::Deepgram
            | TtsBackend::Speechmatics
            | TtsBackend::ElevenLabs
    )
}

/// Canonical lower-case identifier for an assistant chat backend.
#[must_use]
pub const fn assistant_backend_str(b: &AssistantBackend) -> &'static str {
    match b {
        AssistantBackend::None => "none",
        AssistantBackend::OpenAI => "openai",
        AssistantBackend::Anthropic => "anthropic",
        AssistantBackend::Groq => "groq",
        AssistantBackend::Cerebras => "cerebras",
        AssistantBackend::OpenRouter => "openrouter",
        AssistantBackend::Ollama => "local",
    }
}

#[must_use]
pub fn parse_assistant_backend(s: &str) -> Option<AssistantBackend> {
    match s.to_ascii_lowercase().as_str() {
        "none" | "off" | "skip" => Some(AssistantBackend::None),
        "openai" => Some(AssistantBackend::OpenAI),
        "anthropic" => Some(AssistantBackend::Anthropic),
        "groq" => Some(AssistantBackend::Groq),
        "cerebras" => Some(AssistantBackend::Cerebras),
        "openrouter" => Some(AssistantBackend::OpenRouter),
        "local" | "ollama" => Some(AssistantBackend::Ollama),
        _ => None,
    }
}

/// Canonical environment-variable name for the API key of a cloud
/// assistant backend. Reuses the same env vars as the polish
/// path (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc.), so a single
/// stored key serves both consumers without duplicate prompts in
/// the wizard.
#[must_use]
pub const fn assistant_key_env(b: &AssistantBackend) -> &'static str {
    match b {
        AssistantBackend::None | AssistantBackend::Ollama => "",
        AssistantBackend::OpenAI => "OPENAI_API_KEY",
        AssistantBackend::Anthropic => "ANTHROPIC_API_KEY",
        AssistantBackend::Groq => "GROQ_API_KEY",
        AssistantBackend::Cerebras => "CEREBRAS_API_KEY",
        AssistantBackend::OpenRouter => "OPENROUTER_API_KEY",
    }
}

#[must_use]
pub const fn assistant_requires_key(b: &AssistantBackend) -> bool {
    !matches!(b, AssistantBackend::None | AssistantBackend::Ollama)
}

/// Paired cloud preset for `fono use cloud <name>`. Returns `(stt, polish)`
/// for the preset, or `None` if the name isn't a known pair.
///
/// Looks up [`crate::provider_catalog::CLOUD_PROVIDERS`] as the source of
/// truth for which provider id offers which capabilities. When the
/// catalogue entry lacks an STT capability (e.g. Cerebras, Anthropic,
/// OpenRouter), the pair falls back to Groq's whisper-turbo — the
/// de-facto fast cloud STT today. When the entry lacks an LLM
/// capability (Deepgram, AssemblyAI), the pair falls back to Cerebras
/// for cleanup.
#[must_use]
pub fn cloud_pair(name: &str) -> Option<(SttBackend, PolishBackend)> {
    let id = name.to_ascii_lowercase();
    let entry = crate::provider_catalog::find(&id)?;
    // Resolve STT: prefer the entry's own STT capability, otherwise
    // fall back to Groq's whisper-turbo.
    let stt = if entry.stt.is_some() { parse_stt_backend(entry.id)? } else { SttBackend::Groq };
    // Resolve LLM: prefer the entry's own LLM capability, otherwise
    // fall back to Cerebras for cleanup (for STT-only providers like
    // Deepgram and AssemblyAI).
    let polish = if entry.polish.is_some() {
        parse_polish_backend(entry.id)?
    } else {
        PolishBackend::Cerebras
    };
    Some((stt, polish))
}

/// Iterator over every STT backend (for doctor enumeration etc.).
#[must_use]
pub fn all_stt_backends() -> [SttBackend; 13] {
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
        SttBackend::ElevenLabs,
        SttBackend::OpenRouter,
        SttBackend::Wyoming,
    ]
}

#[must_use]
pub fn all_polish_backends() -> [PolishBackend; 9] {
    [
        PolishBackend::None,
        PolishBackend::Local,
        PolishBackend::Cerebras,
        PolishBackend::Groq,
        PolishBackend::OpenAI,
        PolishBackend::Anthropic,
        PolishBackend::OpenRouter,
        PolishBackend::Ollama,
        PolishBackend::Gemini,
    ]
}

#[must_use]
pub fn all_assistant_backends() -> [AssistantBackend; 7] {
    [
        AssistantBackend::None,
        AssistantBackend::Cerebras,
        AssistantBackend::Groq,
        AssistantBackend::OpenAI,
        AssistantBackend::Anthropic,
        AssistantBackend::OpenRouter,
        AssistantBackend::Ollama,
    ]
}

#[must_use]
pub fn all_tts_backends() -> [TtsBackend; 10] {
    [
        TtsBackend::None,
        TtsBackend::Wyoming,
        TtsBackend::OpenAI,
        TtsBackend::Groq,
        TtsBackend::OpenRouter,
        TtsBackend::Cartesia,
        TtsBackend::Deepgram,
        TtsBackend::Speechmatics,
        TtsBackend::ElevenLabs,
        TtsBackend::Local,
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

/// Same idea as [`configured_stt_backends`] but for polish backends.
/// Always includes `None` and `Local` (no key required). Ollama is
/// included only if `OLLAMA_HOST` appears in `secrets.toml` (or it's
/// the active backend), so users without a local Ollama server don't
/// see it in the tray menu. Like its STT cousin, the process
/// environment is ignored — only keys saved in `secrets.toml` count.
#[must_use]
pub fn configured_polish_backends(
    secrets: &crate::Secrets,
    active: &PolishBackend,
) -> Vec<PolishBackend> {
    all_polish_backends()
        .into_iter()
        .filter(|b| {
            if std::mem::discriminant(b) == std::mem::discriminant(active) {
                return true;
            }
            // Ollama doesn't have an API key but still needs an explicit
            // opt-in so users without an Ollama server don't see it in
            // the tray menu. Treat OLLAMA_HOST in secrets.toml as the
            // opt-in marker.
            if matches!(b, PolishBackend::Ollama) {
                return secrets.has_in_file("OLLAMA_HOST");
            }
            if !polish_requires_key(b) {
                return true;
            }
            secrets.has_in_file(polish_key_env(b))
        })
        .collect()
}

/// Same idea as [`configured_stt_backends`] but for TTS backends.
/// Order: backends whose API key is already present in `secrets.toml`
/// come first (so the tray's "(cloud, key already set)" entries lead),
/// followed by Wyoming if the user has configured a `[tts.wyoming]`
/// peer (or it's the active backend), followed by every remaining
/// cloud backend (which would prompt for a key). Always includes the
/// currently-active backend so the tray reflects reality even if its
/// key isn't in `secrets.toml`. Like its STT cousin, the process
/// environment is ignored — only keys saved in `secrets.toml` count.
///
/// `None` is intentionally excluded — it is not a real switchable
/// option. Always includes the currently-active backend so the tray
/// reflects reality even if its key isn't in `secrets.toml`. Like its
/// STT cousin, the process environment is ignored — only keys saved
/// in `secrets.toml` count.
#[must_use]
pub fn configured_tts_backends(
    secrets: &crate::Secrets,
    active: &TtsBackend,
    has_wyoming_block: bool,
) -> Vec<TtsBackend> {
    let mut with_key: Vec<TtsBackend> = Vec::new();
    let mut without_key: Vec<TtsBackend> = Vec::new();
    let mut wyoming: Option<TtsBackend> = None;
    for b in all_tts_backends() {
        if matches!(b, TtsBackend::None) {
            // None is not a real entry; only include when active.
            if std::mem::discriminant(&b) == std::mem::discriminant(active) {
                without_key.push(b);
            }
            continue;
        }
        if matches!(b, TtsBackend::Wyoming) {
            let active_match = std::mem::discriminant(&b) == std::mem::discriminant(active);
            if has_wyoming_block || active_match {
                wyoming = Some(b);
            }
            continue;
        }
        let active_match = std::mem::discriminant(&b) == std::mem::discriminant(active);
        if tts_requires_key(&b) && secrets.has_in_file(tts_key_env(&b)) {
            with_key.push(b);
        } else if active_match {
            // Active backend without a stored key — still show.
            without_key.push(b);
        } else {
            without_key.push(b);
        }
    }
    let mut out = with_key;
    if let Some(w) = wyoming {
        out.push(w);
    }
    out.extend(without_key);
    out
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
        for b in all_polish_backends() {
            let s = polish_backend_str(&b);
            assert_eq!(parse_polish_backend(s).unwrap(), b);
        }
    }

    #[test]
    fn unknown_returns_none() {
        assert!(parse_stt_backend("nope").is_none());
        assert!(parse_polish_backend("nope").is_none());
    }

    #[test]
    fn assistant_local_backend_is_user_facing_local_with_manual_ollama_alias() {
        assert_eq!(assistant_backend_str(&AssistantBackend::Ollama), "local");
        assert_eq!(parse_assistant_backend("local"), Some(AssistantBackend::Ollama));
        assert_eq!(parse_assistant_backend("ollama"), Some(AssistantBackend::Ollama));
    }

    #[test]
    fn key_env_matches_provider() {
        assert_eq!(stt_key_env(&SttBackend::Groq), "GROQ_API_KEY");
        assert_eq!(polish_key_env(&PolishBackend::Cerebras), "CEREBRAS_API_KEY");
        assert!(stt_key_env(&SttBackend::Local).is_empty());
        assert!(polish_key_env(&PolishBackend::None).is_empty());
    }

    #[test]
    fn requires_key_flags() {
        assert!(!stt_requires_key(&SttBackend::Local));
        assert!(stt_requires_key(&SttBackend::Groq));
        assert!(!polish_requires_key(&PolishBackend::None));
        assert!(!polish_requires_key(&PolishBackend::Local));
        assert!(!polish_requires_key(&PolishBackend::Ollama));
        assert!(polish_requires_key(&PolishBackend::Cerebras));
    }

    #[test]
    fn cloud_pairs() {
        let (s, l) = cloud_pair("groq").unwrap();
        assert!(matches!(s, SttBackend::Groq));
        assert!(matches!(l, PolishBackend::Groq));
        let (s, l) = cloud_pair("cerebras").unwrap();
        assert!(matches!(s, SttBackend::Groq));
        assert!(matches!(l, PolishBackend::Cerebras));
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
        let polish = configured_polish_backends(&secrets, &PolishBackend::None);
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("CEREBRAS_API_KEY");
        // Only key-free backends + the active one should be present.
        assert_eq!(stt, vec![SttBackend::Local], "env vars should not expand the STT menu");
        assert!(
            !polish.iter().any(|b| matches!(b, PolishBackend::OpenAI)),
            "env-only OPENAI_API_KEY should not show OpenAI in the LLM menu"
        );
        assert!(
            !polish.iter().any(|b| matches!(b, PolishBackend::Cerebras)),
            "env-only CEREBRAS_API_KEY should not show Cerebras in the LLM menu"
        );
    }

    #[test]
    fn configured_filter_includes_explicit_keys() {
        let mut secrets = crate::Secrets::default();
        secrets.insert("GROQ_API_KEY", "gsk-explicit");
        secrets.insert("CEREBRAS_API_KEY", "cs-explicit");
        let stt = configured_stt_backends(&secrets, &SttBackend::Local);
        let polish = configured_polish_backends(&secrets, &PolishBackend::None);
        assert!(stt.iter().any(|b| matches!(b, SttBackend::Groq)));
        assert!(polish.iter().any(|b| matches!(b, PolishBackend::Cerebras)));
        // Backends without explicit keys must remain hidden.
        assert!(!stt.iter().any(|b| matches!(b, SttBackend::OpenAI)));
        assert!(!polish.iter().any(|b| matches!(b, PolishBackend::Anthropic)));
    }

    #[test]
    fn configured_filter_hides_ollama_without_host() {
        // Ollama has no API key but must still be opt-in via OLLAMA_HOST.
        let secrets = crate::Secrets::default();
        let polish = configured_polish_backends(&secrets, &PolishBackend::None);
        assert!(
            !polish.iter().any(|b| matches!(b, PolishBackend::Ollama)),
            "Ollama must be hidden until OLLAMA_HOST is configured"
        );

        let mut with_host = crate::Secrets::default();
        with_host.insert("OLLAMA_HOST", "http://localhost:11434");
        let polish = configured_polish_backends(&with_host, &PolishBackend::None);
        assert!(
            polish.iter().any(|b| matches!(b, PolishBackend::Ollama)),
            "Ollama must appear once OLLAMA_HOST is configured"
        );
    }

    /// Phase F regression: every TTS backend variant must round-trip
    /// through `parse_tts_backend` / `tts_backend_str`.
    #[test]
    fn tts_roundtrip() {
        for b in all_tts_backends() {
            let s = tts_backend_str(&b);
            assert_eq!(parse_tts_backend(s).unwrap(), b);
        }
        // New Phase F variants explicitly:
        assert_eq!(parse_tts_backend("groq"), Some(TtsBackend::Groq));
        assert_eq!(parse_tts_backend("openrouter"), Some(TtsBackend::OpenRouter));
        assert_eq!(parse_tts_backend("cartesia"), Some(TtsBackend::Cartesia));
        assert_eq!(parse_tts_backend("deepgram"), Some(TtsBackend::Deepgram));
    }

    /// Phase F: every new cloud TTS backend reports the canonical
    /// env-var name. Mirrors `key_env_matches_provider` for STT/LLM.
    #[test]
    fn tts_key_env_matches_provider() {
        assert_eq!(tts_key_env(&TtsBackend::Groq), "GROQ_API_KEY");
        assert_eq!(tts_key_env(&TtsBackend::OpenRouter), "OPENROUTER_API_KEY");
        assert_eq!(tts_key_env(&TtsBackend::Cartesia), "CARTESIA_API_KEY");
        assert_eq!(tts_key_env(&TtsBackend::Deepgram), "DEEPGRAM_API_KEY");
        assert_eq!(tts_key_env(&TtsBackend::OpenAI), "OPENAI_API_KEY");
        assert!(tts_key_env(&TtsBackend::None).is_empty());
        assert!(tts_key_env(&TtsBackend::Wyoming).is_empty());
    }

    /// `configured_tts_backends` ordering: stored-key cloud first,
    /// then Wyoming when the user has a `[tts.wyoming]` block, then
    /// every remaining cloud backend (omitting `None`).
    #[test]
    fn configured_tts_ordering() {
        let mut secrets = crate::Secrets::default();
        secrets.insert("GROQ_API_KEY", "gsk-x");
        secrets.insert("OPENAI_API_KEY", "sk-x");
        let backends = configured_tts_backends(&secrets, &TtsBackend::None, true);
        // First, every cloud backend whose key is in secrets.toml.
        // Order is the canonical `all_tts_backends` order, which
        // places OpenAI before Groq.
        let cloud_present: Vec<_> =
            backends.iter().take_while(|b| !matches!(b, TtsBackend::Wyoming)).cloned().collect();
        assert_eq!(
            cloud_present,
            vec![TtsBackend::OpenAI, TtsBackend::Groq],
            "stored-key cloud backends must lead, in catalogue order"
        );
        // Wyoming next, because the caller asserted a [tts.wyoming]
        // block exists.
        assert!(
            backends.contains(&TtsBackend::Wyoming),
            "Wyoming must appear when has_wyoming_block = true"
        );
        let wyoming_pos = backends
            .iter()
            .position(|b| matches!(b, TtsBackend::Wyoming))
            .expect("wyoming must be present");
        // Every entry after wyoming is a cloud backend with no stored key
        // (or `None` if it happened to be active — but in this
        // test the active backend is `None`, which placed `None` after
        // Wyoming as a disable affordance).
        for b in &backends[wyoming_pos + 1..] {
            if matches!(b, TtsBackend::None | TtsBackend::Local) {
                // `None` is the disable affordance; `Local` is a keyless
                // on-device backend — neither requires an API key.
                continue;
            }
            assert!(tts_requires_key(b));
            assert!(!secrets.has_in_file(tts_key_env(b)));
        }
    }

    /// Wyoming is hidden when neither `[tts.wyoming]` is configured
    /// nor it's the active backend.
    #[test]
    fn configured_tts_hides_wyoming_without_block() {
        let secrets = crate::Secrets::default();
        let backends = configured_tts_backends(&secrets, &TtsBackend::None, false);
        assert!(!backends.contains(&TtsBackend::Wyoming));
        // Active is None, so None ends up in the list (only when active).
        assert!(backends.contains(&TtsBackend::None));
    }

    /// Active backend is always present even when its key is missing
    /// and it would otherwise be filtered out.
    #[test]
    fn configured_tts_always_includes_active() {
        let secrets = crate::Secrets::default();
        let backends = configured_tts_backends(&secrets, &TtsBackend::Cartesia, false);
        assert!(backends.contains(&TtsBackend::Cartesia));
    }
}
