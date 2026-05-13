// SPDX-License-Identifier: GPL-3.0-only
//! Phase D1 — integration tests for the wizard's primary-cloud-provider
//! collapse (issue #9) and the multi-provider TTS fallout (issue #11).
//!
//! These tests exercise the pure helpers
//! ([`fono::wizard::apply_primary_provider`] and friends) rather than
//! driving `dialoguer` end-to-end, so they run without a TTY.

use fono::wizard::{apply_primary_provider, apply_secondary_tts, seed_primary_secret};
use fono_core::config::{
    AssistantBackend, Config, LlmBackend, SttBackend, TtsBackend, TtsWyoming,
};
use fono_core::provider_catalog::find;
use fono_core::Secrets;

/// 1. Primary = OpenAI → STT / LLM / Assistant / TTS all OpenAI, one
///    secret entry covers everything.
#[test]
fn primary_openai_covers_full_stack_with_one_key() {
    let entry = find("openai").expect("openai catalogue entry");
    let mut cfg = Config::default();
    let mut secrets = Secrets::default();

    let inserted = seed_primary_secret(&mut secrets, entry, "sk-test-openai");
    assert!(inserted, "fresh secrets must accept the new key");
    apply_primary_provider(&mut cfg, entry);

    assert_eq!(cfg.stt.backend, SttBackend::OpenAI);
    assert_eq!(cfg.llm.backend, LlmBackend::OpenAI);
    assert!(cfg.llm.enabled);
    assert_eq!(cfg.assistant.backend, AssistantBackend::OpenAI);
    assert!(cfg.assistant.enabled);
    assert_eq!(cfg.tts.backend, TtsBackend::OpenAI);

    // Exactly one secrets entry — that's the issue-#9 acceptance shape.
    assert_eq!(secrets.keys.len(), 1);
    assert!(secrets.has_in_file("OPENAI_API_KEY"));

    // Every capability points at the same provider/key_env.
    for ref_env in [
        cfg.stt.cloud.as_ref().map(|c| c.api_key_ref.as_str()),
        cfg.llm.cloud.as_ref().map(|c| c.api_key_ref.as_str()),
        cfg.assistant.cloud.as_ref().map(|c| c.api_key_ref.as_str()),
        cfg.tts.cloud.as_ref().map(|c| c.api_key_ref.as_str()),
    ] {
        assert_eq!(ref_env, Some("OPENAI_API_KEY"));
    }
}

/// 2. Primary = Groq → STT / LLM / Assistant / TTS all Groq. This is
///    the issue #11 acceptance shape: a non-OpenAI primary that can
///    still drive the assistant end-to-end without a second key.
#[test]
fn primary_groq_covers_full_stack_with_one_key() {
    let entry = find("groq").expect("groq catalogue entry");
    let mut cfg = Config::default();
    let mut secrets = Secrets::default();

    assert!(seed_primary_secret(&mut secrets, entry, "gsk-test"));
    apply_primary_provider(&mut cfg, entry);

    assert_eq!(cfg.stt.backend, SttBackend::Groq);
    assert_eq!(cfg.llm.backend, LlmBackend::Groq);
    assert_eq!(cfg.assistant.backend, AssistantBackend::Groq);
    assert!(cfg.assistant.enabled);
    assert_eq!(cfg.tts.backend, TtsBackend::Groq);

    assert_eq!(secrets.keys.len(), 1);
    assert!(secrets.has_in_file("GROQ_API_KEY"));
}

/// 3. Primary = Anthropic → LLM + Assistant land on Anthropic; STT
///    stays at default (local) because Anthropic has no transcription
///    capability; TTS comes from a secondary Cartesia entry. Two
///    secrets entries total.
#[test]
fn primary_anthropic_secondary_cartesia_tts() {
    let anthropic = find("anthropic").expect("anthropic catalogue entry");
    let cartesia = find("cartesia").expect("cartesia catalogue entry");
    let mut cfg = Config::default();
    let mut secrets = Secrets::default();

    assert!(seed_primary_secret(&mut secrets, anthropic, "sk-ant-test"));
    apply_primary_provider(&mut cfg, anthropic);

    // Anthropic doesn't ship STT or TTS — those slots stay at default.
    assert_eq!(cfg.stt.backend, SttBackend::Local);
    assert_eq!(cfg.llm.backend, LlmBackend::Anthropic);
    assert_eq!(cfg.assistant.backend, AssistantBackend::Anthropic);
    assert_eq!(cfg.tts.backend, TtsBackend::None);

    // User opts into Cartesia TTS as a secondary.
    assert!(seed_primary_secret(&mut secrets, cartesia, "cart-test"));
    apply_secondary_tts(&mut cfg, cartesia);
    assert_eq!(cfg.tts.backend, TtsBackend::Cartesia);
    let cloud = cfg.tts.cloud.as_ref().expect("cartesia tts.cloud set");
    assert_eq!(cloud.api_key_ref, "CARTESIA_API_KEY");
    assert_eq!(cloud.model, "sonic-2");

    // Two distinct key entries — the assistant chat and the TTS each
    // contribute one.
    assert_eq!(secrets.keys.len(), 2);
    assert!(secrets.has_in_file("ANTHROPIC_API_KEY"));
    assert!(secrets.has_in_file("CARTESIA_API_KEY"));
}

/// 4. Re-run with secrets pre-populated → zero new inserts. Each
///    `seed_primary_secret` call reports `false` ("reusing") for an
///    existing key.
#[test]
fn rerun_with_existing_secrets_inserts_nothing() {
    let openai = find("openai").expect("openai catalogue entry");
    let cartesia = find("cartesia").expect("cartesia catalogue entry");

    let mut secrets = Secrets::default();
    secrets.insert("OPENAI_API_KEY", "pre-existing-openai");
    secrets.insert("CARTESIA_API_KEY", "pre-existing-cartesia");
    let before = secrets.keys.len();

    let mut cfg = Config::default();
    let openai_inserted = seed_primary_secret(&mut secrets, openai, "ignored");
    apply_primary_provider(&mut cfg, openai);
    let cartesia_inserted = seed_primary_secret(&mut secrets, cartesia, "ignored");
    apply_secondary_tts(&mut cfg, cartesia);

    assert!(!openai_inserted, "OpenAI key already present → reused");
    assert!(!cartesia_inserted, "Cartesia key already present → reused");
    assert_eq!(secrets.keys.len(), before, "no new inserts on re-run");
    // The pre-existing values are untouched (mock keys above).
    assert_eq!(
        secrets.keys.get("OPENAI_API_KEY").map(String::as_str),
        Some("pre-existing-openai")
    );
    assert_eq!(
        secrets.keys.get("CARTESIA_API_KEY").map(String::as_str),
        Some("pre-existing-cartesia")
    );
    // Cartesia TTS was still applied — the catalogue walk is idempotent.
    assert_eq!(cfg.tts.backend, TtsBackend::Cartesia);
}

/// D2 — Customize-flow regression guard. A config carrying
/// `[stt].backend = "groq"`, `[llm].backend = "anthropic"`, and
/// `[tts].backend = "wyoming"` must survive (de)serialisation and
/// the catalogue helpers without anything flipping the TTS backend
/// to OpenAI/Groq/whatever. Phase B already added a seed
/// round-trip test in `wizard.rs`; this is the broader Customize-mix
/// version.
#[test]
fn customize_groq_stt_anthropic_llm_wyoming_tts_round_trip() {
    let mut cfg = Config::default();
    cfg.stt.backend = SttBackend::Groq;
    cfg.llm.backend = LlmBackend::Anthropic;
    cfg.llm.enabled = true;
    cfg.tts.backend = TtsBackend::Wyoming;
    cfg.tts.wyoming = Some(TtsWyoming {
        uri: "tcp://piper.lan:10200".into(),
        ..TtsWyoming::default()
    });

    // TOML round-trip: serialise → parse → compare. Catches any
    // accidental `#[serde(skip)]` on the new TTS variants.
    let toml = toml::to_string(&cfg).expect("serialise customise mix");
    let parsed: Config = toml::from_str(&toml).expect("parse back");
    assert_eq!(parsed.stt.backend, SttBackend::Groq);
    assert_eq!(parsed.llm.backend, LlmBackend::Anthropic);
    assert_eq!(parsed.tts.backend, TtsBackend::Wyoming);
    assert_eq!(
        parsed.tts.wyoming.as_ref().map(|w| w.uri.as_str()),
        Some("tcp://piper.lan:10200")
    );
}
