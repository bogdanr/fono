// SPDX-License-Identifier: GPL-3.0-only
//! Capability catalogue for cloud providers.
//!
//! This is the single source of truth for which cloud providers cover
//! which capabilities (STT, polish, assistant chat, TTS), plus
//! future-facing assistant extras (multimodal model id, web-search
//! support) and the TTS endpoint shape. The wizard, `fono use cloud`,
//! and `fono doctor` consume this catalogue.
//!
//! This catalogue is the **single source of truth** for default cloud
//! model strings, default voices, key environment variable names, and
//! TTS endpoint shapes. The thin wrappers in `fono-stt::defaults`,
//! `fono-polish::defaults`, and `fono-assistant::factory` (all named
//! `default_cloud_model`) read from here at runtime — to change the
//! default model for a provider, edit only the relevant
//! `CloudProvider` entry below. Consumer crates do not duplicate the
//! literal model id any more.
//!
//! [`CLOUD_PROVIDERS`] enumerates every cloud provider currently
//! referenced by `crates/fono-core/src/providers.rs`. Maintainers must
//! keep the two in lockstep — the unit tests in this module fail if a
//! cloud `*Backend` variant ever lacks a catalogue entry.

use crate::config::{PolishBackend, SttBackend};
use crate::providers::{parse_polish_backend, parse_stt_backend};
use crate::voice_palette::{Gender, PaletteEntry};

/// Defaults for a provider's speech-to-text capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SttDefaults {
    /// Default STT model identifier. Consumed by
    /// `fono_stt::defaults::default_cloud_model`.
    pub model: &'static str,
}

/// Defaults for a provider's polish capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolishDefaults {
    /// Default cleanup model. Consumed by
    /// `fono_polish::defaults::default_cloud_model`.
    pub model: &'static str,
}

/// Defaults for a provider's voice-assistant capability.
#[derive(Debug, Clone, Copy)]
pub struct AssistantDefaults {
    /// Default chat model. Consumed by
    /// `fono_assistant::factory::default_cloud_model`.
    pub text_model: &'static str,
    /// Multimodal sibling model where the provider exposes one — used
    /// when the assistant input includes screenshots/images. `None`
    /// means the provider has no multimodal endpoint Fono is willing
    /// to default to.
    pub multimodal_model: Option<&'static str>,
    /// Native web-search support advertised by the provider.
    pub web_search: WebSearchSupport,
    /// Realtime / speech-to-speech profile, when the provider exposes a
    /// bidirectional voice WebSocket (e.g. Gemini Live). `None` means the
    /// provider is staged-pipeline only. Selected via
    /// `[assistant.cloud].model` matching `RealtimeProfile::model`.
    pub realtime: Option<RealtimeProfile>,
    /// Capability badges to render in the wizard's provider picker.
    pub badges: &'static [Badge],
}

/// A provider's realtime (speech-to-speech) voice profile. When the
/// configured assistant model equals [`RealtimeProfile::model`], the F8
/// path opens a single bidirectional WebSocket instead of running the
/// staged STT → LLM → TTS pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeProfile {
    /// Realtime model id. Matched against `[assistant.cloud].model` to
    /// decide staged vs realtime. Surfaced by `fono doctor`.
    pub model: &'static str,
    /// WebSocket endpoint base URL (key/query appended by the client).
    pub ws_url: &'static str,
    /// Which realtime wire protocol the client must speak.
    pub protocol: RealtimeProtocol,
    /// PCM sample rate the model expects on the mic-input stream (Hz).
    pub input_sample_rate: u32,
    /// PCM sample rate the model emits on the reply-audio stream (Hz).
    pub output_sample_rate: u32,
}

/// Realtime wire protocols Fono can speak. Each maps to one client
/// module in `fono-assistant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeProtocol {
    /// Google Gemini Live `BidiGenerateContent` over WebSocket.
    GeminiLive,
    /// OpenAI Realtime API over WebSocket. (Reserved; not yet wired.)
    OpenAiRealtime,
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
    /// Provider offers polish.
    Polish,
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
    /// Provider exposes a realtime / speech-to-speech (WebSocket) model.
    Realtime,
}

/// Defaults for a provider's text-to-speech capability.
#[derive(Debug, Clone, Copy)]
pub struct TtsDefaults {
    /// Default TTS model identifier.
    pub model: &'static str,
    /// Default voice id / name.
    pub default_voice: &'static str,
    /// Curated, gender-tagged voice palette (≤10) for per-program voices.
    /// The user addresses these by positional gendered label ("Female 1",
    /// "Male 2") via `fono voices`; the cryptic backend id lives only here.
    /// May be empty for a provider whose voice ids cannot be enumerated
    /// safely yet (the resolver then falls back to `default_voice`).
    pub voices: &'static [PaletteEntry],
    /// Endpoint shape (which client to instantiate, plus base URL).
    pub endpoint: TtsEndpoint,
    /// Whether the doctor should runtime-probe the provider's TTS
    /// endpoint. **False for every catalogue entry in Phase A** — no
    /// runtime probes anywhere yet; this flag is reserved for later
    /// phases that may want to verify endpoint availability.
    pub runtime_probe: bool,
    /// Declarative live-voice discovery descriptor. `Some` means the
    /// provider exposes an enumerable voice list (e.g. ElevenLabs
    /// `/v1/voices`, Cartesia `/voices`) that `fono voices discover` can
    /// probe to refresh and expand this curated palette at runtime.
    /// `None` (the default for every entry) means the provider has no
    /// listable voice catalogue, so the curated `voices` above are the
    /// only palette — discovery is simply skipped, never an error. See
    /// [`VoiceDiscovery`]; this mirrors the [`KeyValidation`] pattern so
    /// onboarding a new provider stays declarative.
    pub discovery: Option<VoiceDiscovery>,
    /// True when this provider's TTS only renders intelligible **English**
    /// — i.e. feeding it text in another language produces an English
    /// phonemization of foreign words (gibberish), not speech in that
    /// language. Defaults to `false` (multilingual) so a new provider is
    /// only ever constrained by an explicit opt-in; forgetting to set it
    /// fails safe to the current "speak whatever the cloud returns"
    /// behaviour. When `true` **and** the local ONNX engine is compiled
    /// in, `fono-tts` transparently routes non-English utterances to the
    /// local multilingual voice instead (see
    /// `fono_tts::english_only_fallback`).
    pub english_only: bool,
}

/// Wire-level shape of a provider's TTS API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsEndpoint {
    /// OpenAI-compatible `POST /audio/speech` endpoint at `base_url`.
    OpenAiCompat {
        /// Base URL up to and including `/v1` (the client appends
        /// `/audio/speech`).
        base_url: &'static str,
        /// Wire value for the request's `response_format` field.
        /// OpenAI accepts `pcm` (raw 24 kHz int16 LE mono, the fastest
        /// path); Groq's Orpheus deployment only accepts `wav` and
        /// 400s on `pcm`. The client strips the 44-byte RIFF header
        /// when this is `"wav"`.
        response_format: &'static str,
        /// Optional wire value for the request's `stream_format` field.
        /// When `Some("audio")`, the server streams raw audio bytes as
        /// they are generated (instead of buffering the whole reply
        /// before opening the body). Critical for LLM-based TTS models
        /// like `gpt-4o-mini-tts` where buffer-then-deliver mode costs
        /// ~30 s of synthesis time before the first byte arrives.
        /// Leave `None` for providers whose `/audio/speech` proxy may
        /// reject unknown request fields (e.g. Groq's Orpheus).
        stream_format: Option<&'static str>,
    },
    /// Cartesia's bespoke `POST /v1/tts/bytes` endpoint.
    Cartesia,
    /// Deepgram's `POST /v1/speak` endpoint.
    Deepgram,
    /// Speechmatics' preview `POST /generate/<voice>` endpoint. The
    /// client appends the voice id to `base_url` and requests
    /// `?output_format=pcm_16000` (raw int16 LE mono @ 16 kHz).
    Speechmatics {
        /// Base URL up to (not including) `/generate/<voice>`.
        base_url: &'static str,
    },
    /// ElevenLabs' `POST /v1/text-to-speech/<voice_id>` endpoint. The
    /// client appends the voice id to the path and requests
    /// `?output_format=pcm_24000` (raw int16 LE mono @ 24 kHz). Library
    /// and professional voices are plan-gated (free-tier keys get HTTP
    /// 402); the catalogue default is a premade voice. See
    /// `docs/providers.md`.
    ElevenLabs,
    /// Gemini native TTS: `POST /v1beta/models/<model>:generateContent`
    /// with `responseModalities: ["AUDIO"]` and a `prebuiltVoiceConfig`
    /// voice on a single `GEMINI_API_KEY`. Returns base64 int16 LE mono
    /// PCM (24 kHz). Auth header `x-goog-api-key`. See ADR 0034.
    Gemini,
}

/// How an API key is attached to a key-validation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAuth {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// A named header carrying the bare key (e.g. `x-api-key: <key>`).
    Header(&'static str),
    /// A named header carrying `"<prefix> <key>"` (e.g.
    /// `Authorization: Token <key>` for Deepgram).
    HeaderPrefixed {
        /// Header name.
        header: &'static str,
        /// Literal prefix prepended to the key (no trailing space; one
        /// space is inserted between prefix and key).
        prefix: &'static str,
    },
    /// The key is a URL query parameter (e.g. `?key=<key>` for Gemini).
    QueryParam(&'static str),
}

/// Metadata for validating a provider API key. The wizard issues a
/// `GET url` with the key attached per [`auth`](Self::auth) plus every
/// pair in [`extra_headers`](Self::extra_headers); an HTTP 2xx response
/// means the key is valid. `None` on a [`CloudProvider`] means the key
/// is saved unvalidated (no probe endpoint configured).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyValidation {
    /// Endpoint to probe with a `GET` request.
    pub url: &'static str,
    /// How to attach the API key to the request.
    pub auth: KeyAuth,
    /// Extra static headers attached to every probe (e.g. an API
    /// version pin or OpenRouter attribution headers).
    pub extra_headers: &'static [(&'static str, &'static str)],
}

/// A custom parser for a provider whose voice-list JSON the declarative
/// [`VoiceFieldMap`] cannot express. Takes the parsed response body and
/// returns the raw voices; the engine then caps/balances them. Rare —
/// the declarative map covers ElevenLabs and Cartesia — but keeps the
/// mechanism universal for irregular future APIs.
pub type VoiceParser = fn(&serde_json::Value) -> Vec<RawDiscoveredVoice>;

/// A voice as read from a provider's discovery response, before it is
/// capped/balanced into the runtime palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDiscoveredVoice {
    /// The backend-specific wire id the TTS client will send.
    pub backend_id: String,
    /// Optional human display name (for logging / `fono voices list`).
    pub name: Option<String>,
    /// Perceived gender; `Neutral` when the provider exposes none.
    pub gender: crate::voice_palette::Gender,
}

/// How to read a provider's voice-list JSON declaratively. JSON pointers
/// follow RFC 6901 (`serde_json::Value::pointer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceFieldMap {
    /// JSON pointer to the array of voice objects within the response
    /// body (e.g. `"/voices"` for ElevenLabs). `None` means the body is
    /// itself the array (e.g. Cartesia returns a bare array).
    pub array_pointer: Option<&'static str>,
    /// Key within each voice object holding the backend voice id
    /// (e.g. `"voice_id"`, `"id"`).
    pub id_field: &'static str,
    /// Optional key within each voice object holding a display name.
    pub name_field: Option<&'static str>,
    /// Optional JSON pointer, relative to each voice object, to the
    /// gender token (e.g. `"/labels/gender"` for ElevenLabs, `"/gender"`
    /// for Cartesia). Missing/unparseable ⇒ `Gender::Neutral`.
    pub gender_pointer: Option<&'static str>,
}

/// Declarative descriptor for live voice discovery, modelled on
/// [`KeyValidation`]: a `GET list_url` with the API key attached per
/// [`auth`](Self::auth) plus every [`extra_headers`](Self::extra_headers)
/// pair, whose JSON body is mapped via [`map`](Self::map) (or the
/// [`custom`](Self::custom) escape hatch). Onboarding a new provider is
/// therefore data, not code.
#[derive(Debug, Clone, Copy)]
pub struct VoiceDiscovery {
    /// Endpoint to `GET` for the voice list.
    pub list_url: &'static str,
    /// How to attach the API key (reuses the key-validation auth model).
    pub auth: KeyAuth,
    /// Extra static headers (e.g. an API version pin).
    pub extra_headers: &'static [(&'static str, &'static str)],
    /// Declarative field map for the common case.
    pub map: VoiceFieldMap,
    /// Optional custom parser for an irregular response shape. When
    /// `Some`, it takes precedence over [`map`](Self::map).
    pub custom: Option<VoiceParser>,
}

/// Resolve a key-authenticated `GET` into a final URL plus the headers
/// to attach. Shared by the wizard's key-validation probe and the voice
/// discovery probe so the two never drift. Reqwest-free so it lives in
/// `fono-core`; callers build their own request from the result.
///
/// For [`KeyAuth::QueryParam`] the key is appended to the URL; otherwise
/// it is returned as a header. `extra_headers` are appended verbatim.
#[must_use]
pub fn build_auth_get(
    url: &str,
    auth: KeyAuth,
    key: &str,
    extra_headers: &[(&str, &str)],
) -> (String, Vec<(String, String)>) {
    let final_url = match auth {
        KeyAuth::QueryParam(param) => {
            let sep = if url.contains('?') { '&' } else { '?' };
            format!("{url}{sep}{param}={key}")
        }
        _ => url.to_string(),
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    match auth {
        KeyAuth::Bearer => headers.push(("Authorization".to_string(), format!("Bearer {key}"))),
        KeyAuth::Header(h) => headers.push((h.to_string(), key.to_string())),
        KeyAuth::HeaderPrefixed { header, prefix } => {
            headers.push((header.to_string(), format!("{prefix} {key}")));
        }
        KeyAuth::QueryParam(_) => {}
    }
    for (h, v) in extra_headers {
        headers.push(((*h).to_string(), (*v).to_string()));
    }
    (final_url, headers)
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
    /// polish capability.
    pub polish: Option<PolishDefaults>,
    /// Voice-assistant chat capability.
    pub assistant: Option<AssistantDefaults>,
    /// TTS capability.
    pub tts: Option<TtsDefaults>,
    /// API-key validation metadata. `None` ⇒ the wizard saves the key
    /// without probing (the unwired STT stubs azure/google/nemotron).
    pub key_validation: Option<KeyValidation>,
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
        key_validation: Some(KeyValidation {
            url: "https://api.openai.com/v1/models",
            auth: KeyAuth::Bearer,
            extra_headers: &[],
        }),
        stt: Some(SttDefaults { model: "whisper-1" }),
        polish: Some(PolishDefaults { model: "gpt-5.4-nano" }),
        // TODO: re-enable web search when fono-assistant migrates the
        // OpenAI client to the Responses API (POST /v1/responses). The
        // chat/completions API rejects unknown tool types.
        assistant: Some(AssistantDefaults {
            text_model: "gpt-5.4-mini",
            // GPT-5.4 family is multimodal; reuse the assistant default.
            multimodal_model: Some("gpt-5.4-mini"),
            web_search: WebSearchSupport::None,
            realtime: None,
            badges: &[Badge::Stt, Badge::Polish, Badge::Assistant, Badge::Tts, Badge::Vision],
        }),
        tts: Some(TtsDefaults {
            model: "tts-1",
            default_voice: "alloy",
            // The six voices supported by `tts-1`. Gender labels follow
            // OpenAI's commonly attributed presentation; `alloy` is the
            // neutral default.
            voices: &[
                PaletteEntry { backend_id: "nova", gender: Gender::Female },
                PaletteEntry { backend_id: "shimmer", gender: Gender::Female },
                PaletteEntry { backend_id: "alloy", gender: Gender::Neutral },
                PaletteEntry { backend_id: "echo", gender: Gender::Male },
                PaletteEntry { backend_id: "fable", gender: Gender::Male },
                PaletteEntry { backend_id: "onyx", gender: Gender::Male },
            ],
            endpoint: TtsEndpoint::OpenAiCompat {
                base_url: "https://api.openai.com/v1",
                response_format: "pcm",
                // `tts-1` already streams audio bytes by default, but
                // setting this explicitly future-proofs the entry for
                // model overrides like `gpt-4o-mini-tts` where the
                // server otherwise buffers the entire synthesis
                // before opening the response body.
                stream_format: Some("audio"),
            },
            runtime_probe: false,
            // OpenAI's TTS voice set is fixed (no per-account listing endpoint).
            discovery: None,
            // OpenAI's TTS models are multilingual.
            english_only: false,
        }),
    },
    // ----- Groq --------------------------------------------------------
    CloudProvider {
        id: "groq",
        display_name: "Groq",
        tagline: "Lowest-latency cloud STT and OpenAI-compat LLM hosting.",
        console_url: "https://console.groq.com/keys",
        key_env: "GROQ_API_KEY",
        key_validation: Some(KeyValidation {
            url: "https://api.groq.com/openai/v1/models",
            auth: KeyAuth::Bearer,
            extra_headers: &[],
        }),
        stt: Some(SttDefaults { model: "whisper-large-v3-turbo" }),
        polish: Some(PolishDefaults { model: "openai/gpt-oss-120b" }),
        assistant: Some(AssistantDefaults {
            text_model: "openai/gpt-oss-120b",
            // Groq currently exposes no vision-capable model Fono is
            // willing to default to. `openai/gpt-oss-120b` (the text
            // model above) is text-only; the previously catalogued
            // `llama-4-maverick-17b-128e-instruct` was removed after
            // Groq returned 404 `model_not_found` for it. Re-enable
            // multimodal here only when Groq ships a hosted vision
            // model with an OSI-compatible licence we're willing to
            // make the default.
            multimodal_model: None,
            // TODO: Groq's compound-beta / compound-beta-mini models
            // provide built-in web search via model swap. Wire as an
            // opt-in once we have a coherent search-via-model-swap
            // design (see docs/decisions/0024).
            web_search: WebSearchSupport::None,
            realtime: None,
            badges: &[Badge::Stt, Badge::Polish, Badge::Assistant, Badge::Tts, Badge::Fast],
        }),
        tts: Some(TtsDefaults {
            // Canopy Labs Orpheus on Groq's OpenAI-compatible
            // `/audio/speech` endpoint. Replaces the PlayAI family
            // (the previously catalogued model now returns
            // `model_not_found` after Groq retired it). Groq's
            // hosted Orpheus exposes a curated six-voice set —
            // `autumn` / `diana` / `hannah` / `austin` / `daniel` /
            // `troy` — which is narrower than Canopy's open-source
            // Orpheus checkpoint (tara / leah / jess / ...); sending
            // one of those upstream-only voices returns HTTP 400
            // from Groq's `/audio/speech` ("voice must be one of
            // ..."). We default to `hannah` (the neutral-female
            // option in Groq's set, also used in Groq's own JS
            // sample for Orpheus).
            model: "canopylabs/orpheus-v1-english",
            default_voice: "hannah",
            // Groq's curated Orpheus six-voice set (see the model note
            // above). Sending an upstream-only Orpheus voice 400s, so the
            // palette is exactly Groq's hosted set.
            voices: &[
                PaletteEntry { backend_id: "hannah", gender: Gender::Female },
                PaletteEntry { backend_id: "autumn", gender: Gender::Female },
                PaletteEntry { backend_id: "diana", gender: Gender::Female },
                PaletteEntry { backend_id: "austin", gender: Gender::Male },
                PaletteEntry { backend_id: "daniel", gender: Gender::Male },
                PaletteEntry { backend_id: "troy", gender: Gender::Male },
            ],
            endpoint: TtsEndpoint::OpenAiCompat {
                base_url: "https://api.groq.com/openai/v1",
                // Groq's Orpheus deployment rejects `pcm` with
                // "response_format must be one of [wav]". The client
                // strips the WAV header back to raw PCM transparently.
                response_format: "wav",
                // Groq's Orpheus proxy is conservative about request
                // fields; leave `stream_format` unset to preserve the
                // wire shape that is known to work.
                stream_format: None,
            },
            runtime_probe: false,
            // Groq's hosted Orpheus voice set is fixed (no listing endpoint).
            discovery: None,
            // Groq hosts Canopy Labs Orpheus `…-english`: English-only.
            english_only: true,
        }),
    },
    // ----- Anthropic ---------------------------------------------------
    CloudProvider {
        id: "anthropic",
        display_name: "Anthropic",
        tagline: "Claude family with vision and native web-search tool.",
        console_url: "https://console.anthropic.com/settings/keys",
        key_env: "ANTHROPIC_API_KEY",
        key_validation: Some(KeyValidation {
            url: "https://api.anthropic.com/v1/models",
            auth: KeyAuth::Header("x-api-key"),
            extra_headers: &[("anthropic-version", "2023-06-01")],
        }),
        stt: None,
        polish: Some(PolishDefaults {
            // TODO: verify against Anthropic's current model list — the
            // Groq Maverick incident (issue: 404 model_not_found)
            // exposed that the Phase A catalogue contained at least
            // one hallucinated model id; the Anthropic dated suffix
            // here is the most likely remaining suspect.
            model: "claude-haiku-4-5-20251001",
        }),
        assistant: Some(AssistantDefaults {
            // TODO: verify against Anthropic's current model list.
            text_model: "claude-haiku-4-5-20251001",
            // Claude Haiku 4.5 is multimodal (image input supported).
            multimodal_model: Some("claude-haiku-4-5-20251001"),
            web_search: WebSearchSupport::NativeTool("web_search_20250305"),
            realtime: None,
            badges: &[Badge::Polish, Badge::Assistant, Badge::Vision, Badge::Search],
        }),
        tts: None,
    },
    // ----- Cerebras ----------------------------------------------------
    CloudProvider {
        id: "cerebras",
        display_name: "Cerebras",
        tagline: "Wafer-scale inference for the lowest-latency polish.",
        console_url: "https://cloud.cerebras.ai/platform/keys",
        key_env: "CEREBRAS_API_KEY",
        key_validation: Some(KeyValidation {
            url: "https://api.cerebras.ai/v1/models",
            auth: KeyAuth::Bearer,
            extra_headers: &[],
        }),
        stt: None,
        polish: Some(PolishDefaults { model: "gpt-oss-120b" }),
        assistant: Some(AssistantDefaults {
            text_model: "zai-glm-4.7",
            multimodal_model: None,
            web_search: WebSearchSupport::None,
            realtime: None,
            badges: &[Badge::Polish, Badge::Assistant, Badge::Fast],
        }),
        tts: None,
    },
    // ----- Gemini ------------------------------------------------------
    // Single `GEMINI_API_KEY` (AI Studio), free tier — ADR 0034. Polish
    // and the staged assistant reuse Gemini's OpenAI-compatible surface;
    // `google_search` grounding is declared here but not injected by the
    // compat client (native search is a follow-up). STT/TTS/Live land on
    // the same key via the native `generateContent`/`BidiGenerateContent`
    // endpoints.
    CloudProvider {
        id: "gemini",
        display_name: "Google Gemini",
        tagline: "Gemini Flash on a single free-tier key (polish + assistant).",
        console_url: "https://aistudio.google.com/app/apikey",
        key_env: "GEMINI_API_KEY",
        key_validation: Some(KeyValidation {
            url: "https://generativelanguage.googleapis.com/v1beta/models",
            auth: KeyAuth::QueryParam("key"),
            extra_headers: &[],
        }),
        stt: Some(SttDefaults { model: "gemini-flash-lite-latest" }),
        polish: Some(PolishDefaults { model: "gemini-flash-lite-latest" }),
        assistant: Some(AssistantDefaults {
            text_model: "gemini-flash-lite-latest",
            // The Flash family is multimodal (image input supported); the
            // `-latest` alias tracks the current Flash-Lite model.
            multimodal_model: Some("gemini-flash-lite-latest"),
            // Native grounding tool; not wired through the OpenAI-compat
            // staged client yet (see ADR 0034).
            web_search: WebSearchSupport::NativeTool("google_search"),
            // Gemini Live (BidiGenerateContent) speech-to-speech profile. F8
            // opens one WebSocket; the model ingests mic PCM (16 kHz mono)
            // and streams reply audio back (24 kHz mono) in one continuous
            // voice — fixing both the per-sentence voice drift and the
            // ~6 s/sentence batch-TTS latency of the staged Gemini path.
            // NOTE: model id needs live verification against /v1beta/models
            // (3.1 Flash Live preview); trivially swappable here, and
            // `fono doctor` surfaces the active id. `gemini-2.0-flash-live-001`
            // is the known-GA fallback if this preview id 404s.
            realtime: Some(RealtimeProfile {
                model: "gemini-3.1-flash-live-preview",
                ws_url: "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent",
                protocol: RealtimeProtocol::GeminiLive,
                input_sample_rate: 16_000,
                output_sample_rate: 24_000,
            }),
            badges: &[Badge::Polish, Badge::Assistant, Badge::Vision, Badge::Search, Badge::Fast, Badge::Realtime],
        }),
        tts: Some(TtsDefaults {
            // Native Gemini TTS model (`:generateContent` with an AUDIO
            // response modality). 24 kHz mono PCM out.
            model: "gemini-3.1-flash-tts-preview",
            // `Kore` (firm, female) is Google's documented default prebuilt
            // voice.
            default_voice: "Kore",
            // Curated, gender-balanced subset of the 30 prebuilt voices.
            // The user addresses these by positional gendered label
            // ("Female 1", "Male 2") via `fono voices`; the full list is
            // not enumerable via an API, so this stays a static palette.
            voices: &[
                PaletteEntry { backend_id: "Kore", gender: Gender::Female },
                PaletteEntry { backend_id: "Aoede", gender: Gender::Female },
                PaletteEntry { backend_id: "Leda", gender: Gender::Female },
                PaletteEntry { backend_id: "Zephyr", gender: Gender::Female },
                PaletteEntry { backend_id: "Callirrhoe", gender: Gender::Female },
                PaletteEntry { backend_id: "Puck", gender: Gender::Male },
                PaletteEntry { backend_id: "Charon", gender: Gender::Male },
                PaletteEntry { backend_id: "Fenrir", gender: Gender::Male },
                PaletteEntry { backend_id: "Orus", gender: Gender::Male },
                PaletteEntry { backend_id: "Enceladus", gender: Gender::Male },
            ],
            endpoint: TtsEndpoint::Gemini,
            runtime_probe: false,
            // The 30 prebuilt voices are a fixed set with no listing API.
            discovery: None,
            // Gemini TTS is multilingual (40+ languages incl. Romanian) and
            // auto-detects the spoken language from the text.
            english_only: false,
        }),
    },
    // ----- OpenRouter --------------------------------------------------
    CloudProvider {
        id: "openrouter",
        display_name: "OpenRouter",
        tagline: "Unified gateway across hundreds of model providers.",
        console_url: "https://openrouter.ai/keys",
        key_env: "OPENROUTER_API_KEY",
        key_validation: Some(KeyValidation {
            url: "https://openrouter.ai/api/v1/auth/key",
            auth: KeyAuth::Bearer,
            extra_headers: &[
                ("HTTP-Referer", crate::openrouter_attribution::REFERER),
                ("X-OpenRouter-Title", crate::openrouter_attribution::TITLE),
                ("X-OpenRouter-Categories", crate::openrouter_attribution::CATEGORIES),
            ],
        }),
        stt: Some(SttDefaults {
            // OpenRouter proxies OpenAI-compatible
            // `POST /v1/audio/transcriptions` to upstream providers;
            // `openai/whisper-large-v3-turbo` routes to Groq's fastest
            // Whisper model.
            model: "openai/whisper-large-v3-turbo",
        }),
        polish: Some(PolishDefaults { model: "openai/gpt-5.4-nano" }),
        assistant: Some(AssistantDefaults {
            text_model: "anthropic/claude-haiku-4.5",
            // Multimodal is route-dependent on OpenRouter; leave None
            // until the wizard surfaces explicit per-route choices.
            multimodal_model: None,
            // Web-search support is route-dependent; default to None
            // and let later phases enable per-route overrides.
            web_search: WebSearchSupport::None,
            realtime: None,
            badges: &[Badge::Polish, Badge::Assistant, Badge::Tts],
        }),
        tts: Some(TtsDefaults {
            // xAI's `grok-voice-tts-1.0` via OpenRouter. Replaces the
            // previous `openai/tts-1` default — the OpenAI model is
            // not exposed on OpenRouter today, and Grok Voice TTS
            // works correctly through OpenRouter's `/audio/speech`
            // proxy. Users who want a different voice can pin
            // `model = "…"` and `voice = "…"` in `[tts.openrouter]`
            // of `config.toml`.
            model: "x-ai/grok-voice-tts-1.0",
            default_voice: "ara",
            // Only the default Grok voice is pinned here; the wider voice
            // set is not reliably enumerable through OpenRouter yet, so the
            // palette stays minimal and the resolver falls back to the
            // default (no per-program distinctness on this backend until
            // auto-discovery lands).
            voices: &[PaletteEntry { backend_id: "ara", gender: Gender::Female }],
            endpoint: TtsEndpoint::OpenAiCompat {
                base_url: "https://openrouter.ai/api/v1",
                response_format: "pcm",
                // OpenRouter's `/audio/speech` proxy is conservative
                // about unknown request fields for non-OpenAI models;
                // omit `stream_format` so the wire body matches the
                // shape we know works.
                stream_format: None,
            },
            runtime_probe: false,
            // OpenRouter does not expose an enumerable TTS voice list yet.
            discovery: None,
            // Grok Voice TTS renders several languages; not English-only.
            english_only: false,
        }),
    },
    // ----- Deepgram ----------------------------------------------------
    CloudProvider {
        id: "deepgram",
        display_name: "Deepgram",
        tagline: "Real-time Nova STT and Aura voice TTS.",
        console_url: "https://console.deepgram.com/",
        key_env: "DEEPGRAM_API_KEY",
        key_validation: Some(KeyValidation {
            url: "https://api.deepgram.com/v1/projects",
            auth: KeyAuth::HeaderPrefixed { header: "Authorization", prefix: "Token" },
            extra_headers: &[],
        }),
        stt: Some(SttDefaults { model: "nova-3" }),
        polish: None,
        assistant: None,
        tts: Some(TtsDefaults {
            model: "aura-2-thalia-en",
            // Deepgram TTS selects a voice via the `model` parameter
            // (e.g. `aura-2-thalia-en` *is* the voice). Keep an empty
            // default_voice rather than duplicating the model id.
            default_voice: "",
            // Deepgram selects a voice via the `model` parameter, so each
            // palette id is a full Aura-2 model id. The resolver maps a
            // chosen palette voice onto the request `model` for this
            // backend.
            voices: &[
                PaletteEntry { backend_id: "aura-2-thalia-en", gender: Gender::Female },
                PaletteEntry { backend_id: "aura-2-andromeda-en", gender: Gender::Female },
                PaletteEntry { backend_id: "aura-2-apollo-en", gender: Gender::Male },
                PaletteEntry { backend_id: "aura-2-arcas-en", gender: Gender::Male },
            ],
            endpoint: TtsEndpoint::Deepgram,
            runtime_probe: false,
            // Aura-2's voice set is a fixed catalogue of model ids; no
            // per-account listing endpoint, so discovery is skipped.
            discovery: None,
            // Aura-2 ships voices in several languages (the model id
            // carries the locale, e.g. `…-en`/`…-es`); not English-only.
            english_only: false,
        }),
    },
    // ----- AssemblyAI --------------------------------------------------
    CloudProvider {
        id: "assemblyai",
        display_name: "AssemblyAI",
        tagline: "High-accuracy STT with rich post-processing options.",
        console_url: "https://www.assemblyai.com/app/account",
        key_env: "ASSEMBLYAI_API_KEY",
        key_validation: Some(KeyValidation {
            url: "https://api.assemblyai.com/v2/transcript",
            auth: KeyAuth::Header("Authorization"),
            extra_headers: &[],
        }),
        stt: Some(SttDefaults { model: "best" }),
        polish: None,
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
        key_validation: Some(KeyValidation {
            url: "https://api.cartesia.ai/voices",
            auth: KeyAuth::Header("X-Api-Key"),
            extra_headers: &[("Cartesia-Version", "2026-03-01")],
        }),
        // Cartesia's batch endpoint (`POST /stt`) requires the
        // `ink-whisper` family of models; `ink-2` is reachable only
        // via the realtime WebSocket endpoint
        // (`wss://api.cartesia.ai/stt/turns/websocket`) which is the
        // Phase 2 streaming work — see
        // `plans/2026-05-23-cartesia-stt-support-v2.md`.
        stt: Some(SttDefaults { model: "ink-whisper" }),
        polish: None,
        assistant: None,
        tts: Some(TtsDefaults {
            model: "sonic-3.5",
            // Cartesia's "Sonic English Female" preset voice id.
            default_voice: "a0e99841-438c-4a64-b679-ae501e7d6091",
            // Only the default preset is pinned; Cartesia voice ids are
            // account-scoped UUIDs best refreshed by the optional
            // `/voices` auto-discovery probe rather than hard-coded.
            voices: &[PaletteEntry {
                backend_id: "a0e99841-438c-4a64-b679-ae501e7d6091",
                gender: Gender::Female,
            }],
            endpoint: TtsEndpoint::Cartesia,
            runtime_probe: false,
            // Cartesia exposes account voices at `GET /voices` (a paginated
            // envelope `{ "data": [ { id, name, gender } ], has_more,
            // next_page }`; gender is `feminine`/`masculine`/`gender_neutral`).
            // Reuses the same key auth + version header as key validation.
            discovery: Some(VoiceDiscovery {
                list_url: "https://api.cartesia.ai/voices",
                auth: KeyAuth::Header("X-Api-Key"),
                extra_headers: &[("Cartesia-Version", "2026-03-01")],
                map: VoiceFieldMap {
                    array_pointer: Some("/data"),
                    id_field: "id",
                    name_field: Some("name"),
                    gender_pointer: Some("/gender"),
                },
                custom: None,
            }),
            // Cartesia Sonic is multilingual.
            english_only: false,
        }),
    },
    // ----- ElevenLabs (STT Scribe + TTS Eleven v3) --------------------
    CloudProvider {
        id: "elevenlabs",
        display_name: "ElevenLabs",
        tagline: "Scribe speech-to-text plus the expressive Eleven v3 voice model.",
        console_url: "https://elevenlabs.io/app/settings/api-keys",
        key_env: "ELEVENLABS_API_KEY",
        key_validation: Some(KeyValidation {
            url: "https://api.elevenlabs.io/v1/models",
            auth: KeyAuth::Header("xi-api-key"),
            extra_headers: &[],
        }),
        // Scribe is ElevenLabs' batch speech-to-text model
        // (`POST /v1/speech-to-text`, multipart `model_id` + `file`).
        stt: Some(SttDefaults { model: "scribe_v1" }),
        polish: None,
        assistant: None,
        tts: Some(TtsDefaults {
            // Eleven v3 — the expressive flagship voice model. The
            // client posts plain dictation text; v3's audio-tag /
            // IPA prompting features (see ElevenLabs' v3 best-
            // practices) are available to users who type them into
            // the assistant reply, but Fono adds none itself.
            model: "eleven_v3",
            // "Sarah" — a *current* default premade voice present in
            // every account (including new free-tier ones), so it
            // synthesises via the API without a paid plan. Multilingual,
            // so it serves every configured language. NOTE: do NOT use
            // the legacy "Rachel" id (`21m00Tcm4TlvDq8ikWAM`) from the
            // v3 docs examples — it is a *library* voice absent from
            // modern accounts, and the API rejects it for free users
            // with `402 paid_plan_required` ("Free users cannot use
            // library voices via the API"). Verified 2026-06-15.
            default_voice: "EXAVITQu4vr4xnSDxMaL",
            // Standard premade voices present in every account (including
            // free tier) so they synthesise without a paid plan. Library
            // voices are intentionally excluded (they 402 for free keys).
            voices: &[
                PaletteEntry { backend_id: "EXAVITQu4vr4xnSDxMaL", gender: Gender::Female }, // Sarah
                PaletteEntry { backend_id: "XB0fDUnXU5powFXDhCwa", gender: Gender::Female }, // Charlotte
                PaletteEntry { backend_id: "Xb7hH8MSUJpSbSDYk0k2", gender: Gender::Female }, // Alice
                PaletteEntry { backend_id: "JBFqnCBsd6RMkjVDRZzb", gender: Gender::Male }, // George
                PaletteEntry { backend_id: "TX3LPaxmHKxFdv7VOQHJ", gender: Gender::Male }, // Liam
                PaletteEntry { backend_id: "CwhRBWXzGAHq8TQ4Fs17", gender: Gender::Male }, // Roger
            ],
            endpoint: TtsEndpoint::ElevenLabs,
            runtime_probe: false,
            // ElevenLabs exposes account voices at `GET /v1/voices`
            // (`{ "voices": [ { voice_id, name, labels: { gender } } ] }`).
            // Reuses the `xi-api-key` auth from key validation.
            discovery: Some(VoiceDiscovery {
                list_url: "https://api.elevenlabs.io/v1/voices",
                auth: KeyAuth::Header("xi-api-key"),
                extra_headers: &[],
                map: VoiceFieldMap {
                    array_pointer: Some("/voices"),
                    id_field: "voice_id",
                    name_field: Some("name"),
                    gender_pointer: Some("/labels/gender"),
                },
                custom: None,
            }),
            // Eleven v3 renders 70+ languages — not English-only.
            english_only: false,
        }),
    },
    // ----- Azure (STT-only stub) --------------------------------------
    // Azure / Speechmatics / Google / Nemotron exist as `SttBackend`
    // included here so the "no orphans" unit test (test 4) sees every
    // cloud variant in at least one catalogue entry. Display strings
    // are placeholders until the providers are first-classed.
    CloudProvider {
        id: "azure",
        display_name: "Azure Speech",
        tagline: "Azure Cognitive Services Speech-to-Text (planned).",
        console_url: "https://portal.azure.com/",
        key_env: "AZURE_API_KEY",
        key_validation: None,
        stt: Some(SttDefaults { model: "whisper" }),
        polish: None,
        assistant: None,
        tts: None,
    },
    // ----- Speechmatics (STT realtime + TTS preview) ------------------
    CloudProvider {
        id: "speechmatics",
        display_name: "Speechmatics",
        tagline: "Speechmatics realtime speech-to-text and preview text-to-speech.",
        console_url: "https://portal.speechmatics.com/settings/api-keys",
        key_env: "SPEECHMATICS_API_KEY",
        key_validation: Some(KeyValidation {
            url: "https://asr.api.speechmatics.com/v2/jobs?limit=1",
            auth: KeyAuth::Bearer,
            extra_headers: &[],
        }),
        // Speechmatics' realtime API selects accuracy via an
        // "operating point" rather than a model name; `enhanced` is the
        // higher-accuracy default. The STT client treats this string as
        // the `operating_point` in its `transcription_config`.
        stt: Some(SttDefaults { model: "enhanced" }),
        polish: None,
        assistant: None,
        tts: Some(TtsDefaults {
            // Speechmatics' TTS preview has no model selector; the voice
            // is chosen via the URL path. The client ignores this field
            // — it exists only to satisfy the non-empty-model catalogue
            // invariant and to label the preview service.
            model: "preview",
            // English (UK) female preview voice.
            default_voice: "sarah",
            // The preview exposes a single documented voice; pin it and
            // let the resolver fall back to the default.
            voices: &[PaletteEntry { backend_id: "sarah", gender: Gender::Female }],
            endpoint: TtsEndpoint::Speechmatics {
                base_url: "https://preview.tts.speechmatics.com",
            },
            runtime_probe: false,
            // The Speechmatics TTS preview exposes a single fixed voice.
            discovery: None,
            // The Speechmatics TTS preview is English-only.
            english_only: true,
        }),
    },
    // ----- Google (STT-only stub) -------------------------------------
    CloudProvider {
        id: "google",
        display_name: "Google Cloud Speech",
        tagline: "Google Cloud Speech-to-Text (planned).",
        console_url: "https://console.cloud.google.com/",
        key_env: "GOOGLE_API_KEY",
        key_validation: None,
        stt: Some(SttDefaults { model: "default" }),
        polish: None,
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
        key_validation: None,
        stt: Some(SttDefaults {
            // No specific entry yet; generic Whisper fallback.
            model: "whisper-large-v3",
        }),
        polish: None,
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

/// Look up the first catalogue entry whose `key_env` matches `key_env`.
/// Providers that share an API-key env var (e.g. OpenAI's polish + STT)
/// resolve to the same key regardless, so the first match's validation
/// endpoint is a valid liveness probe for that key. Shared by the
/// wizard's key-validation probe and `fono keys check` / `fono doctor`'s
/// live reachability probes.
#[must_use]
pub fn find_by_key_env(key_env: &str) -> Option<&'static CloudProvider> {
    CLOUD_PROVIDERS.iter().find(|p| p.key_env == key_env)
}

/// True when the given TTS backend only renders intelligible English
/// (see [`TtsDefaults::english_only`]). Non-cloud backends (`None`,
/// `Wyoming`, `Local`) and any backend without a catalogue TTS entry
/// resolve to `false` — they are either multilingual or handle language
/// routing themselves.
#[must_use]
pub fn tts_backend_english_only(backend: &crate::config::TtsBackend) -> bool {
    let id = crate::providers::tts_backend_str(backend);
    find(id).and_then(|p| p.tts.as_ref()).is_some_and(|t| t.english_only)
}

/// The curated voice palette for a cloud TTS provider id, as an owned
/// runtime [`Palette`](crate::voice_palette::Palette). Empty when the id has
/// no catalogue TTS entry or no curated voices.
#[must_use]
pub fn tts_palette(id: &str) -> crate::voice_palette::Palette {
    find(id)
        .and_then(|p| p.tts.as_ref())
        .map(|t| crate::voice_palette::Palette::from_entries(t.voices))
        .unwrap_or_default()
}

/// The [`VoiceDiscovery`] descriptor for a cloud TTS provider id, if it
/// exposes an enumerable voice list. `None` ⇒ the provider has no
/// listable catalogue (discovery is skipped, never an error).
#[must_use]
pub fn tts_discovery(id: &str) -> Option<VoiceDiscovery> {
    find(id).and_then(|p| p.tts.as_ref()).and_then(|t| t.discovery)
}

/// Construct a `(stt, polish)` pair from a catalogue entry, mapping the
/// entry id back through [`parse_stt_backend`] /
/// [`parse_polish_backend`]. Returns `None` if the entry lacks either
/// capability or the id doesn't round-trip through the parsers.
#[must_use]
pub fn cloud_pair_from_catalog(id: &str) -> Option<(SttBackend, PolishBackend)> {
    let entry = find(id)?;
    if entry.stt.is_none() || entry.polish.is_none() {
        return None;
    }
    let stt_backend = parse_stt_backend(entry.id)?;
    let polish_backend = parse_polish_backend(entry.id)?;
    Some((stt_backend, polish_backend))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AssistantBackend, TtsBackend};
    use crate::providers::{
        assistant_backend_str, assistant_key_env, parse_assistant_backend, parse_tts_backend,
        polish_backend_str, polish_key_env, stt_backend_str, stt_key_env, tts_backend_str,
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
            if p.polish.is_some() {
                if let Some(b) = parse_polish_backend(p.id) {
                    let expected = polish_key_env(&b);
                    if !expected.is_empty() {
                        assert_eq!(p.key_env, expected, "LLM key_env mismatch for {}", p.id);
                    }
                }
            }
            if p.assistant.is_some() {
                if let Some(b) = parse_assistant_backend(p.id) {
                    let expected = assistant_key_env(&b);
                    if !expected.is_empty() {
                        assert_eq!(p.key_env, expected, "Assistant key_env mismatch for {}", p.id);
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
            if p.polish.is_some() {
                assert!(
                    parse_polish_backend(p.id).is_some(),
                    "{} claims LLM but parse_polish_backend rejects its id",
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
            if let Some(b) = parse_polish_backend(p.id) {
                assert_eq!(polish_backend_str(&b), p.id);
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
    /// `SttBackend` / `PolishBackend` / `AssistantBackend` / `TtsBackend`
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
        for b in crate::providers::all_polish_backends() {
            if matches!(b, PolishBackend::Local | PolishBackend::None | PolishBackend::Ollama) {
                continue;
            }
            let id = polish_backend_str(&b);
            assert!(
                CLOUD_PROVIDERS.iter().any(|p| p.id == id && p.polish.is_some()),
                "PolishBackend::{b:?} ({id}) is not present in CLOUD_PROVIDERS",
            );
        }
        for b in crate::providers::all_assistant_backends() {
            if matches!(b, AssistantBackend::None | AssistantBackend::Ollama) {
                continue;
            }
            let id = assistant_backend_str(&b);
            assert!(
                CLOUD_PROVIDERS.iter().any(|p| p.id == id && p.assistant.is_some()),
                "AssistantBackend::{b:?} ({id}) is not present in CLOUD_PROVIDERS",
            );
        }
        for b in crate::providers::all_tts_backends() {
            // None / Wyoming / Local are not cloud providers.
            if matches!(b, TtsBackend::None | TtsBackend::Wyoming | TtsBackend::Local) {
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
            if let Some(l) = &p.polish {
                assert!(!l.model.is_empty(), "{}: empty LLM model", p.id);
                count += 1;
            }
            if let Some(a) = &p.assistant {
                assert!(!a.text_model.is_empty(), "{}: empty assistant text_model", p.id);
                if let Some(mm) = a.multimodal_model {
                    assert!(!mm.is_empty(), "{}: empty multimodal_model literal", p.id);
                }
                if let Some(rt) = &a.realtime {
                    assert!(!rt.model.is_empty(), "{}: empty realtime model", p.id);
                    assert!(
                        rt.ws_url.starts_with("wss://"),
                        "{}: realtime ws_url must be wss://",
                        p.id
                    );
                    assert!(rt.input_sample_rate > 0, "{}: zero realtime input rate", p.id);
                    assert!(rt.output_sample_rate > 0, "{}: zero realtime output rate", p.id);
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

    /// Realtime invariants: any provider advertising a realtime profile
    /// must carry the `Realtime` badge (so the wizard can surface it), and
    /// Gemini specifically must expose a Gemini Live profile.
    #[test]
    fn realtime_profiles_are_consistent() {
        let mut saw_gemini_realtime = false;
        for p in CLOUD_PROVIDERS {
            let Some(a) = &p.assistant else { continue };
            if let Some(rt) = &a.realtime {
                assert!(
                    a.badges.contains(&Badge::Realtime),
                    "{}: has a realtime profile but no Badge::Realtime",
                    p.id
                );
                if p.id == "gemini" {
                    saw_gemini_realtime = true;
                    assert_eq!(rt.protocol, RealtimeProtocol::GeminiLive);
                }
            } else {
                assert!(
                    !a.badges.contains(&Badge::Realtime),
                    "{}: carries Badge::Realtime but has no realtime profile",
                    p.id
                );
            }
        }
        assert!(saw_gemini_realtime, "gemini lost its realtime profile");
    }

    /// Every entry must declare at least one capability — an entry
    /// with no STT/LLM/Assistant/TTS is meaningless.
    #[test]
    fn every_entry_has_a_capability() {
        for p in CLOUD_PROVIDERS {
            assert!(
                p.stt.is_some() || p.polish.is_some() || p.assistant.is_some() || p.tts.is_some(),
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
            assert!(!seen.contains(&p.id), "duplicate catalogue entry for id {}", p.id);
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
                // Web search is intentionally None until the OpenAI
                // client migrates to the Responses API; the
                // chat/completions API rejects the
                // `web_search_preview` tool descriptor.
                WebSearchSupport::None,
            ),
            (
                "anthropic",
                Some("claude-haiku-4-5-20251001"),
                WebSearchSupport::NativeTool("web_search_20250305"),
            ),
            // Groq: no multimodal model in the catalogue today — the
            // previously advertised `llama-4-maverick-17b-128e-instruct`
            // returned 404 from Groq and was removed.
            ("groq", None, WebSearchSupport::None),
            ("cerebras", None, WebSearchSupport::None),
            ("openrouter", None, WebSearchSupport::None),
        ];
        for (id, mm, ws) in cases {
            let entry = find(id).unwrap_or_else(|| panic!("missing catalogue entry for {id}"));
            let adef = entry.assistant.unwrap_or_else(|| panic!("{id} has no assistant defaults"));
            assert_eq!(adef.multimodal_model, *mm, "multimodal_model drift for {id}");
            assert_eq!(adef.web_search, *ws, "web_search drift for {id}");
        }
    }

    /// Every entry that exposes a real (non-stub) capability must carry
    /// `key_validation` so the wizard never falls back to the
    /// "no validation endpoint configured" path. The three unwired STT
    /// stubs (azure/google/nemotron) are explicitly allowed `None`.
    #[test]
    fn key_validation_present_for_wired_providers() {
        const UNWIRED_STUBS: &[&str] = &["azure", "google", "nemotron"];
        for p in CLOUD_PROVIDERS {
            if UNWIRED_STUBS.contains(&p.id) {
                assert!(
                    p.key_validation.is_none(),
                    "{} is an unwired stub and should not carry key_validation",
                    p.id
                );
                continue;
            }
            let has_capability =
                p.stt.is_some() || p.polish.is_some() || p.assistant.is_some() || p.tts.is_some();
            assert!(has_capability, "{} declares no capability", p.id);
            assert!(
                p.key_validation.is_some(),
                "{} exposes a capability but has no key_validation metadata",
                p.id
            );
        }
    }

    /// Pin the `english_only` flag per TTS provider so a casual catalogue
    /// edit can't silently flip a multilingual voice to English-only (which
    /// would needlessly route foreign text to the local engine) or vice
    /// versa (which would resurface the gibberish bug). Update this test
    /// together with the corresponding doc note in `docs/providers.md`.
    #[test]
    fn tts_english_only_pinned() {
        let cases: &[(&str, bool)] = &[
            ("openai", false),
            ("groq", true),
            ("openrouter", false),
            ("deepgram", false),
            ("cartesia", false),
            ("speechmatics", true),
            ("elevenlabs", false),
        ];
        for (id, expected) in cases {
            let entry = find(id).unwrap_or_else(|| panic!("missing catalogue entry for {id}"));
            let tts = entry.tts.unwrap_or_else(|| panic!("{id} has no TTS defaults"));
            assert_eq!(tts.english_only, *expected, "english_only drift for {id}");
        }
    }

    /// The `tts_backend_english_only` helper must agree with the catalogue
    /// and resolve non-cloud backends to `false`.
    #[test]
    fn tts_backend_english_only_matches_catalogue() {
        assert!(tts_backend_english_only(&TtsBackend::Groq));
        assert!(tts_backend_english_only(&TtsBackend::Speechmatics));
        assert!(!tts_backend_english_only(&TtsBackend::OpenAI));
        assert!(!tts_backend_english_only(&TtsBackend::Cartesia));
        // Non-cloud backends have no catalogue TTS entry → false.
        assert!(!tts_backend_english_only(&TtsBackend::None));
        assert!(!tts_backend_english_only(&TtsBackend::Wyoming));
        assert!(!tts_backend_english_only(&TtsBackend::Local));
    }

    /// Every TTS palette must be well-formed: unique non-empty ids, and a
    /// non-empty `default_voice` must itself be a palette member (so a
    /// gender filter never hides the documented default).
    #[test]
    fn tts_palettes_are_well_formed() {
        for p in CLOUD_PROVIDERS {
            let Some(tts) = p.tts else { continue };
            let mut seen = std::collections::BTreeSet::new();
            for v in tts.voices {
                assert!(!v.backend_id.is_empty(), "{}: empty palette voice id", p.id);
                assert!(
                    seen.insert(v.backend_id),
                    "{}: duplicate palette voice {}",
                    p.id,
                    v.backend_id
                );
            }
            if !tts.voices.is_empty() && !tts.default_voice.is_empty() {
                assert!(
                    tts.voices.iter().any(|v| v.backend_id == tts.default_voice),
                    "{}: default_voice {} is not in the palette",
                    p.id,
                    tts.default_voice
                );
            }
        }
    }

    /// Pin per-provider palette sizes so a casual catalogue edit can't
    /// silently change the addressable voice set. Update together with the
    /// palette and `docs/providers.md`.
    #[test]
    fn tts_palette_sizes_pinned() {
        let cases: &[(&str, usize)] = &[
            ("openai", 6),
            ("groq", 6),
            ("openrouter", 1),
            ("deepgram", 4),
            ("cartesia", 1),
            ("elevenlabs", 6),
            ("speechmatics", 1),
        ];
        for (id, len) in cases {
            let tts = find(id).unwrap_or_else(|| panic!("missing {id}")).tts.unwrap();
            assert_eq!(tts.voices.len(), *len, "palette size drift for {id}");
            // The runtime palette helper must agree.
            assert_eq!(tts_palette(id).voices().len(), *len, "tts_palette size drift for {id}");
        }
    }
}
