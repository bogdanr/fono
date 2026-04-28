# Streaming rate-limit controls + overlay-less streaming verification — v1

**Date:** 2026-04-28
**Status:** Draft
**Owner:** next implementation session

## Problem

User report (v0.3.2, Groq cloud streaming):

1. **Free-tier Groq's 20 requests/minute ceiling is being hit constantly.**
   Today's hardcoded 700 ms preview cadence + finalize + rerun storms produce
   ~80-100 req/min during continuous speech.
2. **Streaming "doesn't work" when `[interactive].overlay = false`.**
   Code review at `crates/fono/src/session.rs:856-1080` and
   `crates/fono/src/live.rs:200-340` shows `OverlayHandle` is `Option`
   throughout and injection at `session.rs:1066` is overlay-independent —
   so this is either a subtle bug not surfaced in unit tests, or a
   wizard-induced misconfiguration. Verification first, fix-or-not second.

## Constraints

- Do not break the local-Whisper streaming path (which has no rate limit
  and benefits from the 700 ms cadence).
- Do not change wire format of `[interactive]` config block in a
  backward-incompatible way; add new keys only.
- Per-provider defaults must be derivable from existing
  `[stt].backend` + `[stt].provider` selection; no new "free tier"
  detection magic.

## Design

### Task 1 — Preview cadence becomes a config knob

`PSEUDO_STREAM_INTERVAL` at `crates/fono-stt/src/groq_streaming.rs:51` is
the hardcoded `const Duration = 700ms`. Replace with a constructor
parameter on `GroqStreaming`:

```rust
pub fn with_preview_cadence_ms(mut self, ms: u32) -> Self { ... }
```

The streaming pump reads `self.preview_cadence` instead of the const.
Default stays 700 ms (no change for callers that don't opt in). The
factory at `crates/fono-stt/src/factory.rs` reads
`config.interactive.cloud_preview_cadence_ms` and applies it when
constructing `GroqStreaming` for cloud backends.

### Task 2 — New config key with per-provider defaults

In `Interactive` at `crates/fono-core/src/config.rs:554`:

```rust
/// Preview cadence (ms) when the active STT is a *cloud* backend.
/// Default 4000 ms, sized for Groq's free-tier 20 req/min ceiling
/// (60s / 4s ≈ 15 previews + 4-5 finalizes + 1-2 reruns ≈ 20 req/min).
/// Set to 700 to match local-Whisper cadence when paying for a higher
/// tier. The local backend ignores this knob and always uses its own
/// 700 ms cadence (no rate limit).
pub cloud_preview_cadence_ms: u32,
```

`Default` impl: `4000`. The wizard at
`crates/fono/src/wizard.rs:404-438` (cloud-provider branch) prompts:

> *"How many requests per minute does your Groq plan allow?
> Free tier is 20. Press Enter to keep the safe default."*

User enters 20 (or higher) → wizard computes
`cloud_preview_cadence_ms = ceil(60_000 / (req_per_min * 0.75))` so 75 %
of the budget goes to previews and 25 % to finalize + rerun. Stores the
result, not the req/min value (the req/min is purely a wizard input).

### Task 3 — Hard request-rate cap (defence in depth)

`Interactive::max_requests_per_minute: Option<u32>` (default `None`).
When set, the streaming pump tracks a rolling 60-second request count;
if a preview tick would exceed the cap, the preview is **skipped**
(same code path as the existing in-flight drop at
`groq_streaming.rs:283-296`, just a different cause). Counter is
incremented for every preview, finalize, and rerun request.

This catches the "user lowered cadence but a rerun storm still pushed
us over" edge case, and covers other backends added later that might
have different latencies.

### Task 4 — Rate-limit-aware rerun policy

When the rolling counter is within 3 requests of the cap and a banned
detection arrives, suppress the per-peer rerun and accept the unforced
response (same fallback behaviour as
`cloud_rerun_on_language_mismatch = false`). Logs an INFO line:

```
groq: rate-limit budget low ({remaining}/{cap}); deferring per-peer
rerun and accepting unforced response (banned: {detected})
```

### Task 5 — Wizard guidance

Wizard's cloud-provider branch shows the projected req/min based on
the chosen cadence + the user's plan:

> *"At 4000 ms cadence and a typical 5-second utterance, Fono will
> issue roughly 18-22 requests per minute. Within your 20 req/min
> plan: yes/no/borderline."*

Helps the user understand the trade-off without reading docs.

### Task 6 — Investigate "streaming without overlay" report

Reproduce on the user's config:

1. Read user's `[interactive]` block; confirm `enabled = true,
   overlay = false`.
2. Run with `RUST_LOG=fono=debug,fono_stt=debug` and grep for the
   "live-dictation: committed" line at `session.rs:1001`. If absent,
   the run task never returned → a hang somewhere in `LiveSession::run`.
3. If reproduced, instrument the `while let Some(upd) = updates.next()`
   loop at `live.rs:324-336` to confirm updates are being received
   when overlay is None. Most likely culprit is an unintended drop of
   `tx` somewhere only kept alive by the overlay path.
4. If it turns out to be a wizard-induced misconfiguration (the user
   disabled `overlay` and that flipped `enabled` too), the fix is
   wizard-side: separate the two questions clearly. *"Show the live
   text on screen as you speak?"* (overlay) and *"Enable live mode
   instead of recording-then-cleaning?"* (enabled) are independent
   decisions and the wizard should phrase them as such.

### Task 7 — Tests

- Unit: `groq_streaming` constructor honours `with_preview_cadence_ms`.
- Unit: rolling-counter rate cap correctly skips previews.
- Unit: rerun suppression fires when budget low.
- Integration: `fono-bench equivalence --stt groq` with cadence override
  produces same fixture verdicts as the default cadence (proving
  cadence change doesn't compromise quality).

### Task 8 — Docs

- `docs/providers.md`: new "Streaming and rate limits" subsection
  explaining Groq's free-tier ceiling and the
  `cloud_preview_cadence_ms` knob with worked-out examples.
- `docs/troubleshooting.md`: new entry "Hitting `429 too many
  requests`" pointing at the cadence config.
- ADR 0022: cloud streaming cadence policy + per-provider defaults.

## Verification criteria

- Free-tier Groq user with default config sustains 60+ seconds of
  continuous speech without any 429.
- Local-Whisper streaming user sees no behavioural change (700 ms
  cadence preserved).
- Bench equivalence verdict unchanged when running cadence override
  across the existing fixture set.
- The "streaming without overlay" investigation produces either a
  reproducible bug report (with fix) or a documented "user error +
  wizard improvement" finding.

## Risks and mitigations

1. **Cadence too slow degrades UX.** 4 s preview means the user sees
   the first preview only after 4 s of speech, vs 700 ms today. *Mitigation:*
   wizard explains the trade-off; the cadence knob lets paying users
   reset to 700 ms. The first preview lands at *least* by `chunk_ms_initial`
   (600 ms default) regardless — only the *steady-state* cadence shifts.
2. **Rate counter false-positives.** A burst of finalize requests at
   end-of-utterance could hit the cap and starve the next session.
   *Mitigation:* counter is rolling-60s, not per-second; bursts smooth out.
3. **User configures a number that doesn't match their actual plan.**
   They might enter 20 but actually have 30. *Mitigation:* the cap is
   advisory, not a hard upstream contract; we err on the safe side
   (skipping previews is preferable to 429s).

## Alternatives considered

1. **Detect 429 reactively and back off.** Tempting but reactive: the
   user has already lost a request to the floor. Proactive cadence
   control is cheaper.
2. **Use a token bucket with bursts.** More elegant but more code; the
   simple rolling counter handles the dominant case (continuous speech
   for a few minutes) cleanly.
3. **Switch streaming off automatically when cloud is selected.**
   Unfriendly; defeats the value proposition of streaming. Better to
   make streaming work within the user's budget.
