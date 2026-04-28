# ADR 0017 — Cloud STT language stickiness (in-memory rerun-target cache)

Date: 2026-04-28
Status: Accepted
Supersedes: relevant portions of [ADR 0016](0016-language-allow-list.md)

## Context

ADR 0016 established the multi-language allow-list and the
`LanguageSelection { Auto, Forced, AllowList }` enum. With the
`AllowList` mode in production, two failure modes surfaced:

1. **Cloud Turbo misdetection.** Groq's `whisper-large-v3-turbo`
   (and to a lesser extent OpenAI's `whisper-1`) occasionally classify
   accented English as Russian, Bulgarian, or other Slavic languages
   for non-native English speakers. The transcript is then rejected by
   the allow-list filter and falls through to garbage.
2. **No symmetric solution for switchers.** Users who genuinely
   alternate between two or three languages (English at work, Romanian
   at home) cannot use a "forced primary" knob — every other-language
   utterance breaks. The allow-list lets the provider auto-detect
   freely; symptom 1 is the cost of that freedom.

We need a defence against symptom 1 that does not break symptom 2.

## Decision

Add an in-memory per-backend cache of the most recently
correctly-detected language code. The cache is consulted **only as a
rerun target** when the provider returns an out-of-allow-list
language; never as a first-call hint.

### Rules

1. First call: never force `language=`. Let the provider's auto-detect
   handle language switching for free.
2. On in-allow-list detection: record the code in the cache.
3. On banned (out-of-allow-list) detection:
   - If the cache holds a code for this backend → re-issue the same
     audio once with `language=<cached>`; return the rerun's response.
   - Otherwise → accept the unforced response as-is. No rerun.
4. Cache is keyed by backend `name()` (`&'static str`); one
   `Arc<LanguageCache>` is shared process-wide via
   `LanguageCache::global()` so batch and streaming variants of the
   same provider see the same cache.
5. OS locale is used to seed the cache at daemon start **if and only
   if** the locale's alpha-2 is in the configured allow-list.

## Rejected alternatives

### Local-Whisper "language bridge" before every cloud call

Run local Whisper's `lang_detect` on the prefix audio and force
`language=<detected>` on the cloud call. **Rejected.**

- Cloud users typically chose cloud precisely because they can't run
  local inference at acceptable latency. The bridge contradicts the
  whole reason they're on cloud.
- Adds a `whisper-rs` link dependency to the slim cloud-only build,
  defeating `cloud-all` as a lightweight option.
- The first-call detection is still correct in the common case
  (~95%); paying the bridge cost on every utterance is wasteful.

### File-persisted cache (`~/.cache/fono/state/lang_cache.json`)

**Rejected.**

- Cold-start hit-rate is marginal: the cache is helpful only when the
  user happens to open the same language they last spoke in.
- When the cached value is stale across sessions (different topic,
  different language), it actively misleads the rerun and produces
  worse output than today's behaviour.
- Adds corrupt-file recovery, race-on-write, serde plumbing, and
  `state_dir` propagation for negligible benefit.
- Daemon restarts are infrequent; in-memory rebuild within one or two
  utterances is cheap.

### Cache-as-first-call-force

Send `language=<cached>` on every request. **Rejected — actively
harmful for switchers.**

Trace with `languages = ["ro", "en"]`, cache `ro`, user switches to
English:

| Request | Cache | `language=` | Provider | Output |
|---|---|---|---|---|
| #1 (ro) | ro | ro | forced ro | ✓ correct |
| #2 (en) | ro | **ro** | forced ro on English audio | **garbled Romanian-as-English** |
| #3 (en) | ro | ro | same garbled decode | ✗ |

Once stickiness pins the wrong language for a switcher, every
subsequent call is broken until the cache is manually cleared. That's
worse than the bug we set out to fix.

The rerun-target design avoids this entirely: the first call is
always unforced, so the provider's auto-detect handles ro↔en switching
for zero cost. The cache only matters when auto-detect actually
misfires.

### Primary/secondary language model

Designate one entry of `general.languages` as primary and force it on
ambiguous calls. **Rejected.**

- The user-visible mental model "you have one main language plus some
  fallbacks" doesn't match how bilingual / multilingual users actually
  work. The right peer for any given utterance is whichever the user
  just spoke; config-file order is unhelpful as a tiebreaker.
- The implementation requires "primary" to leak into multiple call
  sites (first-call language, rerun fallback when cache empty, wizard
  copy, tray submenu). Each leak becomes a switcher-breaking bug.
- The peer-symmetric model (cache reflects what was last heard) needs
  no order anywhere, which is testable: two configs `["ro", "en"]`
  and `["en", "ro"]` must produce byte-identical transcripts on the
  same audio.

`LanguageSelection::primary()` is renamed to `fallback_hint()` and
its doc-comment scope-restricts use to single-language transports
(streaming WebSockets that physically can't accept a peer set on
connection setup). All other call sites consult the cache instead.

## Consequences

- The `cloud_rerun_on_language_mismatch` knob default flips from
  `false` to `true`. Cost-sensitive users can opt out.
- `cloud_force_primary_language` is deprecated; superseded.
- The wizard collects a checkbox set with no "primary" picker.
- A new tray "Languages" submenu offers a read-only peer display plus
  "Clear language memory" for the rare case where the cache has gone
  stale across topic changes.
- One-off Turbo misdetections self-heal after the first
  correctly-detected utterance per session (or immediately on
  cold-start when the OS locale ∈ allow-list).
