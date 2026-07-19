# Speaker Verification — Calibration, Enrollment Quality & UX (Consolidated)

## Objective

Sequence the next phase of Fono's on-device speaker verification toward three
goals, in priority order: **great UI/UX**, **lower real-world EER**, and
**minimal complexity** (build nothing we don't need). Supersedes
`2026-07-19-speaker-verification-calibration-ux-v1.md`; slots into
`2026-07-17-speaker-verification-v1.md` Slices 4–5. Design only — no code here.

## Grounding (verified in-tree)

- Store is calibration-ready: `Calibration { genuine_mean, genuine_std,
  trials }` + `set_calibration()`, per-utterance `capture_source`,
  `remove_utterance()` (`crates/fono-core/src/speakers.rs:35-40,233-323`).
- Scoring complete + model-free: `cosine`, `Cohort::as_norm`, `decide`,
  `confidence_from_margin`, `SpeechAccumulator.sufficient_audio`
  (`crates/fono-audio/src/speaker.rs:47-315`).
- Engine embeds raw 16 kHz PCM, dim 192, tens of ms/utterance.
- **Language detection is once-per-utterance, not continuous:** local Whisper
  runs an encoder-only `lang_detect` over the ~30 s prefix, caches the pick per
  segment, re-detects only at a segment boundary
  (`crates/fono-stt/src/whisper_local.rs:508-558,627`); cloud STT returns one
  detected language per response. This aligns naturally with the one
  speaker-decision-per-utterance cadence.
- **No diarization exists** — STT emits a single transcript with no
  speaker segmentation.
- Browser enrollment lands (record → review → Submit/Discard), DSP disabled +
  device picker; only embeddings persist, never audio.
- **Blocking dependency:** the impostor-cohort sidecar is not hosted, so
  `Cohort::as_norm` degrades to plain cosine and `threshold="auto"` is
  under-determined (base plan Task 1.3).

---

## Recommended build sequence

Ordered by leverage on the three goals with dependencies respected. Steps 1 and
2 are independent and can proceed in parallel.

### Step 1 — Impostor-cohort sidecar (keystone; unblocks real EER + calibration)
The single highest-leverage technical item. Turns **AS-Norm on** (the
research-cited ~30% relative EER win, currently dormant), lets
**`threshold="auto"` resolve**, and makes **"test my voice" meaningful** — most
users enroll only themselves (N=1), so there are no other-speaker impostors to
bootstrap from; the shipped cohort is the only impostor distribution available.
Same generate → pin → host pipeline already run twice for the model graphs
(embed the held-out speakers through the ReDimNet2 graph locally, pin the
small `.cohort.bin`, host on the mirror). **Not overcomplicated.**

**Decision (2026-07-19): universal multilingual cohort from Mozilla Common
Voice (CC0), one cohort per model — no per-language cohorts.** Rationale:
cohort embeddings are model-specific (must come from OUR `.ort` graphs, so
nothing precomputed is reusable); the embedding is timbre-based and language
is second-order; AS-Norm's top-k selection self-specialises per voice *if* the
pool is bigger than top-k. Parameters:

- **Source:** Common Voice per-language tarballs; speakers selected from each
  `validated.tsv` by `client_id` (≥ 3 validated clips per speaker). CC0
  provenance retires Risk 1 outright; we host only derived vectors, never
  audio.
- **Composition:** ~500–600 speakers, stratified for the expected user base —
  ~100–150 Romanian (`ro`) + a solid `en` block + the remainder spread over
  major languages (`de`, `fr`, `es`, `it`, …). Cohort **must exceed**
  `DEFAULT_AS_NORM_TOP_K` (200, unchanged) so the adaptive top-k selection is
  meaningful; at ~200 it degenerates to a fixed pool.
- **Reusability (regeneration for future models is a requirement):** the
  speaker/clip **selection manifest** (language, `client_id`, clip filenames,
  Common Voice release version) is committed as a small checked-in file, and
  the generation tool is **model-agnostic** (takes the `.ort` graph path as an
  argument). Regenerating for a new tier or a future model family = re-run the
  same tool over the same pinned selection with a different graph; no
  re-curation. Follows the `scripts/gen-*` pattern.

- [x] Task 1.1. Cohort source decided — see the decision block above (was
      Open Decision 1).
- [x] Task 1.2. Done 2026-07-19: `scripts/gen-speaker-cohort.py` (select +
      generate), pinned manifest at `calibration/speaker-cohort/selection.tsv`
      (600 speakers: ro=130 en=150 de/fr/es/it=80; cv-corpus-17.0 dev+test via
      the ungated `fsicoli/common_voice_17_0` HF mirror). Generated cohorts
      (600×192, 460,808 B each):
      `redimnet2-b3.cohort.bin` sha256
      `3562c1c29c0e11ddbb4c72d392e104aeab1192e650bdd12d16214af3f52bc091`,
      `redimnet2-b6.cohort.bin` sha256
      `ee2877a2f56ad7d04f9e6b9011f89cd3bca0d11889f407d8175aecc55b8f6d5c`
      (artefacts in `tmp/cv-cohort/out/`, awaiting Task 1.3 hosting).
- [x] Task 1.3. Hosted on the `ort-1.24.2` mirror release (2026-07-19); sha256
      + size pinned in the registry rows (`speaker.rs`) and `manifest.json`
      (cohort rows flipped from UNPINNED → hosted).
- [x] Task 1.4. Verified: hosted bytes at the exact fetch URLs hash-match the
      pins, registry/loader tests updated and green, cohorts installed into
      the local model cache so AS-Norm is active; empty-cohort fallback
      (raw-score path) unchanged and still covered by tests.

### Step 2 — Capture-quality UX (biggest UX + EER-per-effort; zero blockers)
Attacks the #1 real-world EER killer — bad enrollment audio — entirely in the
browser + existing engine, no cohort dependency. Also the "great UX"
centrepiece.

- [ ] Task 2.1. Live input meter (VU) during capture + heuristic warnings:
      too-quiet, clipping, noisy-room.
- [ ] Task 2.2. Compute intrinsic capture-time quality metrics client-side and
      send with the enroll POST (see "Per-utterance quality metrics").
- [ ] Task 2.3. Post-enroll self-match check: score the just-recorded clip vs
      the growing centroid, show "✓ this sample matches your profile" (catches
      silence / wrong-speaker / dead-mic before it poisons the centroid).
- [ ] Task 2.4. Profile-strength indicator (see its section) with a
      most-limiting-factor nudge that pushes toward the voice test.

### Step 3 — "Test my voice" calibration card (needs Step 1)
Where EER tuning on the user's own room/mic and user confidence materialise.

- [ ] Task 3.1. Calibration math in `fono-audio` (pure, tested): genuine +
      impostor score arrays → EER estimate, EER-threshold, target-FAR
      threshold, per-embed latency stats.
- [ ] Task 3.2. Daemon hook + `POST /api/speakers/{id}/calibrate`: record
      held-out clips, embed, score genuine vs own centroid and impostor vs
      cohort (+ other enrolled speakers when present), `set_calibration`.
- [ ] Task 3.3. `#/speakers` calibration card: inline-SVG genuine/impostor
      histogram + threshold marker + verdict badge + "use recommended
      threshold" (writes `[speaker].threshold`); no new JS deps.
- [ ] Task 3.4. Resolve `threshold="auto"` from `Calibration` + cohort at
      decision time, with a documented uncalibrated fallback.
- [ ] Task 3.5. Utterance-pruning UI (see prune flow) — suggested, confirmable.
- [ ] Task 3.6. `fono speaker test [name]` CLI parity (distributions, self-EER,
      latency, threshold verdict).

### Step 4 — Slice 4 pipeline wiring (make it live)
Until this lands, verification only runs in enrollment/testing.

- [ ] Task 4.1. Run local embedding **concurrently** with the STT call (local
      or cloud) via `tokio::join!` so embed latency hides behind the round-trip;
      join before tagging.
- [ ] Task 4.2. Tag transcripts + history with the decision (name + score,
      never the embedding).
- [ ] Task 4.3. Assert (test + docs) the embedding/biometric never enters any
      cloud request path; attach cloud-detected language to the decision.

### Explicit non-goals (avoid overcomplication)
- **Language tagging** — demoted to diagnostic-only; build the coverage/warning
  UX or drop the column (see Language section). Not on the critical path.
- **Diarization / multi-speaker** beyond safe-blend-reject (see Multi-speaker).
- **Second-factor PIN, QMF calibration, incremental 1→3→5 curve** — all
  deferred; none move the three goals enough to justify the complexity now.

---

## Per-utterance quality metrics (part of Step 2)

**Capture-now-or-never:** audio is discarded and only the embedding is kept, so
intrinsic audio-quality metrics can **never be recomputed** — they must be
captured at enroll time or lost forever. This makes persisting them a *now*
decision even though the prune UI ships in Step 3.

**Store (intrinsic, never changes for a clip)** — nullable columns on
`speaker_utterances`, computed client-side during capture, sent with the enroll
POST (mirrors the `api_keys.rs` migration pattern):
```
speaker_utterances( … existing … ,
    duration_secs  REAL,   -- clip length
    loudness_dbfs  REAL,   -- RMS level; too-quiet / clipping
    snr_db         REAL )  -- background-noise estimate
```

**Do NOT store (relational, goes stale on any add/remove)** — the
embedding-consistency / outlier score (cosine of each utterance to the centroid
of the others). It is the **strongest** "weak one" signal (a clip can have
great SNR yet be an outlier — cold, different mic, background speaker), but it
must be **computed on demand** from the stored embeddings (cheap 192-dim
cosines). Persisting it would create drift bugs.

Rule: **intrinsic facts persisted (recompute-impossible); relational facts
derived (recompute-trivial).**

- [ ] Task Q.1. Migration: add the three nullable metric columns.
- [ ] Task Q.2. Client computes duration/loudness/SNR at capture; enroll POST
      carries them; store writes them.
- [ ] Task Q.3. On-demand consistency score helper over the stored embeddings.

### Suggested prune flow (Step 3 UI)
Realises "keep 4 good clips totalling ~15 s instead of 4 good + 2 bad":
- Rank utterances by combined score = intrinsic quality + on-demand
  consistency.
- Propose removing the weakest **while preserving a coverage floor**: keep
  ≥ ~15 s total, ≥ 3–4 clips, and never drop the only clip on a given
  device/language.
- Present as a confirmable suggestion ("Remove 2 weak samples? Profile stays
  strong: 4 clips, 16 s") — **never** silently auto-delete biometric data;
  `remove_utterance()` does the mechanics.

---

## Multi-speaker stance (v1)

No diarization exists and none is planned for v1. Design:
- **Assume one speaker per turn** — valid for push-to-talk dictation (user
  holds the hotkey and talks).
- **A mixed-speaker turn self-defeats safely** — two voices embed into one
  blended vector that lands between speakers, scores below threshold, and is
  rejected as "unknown": the correct fail-safe (composes with the base-plan
  rule that voice never authorises irreversible actions).
- **Optional cheap robustness check (add-on, not core):** embed the first half
  vs second half of the utterance; low cross-cosine ⇒ flag "multiple/ambiguous
  speakers — not gating." No new model, two extra embeds.
- **Full diarization is out of scope** — document as a future slice.

## Language handling (demoted to diagnostic-only)

The embedding is timbre-based and language-agnostic; a language tag never
changes scoring or the centroid. Its only value is diagnostic — coverage
("enrolled English only, dictating Romanian → enroll a Romanian clip") and
failure explanation (cross-lingual shift is real but second-order, smaller than
mic/channel mismatch and audio quality). Detection is already once-per-utterance
and free from STT.

**Decision:** implement the tag **only if** we build the coverage/warning UX;
otherwise store nothing. If built: a nullable `language` column on
`speaker_utterances`, populated from the STT-detected code, aggregated per
speaker for coverage chips + a cross-lingual mismatch warning + a
multi-language enrollment prompt when `general.languages` has > 1 entry. Do
**not** fork models or keep per-language centroids.

## Profile-strength indicator

A heuristic bucket (weak / ok / strong) surfacing the single most-limiting
factor as the nudge:
- # utterances (diminishing after ~5), total enrolled seconds (~15–30 s
  target), channel diversity (# `capture_source` devices), language coverage,
  and — weighted highest once available — the measured self-separation /
  self-EER from the voice test.

**Honesty rule that shapes the widget:** the first signals are only *proxies*
(five clips of clipped/silent/wrong-speaker audio can still look "strong"). The
**only** true quality measurement is the self-test, so pre-calibration the badge
is a proxy that prominently pushes the user to run the voice test;
post-calibration it is dominated by the measured self-separation.

---

## Verification Criteria

- With the cohort hosted, AS-Norm is active end-to-end and `threshold="auto"`
  resolves to a finite operating point between genuine and impostor means; the
  empty-cohort fallback still degrades cleanly.
- Enrollment persists duration/loudness/SNR per utterance; a deliberately bad
  clip (silence/clipping) is flagged at capture and the post-enroll self-match
  reports a low score.
- The on-demand consistency score identifies an injected outlier clip; the
  suggested prune removes it while respecting the coverage floor and requires
  confirmation.
- "Test my voice" shows a genuine/impostor histogram, threshold marker,
  self-EER, and measured per-embed latency; "use recommended threshold" writes
  config.
- With cloud STT selected, a test asserts no embedding bytes appear in the
  outbound STT request and the decision still attaches locally.
- All new scoring/calibration/metric math has unit tests; full workspace
  fmt/clippy/test + `./tests/check.sh --size-budget` stay green; zero new
  crates (charts via inline SVG).

## Potential Risks and Mitigations

1. **Cohort source licensing** blocks hosting. Retired: Common Voice is CC0
   (see the Step 1 decision block); we ship only derived vectors, and record
   source provenance in the manifest as with the model graphs.
2. **Quality metrics uncaptured are lost forever** (audio discarded).
   Mitigation: persist intrinsic metrics in Step 2 even before the prune UI.
3. **Small-N self-EER is noisy.** Mitigation: present as a smoothed trend with
   an explicit caveat; keep the rigorous EER in the offline oracle (Slice 5.1).
4. **Users over-trust the gate as authentication.** Mitigation: in-UI
   "convenience gate, not a lock" explainer; asymmetric fail-safe/fail-deadly
   gating; second factor for irreversible actions (deferred UI).
5. **Accidental biometric leakage in cloud-STT mode.** Mitigation:
   architectural separation + outbound-request regression test + privacy doc.
6. **Stored consistency score drift** if persisted. Mitigation: derive it on
   demand; never store it.

## Alternative Approaches

1. **Quality-metric storage**: explicit nullable columns (chosen — matches
   `api_keys.rs` style, queryable) vs a single JSON `metrics` blob (flexible
   but opaque to SQL).
2. **Calibration data source**: fresh held-out clips (chosen — no leakage) vs
   leave-one-out over enrollment clips (cheaper, optimistic) — could offer LOO
   as a "quick estimate."
3. **Threshold policy**: EER-crossover default vs target-FAR for strict
   deployments (offer both; strict deployments pin a manual float).
4. **Charts**: hand-drawn inline SVG (chosen — zero deps, size discipline) vs a
   charting library (rejected — binary-size cost).

## Open Decisions (need sign-off)

1. ~~**Cohort source speaker set**~~ — **RESOLVED 2026-07-19**: Common Voice
   (CC0), ~500–600 speakers stratified with a Romanian block, ≥ 3 clips per
   speaker, pinned selection manifest + model-agnostic generation tool for
   future-model regeneration. Full parameters in the Step 1 decision block.
   (VoxCeleb2 rejected on provenance friction and near-zero Romanian; a
   VoxCeleb-style interview slice may be mixed in later only if the Slice 5
   oracle shows channel mis-calibration.)
2. **Whether to build the language coverage/warning UX at all** — if not, drop
   the language column entirely (default: defer, do not store).
