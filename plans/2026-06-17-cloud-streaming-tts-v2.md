# Cloud Streaming TTS (cloud-first) — supersedes general-streaming-tts v1

> **Scope decision (2026-06-17):** do streaming TTS for **cloud providers only**.
> This is the safe, simple subset — cloud backends are realtime-safe by
> construction, so the slow-machine RTF machinery from v1 (probe, adaptive
> revert-to-batch, per-engine benchmark) is **dropped from this task** and
> deferred. Local streaming (Kokoro/Piper) remains future work; see the
> "Deferred" section.
>
> **Prerequisite / separate quick win:** the Gemini thinking fix
> (`reasoning_effort: "low"` for the polish + assistant OpenAI-compat clients)
> is the highest-impact latency change and is independent of this task — land it
> first.

## Objective

Add **intra-utterance audio streaming for cloud TTS providers** so the first
audio of a sentence plays before the whole sentence is synthesised, cutting
time-to-first-audio. Local engines and batch-only backends keep today's
behaviour unchanged via a default trait impl. Stay safe with a small fixed
network prebuffer — no RTF probing, no underrun fallback state machine.

## Why cloud-first is simple and safe

- Cloud streaming models emit audio **faster than realtime**; the only jitter is
  network, absorbed by a small fixed prebuffer (~150–300 ms). No mid-word
  underrun risk like CPU-bound local engines (RTF ≥ 1).
- The trait change is **additive**: `synthesize_stream` defaults to wrapping
  `synthesize` (one chunk), so Kokoro / Piper / batch cloud backends are
  untouched and need zero changes.
- The only unavoidable playback change is **gapless append** within an utterance
  (today the worker drains the ring between `Play` commands —
  `crates/fono-audio/src/playback.rs:430`).

## Architecture (conceptual)

1. **Trait method (additive).** Add to `crates/fono-tts/src/traits.rs`:
   `synthesize_stream(text, voice, lang) -> Result<Stream<Result<TtsChunk>>>`
   with `TtsChunk { pcm, sample_rate, final: bool }`. **Default impl calls
   `synthesize` and yields one terminal chunk.** Add `fn supports_streaming(&self)
   -> bool` (default `false`) so callers know whether to take the streaming path.
2. **Gapless playback mode.** Extend `fono-audio` so chunks of the same utterance
   append to the cpal ring without the drain-between-`Play` gap (a
   `begin_stream` / `push` / `end_stream` API, single resampler kept across the
   utterance). Keep the existing batch `enqueue` as-is.
3. **Light streaming driver.** One consumer that: opens `synthesize_stream`,
   accumulates a fixed `[tts].stream_prebuffer_ms` (default ~200 ms) lead, then
   appends chunks via the gapless playback mode until `final`. No RTF probe, no
   revert-to-batch; on a stream error, surface it and stop (same error path as
   batch). Used by the assistant pump, `fono speak --stream`, and MCP speak.
4. **Transport seam for server mode (lightweight).** Define a minimal `PcmSink`
   (`push_chunk`, `is_idle`, `stop`) with a `LocalPlaybackSink` over
   `AudioPlayback`. The driver writes to a `PcmSink`, so a future server→remote
   audio transport plugs in without backend changes. (Server still plays locally
   today; this just keeps the seam clean.)
5. **Per-provider streaming overrides** (the bulk of the work), in priority
   order — each only overrides `synthesize_stream` + returns
   `supports_streaming() == true`:
   - **Gemini** — `:streamGenerateContent?alt=sse`, decode incremental
     `inlineData` PCM frames (`crates/fono-tts/src/gemini.rs`).
   - **Cartesia Sonic** — native streaming (WebSocket), largest latency win.
   - **Deepgram Aura** — streaming endpoint (`crates/fono-tts/src/deepgram.rs`).
   - **ElevenLabs** — streaming endpoint (`crates/fono-tts/src/elevenlabs.rs`).
   - **OpenAI TTS** — chunked streaming via the openai-compat path.
   Verify each vendor's exact wire format against live docs during implementation.

## Implementation Plan

- [x] **C1. Trait + chunk type.** Add `TtsChunk`, `synthesize_stream` (default
  wraps `synthesize`), `supports_streaming` to `crates/fono-tts/src/traits.rs`.
  No backend changes needed to compile. Offline test: default impl yields exactly
  one chunk equal to `synthesize`.
- [x] **C2. Gapless playback mode.** Add the no-drain-between-chunks streaming
  append path to `crates/fono-audio/src/playback.rs`; keep one resampler per
  utterance; preserve batch `enqueue`. Test: concatenated chunks reconstruct the
  buffer with no inserted silence at boundaries.
- [x] **C3. PcmSink + LocalPlaybackSink.** Minimal sink trait + local impl so the
  driver is transport-agnostic for later server use. Lives in `fono-audio`
  (`crates/fono-audio/src/sink.rs`) so both the daemon and MCP server reach it.
- [x] **C4. Streaming driver.** Implement the fixed-prebuffer driver and config
  `[tts].stream_prebuffer_ms`. Route the assistant pump
  (`crates/fono/src/assistant.rs`), `fono speak --stream`
  (`crates/fono/src/speak_stream.rs`), and MCP speak
  (`crates/fono-mcp-server/src/voice_io.rs`) through it. When the backend reports
  `supports_streaming() == false`, take the existing batch path unchanged.
- [x] **C5. Gemini streaming override.** First real backend; all-Google win.
  Offline test on the SSE chunk decoder; live-verify with a real key (pending).
- [ ] **C6. Cartesia streaming override.** Highest absolute latency payoff.
- [ ] **C7. Deepgram / ElevenLabs / OpenAI streaming overrides.** Add as needed,
  same pattern.
- [x] **C8. Docs + status.** `docs/providers.md`: which cloud backends stream,
  the prebuffer knob, and that local engines remain batch for now. Update
  `docs/status.md`. (C6/C7 — Cartesia, Deepgram/ElevenLabs/OpenAI — still pending.)

## Verification Criteria

- Batch-only and local backends are unchanged: default `synthesize_stream` yields
  one chunk identical to `synthesize`; no behaviour change when
  `supports_streaming()` is false.
- On a streaming cloud backend, measured assistant-turn time-to-first-audio drops
  below one-full-sentence synth latency.
- Gapless playback: no audible clicks/gaps at chunk boundaries (boundary
  concatenation test + manual listen).
- A simulated network stall during a stream surfaces the same error UX as batch;
  the small prebuffer absorbs normal jitter without underrun.
- Pre-commit gate green: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`.

## Potential Risks and Mitigations

1. **Per-provider wire-format drift / preview endpoints (esp. Gemini).**
   Mitigation: keep each override behind `supports_streaming`; offline decoder
   tests; live-verify before recommending as default.
2. **Click/gap at chunk boundaries from resampler discontinuity.**
   Mitigation: one persistent resampler per utterance in the gapless mode; C2
   boundary test.
3. **WebSocket backends (Cartesia/Deepgram) add a new transport dependency.**
   Mitigation: reuse existing HTTP/WS stack; gate behind feature flags already
   present per backend; update `deny.toml` if any new crate is required.
4. **Scope creep back toward local streaming.**
   Mitigation: local streaming explicitly deferred (below); this task ships
   cloud-only value independently.

## Alternative Approaches

1. **Gemini-only streaming first, others later.** Smallest first cut (C1–C5);
   defer C6–C7. Recommended if you want the all-Google latency win fastest.
2. **Smaller first chunk on batch (no streaming).** Synthesise a short opening
   clause first to cut ttfa without any streaming. Interim fallback if C-series
   is deferred.

## Deferred — local streaming (future task)

Local Kokoro/Piper intra-utterance streaming carries the slow-machine RTF risk
(stutter when RTF ≥ 1, amplified by server concurrency). It needs the RTF probe,
prebuffer growth, and adaptive revert-to-batch described in
`plans/2026-06-17-general-streaming-tts-v1.md` (sections S5–S7). Out of scope
here; the additive trait + driver built in this task make it a clean follow-on.
