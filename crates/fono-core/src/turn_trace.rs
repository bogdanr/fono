// SPDX-License-Identifier: GPL-3.0-only
//! Lightweight opt-in turn timeline recorder.
//!
//! Set `FONO_ASSISTANT_TRACE=/path/to/dir` to write Chrome Trace Event JSON
//! files. Open them in `chrome://tracing` or Perfetto to inspect the
//! keys → STT → polish/LLM → TTS → playback waterfall and the prompt-cache
//! decisions taken along the way.
//!
//! Three kinds of trace file are emitted (the env var may also point at an
//! explicit `.json` path, in which case that path is used verbatim):
//!
//! * `dictation-<id>.json` — one per F7 plain-dictation/polish turn; started at
//!   key-press time so the `keys` lane precedes STT.
//! * `assistant-<id>.json` — one per F8 assistant turn (`run_assistant_turn`).
//! * `startup-<id>.json` — one for the daemon's startup prewarm batch
//!   (`spawn_warmups`), written once every warmup task completes.
//!
//! Lanes (Chrome Trace `tid`s) are fixed; see [`KEYS_LANE`] for the taxonomy.
//! Lower-level crates emit via the ambient [`current_span`]/[`current_instant`]
//! helpers once a trace is installed with [`TurnTrace::make_current`], so they
//! never need a diagnostic parameter threaded through their public traits.
//! The cache scoreboard ([`TurnTrace::cache_scoreboard`]) rolls the recorded
//! cache events into `turn.finish` args for a one-glance hit/miss summary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use tracing::warn;

static NEXT_TRACE_ID: AtomicU64 = AtomicU64::new(1);
static CURRENT_TRACE: OnceLock<Mutex<Option<Weak<Inner>>>> = OnceLock::new();
/// Hot-path fast guard: `true` only while a trace is installed as
/// process-current. The per-token instant/span helpers ([`current_span`],
/// [`current_instant`], [`TurnTrace::current`]) load this with a single relaxed
/// atomic and bail before touching the [`CURRENT_TRACE`] mutex, so a normal
/// (untraced) dictation or assistant turn pays nothing for the instrumentation
/// sprinkled through the STT/LLM/TTS inner loops. Flipped on by
/// [`TurnTrace::make_current`] and back off when the guard drops.
static TRACE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Stable trace lane (Chrome Trace `tid`) names. Both the F7 (plain dictation /
/// polish) and F8 (assistant) paths render into the same fixed set of lanes so
/// a loaded trace always shows the pipeline in the same top-to-bottom order:
///
/// | lane            | stage                                                  |
/// |-----------------|--------------------------------------------------------|
/// | `keys`          | key press/release + hotkey FSM transitions             |
/// | `stt`           | speech-to-text                                         |
/// | `f7-polish`     | F7 plain-dictation cleanup (`polish.*`)                |
/// | `f7-inject`     | F7 streaming text injection (`polish.inject_*`)        |
/// | `llm`           | F8 assistant generation (`assistant.llm`)              |
/// | `tts`           | text-to-speech synthesis                               |
/// | `playback`      | audio playback                                         |
/// | `cache`         | prompt-state (KV) cache decisions for both F7 and F8   |
/// | `warmup`        | startup prewarm (startup trace file only)              |
/// | `assistant-pump`| turn lifecycle (`turn.start` / `turn.finish`)          |
pub const KEYS_LANE: &str = "keys";
/// See [`KEYS_LANE`] for the full lane taxonomy.
pub const STT_LANE: &str = "stt";
/// See [`KEYS_LANE`] for the full lane taxonomy.
pub const POLISH_LANE: &str = "f7-polish";
/// Streaming text-injection lane, rendered directly under [`POLISH_LANE`] so
/// per-chunk injection cost is visible running *concurrently* with cleanup
/// generation (the two compete for CPU during local streaming dictation).
/// See [`KEYS_LANE`] for the full lane taxonomy.
pub const INJECT_LANE: &str = "f7-inject";
/// See [`KEYS_LANE`] for the full lane taxonomy.
pub const LLM_LANE: &str = "llm";
/// See [`KEYS_LANE`] for the full lane taxonomy.
pub const CACHE_LANE: &str = "cache";
/// See [`KEYS_LANE`] for the full lane taxonomy.
pub const WARMUP_LANE: &str = "warmup";
/// See [`KEYS_LANE`] for the full lane taxonomy.
pub const PUMP_LANE: &str = "assistant-pump";

/// Cloneable handle for recording one assistant turn timeline.
#[derive(Clone)]
pub struct TurnTrace {
    inner: Arc<Inner>,
}

struct Inner {
    id: u64,
    started: Instant,
    path: PathBuf,
    events: Mutex<Vec<TraceEvent>>,
}

/// Guard that installs a trace as the process-current assistant trace.
///
/// This lets lower-level crates (local LLM / TTS internals) add events without
/// threading a diagnostic parameter through every trait. Fono runs one assistant
/// turn at a time, so this is intentionally simple and opt-in.
pub struct CurrentTraceGuard {
    previous: Option<Weak<Inner>>,
}

/// RAII duration event. Call [`Self::finish`] to record it with args; dropping
/// without finishing records a zero-arg duration.
pub struct TraceSpan {
    trace: Option<TurnTrace>,
    name: &'static str,
    cat: &'static str,
    tid: &'static str,
    started: Instant,
    finished: bool,
}

#[derive(Debug, Clone, Serialize)]
struct TraceEvent {
    name: String,
    cat: String,
    ph: &'static str,
    ts: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    dur: Option<u64>,
    pid: u32,
    tid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    s: Option<&'static str>,
    args: Value,
}

impl TurnTrace {
    /// Start a trace if `FONO_ASSISTANT_TRACE` is set.
    #[must_use]
    pub fn start_from_env() -> Option<Self> {
        Self::start_from_env_named("assistant")
    }

    /// Start a trace if `FONO_ASSISTANT_TRACE` is set, writing to a file whose
    /// name begins with `prefix` (e.g. `assistant-…`, `dictation-…`,
    /// `startup-…`). When the env var points at an explicit `.json` path the
    /// prefix is ignored and that path is used verbatim.
    #[must_use]
    pub fn start_from_env_named(prefix: &'static str) -> Option<Self> {
        let raw = std::env::var("FONO_ASSISTANT_TRACE").ok()?.trim().to_string();
        if raw.is_empty() || raw == "0" || raw.eq_ignore_ascii_case("false") {
            return None;
        }
        Some(Self::start_in_named(Path::new(&raw), prefix))
    }

    /// Start a trace in a directory or at an explicit JSON path.
    #[must_use]
    pub fn start_in(base: &Path) -> Self {
        Self::start_in_named(base, "assistant")
    }

    /// Start a trace in a directory (using `prefix` for the generated file name)
    /// or at an explicit JSON path.
    #[must_use]
    pub fn start_in_named(base: &Path, prefix: &'static str) -> Self {
        let id = NEXT_TRACE_ID.fetch_add(1, Ordering::Relaxed);
        let path = trace_path(base, prefix, id);
        Self {
            inner: Arc::new(Inner {
                id,
                started: Instant::now(),
                path,
                events: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Path where this trace will be written on [`Self::finish`].
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Numeric turn id unique within this process.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.inner.id
    }

    /// Install this trace as process-current until the returned guard drops.
    #[must_use]
    pub fn make_current(&self) -> CurrentTraceGuard {
        let slot = CURRENT_TRACE.get_or_init(|| Mutex::new(None));
        let previous = slot
            .lock()
            .expect("turn trace current mutex poisoned")
            .replace(Arc::downgrade(&self.inner));
        TRACE_ACTIVE.store(true, Ordering::Release);
        CurrentTraceGuard { previous }
    }

    /// Return the process-current trace, if tracing is enabled for this turn.
    #[must_use]
    pub fn current() -> Option<Self> {
        // Hot path: one relaxed load short-circuits the mutex when no trace is
        // installed. This is what keeps the inner-loop `current_instant`
        // (per-token) and `current_span` calls free on untraced turns.
        if !TRACE_ACTIVE.load(Ordering::Acquire) {
            return None;
        }
        let slot = CURRENT_TRACE.get()?;
        let guard = slot.lock().expect("turn trace current mutex poisoned");
        guard.as_ref().and_then(Weak::upgrade).map(|inner| Self { inner })
    }

    /// Start a duration span on a trace lane.
    #[must_use]
    pub fn span(&self, name: &'static str, cat: &'static str, tid: &'static str) -> TraceSpan {
        TraceSpan {
            trace: Some(self.clone()),
            name,
            cat,
            tid,
            started: Instant::now(),
            finished: false,
        }
    }

    /// Record an instant event at the current timestamp.
    pub fn instant(&self, name: &str, cat: &str, tid: &str, args: Value) {
        self.push_event(TraceEvent {
            name: name.to_string(),
            cat: cat.to_string(),
            ph: "i",
            ts: self.ts_us(Instant::now()),
            dur: None,
            pid: 1,
            tid: tid.to_string(),
            s: Some("t"),
            args,
        });
    }

    /// Record a complete duration event using external start/end instants.
    pub fn duration_between(
        &self,
        name: &str,
        cat: &str,
        tid: &str,
        started: Instant,
        ended: Instant,
        args: Value,
    ) {
        self.push_event(TraceEvent {
            name: name.to_string(),
            cat: cat.to_string(),
            ph: "X",
            ts: self.ts_us(started),
            dur: Some(ended.saturating_duration_since(started).as_micros() as u64),
            pid: 1,
            tid: tid.to_string(),
            s: None,
            args,
        });
    }

    /// Write the trace file. Safe to call once at turn end.
    pub fn finish(&self, args: Value) {
        self.instant("turn.finish", "assistant", "assistant-pump", args);
        if let Some(parent) = self.inner.path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!(target: "fono::trace", path = %parent.display(), error = %e, "failed to create trace directory");
                return;
            }
        }
        let trace_events =
            self.inner.events.lock().expect("turn trace events mutex poisoned").clone();
        let payload = json!({
            "traceEvents": trace_events,
            "displayTimeUnit": "ms",
            "metadata": {
                "name": "fono assistant turn",
                "turn_id": self.inner.id,
            }
        });
        match serde_json::to_vec_pretty(&payload) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&self.inner.path, bytes) {
                    warn!(target: "fono::trace", path = %self.inner.path.display(), error = %e, "failed to write assistant trace");
                } else {
                    tracing::info!(target: "fono::trace", path = %self.inner.path.display(), "assistant trace written");
                }
            }
            Err(e) => warn!(target: "fono::trace", error = %e, "failed to encode assistant trace"),
        }
    }

    /// Roll up the cache events recorded so far into a one-glance scoreboard:
    /// `{cache_hits, cache_misses, cold_prefills, bytes_restored}`. A **hit** is
    /// any turn that restored cached state — exact-key OR longest-prefix — i.e.
    /// a `*prompt_cache_restored` event; a **miss** is a genuine
    /// `*prompt_cache_cold_prefill` (nothing reusable). The exact-key
    /// `prompt_cache_lookup` probe missing is NOT a miss on its own: the
    /// longest-prefix fallback usually restores anyway, so counting it would
    /// understate the cache. Works for both the F7 and F8 lanes.
    #[must_use]
    pub fn cache_scoreboard(&self) -> Value {
        let events = self.inner.events.lock().expect("turn trace events mutex poisoned");
        let mut hits = 0_u64;
        let mut cold_prefills = 0_u64;
        let mut bytes_restored = 0_u64;
        for ev in events.iter() {
            if ev.name.ends_with("prompt_cache_restored") {
                hits += 1;
                bytes_restored +=
                    ev.args.get("restored_bytes").and_then(Value::as_u64).unwrap_or(0);
            } else if ev.name.ends_with("prompt_cache_cold_prefill") {
                cold_prefills += 1;
            }
        }
        drop(events);
        json!({
            "cache_hits": hits,
            "cache_misses": cold_prefills,
            "cold_prefills": cold_prefills,
            "bytes_restored": bytes_restored,
        })
    }

    fn push_event(&self, event: TraceEvent) {
        self.inner.events.lock().expect("turn trace events mutex poisoned").push(event);
    }

    fn ts_us(&self, instant: Instant) -> u64 {
        instant.saturating_duration_since(self.inner.started).as_micros() as u64
    }
}

impl CurrentTraceGuard {
    /// Keep the guard alive but intentionally clear the ambient trace now.
    pub fn clear(self) {
        drop(self);
    }
}

impl Drop for CurrentTraceGuard {
    fn drop(&mut self) {
        let slot = CURRENT_TRACE.get_or_init(|| Mutex::new(None));
        let mut guard = slot.lock().expect("turn trace current mutex poisoned");
        let previous = self.previous.take();
        // Keep the fast guard truthful: only stay active if a parent trace is
        // still installed underneath us.
        TRACE_ACTIVE.store(previous.is_some(), Ordering::Release);
        *guard = previous;
    }
}

impl TraceSpan {
    /// Create a disabled span for code paths that should not branch on tracing.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            trace: None,
            name: "disabled",
            cat: "disabled",
            tid: "disabled",
            started: Instant::now(),
            finished: true,
        }
    }

    /// Finish the duration span with structured args.
    pub fn finish(mut self, args: Value) {
        self.finish_inner(args);
    }

    fn finish_inner(&mut self, args: Value) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Some(trace) = &self.trace {
            trace.duration_between(
                self.name,
                self.cat,
                self.tid,
                self.started,
                Instant::now(),
                args,
            );
        }
    }
}

impl Drop for TraceSpan {
    fn drop(&mut self) {
        self.finish_inner(json!({}));
    }
}

/// Start a span on the current assistant trace, or return a disabled span.
#[must_use]
pub fn current_span(name: &'static str, cat: &'static str, tid: &'static str) -> TraceSpan {
    TurnTrace::current().map_or_else(TraceSpan::disabled, |trace| trace.span(name, cat, tid))
}

/// Record an instant on the current assistant trace, if one is active.
pub fn current_instant(name: &str, cat: &str, tid: &str, args: Value) {
    if let Some(trace) = TurnTrace::current() {
        trace.instant(name, cat, tid, args);
    }
}

/// Emit `llm.prompt_cache_evicted` / `llm.prompt_cache_pinned` instants on the
/// `cache` lane from a [`CacheMutationReport`](crate::prompt_cache::CacheMutationReport).
///
/// This is the bridge that lets the llama-agnostic `prompt_cache` data structure
/// surface eviction/pinning churn to the waterfall: the cache returns the facts,
/// the backend caller hands them here, and the trace event is written from this
/// (still llama-free) module. No-op when tracing is disabled for the turn.
pub fn record_cache_mutation(report: &crate::prompt_cache::CacheMutationReport) {
    if report.evicted.is_empty() && report.pruned.is_empty() && report.pinned.is_none() {
        return;
    }
    let Some(trace) = TurnTrace::current() else {
        return;
    };
    for ev in &report.evicted {
        trace.instant(
            "llm.prompt_cache_evicted",
            "cache",
            CACHE_LANE,
            json!({
                "layer": ev.layer.as_str(),
                "token_count": ev.token_count,
                "bytes": ev.bytes,
            }),
        );
    }
    for ev in &report.pruned {
        trace.instant(
            "llm.prompt_cache_pruned",
            "cache",
            CACHE_LANE,
            json!({
                "layer": ev.layer.as_str(),
                "token_count": ev.token_count,
                "bytes": ev.bytes,
            }),
        );
    }
    if let Some(layer) = &report.pinned {
        trace.instant(
            "llm.prompt_cache_pinned",
            "cache",
            CACHE_LANE,
            json!({
                "layer": layer.as_str(),
                "pin_released": report.pin_released.as_ref().map(crate::prompt_cache::PromptStateCacheLayer::as_str),
            }),
        );
    }
}

/// Compute decode throughput in tokens/second, rounded to one decimal place.
///
/// Shared by the F7 (`polish.generate`) and F8 (`llm.generate`) paths so both
/// report `tok_per_sec` identically (same formula, same rounding). Returns
/// `0.0` for a zero-length generation window to avoid a divide-by-zero.
#[must_use]
pub fn tokens_per_second(tokens: u32, gen_ms: u64) -> f64 {
    if gen_ms == 0 {
        return 0.0;
    }
    let raw = f64::from(tokens) * 1000.0 / gen_ms as f64;
    (raw * 10.0).round() / 10.0
}

/// Build the canonical generation-span arg object shared by the F7 polish and
/// F8 assistant LLM phases.
///
/// Having one constructor is the single source of truth for the generation
/// metric schema, so the two paths can never silently drift apart. Both emit a
/// single duration span (`polish.generate` / `llm.generate`) carrying exactly
/// these keys:
///
/// * `tokens` — decoded tokens (greedy ⇒ one per step)
/// * `chars` — `char`-count of the decoded text (NOT byte length)
/// * `deltas` — number of streamed pieces / `on_token` flushes
/// * `ttft_ms` — time-to-first-token latency
/// * `gen_ms` — generation wall-clock
/// * `tok_per_sec` — [`tokens_per_second`] over `tokens`/`gen_ms`
/// * `start_pos` — KV-cache position generation began at
/// * `stop_reason` — why the loop ended (`eos`, `stop_seq`, `max_tokens`, …)
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn generation_span_args(
    tokens: u32,
    chars: usize,
    deltas: u32,
    ttft_ms: u64,
    gen_ms: u64,
    start_pos: i32,
    stop_reason: &str,
) -> Value {
    json!({
        "tokens": tokens,
        "chars": chars,
        "deltas": deltas,
        "ttft_ms": ttft_ms,
        "gen_ms": gen_ms,
        "tok_per_sec": tokens_per_second(tokens, gen_ms),
        "start_pos": start_pos,
        "stop_reason": stop_reason,
    })
}

fn trace_path(base: &Path, prefix: &str, id: u64) -> PathBuf {
    if base.extension().is_some_and(|ext| ext == "json") {
        return base.to_path_buf();
    }
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    base.join(format!("{prefix}-{epoch}-{id:04}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoreboard_counts_prefix_restore_as_hit_not_miss() {
        // A live F8 turn: the exact-key lookup misses, but the longest-prefix
        // fallback restores. That is a HIT, not a miss.
        let trace = TurnTrace::start_in(Path::new("/tmp"));
        trace.instant("llm.prompt_cache_lookup", "cache", "cache", json!({ "hit": false }));
        trace.instant("llm.prompt_cache_prefix_match", "cache", "cache", json!({}));
        trace.instant(
            "llm.prompt_cache_restored",
            "assistant.llm",
            "llm",
            json!({ "restored_bytes": 4096 }),
        );
        let board = trace.cache_scoreboard();
        assert_eq!(board["cache_hits"], 1);
        assert_eq!(board["cache_misses"], 0);
        assert_eq!(board["cold_prefills"], 0);
        assert_eq!(board["bytes_restored"], 4096);
    }

    #[test]
    fn scoreboard_counts_cold_prefill_as_miss() {
        let trace = TurnTrace::start_in(Path::new("/tmp"));
        trace.instant("llm.prompt_cache_lookup", "cache", "cache", json!({ "hit": false }));
        trace.instant("llm.prompt_cache_cold_prefill", "cache", "cache", json!({}));
        let board = trace.cache_scoreboard();
        assert_eq!(board["cache_hits"], 0);
        assert_eq!(board["cache_misses"], 1);
        assert_eq!(board["cold_prefills"], 1);
        assert_eq!(board["bytes_restored"], 0);
    }

    #[test]
    fn tokens_per_second_rounds_to_one_decimal() {
        assert!((tokens_per_second(0, 0) - 0.0).abs() < f64::EPSILON);
        assert!((tokens_per_second(57, 0) - 0.0).abs() < f64::EPSILON);
        // 57 tokens in 2195 ms = 25.968… → 26.0
        assert!((tokens_per_second(57, 2195) - 26.0).abs() < f64::EPSILON);
        // 56 tokens in 3606 ms = 15.529… → 15.5
        assert!((tokens_per_second(56, 3606) - 15.5).abs() < f64::EPSILON);
    }

    #[test]
    fn generation_span_args_carries_canonical_schema() {
        let args = generation_span_args(57, 240, 57, 12, 2195, 119, "eos");
        assert_eq!(args["tokens"], 57);
        assert_eq!(args["chars"], 240);
        assert_eq!(args["deltas"], 57);
        assert_eq!(args["ttft_ms"], 12);
        assert_eq!(args["gen_ms"], 2195);
        assert!((args["tok_per_sec"].as_f64().unwrap() - 26.0).abs() < f64::EPSILON);
        assert_eq!(args["start_pos"], 119);
        assert_eq!(args["stop_reason"], "eos");
    }
}
