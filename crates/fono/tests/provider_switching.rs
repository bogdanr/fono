// SPDX-License-Identifier: GPL-3.0-only
//! Provider-switching plan tests S21+S23: backend swap helpers
//! preserve unrelated config fields, and the parsed CLI strings
//! round-trip through `fono use` semantics without touching disk.

use fono_core::config::{Config, LlmBackend, SttBackend};
use fono_core::providers::{
    cloud_pair, llm_backend_str, parse_llm_backend, parse_stt_backend, stt_backend_str,
};

use fono::cli::{set_active_llm, set_active_stt};

/// S21 — flipping the STT backend must preserve every unrelated field
/// (hotkeys, prompt, history retention, custom local model name).
#[test]
fn set_active_stt_preserves_unrelated_fields() {
    let mut cfg = Config::default();
    cfg.hotkeys.toggle = "Ctrl+Shift+J".into();
    cfg.llm.prompt.main = "BE TERSE".into();
    cfg.history.retention_days = 99;
    cfg.stt.local.model = "medium".into();

    set_active_stt(&mut cfg, SttBackend::OpenAI);

    assert_eq!(cfg.stt.backend, SttBackend::OpenAI);
    assert!(cfg.stt.cloud.is_none(), "stale cloud sub-block must clear");
    // Unrelated fields untouched.
    assert_eq!(cfg.hotkeys.toggle, "Ctrl+Shift+J");
    assert_eq!(cfg.llm.prompt.main, "BE TERSE");
    assert_eq!(cfg.history.retention_days, 99);
    assert_eq!(cfg.stt.local.model, "medium");
}

/// S21 — flipping the LLM backend follows the enabled/disabled rule.
#[test]
fn set_active_llm_disables_cleanup_for_none() {
    let mut cfg = Config::default();
    cfg.llm.enabled = true;
    cfg.llm.backend = LlmBackend::Cerebras;
    cfg.llm.prompt.main = "stay friendly".into();

    set_active_llm(&mut cfg, LlmBackend::None);
    assert_eq!(cfg.llm.backend, LlmBackend::None);
    assert!(!cfg.llm.enabled);
    // Prompt untouched — re-enabling later restores the user's voice.
    assert_eq!(cfg.llm.prompt.main, "stay friendly");

    set_active_llm(&mut cfg, LlmBackend::Anthropic);
    assert_eq!(cfg.llm.backend, LlmBackend::Anthropic);
    assert!(cfg.llm.enabled);
    assert!(cfg.llm.cloud.is_none());
}

/// S21 — Config TOML round-trip after swap must equal the in-memory
/// struct (catches accidental serde-skip or default elision).
#[test]
fn config_roundtrip_preserves_swapped_backend() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");

    let mut cfg = Config::default();
    cfg.hotkeys.toggle = "Ctrl+Alt+K".into();
    set_active_stt(&mut cfg, SttBackend::OpenAI);
    set_active_llm(&mut cfg, LlmBackend::Anthropic);
    cfg.save(&path).unwrap();

    let reloaded = Config::load(&path).unwrap();
    assert_eq!(reloaded.stt.backend, SttBackend::OpenAI);
    assert_eq!(reloaded.llm.backend, LlmBackend::Anthropic);
    assert!(reloaded.llm.enabled);
    assert_eq!(reloaded.hotkeys.toggle, "Ctrl+Alt+K");
}

/// S23 — provider-string parsers and printers form a bijection over
/// every supported backend, and `cloud_pair` returns valid pairs for
/// the documented presets.
#[test]
fn provider_strings_roundtrip_and_pair_correctly() {
    for s in [
        "local",
        "groq",
        "openai",
        "deepgram",
        "assemblyai",
        "cartesia",
    ] {
        let b = parse_stt_backend(s).expect("known stt provider");
        assert_eq!(stt_backend_str(&b), s);
    }
    for s in [
        "local",
        "none",
        "openai",
        "anthropic",
        "groq",
        "cerebras",
        "openrouter",
        "ollama",
    ] {
        let b = parse_llm_backend(s).expect("known llm provider");
        assert_eq!(llm_backend_str(&b), s);
    }

    let (s, l) = cloud_pair("cerebras").expect("cerebras pair");
    assert!(matches!(s, SttBackend::Groq));
    assert!(matches!(l, LlmBackend::Cerebras));

    assert!(parse_stt_backend("nope").is_none());
    assert!(parse_llm_backend("nope").is_none());
    assert!(cloud_pair("nope").is_none());
}
