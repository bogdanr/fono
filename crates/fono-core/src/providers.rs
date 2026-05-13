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

use crate::config::{AssistantBackend, LlmBackend, SttBackend, TtsBackend};

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

/// Canonical lower-case identifier for a TTS backend.
#[must_use]
pub const fn tts_backend_str(b: &TtsBackend) -> &'static str {
    match b {
        TtsBackend::None => "none",
        TtsBackend::Wyoming => "wyoming",
        TtsBackend::Piper => "piper",
        TtsBackend::OpenAI => "openai",
        TtsBackend::Groq => "groq",
        TtsBackend::OpenRouter => "openrouter",
        TtsBackend::Cartesia => "cartesia",
        TtsBackend::Deepgram => "deepgram",
    }
}

#[must_use]
pub fn parse_tts_backend(s: &str) -> Option<TtsBackend> {
    match s.to_ascii_lowercase().as_str() {
        "none" | "off" | "skip" => Some(TtsBackend::None),
        "wyoming" => Some(TtsBackend::Wyoming),
        "piper" => Some(TtsBackend::Piper),
        "openai" => Some(TtsBackend::OpenAI),
        "groq" => Some(TtsBackend::Groq),
        "openrouter" => Some(TtsBackend::OpenRouter),
        "cartesia" => Some(TtsBackend::Cartesia),
        "deepgram" => Some(TtsBackend::Deepgram),
        _ => None,
    }
}

/// Canonical environment-variable name for the API key of a cloud
/// TTS backend. Returned even for `None`/`Wyoming`/`Piper` (where it's
/// unused) for branch-free callers; check [`tts_requires_key`] first.
#[must_use]
pub const fn tts_key_env(b: &TtsBackend) -> &'static str {
    match b {
        TtsBackend::None | TtsBackend::Wyoming | TtsBackend::Piper => "",
        TtsBackend::OpenAI => "OPENAI_API_KEY",
        TtsBackend::Groq => "GROQ_API_KEY",
        TtsBackend::OpenRouter => "OPENROUTER_API_KEY",
        TtsBackend::Cartesia => "CARTESIA_API_KEY",
        TtsBackend::Deepgram => "DEEPGRAM_API_KEY",
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
    )
}

/// Canonical lower-case identifier for an assistant chat backend.
#[must_use]
pub const fn assistant_backend_str(b: &AssistantBackend) -> &'static str {
    match b {
        AssistantBackend::None => "none",
        AssistantBackend::Local => "local",
        AssistantBackend::OpenAI => "openai",
        AssistantBackend::Anthropic => "anthropic",
        AssistantBackend::Gemini => "gemini",
        AssistantBackend::Groq => "groq",
        AssistantBackend::Cerebras => "cerebras",
        AssistantBackend::OpenRouter => "openrouter",
        AssistantBackend::Ollama => "ollama",
    }
}

#[must_use]
pub fn parse_assistant_backend(s: &str) -> Option<AssistantBackend> {
    match s.to_ascii_lowercase().as_str() {
        "none" | "off" | "skip" => Some(AssistantBackend::None),
        "local" => Some(AssistantBackend::Local),
        "openai" => Some(AssistantBackend::OpenAI),
        "anthropic" => Some(AssistantBackend::Anthropic),
        "gemini" => Some(AssistantBackend::Gemini),
        "groq" => Some(AssistantBackend::Groq),
        "cerebras" => Some(AssistantBackend::Cerebras),
        "openrouter" => Some(AssistantBackend::OpenRouter),
        "ollama" => Some(AssistantBackend::Ollama),
        _ => None,
    }
}

/// Canonical environment-variable name for the API key of a cloud
/// assistant backend. Reuses the same env vars as the LLM cleanup
/// path (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc.), so a single
/// stored key serves both consumers without duplicate prompts in
/// the wizard.
#[must_use]
pub const fn assistant_key_env(b: &AssistantBackend) -> &'static str {
    match b {
        AssistantBackend::None | AssistantBackend::Local | AssistantBackend::Ollama => "",
        AssistantBackend::OpenAI => "OPENAI_API_KEY",
        AssistantBackend::Anthropic => "ANTHROPIC_API_KEY",
        AssistantBackend::Gemini => "GEMINI_API_KEY",
        AssistantBackend::Groq => "GROQ_API_KEY",
        AssistantBackend::Cerebras => "CEREBRAS_API_KEY",
        AssistantBackend::OpenRouter => "OPENROUTER_API_KEY",
    }
}

#[must_use]
pub const fn assistant_requires_key(b: &AssistantBackend) -> bool {
    !matches!(
        b,
        AssistantBackend::None | AssistantBackend::Local | AssistantBackend::Ollama
    )
}

/// Paired cloud preset for `fono use cloud <name>`. Returns `(stt, llm)`
/// for the preset, or `None` if the name isn't a known pair.
///
/// Phase A reroute (issues #9/#10/#11): the resolution now consults
/// [`crate::provider_catalog::CLOUD_PROVIDERS`] as the source of truth
/// for which provider id offers which capabilities, and only falls
/// back to the legacy hard-coded pairings for catalogue entries that
/// lack a same-name STT capability (Cerebras, Anthropic, OpenRouter,
/// AssemblyAI). The fallbacks preserve the historical behaviour
/// exactly — Groq's whisper-turbo is the de-facto fast cloud STT
/// today, so providers without a native STT product pair with Groq;
/// AssemblyAI pairs with Cerebras for cleanup since AssemblyAI offers
/// no LLM.
#[must_use]
pub fn cloud_pair(name: &str) -> Option<(SttBackend, LlmBackend)> {
    let id = name.to_ascii_lowercase();
    let entry = crate::provider_catalog::find(&id)?;
    // Resolve STT: prefer the entry's own STT capability, otherwise
    // fall back to Groq's whisper-turbo (legacy behaviour).
    let stt = if entry.stt.is_some() {
        parse_stt_backend(entry.id)?
    } else {
        SttBackend::Groq
    };
    // Resolve LLM: prefer the entry's own LLM capability, otherwise
    // fall back to Cerebras for cleanup (legacy behaviour for STT-only
    // providers like Deepgram and AssemblyAI).
    let llm = if entry.llm.is_some() {
        parse_llm_backend(entry.id)?
    } else {
        LlmBackend::Cerebras
    };
    Some((stt, llm))
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

#[must_use]
pub fn all_assistant_backends() -> [AssistantBackend; 9] {
    [
        AssistantBackend::None,
        AssistantBackend::Local,
        AssistantBackend::Cerebras,
        AssistantBackend::Groq,
        AssistantBackend::OpenAI,
        AssistantBackend::Anthropic,
        AssistantBackend::OpenRouter,
        AssistantBackend::Ollama,
        AssistantBackend::Gemini,
    ]
}

#[must_use]
pub fn all_tts_backends() -> [TtsBackend; 8] {
    [
        TtsBackend::None,
        TtsBackend::Wyoming,
        TtsBackend::Piper,
        TtsBackend::OpenAI,
        TtsBackend::Groq,
        TtsBackend::OpenRouter,
        TtsBackend::Cartesia,
        TtsBackend::Deepgram,
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
/// `None`, `Piper` are intentionally excluded — `None` is not a real
/// switchable option and `Piper` is a v1 stub that returns an error
/// from the factory.
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
        if matches!(b, TtsBackend::None | TtsBackend::Piper) {
            // None is not a real entry; Piper is a v1 stub. Both are
            // excluded from the menu unless they are the active one.
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

    /// Regression test for the Phase A `cloud_pair` reroute (issues
    /// #9/#10/#11): every pair returned by the old hard-coded match
    /// arm must come back unchanged from the catalogue-driven
    /// implementation. If this fails after editing the catalogue,
    /// the change is observable to existing users of
    /// `fono use cloud <id>` and needs a migration note.
    #[test]
    fn cloud_pair_catalogue_matches_legacy_behaviour() {
        let legacy: &[(&str, SttBackend, LlmBackend)] = &[
            ("groq", SttBackend::Groq, LlmBackend::Groq),
            // Cerebras has no STT product — pair with Groq's
            // whisper-turbo, which is the de-facto fast cloud STT today.
            ("cerebras", SttBackend::Groq, LlmBackend::Cerebras),
            ("openai", SttBackend::OpenAI, LlmBackend::OpenAI),
            ("anthropic", SttBackend::Groq, LlmBackend::Anthropic),
            ("openrouter", SttBackend::Groq, LlmBackend::OpenRouter),
            ("deepgram", SttBackend::Deepgram, LlmBackend::Cerebras),
            ("assemblyai", SttBackend::AssemblyAI, LlmBackend::Cerebras),
        ];
        for (id, want_stt, want_llm) in legacy {
            let (stt, llm) = cloud_pair(id)
                .unwrap_or_else(|| panic!("cloud_pair({id}) returned None after reroute"));
            assert_eq!(
                std::mem::discriminant(&stt),
                std::mem::discriminant(want_stt),
                "cloud_pair({id}).stt drifted"
            );
            assert_eq!(
                std::mem::discriminant(&llm),
                std::mem::discriminant(want_llm),
                "cloud_pair({id}).llm drifted"
            );
        }
        // Unknown ids still return None.
        assert!(cloud_pair("definitely-not-a-provider").is_none());
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
        assert_eq!(
            parse_tts_backend("openrouter"),
            Some(TtsBackend::OpenRouter)
        );
        assert_eq!(parse_tts_backend("cartesia"), Some(TtsBackend::Cartesia));
        assert_eq!(parse_tts_backend("deepgram"), Some(TtsBackend::Deepgram));
    }

    /// Phase F: every new cloud TTS backend reports the canonical
    /// env-var name. Mirrors `key_env_matches_provider` for STT/LLM.
    #[test]
    fn tts_key_env_matches_provider() {
        assert_eq!(tts_key_env(&TtsBackend::Groq), "GROQ_API_KEY");
        assert_eq!(
            tts_key_env(&TtsBackend::OpenRouter),
            "OPENROUTER_API_KEY"
        );
        assert_eq!(tts_key_env(&TtsBackend::Cartesia), "CARTESIA_API_KEY");
        assert_eq!(tts_key_env(&TtsBackend::Deepgram), "DEEPGRAM_API_KEY");
        assert_eq!(tts_key_env(&TtsBackend::OpenAI), "OPENAI_API_KEY");
        assert!(tts_key_env(&TtsBackend::None).is_empty());
        assert!(tts_key_env(&TtsBackend::Wyoming).is_empty());
        assert!(tts_key_env(&TtsBackend::Piper).is_empty());
    }

    /// `configured_tts_backends` ordering: stored-key cloud first,
    /// then Wyoming when the user has a `[tts.wyoming]` block, then
    /// every remaining cloud backend (omitting `None`/`Piper`).
    #[test]
    fn configured_tts_ordering() {
        let mut secrets = crate::Secrets::default();
        secrets.insert("GROQ_API_KEY", "gsk-x");
        secrets.insert("OPENAI_API_KEY", "sk-x");
        let backends = configured_tts_backends(&secrets, &TtsBackend::None, true);
        // First, every cloud backend whose key is in secrets.toml.
        // Order is the canonical `all_tts_backends` order, which
        // places OpenAI before Groq.
        let cloud_present: Vec<_> = backends
            .iter()
            .take_while(|b| !matches!(b, TtsBackend::Wyoming))
            .cloned()
            .collect();
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
        // (or `None`/`Piper` if they happened to be active — but in this
        // test the active backend is `None`, which placed `None` after
        // Wyoming as a disable affordance).
        for b in &backends[wyoming_pos + 1..] {
            if matches!(b, TtsBackend::None | TtsBackend::Piper) {
                continue;
            }
            assert!(tts_requires_key(b));
            assert!(!secrets.has_in_file(tts_key_env(b)));
        }
        // Piper is always excluded (active backend in this test is None).
        assert!(!backends.contains(&TtsBackend::Piper));
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
        let backends =
            configured_tts_backends(&secrets, &TtsBackend::Cartesia, false);
        assert!(backends.contains(&TtsBackend::Cartesia));
    }
}
