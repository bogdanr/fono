# ADR 0009 — Interactive / live dictation (Slice A)

## Status

Accepted (Slice A — `2026-04-27`).
Supersedes the implicit "batch only" assumption baked into ADRs
0001–0008. Will be revised when Slice B lands cloud streaming, the
sub-process overlay refactor, and the realtime cpal-callback push.

## Context

Fono v0.1.x is a *batch* dictation tool: the user holds the hotkey,
fono records the whole utterance, then runs whisper end-to-end and
injects the cleaned text. That model has two felt-bad properties:

1. **Long perceived latency on long utterances.** A 30-second voice
   note takes 1–4 s to come back even on local-fast hardware, because
   the whole pipeline is serial. The user stares at nothing.
2. **No incremental feedback.** When whisper mishears the first word,
   the user finds out only after the whole transcription finishes —
   too late to re-dictate with less friction than starting over.

The reference projects we replace (Tambourine, OpenWhispr) both ship
*streaming* dictation modes that paint preview text into an overlay as
the user speaks. Plan v6 (`plans/2026-04-27-fono-interactive-v6.md`)
elevates this to a Slice A deliverable behind a runtime-toggleable
`[interactive].enabled` knob, with the streaming code itself gated
behind the cargo `interactive` feature so slim builds stay slim.

The design space has several axes that needed deciding *before* code
landed in the workspace.

## Decisions

### 1. Hybrid model: overlay-first preview, opt-in live-inject

Streaming text is rendered into the live overlay window by default.
Writing the in-progress text directly into the focused application
("live inject") is gated behind `[interactive].live_inject = true`
(Slice B).

Rationale: live-inject sometimes corrupts unrelated state (think of
Vim normal mode, a terminal in vi-edit, a search-as-you-type field
that triggers on every keystroke). Overlay-only is universally safe
and matches OpenWhispr's default. Users who want Tambourine-style
in-place dictation can opt in once Slice B's per-app context awareness
ships.

### 2. `LocalAgreement-2` for token-level stability

The streaming preview lane runs whisper on the *growing* segment audio
every ~700 ms. Naive emission flickers because whisper's output is not
prefix-stable across passes. The `LocalAgreement` helper in
`crates/fono-stt/src/streaming.rs:153-209` keeps a rolling 2-pass
intersection: tokens that appear identically in two consecutive
decodes are committed to the "stable" prefix and never revoked, while
the divergent tail is rendered as in-flux.

We picked the pairwise (`-2`) variant rather than triple-confirmation
(`-3`) because Tier-1 equivalence-harness telemetry shows the win/loss
crossover sits at the pairwise mark for whisper-tiny / whisper-base —
extra confirmations buy negligible accuracy for a TTFF cost. We will
revisit on a real-fixture batch in Slice B.

### 3. Two-lane preview/finalize architecture

Every streaming STT impl emits two kinds of `TranscriptUpdate`:

- `Preview` — speculative, may revise the same segment many times.
- `Finalize` — authoritative, fired once per VAD-bounded segment, and
  treated as the equivalence-comparable text by R18.

The lane split lets the orchestrator render preview into the overlay
without committing to history, and lets the equivalence harness
compare *only* finalize-lane output against the batch lane. The
alternative (a single emission lane with a "is_final" flag) was
rejected because callers consistently treat the two lanes as
behaviourally distinct (display vs commit), and an explicit enum
catches mis-routing at compile time.

### 4. Cleanup-on-finalize (no streaming LLM during dictation)

The LLM cleanup stage (`fono-llm::TextFormatter::format`) is *not*
streamed during dictation. It runs once on the full assembled
`committed` text after the user releases the hotkey.

Rationale: a streaming LLM that emits and then revises tokens — and
whose revisions are then re-injected into a focused app — produces
visually unstable text and high perceived risk for the user. The
budget-controller wins from streaming whisper alone (R12) cover the
observable latency target without engaging the LLM in the streaming
loop. Streaming cleanup is on the Slice E "polish" backlog, not Slice
A.

### 5. Overlay stays in-process for Slice A

The overlay is a `winit` + `softbuffer` window driven from a
background thread inside the main fono process (see
`crates/fono-overlay/src/real.rs`). The v6 plan R5.6 wants it eventually
extracted to a sub-process so that an overlay-side panic / graphics-
driver wedge cannot take the daemon down.

That refactor is **deferred to Slice B**. The Slice-A in-process
overlay is good enough for the perceived-latency win, and the
sub-process IPC contract is a meaningful design exercise that
shouldn't be rushed under Slice A's timeline. The deferral is
documented in `crates/fono-overlay/src/lib.rs:9-12`.

### 6. Realtime cpal-callback push deferred (record-then-replay
       covers Slice A)

`fono record --live` (and the equivalence harness's streaming pass)
captures PCM into an in-memory buffer first and *then* replays it
through the streaming pipeline in 30 ms chunks. The streaming code
path is fully exercised, but the user does not yet see preview text
*while* speaking — the preview lane runs after the hotkey is
released.

Rationale: pushing PCM directly from cpal's audio-thread callback
into the broadcast channel needs careful priority + back-pressure
handling that we don't want to design under Slice A's clock. The
streaming↔batch equivalence guarantee (R18) is unaffected by the
push timing; record-then-replay produces the same `committed` text
the realtime path will. Slice B turns on the realtime push and the
overlay comes alive while the user speaks.

### 7. Equivalence harness as the acceptance gate

Plan v6 R18 promotes the streaming↔batch equivalence harness from a
"test consideration" to a first-class Slice-A deliverable and a hard
gate on every later slice that touches streaming code. Slice A ships
the harness skeleton with two fixtures (synthetic-tone placeholders
flagged in the manifest) and a Tier-1 (whisper-only) comparison; the
remaining 10 fixtures and Tier-2 (with-LLM) land in Slice B.

The Slice-A Tier-1 PASS threshold is loosened to
`stt_levenshtein_norm ≤ 0.05` to account for synthetic placeholders
that exercise harness *shape* rather than transcription accuracy. It
tightens to the v6 R18.1 strict bar (`≤ 0.01`) once real-speech
fixtures replace the placeholders. The threshold is a single-line
constant in `crates/fono-bench/src/equivalence.rs`
(`TIER1_LEVENSHTEIN_THRESHOLD`); refresh in the same commit that
swaps the fixtures.

## Consequences

### Positive

- Clean compilation matrix: the slim build (no `interactive` feature)
  has zero streaming code linked in and zero new attack surface.
- Single-binary A/B testing: the same binary respects
  `[interactive].enabled = false` at runtime, so users can flip the
  toggle without a rebuild.
- The two-lane STT trait (`StreamingStt`) lets cloud streaming
  providers in Slice B implement the same contract whisper does
  without disturbing the orchestrator.
- The R18 equivalence harness catches future "streaming silently
  diverges from batch" regressions on PRs that touch streaming code,
  which is exactly the failure mode that kills streaming pipelines
  long-term.

### Negative

- The realtime visual win lands in Slice B, not Slice A; users on
  Slice-A builds still see the overlay paint the *final* text, not
  the typed-as-they-talk text. We have decided this is acceptable in
  exchange for landing the streaming-decoder + equivalence-harness
  primitives early.
- The overlay sub-process refactor is now Slice B work; an overlay
  crash in Slice A is a daemon crash. Mitigation: the
  `RealOverlay::spawn()` path tolerates winit failure and falls back
  to no overlay (see `crates/fono/src/cli.rs:1408-1412`).
- The synthetic-tone placeholder fixtures cap the harness's Tier-1
  threshold above the v6 strict bar until real speech lands.

## Alternatives considered

- **Live-inject default.** Rejected — too easy to corrupt
  unrelated app state; users who really want it can flip
  `[interactive].live_inject` once Slice B lands the per-app context
  guard.
- **Triple-confirmation (`LocalAgreement-3`).** Rejected — measurable
  TTFF cost without measurable accuracy win on the whisper-tiny /
  base models we ship by default.
- **Single-lane emissions with `is_final: bool`.** Rejected —
  callers consistently treat preview and finalize as behaviourally
  distinct; a typed enum catches mis-routing at compile time.
- **Stream the LLM cleanup during dictation.** Rejected — visually
  unstable for the user; a streaming-cleanup cost-benefit re-eval
  belongs on the Slice E polish list, not in the same release that
  introduces streaming whisper.
- **Land the equivalence harness in Slice B.** Rejected — silent
  streaming-vs-batch regressions are exactly the class of bug that
  motivates the harness in the first place; if Slice A lacks the
  harness, every later slice silently re-asks the same correctness
  questions.
