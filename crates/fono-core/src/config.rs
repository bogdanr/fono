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
    pub llm: Llm,

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            general: General::default(),
            hotkeys: Hotkeys::default(),
            audio: Audio::default(),
            stt: Stt::default(),
            llm: Llm::default(),
            context_rules: Vec::new(),
            overlay: Overlay::default(),
            history: History::default(),
            inject: Inject::default(),
            update: Update::default(),
            interactive: Interactive::default(),
        }
    }
}

fn default_version() -> u32 {
    CURRENT_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct General {
    /// **Deprecated** in favour of [`General::languages`]. Kept for
    /// one release cycle so v1 configs with `language = "ro"` migrate
    /// cleanly. New code should read `languages` exclusively.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
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
    pub sound_feedback: bool,
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
    /// **Deprecated** (plan v3). Cloud STT only: when
    /// [`General::languages`] has > 1 entry, force `fallback_hint()`
    /// on the first request rather than letting the provider
    /// auto-detect. v3 supersedes this with cache-as-rerun-target;
    /// scheduled for removal in v0.5. Default `false`.
    #[deprecated(note = "see plan v3 — superseded by in-memory language cache (lang_cache.rs)")]
    pub cloud_force_primary_language: bool,
    /// Cloud STT only: when the provider returns a banned language
    /// **and** the in-memory language cache holds a previously-
    /// observed peer code for this backend, re-issue the request
    /// with that code forced. Cold-start (empty cache) accepts the
    /// unforced response and lets the cache populate from the next
    /// correct detection. Default `true` (plan v3); set to `false`
    /// to skip the rerun unconditionally for cost-sensitive setups.
    pub cloud_rerun_on_language_mismatch: bool,
}

#[allow(deprecated)]
impl Default for General {
    fn default() -> Self {
        Self {
            language: String::new(),
            languages: Vec::new(),
            startup_autostart: false,
            sound_feedback: true,
            auto_mute_system: true,
            always_warm_mic: false,
            also_copy_to_clipboard: true,
            cloud_force_primary_language: false,
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
    pub hold: String,
    pub toggle: String,
    pub cancel: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            hold: "F8".into(),
            toggle: "F9".into(),
            cancel: "Escape".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    pub input_device: String,
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
            input_device: String::new(),
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
    /// **Deprecated** in favour of [`SttLocal::languages`] / the
    /// top-level [`General::languages`]. Kept for one release cycle
    /// for migration. Empty string means "fall through to
    /// `general.languages`".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
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
            quantization: "q5_1".into(),
            language: String::new(),
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
    /// Opt-in to the streaming pseudo-stream pipeline for hosted
    /// providers that do not expose a native streaming endpoint (Groq
    /// today). When `true`, the live-dictation path re-POSTs the
    /// trailing N seconds of audio every ~700 ms to the same batch
    /// endpoint and pipes results through the `LocalAgreement` helper
    /// to produce preview text. Costs roughly +25% vs a single batch
    /// POST per utterance — opt in deliberately on usage-billed
    /// plans. Default `false` so existing Groq users stay on the
    /// cheaper batch profile.
    ///
    /// Plan: `plans/2026-04-27-fono-interactive-v1.md` R4.2 /
    /// `plans/2026-04-28-wave-3-slice-b1-v1.md` Thread B.
    #[serde(default)]
    pub streaming: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Llm {
    pub enabled: bool,
    pub backend: LlmBackend,
    pub local: LlmLocal,
    pub cloud: Option<LlmCloud>,
    pub prompt: Prompt,
    /// Skip the LLM cleanup roundtrip when the raw STT output has
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

impl Default for Llm {
    fn default() -> Self {
        Self {
            // Disabled by default until the user opts into a cloud
            // provider via `fono setup`, or compiles in `llama-local`
            // and configures a model. Avoids "first dictation crashes
            // because LlamaLocal is a stub" trap.
            enabled: false,
            backend: LlmBackend::None,
            local: LlmLocal::default(),
            cloud: None,
            prompt: Prompt::default(),
            skip_if_words_lt: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LlmBackend {
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

impl Default for LlmBackend {
    fn default() -> Self {
        Self::Local
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmLocal {
    pub model: String,
    pub quantization: String,
    pub context: u32,
}

impl Default for LlmLocal {
    fn default() -> Self {
        Self {
            model: "qwen2.5-1.5b-instruct".into(),
            quantization: "q4_k_m".into(),
            context: 4096,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmCloud {
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
/// `plans/2026-04-28-llm-cleanup-clarification-refusal-fix-v1.md`.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Overlay {
    pub enabled: bool,
    pub position: String,
    pub opacity: f32,
}

impl Default for Overlay {
    fn default() -> Self {
        Self {
            enabled: true,
            position: "bottom-right".into(),
            opacity: 0.85,
        }
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
        Self {
            enabled: true,
            retention_days: 90,
            redact_secrets: true,
        }
    }
}

/// Text-injection tuning. Currently a single knob for the X11 XTEST
/// paste shortcut; reserved as the home of future per-app paste rules
/// and inject backend overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Inject {
    /// Which keystroke combo `Injector::XtestPaste` synthesizes after
    /// writing the system clipboard. Accepted values: `"shift-insert"`
    /// (default — universal X11 paste, works in terminals + GUI),
    /// `"ctrl-v"` (GUI-only — captured by shells/tmux/vim),
    /// `"ctrl-shift-v"` (modern terminal "official" paste).
    /// Override at runtime with `FONO_PASTE_SHORTCUT=...`.
    pub paste_shortcut: String,
}

impl Default for Inject {
    fn default() -> Self {
        Self {
            paste_shortcut: "shift-insert".into(),
        }
    }
}

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
        Self {
            auto_check: true,
            channel: "stable".into(),
        }
    }
}

/// Live-dictation runtime toggle and tuning knobs. Plan R7.4 / R18.21.
///
/// When the cargo `interactive` feature is **not** compiled in, this
/// block is parsed but ignored (the daemon has no streaming code to
/// turn on). When the feature *is* compiled in, the daemon consults
/// `enabled` at startup and on every `Reload` IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Interactive {
    /// Master toggle. Default `false` everywhere in v0.2.0-alpha. Tier-
    /// aware auto-enable is a Slice B decision (see ADR 0009).
    pub enabled: bool,
    /// Per-minute spending ceiling, in USD micro-cents (1¢ = 10_000 µ¢).
    /// `0` disables the budget controller entirely (default — local STT
    /// is free). Cloud streaming sets a sensible default at wizard time.
    pub budget_ceiling_per_minute_umicros: u64,
    /// Quality floor under budget pressure. `"max"` (default) never
    /// skips finalize; `"balanced"` may slow preview cadence;
    /// `"aggressive"` may skip finalize on high-confidence segments.
    pub quality_floor: String,
    /// Show the live-dictation overlay. Independent of the static
    /// `[overlay].enabled` knob so the user can keep the recording-
    /// indicator overlay disabled but still see live text. Default
    /// `true`.
    pub overlay: bool,
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
    /// When `true`, run the LLM cleanup pass once on the assembled
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
            enabled: false,
            budget_ceiling_per_minute_umicros: 0,
            quality_floor: "max".into(),
            overlay: true,
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
            hold_release_grace_ms: 300,
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
    ["um", "uh", "er", "ah", "mm", "like", "you know"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
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
    /// Load from disk; if the file does not exist, return defaults (caller
    /// may choose to persist them via [`Config::save`]).
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                let mut cfg: Self = toml::from_str(&raw).map_err(|source| Error::TomlParse {
                    path: path.to_path_buf(),
                    source,
                })?;
                cfg.migrate()?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::Io {
                path: path.to_path_buf(),
                source,
            }),
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

        // ----- Language allow-list migration (ADR 0016) -------------------
        // v0.1 had a single `general.language = "auto" | "<bcp-47>"` knob.
        // v0.2 introduces `general.languages: Vec<String>` (allow-list);
        // empty = unconstrained auto-detect, one = forced, many = banned
        // outside the list. Lift the legacy scalar exactly once, then drop
        // it from disk on the next save (`skip_serializing_if`).
        if self.general.languages.is_empty() {
            let legacy = self.general.language.trim().to_ascii_lowercase();
            if !legacy.is_empty() && legacy != "auto" {
                self.general.languages = vec![legacy];
            }
        }
        // Keep on disk only when explicitly cleared by the user; the
        // serializer will skip the empty string.
        self.general.language.clear();

        // Same migration on `[stt.local]` for users who pinned the
        // language at the backend level rather than at `[general]`.
        if self.stt.local.languages.is_empty() {
            let legacy = self.stt.local.language.trim().to_ascii_lowercase();
            if !legacy.is_empty() && legacy != "auto" {
                self.stt.local.languages = vec![legacy];
            }
        }
        self.stt.local.language.clear();

        self.version = CURRENT_VERSION;
        Ok(())
    }

    /// Atomic write via tempfile + rename in the same directory.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| Error::Io {
                path: dir.to_path_buf(),
                source,
            })?;
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
    std::fs::create_dir_all(dir).map_err(|source| Error::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|source| Error::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    tmp.write_all(data).map_err(|source| Error::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.as_file_mut().sync_all().map_err(|source| Error::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(_mode)).map_err(
            |source| Error::Io {
                path: tmp.path().to_path_buf(),
                source,
            },
        )?;
    }

    tmp.persist(path).map_err(|e| Error::Io {
        path: PathBuf::from(path),
        source: e.error,
    })?;
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
        assert!(
            loaded.general.languages.is_empty(),
            "default = unconstrained auto-detect"
        );
        assert!(loaded.general.language.is_empty());
        assert_eq!(loaded.stt.local.model, "small");
        assert_eq!(loaded.llm.local.model, "qwen2.5-1.5b-instruct");
    }

    #[test]
    fn missing_file_returns_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = Config::load(&tmp.path().join("nope.toml")).unwrap();
        assert_eq!(cfg.version, CURRENT_VERSION);
    }

    #[test]
    fn future_version_rejected() {
        let mut cfg = Config {
            version: CURRENT_VERSION + 42,
            ..Config::default()
        };
        assert!(matches!(
            cfg.migrate(),
            Err(Error::ConfigVersionTooNew { .. })
        ));
    }

    #[test]
    fn interactive_v7_keys_round_trip() {
        let raw = r#"
            version = 1
            [interactive]
            enabled = true
            budget_ceiling_per_minute_umicros = 1000
            quality_floor = "balanced"
            overlay = false
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
        assert!(i.enabled);
        assert_eq!(i.budget_ceiling_per_minute_umicros, 1000);
        assert_eq!(i.quality_floor, "balanced");
        assert!(!i.overlay);
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
        assert_eq!(i.enabled, d.enabled);
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
        assert_eq!(i.hold_release_grace_ms, 300);
        assert_eq!(i.hold_release_grace_ms, d.hold_release_grace_ms);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("partial.toml");
        std::fs::write(&path, "version = 1\n[general]\nlanguage = \"ro\"\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        // Migration lifts the legacy scalar into the new allow-list.
        assert_eq!(cfg.general.languages, vec!["ro"]);
        assert!(cfg.general.language.is_empty(), "deprecated key cleared");
        assert!(cfg.general.sound_feedback);
        assert_eq!(cfg.stt.local.model, "small");
    }

    #[test]
    fn legacy_language_auto_migrates_to_empty_allow_list() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy_auto.toml");
        std::fs::write(&path, "version = 1\n[general]\nlanguage = \"auto\"\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert!(
            cfg.general.languages.is_empty(),
            "auto -> unconstrained allow-list"
        );
        assert!(cfg.general.language.is_empty());
    }

    #[test]
    fn languages_round_trip_drops_legacy_field() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("multi.toml");
        let mut cfg = Config::default();
        cfg.general.languages = vec!["en".into(), "ro".into(), "fr".into()];
        cfg.save(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.lines()
                .any(|l| l.trim_start().starts_with("language =")),
            "deprecated scalar must not be serialised: {raw}"
        );
        assert!(raw.contains("languages = ["));
        let reloaded = Config::load(&path).unwrap();
        assert_eq!(
            reloaded.general.languages,
            vec!["en".to_string(), "ro".into(), "fr".into()]
        );
    }

    #[test]
    fn explicit_languages_wins_over_legacy_scalar() {
        let raw = r#"
            version = 1
            [general]
            language = "fr"
            languages = ["en", "ro"]
        "#;
        let mut cfg: Config = toml::from_str(raw).expect("parse");
        cfg.migrate().expect("migrate");
        assert_eq!(cfg.general.languages, vec!["en", "ro"]);
        assert!(cfg.general.language.is_empty());
    }
}
