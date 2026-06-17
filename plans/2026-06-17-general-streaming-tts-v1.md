# General Streaming TTS — provider-agnostic, server-aware, slow-machine-safe

## Objective

Introduce a **general intra-utterance audio-streaming capability** for text-to-speech that:

1. Applies to **any** TTS provider — cloud (Gemini `streamGenerateContent`,
   OpenAI, Cartesia, Deepgram) **and** local (Kokoro / Piper via `LocalRouter`),
   with a zero-change default for batch-only backends.
2. Is **transport-agnostic** so produced PCM can go to the local audio device
   today and to a remote client (server mode) later, without touching backends.
3. Is **safe on slow machines**: it must never trade an occasional inter-sentence
   gap (today's benign artifact) for mid-word underruns/stutter. Intra-utterance
   streaming is capability-gated and RTF-probed, with automatic fallback to the
   current sentence-batch behaviour.

This is a **separate task** from the Gemini latency fixes (reasoning-effort /
model selection) and a **companion** to the realtime Live API work (the same
sink abstraction consumes the Live WebSocket audio).

## Background — current state (grounded)

- **Trait**: `TextToSpeech::synthesize()` returns one whole-utterance
  `TtsAudio { pcm, sample_rate }` — no streaming
  (`crates/fono-tts/src/traits.rs:17-46`).
- **Two existing layers of "streaming"**:
  - *Sentence-level* (already present and underrun-safe): the assistant pump
    splits LLM output with `SentenceSplitter` and synth+enqueues per sentence
    (`crates/fono/src/assistant.rs` `synth_and_enqueue`, ~`:657-768`,
    `:893`+). `fono speak --stream` does the same with backpressure
    `MAX_PENDING = 5` (`crates/fono/src/speak_stream.rs:32,99-120`).
  - *Intra-utterance* (does **not** exist): a single synth call emitting PCM as
    produced. This is the new capability.
- **Playback**: `AudioPlayback::{new,enqueue,is_idle,stop}`
  (`crates/fono-audio/src/playback.rs:97-156`). A background worker feeds a cpal
  ring (`VecDeque`, ~96k samples ≈ 2 s @ 48 kHz, `:332`), resamples per
  `sample_rate` (`:393`+), and **waits for the ring to drain between `Play`
  commands** (`:430`). Ring depth is tracked in `in_flight` (`:349`).
- **Server mode**: `fono mcp serve` synthesises whole text and plays it on the
  **server host's** device, serialised by a mutex across concurrent speak calls
  (`crates/fono-mcp-server/src/voice_io.rs:580-634`, lock at `:613`). There is
  **no remote-audio transport** today.
- **Local engines**: Kokoro (English, ONNX, pinned to **one** inference thread —
  `crates/fono-tts/src/kokoro.rs:237`, serialised per session via `Mutex`,
  `:205`) and Piper (other languages), dispatched by `LocalRouter`
  (`crates/fono-tts/src/local_router.rs:339-377`). Synthesis runs on
  `spawn_blocking`; output is whole-utterance.

## The slow-machine risk (the design's central constraint)

Define **RTF = synth_wall_time / audio_duration_produced**.

- Current **sentence-batch** pipeline is underrun-proof *within* a sentence:
  audio is enqueued only after the whole sentence is synthesised, and the worker
  drains between `Play`s. Slow synth → benign **inter-sentence silence**.
- **Intra-utterance streaming** plays chunk 1 while later chunks synthesise. If
  RTF ≥ 1 the consumer overtakes the producer → **mid-word ring underrun /
  stutter** — much worse than a gap.
- Local RTF is hardware-dependent: ~0.3–0.7 on a modern desktop core (safe), but
  can reach ~1–3 on low-power CPUs (Kokoro is single-threaded). Contention
  (local LLM/STT) and **server concurrency** (per-engine `Mutex` + audio-output
  mutex) multiply effective RTF; an N-client server with local TTS is the worst
  case.

**Therefore**: intra-utterance streaming MUST be gated, never unconditional, and
must degrade to sentence-batch automatically.

## Architecture (conceptual)

1. **Trait extension with a safe default.** Add
   `synthesize_stream(text, voice, lang) -> Result<Stream<Result<TtsChunk>>>`
   to `TextToSpeech`, where `TtsChunk { pcm, sample_rate, final: bool }`. The
   **default implementation calls `synthesize` and yields a single terminal
   chunk**, so every existing backend works untouched. Add capability hints:
   `fn supports_streaming(&self) -> bool` (default `false`) and
   `fn streaming_realtime_safe(&self) -> Option<bool>` (`Some(true)` for cloud
   streaming backends, `None` = "unknown, probe it" for local, `Some(false)`
   to force batch).
2. **Transport-agnostic sink.** New `PcmSink` trait
   (`push_chunk(pcm, sample_rate)`, `is_idle()`, `stop()`, ring-depth query).
   Implement `LocalPlaybackSink` over `AudioPlayback` now; define the trait so a
   future `ChannelSink` / network sink (server → remote client) and the realtime
   Live audio path plug in without backend changes.
3. **Continuous playback mode.** Extend `fono-audio` so chunks of the *same*
   utterance append to the ring **without** the drain-between-`Play` gap (e.g. a
   `begin_stream` / `push` / `end_stream` API or a "no-drain-until-final" flag).
   Expose ring depth / underrun signal from `in_flight` (`playback.rs:349`).
4. **One stream driver.** A single consumer drives `synthesize_stream` → sink,
   applying: a **prebuffer lead** (start playback only after `stream_prebuffer_ms`
   or K chunks), **online underrun detection**, and **adaptive fallback** (grow
   prebuffer; after repeated underruns disable intra-utterance streaming for that
   backend for the session and revert to batch). The assistant pump,
   `fono speak --stream`, and the MCP speak path all call this driver.
5. **Local RTF probe.** At startup / first use, synth a short fixed phrase per
   local engine, measure RTF, and enable intra-utterance streaming only if
   RTF ≤ a safety threshold (e.g. ≤ 0.6) with margin; cache the result.
   Otherwise stay in sentence-batch (today's safe mode).
6. **Server-mode policy.** Default the server to **not** intra-utterance-stream
   with local engines (or cap concurrency); recommend cloud TTS or a fast
   dedicated host for low-latency server streaming. Document that the network
   adds its own jitter buffer, so whole-sentence chunks over the wire are fine.
7. **Gemini streaming backend** as the first real override:
   `:streamGenerateContent?alt=sse`, decode incremental `inlineData` PCM frames
   into `TtsChunk`s (realtime-safe = `Some(true)`).

## Implementation Plan

- [ ] **S1. Trait + chunk type.** Add `TtsChunk`, `synthesize_stream` (default
  wraps `synthesize`), `supports_streaming`, `streaming_realtime_safe` to
  `crates/fono-tts/src/traits.rs`. No backend changes required to compile.
  Rationale: establishes the universal contract with zero regression risk.
- [ ] **S2. PcmSink abstraction.** Define `PcmSink` and `LocalPlaybackSink`
  (wraps `AudioPlayback`). Keep it in a place both `fono` and future server
  transports can use. Rationale: decouples production from consumption so server
  /remote and realtime paths reuse it.
- [ ] **S3. Continuous playback mode.** Add a no-drain-between-chunks streaming
  enqueue path to `crates/fono-audio/src/playback.rs` and expose ring depth /
  underrun status. Rationale: the current drain-between-`Play` behaviour injects
  gaps between same-utterance chunks; streaming needs gapless append + a buffer
  signal for the driver.
- [ ] **S4. Stream driver with prebuffer + adaptive fallback.** Implement the
  single driver (prebuffer lead, underrun detection, revert-to-batch). Add config
  `[tts].stream_prebuffer_ms` (default ~250–400 ms) and an enable/auto/off knob
  `[tts].intra_utterance_streaming`. Rationale: the slow-machine safety lives
  here; one driver keeps assistant / speak-stream / MCP consistent.
- [ ] **S5. Local RTF probe + gating.** Add a short-phrase RTF benchmark for
  Kokoro/Piper engines; cache and use it (plus `streaming_realtime_safe`) to
  decide per-engine whether to stream or batch. Rationale: the concrete
  "handles slow machines" mechanism.
- [ ] **S6. Wire the three consumers to the driver.** Route the assistant pump
  (`crates/fono/src/assistant.rs`), `fono speak --stream`
  (`crates/fono/src/speak_stream.rs`), and MCP speak
  (`crates/fono-mcp-server/src/voice_io.rs`) through S4. Preserve current
  behaviour exactly when streaming is disabled / not safe. Rationale: single
  integration point; batch path remains the safe default.
- [ ] **S7. Server-mode policy + concurrency guard.** Default server to batch
  with local engines (or cap concurrent local synth); document the policy in
  `docs/providers.md`. Rationale: prevents routine underruns on shared hosts.
- [ ] **S8. First streaming backend — Gemini.** Override `synthesize_stream` on
  the Gemini TTS client (`crates/fono-tts/src/gemini.rs`) using
  `:streamGenerateContent?alt=sse`; mark realtime-safe. Rationale: proves the
  abstraction end-to-end and delivers the latency win for the all-Google stack.
- [ ] **S9. (Optional, later) Additional cloud overrides** (Cartesia / Deepgram /
  OpenAI streaming) and a **`ChannelSink`/network sink** for server→remote audio.
  Rationale: incremental value once the core lands; sink trait already supports it.
- [ ] **S10. Tests + docs.** Offline unit tests for: default-impl single-chunk
  wrapping, driver prebuffer/underrun/fallback state machine (with a synthetic
  slow-producer), RTF-probe gating decision, and gapless playback append. Update
  `docs/providers.md` (streaming support, slow-machine caveat, server policy) and
  `docs/status.md`.

## Verification Criteria

- Batch-only backends are byte-for-byte unchanged: `synthesize_stream` default
  yields exactly one chunk equal to `synthesize` output.
- With streaming enabled on a realtime-safe backend, time-to-first-audio drops
  below one-full-sentence synth latency in a measured assistant turn.
- A synthetic slow producer (RTF > 1) in tests triggers prebuffer growth and
  then session fallback to batch — **no mid-utterance underrun is surfaced** once
  fallback engages.
- Local engines on a slow host (or simulated via the probe threshold) stay in
  sentence-batch mode; no stutter regression vs. today.
- Server mode with local TTS defaults to batch / bounded concurrency; concurrent
  speak calls do not underrun.
- Pre-commit gate green: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`.

## Potential Risks and Mitigations

1. **Mid-word underruns on slow/contended machines.**
   Mitigation: capability gating + RTF probe + prebuffer + adaptive
   revert-to-batch; sentence-batch remains the default safe mode.
2. **Playback-layer regression (gaps or clicks at chunk boundaries).**
   Mitigation: dedicated continuous-append mode with resampler continuity across
   chunks; unit tests asserting gapless concatenation and a single resampler per
   utterance.
3. **Server concurrency amplifying RTF.**
   Mitigation: server defaults to batch with local engines and/or a concurrency
   cap; documented guidance to use cloud TTS for low-latency server streaming.
4. **Backend wire-format drift (Gemini streaming preview).**
   Mitigation: keep S8 behind the capability flag; offline tests on the chunk
   decoder; live verification with a real key before recommending as default.
5. **Complexity creep in the trait.**
   Mitigation: default impl keeps the trait additive; only streaming-capable
   backends override; sink/driver isolate policy from backends.

## Alternative Approaches

1. **Sentence-batch only (status quo) + smaller first chunk.** Synthesise a
   short opening clause first to cut ttfa, keep whole-utterance batch otherwise.
   Lower risk, no underrun exposure, modest ttfa gain; no true streaming. Good
   interim if S-series is deferred.
2. **Cloud-only streaming.** Implement `synthesize_stream` exclusively for
   realtime-safe cloud backends and never stream local engines. Simplest safe
   subset; drops the local low-latency goal but eliminates the RTF risk entirely.
3. **Push intra-utterance latency entirely to the realtime Live path.** Treat the
   Gemini Live WebSocket (v2 plan Sections F/G) as the only low-latency audio
   route and leave staged TTS batch. Defers this task but loses streaming for
   non-Gemini and non-realtime flows.
