// SPDX-License-Identifier: GPL-3.0-only
//! The `Assistant` trait + per-turn context type.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use fono_core::prompt_cache_view::CacheSnapshot;
use fono_core::screen_capture::{CaptureError, CaptureMode, CapturedImage};
use futures::stream::BoxStream;

use crate::history::{ChatTurn, ToolCall};

/// Hotkey/runtime trigger that selected a prompt-state cache family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantCacheTrigger {
    /// F7 dictation/polish flow. Local assistant backends may warm a compatible
    /// cleanup prompt prefix when they share the embedded llama.cpp runtime.
    F7,
    /// F8 voice-assistant flow.
    F8,
}

/// Stable prompt families the daemon may ask an assistant backend to warm at
/// startup/idle time. Non-local backends ignore this through the default trait
/// implementation.
#[derive(Debug, Clone, Default)]
pub struct AssistantPromptCacheWarmup {
    pub f7_system_prompt: Option<String>,
    pub f8_system_prompt: Option<String>,
    /// The user's own tools, exactly as the reply path will be given them.
    ///
    /// A backend that describes tools in the system prompt — the embedded one
    /// does, because it renders its own chat markers and so never sees the
    /// model's tool template — must warm the prompt it will actually use. A
    /// trace of a small local model showed the cost of not doing so: the warm
    /// pinned 72 tokens of bare greeting while the live prompt opened with 1510
    /// tokens of greeting, areas, devices and tool descriptions, so every first
    /// command of a conversation paid thirteen seconds to read a device list
    /// that had not changed since boot.
    ///
    /// Descriptors travel rather than rendered text so that only one piece of
    /// code ever renders them. Two renderings that must agree byte for byte
    /// have drifted twice before, and each time the symptom was a checkpoint
    /// that could never match.
    pub f8_action_descriptors: Vec<serde_json::Value>,
    /// How the assistant should behave, rendered *after* everything else.
    ///
    /// See [`compose_head`] for why last.
    pub f8_instructions: Option<String>,
}

/// Per-turn cache preparation request captured at hotkey press time, before STT
/// finishes. The user transcript is intentionally absent; only stable prompt and
/// active-window state are available this early.
#[derive(Debug, Clone)]
pub struct AssistantPromptCacheSnapshot {
    pub trigger: AssistantCacheTrigger,
    pub system_prompt: String,
    /// The user's tools, as [`AssistantPromptCacheWarmup::f8_action_descriptors`].
    /// Carried for the same reason: a backend that describes tools in the
    /// system prompt must warm the prompt it will actually use.
    pub action_descriptors: Vec<serde_json::Value>,
    /// How the assistant should behave, as
    /// [`AssistantPromptCacheWarmup::f8_instructions`].
    pub instructions: Option<String>,
    pub history: Vec<ChatTurn>,
    pub active_window_context: Option<String>,
    pub prefer_vision: bool,
}

/// Assemble the steady head, in the order that survives a weak model.
///
/// `context` — who the assistant is, the areas, the device names — then the
/// tool block, then `instructions`: how to behave. Everything here changes
/// only when the house does, so the whole string stays checkpointable.
///
/// **Why the instructions come last.** A trace of a small local model showed
/// *"Match the user's language"* sitting roughly fourteen hundred tokens back,
/// behind seventy-seven device names and twenty-three tool signatures, and
/// being ignored twice in a row: two Romanian commands, two English replies.
/// Attention thins with distance, and the machine-readable bulk in the middle
/// is exactly the kind of text that dilutes it. Moving the behavioural rules to
/// the end costs a capable model nothing and gives a weak one its best chance
/// of honouring them.
///
/// The order also has to be *stable*, not merely good: this is what the prompt
/// cache pins, so anything volatile — the speaker note — is composed afterwards
/// by [`compose_system_prompt`], never woven in here.
#[must_use]
pub fn compose_head(context: &str, tool_block: Option<&str>, instructions: Option<&str>) -> String {
    let mut parts = std::iter::once(context)
        .chain(tool_block)
        .chain(instructions)
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let Some(first) = parts.next() else { return String::new() };
    let mut out = first.to_string();
    for p in parts {
        out.push_str("\n\n");
        out.push_str(p);
    }
    out
}

/// Compose the system block a backend will actually send: the steady head
/// first, the volatile per-turn notes last.
///
/// The head — greeting, areas, devices, tool descriptions — changes only when
/// the house does, so a backend that checkpoints its prompt can keep it for
/// days. The notes change from one turn to the next. Putting them last is what
/// lets a change of speaker cost a handful of tokens instead of throwing away
/// nine hundred tokens of perfectly good device list.
#[must_use]
pub fn compose_system_prompt(head: &str, notes: Option<&str>) -> String {
    notes.map(str::trim).filter(|n| !n.is_empty()).map_or_else(
        // Leave the head byte-identical to what the cache pinned — trailing
        // whitespace and all — when there is nothing to add.
        || head.to_string(),
        |note| format!("{}\n\n{note}", head.trim_end()),
    )
}

/// Name the language to reply in, for a BCP-47 code Fono recognises.
///
/// *"Match the user's language"* is in the behavioural rules, and a small model
/// ignores it: four traces of Romanian and English commands came back in English
/// every time, including the two spoken in Romanian. The rule asks the model to
/// do two things at once — work out which language it just heard, and then use
/// it — and the first is the one it fails, silently and with no way to tell.
///
/// Naming the language removes the inference. Fono has already decided which
/// language this was, minutes more reliably than the model can, because that
/// same decision picks the speech recogniser and the voice that speaks the
/// answer. Saying it plainly costs a handful of tokens and needs nothing of the
/// model but obedience.
///
/// Deliberately silent on a code that is not in the curated list: rendering
/// *"Reply in haw."* is worse than saying nothing, because it asks the model to
/// decode a language tag mid-instruction. Those fall back to the general rule,
/// which is exactly today's behaviour.
#[must_use]
pub fn language_note(code: &str) -> Option<String> {
    let code = code.trim();
    let name = fono_core::languages::display_name(code);
    // `display_name` echoes the code back when it does not know it.
    (!code.is_empty() && name != code).then(|| format!("Reply in {name}."))
}

/// One token delta yielded by [`Assistant::reply_stream`]. Most
/// deltas carry spoken `text`; a small number carry a sentinel
/// [`ToolEvent`] that the caller MUST record in
/// [`crate::ConversationHistory`] so subsequent turns can echo the
/// tool sequence back to the model.
///
/// A single delta carries _either_ `text` or `tool_event` — never
/// both at once. Callers should branch on `tool_event` first; if
/// `Some`, ignore `text`.
#[derive(Debug, Clone, Default)]
pub struct TokenDelta {
    pub text: String,
    /// Sentinel for non-text events on the stream (tool call issued,
    /// tool result observed). When `Some`, this delta has no spoken
    /// content and the caller should append a corresponding entry
    /// to the rolling history before pushing the final assistant
    /// reply.
    pub tool_event: Option<ToolEvent>,
}

impl TokenDelta {
    /// Build a pure-text delta. Equivalent to `TokenDelta { text,
    /// tool_event: None }` but reads cleaner at call sites.
    #[must_use]
    pub fn text(text: String) -> Self {
        Self { text, tool_event: None }
    }

    /// Build a sentinel delta carrying a [`ToolEvent`]. The `text`
    /// field is empty and must not be spoken.
    #[must_use]
    pub fn tool(event: ToolEvent) -> Self {
        Self { text: String::new(), tool_event: Some(event) }
    }
}

/// Side-band events on the token stream that record tool usage.
/// Emitted by the assistant client during a turn where the model
/// invoked a function-calling tool.
#[derive(Debug, Clone)]
pub enum ToolEvent {
    /// The model issued a tool call. The caller should append an
    /// assistant turn with this tool call to history so the next
    /// turn's wire request can echo it back to the model.
    Called(ToolCall),
    /// The tool returned a result. `summary` is a short, prose
    /// description suitable for storing in history; the actual
    /// payload (image bytes, etc.) is _not_ retained.
    ///
    /// `failed` is the executor's own verdict, carried here rather than
    /// re-derived from `summary`. Guessing it back out of the prose was a
    /// real bug: a Home Assistant result that succeeded ends with
    /// `"failed": []`, and keyword matching duly logged the turn as a
    /// failure. Only the party that ran the tool knows how it went.
    ///
    /// `sent` is the executor's copy of [`ToolOutcome::sent`], carried on this
    /// event because it is the first moment it exists: the call was announced
    /// before it ran, and only running it settles what was asked of the
    /// server.
    Result { tool_call_id: String, summary: String, failed: bool, sent: Option<String> },
}

/// Synchronous screen-capture callback type. The closure runs the
/// full [`fono_core::screen_capture::GrabberProbe`] pipeline (including the
/// privacy gate) and returns the captured PNG or a [`CaptureError`].
/// Wrapped in [`Arc`] so it can be cheaply cloned into spawned tasks.
pub type ScreenCaptureFn =
    Arc<dyn Fn(CaptureMode) -> Result<CapturedImage, CaptureError> + Send + Sync>;

/// Runs one tool call the model asked for, and reports back in prose.
///
/// Returns a summary rather than a `Result` on purpose. A tool that
/// failed is not an error in the turn — it is *the news*, and the user
/// must hear it. Bubbling it up as `Err` would abort the reply and leave
/// them wondering whether the light came on. The executor is also the
/// only party that can word the outcome honestly, because it is the one
/// that knows whether the effect could be checked afterwards — and for
/// the same reason it is the only party that can say whether it failed,
/// which is why the verdict travels alongside the prose.
pub type ToolExecFn =
    Arc<dyn Fn(ToolCall) -> futures::future::BoxFuture<'static, ToolOutcome> + Send + Sync>;

/// What running one tool came to: what to tell the model, and whether it
/// worked. `failed` means "something demonstrably went wrong" — never
/// "we checked and it was fine", which for many tools is unknowable.
///
/// The four flags are four separate facts about one call, and no two of them
/// can be folded into a state: a command can fail and be safe to repeat, or
/// succeed without ever being checked, or be refused by Fono in a way that
/// invites the identical call back. Collapsing them into an enumeration would
/// name states nothing observes.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct ToolOutcome {
    pub summary: String,
    pub failed: bool,
    /// Whether the model may be given one more go at this, with the tools
    /// still offered, instead of the turn ending in an apology.
    ///
    /// Four traces of one small model asking for the bedroom lights ended the
    /// same way: the house objected in plain words, Fono read the objection
    /// aloud, and the user had to say the whole thing again. The correction
    /// the model needed was already in its hands — the server's own error text
    /// — and nothing used it.
    ///
    /// Only the executor can set this, because only the executor knows whether
    /// running the command a second time is harmless. Two cases qualify:
    /// nothing happened at all, so there is nothing to double; or the command
    /// asks for an absolute end state ("be on", "be off"), where asking twice
    /// and asking once are the same request. A relative change — two degrees
    /// warmer — is never retried, because twice is four degrees.
    pub retryable: bool,
    /// The arguments as they actually reached the server, when the executor
    /// changed them — blanks dropped, values the home itself had already
    /// stated corrected. `None` means "exactly as the model wrote them".
    ///
    /// Carried out of the executor because nothing downstream can reconstruct
    /// it, and because the record was misleading without it: a user reading
    /// their own history saw `"floor": ""` in a call whose blank fields had
    /// been removed before it left, and reasonably concluded the removal was
    /// never happening.
    pub sent: Option<String>,
    /// Whether writing the identical call again is worth sending.
    ///
    /// Normally it is not: the same request gets the same answer, so a model
    /// that repeats itself is only making the user wait twice. The exception is
    /// a refusal Fono itself made on a guess about the request — a number the
    /// user may have spoken in words rather than digits — where repeating the
    /// call is how the model says the guess was wrong, and where the refusal
    /// itself invites exactly that.
    pub repeat_ok: bool,
    /// Whether the world was read again after the command and agreed with it.
    ///
    /// Stronger than `!failed`, and the difference is the whole point: a server
    /// that answers "done" has made a claim, while this says the devices were
    /// looked at afterwards and are in the state that was asked for. Only that
    /// second thing is enough to let a sentence written *before* the command
    /// stand as the report of it.
    pub confirmed: bool,
}

impl ToolOutcome {
    /// A tool that did what was asked, with no second chance needed.
    #[must_use]
    pub fn worked(summary: String) -> Self {
        Self {
            summary,
            failed: false,
            retryable: false,
            sent: None,
            repeat_ok: false,
            confirmed: false,
        }
    }
}

/// The tools Fono may let the model invoke this turn, and how to run
/// them. Built from the user's tool catalogue; absent when they have no
/// servers configured or have switched the feature off.
#[derive(Clone)]
pub struct ActionTools {
    /// OpenAI-style function descriptors, already filtered to the tools
    /// the user left switched on.
    pub descriptors: Vec<serde_json::Value>,
    pub execute: ToolExecFn,
    /// A line to append to the system prompt naming the real areas, so the
    /// model picks one instead of translating and inventing. `None` when
    /// nothing is known or the user switched it off.
    pub hint: Option<String>,
    /// Rails for a model writing a command on this machine: the only text it
    /// is allowed to sample once it starts one.
    ///
    /// Built from the tools offered this turn and the areas and devices the
    /// house reported, so it can only ever describe things that exist. `None`
    /// when the user has not switched it on, when nothing could be derived, or
    /// on any backend that is not running the model here — a service enforces
    /// its own schema and needs no help.
    ///
    /// A hint asks; this decides. Both are kept because they answer different
    /// halves of the same failure: the hint tells the model which area to pick,
    /// the rails stop it writing one that does not exist.
    pub grammar: Option<String>,
    /// The words this turn is about, for whoever runs the tools to read.
    pub said: Said,
}

/// What the user said this turn, shared with whatever runs the tools.
///
/// Exists because one class of wrong call can only be recognised against the
/// request: a value the user never mentioned. Asked plainly to switch an air
/// conditioner off, a model reached for the tool that sets a temperature and
/// wrote `"temperature": 0` — a value no schema can object to and no blank
/// check can catch, and the only evidence it was invented is that nobody said
/// a number.
///
/// A slot rather than an argument because the tools are assembled before the
/// speech has been recognised, and the same assembly runs the calls afterwards.
#[derive(Clone, Default)]
pub struct Said(Arc<std::sync::Mutex<String>>);

impl Said {
    /// Record what the user said. Replaces whatever the last turn said.
    pub fn heard(&self, text: &str) {
        if let Ok(mut said) = self.0.lock() {
            *said = text.to_string();
        }
    }

    /// The words, or empty when nothing was recorded — which every check here
    /// must read as "no evidence", never as "the user said nothing".
    #[must_use]
    pub fn words(&self) -> String {
        self.0.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl std::fmt::Debug for ActionTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionTools")
            .field("descriptors", &self.descriptors.len())
            .finish_non_exhaustive()
    }
}

/// Per-turn context passed to [`Assistant::reply_stream`]. The history
/// is a snapshot taken by the caller *before* it pushed the new user
/// turn, so the user's current text is the `user_text` argument and
/// not duplicated here. `system_prompt` is the chat-style prompt from
/// `[assistant].prompt_main` (distinct from the cleanup prompt).
#[derive(Clone, Default)]
pub struct AssistantContext {
    pub system_prompt: String,
    /// Who Fono believes it is talking to, when voice verification matched.
    ///
    /// Carried apart from [`system_prompt`] because it is the most volatile
    /// thing in the prompt — it appears, disappears and changes identity from
    /// one turn to the next — while everything around it (the greeting, the
    /// areas, the devices, the tool descriptions) changes only when the house
    /// does. A backend that caches its prompt must put the steady part first
    /// and this last, or a change of speaker throws away nine hundred tokens
    /// of device list that was perfectly good.
    pub speaker_note: Option<String>,
    /// How the assistant should behave — reply length, plain prose, match the
    /// user's language.
    ///
    /// Carried apart from [`system_prompt`](Self::system_prompt) so it can be
    /// rendered *after* the tool block. See [`compose_head`] for why last; a
    /// backend that does not describe tools in the prompt gets the same order
    /// for free through [`system_block`](Self::system_block).
    pub instructions: Option<String>,
    pub language: Option<String>,
    pub history: Vec<ChatTurn>,
    /// Short, runtime-only description of the window active when the assistant
    /// hotkey was pressed. This is cached separately from stable prompts so a
    /// window change cannot invalidate F8's base prompt checkpoint.
    pub active_window_context: Option<String>,
    /// When `Some`, tool-calling is enabled and the model may invoke
    /// `fono_screen` to capture the screen during a voice turn.
    /// Set from the F8 voice loop when a `GrabberProbe` is available.
    pub screen_capture: Option<ScreenCaptureFn>,
    /// When `Some`, the model may also invoke the user's own tools —
    /// smart-home commands and the like. Set only on turns Fono is
    /// willing to act on.
    pub actions: Option<Arc<ActionTools>>,
    /// When `true` (and [`screen_capture`] is `Some`), include the
    /// `fono_screen` tool descriptor in every request. Users opt in
    /// with `[assistant].prefer_vision = true`.
    pub prefer_vision: bool,
    /// Optional per-request cap on generated tokens. When `Some`, local
    /// backends clamp it to their global budget; `None` keeps the
    /// backend default. Used by short-form tasks (e.g. notification
    /// summaries) that never need a long reply.
    pub max_new_tokens: Option<u32>,
    /// Whether this turn may drive the Glas Cortex "brain" capture on the
    /// embedded llama.cpp backend. `true` only for turns triggered on the
    /// local machine (the F8 voice hotkey) whose thinking the on-screen
    /// overlay is meant to show. Left `false` (the [`Default`]) for turns
    /// arriving over the network — e.g. the OpenAI/Ollama-compatible LLM
    /// server shares the same backend `Arc`, and a remote client's request
    /// must never light up this computer's overlay or pay the capture cost.
    pub allow_brain_capture: bool,
}

impl AssistantContext {
    /// The volatile tail: which language to reply in, then who is speaking.
    ///
    /// Both change from one turn to the next, so both are composed *after* the
    /// steady head rather than woven into it — see [`compose_system_prompt`].
    /// The language is named rather than inferred because Fono already knows it
    /// and a small model demonstrably does not act on being told to work it out.
    #[must_use]
    pub fn turn_notes(&self) -> Option<String> {
        let lang = self.language.as_deref().and_then(language_note);
        match (lang, self.speaker_note.as_deref().map(str::trim).filter(|n| !n.is_empty())) {
            (None, None) => None,
            (Some(l), None) => Some(l),
            (None, Some(s)) => Some(s.to_string()),
            (Some(l), Some(s)) => Some(format!("{l} {s}")),
        }
    }

    /// The system block to send: the steady head, then this turn's notes.
    ///
    /// Backends that pass the system prompt straight through should use this
    /// rather than [`system_prompt`](Self::system_prompt), so the behavioural
    /// rules land after the context and the volatile notes land last.
    /// A backend that appends more steady material of its own — the embedded
    /// one describes the user's tools in the system prompt — must build the
    /// head with [`compose_head`] and call [`compose_system_prompt`] itself.
    #[must_use]
    pub fn system_block(&self) -> String {
        let head = compose_head(&self.system_prompt, None, self.instructions.as_deref());
        compose_system_prompt(&head, self.turn_notes().as_deref())
    }
}

impl std::fmt::Debug for AssistantContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantContext")
            .field("system_prompt", &self.system_prompt)
            .field("speaker_note", &self.speaker_note)
            .field("instructions", &self.instructions)
            .field("language", &self.language)
            .field("history", &self.history)
            .field("active_window_context", &self.active_window_context)
            .field("screen_capture", &self.screen_capture.is_some())
            .field("actions", &self.actions)
            .field("prefer_vision", &self.prefer_vision)
            .field("max_new_tokens", &self.max_new_tokens)
            .field("allow_brain_capture", &self.allow_brain_capture)
            .finish()
    }
}

#[async_trait]
pub trait Assistant: Send + Sync {
    /// Stream the model's reply token-by-token. The returned stream
    /// yields `Ok(TokenDelta)` per delta and ends when the model
    /// finishes (or errors). Cancellation is by dropping the stream;
    /// implementations MUST not require an explicit cancel call.
    async fn reply_stream(
        &self,
        user_text: &str,
        ctx: &AssistantContext,
    ) -> Result<BoxStream<'static, Result<TokenDelta>>>;

    /// Backend identifier for history / logging.
    fn name(&self) -> &'static str;

    /// The model this backend is talking to, as a person would name it —
    /// `gpt-5-mini`, `qwen3.5-4b`. Recorded against a saved conversation so
    /// the history page can say which model answered rather than which of
    /// Fono's internal backends carried the request, a distinction only
    /// Fono's own source makes sense of. `None` when the backend has no one
    /// named model.
    fn model(&self) -> Option<String> {
        None
    }

    /// Whether this backend can actually invoke [`AssistantContext::actions`].
    ///
    /// The default is `false`, and deliberately so: a backend that ignores
    /// the field still produces a perfectly fluent reply, so the model
    /// cheerfully promises to switch the lights on and nothing happens. That
    /// is the worst failure available to us — from where the user is
    /// standing it is indistinguishable from success. Opting in is therefore
    /// explicit, and a backend that has not opted in is told plainly that it
    /// cannot act, so it says so instead of promising.
    fn can_run_actions(&self) -> bool {
        false
    }

    /// Optional best-effort warmup. Cloud backends should fire a cheap
    /// HEAD/GET; local backends should mmap their model. Failures are
    /// non-fatal.
    async fn prewarm(&self) -> Result<()> {
        Ok(())
    }

    /// Optional startup/idle prompt-state cache warmup. Embedded local
    /// backends use this to prefill stable F7/F8/tool prompts without making
    /// the hotkey path pay the prompt cost. Cloud backends ignore it.
    async fn prewarm_prompt_caches(&self, _warmup: AssistantPromptCacheWarmup) -> Result<()> {
        Ok(())
    }

    /// Optional hotkey-time cache preparation. The default is a no-op; embedded
    /// local backends may restore/build a stable checkpoint and, when window
    /// context is available, schedule a dynamic window checkpoint.
    async fn prepare_prompt_cache_for_turn(
        &self,
        _snapshot: AssistantPromptCacheSnapshot,
    ) -> Result<()> {
        Ok(())
    }

    /// Shape and running totals of this backend's prompt-state cache, for the
    /// diagnostics panel. `None` from every backend that keeps no local KV
    /// cache, which is every cloud one.
    fn prompt_cache_snapshot(&self) -> Option<CacheSnapshot> {
        None
    }
}

/// One event emitted by a [`RealtimeSession`] as the model streams its
/// reply. The realtime (speech-to-speech) path bypasses the staged
/// STT → LLM → TTS pipeline: the model owns VAD, transcription, and
/// audio synthesis, emitting these events over a single WebSocket.
///
/// Tool-calling is **not** represented here yet — the first realtime
/// slice (Gemini Live audio loop) ships without tools. A
/// `ToolCallRequested` variant plus a tool-result submission channel on
/// [`RealtimeSession`] will be added when `fono-action` lands, matching
/// the session-handle design in the realtime plan.
#[derive(Debug, Clone)]
pub enum RealtimeEvent {
    /// A chunk of reply audio as mono f32 PCM at `sample_rate` Hz.
    Audio { pcm: Vec<f32>, sample_rate: u32 },
    /// Incremental transcript of the model's spoken reply (for history
    /// / on-screen display). May arrive interleaved with `Audio`.
    AssistantTextDelta(String),
    /// Final transcript of the user's utterance, as recognised by the
    /// model's own input transcription. Pushed to history as the user
    /// turn.
    UserTextFinal(String),
    /// The model detected the user barging in (VAD on the model side)
    /// and is discarding the rest of its current spoken reply. The
    /// consumer must immediately drop any buffered/queued reply audio so
    /// playback stops at once — Gemini Live signals this with
    /// `serverContent.interrupted: true`. Reply text already received
    /// for the interrupted turn is left as-is (the model stops
    /// extending it).
    Interrupted,
    /// The model finished its turn. The consumer flushes history and
    /// waits for playback to drain.
    Done,
    /// The model decided the conversation is over (full-duplex live mode
    /// only) — e.g. the user said goodbye / "that's all". Signalled via a
    /// provider tool/function call the model is instructed to invoke
    /// (Gemini Live `toolCall` for the `end_conversation` function). The
    /// consumer should finish the current reply, then close the live
    /// session gracefully. Never emitted in push-to-talk mode.
    EndConversation,
}

/// An open realtime session: a live WebSocket to a speech-to-speech
/// model. The caller forwards mic PCM into [`audio_in`](Self::audio_in)
/// and consumes reply events from [`events`](Self::events). Dropping the
/// struct closes the underlying WebSocket (the client's `Drop`/task
/// teardown sends a Close frame and aborts the reader).
pub struct RealtimeSession {
    /// Mic input sink: mono f32 PCM frames at the model's expected input
    /// rate (see [`RealtimeAssistant::native_input_rate`]). Closing the
    /// sender signals end-of-input for the current utterance.
    pub audio_in: tokio::sync::mpsc::Sender<Vec<f32>>,
    /// Reply event stream. Ends (yields `None`) when the session closes.
    pub events: BoxStream<'static, Result<RealtimeEvent>>,
}

impl std::fmt::Debug for RealtimeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeSession")
            .field("audio_in_closed", &self.audio_in.is_closed())
            .finish_non_exhaustive()
    }
}

/// How a realtime session handles turn-taking. Chosen per session, not
/// per backend: the same provider client serves both gestures.
///
/// - [`PushToTalk`](Self::PushToTalk) — F8 *hold*. The caller streams one
///   buffered utterance, signals end-of-input, and waits for a single
///   reply. The client commits the turn explicitly (Gemini `audioStreamEnd`),
///   so server-side activity detection is irrelevant and the mic is closed
///   before the reply plays. Cheapest; no echo cancellation needed.
/// - [`FullDuplex`](Self::FullDuplex) — F8 *tap* (live mode). Continuous mic
///   for the session lifetime; the model owns turn boundaries via server VAD
///   and the user can interrupt by speaking. Requires echo cancellation when
///   played over speakers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeMode {
    /// One buffered utterance → one reply, mic closed during playback.
    PushToTalk,
    /// Continuous full-duplex conversation with server VAD + barge-in.
    FullDuplex,
}

/// A realtime / speech-to-speech assistant backend. Implementors open a
/// bidirectional WebSocket where the model ingests the user's mic audio
/// and streams reply audio back directly. Selected (over the staged
/// [`Assistant`]) when the configured model matches a provider's
/// `RealtimeProfile` in the catalogue.
#[async_trait]
pub trait RealtimeAssistant: Send + Sync {
    /// Open a fresh realtime session. `ctx` supplies the system prompt,
    /// language, and rolling history used to seed the model's setup
    /// message. `mode` selects push-to-talk vs full-duplex turn-taking,
    /// which the client maps onto its own wire config.
    async fn open_session(
        &self,
        ctx: &AssistantContext,
        mode: RealtimeMode,
    ) -> Result<RealtimeSession>;

    /// Backend identifier for history / logging.
    fn name(&self) -> &'static str;

    /// PCM sample rate (Hz) the model expects on the mic-input stream.
    /// The capture path resamples to this before forwarding.
    fn native_input_rate(&self) -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_language_is_named_for_codes_fono_knows() {
        assert_eq!(language_note("ro").as_deref(), Some("Reply in Romanian."));
        assert_eq!(language_note("en").as_deref(), Some("Reply in English."));
        // Whitespace is incidental, not a different language.
        assert_eq!(language_note(" ro ").as_deref(), Some("Reply in Romanian."));
    }

    #[test]
    fn an_unknown_code_says_nothing_rather_than_something_confusing() {
        // "Reply in haw." asks the model to decode a language tag mid-sentence,
        // which is worse than falling back to the general behavioural rule.
        assert!(language_note("haw").is_none());
        assert!(language_note("").is_none());
        assert!(language_note("   ").is_none());
    }

    #[test]
    fn the_turn_notes_carry_language_then_speaker() {
        let ctx = AssistantContext {
            language: Some("ro".into()),
            speaker_note: Some("You are talking to Ana.".into()),
            ..Default::default()
        };
        assert_eq!(ctx.turn_notes().as_deref(), Some("Reply in Romanian. You are talking to Ana."));
    }

    #[test]
    fn either_note_alone_is_enough_and_neither_is_fine() {
        let lang = AssistantContext { language: Some("ro".into()), ..Default::default() };
        assert_eq!(lang.turn_notes().as_deref(), Some("Reply in Romanian."));

        let who = AssistantContext {
            speaker_note: Some("You are talking to Ana.".into()),
            ..Default::default()
        };
        assert_eq!(who.turn_notes().as_deref(), Some("You are talking to Ana."));

        assert!(AssistantContext::default().turn_notes().is_none());
    }

    #[test]
    fn the_notes_land_behind_everything_steady() {
        // The whole point of composing them separately: a change of language or
        // speaker must cost the notes, never the device list in front of them.
        let ctx = AssistantContext {
            system_prompt: "You are Fono.\n\nAreas: Kitchen, Office.".into(),
            instructions: Some("Keep replies short.".into()),
            language: Some("ro".into()),
            ..Default::default()
        };
        let block = ctx.system_block();
        assert!(block.starts_with("You are Fono.\n\nAreas: Kitchen, Office."), "{block}");
        assert!(block.ends_with("Reply in Romanian."), "{block}");
        // Rules after the context, notes after the rules.
        let rules = block.find("Keep replies short.").expect("instructions present");
        let notes = block.find("Reply in Romanian.").expect("language note present");
        assert!(rules < notes, "{block}");
    }

    #[test]
    fn a_turn_with_no_notes_leaves_the_head_byte_identical() {
        // A pinned prefix is only reusable while it stays a *byte* prefix of the
        // live prompt, so the no-notes path must not so much as trim.
        let head = "You are Fono.\n\nAreas: Kitchen.\n";
        assert_eq!(compose_system_prompt(head, None), head);
        assert_eq!(compose_system_prompt(head, Some("   ")), head);
    }
}
