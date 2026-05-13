// SPDX-License-Identifier: GPL-3.0-only
//! Capability catalogue for cloud providers.
//!
//! This is the single source of truth for which cloud providers cover
//! which capabilities (STT, LLM cleanup, assistant chat, TTS), plus
//! future-facing assistant extras (multimodal model id, web-search
//! support) and the TTS endpoint shape. The wizard, `fono use cloud`,
//! and `fono doctor` consume this catalogue.
//!
//! Phase A (issues #9/#10/#11, plan
//! `plans/2026-05-13-2026-05-13-wizard-catalogue-multimodal-and-multi-tts-issues-9-11-v2.md`)
//! lands the data structures and reroutes `cloud_pair` through the
//! catalogue. The wizard rewrite, TTS client wiring, and doctor
//! upgrades land in later phases.
//!
//! Model strings carry inline references to the matching `default_*`
//! function in `fono-stt::defaults` / `fono-llm::defaults` /
//! `fono-tts::defaults` / `fono-assistant::factory::default_cloud_model`.
//! Because `fono-core` is upstream of those crates (they depend on it,
//! not the other way around), the literal `&'static str` constants
//! cannot be re-exported back into `fono-core` without inverting the
//! dependency direction. Keeping them as literal constants here, with
//! the consumer-crate cross-reference as a comment, was the pragmatic
//! Phase-A trade-off; a later refactor can move the model constants
//! into this module and have the consumer crates `pub use` them.
//!
//! See the plan's "deviations" section for the corresponding entry.
//!
//! [`CLOUD_PROVIDERS`] enumerates every cloud provider currently
//! referenced by `crates/fono-core/src/providers.rs`. Maintainers must
//! keep the two in lockstep — the unit tests in this module fail if a
//! cloud `*Backend` variant ever lacks a catalogue entry.

use crate::config::{LlmBackend, SttBackend};
use crate::providers::{parse_llm_backend, parse_stt_backend};

/// Defaults for a provider's speech-to-text capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SttDefaults {
    /// Default STT model identifier. Mirrors
    /// `fono_stt::defaults::default_cloud_model(provider)` for the
    /// catalogue id. Drift between the two is caught at runtime by
    /// the doctor; eventually the consumer crate should `pub use`
    /// the constant defined here.
    pub model: &'static str,
}

/// Defaults for a provider's LLM cleanup capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmDefaults {
    /// Default cleanup model. Mirrors
    /// `fono_llm::defaults::default_cloud_model(provider)`.
    pub model: &'static str,
}

/// Defaults for a provider's voice-assistant capability.
#[derive(Debug, Clone, Copy)]
pub struct AssistantDefaults {
    /// Default chat model. Mirrors the per-provider default in
    /// `fono_assistant::factory::default_cloud_model`.
    pub text_model: &'static str,
    /// Multimodal sibling model where the provider exposes one — used
    /// when the assistant input includes screenshots/images. `None`
    /// means the provider has no multimodal endpoint Fono is willing
    /// to default to.
    pub multimodal_model: Option<&'static str>,
    /// Native web-search support advertised by the provider.
    pub web_search: WebSearchSupport,
    /// Capability badges to render in the wizard's provider picker.
    pub badges: &'static [Badge],
}

/// How the provider exposes a web-search tool to the assistant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchSupport {
    /// No native web search; Fono would need an external tool.
    None,
    /// Provider exposes a named native tool — e.g. OpenAI's
    /// `web_search_preview`, Anthropic's `web_search_20250305`, or
    /// Gemini's `google_search` grounding tool.
    NativeTool(&'static str),
    /// Provider's models always search the web (no toggle).
    Always,
}

/// Capability badges rendered next to a provider in the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    /// Provider offers speech-to-text.
    Stt,
    /// Provider offers LLM cleanup.
    Llm,
    /// Provider offers voice-assistant chat.
    Assistant,
    /// Provider offers text-to-speech.
    Tts,
    /// Provider exposes a multimodal/vision-capable model.
    Vision,
    /// Provider offers native web search.
    Search,
    /// Provider's models advertise extended reasoning.
    Reasoning,
    /// Provider is positioned as a low-latency / fast tier.
    Fast,
}

/// Defaults for a provider's text-to-speech capability.
#[derive(Debug, Clone, Copy)]
pub struct TtsDefaults {
    /// Default TTS model identifier.
    pub model: &'static str,
    /// Default voice id / name.
    pub default_voice: &'static str,
    /// Endpoint shape (which client to instantiate, plus base URL).
    pub endpoint: TtsEndpoint,
    /// Whether the doctor should runtime-probe the provider's TTS
    /// endpoint. **False for every catalogue entry in Phase A** — no
    /// runtime probes anywhere yet; this flag is reserved for later
    /// phases that may want to verify endpoint availability.
    pub runtime_probe: bool,
}

/// Wire-level shape of a provider's TTS API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsEndpoint {
    /// OpenAI-compatible `POST /audio/speech` endpoint at `base_url`.
    OpenAiCompat {
        /// Base URL up to and including `/v1` (the client appends
        /// `/audio/speech`).
        base_url: &'static str,
    },
    /// Cartesia's bespoke `POST /v1/tts/bytes` endpoint.
    Cartesia,
    /// Deepgram's `POST /v1/speak` endpoint.
    Deepgram,
}

/// One cloud provider entry in the catalogue.
#[derive(Debug, Clone, Copy)]
pub struct CloudProvider {
    /// Lower-case canonical id, matching the corresponding
    /// `*_backend_str` value in [`crate::providers`].
    pub id: &'static str,
    /// Human-readable display name for the wizard.
    pub display_name: &'static str,
    /// Short one-liner shown in the wizard's provider picker.
    pub tagline: &'static str,
    /// URL where the user obtains an API key.
    pub console_url: &'static str,
    /// Canonical API-key env var (e.g. `OPENAI_API_KEY`). Matches
    /// `*_key_env()` for the capabilities this entry claims.
    pub key_env: &'static str,
    /// STT capability if the provider offers transcription.
    pub stt: Option<SttDefaults>,
    /// LLM cleanup capability.
    pub llm: Option<LlmDefaults>,
    /// Voice-assistant chat capability.
    pub assistant: Option<AssistantDefaults>,
    /// TTS capability.
    pub tts: Option<TtsDefaults>,
}

/// Canonical capability catalogue. Order matches the wizard's
/// "recommended first" presentation.
pub const CLOUD_PROVIDERS: &[CloudProvider] = &[
    // ----- OpenAI ------------------------------------------------------
    CloudProvider {
        id: "openai",
        display_name: "OpenAI",
        tagline: "Flagship multimodal models with native web search and TTS.",
        console_url: "https://platform.openai.com/api-keys",
        key_env: "OPENAI_API_KEY",
        stt: Some(SttDefaults {
            // Mirrors fono_stt::defaults::default_cloud_model("openai").
            model: "whisper-1",
        }),
        llm: Some(LlmDefaults {
            // Mirrors fono_llm::defaults::default_cloud_model("openai").
            model: "gpt-5.4-nano",
        }),
        assistant: Some(AssistantDefaults {
            // Mirrors fono_assistant::factory::default_cloud_model("openai").
            text_model: "gpt-5.4-mini",
            // GPT-5.4 family is multimodal; reuse the assistant default.
            multimodal_model: Some("gpt-5.4-mini"),
            web_search: WebSearchSupport::NativeTool("web_search_preview"),
            badges: &[
                Badge::Stt,
                Badge::Llm,
                Badge::Assistant,
                Badge::Tts,
                Badge::Vision,
                Badge::Search,
            ],
        }),
        tts: Some(TtsDefaults {
            // Mirrors fono_tts::defaults::default_cloud_model("openai").
            model: "tts-1",
            default_voice: "alloy",
            endpoint: TtsEndpoint::OpenAiCompat {
                base_url: "https://api.openai.com/v1",
            },
            runtime_probe: false,
        }),
    },
    // ----- Groq --------------------------------------------------------
    CloudProvider {
        id: "groq",
        display_name: "Groq",
        tagline: "Lowest-latency cloud STT and OpenAI-compat LLM hosting.",
        console_url: "https://console.groq.com/keys",
        key_env: "GROQ_API_KEY",
        stt: Some(SttDefaults {
            // Mirrors fono_stt::defaults::default_cloud_model("groq").
            model: "whisper-large-v3-turbo",
        }),
        llm: Some(LlmDefaults {
            // Mirrors fono_llm::defaults::default_cloud_model("groq").
            model: "openai/gpt-oss-20b",
        }),
        assistant: Some(AssistantDefaults {
            // Mirrors fono_assistant::factory::default_cloud_model("groq").
            text_model: "openai/gpt-oss-120b",
            // Llama-4 Maverick is Meta's vision-capable open-weight
            // model hosted on Groq. Llama-family licence is not OSI-
            // approved, so it ships opt-in only (the user must pick a
            // multimodal task before the wizard exposes it).
            multimodal_model: Some("llama-4-maverick-17b-128e-instruct"),
            web_search: WebSearchSupport::None,
            badges: &[Badge::Stt, Badge::Llm, Badge::Assistant, Badge::Tts, Badge::Fast],
        }),
        tts: Some(TtsDefaults {
            // PlayAI on Groq — endpoint declared here for Phase A;
            // runtime client wiring lands in Phase F1/F2.
            model: "playai-tts",
            default_voice: "Fritz-PlayAI",
            endpoint: TtsEndpoint::OpenAiCompat {
                base_url: "https://api.groq.com/openai/v1",
            },
            runtime_probe: false,
        }),
    },
    // ----- Anthropic ---------------------------------------------------
    CloudProvider {
        id: "anthropic",
        display_name: "Anthropic",
        tagline: "Claude family with vision and native web-search tool.",
        console_url: "https://console.anthropic.com/settings/keys",
        key_env: "ANTHROPIC_API_KEY",
        stt: None,
        llm: Some(LlmDefaults {
            // Mirrors fono_llm::defaults::default_cloud_model("anthropic").
            model: "claude-haiku-4-5-20251001",
        }),
        assistant: Some(AssistantDefaults {
            // Mirrors fono_assistant::factory::default_cloud_model("anthropic").
            text_model: "claude-haiku-4-5-20251001",
            // Claude Haiku 4.5 is multimodal (image input supported).
            multimodal_model: Some("claude-haiku-4-5-20251001"),
            web_search: WebSearchSupport::NativeTool("web_search_20250305"),
            badges: &[Badge::Llm, Badge::Assistant, Badge::Vision, Badge::Search],
        }),
        tts: None,
    },
    // ----- Cerebras ----------------------------------------------------
    CloudProvider {
        id: "cerebras",
        display_name: "Cerebras",
        tagline: "Wafer-scale inference for the lowest-latency LLM cleanup.",
        console_url: "https://cloud.cerebras.ai/platform/keys",
        key_env: "CEREBRAS_API_KEY",
        stt: None,
        llm: Some(LlmDefaults {
            // Mirrors fono_llm::defaults::default_cloud_model("cerebras").
            model: "llama3.1-8b",
        }),
        assistant: Some(AssistantDefaults {
            // Mirrors fono_assistant::factory::default_cloud_model("cerebras").
            text_model: "qwen-3-235b-a22b-instruct-2507",
            multimodal_model: None,
            web_search: WebSearchSupport::None,
            badges: &[Badge::Llm, Badge::Assistant, Badge::Fast],
        }),
        tts: None,
    },
    // ----- Gemini ------------------------------------------------------
    CloudProvider {
        id: "gemini",
        display_name: "Google Gemini",
        tagline: "Gemini Flash with native Google Search grounding.",
        console_url: "https://aistudio.google.com/app/apikey",
        key_env: "GEMINI_API_KEY",
        stt: None,
        llm: Some(LlmDefaults {
            // Mirrors fono_llm::defaults::default_cloud_model("gemini").
            model: "gemini-1.5-flash",
        }),
        assistant: Some(AssistantDefaults {
            text_model: "gemini-1.5-flash",
            multimodal_model: Some("gemini-1.5-flash"),
            web_search: WebSearchSupport::NativeTool("google_search"),
            badges: &[Badge::Llm, Badge::Assistant, Badge::Vision, Badge::Search],
        }),
        tts: None,
    },
    // ----- OpenRouter --------------------------------------------------
    CloudProvider {
        id: "openrouter",
        display_name: "OpenRouter",
        tagline: "Unified gateway across hundreds of model providers.",
        console_url: "https://openrouter.ai/keys",
        key_env: "OPENROUTER_API_KEY",
        stt: None,
        llm: Some(LlmDefaults {
            // Mirrors fono_llm::defaults::default_cloud_model("openrouter").
            model: "openai/gpt-5.4-nano",
        }),
        assistant: Some(AssistantDefaults {
            // Mirrors fono_assistant::factory::default_cloud_model("openrouter").
            text_model: "anthropic/claude-haiku-4.5",
            // Multimodal is route-dependent on OpenRouter; leave None
            // until the wizard surfaces explicit per-route choices.
            multimodal_model: None,
            // Web-search support is route-dependent; default to None
            // and let later phases enable per-route overrides.
            web_search: WebSearchSupport::None,
            badges: &[Badge::Llm, Badge::Assistant, Badge::Tts],
        }),
        tts: Some(TtsDefaults {
            // Kokoro on OpenRouter — declared here for Phase A,
            // runtime client wiring lands in Phase F.
            model: "hexgrad/kokoro-82m",
            default_voice: "af_heart",
            endpoint: TtsEndpoint::OpenAiCompat {
                base_url: "https://openrouter.ai/api/v1",
            },
            runtime_probe: false,
        }),
    },
    // ----- Deepgram ----------------------------------------------------
    CloudProvider {
        id: "deepgram",
        display_name: "Deepgram",
        tagline: "Real-time Nova STT and Aura voice TTS.",
        console_url: "https://console.deepgram.com/",
        key_env: "DEEPGRAM_API_KEY",
        stt: Some(SttDefaults {
            // Mirrors fono_stt::defaults::default_cloud_model("deepgram").
            model: "nova-2",
        }),
        llm: None,
        assistant: None,
        tts: Some(TtsDefaults {
            model: "aura-2-thalia-en",
            // Deepgram TTS selects a voice via the `model` parameter
            // (e.g. `aura-2-thalia-en` *is* the voice). Keep an empty
            // default_voice rather than duplicating the model id.
            default_voice: "",
            endpoint: TtsEndpoint::Deepgram,
            runtime_probe: false,
        }),
    },
    // ----- AssemblyAI --------------------------------------------------
    CloudProvider {
        id: "assemblyai",
        display_name: "AssemblyAI",
        tagline: "High-accuracy STT with rich post-processing options.",
        console_url: "https://www.assemblyai.com/app/account",
        key_env: "ASSEMBLYAI_API_KEY",
        stt: Some(SttDefaults {
            // Mirrors fono_stt::defaults::default_cloud_model("assemblyai").
            model: "best",
        }),
        llm: None,
        assistant: None,
        tts: None,
    },
    // ----- Cartesia ----------------------------------------------------
    CloudProvider {
        id: "cartesia",
        display_name: "Cartesia",
        tagline: "Sonic ultra-low-latency speech models (STT + TTS).",
        console_url: "https://play.cartesia.ai/keys",
        key_env: "CARTESIA_API_KEY",
        stt: Some(SttDefaults {
            // Mirrors fono_stt::defaults::default_cloud_model("cartesia").
            model: "sonic-transcribe",
        }),
        llm: None,
        assistant: None,
        tts: Some(TtsDefaults {
            model: "sonic-2",
            // Cartesia's "Sonic English Female" preset voice id.
            default_voice: "a0e99841-438c-4a64-b679-ae501e7d6091",
            endpoint: TtsEndpoint::Cartesia,
            runtime_probe: false,
        }),
    },
    // ----- Azure (STT-only stub) --------------------------------------
    // Azure / Speechmatics / Google / Nemotron exist as `SttBackend`
    // variants but are not yet wired in `fono-stt::factory`. They are
    // included here so the "no orphans" unit test (test 4) sees every
    // cloud variant in at least one catalogue entry. Display strings
    // are placeholders until the providers are first-classed.
    CloudProvider {
        id: "azure",
        display_name: "Azure Speech",
        tagline: "Azure Cognitive Services Speech-to-Text (planned).",
        console_url: "https://portal.azure.com/",
        key_env: "AZURE_API_KEY",
        stt: Some(SttDefaults {
            // Mirrors fono_stt::defaults::default_cloud_model("azure").
            model: "whisper",
        }),
        llm: None,
        assistant: None,
        tts: None,
    },
    // ----- Speechmatics (STT-only stub) -------------------------------
    CloudProvider {
        id: "speechmatics",
        display_name: "Speechmatics",
        tagline: "Speechmatics real-time and batch STT (planned).",
        console_url: "https://portal.speechmatics.com/",
        key_env: "SPEECHMATICS_API_KEY",
        stt: Some(SttDefaults {
            // No specific default in fono_stt::defaults yet; uses the
            // generic Whisper fallback.
            model: "whisper-large-v3",
        }),
        llm: None,
        assistant: None,
        tts: None,
    },
    // ----- Google (STT-only stub) -------------------------------------
    CloudProvider {
        id: "google",
        display_name: "Google Cloud Speech",
        tagline: "Google Cloud Speech-to-Text (planned).",
        console_url: "https://console.cloud.google.com/",
        key_env: "GOOGLE_API_KEY",
        stt: Some(SttDefaults {
            // Mirrors fono_stt::defaults::default_cloud_model("google").
            model: "default",
        }),
        llm: None,
        assistant: None,
        tts: None,
    },
    // ----- Nemotron (STT-only stub) -----------------------------------
    CloudProvider {
        id: "nemotron",
        display_name: "NVIDIA Nemotron",
        tagline: "NVIDIA Nemotron speech models (planned).",
        console_url: "https://build.nvidia.com/",
        key_env: "NEMOTRON_API_KEY",
        stt: Some(SttDefaults {
            // No specific default in fono_stt::defaults yet; uses the
            // generic Whisper fallback.
            model: "whisper-large-v3",
        }),
        llm: None,
        assistant: None,
        tts: None,
    },
];

/// Look up a catalogue entry by its canonical id. Returns `None`
/// for unknown ids; callers surface a clear error.
#[must_use]
pub fn find(id: &str) -> Option<&'static CloudProvider> {
    CLOUD_PROVIDERS.iter().find(|p| p.id == id)
}

/// Construct a `(stt, llm)` pair from a catalogue entry, mapping the
/// entry id back through [`parse_stt_backend`] /
/// [`parse_llm_backend`]. Returns `None` if the entry lacks either
/// capability or the id doesn't round-trip through the parsers.
#[must_use]
pub fn cloud_pair_from_catalog(id: &str) -> Option<(SttBackend, LlmBackend)> {
    let entry = find(id)?;
    if entry.stt.is_none() || entry.llm.is_none() {
        return None;
    }
    let stt_backend = parse_stt_backend(entry.id)?;
    let llm_backend = parse_llm_backend(entry.id)?;
    Some((stt_backend, llm_backend))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AssistantBackend, TtsBackend};
    use crate::providers::{
        assistant_backend_str, assistant_key_env, llm_backend_str, llm_key_env,
        parse_assistant_backend, parse_tts_backend, stt_backend_str, stt_key_env, tts_backend_str,
        tts_key_env,
    };

    /// Test 1 — key_env on every entry matches the canonical
    /// `*_key_env` of every capability variant it claims.
    #[test]
    fn key_env_matches_provider_for_every_capability() {
        for p in CLOUD_PROVIDERS {
            if p.stt.is_some() {
                if let Some(b) = parse_stt_backend(p.id) {
                    let expected = stt_key_env(&b);
                    if !expected.is_empty() {
                        assert_eq!(
                            p.key_env, expected,
                            "STT key_env mismatch for {} (entry={}, providers={})",
                            p.id, p.key_env, expected
                        );
                    }
                }
            }
            if p.llm.is_some() {
                if let Some(b) = parse_llm_backend(p.id) {
                    let expected = llm_key_env(&b);
                    if !expected.is_empty() {
                        assert_eq!(p.key_env, expected, "LLM key_env mismatch for {}", p.id);
                    }
                }
            }
            if p.assistant.is_some() {
                if let Some(b) = parse_assistant_backend(p.id) {
                    let expected = assistant_key_env(&b);
                    if !expected.is_empty() {
                        assert_eq!(
                            p.key_env, expected,
                            "Assistant key_env mismatch for {}",
                            p.id
                        );
                    }
                }
            }
            if p.tts.is_some() {
                if let Some(b) = parse_tts_backend(p.id) {
                    let expected = tts_key_env(&b);
                    if !expected.is_empty() {
                        assert_eq!(p.key_env, expected, "TTS key_env mismatch for {}", p.id);
                    }
                }
            }
        }
    }

    /// Test 2 — every backend variant claimed by an entry parses
    /// back through the matching `parse_*_backend`. For capabilities
    /// where the corresponding `*Backend` enum variant doesn't yet
    /// exist (e.g. Groq/OpenRouter/Deepgram/Cartesia TTS in Phase A),
    /// we skip silently; later phases that add the variant will start
    /// exercising the check automatically.
    #[test]
    fn claimed_backends_roundtrip() {
        for p in CLOUD_PROVIDERS {
            if p.stt.is_some() {
                assert!(
                    parse_stt_backend(p.id).is_some(),
                    "{} claims STT but parse_stt_backend rejects its id",
                    p.id
                );
            }
            if p.llm.is_some() {
                assert!(
                    parse_llm_backend(p.id).is_some(),
                    "{} claims LLM but parse_llm_backend rejects its id",
                    p.id
                );
            }
            if p.assistant.is_some() {
                assert!(
                    parse_assistant_backend(p.id).is_some(),
                    "{} claims Assistant but parse_assistant_backend rejects its id",
                    p.id
                );
            }
            // TTS: only enforce roundtrip when the enum variant
            // already exists. Phase A intentionally pre-declares TTS
            // metadata for providers whose `TtsBackend` variant isn't
            // wired yet.
            if p.tts.is_some() {
                if let Some(b) = parse_tts_backend(p.id) {
                    assert_eq!(tts_backend_str(&b), p.id);
                }
            }
        }
    }

    /// Test 3 — every entry's id matches the `*_backend_str` returned
    /// for the backend it represents.
    #[test]
    fn id_matches_backend_str() {
        for p in CLOUD_PROVIDERS {
            if let Some(b) = parse_stt_backend(p.id) {
                assert_eq!(stt_backend_str(&b), p.id);
            }
            if let Some(b) = parse_llm_backend(p.id) {
                assert_eq!(llm_backend_str(&b), p.id);
            }
            if let Some(b) = parse_assistant_backend(p.id) {
                assert_eq!(assistant_backend_str(&b), p.id);
            }
            if let Some(b) = parse_tts_backend(p.id) {
                assert_eq!(tts_backend_str(&b), p.id);
            }
        }
    }

    /// Test 4 — no orphan cloud backend variants. Every variant of
    /// `SttBackend` / `LlmBackend` / `AssistantBackend` / `TtsBackend`
    /// that represents a *cloud* provider must appear in at least one
    /// catalogue entry. "Cloud" excludes local, none, ollama, wyoming,
    /// piper, and the model-host-agnostic Whisper local backend.
    #[test]
    fn no_orphan_cloud_variants() {
        for b in crate::providers::all_stt_backends() {
            if matches!(b, SttBackend::Local | SttBackend::Wyoming) {
                continue;
            }
            let id = stt_backend_str(&b);
            assert!(
                CLOUD_PROVIDERS.iter().any(|p| p.id == id && p.stt.is_some()),
                "SttBackend::{b:?} ({id}) is not present in CLOUD_PROVIDERS",
            );
        }
        for b in crate::providers::all_llm_backends() {
            if matches!(b, LlmBackend::Local | LlmBackend::None | LlmBackend::Ollama) {
                continue;
            }
            let id = llm_backend_str(&b);
            assert!(
                CLOUD_PROVIDERS.iter().any(|p| p.id == id && p.llm.is_some()),
                "LlmBackend::{b:?} ({id}) is not present in CLOUD_PROVIDERS",
            );
        }
        for b in crate::providers::all_assistant_backends() {
            if matches!(
                b,
                AssistantBackend::None | AssistantBackend::Local | AssistantBackend::Ollama
            ) {
                continue;
            }
            let id = assistant_backend_str(&b);
            assert!(
                CLOUD_PROVIDERS
                    .iter()
                    .any(|p| p.id == id && p.assistant.is_some()),
                "AssistantBackend::{b:?} ({id}) is not present in CLOUD_PROVIDERS",
            );
        }
        for b in crate::providers::all_tts_backends() {
            if matches!(b, TtsBackend::None | TtsBackend::Wyoming | TtsBackend::Piper) {
                continue;
            }
            let id = tts_backend_str(&b);
            assert!(
                CLOUD_PROVIDERS.iter().any(|p| p.id == id && p.tts.is_some()),
                "TtsBackend::{b:?} ({id}) is not present in CLOUD_PROVIDERS",
            );
        }
    }

    /// Test 5 — smoke check that every `&'static str` model reference
    /// resolves at runtime. Compile time guarantees the string is a
    /// valid `'static`; this just exercises every branch so a future
    /// `cargo expand` regression that swaps a literal for a function
    /// call is caught immediately.
    #[test]
    fn model_strings_resolve() {
        let mut count = 0usize;
        for p in CLOUD_PROVIDERS {
            if let Some(s) = &p.stt {
                assert!(!s.model.is_empty(), "{}: empty STT model", p.id);
                count += 1;
            }
            if let Some(l) = &p.llm {
                assert!(!l.model.is_empty(), "{}: empty LLM model", p.id);
                count += 1;
            }
            if let Some(a) = &p.assistant {
                assert!(!a.text_model.is_empty(), "{}: empty assistant text_model", p.id);
                if let Some(mm) = a.multimodal_model {
                    assert!(!mm.is_empty(), "{}: empty multimodal_model literal", p.id);
                }
                count += 1;
            }
            if let Some(t) = &p.tts {
                assert!(!t.model.is_empty(), "{}: empty TTS model", p.id);
                // default_voice is allowed to be empty (Deepgram, where
                // the model id encodes the voice).
                count += 1;
            }
        }
        assert!(count > 0, "no capabilities declared — catalogue is empty?");
    }

    /// Every entry must declare at least one capability — an entry
    /// with no STT/LLM/Assistant/TTS is meaningless.
    #[test]
    fn every_entry_has_a_capability() {
        for p in CLOUD_PROVIDERS {
            assert!(
                p.stt.is_some() || p.llm.is_some() || p.assistant.is_some() || p.tts.is_some(),
                "{} declares no capability",
                p.id
            );
        }
    }

    /// Ids are unique — duplicates would silently make `find()` lose
    /// later entries.
    #[test]
    fn ids_are_unique() {
        let mut seen: Vec<&'static str> = Vec::new();
        for p in CLOUD_PROVIDERS {
            assert!(
                !seen.contains(&p.id),
                "duplicate catalogue entry for id {}",
                p.id
            );
            seen.push(p.id);
        }
    }

    /// Phase E1 — pin the multimodal / web-search values per provider
    /// so a casual catalogue edit doesn't silently flip a vision-
    /// capable provider to text-only or vice versa. Update this test
    /// together with the corresponding ADR (`docs/decisions/0007-…`).
    #[test]
    fn assistant_multimodal_and_web_search_pinned() {
        let cases: &[(&str, Option<&str>, WebSearchSupport)] = &[
            (
                "openai",
                Some("gpt-5.4-mini"),
                WebSearchSupport::NativeTool("web_search_preview"),
            ),
            (
                "anthropic",
                Some("claude-haiku-4-5-20251001"),
                WebSearchSupport::NativeTool("web_search_20250305"),
            ),
            (
                "gemini",
                Some("gemini-1.5-flash"),
                WebSearchSupport::NativeTool("google_search"),
            ),
            (
                "groq",
                Some("llama-4-maverick-17b-128e-instruct"),
                WebSearchSupport::None,
            ),
            ("cerebras", None, WebSearchSupport::None),
            ("openrouter", None, WebSearchSupport::None),
        ];
        for (id, mm, ws) in cases {
            let entry = find(id).unwrap_or_else(|| panic!("missing catalogue entry for {id}"));
            let adef = entry
                .assistant
                .unwrap_or_else(|| panic!("{id} has no assistant defaults"));
            assert_eq!(
                adef.multimodal_model, *mm,
                "multimodal_model drift for {id}"
            );
            assert_eq!(adef.web_search, *ws, "web_search drift for {id}");
        }
    }
}
