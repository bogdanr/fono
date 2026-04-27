# ADR 0015 — Boundary heuristics (Slice A v7)

## Status

Accepted (Slice A v7 — `2026-04-27`). Builds on ADR 0009 (interactive
live dictation). Will be revised when Slice D introduces the adaptive
end-of-utterance detector and resume-grace window (currently reserved
config keys).

## Context

ADR 0009 established that Slice A ships a streaming dictation pipeline
with a two-lane preview/finalize architecture, VAD-driven segment
boundaries, and cleanup-on-finalize. Real-fixture telemetry from the
v6 equivalence harness exposed two failure modes that the bare VAD
boundary cannot fix on its own:

1. **Premature segment finalize when the speaker is mid-thought.** The
   silence-gap that triggers `SegmentBoundary` fires after the
   configured `silence_frames_for_boundary`, but human speech includes
   intentional ~400 ms pauses for prosodic phrasing (commas, mid-
   clause breaths). Whisper-tiny finalises and the LLM cleanup pass
   then runs against an incomplete clause.
2. **End-of-utterance hallucination on dangling words.** When the user
   releases the hotkey mid-thought (`"and then I…"`), the assembled
   transcript ends in a syntactically-dangling word. Cleanup-on-
   finalize sees that as a complete sentence and either deletes the
   trailing word or invents a continuation.

Plan v7 R2.5 / R7.3a introduce two small, additive heuristics that
mitigate these failure modes without changing the streaming-vs-batch
equivalence guarantee.

## Decisions

### 22. Heuristics live in the session layer, not in `fono-stt`

The boundary-extension logic — punctuation hint (`commit_use_punctuation_hint`),
prosody hint (`commit_use_prosody`), and end-of-utterance hold-on-filler
(`commit_hold_on_filler`) — is implemented in `crates/fono/src/live.rs`,
not inside any `StreamingStt` impl.

Rationale: the heuristics are **per-session policy**, configured by
`[interactive]` keys the user can flip without rebuilding. Embedding
them in `WhisperLocal::stream_transcribe` would force every cloud
streaming backend (Slice B) to re-implement them or carry knob plumbing
through the trait. Keeping them in `live.rs` means each backend stays a
pure transcribe-PCM-emit-updates engine; the orchestrator decides when a
SegmentBoundary forwarded from audio gets delayed and when the
finalised text gets a "trailing-dangling-word" annotation.

### 23. Heuristics are additive-only

A heuristic may **delay** a segment boundary or **flag** a transcript
suffix; it must never **change** the committed text. The integration
test `heuristics_are_additive_when_no_trigger_present` in
`crates/fono/tests/live_pipeline.rs:251` enforces this contract: the
same input run with `HeuristicConfig::all_off()` and
`HeuristicConfig::default()` must produce identical `LiveTranscript.committed`.

The prosody extension is capped at `chunk_ms_steady * 1.5` (see
`cap_extension_ms` in `crates/fono/src/live.rs:652`) so a misbehaving
F0 estimator can never freeze a session indefinitely.

### Defaults and rationale

| Knob                            | Default | Rationale                                                                                              |
| ------------------------------- | ------- | ------------------------------------------------------------------------------------------------------ |
| `commit_use_prosody`            | `false` | Off until Slice B real-fixture telemetry validates the F0 slope thresholds across speakers.            |
| `commit_prosody_extend_ms`      | `250`   | Long enough to bridge a typical mid-clause breath (~200 ms) without delaying obvious sentence ends.   |
| `commit_use_punctuation_hint`   | `true`  | Cheap (string-end check), high-precision: terminal punctuation in preview text is a strong signal.    |
| `commit_punct_extend_ms`        | `150`   | Smaller than prosody — punctuation is the primary signal; the extension only buys confirmation room.  |
| `commit_hold_on_filler`         | `true`  | The dangling-word/filler vocabularies are bounded and high-precision in English.                      |
| `commit_filler_words`           | English | Localisation caveat: users dictating in other languages should override (see `docs/interactive.md`).  |
| `commit_dangling_words`         | English | Same caveat.                                                                                           |
| `eou_drain_extended_ms`         | `1500`  | Longest comfortable "did I finish?" pause without making cancel-to-restart feel sluggish.             |

### 24. Harness pins the knob set

Equivalence-harness JSON reports embed the active `BoundaryKnobs` in
`pinned_params` (R18.23). Streaming runs are reproducible only when the
heuristic config is identical across machines, so the harness does not
read `[interactive]` from disk — it constructs its own `BoundaryKnobs`
from `BoundaryKnobs::defaults()` (or the per-row variants) and persists
the chosen value into the report. CI consumers diff against the pinned
value to detect "default drifted, gate silently shifted" regressions.

Tier-1 + Tier-2 *gate* on `A2-default`, the row that uses the on-disk
defaults. Three additional rows ship as informational diff outputs:

- `A2-no-heur` — every heuristic off; proves the gate also passes
  without any boundary tuning, i.e. the heuristics are additive.
- `A2-prosody` — prosody isolated; surfaces the F0 estimator's
  contribution to per-fixture boundary timing.
- `A2-filler` — hold-on-filler isolated; surfaces dangling-word
  detection sensitivity.

## Forward references

The `eou_adaptive` and `resume_grace_ms` keys are reserved in
`fono_core::config::Interactive` but inert in Slice A. Slice D (R15)
replaces the static `eou_drain_extended_ms` window with a silence-
distribution estimator and adds a hotkey-resume grace window. ADR 0015
will be revised at that time.

## Consequences

### Positive

- Cloud streaming backends in Slice B do not need to re-implement the
  heuristics; they still benefit by virtue of running through the
  same `LiveSession::run`.
- Each heuristic can be individually disabled at runtime via the
  config TOML — useful when a user reports a regression and we need
  to isolate which knob caused it.
- The harness pins make every JSON report self-describing; a future
  CI bisect on a streaming regression can replay the exact knob set.

### Negative

- The punctuation hint is currently a stub in the wired path (it is
  unit-tested as a pure function but the translator task does not
  yet receive preview text). Slice B will plumb the feedback channel.
- The English-only default vocabularies require users dictating in
  other languages to provide their own `commit_filler_words` /
  `commit_dangling_words`. Documented in `docs/interactive.md`.
