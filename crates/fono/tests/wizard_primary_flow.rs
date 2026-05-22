// SPDX-License-Identifier: GPL-3.0-only
//! Phase D1 — integration tests for the wizard's primary-cloud-provider
//! collapse (issue #9) and the multi-provider TTS fallout (issue #11).
//!
//! These tests exercise the pure helpers
//! ([`fono::wizard::apply_primary_provider`] and friends) rather than
//! driving `dialoguer` end-to-end, so they run without a TTY.

use fono::wizard::{apply_primary_provider, apply_secondary_tts, seed_primary_secret};
use fono_core::config::{
    AssistantBackend, Config, PolishBackend, SttBackend, TtsBackend, TtsWyoming,
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
    assert_eq!(cfg.polish.backend, PolishBackend::OpenAI);
    assert!(cfg.polish.enabled);
    assert_eq!(cfg.assistant.backend, AssistantBackend::OpenAI);
    assert!(cfg.assistant.enabled);
    assert_eq!(cfg.tts.backend, TtsBackend::OpenAI);

    // Exactly one secrets entry — that's the issue-#9 acceptance shape.
    assert_eq!(secrets.keys.len(), 1);
    assert!(secrets.has_in_file("OPENAI_API_KEY"));

    // Every capability points at the same provider/key_env.
    for ref_env in [
        cfg.stt.cloud.as_ref().map(|c| c.api_key_ref.as_str()),
        cfg.polish.cloud.as_ref().map(|c| c.api_key_ref.as_str()),
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
    assert_eq!(cfg.polish.backend, PolishBackend::Groq);
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
    assert_eq!(cfg.polish.backend, PolishBackend::Anthropic);
    assert_eq!(cfg.assistant.backend, AssistantBackend::Anthropic);
    assert_eq!(cfg.tts.backend, TtsBackend::None);

    // User opts into Cartesia TTS as a secondary.
    assert!(seed_primary_secret(&mut secrets, cartesia, "cart-test"));
    apply_secondary_tts(&mut cfg, cartesia);
    assert_eq!(cfg.tts.backend, TtsBackend::Cartesia);
    let cloud = cfg.tts.cloud.as_ref().expect("cartesia tts.cloud set");
    assert_eq!(cloud.api_key_ref, "CARTESIA_API_KEY");
    assert_eq!(cloud.model, "sonic-3.5");

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
    assert_eq!(secrets.keys.get("OPENAI_API_KEY").map(String::as_str), Some("pre-existing-openai"));
    assert_eq!(
        secrets.keys.get("CARTESIA_API_KEY").map(String::as_str),
        Some("pre-existing-cartesia")
    );
    // Cartesia TTS was still applied — the catalogue walk is idempotent.
    assert_eq!(cfg.tts.backend, TtsBackend::Cartesia);
}

/// 5. Primary = Cartesia → STT + TTS land on Cartesia from the single
///    `CARTESIA_API_KEY`. Cartesia ships no polish / assistant
///    capability, so those slots stay at default (local /
///    disabled). Guards against the catalogue's STT model id
///    drifting back to the legacy `sonic-transcribe` literal (the
///    catalogue is the single source of truth for the wizard).
#[test]
fn primary_cartesia_covers_stt_and_tts_with_one_key() {
    let entry = find("cartesia").expect("cartesia catalogue entry");
    let mut cfg = Config::default();
    let mut secrets = Secrets::default();

    assert!(seed_primary_secret(&mut secrets, entry, "cart-test"));
    apply_primary_provider(&mut cfg, entry);

    // STT — Cartesia batch (`POST /stt`) only accepts the
    // `ink-whisper` family; `ink-2` is realtime-only and lives
    // behind a Phase 2 WebSocket slice. If this assertion fails,
    // `crates/fono-core/src/provider_catalog.rs` has drifted.
    assert_eq!(cfg.stt.backend, SttBackend::Cartesia);
    let stt_cloud = cfg.stt.cloud.as_ref().expect("cartesia stt.cloud set");
    assert_eq!(stt_cloud.api_key_ref, "CARTESIA_API_KEY");
    assert_eq!(stt_cloud.model, "ink-whisper");

    // TTS comes from the same catalogue entry — one secret covers both.
    assert_eq!(cfg.tts.backend, TtsBackend::Cartesia);
    let tts_cloud = cfg.tts.cloud.as_ref().expect("cartesia tts.cloud set");
    assert_eq!(tts_cloud.api_key_ref, "CARTESIA_API_KEY");

    // Cartesia ships neither polish nor assistant; defaults preserved.
    assert!(!cfg.polish.enabled);
    assert!(!cfg.assistant.enabled);

    assert_eq!(secrets.keys.len(), 1);
    assert!(secrets.has_in_file("CARTESIA_API_KEY"));
}

/// D2 — Customize-flow regression guard. A config carrying
/// `[stt].backend = "groq"`, `[polish].backend = "anthropic"`, and
/// `[tts].backend = "wyoming"` must survive (de)serialisation and
/// the catalogue helpers without anything flipping the TTS backend
/// to OpenAI/Groq/whatever. Phase B already added a seed
/// round-trip test in `wizard.rs`; this is the broader Customize-mix
/// version.
#[test]
fn customize_groq_stt_anthropic_llm_wyoming_tts_round_trip() {
    let mut cfg = Config::default();
    cfg.stt.backend = SttBackend::Groq;
    cfg.polish.backend = PolishBackend::Anthropic;
    cfg.polish.enabled = true;
    cfg.tts.backend = TtsBackend::Wyoming;
    cfg.tts.wyoming =
        Some(TtsWyoming { uri: "tcp://piper.lan:10200".into(), ..TtsWyoming::default() });

    // TOML round-trip: serialise → parse → compare. Catches any
    // accidental `#[serde(skip)]` on the new TTS variants.
    let toml = toml::to_string(&cfg).expect("serialise customise mix");
    let parsed: Config = toml::from_str(&toml).expect("parse back");
    assert_eq!(parsed.stt.backend, SttBackend::Groq);
    assert_eq!(parsed.polish.backend, PolishBackend::Anthropic);
    assert_eq!(parsed.tts.backend, TtsBackend::Wyoming);
    assert_eq!(parsed.tts.wyoming.as_ref().map(|w| w.uri.as_str()), Some("tcp://piper.lan:10200"));
}
