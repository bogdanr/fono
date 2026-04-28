# Multi-Language STT Without Primary/Secondary Friction

## Objective

Eliminate cloud-STT language misclassification (Groq Turbo classifying
non-native English as Russian, etc.) for users on resource-constrained
machines who chose cloud STT precisely to **avoid** local inference.
The fix must be cloud-native (no local Whisper passes), zero-typing
out of the box, and treat the user's languages as peers rather than
imposing a primary/secondary ordering.

This v2 supersedes v1 (`2026-04-28-multi-language-stt-no-primary-v1.md`).
v1 anchored on a local-Whisper "language bridge" which is actively
hostile to the user's resource budget.

## Background — what we already have

- `crates/fono-stt/src/lang.rs:25-37` — `LanguageSelection` enum.
- `crates/fono-core/src/config.rs:101` — `general.languages: Vec<String>`,
  default empty.
- `crates/fono-stt/src/groq.rs:139-203` — cloud post-validation +
  optional rerun. Both knobs (`cloud_force_primary_language`,
  `cloud_rerun_on_language_mismatch`) default `false`.
- Tray submenu pattern at `crates/fono-tray/src/lib.rs:399-403`.
- ADR 0016 documents the current allow-list. This plan adds
  language-stickiness and reframes the wizard, but keeps ADR 0016's
  schema.

## Strategic shape

Three cheap, additive cloud-native layers, in order of automation:

1. **Language stickiness (zero-cost, automatic).** Cloud STT
   responses already carry the detected `language` field. Cache the
   last successfully-detected code (per backend, in-memory + on
   disk under `state_dir`). Send it as `language=` on the next
   request when the configured set has more than one peer. Result:
   one mistake self-corrects on the next utterance. No primary
   anywhere — the cache reflects what the user **actually** spoke
   most recently, not config order.

2. **Smart-default wizard suggestion (one-key confirmation).**
   First-run wizard reads OS locale (`LANG`, `localectl`,
   `setxkbmap`, …) and, when it differs from `en`, suggests
   `languages = [<os_locale>, "en"]`. User confirms with one key.
   English is a **suggestion**, not a hard-coded peer — they can
   un-check it, edit the file to drop it, or skip the wizard
   entirely.

3. **Tray submenu manual override (escape hatch).** Persistent
   checkboxes for the peer set + a one-shot "Force next dictation
   as: …" radio that decays after one capture. Used when the
   stickiness cache happens to be wrong for this particular
   utterance.

The local-Whisper bridge from v1 is **dropped** as a mechanism. Users
on the local profile already have symmetric allow-list enforcement
via `whisper_local::lang_detect`; they don't need the bridge either.

## Implementation Plan

- [ ] Task 1. Rename `LanguageSelection::primary()` to
  `fallback_hint()` and document it as a transport-level concept.
  Keep `primary()` as a deprecated alias for one release.

- [ ] Task 2. New `crates/fono-stt/src/lang_cache.rs`:
  `pub struct LanguageCache` with a per-backend last-seen code,
  persisted as `~/.cache/fono/state/lang_cache.json`. `get(backend)
  -> Option<String>` and `record(backend, code)`. Loaded once at
  daemon start, written best-effort on `record`. Failures never
  bubble up.

- [ ] Task 3. Wire the cache into every cloud STT backend
  (`groq.rs`, `openai.rs`, `deepgram.rs`, `assemblyai.rs`,
  `cartesia.rs`, `groq_streaming.rs`). On request:
  - `Forced(c)` → send `c` (unchanged).
  - `Auto` → send nothing (unchanged).
  - `AllowList` → send `cache.get(backend).filter(|c|
    selection.contains(c)).unwrap_or_else(|| selection.fallback_hint())`.
  On a successful response with `language` inside the allow-list,
  call `cache.record`. On a banned `language`, skip recording and
  let the post-validation rerun fire.

- [ ] Task 4. Flip `cloud_rerun_on_language_mismatch` default to
  `true` for all builds. The cost is one extra round-trip on a
  rare mismatch (after stickiness has had a chance to converge).
  Document the `false` override for cost-sensitive users in
  `docs/providers.md`.

- [ ] Task 5. Mark `cloud_force_primary_language` as deprecated in
  the field doc-comment. With stickiness running, it's
  semantically dead — the cache replaces it. Keep on schema for
  one release; remove in v0.5.

- [ ] Task 6. New `crates/fono-core/src/locale.rs`:
  `pub fn detect_os_languages() -> Vec<String>` reading
  `LC_ALL` / `LC_MESSAGES` / `LANG`, `localectl status` (Linux),
  `defaults read .GlobalPreferences AppleLanguages` (macOS),
  `Get-WinUserLanguageList` (Windows). Returns deduplicated
  alpha-2 codes. Pure best-effort; empty on any error.

- [ ] Task 7. Wizard rework in `crates/fono/src/wizard.rs`:
  replace the free-text language prompt with a "Languages you
  dictate in" step. Behaviour:
  - Detect OS locale via Task 6.
  - If detected locale ∈ {empty, `en`}: pre-fill `["en"]`.
    Otherwise pre-fill `[<os_locale>, "en"]`.
  - Show as a checkbox list; user toggles peers and confirms.
  - English is **default-on but unchekable** — no special-case
    code. A user in pure-Romanian environment can write
    `languages = ["ro"]` directly to `config.toml` or uncheck `en`
    in the wizard.

- [ ] Task 8. New tray "Languages" submenu in
  `crates/fono-tray/src/lib.rs`, mirroring the STT/LLM submenu
  pattern:
  - Static checkbox list of `general.languages`. Toggle persists
    to disk + triggers `Reload`.
  - Radio "Force next dictation as: [Auto / <each peer>]"
    one-shot override; emits a tray notification when applied
    and decayed.

- [ ] Task 9. Notification on stickiness rerun. When the
  post-validation rerun fires (banned language detected), emit a
  **rate-limited** desktop notification ("Fono retried with
  language=ro — set Force in tray if this is wrong"). Once per
  60 s ceiling.

- [ ] Task 10. Tests:
  - `lang_cache.rs` — round-trip persistence; corrupt-file
    recovery; cache-miss returns `None`.
  - `groq.rs` (extend) using `with_request_fn` closure to assert
    the request carries the cached code on call N+1.
  - `locale.rs` — env-var permutations under `serial_test`.
  - Wizard integration test: `LANG=ro_RO.UTF-8` produces
    `languages = ["ro", "en"]`; `LANG=en_US.UTF-8` produces
    `["en"]`; user can override before persistence.
  - Tray submenu unit test for the one-shot override decay.

- [ ] Task 11. Docs:
  - `docs/providers.md` — replace the "primary code on cloud"
    paragraph with the stickiness story.
  - `docs/troubleshooting.md` — new "STT keeps detecting the
    wrong language" section pointing at the tray override + the
    `languages` config field, plus a one-line note that English
    is a wizard suggestion and can be removed.
  - `CHANGELOG.md` — `Added` (stickiness, tray submenu),
    `Changed` (`cloud_rerun_on_language_mismatch` default,
    wizard flow), `Deprecated`
    (`cloud_force_primary_language`).

- [ ] Task 12. New ADR
  `docs/decisions/0017-cloud-stt-language-stickiness.md` (one page):
  why local-bridge was rejected, why stickiness is the
  cloud-native peer-symmetric mechanism, why English is a
  suggestion not a hard-coded peer.

- [ ] Task 13. `docs/status.md` session log entry.

## Verification Criteria

- Manual repro on Groq Turbo with `languages = ["en", "ro"]`: ten
  English clips read by a non-native speaker. Before: some return
  Russian/Ukrainian text. After: at most one initial mistake; from
  call N+1 the cache forces `language=en` and zero further
  misclassifications until the user actually speaks Romanian.
- A user editing `config.toml` to `languages = ["ro"]` produces
  exclusively Romanian transcripts; English is fully removable.
- Wizard run on `LANG=ro_RO.UTF-8` host → wizard suggests
  `["ro", "en"]` without any typing.
- Slim cloud-only build (`--no-default-features --features
  tray,cloud-all`) compiles, ships, and exhibits the same
  stickiness behaviour. No `whisper-rs` symbols leak in.
- `cargo test --workspace --all-features` and slim build green.

## Potential Risks and Mitigations

1. **Stickiness pins the wrong language on first call after a
   topic switch.**
   Mitigation: tray one-shot Force. Notification on rerun makes
   the cache state visible. Cache evicts on `Reload`.

2. **Multiple users share `~/.cache/fono` on a multi-user host.**
   Mitigation: `~/.cache` is already per-user; nothing new here.

3. **A user without OS-locale support (minimal container, Windows
   service) hits the wizard.**
   Mitigation: locale detection returns empty → wizard pre-fills
   `["en"]`. User can edit later.

4. **Flipping `cloud_rerun_on_language_mismatch` default surprises
   cost-sensitive users.**
   Mitigation: changelog entry under `Changed`, docs call out the
   knob, the rerun fires only on actual mismatches (rare once
   stickiness converges).

5. **Cache file corruption.**
   Mitigation: parse failure → start with empty cache, log at
   `debug`. Best-effort writes; a write failure is silent.

## Alternative Approaches

1. **Pure rerun-on-mismatch, no cache.**
   Flip the existing knob to `true` and stop. Trade-off: every
   misclassification costs a round-trip (200–600 ms) — perpetually,
   not just once. Stickiness amortises that cost to near-zero
   after the first call. Not recommended as the sole fix.

2. **Reintroduce the local-Whisper bridge as opt-in.**
   Compile it under the existing `whisper-local` feature; gate
   activation on a config flag the user must set explicitly.
   Trade-off: doubles the implementation surface for a feature
   that helps the wrong audience (users with local Whisper
   already have symmetric handling). Defer until a concrete user
   asks. Not in this plan.

3. **Provider-native multi-language modes (Deepgram
   `language=multi`).**
   Use them where available. Trade-off: provider-specific; Groq
   Turbo (the actual buggy backend) doesn't expose one. Track as
   a per-backend follow-up after stickiness is in place.
