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
    pub language: String,
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
    /// Pop a desktop notification after every successful pipeline
    /// showing the dictated text. Default `true` so users always have
    /// feedback even when injection silently fails.
    pub notify_on_dictation: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            language: "auto".into(),
            startup_autostart: false,
            sound_feedback: true,
            auto_mute_system: true,
            always_warm_mic: false,
            also_copy_to_clipboard: true,
            notify_on_dictation: true,
        }
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
    pub language: String,
    /// Whisper inference thread count. `0` = auto-detect physical
    /// cores (avoids SMT thrash). Latency plan L18.
    pub threads: u32,
}

impl Default for SttLocal {
    fn default() -> Self {
        Self {
            model: "small".into(),
            quantization: "q5_1".into(),
            language: "auto".into(),
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
    /// Latency plan L9 — for short utterances (chat, search bars) the
    /// LLM costs more than it cleans.
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
            skip_if_words_lt: 0,
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
pub const fn default_prompt_main() -> &'static str {
    "You are a transcription cleanup assistant. Given raw speech-to-text output, return the \
same text with filler words removed (um, uh, like), proper punctuation and capitalization \
added, and obvious stutters collapsed. Preserve the speaker's language and tone exactly — \
do not translate, summarise, or add content. Output only the cleaned text with no commentary."
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
        }
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
        "and", "but", "or", "so", "because", "the", "a", "an", "of", "to", "with", "for",
        "in", "on", "at", "from",
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
        // No older versions yet. Future arms go here, e.g. `if self.version < 2 { … }`.
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
        assert_eq!(loaded.general.language, "auto");
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
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("partial.toml");
        std::fs::write(&path, "version = 1\n[general]\nlanguage = \"ro\"\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.general.language, "ro");
        assert!(cfg.general.sound_feedback);
        assert_eq!(cfg.stt.local.model, "small");
    }
}
