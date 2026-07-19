# Speaker Verification — "Test My Voice", Calibration, Language & Cloud-STT UX

## Objective

Design the calibration / self-benchmark / enrollment-quality experience for
Fono's on-device speaker verification, answering seven product questions:

1. A "test my voice" flow — what it looks like and how it works.
2. Whether to allow enrollment from short (< 3 s) utterances.
3. Replaying utterances through the model to measure practical EER + latency.
4. Showing verification improving as enrolled samples go 1 → 3 → 4 → 5.
5. Using Fono's existing language detection/config meaningfully.
6. The important UX/UI pieces still missing for a great experience.
7. Pairing verification with cloud STT (not just local).

This plan is **design + task breakdown only**. It slots into
`plans/2026-07-17-speaker-verification-v1.md` Slices 4–5. No code here.

## Grounding (verified in-tree)

- Store is calibration-ready: `Calibration { genuine_mean, genuine_std,
  trials }` + `set_calibration()`, per-utterance `capture_source`,
  `remove_utterance()` (`crates/fono-core/src/speakers.rs:35-40,233-323`).
- Scoring is complete + model-free: `cosine`, `Cohort::as_norm`, `decide`,
  `confidence_from_margin`, `SpeechAccumulator.sufficient_audio`
  (`crates/fono-audio/src/speaker.rs:47-315`).
- Engine embeds raw 16 kHz PCM, dim 192, tens of ms/utterance.
- Language: `general.languages`, `LanguageSelection` (Auto/Forced/AllowList),
  local Whisper `lang_detect`, cloud detected-language
  (`crates/fono-stt/src/lang.rs`, `cartesia.rs`).
- Browser enrollment already lands (record → review → Submit/Discard) with
  DSP disabled + device picker; only embeddings persist, never audio.
- **Blocking dependency:** impostor-cohort sidecar not hosted → AS-Norm
  degrades to plain cosine, `threshold="auto"` under-determined (plan 1.3).

---

## Q1 — "Test my voice" calibration flow

### How it works (conceptual)
A per-speaker calibration that records a few **held-out** clips (separate from
the enrollment set), embeds each, and builds two score distributions:

- **Genuine**: held-out clip vs the speaker's own centroid.
- **Impostor**: the same clip vs (a) the shipped cohort once hosted, and/or
  (b) *other enrolled speakers* as local impostors (bootstrap before the
  cohort lands).

From those it derives: an operating threshold (EER-crossover or a target
false-accept point), a practical self-EER, and a verdict badge. Persists to
`Calibration` so `threshold="auto"` resolves.

### How it looks
A "Test my voice" card on the `#/speakers` section, per speaker: a Run button,
live capture (reusing the enrollment recorder), then a results panel with an
inline SVG histogram (genuine vs impostor), a threshold marker line, the
self-EER number, a plain-language verdict ("Good separation" / "Marginal —
enroll more" / "Poor — check mic & room"), and a "Use recommended threshold"
action.

### Implementation tasks
- [ ] Task Q1.1. Add a calibration compute function in `fono-audio` (pure,
      model-free): inputs = genuine scores + impostor scores; outputs =
      EER estimate, EER-threshold, and target-FAR threshold. Rationale:
      keep math in the tested `speaker.rs` layer, reusable by CLI + web.
- [ ] Task Q1.2. Daemon hook `calibrate_speaker`: accept N held-out PCM clips,
      embed each, score genuine vs own centroid and impostor vs cohort +
      other-speaker centroids, call `set_calibration`. Rationale: mirrors the
      existing `enroll_speaker` hook path over `SpeakerStore` + `SpeakerEngine`.
- [ ] Task Q1.3. Route `POST /api/speakers/{id}/calibrate` on the settings
      server (same loopback-trust + API-key rules as `/api/*`).
- [ ] Task Q1.4. `#/speakers` calibration card: recorder reuse, inline-SVG
      histogram + threshold marker, verdict badge, "use recommended threshold"
      writes `[speaker].threshold`. No new JS deps (draw with SVG like the
      existing Web Audio testers).
- [ ] Task Q1.5. Resolve `threshold="auto"` from `Calibration` + cohort at
      decision time (`decide` caller), with a documented fallback when
      uncalibrated.

## Q2 — Short (< 3 s) enrollment utterances

### Decision
Allow short enrollment clips with a **soft quality band**, not a hard 3 s gate.
Keep the existing ~1 s hard floor (`< 16000` samples rejected). Track
**cumulative enrolled seconds** and surface a target ("aim for ~15–30 s across
3–5 clips"). Verification keeps `min_speech_secs = 3.0` and the provisional
`sufficient_audio` path. Rationale: many decent short clips averaged into a
centroid are robust; the real failure mode is *too little total* speech or
bad audio, not any single short clip.

### Implementation tasks
- [ ] Task Q2.1. Per-utterance quality gauge (duration + level/SNR estimate)
      computed client-side at review time; warn (not block) below a soft
      threshold. Rationale: catch silence/clipping before it poisons the
      centroid.
- [ ] Task Q2.2. Show cumulative enrolled seconds + a "profile strength"
      indicator (utterances × total seconds × #channels × #languages).
- [ ] Task Q2.3. Keep the ~1 s hard floor; document the soft band in the UI
      hint and CLI.

## Q3 — Replay utterances to measure practical EER + latency

### Decision
Two honest levels, clearly labelled:
- **In-app (per-user)**: replay held-out genuine clips + impostor set through
  the engine → practical self-EER ("your mic, your room" — NOT a benchmark) +
  measured embed latency ("≈ X ms/utterance on this machine"). This satisfies
  the deferred CPU-RTF measurement.
- **Offline (dev/CI)**: `fono speaker test` / `identify` + the Python-oracle
  cross-check on a pinned trial list — the rigorous acceptance-gate EER
  (plan Slice 5.1).

### Implementation tasks
- [ ] Task Q3.1. Measure and surface per-embed wall-clock latency in the
      calibration result (min/median/max over the trial clips).
- [ ] Task Q3.2. Compute practical self-EER from the genuine/impostor score
      arrays (reuse Task Q1.1); label it as room/mic-specific, not a headline.
- [ ] Task Q3.3. `fono speaker test [name]` CLI: prints score distributions,
      self-EER, latency, active threshold verdict (plan Task 3.3 `test`).

## Q4 — Show improvement 1 → 3 → 4 → 5 utterances

### Decision
Compute an incremental-centroid curve: for k = 1..N, build the centroid from
the first k enrollment utterances, score the held-out genuine clips + impostor
set, and record the separation margin (or self-EER) at each k. Render a small
line chart that visibly climbs as k grows. Present as a smoothed **trend** with
a caveat about small-N noise — motivating without overclaiming.

### Implementation tasks
- [ ] Task Q4.1. Incremental-evaluation function (pure, model-free): given
      per-utterance embeddings + held-out genuine + impostor scores, return
      the separation/EER-vs-k series.
- [ ] Task Q4.2. Render the "matching confidence vs samples enrolled" line
      chart (inline SVG) in the calibration card, updating after each enroll.
- [ ] Task Q4.3. Nudge copy: "Add another sample to strengthen your profile"
      while the curve is still climbing.

## Q5 — Use Fono's language detection/config

### Decision
Embeddings stay language-agnostic (one model, no per-language fork), but
**record and reason about language** because cross-lingual verification
degrades EER (the CN-Celeb numbers in the base plan):
- Tag each enrollment utterance and each verification with its detected
  language (from Whisper `lang_detect` locally / cloud detected-language).
- Warn on cross-lingual mismatch (enrolled only in English, verifying in
  Romanian) — same pattern as the planned channel-mismatch warning.
- When `general.languages` has > 1 entry, prompt multi-language enrollment
  ("you dictate in English and Romanian — enroll a clip in each").

### Implementation tasks
- [ ] Task Q5.1. Add a `language` column to `speaker_utterances` (nullable;
      migration in the `api_keys.rs` pattern). Rationale: enables mismatch
      warnings + per-language coverage without touching embeddings.
- [ ] Task Q5.2. Populate language at enroll time (detected or configured) and
      thread the detected language into the verification decision.
- [ ] Task Q5.3. UI: per-language coverage chips + a cross-lingual mismatch
      warning; multi-language enrollment prompt when configured langs > 1.

## Q6 — Missing pieces for great UX/UI

### Prioritised (highest impact first)
1. **Live input meter + too-quiet / clipping / noisy warnings during capture**
   — bad audio is the #1 EER killer; this is the single biggest quality win.
2. **Immediate post-enroll sanity check** — score the just-recorded clip vs the
   growing centroid; confirm "this sample matches your profile ✓" so users
   don't silently enroll silence or the wrong person.
3. **Per-utterance management list** — source/language/date + delete/re-record
   individual clips (`remove_utterance` already exists); no playback (only
   embeddings are stored — state that).
4. **Profile-strength indicator** — utterances × total seconds × #channels ×
   #languages × self-EER, with actionable nudges.
5. **Security-posture explainer in-UI** — "convenience gate, not a lock"; make
   the ADR's asymmetric-gating stance visible where users act on it.
6. **Channel/device guidance + recorded device label** — "enroll through the
   same mic you dictate with"; warn on channel mismatch.
7. **Doctor integration** — mic-permission issues, model-not-downloaded,
   cohort-not-hosted (AS-Norm degraded) warnings.
8. **Second-factor (PIN) UX** for gating fail-deadly actions (base-plan
   security posture) — registration + prompt flow.
9. **Accessibility** — keyboard control of record/submit, ARIA live region for
   status; **failure transparency** — reject reason (low score / insufficient
   audio / cross-language).
10. **Re-enrollment / drift prompts** — voices and rooms change over time.

### Implementation tasks
- [ ] Task Q6.1. Capture-time VU meter + quiet/clipping/noise heuristics.
- [ ] Task Q6.2. Post-enroll self-match confirmation.
- [ ] Task Q6.3. Per-utterance table with individual delete/re-record.
- [ ] Task Q6.4. Profile-strength widget + nudges.
- [ ] Task Q6.5. In-UI security-posture explainer + reject-reason surfacing.
- [ ] Task Q6.6. Doctor Speaker-section checks (mic/model/cohort/permission).
- [ ] Task Q6.7. (Deferred/optional) second-factor PIN registration + gate UI.

## Q7 — Pairing with cloud STT

### Key principle
Speaker verification is **local-only and STT-backend-independent**. The
embedding runs on the daemon's ONNX stack on the same 16 kHz PCM buffer,
concurrently with STT, whether STT is local or cloud. The biometric embedding
**never** goes to any cloud; audio is not sent to any cloud *speaker-ID* API.

### Behaviour by mode
- **Local STT**: embed in parallel with transcription on the same buffer;
  zero extra audio movement.
- **Cloud STT**: audio is uploaded for STT, but the embedding is computed
  locally on the same PCM before/while uploading; the decision is attached to
  the transcript locally after both return.
- **Remote daemon / OpenAI-compatible upload / Wyoming satellite**: audio
  reaches the daemon, which embeds locally even if STT then goes to cloud
  (same model as browser enrollment).
- **Latency**: local embed (~tens of ms) ≪ cloud round-trip, so the decision
  is ready before the transcript — run concurrently (`tokio::join!`), no added
  user-visible latency.
- **Language for free**: cloud STT's detected language feeds the decision's
  language tag + cross-lingual mismatch warning (Q5) at no extra cost.

### Implementation tasks
- [ ] Task Q7.1. In the Slice-4 pipeline wiring, run local embedding
      concurrently with the STT call (local or cloud) and join before tagging
      the transcript. Rationale: hides embed latency behind cloud round-trip.
- [ ] Task Q7.2. Guarantee (and document) that biometric data never enters any
      cloud request path; add a test asserting the embedding stays local in
      cloud-STT mode.
- [ ] Task Q7.3. Attach cloud-detected language to the speaker decision when
      cloud STT is active; reuse it for the Q5 mismatch warning.
- [ ] Task Q7.4. Docs (`docs/privacy.md`): state that even in cloud-STT mode
      the voice-print is computed and kept locally; only STT audio leaves.

---

## Verification Criteria

- Calibration writes valid `Calibration` stats and `threshold="auto"`
  resolves to a finite, sensible operating point (between genuine and impostor
  means).
- "Test my voice" shows a genuine/impostor histogram, a threshold marker, a
  self-EER number, and a measured per-embed latency.
- The incremental curve is monotone-ish upward on a clean enrollment and
  flat/declining on a bad one (silence/wrong-speaker), demonstrating it
  reflects reality.
- Short clips (≥ 1 s, < 3 s) enroll successfully and contribute to a rising
  profile-strength indicator; < 1 s still rejected.
- Each enrolled utterance carries a language tag; a deliberate cross-lingual
  test raises the mismatch warning.
- With cloud STT selected, a test asserts no embedding/biometric bytes appear
  in the outbound STT request, and the decision still attaches locally.
- All new scoring/calibration math has unit tests; full workspace
  fmt/clippy/test gate + `./tests/check.sh --size-budget` stay green; zero new
  crates (draw charts with inline SVG).

## Potential Risks and Mitigations

1. **Cohort sidecar not hosted → AS-Norm degraded, `auto` under-determined.**
   Mitigation: bootstrap calibration from other enrolled speakers as local
   impostors; gate the "official" self-EER claim on the hosted cohort; show a
   doctor warning while degraded.
2. **Small-N self-EER is noisy and could mislead.** Mitigation: present as a
   smoothed trend with an explicit caveat; never label it a benchmark; keep
   the rigorous EER in the offline oracle (Slice 5.1).
3. **Users over-trust the gate as authentication.** Mitigation: prominent
   in-UI "convenience gate, not a lock" explainer; enforce the asymmetric
   fail-safe/fail-deadly gating + second factor for irreversible actions.
4. **Bad enrollment audio (silence/clipping/noise) poisons the centroid.**
   Mitigation: capture-time VU + quality warnings, post-enroll self-match
   confirmation, per-utterance delete/re-record.
5. **Cross-lingual / cross-channel drift lowers accuracy silently.**
   Mitigation: language + capture-source tags, coverage chips, mismatch
   warnings, multi-language/-channel enrollment prompts.
6. **Accidental biometric leakage to cloud in cloud-STT mode.** Mitigation:
   architectural separation + an explicit regression test on the outbound
   request; privacy-doc statement.

## Alternative Approaches

1. **Calibration data source**: (a) held-out fresh clips (chosen — cleanest,
   no leakage) vs (b) reuse enrollment clips via leave-one-out (cheaper, no
   extra recording, but optimistic). Could offer LOO as a "quick estimate"
   and fresh clips as the "accurate" mode.
2. **Threshold policy**: EER-crossover (balanced, chosen default) vs a
   target-false-accept operating point for strict deployments (offer both;
   strict deployments pin a manual float per the base plan).
3. **Charts**: inline SVG hand-drawn (chosen — zero deps, matches size
   discipline) vs a charting library (rejected — new dependency, binary-size
   cost).
4. **Language handling**: tag-and-warn (chosen — cheap, honest) vs
   per-language enrollment sets / per-language calibration (heavier; revisit
   only if mismatch proves costly in Slice-5 measurements).
