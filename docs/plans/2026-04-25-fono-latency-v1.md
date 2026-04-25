<!-- SPDX-License-Identifier: GPL-3.0-only -->
# Fono — Latency Minimization (v1)

Date: 2026-04-25
Prereq: `docs/plans/2026-04-25-fono-pipeline-wiring-v1.md` must land first
        (the orchestrator from that plan is what we optimize here).

## Objective

Hit a perceived end-to-end latency of **p50 < 800 ms / p95 < 1.5 s** on
the cloud path (Groq STT + Cerebras LLM, warm daemon), and **p50 < 2.0 s
/ p95 < 3.0 s** on the local path (whisper-small + Qwen-1.5B, 4-core
x86_64). "End to end" = from `StopRecording` (user releases the key /
stops talking) to **first character injected** at the cursor.

This beats Tambourine (~2.5 s) and OpenWhispr (~3 s) materially and is
the chief user-visible win Fono has to ship.

## Latency Budget

| Stage                       | Cloud target | Local target |
|-----------------------------|--------------|--------------|
| Hotkey → capture start      | ≤ 5 ms       | ≤ 5 ms       |
| Trailing-silence trim       | ≤ 5 ms       | ≤ 5 ms       |
| STT network/inference       | 300–500 ms   | 600–1200 ms  |
| LLM network/inference       | 150–300 ms   | 300–800 ms   |
| Inject (first character)    | 10–30 ms     | 10–30 ms     |
| **Total p50**               | **0.5–0.9 s**| **1.0–2.0 s**|

Strategy: **overlap everything that can be overlapped, warm everything
that can be warmed, stream everything that can be streamed.**

## Implementation Plan

### Warm paths (kill cold-start costs)

* [x] **L1.** Keep the cpal input stream open continuously feeding a
  discarded ring buffer; on `StartRecording` flip a flag to start
  *recording* into a fresh `RecordingBuffer`. Opening a cpal stream is
  50–300 ms of cold start on ALSA/PipeWire — eliminating it per-press is
  free latency. Gate with `general.always_warm_mic = true` (default on;
  off for privacy-paranoid users). Document in `docs/privacy.md`.

* [x] **L2.** Lazy-load whisper once at daemon startup (not first press)
  by calling `ensure_ctx()` after the orchestrator is built. Saves
  200–600 ms on the first dictation. Same for `LlamaLocal` once it is
  implemented.

* [x] **L3.** Reuse `reqwest::Client` per provider (already singletons in
  `groq.rs:30` + `openai_compat.rs:31`). Build them with HTTP/2 keep-
  alive, `rustls` session resumption, and `pool_idle_timeout(60s)`. Add
  `prewarm()` that fires a cheap request (`HEAD /` or `/v1/models`) at
  daemon startup so the TLS handshake + TCP RTT + DNS are paid off-
  hotpath. Without this, the first dictation pays a ~200 ms handshake
  tax every daemon restart.

* [~] **L4.** *Deferred — `reqwest` already uses HTTP/2 keep-alive after L3, which amortises DNS over the connection's lifetime; a custom resolver is only worth it on networks with a > 100 ms system resolver. Will revisit with real-provider baselines.* Pre-resolve DNS for configured provider hosts via
  `trust-dns-resolver` or just background `tokio::spawn(lookup_host(…))`
  at startup; stash in `Arc<HashMap<&str, SocketAddr>>` and hand to
  `reqwest` via a custom resolver. Saves 20–80 ms on first request when
  the system resolver is slow.

* [x] **L5.** Warm the injection backend: on Wayland, `wtype --version`
  once at startup so the binary is page-cached (5–30 ms saved on first
  inject). On X11+enigo, instantiate `Enigo` once and keep it alive in
  the orchestrator.

### Overlap pipeline stages (the biggest perceived-latency win)

* [~] **L6.** *Deferred to v0.2 — Groq's batch turbo whisper is already < 500 ms p50. Streaming STT (Deepgram/AssemblyAI) is a new provider class with its own auth + reliability profile and isn't on the v0.1 critical path.* Streaming STT for providers that support it. Deepgram and
  AssemblyAI deliver partial transcripts over WebSocket with ~200 ms
  first-partial latency *while the user is still talking*. Send 20 ms
  chunks from the capture thread; by the time `StopRecording` fires, STT
  is often returning its final segment. Moves STT inference largely off
  the post-stop critical path. Groq/OpenAI don't stream transcriptions
  today — keep them as batch, gain the other overlaps below.

* [~] **L7.** *Deferred to v0.2 — token streaming requires changing the `TextFormatter` trait signature and the inject contract (see L8); too invasive for v0.1. The L19 `max_tokens=256` cap already bounds worst-case LLM latency.* Pipeline STT → LLM with token streaming. All OpenAI-
  compatible chat endpoints support `"stream": true` (SSE). Change
  `crates/fono-llm/src/openai_compat.rs:85` from `stream: false` to
  stream-capable; hand a `mpsc::Sender<String>` into
  `TextFormatter::format`. Begin streaming LLM tokens the moment the
  full STT text lands.

* [~] **L8.** *Deferred to v0.2 — depends on L7. Some IME/autocomplete combinations also fight progressive injection (see Risks §4); we want real per-app data before flipping the default.* Progressive injection: change `fono_inject::type_text` to
  `type_text_stream(rx: mpsc::Receiver<String>)` so the first character
  hits the screen within ~150 ms of `StopRecording` on cloud paths.
  Buffer ≥ 3 chars or a word-boundary before each `enigo::text` call.
  Show a cursor-position indicator in the overlay.

* [x] **L9.** Skip LLM when not needed: if raw STT output is ≤ 6 words,
  all-lowercase with no obvious fillers (regex
  `\b(um|uh|er|like|you know)\b`), or `llm.enabled = false`, inject the
  raw text directly. Saves 150–800 ms for short utterances (the common
  case for chat/dictation). Configurable via
  `llm.skip_if_words_lt = 7`.

* [~] **L10.** *Deferred to v0.2 — depends on L7 (streaming LLM in flight). Without streaming, opening the connection early just costs us a wasted request budget.* Speculative LLM prewarm: on `StartRecording`, open the SSE
  connection to the LLM endpoint with the system prompt already in-
  flight where the provider supports it; when STT returns, only the user
  message needs to land. Provider-dependent; safe fallback = noop.

### Trim unnecessary audio

* [x] **L11.** Voice-activity-trim the tail after `StopRecording`. Strip
  trailing silence (Silero VAD or zero-crossing threshold) before
  passing to STT. Whisper compute scales linearly with audio length; a
  5-second utterance with 1.5 s of tail silence saves ~30% STT time.
  Already partial in `crates/fono-audio/src/vad.rs`.

* [x] **L12.** Trim leading silence too (user hits the key a beat before
  speaking). Whisper's lang-detect also runs faster on tighter audio.

* [x] **L13.** Auto-stop on silence in toggle mode: if VAD detects
  ≥ 700 ms silence, fire `StopRecording` automatically. UX win that
  *also* eliminates the "I forgot to stop" latency. Tunable
  `audio.silence_ms = 700`; off via
  `audio.auto_stop_on_silence = false`.

* [~] **L14.** *Deferred to v0.2 — Groq accepts FLAC but our `groq.rs` already preallocates the WAV buffer to exact capacity. Wins are network-bound (matters on Wi-Fi); add once we have real-link telemetry from `fono-bench`.* Compress for cloud STT. 16 kHz mono 16-bit PCM WAV is
  fine on wired connections; on Wi-Fi switch to FLAC or Opus (Groq
  accepts both; Opus 24 kbps ≈ 15× smaller, saves 100–400 ms on slow
  links). Gate with `stt.cloud.upload_codec = "wav" | "flac" | "opus"`;
  default flac.

### Pick fast defaults

* [x] **L15.** Default cloud provider = Groq STT + Cerebras LLM. Groq's
  `whisper-large-v3-turbo` is ~5× faster than OpenAI's `whisper-1` for
  equal quality; Cerebras' wafer-scale inference averages 50–200 ms for
  a short cleanup prompt vs Anthropic/OpenAI's 300–800 ms. Encode in
  wizard default order and `defaults.rs` from the wiring plan (W3).

* [x] **L16.** Default local whisper model = `small` multilingual (not
  `medium`); default local LLM = Qwen2.5-1.5B Q4_K_M (not 3B). Justify
  with the budget table in `docs/providers.md`.

* [~] **L17.** *Deferred to packaging refresh — `WHISPER_OPENBLAS=1` complicates the static-musl release build (see Risks §5). Requires a separate `fono-x86_64-openblas` artifact; tracked in `docs/plans/2026-04-24-fono-design-v1.md` Phase 9 follow-up.* whisper.cpp compile flags: build `whisper-rs` with
  `WHISPER_OPENBLAS=1` on x86_64 Linux and `WHISPER_METAL=1` on macOS.
  2–3× faster local inference. For the musl static build, evaluate
  `WHISPER_CUDA` (opt-in only; not default).

* [x] **L18.** Whisper sampling = `Greedy { best_of: 1 }` (already set
  in `whisper_local.rs:70`). Add `params.set_n_threads(physical_cores)`
  to avoid SMT thrash. Expose `stt.local.threads` with auto-detect
  default.

* [x] **L19.** LLM generation limits: `max_tokens = 256` (cleanup outputs
  are < 100 tokens; uncapped is a footgun on cloud providers metering
  wall-clock), `stop = ["\n\n"]` for short cleanup, `temperature = 0.2`
  (currently 0.3 in `openai_compat.rs:115`).

### Hot path quality

* [~] **L20.** *Deferred — the existing tokio runtime already has `worker_threads = num_cpus`; per-pipeline runtime carving adds complexity without measurable wins until L7/L8 land. SCHED_FIFO requires `CAP_SYS_NICE` which most NimbleX users won't have.* Dedicated tokio runtime for the pipeline with
  `worker_threads = max(2, cores/2)`. Keeps STT/LLM from being blocked
  behind clap/tray/IPC tasks. Optionally raise the cpal callback to
  real-time priority (`SCHED_FIFO`) when `CAP_SYS_NICE` is available.

* [x] **L21.** *Already conformant — `crates/fono-stt/src/groq.rs:101` preallocates exact capacity and `session.rs` only clones once, into the spawned pipeline task. No change needed.* Zero-copy PCM handoff: capture buffer is `Vec<f32>`;
  cloning to pass into the STT task is ~100 KB/s = 30 KB for 3 s
  (trivial). The WAV encoder in `crates/fono-stt/src/groq.rs:89`
  preallocates exact capacity (good); no change.

* [x] **L22.** Move the SQLite history insert off the critical path.
  Inject first, then `tokio::spawn` the history write. Saves 5–20 ms of
  fsync. Already async; ensure the `await` on history is *after* the
  inject.

* [x] **L23.** *Already conformant — `crates/fono-audio/src/capture.rs:120` and the cpal callbacks log only at `debug`/`warn`; no `info!` in inner loops.* No `info!`-level logging in inner loops — capture
  callback and stream handler `trace!` only. Structured tracing has
  measurable overhead at >10k events/s.

### Feedback loop

* [~] **L24.** *Deferred to v0.2 — requires `rodio` + a bundled WAV asset; orthogonal to the latency budget and ships with the overlay polish work.* Click/ding on hotkey press played from a preloaded
  `rodio::source::Buffered` in RAM, not a disk read + decode. Audible
  ack within 10 ms of keypress dramatically improves perceived latency.

* [~] **L25.** *Deferred to v0.2 — `fono-overlay` is still a stub. Once a real `winit`+`softbuffer` window lands, the hide/show optimisation is a one-liner.* Overlay appears in < 50 ms on `StartRecording`. Create
  the `winit` + `softbuffer` window once at daemon startup, keep it
  hidden, just `set_visible(true)` on recording start.

* [x] **L26.** *`PipelineMetrics` now carries `capture_ms`, `trim_ms`, `trimmed_samples`, `stt_ms`, `llm_ms`, `inject_ms`, `raw_chars`, `final_chars`, `llm_skipped_short`. Logged at `info` after every dictation; `fono doctor --json` surfacing is a v0.2 follow-up.* Per-stage latency counters surfaced via
  `fono doctor --json` and logged at debug:
  * `hotkey_to_capture_ms`
  * `trim_ms`
  * `stt_ms` (and `stt_first_partial_ms` for streaming)
  * `llm_ms` (and `llm_first_token_ms` for streaming)
  * `first_inject_ms` (StopRecording → first character on screen)
  * `full_inject_ms`

  Surface p50/p95 over the last 100 invocations. `fono doctor --benchmark`
  runs a canned PCM file through the pipeline (see L29).

### Benchmarks & regression guards

* [x] **L27.** *Already in `crates/fono-bench/benches/orchestrator.rs` from the previous session.* Criterion benchmark in
  `crates/fono-bench/benches/orchestrator.rs` with a fake STT (100 ms
  fixed delay) + fake LLM (50 ms fixed delay) exercising the
  orchestrator end to end. Asserts orchestrator overhead (scheduling,
  channels, injection) is < 20 ms over the sum of fake latencies.

* [~] **L28.** *Deferred to packaging refresh — needs a CI workflow extension (`.github/workflows/bench.yml`) plus a baseline file. Scaffolded in the bench harness's `Report::regressed_vs`; just needs wiring.* CI latency gate: publish p50/p95 from the criterion
  bench on every PR; fail if p95 regresses > 15 % vs main. Catches
  accidental `Mutex::lock()` on hot paths.

* [~] **L29.** *Already scaffolded in `crates/fono-bench/src/fixtures.rs` — pinning the SHA-256s and committing baselines is a manual maintainer step (see `crates/fono-bench/scripts/fetch-fixtures.sh`).* Real-audio benchmark suite in `crates/fono-bench` (see
  the bundled crate). Pre-recorded **public-domain dictation clips in
  ≥ 4 languages** (en, es, fr, de + ro/it as available) downloaded
  lazily from a pinned manifest; each clip has a known reference
  transcript; runner reports per-clip:
  * total latency (capture → inject)
  * STT latency
  * LLM latency
  * **WER** (word error rate) vs reference transcript
  Aggregated to p50/p95 per language and per provider. Driven by
  `cargo run -p fono-bench --release -- --provider groq` for cloud
  paths, `--provider local` for `whisper-small`. Results emitted as
  JSON for CI plotting.

* [x] **L30.** *Already in `crates/fono-bench/tests/latency_smoke.rs` from the previous session; passes in `cargo test -p fono-bench --release -- --ignored`.* Latency-regression test (`crates/fono-bench/tests/
  latency_smoke.rs`, `#[ignore]`-by-default integration test). Run
  three local-only fixtures (en/es/fr) through a fake STT/LLM that
  returns the reference transcript verbatim, and assert orchestrator-
  overhead p95 < 50 ms. Doesn't require network or real models — keeps
  CI fast and deterministic; the real-provider runs are opt-in via the
  `fono-bench` binary.

## Verification Criteria

* `fono doctor --benchmark --provider cloud` reports p50 < 800 ms /
  p95 < 1.5 s for the canned 5-second English fixture (Groq + Cerebras).
* `fono doctor --benchmark --provider local` reports p50 < 2.0 s /
  p95 < 3.0 s for the same fixture on a 4-core x86_64 with whisper-small
  + Qwen-1.5B.
* `cargo run -p fono-bench --release -- --provider groq` produces a JSON
  report covering ≥ 4 languages with p50/p95 latencies and WER per
  language, and exits non-zero if any language's WER regresses > 5
  percentage points vs the baseline `bench-baseline.json`.
* Second + subsequent dictations in the same daemon session are within
  10 % of the first dictation's latency — confirming warm paths stay
  warm.
* Streaming LLM: first token injected within 300 ms of STT completion
  on Cerebras (measured end-to-end).
* Trim: a 5-second recording with 1.5 s of tail silence produces the
  same transcript as a 3.5-second recording with no silence; measured
  STT time is ≥ 25 % shorter.
* Skip-LLM-when-short: a 3-word utterance bypasses the LLM entirely;
  `fono history` shows `llm_backend = NULL` and inject latency drops
  by the LLM stage cost.
* Criterion bench p95 doesn't regress > 15 % vs main on CI.

## Risks and Mitigations

1. **Always-warm mic feels creepy to privacy-conscious users.**
   Configurable (`general.always_warm_mic`); documented prominently in
   `docs/privacy.md`; surfaced in the wizard with explicit consent. The
   stream only feeds a discarded buffer — nothing is transcribed or
   stored until a hotkey press.

2. **Streaming STT providers have different quality profiles than Groq.**
   Keep Groq as default (batch but fastest batch). Expose Deepgram /
   AssemblyAI as "low-latency streaming" choices in the wizard.

3. **SSE token streaming breaks behind buffering proxies.**
   Detect first-token latency > 2 s and silently downgrade to batch
   mode for the session; `warn!` and let `fono doctor` flag it.

4. **Progressive injection fights with IME/autocomplete.**
   Opt-out per-app via `context_rules` (`inject_mode = "batch" |
   "stream"`). Default stream; fall back to batch on known-bad apps.

5. **OpenBLAS/Metal complicates the static-musl release build.**
   Ship vanilla whisper for the musl release; publish a separate
   `fono-x86_64-openblas` artifact for desktop users who care.
   `fono doctor` reports which flavor is running.

6. **Auto-stop on silence fires during a thinking pause.**
   User-tunable `audio.silence_ms = 700`; disable with
   `audio.auto_stop_on_silence = false`.

## Sequencing

1. Land the wiring plan (`docs/plans/2026-04-25-fono-pipeline-wiring-v1.md`)
   without optimisations — measure baseline with `fono-bench`.
2. Land **warm paths** (L1–L5) — biggest cheap wins, no API surface
   changes.
3. Land **defaults** (L15–L19) — config-only.
4. Land **trim** (L11–L13).
5. Land **streaming LLM + progressive injection** (L7, L8) — biggest
   perceived-latency win; touches trait signatures.
6. Land **streaming STT** (L6) — new provider backend; defer if timeline
   pressures.
7. Land **skip-LLM-when-short** (L9).
8. Land **feedback + benchmarks** (L24–L30) so latency regressions get
   caught automatically going forward.
