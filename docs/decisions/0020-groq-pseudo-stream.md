# ADR 0020 — Groq streaming via pseudo-stream re-POST

## Status

Accepted 2026-04-28.

## Context

R4.2 of `plans/2026-04-27-fono-interactive-v1.md` calls for a Groq
streaming STT backend so the live-dictation overlay can paint preview
text as the user speaks, mirroring the local Whisper streaming path
that shipped in Slice A.

Groq today (April 2026) does **not** expose a native streaming /
WebSocket transcription endpoint. The only API surface is the
existing batch multipart POST documented at `crates/fono-stt/src/groq.rs`
(`/openai/v1/audio/transcriptions`). The endpoint accepts ≤ 30 s of
audio per request and returns a single JSON `{ "text": …,
"language": … }`.

The `StreamingStt` trait at `crates/fono-stt/src/streaming.rs`
expects an implementation to consume `StreamFrame::Pcm` chunks from a
broadcast channel and emit `TranscriptUpdate::preview` /
`TranscriptUpdate::finalize` items. Mapping that contract onto a
batch-only HTTPS endpoint requires choosing how often, and on what
audio window, to issue requests.

## Decision

Implement Groq streaming as a "pseudo-stream":

1. Buffer all incoming `StreamFrame::Pcm` chunks into a per-segment
   PCM accumulator.
2. Every `PSEUDO_STREAM_INTERVAL` (= 700 ms wall-clock), if the
   buffer has grown since the last decode AND no prior request is
   in flight, fire a single batch POST against the trailing
   `TRAILING_WINDOW_SAMPLES` (= 28 s at 16 kHz) of audio.
3. Pipe each preview decode through the existing `LocalAgreement`
   helper to extract a stable token-prefix as the preview text;
   any unstable suffix is shown tentatively.
4. On `StreamFrame::SegmentBoundary` or `StreamFrame::Eof`, fire one
   final batch POST against the **full** segment audio and emit
   `TranscriptUpdate::finalize`. Reset the per-segment
   `LocalAgreement` and PCM buffer.
5. **In-flight cap = 1.** A Groq request that hasn't returned by the
   next 700 ms tick causes the would-be preview to be dropped
   (counted in `preview_skipped_count` for diagnostics) rather than
   queued.
6. Opt-in via `[interactive].enabled = true`. Default `false`. (Until
   v0.3.4 a separate `[stt.cloud].streaming` knob also gated this; it
   was collapsed into the master live-dictation switch in v0.3.5 — see
   `plans/2026-04-29-streaming-config-collapse-v1.md`.)

### Why 700 ms

Smaller cadence ⇒ lower preview latency, higher cost (more requests
per second of speech).
Larger cadence ⇒ lower cost, higher latency.

700 ms is the value picked in the v1 plan. Empirically the Groq
batch endpoint round-trips in ~150 ms on `whisper-large-v3-turbo`,
so a 700 ms cadence produces previews ~850 ms after each new word —
fast enough that the overlay feels live without saturating the
endpoint or the user's API quota. Captured as a tunable constant
(`PSEUDO_STREAM_INTERVAL`) so a future commit can wire it through
the config.

### Why in-flight cap = 1

Real audio is bursty. If a request is in flight when the next tick
fires, we can either (a) queue and serialize, (b) drop and continue,
or (c) issue concurrently. Option (a) introduces unbounded latency
under sustained burstiness. Option (c) blows the API quota for no
gain (Groq's batch responses are deterministic per input, so two
in-flight requests on overlapping windows produce redundant work).
Option (b) — drop and continue — preserves the cadence, keeps
quota usage proportional to wall-clock time, and matches what the
user actually wants from the overlay: the *most recent* preview,
not an exhaustive history.

### Why pseudo-stream over WebSocket

Groq has no WebSocket endpoint today. When they ship one, the right
move is a fresh `GroqStreamingWs` impl that obsoletes the
pseudo-stream path. The pseudo-stream backend stays available as a
fallback for `whisper-large-v3-turbo` (or other batch-only models).

## Consequences

- **Cost overhead.** A 30 s utterance with the pseudo-stream path
  issues ~43 batch POSTs (1 per 700 ms + 1 finalize) vs 1 POST on
  the batch-only path. On a usage-billed Groq plan that's roughly
  43× the dollar cost per utterance. The wizard prompt at
  `crates/fono/src/wizard.rs` calls this out (~25% extra cost is
  the conservative estimate; pathological cases are higher) and
  defaults to off.
- **Latency floor.** First preview lands ~850 ms after the first
  audible word (700 ms cadence + ~150 ms round-trip). For a
  noticeably-sub-second perceived latency users want either
  (a) the local Whisper streaming path, or (b) a future native
  Groq WebSocket impl.
- **Determinism for tests.** Because the backend just calls a
  closure that returns `Result<GroqResponse>`, the test suite
  injects a scripted closure (see `groq_streaming.rs::tests`) and
  the equivalence harness's cloud-mock mode (Slice B1 / Thread C,
  R18.12) injects a recorded-HTTP closure that pattern-matches
  request bodies to a JSON fixture file.

## References

- `crates/fono-stt/src/groq_streaming.rs` — the backend.
- `crates/fono-stt/src/streaming.rs` — the trait + `LocalAgreement`.
- `plans/2026-04-27-fono-interactive-v1.md` R4.2.
- `plans/2026-04-28-wave-3-slice-b1-v1.md` Thread B.
