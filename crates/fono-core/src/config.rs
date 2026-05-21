// SPDX-License-Identifier: GPL-3.0-only
//! Fono configuration schema with serde defaults + atomic load/save.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Highest config schema version supported by this binary. Bump when adding
/// breaking fields and add a migration arm in [`Config::migrate`].
pub const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,

    #[serde(default)]
    pub general: General,

    #[serde(default)]
    pub hotkeys: Hotkeys,

    #[serde(default)]
    pub audio: Audio,

    #[serde(default)]
    pub stt: Stt,

    #[serde(default)]
    pub tts: Tts,

    #[serde(default)]
    pub polish: Polish,

    #[serde(default)]
    pub assistant: Assistant,

    #[serde(default, rename = "context_rules")]
    pub context_rules: Vec<ContextRule>,

    #[serde(default)]
    pub overlay: Overlay,

    #[serde(default)]
    pub history: History,

    #[serde(default)]
    pub inject: Inject,

    #[serde(default)]
    pub update: Update,

    /// Live-dictation (streaming) settings. Plan R7.4 / R18.21. The
    /// cargo `interactive` feature gates *compilation* of streaming
    /// code; this block governs whether the daemon turns it on at
    /// runtime when the feature is compiled in.
    #[serde(default)]
    pub interactive: Interactive,

    /// LAN-server settings. Slice 3 of the network plan. When
    /// `[server.wyoming].enabled = true`, the daemon hosts a
    /// Wyoming-protocol STT server on the LAN bound to the active
    /// `Arc<dyn SpeechToText>`. Off by default — Fono is a desktop
    /// dictation tool first, a network service second.
    #[serde(default)]
    pub server: Server,

    /// LAN-discovery (mDNS) settings. Discovery browsing is always on
    /// when the daemon starts successfully; servers advertise themselves
    /// automatically when their `[server.*].enabled` block is true.
    /// This block is only for cosmetic mDNS metadata overrides.
    #[serde(default, skip_serializing_if = "Network::is_default")]
    pub network: Network,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            general: General::default(),
            hotkeys: Hotkeys::default(),
            audio: Audio::default(),
            stt: Stt::default(),
            tts: Tts::default(),
            polish: Polish::default(),
            assistant: Assistant::default(),
            context_rules: Vec::new(),
            overlay: Overlay::default(),
            history: History::default(),
            inject: Inject::default(),
            update: Update::default(),
            interactive: Interactive::default(),
            server: Server::default(),
            network: Network::default(),
        }
    }
}

fn default_version() -> u32 {
    CURRENT_VERSION
}

impl Config {
    /// True when the live streaming-preview pipeline should run for
    /// dictation and the assistant. Single source of truth: it's on
    /// iff the user picked the `Transcript` overlay style. The four
    /// passive visualisation styles (Bars, Oscilloscope, FFT,
    /// Heatmap) keep the daemon on the batch path.
    #[must_use]
    pub fn live_preview(&self) -> bool {
        self.overlay.style.requires_streaming()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct General {
    /// BCP-47 codes restricting which languages whisper / cloud STT
    /// is allowed to consider:
    ///
    /// * empty (default) — unconstrained Whisper auto-detect;
    /// * one entry — forced single language (e.g. `["en"]`);
    /// * two or more — constrained auto-detect: Whisper picks from
    ///   this set and **bans** every other language.
    ///
    /// See `docs/providers.md` and ADR 0016. Cloud STT enforces this
    /// best-effort via post-validation; see
    /// [`General::cloud_rerun_on_language_mismatch`].
    #[serde(default)]
    pub languages: Vec<String>,
    pub startup_autostart: bool,
    pub auto_mute_system: bool,
    /// Keep the cpal input stream open continuously feeding a discarded
    /// buffer; on `StartRecording` flip a flag rather than open a new
    /// stream. Saves 50–300 ms cold-start on ALSA/PipeWire. Latency
    /// plan L1. Off by default for privacy until the wizard surfaces
    /// explicit consent — see `docs/privacy.md`.
    pub always_warm_mic: bool,
    /// After every successful pipeline, also place the cleaned/raw text
    /// on the system clipboard as a belt-and-suspenders safety net.
    /// Robust against KDE Wayland where `wtype` exits 0 but doesn't
    /// actually deliver keys to the focused window. Default `true`.
    pub also_copy_to_clipboard: bool,
    /// Cloud STT only: when the provider returns a banned language
    /// **and** the in-memory language cache holds a previously-
    /// observed peer code for this backend, re-issue the request
    /// with that code forced. Cold-start (empty cache) accepts the
    /// unforced response and lets the cache populate from the next
    /// correct detection. Default `true` (plan v3); set to `false`
    /// to skip the rerun unconditionally for cost-sensitive setups.
    pub cloud_rerun_on_language_mismatch: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            languages: Vec::new(),
            startup_autostart: false,
            auto_mute_system: true,
            always_warm_mic: false,
            also_copy_to_clipboard: true,
            cloud_rerun_on_language_mismatch: true,
        }
    }
}

impl General {
    /// Per-call `lang` override for the STT trait's
    /// `transcribe(... lang: Option<&str>)`. Always returns `None` so
    /// the backend uses its allow-list + rerun-target cache regardless
    /// of how many languages are configured.
    ///
    /// The old behaviour — returning `Some(code)` for a single-entry
    /// `languages` list — turned every request into a hard force, which
    /// caused English audio to be transcribed as Romanian (or any other
    /// sole configured language). Single-entry allow-lists are now
    /// treated identically to multi-entry ones: the cloud provider
    /// auto-detects, and a rerun fires only when the detection is
    /// outside the list and the cache holds a prior peer code.
    #[must_use]
    pub fn language_override(&self) -> Option<&str> {
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    /// Dictation hotkey. Auto-detects toggle vs push-to-talk based on
    /// press duration: a short press (< 1 s) toggles recording, a long
    /// press holds — release stops capture.
    pub dictation: String,
    pub cancel: String,
    /// Voice-assistant key. Empty disables the assistant hotkey
    /// entirely (the daemon won't register the key, the FSM never
    /// enters the assistant states). Distinct from `dictation` so the
    /// user can dictate into a focused window and ask the assistant
    /// in the same session without changing modes. Same auto short/
    /// long-press behaviour as `dictation`.
    #[serde(default = "default_assistant_hotkey")]
    pub assistant: String,
}

fn default_assistant_hotkey() -> String {
    "F8".into()
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            dictation: "F7".into(),
            cancel: "Escape".into(),
            assistant: default_assistant_hotkey(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    pub sample_rate: u32,
    pub vad_backend: String,
    /// Trim leading/trailing silence before passing audio to STT.
    /// Latency plan L11/L12 — whisper compute scales linearly with
    /// audio length so this saves real wall-clock time.
    pub trim_silence: bool,
    /// In toggle mode, fire StopRecording automatically when this many
    /// milliseconds of contiguous silence are detected. `0` disables.
    /// Latency plan L13.
    pub auto_stop_silence_ms: u32,
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            vad_backend: "silero".into(),
            trim_silence: true,
            auto_stop_silence_ms: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stt {
    pub backend: SttBackend,
    pub local: SttLocal,
    #[serde(default)]
    pub cloud: Option<SttCloud>,
    /// LAN Wyoming server (e.g. `wyoming-faster-whisper`, another
    /// `fono serve wyoming` instance, Rhasspy). Optional — populated
    /// either by `fono use stt wyoming --uri …` or by clicking a
    /// discovered peer in the tray menu (Slice 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wyoming: Option<SttWyoming>,
    /// Optional initial prompts keyed by BCP-47 alpha-2 language code
    /// (e.g. `"en"`, `"ro"`). Sent to Whisper as `initial_prompt`
    /// (local) / `prompt` (cloud) when the resolved language matches a
    /// key. A short prompt biases Whisper away from training-corpus
    /// closers ("Thank you for watching") without affecting accent
    /// or vocabulary; mismatched languages can mislead the language
    /// classifier so we only send a prompt once the language is known.
    /// Out of the box this map is empty; English-only local models
    /// fall back to a built-in default prompt for `en` audio.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub prompts: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SttBackend {
    Local,
    Groq,
    Deepgram,
    OpenAI,
    Cartesia,
    AssemblyAI,
    Azure,
    Speechmatics,
    Google,
    Nemotron,
    /// OpenRouter — proxies OpenAI-compatible
    /// `POST /v1/audio/transcriptions` to upstream providers
    /// (Groq Whisper, Google Chirp, …). Selects the route via the
    /// `model` field, e.g. `openai/whisper-large-v3-turbo`.
    OpenRouter,
    /// Wyoming-protocol speech-to-text server on the LAN
    /// (`wyoming-faster-whisper`, `fono serve wyoming`, Rhasspy, etc.).
    /// Configure via `[stt.wyoming]`.
    Wyoming,
}

impl Default for SttBackend {
    fn default() -> Self {
        Self::Local
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SttLocal {
    pub model: String,
    pub quantization: String,
    /// Optional per-backend allow-list override. When non-empty,
    /// overrides [`General::languages`] for the local Whisper backend
    /// only. Most users configure the list once on `[general]`; this
    /// is for power users who run mixed STT setups.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    /// Whisper inference thread count. `0` = auto-detect physical
    /// cores (avoids SMT thrash). Latency plan L18.
    pub threads: u32,
}

impl Default for SttLocal {
    fn default() -> Self {
        Self {
            model: "small".into(),
            quantization: "auto".into(),
            languages: Vec::new(),
            threads: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttCloud {
    pub provider: String,
    pub api_key_ref: String,
    pub model: String,
}

/// `[stt.wyoming]` — coordinates of a Wyoming-protocol STT server on
/// the LAN. Slice 2 of the network plan. The URI accepts
/// `tcp://host:port`, bare `host:port`, or just `host` (default port
/// 10300).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SttWyoming {
    /// `tcp://host:port`, `host:port`, or `host`.
    pub uri: String,
    /// Optional model hint (`transcribe.name` on the wire). Empty =
    /// server picks default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Optional pre-shared bearer token reference (resolved through
    /// `secrets.toml` / env). Empty = no auth (Wyoming v1 has no
    /// in-band auth; Fono will gain an extension event in Slice 5).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_token_ref: String,
}

/// `[tts]` — text-to-speech for the voice-assistant path. Off by
/// default (`backend = none`); enabling it requires either a Wyoming
/// TTS server on the LAN or a cloud TTS API. The TTS pipeline is
/// fully independent of `[stt]` / `[polish]`: a user can dictate with
/// cloud STT + local cleanup and run the assistant against a
/// different cloud + Wyoming TTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Tts {
    pub backend: TtsBackend,
    /// Voice override. Empty = backend default. Wyoming voices look
    /// like `en_US-amy-low`; OpenAI voices are `alloy`/`echo`/etc.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice: String,
    /// cpal output device name. Empty = system default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output_device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<TtsCloud>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wyoming: Option<TtsWyoming>,
}

impl Default for Tts {
    fn default() -> Self {
        Self {
            backend: TtsBackend::None,
            voice: String::new(),
            output_device: String::new(),
            cloud: None,
            wyoming: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TtsBackend {
    None,
    /// Wyoming-protocol TTS server on the LAN (e.g. `wyoming-piper`).
    /// Configure via `[tts.wyoming]`.
    Wyoming,
    /// OpenAI `/v1/audio/speech` API. Configure via `[tts.cloud]`.
    OpenAI,
    /// Groq's OpenAI-compatible `/audio/speech` endpoint (Orpheus TTS).
    Groq,
    /// OpenRouter's OpenAI-compatible `/audio/speech` endpoint
    /// (defaults to Kokoro).
    OpenRouter,
    /// Cartesia's bespoke `/tts/bytes` endpoint (Sonic TTS).
    Cartesia,
    /// Deepgram's `/v1/speak` endpoint (Aura TTS).
    Deepgram,
}

impl Default for TtsBackend {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TtsCloud {
    /// Provider identifier (`openai`, future: `elevenlabs`, ...).
    pub provider: String,
    /// Reference into `secrets.toml` / environment for the API key.
    /// Empty falls through to the canonical env var
    /// (e.g. `OPENAI_API_KEY`) so existing dictation keys work for TTS
    /// too without reconfiguration.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key_ref: String,
    /// Model override. Empty = factory default (e.g. `tts-1`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

/// `[tts.wyoming]` — coordinates of a Wyoming-protocol TTS server.
/// Mirrors [`SttWyoming`]. Default port is 10200 (the wyoming-piper
/// default), distinct from STT's 10300.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TtsWyoming {
    pub uri: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_token_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Polish {
    pub enabled: bool,
    pub backend: PolishBackend,
    pub local: PolishLocal,
    pub cloud: Option<PolishCloud>,
    pub prompt: Prompt,
    /// Skip the polish roundtrip when the raw STT output has
    /// fewer than this many words (whitespace-split). 0 = never skip.
    /// Latency plan L9 — for short utterances (chat, search bars,
    /// quick push-to-talk taps) the LLM costs more than it cleans, and
    /// chat-trained models (cloud or local alike) are also more likely
    /// to misinterpret a short fragment as a question and respond with
    /// a clarification ("Could you provide the full text?") instead of
    /// a cleaned transcript. The default of 3 keeps the cleanup pass
    /// for any sentence-shaped utterance while short-circuiting one-
    /// and two-word captures regardless of which backend is active.
    pub skip_if_words_lt: u32,
}

impl Default for Polish {
    fn default() -> Self {
        Self {
            // Disabled by default until the user opts into a cloud
            // provider via `fono setup`, or compiles in `llama-local`
            // and configures a model. Avoids "first dictation crashes
            // because LlamaLocal is a stub" trap.
            enabled: false,
            backend: PolishBackend::None,
            local: PolishLocal::default(),
            cloud: None,
            prompt: Prompt::default(),
            skip_if_words_lt: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PolishBackend {
    Local,
    None,
    OpenAI,
    Anthropic,
    Gemini,
    Groq,
    Cerebras,
    OpenRouter,
    Ollama,
}

impl Default for PolishBackend {
    fn default() -> Self {
        Self::Local
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PolishLocal {
    pub model: String,
    pub quantization: String,
    pub context: u32,
}

impl Default for PolishLocal {
    fn default() -> Self {
        Self { model: "qwen2.5-1.5b-instruct".into(), quantization: "q4_k_m".into(), context: 4096 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolishCloud {
    pub provider: String,
    pub api_key_ref: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prompt {
    pub main: String,
    pub advanced: String,
    pub dictionary: Vec<String>,
}

impl Default for Prompt {
    fn default() -> Self {
        Self {
            main: default_prompt_main().into(),
            advanced: default_prompt_advanced().into(),
            dictionary: Vec::new(),
        }
    }
}

/// Baked-in default main prompt (Phase 5 Task 5.5).
///
/// The hard rules at the top exist to stop chat-trained LLMs (cloud or
/// local: Cerebras / Groq Llama-3.3-70B, gpt-4o-mini, Claude Haiku, the
/// local llama.cpp backend, …) from responding with clarification
/// questions like *"Could you provide the full text you're referring
/// to?"* on short or ambiguous captures. The failure mode is a property
/// of chat fine-tuning, not of any specific provider, so this prompt is
/// applied identically across every `TextFormatter` impl. Tested
/// against the bug report in
/// `plans/2026-04-28-polish-cleanup-clarification-refusal-fix-v1.md`.
pub const fn default_prompt_main() -> &'static str {
    "You are a transcription cleanup post-processor, not a chat assistant. The user message \
between the <<< and >>> markers is a raw speech-to-text transcript. Your only job is to \
return that transcript with filler words removed (um, uh, like), proper punctuation and \
capitalization added, and obvious stutters collapsed. Preserve the speaker's language and \
tone exactly — do not translate, summarise, explain, or add content.\n\n\
Hard rules:\n\
- Output ONLY the cleaned transcript text. No quotes, no markdown, no preamble, no commentary.\n\
- NEVER ask the user for clarification or more context. NEVER respond with a question, an \
apology, or a meta-comment about the input.\n\
- If the transcript is short, ambiguous, a single word, empty, or already clean, return it \
verbatim (with at most punctuation/capitalization fixes). Do not invent missing content.\n\
- Do not include the <<< or >>> markers in your output."
}

/// Baked-in default advanced prompt (Phase 5 Task 5.5).
pub const fn default_prompt_advanced() -> &'static str {
    "If the speaker self-corrects (\"scratch that\", \"I mean\", \"no wait\"), apply the \
correction and drop the discarded fragment. If the speaker dictates a list (\"first\", \
\"second\", \"next point\"), format it as a bulleted or numbered list. If the speaker names \
a term in the personal dictionary, prefer that exact spelling."
}

/// `[assistant]` — voice-assistant chat config. Distinct from `[polish]`
/// (the dictation cleanup pipeline) so a user can run a fast local
/// model for cleanup and a bigger cloud model for the assistant
/// (or vice versa). Off by default until the user opts in via the
/// wizard or `fono use assistant <backend>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Assistant {
    pub enabled: bool,
    pub backend: AssistantBackend,
    pub local: AssistantLocal,
    pub cloud: Option<AssistantCloud>,
    /// System prompt sent on every turn. Distinct from `[polish].prompt`
    /// — the cleanup prompt forbids chat-style replies, this one
    /// invites them, capped to 1-3 sentences for low TTS latency.
    pub prompt_main: String,
    /// Rolling-history time window. Turns older than this from the
    /// most recent activity are pruned on every snapshot.
    pub history_window_minutes: u32,
    /// Belt-and-suspenders cap on the number of turns retained,
    /// independent of the time window. Caps token cost on long
    /// idle-then-resume flows.
    pub history_max_turns: u32,
    /// When the user presses the dictation key, clear the assistant's
    /// rolling history. Default `true`: dictation and assistant are
    /// separate intents; mixing their contexts is rarely what the user
    /// wants.
    pub auto_clear_on_dictation: bool,
    /// When `true`, the assistant builder swaps the catalogue's
    /// `text_model` for `multimodal_model` if the provider exposes
    /// one (e.g. Claude Haiku 4.5, Llama-4 Maverick on Groq).
    /// Defaults to `true` via `Assistant::default()`.
    pub prefer_vision: bool,
    /// When `true`, the assistant's request builder appends the
    /// provider's native web-search tool to the `tools` field on
    /// every turn (OpenAI `web_search_preview`, Anthropic
    /// `web_search_20250305`). No-op for
    /// providers whose catalogue entry says `WebSearchSupport::None`.
    /// Defaults to `false` until OpenAI's chat/completions gains a
    /// stable tool descriptor for web search (today it's a Responses
    /// API feature). Anthropic's `web_search_20250305` tool works on
    /// the Messages API.
    pub prefer_web_search: bool,
}

impl Default for Assistant {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: AssistantBackend::None,
            local: AssistantLocal::default(),
            cloud: None,
            prompt_main: default_assistant_prompt().into(),
            history_window_minutes: 5,
            history_max_turns: 12,
            auto_clear_on_dictation: true,
            prefer_vision: true,
            prefer_web_search: false,
        }
    }
}

/// Backend selector for the assistant. Same provider set as
/// [`PolishBackend`] minus a shape change: assistant defaults to `None`
/// (off) rather than `Local`, because turning on the assistant
/// without picking a backend would be a footgun.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AssistantBackend {
    None,
    OpenAI,
    Anthropic,
    Groq,
    Cerebras,
    OpenRouter,
    Ollama,
}

impl Default for AssistantBackend {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AssistantLocal {
    pub model: String,
    pub quantization: String,
    pub context: u32,
}

impl Default for AssistantLocal {
    fn default() -> Self {
        // A 3B-class chat model is the floor for usable assistant
        // quality; 1.5B (the cleanup default) tends to ramble.
        Self { model: "qwen2.5-3b-instruct".into(), quantization: "q4_k_m".into(), context: 8192 }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AssistantCloud {
    pub provider: String,
    pub api_key_ref: String,
    pub model: String,
}

/// Default chat-style system prompt for the voice assistant. Designed
/// for low time-to-first-audio: short answers, plain prose, no
/// markdown / lists / code that the TTS layer would have to skip.
pub const fn default_assistant_prompt() -> &'static str {
    "You are a concise voice assistant. Reply in 1-3 sentences unless the user explicitly asks \
for detail. Spoken plain prose only — no markdown, no bullet lists, no code blocks, no headings. \
If you would normally include code or a structured list, describe it briefly in spoken language \
instead. Match the user's language."
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextRule {
    #[serde(default)]
    pub match_: ContextMatch,
    #[serde(default)]
    pub prompt_suffix: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextMatch {
    #[serde(default)]
    pub window_class: Option<String>,
    #[serde(default)]
    pub window_title_regex: Option<String>,
}

/// Overlay visualisation style. Drives both the standalone audio-vis
/// overlay (Bars / Oscilloscope / FFT / Heatmap) and the live-
/// transcript preview panel (`Transcript`). Picking `Transcript` is
/// what enables the streaming pipeline end-to-end — there is no
/// separate "interactive" master toggle.
///
/// The transcript style costs more CPU (local Whisper) or more API
/// requests (cloud STT) than the four waveform styles, which only
/// drive a passive level/spectrum visualisation off the recording
/// buffer. See `docs/interactive.md` for the cost shape per backend.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WaveformStyle {
    /// Scrolling amplitude bars; bars glow brighter at higher amplitude.
    Bars,
    /// Connected-line waveform drawn from raw PCM samples.
    Oscilloscope,
    /// Vertical spectrum bars from a real-input FFT — current frame
    /// only.
    Fft,
    /// Rolling spectrogram (frequency on Y, time on X, magnitude as
    /// colour).
    Heatmap,
    /// Live streaming transcription preview. Renders the words the
    /// STT backend is producing in realtime, replacing them with the
    /// finalised text as each segment closes. Requires a streaming-
    /// capable STT backend (local Whisper, or Groq); cloud backends
    /// without a streaming impl (OpenAI / Anthropic / Cerebras /
    /// OpenRouter) fall back to a one-line placeholder during
    /// recording and the final transcript on inject.
    ///
    /// Heavier than the other styles: local Whisper re-decodes the
    /// trailing audio window every `chunk_ms_steady` ms (CPU /
    /// GPU); cloud backends re-POST the trailing window at
    /// `streaming_interval` cadence (API tokens / requests).
    Transcript,
}

impl Default for WaveformStyle {
    fn default() -> Self {
        // FFT reads as the most "active" of the four passive styles
        // during both recording (real spectrum) and assistant-thinking
        // (sweeping bell scanner with crisp inter-bar gaps), so it
        // makes the strongest first-run impression. Users can switch
        // to Bars/Oscilloscope/Heatmap or to the heavier Transcript
        // style from the tray submenu or `[overlay].style`.
        Self::Fft
    }
}

impl WaveformStyle {
    /// True when this style requires the streaming STT pipeline to
    /// produce text in realtime. Only `Transcript` returns `true`;
    /// the four passive visualisations only need PCM / FFT taps off
    /// the recording buffer.
    #[must_use]
    pub fn requires_streaming(self) -> bool {
        matches!(self, Self::Transcript)
    }
}

/// Overlay panel configuration.
///
/// `waveform` toggles the overlay window at all; `style` selects one
/// of five visualisations including the live `Transcript` preview;
/// `volume_bar` adds a thin vertical VU meter on the right side of
/// the transcript panel. All fields are silently ignored on builds
/// without the `real-window` / `waveform` features compiled in
/// (server / headless).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Overlay {
    /// Show the overlay window during recording. When `false`, the
    /// daemon runs headless regardless of `style`.
    pub waveform: bool,
    /// Visualisation style. Picking `Transcript` enables the
    /// streaming live-preview pipeline; the other four styles drive
    /// passive audio visualisations off the recording buffer.
    pub style: WaveformStyle,
    /// Right-side VU bar on the transcript panel. Default on; set to
    /// `false` to restore the pre-VU layout.
    pub volume_bar: bool,
}

impl Default for Overlay {
    fn default() -> Self {
        Self { waveform: true, style: WaveformStyle::default(), volume_bar: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub enabled: bool,
    pub retention_days: u32,
    pub redact_secrets: bool,
}

impl Default for History {
    fn default() -> Self {
        Self { enabled: true, retention_days: 90, redact_secrets: true }
    }
}

/// `[inject]` — text-injection tuning. Currently empty after the
/// removal of the X11 `xtest-paste` backend (which had a configurable
/// paste shortcut). Retained as a stable section header so future
/// per-app paste rules and backend overrides can land here without
/// breaking existing config files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Inject {}

/// Background update-check settings. The daemon spawns a worker that
/// hits the GitHub Releases API on the configured cadence and surfaces
/// results in the tray menu. All knobs are opt-out — the privacy-
/// conscious user can disable the entire feature with one flag, the
/// `FONO_NO_UPDATE_CHECK=1` env var, or by sticking to a distro
/// package (which auto-disables self-replace anyway).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Update {
    /// Run a one-shot update check on daemon start. Default `true`.
    /// The check fires once ~10 s after launch and never again until
    /// the next start — fono is a desktop tool that's started often
    /// enough that a recurring timer would just add noise without
    /// catching releases any sooner. Disable to suppress the GitHub
    /// API request entirely.
    pub auto_check: bool,
    /// `"stable"` (default) or `"prerelease"`. Prerelease enumerates
    /// every release including drafts/RCs.
    pub channel: String,
}

impl Default for Update {
    fn default() -> Self {
        Self { auto_check: true, channel: "stable".into() }
    }
}

/// LAN-server settings. Slice 3 of
/// `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`.
///
/// Holds the `[server.wyoming]` block today; gains `[server.fono]` in
/// Slice 6 (the WebSocket-based Fono-native server). All flags are
/// off by default — Fono only listens on a network socket if the user
/// explicitly opts in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Server {
    /// Wyoming-protocol STT server. Hosts the active
    /// `Arc<dyn SpeechToText>` so Home Assistant satellites and other
    /// Wyoming peers can route inference through this daemon.
    pub wyoming: ServerWyoming,
}

/// `[server.wyoming]` — coordinates of the LAN Wyoming server. The
/// listener is **only** spawned when `enabled = true`. `bind` is the
/// exposure control: keep the default loopback address for local-only
/// serving, set `0.0.0.0` / `::` for all interfaces, or set a specific
/// interface address to serve only that network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ServerWyoming {
    /// Master switch. Default `false`.
    pub enabled: bool,
    /// Bind address. Default `"127.0.0.1"` (loopback only). Set to
    /// `"0.0.0.0"` to accept LAN peers on every interface, or to a
    /// specific interface address (`"192.168.1.5"`) to bind one NIC.
    pub bind: String,
    /// TCP port. Default `10300` (the de-facto Wyoming port).
    pub port: u16,
    /// Optional pre-shared bearer token reference, resolved through
    /// `secrets.toml` / env. Empty = no auth (Wyoming v1 has no in-band
    /// auth; the Fono-native protocol gains it in Slice 5).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_token_ref: String,
}

impl Default for ServerWyoming {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "127.0.0.1".to_string(),
            port: 10_300,
            auth_token_ref: String::new(),
        }
    }
}

/// `[network]` — optional mDNS / DNS-SD metadata overrides. Discovery
/// browsing is always enabled while the daemon is running. Advertising is
/// automatic for enabled `[server.*]` blocks, so there is no user-facing
/// discovery on/off switch.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Network {
    /// Friendly instance-name override. Empty (default) ⇒ derive from
    /// `hostname` at startup (`fono-<hostname>`). mDNS guarantees
    /// uniqueness per service type per LAN even when several hosts
    /// pick the same friendly name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance_name: String,
}

impl Network {
    #[must_use]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// Live-dictation streaming-pipeline tuning. Plan R7.4 / R18.21.
///
/// Whether the streaming pipeline runs at all is gated by
/// `[overlay].style = "transcript"` (see [`WaveformStyle`]); this
/// block only carries the per-knob tuning that applies once it is
/// running. When the cargo `interactive` feature is **not** compiled
/// in, the block is parsed but ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Interactive {
    /// Per-minute spending ceiling, in USD micro-cents (1¢ = 10_000 µ¢).
    /// `0` disables the budget controller entirely (default — local STT
    /// is free). Cloud streaming sets a sensible default at wizard time.
    pub budget_ceiling_per_minute_umicros: u64,
    /// Quality floor under budget pressure. `"max"` (default) never
    /// skips finalize; `"balanced"` may slow preview cadence;
    /// `"aggressive"` may skip finalize on high-confidence segments.
    pub quality_floor: String,
    // ----- v6 carryover knobs (R7.4 / R9.1) ---------------------------
    /// Pipeline mode. `"hybrid"` (default) uses streaming preview +
    /// finalize-on-segment-boundary + cleanup-on-finalize. Reserved
    /// for Slice B variants (`"streaming-only"`, `"batch"`).
    pub mode: String,
    /// Initial chunk window the streaming decoder waits before its
    /// first preview pass, in milliseconds. Smaller = lower TTFF,
    /// noisier early previews.
    pub chunk_ms_initial: u32,
    /// Steady-state chunk window between preview passes, in
    /// milliseconds.
    pub chunk_ms_steady: u32,
    /// When `true`, run the polish pass once on the assembled
    /// transcript after the user releases the hotkey. Default `true`.
    pub cleanup_on_finalize: bool,
    /// Hard ceiling on a single live session, in seconds. The daemon
    /// auto-finishes at this cap to bound the budget controller and
    /// the overlay's resident memory.
    pub max_session_seconds: u32,
    /// Optional hard cost cap for cloud-streaming sessions, in USD.
    /// `None` (default) defers to `budget_ceiling_per_minute_umicros`.
    pub max_session_cost_usd: Option<f32>,
    // ----- v7 boundary heuristics (R2.5 / R7.3a / R9.1) ---------------
    /// Engage the prosody-aware chunk-boundary heuristic (R2.5). When
    /// `true`, segment boundaries are delayed up to
    /// `commit_prosody_extend_ms` if the speaker's pitch contour is
    /// flat or rising at the boundary (signal of unfinished thought).
    /// Default `false` until Slice B real-fixture telemetry validates
    /// the heuristic.
    pub commit_use_prosody: bool,
    /// Extension granted by the prosody heuristic when it fires, in
    /// milliseconds. Capped by the session at `chunk_ms_steady * 1.5`.
    pub commit_prosody_extend_ms: u32,
    /// Engage the punctuation-hint chunk-boundary heuristic (R2.5).
    /// When `true`, segment boundaries that would interrupt mid-clause
    /// (preview text ends in `,;:` or alphanumerics — i.e. no terminal
    /// punctuation) are delayed by `commit_punct_extend_ms`. Default
    /// `true`.
    pub commit_use_punctuation_hint: bool,
    /// Extension granted by the punctuation hint when it fires, in
    /// milliseconds.
    pub commit_punct_extend_ms: u32,
    /// At end-of-input (R7.3a), if the trailing word of the committed
    /// transcript is a filler or a syntactically-dangling word, hold
    /// the session open for `eou_drain_extended_ms` to wait for a
    /// continuation. Default `true`.
    pub commit_hold_on_filler: bool,
    /// Filler-word vocabulary checked by `commit_hold_on_filler`.
    /// English-only by default; users dictating in other languages
    /// should override. Comparison is case-insensitive after stripping
    /// trailing `.,;:!?`.
    pub commit_filler_words: Vec<String>,
    /// Syntactically-dangling word vocabulary (conjunctions, articles,
    /// prepositions). English-only by default; see
    /// `commit_filler_words` for the localization caveat.
    pub commit_dangling_words: Vec<String>,
    /// End-of-utterance extended drain window, in milliseconds. The
    /// session waits up to this long for additional voiced PCM after
    /// the upstream stream closes when a filler/dangling suffix is
    /// detected. Has no effect unless `commit_hold_on_filler = true`.
    pub eou_drain_extended_ms: u32,
    /// Reserved for Slice D (R15); inert in Slice A. Future adaptive
    /// EOU detector will replace the static drain window with a
    /// silence-distribution estimator.
    pub eou_adaptive: bool,
    /// Reserved for Slice D (R15); inert in Slice A. Grace window in
    /// milliseconds during which a re-pressed hotkey resumes the prior
    /// session instead of opening a new one.
    pub resume_grace_ms: u32,
    /// Cloud streaming preview cadence, in seconds. Re-POSTs the
    /// trailing audio window at this interval to drive the live
    /// overlay. Default `1.0`.
    ///
    /// Effective range: clamped to `0.5` minimum (anything below
    /// drowns the cloud in requests). Values **greater than `3.0`**
    /// disable the preview lane entirely — Fono only decodes on VAD
    /// segment boundaries (slower feedback, but the cheapest mode for
    /// rate-limited free tiers).
    ///
    /// Worked-out req/min budgets, continuous speech:
    /// - `1.0` ≈ 60 previews/min + ~5 finalizes (paid Groq tier).
    /// - `1.5` ≈ 40 previews/min (mid).
    /// - `2.0` ≈ 30 previews/min (suggested when 429s are observed).
    /// - `> 3.0` ≈ ~5 finalizes/min only (free-tier safe).
    ///
    /// Local Whisper backends ignore this knob (no rate limit).
    pub streaming_interval: f32,
    /// Delay between `LiveHoldReleased` arriving at the session
    /// orchestrator and the actual cpal capture stop, in
    /// milliseconds. Gives cpal's host-side callback buffer time to
    /// drain through the audio bridge so the trailing portion of the
    /// utterance reaches the streaming STT before EOF. Default `300`.
    /// Lower (e.g. `150`) for snappier feel; raise (e.g. `500`) on
    /// audio interfaces with longer internal buffers.
    pub hold_release_grace_ms: u32,
}

impl Default for Interactive {
    fn default() -> Self {
        Self {
            budget_ceiling_per_minute_umicros: 0,
            quality_floor: "max".into(),
            mode: "hybrid".into(),
            chunk_ms_initial: 600,
            chunk_ms_steady: 1500,
            cleanup_on_finalize: true,
            max_session_seconds: 120,
            max_session_cost_usd: None,
            commit_use_prosody: false,
            commit_prosody_extend_ms: 250,
            commit_use_punctuation_hint: true,
            commit_punct_extend_ms: 150,
            commit_hold_on_filler: true,
            commit_filler_words: default_filler_words(),
            commit_dangling_words: default_dangling_words(),
            eou_drain_extended_ms: 1500,
            eou_adaptive: false,
            resume_grace_ms: 0,
            streaming_interval: 1.0,
            hold_release_grace_ms: 150,
        }
    }
}

/// Resolved cadence for the cloud streaming preview lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewCadence {
    /// Steady-state preview cadence in milliseconds. Already clamped
    /// to the `[0.5s, 3.0s]` valid range.
    Interval(u32),
    /// User-configured value > 3.0s: preview lane is disabled and
    /// only VAD-boundary finalize requests are sent.
    DisabledFinalizeOnly,
}

impl Interactive {
    /// Resolve `streaming_interval` to the effective cadence policy.
    /// Clamps below `0.5s`, switches to `DisabledFinalizeOnly` above
    /// `3.0s`. NaN/negative collapses to the default `1.0s`.
    #[must_use]
    pub fn preview_cadence(&self) -> PreviewCadence {
        let s = self.streaming_interval;
        if !s.is_finite() || s <= 0.0 {
            return PreviewCadence::Interval(1000);
        }
        if s > 3.0 {
            return PreviewCadence::DisabledFinalizeOnly;
        }
        let clamped = s.max(0.5);
        PreviewCadence::Interval((clamped * 1000.0) as u32)
    }
}

/// Default English filler-word vocabulary for `commit_hold_on_filler`.
/// Centralised so the equivalence harness can reference the same list.
#[must_use]
pub fn default_filler_words() -> Vec<String> {
    ["um", "uh", "er", "ah", "mm", "like", "you know"].iter().map(|s| (*s).to_string()).collect()
}

/// Default English syntactically-dangling-word vocabulary.
#[must_use]
pub fn default_dangling_words() -> Vec<String> {
    [
        "and", "but", "or", "so", "because", "the", "a", "an", "of", "to", "with", "for", "in",
        "on", "at", "from",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

impl Config {
    /// Is the text-to-speech surface actually usable?
    ///
    /// Onboarding signal — single source of truth for both the
    /// "you haven't configured TTS yet" startup notification and the
    /// tray left-click branch. Returns true iff the configured backend
    /// is something other than [`TtsBackend::None`] **and** the
    /// matching credential / endpoint is reachable:
    ///
    /// * [`TtsBackend::Wyoming`] → `[tts.wyoming].uri` is non-empty.
    /// * Cloud (OpenAI / Groq / OpenRouter / Cartesia / Deepgram) →
    ///   the matching API key resolves from `secrets.toml`.
    #[must_use]
    pub fn tts_configured(&self, secrets: &crate::Secrets) -> bool {
        match self.tts.backend {
            TtsBackend::None => false,
            TtsBackend::Wyoming => {
                self.tts.wyoming.as_ref().is_some_and(|w| !w.uri.trim().is_empty())
            }
            TtsBackend::OpenAI
            | TtsBackend::Groq
            | TtsBackend::OpenRouter
            | TtsBackend::Cartesia
            | TtsBackend::Deepgram => {
                let env = crate::providers::tts_key_env(&self.tts.backend);
                !env.is_empty() && secrets.has_in_file(env)
            }
        }
    }

    /// Load from disk; if the file does not exist, return defaults (caller
    /// may choose to persist them via [`Config::save`]).
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                let mut cfg: Self = toml::from_str(&raw)
                    .map_err(|source| Error::TomlParse { path: path.to_path_buf(), source })?;
                cfg.migrate()?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::Io { path: path.to_path_buf(), source }),
        }
    }

    /// Forward-compat migration: bumps `version` to `CURRENT_VERSION`, adding
    /// arms as the schema evolves.
    pub fn migrate(&mut self) -> Result<()> {
        if self.version > CURRENT_VERSION {
            return Err(Error::ConfigVersionTooNew {
                found: self.version,
                supported: CURRENT_VERSION,
            });
        }

        self.version = CURRENT_VERSION;
        Ok(())
    }

    /// Atomic write via tempfile + rename in the same directory.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|source| Error::Io { path: dir.to_path_buf(), source })?;
        }
        let toml_str = toml::to_string_pretty(self)?;
        atomic_write(path, toml_str.as_bytes(), 0o644)?;
        Ok(())
    }
}

/// Atomically write `data` to `path` with the given Unix mode. On platforms
/// without Unix permissions the mode is ignored.
pub(crate) fn atomic_write(path: &Path, data: &[u8], _mode: u32) -> Result<()> {
    use std::io::Write;

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|source| Error::Io { path: dir.to_path_buf(), source })?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|source| Error::Io { path: dir.to_path_buf(), source })?;
    tmp.write_all(data).map_err(|source| Error::Io { path: tmp.path().to_path_buf(), source })?;
    tmp.as_file_mut()
        .sync_all()
        .map_err(|source| Error::Io { path: tmp.path().to_path_buf(), source })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(_mode))
            .map_err(|source| Error::Io { path: tmp.path().to_path_buf(), source })?;
    }

    tmp.persist(path).map_err(|e| Error::Io { path: PathBuf::from(path), source: e.error })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_default() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let cfg = Config::default();
        cfg.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.version, CURRENT_VERSION);
        assert!(loaded.general.languages.is_empty(), "default = unconstrained auto-detect");
        assert_eq!(loaded.stt.local.model, "small");
        assert_eq!(loaded.polish.local.model, "qwen2.5-1.5b-instruct");
    }

    #[test]
    fn missing_file_returns_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = Config::load(&tmp.path().join("nope.toml")).unwrap();
        assert_eq!(cfg.version, CURRENT_VERSION);
    }

    #[test]
    fn future_version_rejected() {
        let mut cfg = Config { version: CURRENT_VERSION + 42, ..Config::default() };
        assert!(matches!(cfg.migrate(), Err(Error::ConfigVersionTooNew { .. })));
    }

    #[test]
    fn interactive_v7_keys_round_trip() {
        let raw = r#"
            version = 1
            [interactive]
            budget_ceiling_per_minute_umicros = 1000
            quality_floor = "balanced"
            mode = "hybrid"
            chunk_ms_initial = 700
            chunk_ms_steady = 1400
            cleanup_on_finalize = false
            max_session_seconds = 60
            max_session_cost_usd = 0.25
            commit_use_prosody = true
            commit_prosody_extend_ms = 200
            commit_use_punctuation_hint = false
            commit_punct_extend_ms = 100
            commit_hold_on_filler = false
            commit_filler_words = ["uh", "erm"]
            commit_dangling_words = ["and"]
            eou_drain_extended_ms = 2000
            eou_adaptive = true
            resume_grace_ms = 250
        "#;
        let cfg: Config = toml::from_str(raw).expect("parse");
        let i = &cfg.interactive;
        assert_eq!(i.budget_ceiling_per_minute_umicros, 1000);
        assert_eq!(i.quality_floor, "balanced");
        assert_eq!(i.mode, "hybrid");
        assert_eq!(i.chunk_ms_initial, 700);
        assert_eq!(i.chunk_ms_steady, 1400);
        assert!(!i.cleanup_on_finalize);
        assert_eq!(i.max_session_seconds, 60);
        assert!((i.max_session_cost_usd.unwrap() - 0.25).abs() < 1e-6);
        assert!(i.commit_use_prosody);
        assert_eq!(i.commit_prosody_extend_ms, 200);
        assert!(!i.commit_use_punctuation_hint);
        assert_eq!(i.commit_punct_extend_ms, 100);
        assert!(!i.commit_hold_on_filler);
        assert_eq!(i.commit_filler_words, vec!["uh", "erm"]);
        assert_eq!(i.commit_dangling_words, vec!["and"]);
        assert_eq!(i.eou_drain_extended_ms, 2000);
        assert!(i.eou_adaptive);
        assert_eq!(i.resume_grace_ms, 250);
    }

    #[test]
    fn empty_interactive_block_yields_defaults() {
        let raw = "version = 1\n[interactive]\n";
        let cfg: Config = toml::from_str(raw).expect("parse");
        let d = Interactive::default();
        let i = &cfg.interactive;
        assert_eq!(i.mode, d.mode);
        assert_eq!(i.chunk_ms_initial, d.chunk_ms_initial);
        assert_eq!(i.chunk_ms_steady, d.chunk_ms_steady);
        assert_eq!(i.cleanup_on_finalize, d.cleanup_on_finalize);
        assert_eq!(i.max_session_seconds, d.max_session_seconds);
        assert_eq!(i.max_session_cost_usd, d.max_session_cost_usd);
        assert_eq!(i.commit_use_prosody, d.commit_use_prosody);
        assert_eq!(i.commit_prosody_extend_ms, d.commit_prosody_extend_ms);
        assert_eq!(i.commit_use_punctuation_hint, d.commit_use_punctuation_hint);
        assert_eq!(i.commit_punct_extend_ms, d.commit_punct_extend_ms);
        assert_eq!(i.commit_hold_on_filler, d.commit_hold_on_filler);
        assert_eq!(i.commit_filler_words, d.commit_filler_words);
        assert_eq!(i.commit_dangling_words, d.commit_dangling_words);
        assert_eq!(i.eou_drain_extended_ms, d.eou_drain_extended_ms);
        assert_eq!(i.eou_adaptive, d.eou_adaptive);
        assert_eq!(i.resume_grace_ms, d.resume_grace_ms);
        assert_eq!(i.hold_release_grace_ms, 150);
        assert_eq!(i.hold_release_grace_ms, d.hold_release_grace_ms);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("partial.toml");
        std::fs::write(&path, "version = 1\n[general]\nlanguages = [\"ro\"]\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.general.languages, vec!["ro"]);
        assert_eq!(cfg.stt.local.model, "small");
    }

    #[test]
    fn languages_round_trip_serializes_plural_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("multi.toml");
        let mut cfg = Config::default();
        cfg.general.languages = vec!["en".into(), "ro".into(), "fr".into()];
        cfg.save(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.lines().any(|l| l.trim_start().starts_with("language =")),
            "singular scalar must not be serialised: {raw}"
        );
        assert!(raw.contains("languages = ["));
        assert!(
            !raw.contains("[network]"),
            "default discovery settings must not be serialized as user-facing knobs: {raw}"
        );
        let reloaded = Config::load(&path).unwrap();
        assert_eq!(reloaded.general.languages, vec!["en".to_string(), "ro".into(), "fr".into()]);
    }

    #[test]
    fn hotkeys_defaults_are_f7_f8() {
        let h = Hotkeys::default();
        assert_eq!(h.dictation, "F7");
        assert_eq!(h.assistant, "F8");
        assert_eq!(h.cancel, "Escape");
    }

    /// A partial `[assistant]` block (some fields present, others
    /// absent) must populate the missing flags from `Assistant::default()`
    /// via the struct-level `#[serde(default)]`. `prefer_vision`
    /// defaults to `true` (multimodal model = text model on
    /// OpenAI/Anthropic — no API cost); `prefer_web_search` defaults
    /// to `false` until the OpenAI client migrates to the Responses
    /// API (chat/completions hard-rejects the `web_search_preview`
    /// tool descriptor).
    #[test]
    fn partial_assistant_block_defaults_prefer_vision_only() {
        let raw = "version = 1\n[assistant]\nenabled = true\n";
        let cfg: Config = toml::from_str(raw).expect("partial assistant block must parse");
        assert!(cfg.assistant.enabled);
        assert!(cfg.assistant.prefer_vision);
        assert!(!cfg.assistant.prefer_web_search);
    }

    // ----- tts_configured onboarding predicate ------------------------

    #[test]
    fn tts_configured_default_is_false() {
        let cfg = Config::default();
        let secrets = crate::Secrets::default();
        assert!(!cfg.tts_configured(&secrets));
    }

    #[test]
    fn tts_configured_wyoming_requires_non_empty_uri() {
        let mut cfg = Config::default();
        cfg.tts.backend = TtsBackend::Wyoming;
        let secrets = crate::Secrets::default();
        // No wyoming block at all → not configured.
        assert!(!cfg.tts_configured(&secrets));
        // Block present but empty URI → not configured.
        cfg.tts.wyoming = Some(TtsWyoming { uri: String::new(), auth_token_ref: String::new() });
        assert!(!cfg.tts_configured(&secrets));
        // Non-empty URI → configured.
        cfg.tts.wyoming =
            Some(TtsWyoming { uri: "tcp://10.0.0.5:10200".into(), auth_token_ref: String::new() });
        assert!(cfg.tts_configured(&secrets));
    }

    #[test]
    fn tts_configured_cloud_requires_secret() {
        let mut cfg = Config::default();
        cfg.tts.backend = TtsBackend::OpenAI;
        let mut secrets = crate::Secrets::default();
        // Empty secrets → not configured even though the backend is
        // selected.
        assert!(!cfg.tts_configured(&secrets));
        // Secret present → configured.
        secrets.insert("OPENAI_API_KEY", "sk-test");
        assert!(cfg.tts_configured(&secrets));
    }

    #[test]
    fn tts_configured_ignores_env_only_keys() {
        // The onboarding predicate must read secrets.toml only, not
        // the process environment. A stray exported key must not flip
        // the user's onboarding state.
        std::env::set_var("GROQ_API_KEY", "leaky-env-value");
        let mut cfg = Config::default();
        cfg.tts.backend = TtsBackend::Groq;
        let secrets = crate::Secrets::default();
        let configured = cfg.tts_configured(&secrets);
        std::env::remove_var("GROQ_API_KEY");
        assert!(!configured, "env-only GROQ_API_KEY must not satisfy tts_configured");
    }
}
