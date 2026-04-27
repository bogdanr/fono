# ADR 0016 — STT language allow-list (constrained auto-detect)

Date: 2026-04-28
Status: Accepted

## Context

Users frequently dictate in two or three languages (e.g. English at
work, Romanian at home). The pre-0.3 schema offered a single
`general.language` knob with two modes:

- `"auto"` — unconstrained Whisper auto-detect. Robust across
  languages but occasionally classifies a noisy English clip as Welsh,
  Dutch, Indonesian, or another low-prior tongue and emits garbage.
- `"<code>"` — forced single language. Fixes the misclassification
  but is unusable for genuine multilingual workflows.

The user feedback was concrete: *"Whisper might autodetect some of
the other languages. We need to be able to specify a list of
languages that should be considered and the others should essentially
be banned."*

## Decision

Replace the single `language: String` with a `languages: Vec<String>`
on both `[general]` and `[stt.local]`, and thread a
`LanguageSelection { Auto, Forced(code), AllowList(Vec) }` enum through
`SpeechToText` / `StreamingStt`.

| `languages` value      | Meaning                                                           |
| ---------------------- | ----------------------------------------------------------------- |
| `[]` (or `["auto"]`)   | Unconstrained Whisper auto-detect (today's default behaviour).    |
| `["en"]`               | Forced single language (today's `language = "en"`).               |
| `["en", "ro", "fr"]`   | Constrained auto-detect: Whisper picks one of these; others banned. |

### Local Whisper enforcement (`whisper-rs` 0.16)

`WhisperState::lang_detect(offset_ms=0, n_threads)` returns
`(detected_id, probs)` after a single `pcm_to_mel` call on the prefix
audio. We mask `probs` to allow-list members only, take the argmax,
then call `full()` with that code locked via `params.set_language`.

This is **detect-then-constrain**, not token suppression: the decoder
still runs once with a single picked language. Token-suppression
alternatives (banning the language tokens themselves) are fragile,
undocumented in `whisper-rs`, and would force every backend to keep
a per-model token map in sync.

### Cloud STT (Groq, OpenAI, …)

Provider APIs accept at most a single `language` field. **Hard banning
is impossible**; we expose two opt-in knobs on `[general]`:

- `cloud_force_primary_language` (default `false`) — send
  `languages[0]` instead of letting the provider auto-detect. Useful
  for users whose primary language dominates ≥ 90 % of dictations.
- `cloud_rerun_on_language_mismatch` (default `false`) — when the
  provider returns a `language` field outside the allow-list, retry
  the request once with `languages[0]` forced. Trades latency and
  cost for correctness.

Both default to `false` so the cost/latency profile of existing cloud
users is preserved.

### Streaming

`WhisperState::lang_detect` is cheap (one encoder pass over the
prefix mel) but not free, so the streaming backend caches the
detected language per segment and resets on `SegmentBoundary`. The
preview lane never re-detects mid-segment.

### Migration

The legacy `language: String` field is kept on the schema with
`#[serde(default, skip_serializing_if = "String::is_empty")]` for one
release cycle. Migration in `Config::migrate`:

- if `languages` is empty and `language` is non-empty → lift it
  (`language = "ro"` becomes `languages = ["ro"]`);
- always clear `language` on save so it disappears from disk.

## Consequences

- One small extra encoder pass when the allow-list has ≥ 2 entries
  (~5–15 ms on a tiny model, negligible relative to decode time).
- New module `crates/fono-stt/src/lang.rs` becomes the single source
  of truth for normalisation, parsing, and the `Auto`/`Forced`/
  `AllowList` taxonomy. Backends do not compare sentinel strings.
- Cloud STT enforcement is best-effort by construction; users who
  want a hard ban must either pick a single forced code or move to
  the local backend. This is documented in `docs/providers.md`.
- The wizard now persists its language prompt instead of discarding
  it (a long-standing bug uncovered by this work).
