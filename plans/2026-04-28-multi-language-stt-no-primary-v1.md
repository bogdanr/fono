# Multi-Language STT Without Primary/Secondary Friction

## Objective

Mitigate Groq Turbo (and other cloud STT) misclassifying a non-native
English speaker's audio as a third language (e.g. Russian) without
forcing the user to designate a "primary" language. The user describes
their workflow as "I always speak English plus optionally one other
language". The plan reframes the existing `LanguageSelection` UX as a
**peer set** rather than an ordered allow-list, eliminates the
primary/secondary distinction wherever the architecture allows, and
falls back to a tray-driven explicit override only when the underlying
provider truly cannot accept a multi-language signal.

## Background — what we already have

- `crates/fono-stt/src/lang.rs:25-37` — `LanguageSelection { Auto,
  Forced(c), AllowList(Vec) }` already plumbed through every backend.
  `LanguageSelection::primary()` returns `vec.first()` — that
  ordering is the *only* place the current schema implies primacy.
- `crates/fono-core/src/config.rs:101` — `general.languages: Vec<String>`
  is the user-facing field.
- Local STT (`crates/fono-stt/src/whisper_local.rs:189-280`) already
  implements **symmetric** allow-list enforcement via
  `lang_detect` mask + argmax — no primary preference; the audio
  decides.
- Cloud STT (`crates/fono-stt/src/groq.rs:139-203`) is where the
  asymmetry leaks: provider APIs accept exactly one `language` field
  per request, so the current code either (a) sends nothing and
  accepts the buggy auto-detect, (b) sends `primary` if
  `cloud_force_primary_language=true`, or (c) post-validates and
  optionally reruns with `primary` if
  `cloud_rerun_on_language_mismatch=true`. Both knobs default `false`,
  which is exactly why the user's English currently ships up as
  Russian.
- Tray (`crates/fono-tray/src/lib.rs:399-403`) already exposes
  STT / LLM submenus with active-marker rendering and reload
  actions; adding a Languages submenu mirrors that pattern.
- ADR 0016 (`docs/decisions/0016-language-allow-list.md`)
  documented the cloud "best-effort" caveat. This plan partially
  supersedes ADR 0016's "primary code for cloud" stance and needs a
  short ADR addendum.

## Strategic shape

Three layers, applied in priority order. The first layer that fits
the user's environment is enough; later layers are fallbacks.

1. **Audio-driven peer detection (local Whisper bridge).** When local
   Whisper is available in the running build (the default profile
   ships it), use a fast `lang_detect` pass on the prefix of the
   captured audio — *before* dispatching to cloud STT — to pick a
   single forced code from the peer set. Cost: ~5–15 ms on a
   tiny/base model that's already loaded for prewarm. Result: the
   cloud request always carries the **right** `language=` value,
   and there is no primary/secondary; the audio decides every call.
2. **OS-locale seeding (zero-config inference).** First-run wizard
   reads `LANG` / `LC_ALL` / keyboard layout (`localectl`,
   `setxkbmap -query`, GNOME `gsettings`) and timezone to **suggest**
   the peer set without forcing a primary. English is always
   pre-selected; the inferred locale is added as a peer if non-en.
   The user confirms with one key. Stored as
   `languages = ["en", "ro"]` — order is incidental, not semantic.
3. **Tray submenu manual override (last-mile escape hatch).** New
   "Languages" submenu shows the configured peer set with
   checkboxes plus a "Force next dictation: …" radio that
   deactivates after one capture. Slim cloud-only builds (no local
   Whisper, e.g. `--features tray,cloud-all`) rely primarily on
   this layer, since they can't run local `lang_detect`.

The combination keeps "I dictate in English plus Romanian" the
default mental model, with a one-click escape when the user knows
this particular utterance is in the secondary language and the local
bridge can't run.

## Implementation Plan

- [ ] Task 1. Re-purpose `LanguageSelection::primary()` to return
  `Option<&str>` documented as a **fallback hint** (only consulted
  by transports that physically cannot send multi-language signals).
  Rename the internal accessor to `fallback_hint()` and leave
  `primary()` as a deprecated alias (one release cycle) to soften the
  schema break. Update every call site under `crates/fono-stt/`,
  `crates/fono-bench/`, `crates/fono/` to use the new name.
  Rationale: surface the asymmetry as a **transport** concern, not a
  user-facing concept.

- [ ] Task 2. New `crates/fono-stt/src/lang_bridge.rs` module
  exposing `pub async fn detect_from_peers(pcm: &[f32], sr: u32, peers:
  &[String]) -> Option<String>`. Implementation reuses the existing
  `whisper_local::lang_detect_from_prefix` helper at
  `crates/fono-stt/src/whisper_local.rs:221-280` against a
  process-wide cached `tiny.en`-or-`base`-class state (whichever the
  user already has materialised in `~/.cache/fono/models/stt/`).
  Returns `None` when no local model is present (slim builds) or
  when `lang_detect` confidence is below a threshold so the cloud
  call can still fall back to its own auto-detect rather than be
  forced into a wrong code. Rationale: orthogonal to backend
  selection — same bridge serves Groq, OpenAI, Deepgram, …

- [ ] Task 3. Wire the bridge into the cloud STT path. In
  `crates/fono-stt/src/groq.rs:139-203` (and the analogous
  `openai.rs`, `deepgram.rs`, `assemblyai.rs`, `cartesia.rs`,
  `groq_streaming.rs`), when `LanguageSelection::AllowList` and
  the bridge returns `Some(code)`, send that code as `language=`
  on the **first** request (replacing today's
  `cloud_force_primary_language` branch). When the bridge returns
  `None`, fall back to today's behaviour. Keep the existing
  post-validation rerun as a safety net; it now fires far less
  often. Rationale: closes the user's actual bug
  (Groq Turbo classifying their English as Russian) at the
  request level, not the response level — no extra round-trip.

- [ ] Task 4. Flip `cloud_rerun_on_language_mismatch` default from
  `false` to `true` for **slim cloud-only builds** (no
  `whisper-local` feature) so users without the local bridge still
  get correctness-by-default at the cost of one occasional retry.
  Local-built users keep `false` because the bridge already prevents
  most mismatches. Implement via `cfg!(feature = "whisper-local")`
  inside `General::default()` at
  `crates/fono-core/src/config.rs:131-145`. Rationale: each profile
  gets the cheapest path to "transcript text matches what I said".

- [ ] Task 5. New `crates/fono-core/src/locale.rs` with
  `pub fn detect_os_languages() -> Vec<String>` returning a
  best-effort BCP-47 list inferred from, in order:
  (a) `LC_ALL` / `LC_MESSAGES` / `LANG` env vars (POSIX two-letter
  prefix);
  (b) `localectl status` parsed for `System Locale: LANG=…` and
  `X11 Layout: …` (Linux);
  (c) `gsettings get org.gnome.desktop.input-sources sources` if
  present (GNOME);
  (d) `setxkbmap -query` (X11 fallback);
  (e) `defaults read .GlobalPreferences AppleLanguages` on macOS;
  (f) `Get-WinUserLanguageList` PowerShell on Windows.
  Returns deduplicated, lowercased, alpha-2 codes. Gracefully
  returns empty on every error — never panics. Time-zone is read
  via `localectl show --property=Timezone` and **only** used as a
  tiebreaker (e.g. distinguishing `pt_PT` from `pt_BR` when the
  locale is bare `pt`). Rationale: the user's "find from the OS"
  hint, kept single-purpose and well-tested.

- [ ] Task 6. Wizard integration in
  `crates/fono/src/wizard.rs:80-100` (config-save section). Replace
  the current free-text language prompt with:
  > **Languages you dictate in** [English ✓, Romanian ✓ (detected
  > from OS), + add another]
  English is always pre-checked and cannot be unchecked (matches the
  user's explicit mental model). The OS-detected language(s) are
  pre-checked but unchecking is allowed. Persists as
  `languages = [...]`. Rationale: zero-typing for the common
  bilingual case; the user said this needed to be intuitive.

- [ ] Task 7. New tray "Languages" submenu mirroring the
  STT/LLM submenu pattern at `crates/fono-tray/src/lib.rs:399-450`:
  - Static checkbox list of the configured peer set (`General::languages`);
    toggling persists to disk and triggers `Reload` so the
    orchestrator picks up the change without daemon restart, mirroring
    `set_active_stt`.
  - Radio "Force next dictation as: [Auto / English / Romanian / …]"
    — one-shot override that decays after one successful pipeline.
    Stored in tray state, not config. Surfaces as a per-call
    `LanguageSelection::with_override(...)` at the existing call
    site in `crates/fono/src/session.rs:1188-1192`. Rationale: the
    user's tray-toggle suggestion, scoped to one capture so it
    cannot accidentally pin the wrong language across sessions.

- [ ] Task 8. New `[stt].mismatch_warn_notification: bool` (default
  `true`). When the cloud-only fallback path detects a
  language mismatch in the response, fire a desktop notification
  ("Fono detected a language outside your set — re-running…" or
  "…accepted as-is, set Force in tray") so the user sees what
  happened instead of silently getting wrong text. Threshold-rate
  the notification to once per 60 s to avoid spam during a long
  multi-language session. Rationale: the current `tracing::warn!`
  at `groq.rs:188-193` is invisible to non-CLI users.

- [ ] Task 9. New ADR `docs/decisions/0017-multi-language-no-primary.md`
  documenting the peer-set model, the audio-driven bridge as the
  primary mechanism, and the explicit decision that `primary()` is
  now a transport-level fallback hint not a user concept. References
  ADR 0016 as superseded in part. Short — one page.

- [ ] Task 10. Tests covering the new mechanisms:
  - `crates/fono-stt/src/lang_bridge.rs` — table-driven tests with
    canned `(pcm_fixture, peer_set, expected_code_or_none)` rows;
    use the existing `tests/fixtures/` WAVs.
  - `crates/fono-stt/tests/groq_bridge.rs` (new) — uses the
    `with_request_fn` closure injection (already there for Wave 3
    Thread C) to assert the cloud request carries the
    bridge-picked language, not the configured first entry.
  - `crates/fono-core/src/locale.rs` — pure-function tests that
    mock env vars (`std::env::set_var` in a serial test) and assert
    the right code is returned; on-the-OS shell-out paths are
    feature-gated behind a test-only "actually-shell-out" flag so
    CI stays hermetic.
  - `crates/fono-tray/src/lib.rs` — tray-Languages submenu unit
    tests where `MenuEvent` matching is already mocked.
  - Wizard integration test asserting OS-detected languages
    pre-populate the persisted `languages` field.

- [ ] Task 11. Docs:
  - `docs/providers.md` — replace the "primary code on cloud STT"
    paragraph with the bridge + fallback story.
  - `docs/troubleshooting.md` — new section "STT keeps detecting
    the wrong language" pointing at the tray override and the
    bridge-availability check (`fono doctor` should print whether
    the local bridge is loadable).
  - `docs/wayland.md` / `docs/inject.md` — leave alone.
  - `CHANGELOG.md` — `### Added`, `### Changed`, `### Fixed`
    entries under Unreleased.

- [ ] Task 12. `docs/status.md` session log entry summarising:
  bug → peer-set reframe → 3-layer mitigation → the specific fix
  for Groq Turbo's English-as-Russian misclassification.

## Verification Criteria

- Manual repro of the user's bug: ten clips of accented English on
  Groq Turbo with `languages = ["en", "ro"]` configured. Before:
  some clips return Cyrillic. After: zero clips return text in a
  language outside the peer set, regardless of which build profile
  is in use.
- Unit tests for `detect_from_peers` correctly pick `en` for a
  known-English fixture and `ro` for a known-Romanian fixture
  when both are in the peer set; return `None` for low-confidence
  audio.
- Wizard run on a host with `LANG=ro_RO.UTF-8` produces a config
  with `languages = ["en", "ro"]` without typing anything.
- Tray Languages submenu: toggling "Romanian" off, then dictating,
  then toggling it back on works without daemon restart and is
  reflected immediately in `fono doctor`.
- `cargo test --workspace --all-features` and slim
  (`--no-default-features --features tray,cloud-all`) both green.
- No regression on the existing local-Whisper allow-list tests at
  `crates/fono-stt/tests/lang_*.rs`.

## Potential Risks and Mitigations

1. **Local bridge adds latency to every cloud STT call.**
   Mitigation: `lang_detect` is encoder-only on the first ~6 s of
   audio; cached state across calls; on a tiny/base model it's
   ~5–15 ms. Bench gate in `fono-bench equivalence` covers it.
   Skipped entirely when the audio is < 0.8 s (heuristic: too
   short for reliable detection — fall back to today's path).

2. **Bridge picks the wrong language confidently and the user
   never sees auto-detect again.**
   Mitigation: a confidence threshold (e.g. `prob_top - prob_2nd <
   0.15`) on the masked argmax forces the bridge to return `None`,
   letting the cloud provider auto-detect. The post-validation
   rerun stays armed for slim builds.

3. **OS-locale detection misfires on minimal containers / non-Linux
   hosts.**
   Mitigation: every `locale.rs` source falls back gracefully on
   error and returns at most a hint list. Wizard always lets the
   user override before persisting, so a misfire is one un-check
   away.

4. **Tray "Force next dictation" radio confuses users into thinking
   it's persistent.**
   Mitigation: label clearly ("Force next dictation only") and emit
   a tray notification when the override fires + decays
   ("Forced: Romanian — applied to last dictation, now Auto again").

5. **ADR 0016's `cloud_force_primary_language` knob becomes
   semantically dead.**
   Mitigation: keep it on the schema with a deprecation comment;
   ignore at runtime when the bridge is enabled; remove in two
   release cycles.

6. **Schema churn breaks downstream tooling.**
   Mitigation: `LanguageSelection::primary()` stays as a deprecated
   alias for `fallback_hint()`; `cloud_force_primary_language` and
   `cloud_rerun_on_language_mismatch` keep their TOML names and
   serde defaults.

## Alternative Approaches

1. **Cloud-only solution: rerun-on-mismatch by default everywhere.**
   Cheapest patch: just flip `cloud_rerun_on_language_mismatch` to
   `true` unconditionally. Trade-off: every misclassification costs
   one extra cloud round-trip (200–600 ms), and the rerun still
   needs to choose a code so the "primary" concept persists for
   that fallback. Doesn't solve the user's UX complaint, only the
   correctness one. Recommended only if Tasks 2–3 are too heavy.

2. **Two-shot parallel cloud requests.**
   Send the audio in parallel with each peer code forced; pick the
   response with higher self-reported confidence. Truly symmetric,
   no primary anywhere. Trade-off: doubles cloud cost on every
   dictation, and Groq's per-request rate limits would bite hard
   on a multi-language session. Not recommended.

3. **Defer to provider-level language hints (Deepgram
   `language: multi`).**
   Some providers have native multi-language modes that don't
   force a single code. Trade-off: provider-specific; Groq Turbo
   (the buggy backend in the bug report) does **not** offer this.
   Not a general solution; covered as an enhancement in
   `crates/fono-stt/src/deepgram.rs` if/when relevant.

4. **Move the user wholesale to local Whisper.**
   Local Whisper's allow-list enforcement is already symmetric.
   Trade-off: latency / hardware story is what drove the user to
   cloud in the first place. Recommend in `docs/troubleshooting.md`
   as the "if you only ever dictate in two close languages and
   want zero misclassifications" path; not the default fix.
