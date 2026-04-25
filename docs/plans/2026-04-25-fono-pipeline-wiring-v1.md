<!-- SPDX-License-Identifier: GPL-3.0-only -->
# Fono — Wire The Core Dictation Pipeline (v1)

Date: 2026-04-25
Prereq: `docs/plans/2026-04-24-fono-design-v1.md` (Phases 0–10).
Successor: `docs/plans/2026-04-25-fono-latency-v1.md` (depends on this plan
landing first).

## Objective

Replace the placeholder shim in `crates/fono/src/daemon.rs:72-109` with a
real session orchestrator so that pressing the configured hotkey produces:

```
capture  ──►  resample 16 kHz mono  ──►  STT  ──►  (LLM cleanup?)  ──►
inject  ──►  history row  ──►  ProcessingDone
```

Both **cloud** (Groq STT + Cerebras/Groq/OpenAI LLM) and **local**
(`whisper-rs`) paths must work end-to-end. `fono record` gains the same
pipeline as a one-shot CLI so users can smoke-test without a running
daemon.

## Why this is needed

The building blocks all exist and are tested individually, but the
glue is intentionally stubbed:

* `crates/fono/src/daemon.rs:72-109` — FSM consumer logs events and, on
  `StopRecording`, sleeps 150 ms then sends a synthetic `ProcessingDone`.
  No `AudioCapture::start()` call; no STT; no LLM; no `type_text`; no
  history write.
* `crates/fono/src/cli.rs:167-175` — `fono record` prints "scheduled for
  a follow-up phase" and returns.
* `crates/fono-llm/src/llama_local.rs:28-40` — `format()` always returns
  `Err("not yet wired")`, yet `LlmBackend::Local` is the configured
  default at `crates/fono-core/src/config.rs:213-216`.
* `crates/fono-stt/src/whisper_local.rs:1-107` — implemented but behind
  the `whisper-local` Cargo feature, which is off by default.
* No factory anywhere turns `Config` + `Secrets` into
  `Box<dyn SpeechToText>` / `Box<dyn TextFormatter>`. Cloud backends
  exist (`GroqStt`, `OpenAiCompat`, `AnthropicLlm`) but nothing constructs
  them from config.
* `PasteLast` and `Cancel` paths are no-ops; `Cancel` doesn't tear down
  the cpal stream.

Net effect: pressing the hotkey flips tray state and the FSM, but no
audio ever reaches an STT model.

## Implementation Plan

### Backend factories (new shared layer)

* [x] **W1.** Add `crates/fono-stt/src/factory.rs` exposing
  `build_stt(config: &SttConfig, secrets: &Secrets, paths: &Paths) -> Result<Arc<dyn SpeechToText>>`.
  Match on `SttBackend::{Local, Groq, OpenAI, …}`. Resolve the API key via
  `api_key_ref` (env var first, then `secrets.toml`). Resolve the local
  model path via
  `paths.whisper_models_dir().join(format!("ggml-{}.bin", cfg.local.model))`.
  For providers not yet implemented (Deepgram, Cartesia, …) return
  `anyhow!("provider X not yet implemented; pick groq/openai/local")` so
  the daemon logs it and the user sees a doctor-visible error.

* [x] **W2.** Add `crates/fono-llm/src/factory.rs` with the analogous
  `build_llm(config: &LlmConfig, secrets: &Secrets, paths: &Paths) -> Result<Option<Arc<dyn TextFormatter>>>`.
  Return `Ok(None)` when `llm.enabled == false` or `backend == None`.
  Route `Groq/Cerebras/OpenAI/OpenRouter/Ollama` through
  `OpenAiCompat::*`, `Anthropic` through `AnthropicLlm`. `LlamaLocal`
  stays opt-in behind its feature flag.

* [x] **W3.** Add a small `defaults.rs` module in each crate that maps a
  provider to a sane default model when `cloud.model` is empty
  (Groq STT → `whisper-large-v3`; OpenAI STT → `whisper-1`; Cerebras LLM
  → `llama3.1-70b`; OpenAI LLM → `gpt-4o-mini`; Anthropic LLM →
  `claude-3-5-haiku-latest`). Wizard reuses the same table so first-run
  defaults match the factory's expectations.

### Session orchestrator (the missing glue)

* [x] **W4.** Create `crates/fono/src/session.rs` with a
  `SessionOrchestrator` owning:
  * `Arc<dyn SpeechToText>`,
  * `Option<Arc<dyn TextFormatter>>`,
  * `HistoryDb`,
  * `CaptureConfig`,
  * `Mutex<Option<CaptureHandle>>` for the live recording,
  * the `action_tx` channel back into the FSM,
  * a `Notify` to gate concurrent pipeline runs.

* [x] **W5.** `on_start_recording(mode)` — clear stale buffer, call
  `AudioCapture::new(cfg).start()?`, store the handle, optionally mute
  the default sink via `fono_audio::mute`, record a start timestamp.

* [x] **W6.** `on_stop_recording()` — drop the `CaptureHandle` to end the
  cpal stream, drain the `RecordingBuffer`, bail early (with a
  `notify-rust` "recording too short" toast) if `samples.is_empty()` or
  duration < 300 ms, then `tokio::spawn` the pipeline so the daemon loop
  is never blocked.

* [x] **W7.** Pipeline body: `stt.transcribe(&pcm, 16_000, lang)` → if
  `text.trim().is_empty()` notify and exit; `llm.format(&raw, &ctx)` when
  present, else pass raw; `fono_inject::type_text(final)`;
  `history.insert(HistoryRow { ts, duration_ms, raw, cleaned, app_class,
  app_title, stt_backend, llm_backend, language })`; finally
  `action_tx.send(HotkeyAction::ProcessingDone)`. On any error: toast,
  still emit `ProcessingDone`, log with `tracing::error!`.

* [x] **W8.** `on_cancel()` — drop the capture handle, discard the
  buffer, no STT call, flip tray to Idle, emit `ProcessingDone`.

* [x] **W9.** `on_paste_last()` — query `HistoryDb::recent(1)`, inject
  `cleaned.unwrap_or(raw)` via `fono_inject::type_text`, no FSM state
  change.

* [x] **W10.** Build `FormatContext` from `config.llm.prompt` + matched
  `context_rules`. Use `fono_inject::focus::detect()` for `app_class` +
  `app_title`; iterate `context_rules` for the first match on
  `window_class` / `window_title_regex`; concat `prompt_suffix`. When
  focus detection fails, fall back to no rule suffix — don't block the
  pipeline.

### Daemon re-wiring

* [x] **W11.** In `crates/fono/src/daemon.rs:22-110`, replace the
  placeholder FSM consumer with a `SessionOrchestrator`. Construct it
  once after config load via `build_stt` / `build_llm`; if STT
  construction fails, log a loud `error!` and enter a degraded mode
  where recording hotkeys notify the user instead of silently no-op.

* [x] **W12.** Route FSM events:
  * `StartRecording(_)` → `orch.on_start_recording`
  * `StopRecording`     → `orch.on_stop_recording`
  * `Cancel`            → `orch.on_cancel`
  * `PasteLast`         → `orch.on_paste_last`

  Delete the 150 ms synthetic `ProcessingDone` shim.

* [x] **W13.** Tray state changes move to the orchestrator (not the FSM
  consumer) so `Processing` stays lit until the pipeline task finishes,
  not just until `StopRecording` fires.

### CLI surface

* [x] **W14.** Replace the `fono record` stub in `cli.rs:167-175` with a
  real one-shot: load config + secrets, build STT/LLM via the factories,
  start `AudioCapture`, wait for `Ctrl-C` OR a simple silence timeout
  (reuse the VAD stub as a gate — *N* consecutive frames below threshold
  → stop), run the pipeline, print the cleaned text to stdout, optionally
  skip injection with `--no-inject`, exit. Standalone smoke test that
  doesn't need a running daemon.

* [x] **W15.** Add `fono transcribe <WAV_PATH>` — read a WAV, run it
  through the same factory-built STT+LLM, print the result. Lets users
  verify their API keys without a microphone, and feeds the bench
  fixtures (see latency plan).

### Default config sanity

* [x] **W16.** Change the default `LlmBackend` in
  `crates/fono-core/src/config.rs:213-216` from `Local` to `None` until
  `LlamaLocal` is actually implemented, so a fresh install doesn't crash
  on the first dictation attempt. The wizard still offers `Local`; it
  just isn't the silent default.

* [x] **W17.** Gate the daemon's model-preflight (`daemon.rs:30-32`) so
  it only downloads whisper when `stt.backend == Local` and only
  downloads an LLM gguf when `llm.backend == Local`. Today it tries to
  download Qwen on every startup even for users who picked a cloud LLM.

### Doctor + observability

* [x] **W18.** Extend `crates/fono/src/doctor.rs` to actually exercise the
  factories: "STT: groq (api key found, endpoint reachable: yes)",
  "LLM: cerebras (api key found, endpoint reachable: yes)", "Audio:
  default input = …, 48000 Hz, resample → 16000", "Inject: wtype
  detected". Use short `reqwest` HEAD/GET with a 2-second timeout. A
  one-shot way to verify the plumbing without speaking.

* [x] **W19.** `tracing::info!` breadcrumbs at each pipeline stage with
  durations: "capture: 2100ms / 33600 samples", "stt: groq 540ms → 42
  chars", "llm: cerebras 310ms → 45 chars", "inject: wtype 12ms".
  Crucial for diagnosing "it feels slow".

### Safety and correctness

* [x] **W20.** Hard-cap in-flight pipeline to one at a time — refuse
  `on_start_recording` while a previous task is still running and emit a
  toast.

* [x] **W21.** Integration-style test in `crates/fono/tests/pipeline.rs`
  using a fake `SpeechToText` and fake `TextFormatter` that return canned
  strings, feeding a synthetic PCM buffer through `SessionOrchestrator`
  and asserting a history row lands with the right
  `raw`/`cleaned`/`stt_backend`/`llm_backend`. No network, no real audio
  device.

* [x] **W22.** Update `docs/status.md` to move pipeline wiring into a
  "v0.1.0-rc" section once these items land.

## Verification Criteria

* `fono wizard` → cloud → Groq → key → Cerebras → key → write leaves the
  daemon starting without errors; `fono doctor` shows both providers
  reachable.
* With the daemon running, pressing `Ctrl+Alt+Space`, speaking a 5-second
  sentence, pressing again injects the cleaned sentence into the focused
  window within 2 seconds; raw + cleaned strings appear in
  `fono history --limit 1`.
* `fono record` invoked on a TTY transcribes a spoken utterance and
  prints the cleaned text to stdout; `--no-inject` is respected.
* `fono transcribe sample.wav` produces cleaned text against a canned WAV
  with no microphone involved.
* `Ctrl+Alt+Grave` hold-to-talk: pressing, speaking, releasing injects
  text; releasing with < 300 ms of audio produces a "too short"
  notification, not an empty paste.
* Cancelling with `Escape` mid-recording discards audio, returns tray to
  `Idle`, writes no history row.
* `cargo test --workspace --lib` remains green; new `pipeline.rs`
  integration test passes without network access.
* `cargo clippy --workspace --no-deps -- -D warnings` stays clean with
  pedantic + nursery.

## Risks and Mitigations

1. **`LlamaLocal` is a stub and is today's default LLM.**
   Task W16 flips the default to `LlmBackend::None` until Llama is
   wired; users who want local LLM still pick it in the wizard.

2. **`WhisperLocal` is feature-gated and off by default.**
   Recommendation: enable `whisper-local` by default at the binary
   level, keep the feature gate at the library crate level. Matches the
   "single-binary, zero-config" promise.

3. **cpal teardown from an async worker on some backends.**
   Own the `CaptureHandle` behind `Arc<Mutex<Option<…>>>` and drop via
   `tokio::task::spawn_blocking` to avoid ALSA/PipeWire teardown issues.

4. **Wayland injection without `wtype`/`ydotool` fails silently.**
   `Injector::detect()` already returns `Injector::None`; surface that
   as a `notify-rust` toast at orchestrator startup, not at inject time.

5. **History DB grows unboundedly.**
   Add a once-per-hour tokio interval pruning so long-running sessions
   also retain `retention_days`.

## Sequencing

1. W1–W3 (factories + defaults) — pure library work.
2. W16–W17 (config defaults, preflight gating) — tiny, unblocks below.
3. W4–W10 (orchestrator) — the real work.
4. W11–W13 (daemon re-wire) — one file, one commit.
5. W14 (`fono record`) — quickest user-visible smoke test.
6. W15 (`fono transcribe`) — feeds bench fixtures.
7. W18–W19 (doctor + tracing) — diagnostic polish.
8. W20–W21 (safety + integration test).
9. W22 (status update).
