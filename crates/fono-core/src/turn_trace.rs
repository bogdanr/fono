// SPDX-License-Identifier: GPL-3.0-only
//! Lightweight opt-in assistant turn timeline recorder.
//!
//! Set `FONO_ASSISTANT_TRACE=/path/to/dir` to write one Chrome Trace Event JSON
//! file per assistant turn. Open the file in `chrome://tracing` or Perfetto to
//! inspect the STT → LLM → TTS → playback waterfall.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use tracing::warn;

static NEXT_TRACE_ID: AtomicU64 = AtomicU64::new(1);
static CURRENT_TRACE: OnceLock<Mutex<Option<Weak<Inner>>>> = OnceLock::new();

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
        let raw = std::env::var("FONO_ASSISTANT_TRACE").ok()?.trim().to_string();
        if raw.is_empty() || raw == "0" || raw.eq_ignore_ascii_case("false") {
            return None;
        }
        Some(Self::start_in(Path::new(&raw)))
    }

    /// Start a trace in a directory or at an explicit JSON path.
    #[must_use]
    pub fn start_in(base: &Path) -> Self {
        let id = NEXT_TRACE_ID.fetch_add(1, Ordering::Relaxed);
        let path = trace_path(base, id);
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
        CurrentTraceGuard { previous }
    }

    /// Return the process-current trace, if tracing is enabled for this turn.
    #[must_use]
    pub fn current() -> Option<Self> {
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
        *guard = self.previous.take();
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

fn trace_path(base: &Path, id: u64) -> PathBuf {
    if base.extension().is_some_and(|ext| ext == "json") {
        return base.to_path_buf();
    }
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    base.join(format!("assistant-{epoch}-{id:04}.json"))
}
