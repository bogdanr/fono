# Multi-Language STT Without Primary/Secondary Friction

## Objective

Eliminate cloud-STT language misclassification (Groq Turbo classifying
non-native English as Russian, etc.) for users on resource-constrained
machines who chose cloud STT precisely to **avoid** local inference.

Three hard constraints, derived from the conversation that produced
this plan:

1. **Cloud-native.** No local-Whisper passes. The user's whole reason
   for being on cloud is to keep CPU/RAM idle.
2. **No primary/secondary user model.** The user lists languages they
   dictate in; Fono treats every entry as an equal peer. Order in the
   config array is cosmetic and consulted nowhere at runtime.
3. **Switcher-safe.** Users who alternate languages (Romanian one
   minute, English the next) must not pay a per-utterance penalty,
   and a single misclassification must never lock subsequent calls
   into the wrong language.

This v3 supersedes v2 (`2026-04-28-multi-language-stt-no-primary-v2.md`).
v2 sent the cached code on every call as `language=`, which silently
broke switchers — once the cache pinned `ro`, every English utterance
afterwards was decoded as Romanian until something reset the cache.
v3 inverts the cache role to fix that.

## Background — what we already have

- `crates/fono-stt/src/lang.rs:25-37` — `LanguageSelection { Auto,
  Forced(String), AllowList(Vec<String>) }`.
- `crates/fono-core/src/config.rs:101` — `general.languages:
  Vec<String>`, default empty.
- `crates/fono-stt/src/groq.rs:139-203` — cloud post-validation +
  optional rerun. Both knobs (`cloud_force_primary_language`,
  `cloud_rerun_on_language_mismatch`) default `false`.
- `crates/fono-stt/src/groq_streaming.rs` — `with_request_fn` closure
  injection (Wave 3 Thread B) is reused by tests.
- Tray submenu pattern at `crates/fono-tray/src/lib.rs:399-403`.
- ADR 0016 documents the current allow-list. This plan keeps that
  schema and adds an in-memory cache plus a wizard reframe.

## Strategic shape

The cloud STT `language=` parameter is a **hard force**, not a hint:
once set, the model trusts the caller completely and decodes audio as
that language even when it isn't. v2 sent the cache on every request
and therefore mangled every switched utterance. v3 takes the opposite
default:

> **First call always unforced. The cache is only ever consulted as a
> rerun target.**

Three layers, ordered by automation:

1. **Unforced first call + post-validation rerun.** Cloud auto-detect
   handles the common bilingual case for free; switching ro↔en costs
   nothing extra. When auto-detect returns a banned language *and* we
   have a confident cache value, a single rerun forces that cached
   code. When the cache is empty (cold start), we accept the
   banned-language transcript as-is — same as today's behaviour
   without rerun. This avoids the "guess wrong on the rerun and lock
   in" failure mode.

2. **OS-locale bootstrap, in-memory only.** At daemon start, seed the
   cache from `LANG`/`localectl`/`AppleLanguages`/
   `Get-WinUserLanguageList` *if and only if* the detected code is
   already in `general.languages`. No file persistence — daemon
   restarts rebuild the cache within one or two utterances, and a
   stale persisted cache could mislead the rerun.

3. **Wizard suggestion + tray submenu.** First-run wizard prefills
   `["en"]` or `[<os_locale>, "en"]` as a *suggestion only* — the
   user can uncheck English, edit it out of `config.toml`, or skip
   the wizard. Tray "Languages" submenu shows the peer set (toggle
   = persist) plus a one-shot "Force next dictation as: …" radio
   that decays after one capture.

The local-Whisper bridge from v1 stays dropped. The on-disk cache
from v2 is dropped (in-memory only).

## Order is cosmetic

`LanguageSelection::primary()` is renamed to `fallback_hint()` and
its use restricted to **single-language cloud transports** that
physically cannot accept a peer set. Outside that narrow case, no
code path consults config-array order. Specifically:

- First call: no `language=` field sent for `AllowList`. Order
  irrelevant.
- Rerun with cache populated: cache value used. Order irrelevant.
- Rerun with cache empty: **no rerun fires.** We accept the
  unforced response, log at `debug`, and let the cache populate
  from the next correctly-detected utterance. Order irrelevant.
- Cold-start cache bootstrap: from OS locale, not from
  `languages[0]`. Order irrelevant.

This keeps `Vec<String>` as the wire schema (TOML serialises arrays
either way) while making it impossible for a user's typing order in
the wizard to silently change runtime behaviour.

## Implementation Plan

- [x] **Task 1.** Rename `LanguageSelection::primary()` →
  `fallback_hint()` in `crates/fono-stt/src/lang.rs`. Doc-comment:
  "Used only by single-language cloud transports that cannot accept
  a peer set. Do not use as a 'primary language' notion — Fono
  treats every entry in `languages` as an equal peer." Keep
  `primary()` as a `#[deprecated]` shim for one release.

- [x] **Task 2.** New `crates/fono-stt/src/lang_cache.rs`. Pure
  in-memory:

  ```rust
  pub struct LanguageCache {
      inner: parking_lot::RwLock<HashMap<BackendId, String>>,
  }

  impl LanguageCache {
      pub fn new() -> Self;
      pub fn get(&self, backend: BackendId) -> Option<String>;
      pub fn record(&self, backend: BackendId, code: String);
      pub fn clear(&self);
      pub fn seed_if_empty(&self, backend: BackendId, code: String);
  }
  ```

  No serde, no file I/O, no `state_dir` plumbing. `BackendId` is a
  small enum (`Groq`, `OpenAI`, `Deepgram`, `AssemblyAi`,
  `Cartesia`, `GroqStreaming`, …). One `Arc<LanguageCache>` lives
  in the daemon and is cloned into each backend.

- [x] **Task 3.** OS-locale bootstrap. New
  `crates/fono-core/src/locale.rs`:

  ```rust
  pub fn detect_os_languages() -> Vec<String>;
  ```

  Reads `LC_ALL` / `LC_MESSAGES` / `LANG`, `localectl status`
  (Linux), `defaults read .GlobalPreferences AppleLanguages`
  (macOS), `Get-WinUserLanguageList` (Windows). Returns
  deduplicated lowercased alpha-2 codes. Empty on any error.

  Daemon startup (`crates/fono/src/daemon.rs`): after loading
  config, take the first OS-detected code that is in
  `general.languages` and call `cache.seed_if_empty` for **every**
  registered backend with that code. If no OS code matches, leave
  the cache empty.

- [x] **Task 4.** Wire cache into the **rerun path only** for every
  cloud STT backend (`groq.rs`, `openai.rs`, `deepgram.rs`,
  `assemblyai.rs`, `cartesia.rs`, `groq_streaming.rs`). On request:

  - `Forced(c)` → send `c` (unchanged).
  - `Auto` → send nothing (unchanged).
  - `AllowList` → **send nothing** on the first request.

  On response:
  - Detected language ∈ allow-list → accept transcript;
    `cache.record(backend, code)`.
  - Detected language ∉ allow-list:
    - `cache.get(backend)` returns `Some(c)` → rerun with
      `language=c`. **Do not** record from the rerun response —
      the rerun was forced, so its `language` field is not
      independent evidence.
    - `cache.get(backend)` returns `None` → **no rerun.** Accept
      the unforced transcript as-is, log at `debug` ("language
      mismatch but no cache; accepting unforced response").

- [x] **Task 5.** Flip `cloud_rerun_on_language_mismatch` default to
  `true`. Note in the field doc-comment: "When the cache has a
  recently-detected peer language, a banned auto-detect triggers
  one rerun forced to that code; on cold start (empty cache) the
  unforced response is accepted."

- [x] **Task 6.** Mark `cloud_force_primary_language` as
  `#[deprecated]` in the field doc-comment. With the cache running
  it is semantically dead. Schedule removal in v0.5; serde keeps
  accepting it as `#[serde(default, alias = …)]` for one release.

- [x] **Task 7.** Wizard rework in `crates/fono/src/wizard.rs`.
  Replace the free-text language prompt with a "Languages you
  dictate in" checkbox step.

  - Detect OS locale via Task 3.
  - If detected ∈ {empty, `en`}: pre-check `["en"]`.
    Otherwise pre-check `[<os_locale>, "en"]`.
  - Render checkboxes; user toggles, presses Enter.
  - English is **default-on but uncheckable** by toggling — no
    special-case enforcement code. The wizard simply pre-checks
    `en`; the user can uncheck it like any other entry. Persist
    whatever set the user confirmed.
  - Copy: "Languages you dictate in (Fono treats them as peers — no
    primary)." Avoid the words "primary" and "default language"
    anywhere in the wizard text.

- [x] **Task 8.** Tray "Languages" submenu in
  `crates/fono-tray/src/lib.rs`, mirroring the STT/LLM submenu
  pattern:

  - Static checkbox list of `general.languages`. Toggle persists
    to disk + emits `Reload`.
  - Radio "Force next dictation as: [Auto / <each peer>]"
    one-shot override; emits a tray notification when applied and
    again when decayed. Options sorted alphabetically by display
    name so submenu order ≠ config order.
  - "Clear language memory" item that calls `cache.clear()` —
    useful when the cache has gone stale across topic changes.

- [x] **Task 9.** Rate-limited desktop notification when the rerun
  fires: "Fono retried with language=ro — set Force in tray if this
  is wrong." Once per 60 s ceiling per backend.

- [x] **Task 10.** Tests:

  - `lang_cache.rs` — `seed_if_empty` no-ops when populated;
    `record` + `get` round-trip; `clear` empties.
  - `groq.rs` (extend) using `with_request_fn` closure:
    - Two calls, first returns banned `ru`, cache empty → no
      rerun, transcript returned unchanged.
    - Two calls, first returns `en` (in allow-list) → cache
      records `en`; second returns banned `ru` → rerun fires
      with `language=en`.
    - Switcher trace: ro → en → en → ro returns three accepted
      transcripts and zero reruns.
  - `locale.rs` — env-var permutations under `serial_test`.
  - Wizard integration test: `LANG=ro_RO.UTF-8` → wizard
    suggests `["ro", "en"]`; `LANG=en_US.UTF-8` → `["en"]`;
    user can uncheck `en` before persistence.
  - Tray submenu unit test for the one-shot Force decay.

- [x] **Task 11.** Docs:

  - `docs/providers.md` — replace the
    `cloud_force_primary_language` paragraph with the
    cache-as-rerun-target story; explicitly call out
    "switcher-safe by default".
  - `docs/troubleshooting.md` — new "Cloud STT keeps detecting the
    wrong language" section: wait for the rerun, use tray Force
    for one-shot, "Clear language memory" if it's gone stale,
    edit `config.toml` to remove peers permanently. One sentence
    that English is a wizard suggestion and freely removable.
  - `CHANGELOG.md` — `Added` (in-memory cache, OS-locale
    bootstrap, tray Languages submenu, "Clear language memory"),
    `Changed` (`cloud_rerun_on_language_mismatch` default →
    `true`; wizard flow), `Deprecated`
    (`cloud_force_primary_language`, `LanguageSelection::primary`
    alias).

- [x] **Task 12.** New ADR
  `docs/decisions/0017-cloud-stt-language-stickiness.md`:

  - Why local-Whisper bridge was rejected (resource budget).
  - Why on-disk cache was rejected (stale-cache risk vs marginal
    cold-start benefit).
  - Why the cache is **rerun-only** rather than first-call force
    (switcher safety).
  - Why no primary/secondary in the user model (peer-symmetric
    semantics; order leaks are bugs, not features).

- [x] **Task 13.** `docs/status.md` session log entry.

## Verification Criteria

- **Switcher trace.** With `languages = ["ro", "en"]`, dictate
  `ro → en → en → ro` (eight clips, two of each in alternation).
  Zero reruns when auto-detect is correct. Transcripts in the
  correct language for every clip. The cache value at the end
  reflects whichever language was last spoken, not config order.

- **Self-healing trace.** With `languages = ["en", "ro"]` on Groq
  Turbo, ten English clips by a non-native speaker. Auto-detect
  returns `ru` on, say, two of them. Both reruns fire with
  `language=en` (cache populated from earlier correct detections);
  the user-visible transcripts are English ten out of ten.

- **Cold-start trace.** Fresh daemon start, OS locale `en_US`,
  configured `languages = ["en", "ro"]`. First clip is English,
  Groq misdetects as `ru`. Cache was seeded with `en` at startup
  (OS locale ∈ allow-list) → rerun fires with `language=en` →
  English transcript returned.

- **Cold-start, empty cache.** Daemon start with `LANG=de_DE` but
  configured `languages = ["en", "ro"]` (no overlap). Cache stays
  empty. First English clip misdetected as `ru` → no rerun, banned
  transcript returned, debug log notes the skip. Second clip
  detected correctly as `en` → cache populates → from there
  on-self-healing.

- **Removable English.** A user editing `config.toml` to
  `languages = ["ro"]` produces exclusively Romanian transcripts;
  English is fully removable; no warning, no special case.

- **Order-doesn't-matter.** Two configs `languages = ["ro", "en"]`
  and `languages = ["en", "ro"]` produce **byte-identical**
  transcripts on the same audio fixtures. (Asserted in an
  integration test that runs both orders against a recorded
  fixture set.)

- **Slim cloud build.** `--no-default-features --features
  tray,cloud-all` compiles, ships, and exhibits the same behaviour.
  No `whisper-rs` symbols leak in. `cargo bloat` shows no
  language-detection model loaded.

- `cargo test --workspace --all-features` and slim build green;
  `cargo clippy --workspace --all-targets -- -D warnings` clean.

## Potential Risks and Mitigations

1. **Cache goes stale across long topic switches** (user dictates
   English all morning, switches to Romanian after lunch, first
   Romanian clip is misdetected as Bulgarian).
   Mitigation: cache is rerun-only and the rerun is forced toward
   `en` once, returning gibberish English. The debug log records
   the rerun and its forced code. User's recourse: tray "Force
   next dictation as: Romanian" for the next clip, after which the
   cache repopulates correctly. The "Clear language memory" tray
   item is the nuclear option.

2. **Cold-start with no allow-list overlap on OS locale** (user in
   Germany dictates English/Romanian).
   Mitigation: by design, no rerun fires until the cache populates
   from a correctly-detected utterance. First misdetection round-
   trips through the user once; from utterance two onward, normal
   self-healing applies.

3. **Provider returns `language` in a non-BCP-47 form** (full names,
   ISO-3, locale tags).
   Mitigation: per-backend normalisation already lives in
   `crates/fono-stt/src/lang.rs` (extend the helper). Cache stores
   normalised alpha-2 codes only.

4. **Flipping `cloud_rerun_on_language_mismatch` default surprises
   cost-sensitive users.**
   Mitigation: changelog `Changed` entry; docs call out the knob
   and the cold-start no-rerun behaviour. The rerun fires only on
   actual mismatches with a populated cache (rare once stickiness
   converges).

5. **Cache concurrency** under simultaneous live + batch pipelines.
   Mitigation: `parking_lot::RwLock` around a `HashMap`; record
   takes a write lock briefly. No blocking I/O held across the
   cloud request.

## Alternative Approaches

1. **Pure rerun-on-mismatch, no cache.**
   Flip the existing knob to `true` and stop. Trade-off: every
   misdetection costs a round-trip perpetually, and the rerun has
   no informed code to force toward — it would have to fall back
   to `languages[0]`, reintroducing the order-leak problem. Not
   recommended.

2. **Confidence-aware secondary rerun for 2-peer sets.**
   When the forced rerun's `verbose_json` average per-segment
   `avg_logprob` is below `-1.0`, retry once with the *other*
   peer; pick the higher-logprob result. Trade-off: backend-
   specific JSON parsing surface and a third round-trip in the
   pathological case. Compelling when the data justifies it; defer
   until we have telemetry showing rerun-also-wrong is non-rare.
   Track as a follow-up plan, not in v3.

3. **Provider-native multi-language modes (Deepgram
   `language=multi`, AssemblyAI `language_detection=true` with
   `expected_languages`).**
   Use them where available. Trade-off: provider-specific; Groq
   Turbo (the actual buggy backend) doesn't expose one. Track as a
   per-backend follow-up after the cache is in place.

4. **Reintroduce the local-Whisper bridge as opt-in.**
   Compile under the existing `whisper-local` feature; gate
   activation on a config flag the user must set. Trade-off:
   doubles the implementation surface for a feature that helps the
   wrong audience. Defer until a concrete user asks. Not in v3.
