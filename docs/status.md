# Fono — Project Status
Last updated: 2026-07-21

## 2026-07-21 — Release 0.17.1 (natural multilingual voice by default)

Cut the 0.17.1 release. Headline: the on-device TTS default flips from the
cross-engine `auto` router to **Supertonic** — one compact multilingual pack
(Romanian included) that stays comfortably real-time on low-end hardware — plus
two new Settings controls for the built-in voice (Speed slider, optional
extra-passes quality toggle). Piper and Kokoro remain explicit opt-ins. Also
folded in: the `stt.raw` diagnostic log now prints the pre-vocabulary-correction
transcript. (See the two 2026-07-21 sessions below for the implementation.)

Release chores this session:
- Bumped workspace version `0.17.0 → 0.17.1` (`Cargo.toml` + `Cargo.lock`,
  19 crates).
- Added the `CHANGELOG.md` `[0.17.1]` section (Changed / Added / Fixed) and the
  compare-link ref.
- Updated `ROADMAP.md`: new Recently-shipped banner + Shipped entry, and moved
  the "Natural local voices in 31 languages" item out of **Up next** (the
  Supertonic default fulfils it) — removed from both the summary table and the
  section body.
- The three feature commits since v0.17.0 (Supertonic default, TTS benchmark,
  `stt.raw` log) were left intact; the release doc/version changes were squashed
  into the last commit per the maintainer's instruction.

Pre-tag gates run: fmt, clippy `-D warnings`, workspace tests, and the
size-budget gate. Committed (not pushed) with an annotated `v0.17.1` tag ready.

## 2026-07-21 — Make Supertonic the default local TTS engine; drop the auto router

Flipped the on-device TTS default from the cross-engine `auto` router to
**Supertonic**, and removed `auto` as a selectable engine. Piper and Kokoro
stay available as explicit opt-in pins (`[tts.local].engine = "piper" |
"kokoro"`), still routing per-language within their own catalog. Backed by the
three-engine benchmark (see the prior TTS-benchmark session + `fono-bench tts`
/ `tts-score`): Supertonic is at least as intelligible as Kokoro on English
(STT-scored CER ≈ half), strong on Romanian, and comfortably real-time even on
a 2016 dual-core i7-7500U (RTF ≈ 0.45 vs Kokoro's unusable ≈ 2.1), from one
shared ~140 MiB multilingual pack — so the flip *improves* the low-end story
over the old Kokoro English default.

Changes: `TtsLocalEngine` drops the `Auto` variant and defaults to
`Supertonic` (`config.rs`); the `auto` token is removed entirely (no config /
route alias); `is_auto` → `is_supertonic`. Factory + ensure paths already handled Supertonic
(built directly, pack ensured). `local_tts_pending_mb` now sizes the Supertonic
pack by presence (~140 MiB) instead of mis-reporting catalog voices. Settings
UI: engine picker drops the Auto card, defaults to "Supertonic (recommended)"
(`daemon.rs` meta + `app.js`). ADR 0033 amended. Gates green (fmt, clippy
`-D warnings`, tests).

## 2026-07-21 — Log the raw STT text before personal-vocabulary correction

Small observability fix. The `stt.raw` pipeline log line
(`crates/fono/src/session.rs`) printed `raw` *after* it had been reassigned to
the vocabulary-corrected transcript, so the original speech-to-text output was
never visible — only the corrected text, which also appears later on the
injected-text line. Captured the pre-correction transcript (`raw_pre_vocab`) and
changed the log line to print it, appending ` (vocabulary → …)` only when a
correction actually fired. Debug-level, off the audio hot path (clone/format
only when the line is emitted).

Docs: added a CHANGELOG `[Unreleased] / Fixed` entry, added a status note to the
ROADMAP personal-vocabulary item, and dropped the `fono vocabulary suggest` and
voice "fix that" items from that roadmap entry (not planned). Gate green (fmt,
clippy `-D warnings`, tests).

## 2026-07-21 — Retire the pointer-hover context-injection roadmap item

Roadmap-only change. Removed the "Hover-context injection" item from
`ROADMAP.md` (the "On the horizon" list and the top summary table). Rationale:
the focused-window half already shipped in v0.8.2 and is the right signal for
dictation — text lands in the focused window, so context should key off focus.
The remaining pointer-under-mouse variant serves only the niche "dictate into A
while pointing at B" case, off a noisy signal, and would cost per-platform
pointer→window mapping (X11/Wayland/macOS/Windows) and binary weight against a
size-first budget. Considered done as-is; not pursuing the pointer variant.

## 2026-07-20 — Release 0.17.0 prepared (speaker recognition)

Cut the 0.17.0 release. Headline: **on-device speaker recognition** — the full
enrol → "test my voice" → live-tagging slice landed across the 31 commits since
v0.16.0 (Speakers settings page, `fono speaker` CLI incl. `speaker test`,
sample review/cleanup, auto-threshold calibration, live + assistant tagging,
biometric-leak regression test, `docs/speakers.md`). Also shipped: LAN access
to Fono's STT/TTS gated by inbound API keys with per-key usage tracking;
settings-page improvements (local voice-engine picker + tester, inline system
health, network options, rebuilt activity overlay); and a public-surface
repositioning to "a complete voice-AI stack in one small binary".

Release chores this session:
- Bumped workspace version `0.16.0 → 0.17.0` (`Cargo.toml` + `Cargo.lock`, 21
  crates).
- Added the `CHANGELOG.md` `[0.17.0]` section and the compare-link ref.
- Updated `ROADMAP.md` (new Recently-shipped banner + Shipped entry).
- CI/release fixes folded into this release: container GHCR login moved to
  `docker/login-action@v4` (kills the Node 20 deprecation warning); release
  artefacts now ship a single consolidated `SHA256SUMS` instead of a per-asset
  `.sha256` sidecar, with `fono-update` verifying against the matching row and
  still accepting legacy sidecars (Task 2.6 of the TTS-stack plan).

Pre-tag gates run: fmt, clippy `-D warnings`, workspace tests, and the
size-budget gate. Committed (not pushed) with an annotated tag ready.

## 2026-07-19 — Speaker verification: live-path + assistant wiring (Step 4 follow-up)

Closed the two Slice-4 gaps flagged in the previous entry: streaming live
dictation and the assistant turn are now speaker-tagged too, so verification
fires for every capture path, not just the batch record-then-transcribe one.

- **Streaming live dictation.** `LiveSession` (`crates/fono/src/live.rs`) gains
  an opt-in `with_collect_pcm(bool)`: when verification is enabled the translator
  task accumulates exactly the voiced PCM it forwards to STT (capped at 60 s) and
  returns it on `LiveTranscript::voiced_pcm`. Zero allocation when verification is
  off. `on_stop_live_dictation` (`session.rs`) embeds that buffer and tags the
  history row — previously always `speaker: None` on the streaming path.
- **Assistant turn.** The same shared pipeline builder enables collection for the
  assistant path, so the streamed F8 turn resolves the speaker from its voiced
  PCM; the batch (record-then-send) assistant turn verifies its whole-utterance
  buffer. When matched, the verified name is prepended to the assistant's system
  prompt for that turn ("The current speaker is <name>.") so the assistant knows
  who it's addressing (Option 1 — context injection, no gating).
- **Logging.** The verified name is merged into the existing single-line
  summaries — ` | speaker <name>` on both the `pipeline (live):` dictation line
  and the `assistant:` line — no extra log line.

Gates green: fmt, clippy workspace (`-D warnings`), full workspace tests
(all suites pass, incl. a new `assistant:`-summary speaker-rendering test).
Builds verified with and without the `speaker-onnx` / `interactive` features.

## 2026-07-19 — Speaker verification: Slice 4 pipeline wiring (Step 4, complete)

Verification now runs on real dictation, not just enrollment/testing. Step 4 is
done:

- **Task 4.1 — concurrent embed + decision.** `run_pipeline` (`session.rs`),
  when `config.speaker.enabled` is on, runs the voice embedding + AS-Norm
  decision **concurrently** with the STT call via `tokio::join!`, so the embed
  latency hides behind the transcription round-trip. The process-lived
  `SpeakerEngine` is cached behind a new `SpeakerVerify` handle (`daemon.rs`),
  so the ONNX model + cohort load **once** on first use — not per utterance.
  Threshold resolves through `SpeakerThreshold::Fixed(f)` / `Auto =>
  resolve_auto_threshold(...)`. A broken/missing model degrades to un-tagged
  dictation rather than blocking the pipeline.
- **Task 4.2 — tag transcripts + history.** History carries a nullable
  `speaker` column (`fono-core/history.rs`, additive migration). Only the
  matched **name** is stored; the score/threshold are logged, and the embedding
  never touches history. Tagged on the batch path today; the live push-to-talk
  path (which doesn't retain the whole-utterance buffer) accumulates PCM in a
  follow-up.
- **Task 4.3 — biometric-leak regression test + privacy doc.**
  `pipeline_speaker_verification_never_leaks_audio_or_embedding_to_stt`
  (`crates/fono/tests/pipeline.rs`) drives a full `run_oneshot` with
  verification on and a recording STT, asserting the STT payload is
  byte-for-byte the same dictation PCM (no appended/mixed embedding) and the
  `TranscribeOptions` carry nothing biometric. `docs/privacy.md` gains a "voice
  embeddings never leave the machine" bullet.

Gates green: fmt, clippy workspace, workspace tests (all suites pass, incl. the
new pipeline test). Next: Slice 5 (Python-oracle EER cross-check, deterministic
E2E, ADR/docs) — the verification/validation slice.

## 2026-07-19 — Speaker verification: `fono speaker test` CLI (Step 3, Task 3.6)

Added terminal parity for the web "test my voice" card, closing out Step 3:

- `fono speaker test <id> <wav>...` — loads one or more held-out WAV clips
  (16-bit PCM; non-16 kHz files are resampled through the shared `rubato`
  wrapper to 16 kHz mono), then calls the very same `run_calibration` the
  `/api/speakers/{id}/calibrate` endpoint uses, so terminal results and the
  persisted calibration are identical to the web page.
- Prints the genuine/impostor score distributions, cohort size, self error-rate
  (EER) with its recommended sensitivity, the strict target-FAR threshold,
  per-embed latency (mean/p50/p95), and a plain-language verdict. The resulting
  calibration is saved so `threshold = "auto"` can use it.
- `run_calibration` is now `pub(crate)` (both the `speaker-onnx` and the
  compiled-out fallback definitions); `speaker_cmd` became `async` and the
  dispatch site awaits it. Reuses the existing `read_wav_mono_f32` loader.

Gates green: fmt, clippy workspace, workspace tests, size-budget 21.88 MiB / 25.
Step 3 (calibration + UX) is complete. Next: Slice 4 (live pipeline wiring) —
where `resolve_auto_threshold` + `decide` finally run on real dictation.

## 2026-07-19 — Speaker verification: sample-manager + prune UI (Step 3, Task 3.5)

Added per-utterance sample management with a suggested, confirmable prune, so a
profile can be cleaned of weak clips without dropping below a safe coverage
floor:

- `fono-core::suggest_prune(utterances, consistency)` — pure and unit-tested.
  Ranks clips weakest-first and proposes removing only genuinely weak ones (low
  peer-consistency, low SNR, too quiet, clipping, or too short) while preserving
  a coverage floor: `PRUNE_MIN_CLIPS` clips, `PRUNE_MIN_SECS` seconds, and at
  least one clip per capture source. Never proposes dropping good audio to hit a
  target; the result is advisory.
- `GET /api/speakers/{id}/utterances` — lists each clip with its capture-time
  quality metrics (duration, loudness, SNR) and on-demand consistency score,
  plus the prune suggestion and the floor. Never carries embeddings over the
  wire. `DELETE /api/speakers/{id}/utterances/{uid}` removes one clip, refusing
  the speaker's last remaining one (delete the speaker instead).
- A "Manage samples" card on `#/speakers` (gear button per roster row) lists the
  clips, tints the weak ones, and offers per-clip removal or a one-click "remove
  N weak samples" that accepts the whole suggestion — each confirmable.

Two `web_settings_hooks` helpers (`doctor_hook`, and the existing utterance
builders) were extracted to keep clippy's line budget.

Gates green: fmt, clippy workspace, workspace tests (incl. 5 prune + 2 route
round-trip), size-budget 21.88 MiB / 25.
Next: Task 3.6 (`fono speaker test` CLI parity), then Slice 4 wiring.

## 2026-07-19 — Speaker verification: auto-threshold resolver (Step 3, Task 3.4)

Added the model-independent logic that turns `threshold = "auto"` into a
concrete decision cutoff, ready for the Slice 4 live path to call:

- `fono-audio::resolve_auto_threshold(calibration, impostor, target_far)` — pure
  and fully unit-tested, taking primitives so the scoring layer stays free of
  config coupling. Four cases, most-informed first: (1) **calibration + cohort**
  → the std-weighted midpoint between the user's genuine mean and the cohort
  impostor mean (approximating the two-Gaussian equal-density boundary, pulled
  toward the tighter cluster), floored at the `target_far` operating point so
  impostors stay out even on overlap; (2) **calibration only** → the genuine
  lower tail `mean − 2σ`; (3) **cohort only** → the `target_far` point;
  (4) **neither** → `DEFAULT_UNCALIBRATED_THRESHOLD` (a documented conservative
  fallback that only bites on a cold, uncalibrated, cohort-less install).
- New constants `GENUINE_MARGIN_STDS` and `DEFAULT_UNCALIBRATED_THRESHOLD`,
  re-exported from `fono-audio`.

The live invocation (match `SpeakerThreshold::Fixed(f) => f`, `Auto =>
resolve_auto_threshold(...)`) is deliberately deferred to Slice 4, which is the
"make it live" step; this task delivers the single resolution point it calls.

Gates green: fmt, clippy workspace, workspace tests, size-budget 21.88 MiB / 25.
Next: Task 3.5 (prune UI), 3.6 (`fono speaker test` CLI), then Slice 4 wiring.

## 2026-07-19 — Speaker verification: calibration card (Step 3, Task 3.3)

Gave the "test my voice" path a face on the `#/speakers` page — the calibrate
backend (Task 3.2) is now driven from the UI:

- A self-contained calibration recorder (its own mic picker + live level meter,
  reusing the generalised enrollment meter helpers) captures held-out clips into
  `spkCalClips`, separate from enrollment. Record two or more, then Run test.
- Run test POSTs the clips to `/api/speakers/{id}/calibrate` and renders the
  response: an inline-SVG genuine-vs-impostor histogram (no chart library) with
  a vertical marker at the recommended threshold, the self-EER, measured
  per-embed latency, a plain-language verdict, and a "use recommended threshold"
  button that writes `[speaker].threshold`.
- Calibration state (`spkCalResult`) is preserved across section re-renders so
  the histogram survives list refreshes; Clear resets it.

Gates green: fmt, clippy workspace, workspace tests, size-budget 21.88 MiB / 25.
Next: Task 3.4 (`threshold="auto"` resolution from the persisted `Calibration`),
3.5 (prune UI), 3.6 (`fono speaker test` CLI).

## 2026-07-19 — Speaker verification: calibrate endpoint (Step 3, Task 3.2)

Wired the "test my voice" calibration path end-to-end (still headless — the
`#/speakers` card is Task 3.3):

- `fono-audio`: `Cohort::impostor_scores` (score every cohort member against a
  centroid, for the impostor distribution) and `centroid` (L2-normalised mean of
  enrolled voice prints, the canonical profile point reused by Slice 4). Both
  unit-tested.
- `fono-net`: `CalibrateSpeakerFn` hook + `POST /api/speakers/{id}/calibrate`
  route (ordered before the generic PATCH/DELETE arms), plus round-trip tests
  proving the route reaches the hook (422 on stub reject, 400 on a bad id).
- `fono` daemon: `calibrate_speaker_hook` + `run_calibration` — decodes held-out
  16 kHz clips, fetches/loads the model + cohort, embeds off the accept loop
  (timing each embed), builds the target centroid, scores genuine
  vs-own-centroid and impostor vs-cohort (plus other enrolled speakers as extra
  local impostors via `other_speaker_centroids`), runs `calibrate`, persists the
  genuine distribution via `set_calibration`, and returns the score
  distributions + EER + recommended thresholds + latency stats. No audio
  persisted. Feature-gated on `speaker-onnx` with a clean fallback.

Gates green: fmt, clippy workspace, workspace tests, size-budget 21.87 MiB / 25.
Next: Task 3.3 — the `#/speakers` calibration card (inline-SVG histogram +
verdict + "use recommended threshold"), then 3.4 (`threshold="auto"`
resolution), 3.5 (prune UI), 3.6 (`fono speaker test` CLI).

## 2026-07-19 — Speaker verification: calibration math (Step 3, Task 3.1)

Started Step 3 ("test my voice") of
`plans/2026-07-19-speaker-verification-calibration-ux-v3.md` with its
model-independent foundation — the calibration math in `fono-audio`, pure and
fully unit-tested:

- `score_mean_std` — population mean/std for reporting (no std floor, unlike the
  private AS-Norm helper), `(0,0)` on empty.
- `eer_and_threshold` — sweeps every observed score as a candidate threshold and
  returns the equal-error-rate estimate + its threshold (accept when `score >=
  t`); neutral `(0.5, 0.0)` when either side is empty.
- `threshold_for_far` — the strict operating point: lowest threshold whose
  impostor false-accept rate is `<= target_far` (the `1 − target_far` quantile).
- `calibrate` — assembles a full `CalibrationReport` (both distributions, EER,
  EER-threshold, and a `DEFAULT_TARGET_FAR` = 1 % strict point).
- `latency_stats` / `LatencyStats` — nearest-rank p50/p95 + mean for per-embed
  latency reporting ("≈X ms on this machine").

Re-exported from `fono-audio`. 6 new unit tests (37 speaker tests total). Gates
green (fmt, clippy workspace, workspace tests). Next: Task 3.2 — the daemon
calibrate hook + `POST /api/speakers/{id}/calibrate`, then the `#/speakers`
calibration card.

## 2026-07-19 — Speaker verification: impostor cohort shipped (Step 1 complete)

Completed Step 1 of `plans/2026-07-19-speaker-verification-calibration-ux-v2.md`
end to end — AS-Norm score normalisation is now live:

1. **Open Decision 1 resolved.** Universal multilingual cohort from Mozilla
   Common Voice (CC0) — no per-language cohorts; the adaptive top-k selection
   plus the Step 3 per-user calibration covers language-heavy deployments
   (e.g. Romanian) without bespoke machinery.
2. **Pinned selection manifest** (`calibration/speaker-cohort/selection.tsv`):
   600 speakers (ro=130, en=150, de/fr/es/it=80), ≥3 validated clips each,
   deterministic seed, anonymised speaker hashes, full provenance header.
   Sourced from the ungated `fsicoli/common_voice_17_0` HF mirror — no manual
   download gate after all.
3. **Reusable generation tool** (`scripts/gen-speaker-cohort.py`): `select` +
   `generate` subcommands, model-agnostic (`.ort` path is an argument), so
   future models rebuild the pack with one command from the pinned manifest.
4. **Cohorts generated, hosted, pinned.** `redimnet2-{b3,b6}.cohort.bin`
   (600×192, 460,808 B each) uploaded to the `ort-1.24.2` mirror release;
   registry rows in `speaker.rs` and `../fono-voice/manifest.json` flipped
   from UNPINNED → hosted; pending-cohort tests updated. Hosted bytes verified
   hash-identical to the pins at the exact fetch URLs.

Gates green (fmt, clippy, workspace tests). Next: Step 3 — the "test my
voice" calibration card, now unblocked by the live cohort.

## 2026-07-19 — Speaker verification: capture-quality UX (Step 2)

Built the capture-quality half of the calibration-UX plan
(`plans/2026-07-19-speaker-verification-calibration-ux-v3.md` Step 2) — all
model-independent, gates green. It attacks the #1 real-world EER killer: bad
enrollment audio.

1. **Per-utterance quality metrics persisted (Tasks Q.1–Q.2).** Added three
   nullable columns to `speaker_utterances` (`duration_secs`, `loudness_dbfs`,
   `snr_db`) via an idempotent `column_exists`-guarded `ALTER TABLE` migration.
   The browser computes these intrinsic metrics once on the resampled 16 kHz
   clip and sends them with the enroll POST; the store writes them through a new
   `add_utterance_with_quality` (the old `add_utterance` delegates with
   defaults). Rationale: the audio is dropped after embedding, so these are
   recompute-impossible — capture now or never.
2. **On-demand consistency helper (Task Q.3).** `consistency_scores` in
   `fono-audio` scores each utterance against the centroid of the others —
   the "which clip is the weak one" signal, computed on demand (never stored,
   since it goes stale when the set changes).
3. **Live VU meter + warnings (Task 2.1).** The enroll card shows a running
   input meter during capture (RMS→bar, clip tint on peak≥0.99) and, on stop,
   plain-language warnings for too-quiet / clipping / noisy-room clips.
4. **Post-enroll self-match (Task 2.3).** The daemon scores the just-recorded
   clip against the profile-so-far (prior utterances only) and returns
   `self_match`; the UI shows "✓ this sample matches" or flags an odd capture.
   `None` on the first sample.
5. **Profile-strength indicator (Task 2.4).** A strength badge (weak/ok/strong)
   in the roster from count × seconds × device-diversity, with a
   most-limiting-factor nudge. Honest by design: these are only proxies until
   the voice test (Step 3) gives ground-truth self-EER. `list_speakers` now
   projects `total_secs` + `source_count` aggregates.

Refactored the enroll closure into `enroll_speaker_hook` to stay within the
line budget. Gates green: fmt, `clippy --workspace`, workspace tests, and the
size-budget gate (21.83 MiB / 25 MiB). Step 1 (cohort) and Step 3 (calibration
card) remain — Step 1 is being done in a separate session.

**Not committed** — holding for your review.

## 2026-07-19 — Speaker verification: browser enrollment (Task 3.2) + speaker-onnx default

Wired the speaker engine into the shipped binary and built end-to-end voice
enrollment from the web settings page:

1. **`speaker-onnx` is now a default `fono` feature.** Forwarded
   `fono-audio/speaker-onnx` and added it to the default + ship sets, so
   `SpeakerEngine` compiles into every release build. `ort` was already linked
   via `wakeword-onnx`, so the binary only gained the extra kernels — size
   holds at 21.82 MiB (budget 25 MiB).
2. **`POST /api/speakers` enrollment.** New async `enroll_speaker` hook: decodes
   base64 i16 PCM, fetches/loads the configured model on demand, embeds off the
   accept loop via `spawn_blocking`, and appends the voice print to
   `SpeakerStore` (create-or-append by name). Only the derived embedding is
   persisted — raw audio never touches disk. A `cohort.bin` sidecar loader is in
   place for when the AS-Norm cohort is hosted (degrades to plain cosine until
   then). Feature-gated with a clean "not compiled in" fallback.
3. **Browser capture UI on the Speakers section.** Enrollment card with a name
   field + microphone device picker + record/stop button: captures via
   `getUserMedia` with browser DSP disabled (no AEC/NS/AGC, mono), resamples to
   16 kHz with `OfflineAudioContext`, and uploads base64 i16 PCM. Name persists
   across re-renders; roster refreshes after each enrollment.
4. **`base64`** added as a `fono` dependency edge (already in the graph via
   `fono-assistant` — net-zero on binary size).

Gates green: fmt, `clippy --workspace` (default now includes speaker-onnx),
workspace tests (incl. the `POST /api/speakers` round-trip), the no-feature
fallback build, and the size-budget gate. Plan Task 3.2 updated (browser
enrollment done; guided wizard + calibration card remain for Slice 5).

**Not committed** — holding for your review.

## 2026-07-19 — Speaker verification: Slice 1 model hosting (runtime rebuild + upload)

Completed the mirror-hosting half of Slice 1 so the ReDimNet2 speaker models
load end-to-end:

1. **Runtime rebuilt + re-pinned.** The `fono-voice` `build-onnxruntime`
   workflow rebuilt the minimal runtime from the unioned `ops.config` (adding
   `InstanceNormalization`, `ReduceProd`, `FastGelu`) and republished all five
   triples under `onnxruntime-1.24.2`. Re-pinned every row of
   `scripts/fetch-onnxruntime.sh` from the published `sha-<triple>.txt` and
   flipped its header note PENDING→DONE.
2. **Model graphs hosted + pinned.** Uploaded `redimnet2-b3.ort` /
   `redimnet2-b6.ort` to the `ort-1.24.2` release; verified the hosted bytes
   are byte-identical to the local conversion (sha `7bb3…e0e2` / `9030…f087`).
   Pinned both graph SHAs in the `fono-audio::speaker` registry and flipped the
   guard test (`graphs_are_hosted_cohorts_still_pending`). The manifest
   `speaker_models[]` statuses are now `hosted`.
3. **Size gate re-verified.** `./tests/check.sh --size-budget` passes at 21.79
   MiB (budget 25 MiB) — the larger op-set added no measurable binary delta,
   and the build fetching through the re-pinned SHA confirms the pins are
   correct end-to-end.

Still pending on the model side: the AS-Norm impostor-cohort sidecar (Slice
4/5, so cohort rows stay UNPINNED and AS-Norm degrades to plain cosine), and
the Slice 5 Python-oracle EER cross-check. Plan Slice 1 Tasks 1.2/1.4 ticked,
1.1/1.3 partial. Gates green (fmt, clippy, workspace tests, size budget).

**Committed but NOT pushed** — holding for your review of the commits.

## 2026-07-19 — Speaker verification: decision layer (Task 2.5)

Landed the model-independent decision layer in `fono-audio::speaker`, the last
Slice 2 piece that needs no hosted model:

1. **`SpeechAccumulator`.** Accumulates 16 kHz mono PCM across the wake phrase
   and the following command until `min_speech_secs` of audio is gathered
   (`SAMPLE_RATE`-based), reporting `seconds()` / `is_sufficient()` so short
   commands keep accumulating until enough voice backs a decision.
2. **`decide()` → `SpeakerDecision { name, score, confidence,
   sufficient_audio }`.** Scores a centred/normalised test embedding against
   every `EnrolledSpeaker` centroid with AS-Norm, picks the best, and only
   names a match when the score clears the (config) threshold. `confidence` is
   a logistic of the score's margin over the threshold (0.5 at the threshold,
   monotone in the score); the winning score/confidence are reported even on a
   reject, for logging/calibration.

Pure arithmetic — 7 new unit tests (accumulator seconds/sufficiency/clear,
zero-minimum, empty-candidate reject, genuine-match-vs-reject, threshold
confidence). Re-exported from `fono-audio`. Gates green: fmt, `clippy
--workspace` (and `--features speaker-onnx`), full `cargo test --workspace`.
Plan Task 2.5 ticked.

**Slice 2 is now complete bar Task 2.3's oracle fixtures (Slice 5).** Next
model-independent work is Slice 4 wiring (tagging transcripts / history with
the decision). The audio-dependent verbs and Slice 1 mirror hosting +
runtime rebuild remain as previously noted.

## 2026-07-17 — Speaker verification: web + CLI management surface (Tasks 3.2–3.3, model-independent half)

Built the model-independent half of the speaker web/CLI surface on top of the
Slice 2/3.1 foundation. Everything below compiles and tests in the default
gate; enrollment-from-audio, browser capture, and calibration remain blocked
on the Slice 1 model pack and are clearly deferred.

1. **`/api/speakers/*` (Task 3.2 backend).** New `route_speakers` on the
   settings server — `GET /api/speakers` (list), `PATCH /api/speakers/{id}`
   (rename), `DELETE /api/speakers/{id}` (remove) — behind the same
   loopback-trust + API-key rules as the rest of `/api/*`. Three new
   `WebSettingsHooks` closures (`list_speakers`/`rename_speaker`/
   `delete_speaker`) wired in the daemon over `SpeakerStore` (reopened per
   call, like the config/vocabulary hooks). The list projects `SpeakerView`
   down to metadata only — voice-print embeddings never cross the wire.
2. **Speakers settings section (Task 3.2 frontend).** New `Speakers (voice
   ID)` section in the SPA: a settings card (enable toggle, model picker,
   `"auto"`-or-float threshold, min-speech seconds) bound to the `[speaker]`
   config, plus a roster table (name / utterances / calibrated / updated)
   with rename + delete actions. Enrollment/calibration are flagged in-UI as
   arriving with the model pack, pointing users at the CLI meanwhile. This
   binds every `speaker.*` config key, satisfying the config-coverage guard.
3. **`fono speaker` CLI + doctor (Task 3.3).** `fono speaker
   list|rename|remove` over `SpeakerStore`, and a `fono doctor` Speaker
   section: enabled state, registry-model check (loud on an unknown
   `[speaker].model`), threshold source, enrolled count, and a warning when
   enabled with zero enrolled speakers.

Gates green: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, full `cargo test --workspace --tests --lib`
(incl. a new `speakers_list_and_mutations_round_trip` HTTP test).

**Next / blocked:** the audio-dependent verbs — WAV/mic enrollment, the
browser capture flow (DSP-off + device picker), calibration ("test my
voice"), and `fono speaker enroll|test|identify` — all need **Slice 1**
(hosting the ReDimNet-B6 `.ort` pack + impostor cohort, the minimal-runtime
rebuild) and the Slice 5 Python-oracle validation. Those need network/mirror
resources and sign-off. Nothing is committed yet.

## 2026-07-17 — Speaker verification: model-independent foundation (Slice 2 core + 3.1)

Started `plans/2026-07-17-speaker-verification-v1.md`. Landed the parts that
need no hosted model, no ONNX runtime, and no network — so they compile and
test in the default workspace gate. Adopted the plan's default open-question
resolutions: engine will be a `fono-audio` module behind a future
`speaker-onnx` feature; the store lives in `fono-core` beside `api_keys.rs`.
Confirmed **zero new crates** (`ort` already in the graph via `fono-audio`,
`rusqlite` via `fono-core`).

1. **Back-end scoring (Task 2.3, `crates/fono-audio/src/speaker.rs`).**
   Pure-Rust `l2_normalize`, `cosine`, and a `Cohort` (impostor cohort) with
   mean-centering and **AS-Norm** score normalisation. 10 unit tests incl. a
   genuine-vs-impostor separation check. Also holds the **model registry**
   (Slice 3.1): `redimnet-b6` default row with its fbank config (16 kHz,
   80-mel, 25/10 ms) and 256-dim embedding; `registry()` / `model(name)`.
2. **Speaker store (Task 2.4, `crates/fono-core/src/speakers.rs`).** New
   `SpeakerStore` over a dedicated `speakers.sqlite` (mode `0600`, WAL,
   owner-clamp per the `api_keys.rs` pattern): `speakers` +
   `speaker_utterances(embedding BLOB, capture_source, …)` with cascade
   delete, calibration stats, and LE-`f32` embedding (de)serialisation.
   13 unit tests. New `Paths::speakers_db()`.
3. **Config (Slice 3.1, `crates/fono-core/src/config.rs`).** `[speaker]`
   block (`enabled`/`model`/`threshold`/`min_speech_secs`), off by default and
   skipped from serialization when default. `threshold` is a `"auto"`-or-float
   union with a custom serde visitor. 5 round-trip tests.
4. **Fbank front-end + ONNX engine (Tasks 2.1–2.2, `speaker.rs`).** A
   log-mel `Fbank` (povey window, 0.97 pre-emphasis, HTK mel triangles,
   per-utterance CMN) built on `realfft` — already in the graph via `rubato`,
   so **net-zero** on binary size and always-compiled/testable (4 tests incl.
   a tone-localisation check). A feature-gated `speaker-onnx` `engine`
   (`SpeakerEngine`) wraps an `ort` session mirroring the wakeword build
   idiom: fbank → session → centred + length-normalised embedding. Compiles
   clean under `--features speaker-onnx`. Exact numerical parity with the
   Python oracle is deferred to Slice 5.

Gates green: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings` (and with `--features speaker-onnx`), full
`cargo test --workspace --tests --lib`.

**Next / blocked:** the engine's *runtime validation* still needs **Slice 1**
— hosting the ReDimNet-B6 `.ort` pack + impostor cohort on the voice mirror,
the `ops.config` union / minimal-runtime rebuild, and the Python-oracle
cross-check (Slice 5) — which needs network/mirror resources and sign-off.
Also still open on model-independent surface: `/api/speakers/*` + `#/speakers`
web page and the `fono speaker` CLI (Tasks 3.2–3.3). Nothing is committed yet.

## 2026-07-17 — Inbound API-key authentication with bounded usage

Replaced the single pre-shared bearer token on the served HTTP surfaces
(the OpenAI/Ollama LLM API, its STT `/v1/audio/transcriptions` + TTS
`/v1/audio/speech` routes, and the web settings page) with a proper
multi-key store, a simple on/off auth toggle, and per-key usage
tracking that never grows into an access log. Implements
`plans/2026-07-17-inbound-api-key-auth-and-usage-v1.md` (all 5 phases)
and ADR 0038.

1. **Key store (`crates/fono-core/src/api_keys.rs`).** New
   `ApiKeyStore` over a dedicated `api_keys.sqlite` (mode `0600`):
   create/list/rename/set-expiry/revoke/delete, SHA-256 hash at rest
   (plaintext shown once), constant-time verify that rejects
   revoked/expired keys. Usage is stored as **bounded per-day/per-month
   counters** (UPSERT) plus a debounced `last_used_at`, with `prune()`
   trimming old buckets — DB size scales with key count, not request
   volume. 13 unit tests incl. a bounded-growth proof over 400 days.
2. **Config: one boolean, on by default.** `[server.llm].auth` and
   `[server.web].auth` (`default_true`) replace `auth_token_ref`; the
   legacy ref is migrated into a named key on first load, then cleared.
3. **Shared enforcement.** `fono_net::auth::decide(...)` is the single
   testable auth seam used by both servers: auth off ⇒ open; loopback
   always trusted (no bootstrap lockout); otherwise a bearer token must
   resolve via an injected verifier, with a usage sink recording the
   hit. 6 unit tests cover every branch.
4. **Web UI "API Keys" section.** Groq-style table (name, masked
   secret, created, last-used, expires with warning styling, monthly
   usage) with create-once secret reveal + rename/revoke/delete; the
   old "Token ref" server fields became "Require API key" toggles.
5. **CLI + doctor.** `fono server keys {create|list|rename|expire|
   revoke|delete}` (distinct from outbound `fono keys`); `fono doctor`
   reports per-server auth state, active/inactive key counts, and warns
   loudly on LAN-exposed-with-auth-off or on-with-no-keys.
6. **Docs.** `docs/configuration.md` rewritten for the toggle + API Keys
   table + usage + migration; new ADR 0038.

Gates green: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, full workspace tests (all suites pass,
incl. new `auth`, `api_keys`, and updated llm/web round-trip tests).

## 2026-07-17 — Assistant usable without TTS (on-screen reply panel) — GitHub #15

Anthropic-only (and any STT + LLM, no-TTS) users could not use the
voice assistant at all: the staged turn hard-required a TTS backend
and otherwise bailed with a "backend missing" notification. Since
`tts.backend = none` is the default, that was a silent dead end for a
large slice of users. Fixed by showing the reply as an on-screen text
panel when no TTS is configured.

1. **TTS is now optional.** `AssistantTurnInputs.tts` became
   `Option<...>` and the staged-turn guard requires only the assistant
   (`crates/fono/src/assistant.rs`, `crates/fono/src/session.rs`).
2. **Text-only reply pump.** `drive_text_only_reply` streams the reply
   into the overlay, records tool events + reply into history, then
   holds the panel for a deliberately slow reading-time dwell (~130 wpm,
   3 s floor, 60 s cap — no config knob; Escape / barge-in ends it
   early).
3. **Reading overlay state.** New `OverlayState::AssistantReading`
   (teal, "REPLY" label). The panel grows to fit as the reply streams
   and tail-follows the newest line with a smooth, frame-rate-
   independent pixel scroll (sub-line clipping, not row jumps).
4. **Zero idle CPU.** The Wayland loop blocks unless a scroll is
   actively catching up (`wants_animation_frame`); a settled/short reply
   costs no CPU. Fixed the earlier 25 %-CPU-while-visible regression.
5. **Second-turn sizing.** Fixed the reply panel re-opening at the
   previous turn's tall height: the X11 backend now resizes on every
   `SetState` (it previously only resized on text/style changes, unlike
   Wayland), and the renderer clears stale reply text on entering the
   reading state.
6. **Reporting + docs.** `fono doctor` reports no-TTS as an
   informational line (not a warning); `docs/providers.md` documents
   text-only mode. `fono test-overlay` gained a reading-panel demo.

Gates green: `fmt`, `clippy --workspace -D warnings` (+ overlay
`--all-features`), full workspace tests and overlay `--all-features`
(63 lib tests, incl. dwell bounds, tail-follow easing, overflow, and
the second-turn resize regression). Plan:
`plans/2026-07-16-assistant-without-tts-v1.md`.

## 2026-07-16 — Glas Cortex: tray label, cloud MoE sim, brighter listening

Three follow-up polish items from user feedback:

1. **Tray label.** The Glass Cortex waveform style was the only one
   with no parenthetical description in the tray menu. Added
   "Glass Cortex (live AI thinking)" (`crates/fono-tray/src/menu.rs`).
   The web settings picker already lists it (`app.js:108`, committed
   at `77e79e8`) — no change needed there.
2. **Traceless (cloud) fallback = simulated MoE.** A *local* assistant
   turn on a cloud backend produces no `brain_tap` keyframes, so the
   bar used to sit idle through the whole reply. After a short grace
   window (`SIM_GRACE = 0.7 s`, so grounded local turns that publish
   `ReplyBegin`/`Prefill` first are never affected) it now drives a
   simulated sparse expert-lane sweep that drifts across depth and
   time — the bar reads as an active routing network. This is the one
   path *not* grounded in real data; it engages only when no trace
   exists and never for network requests (which don't move the overlay
   into a busy phase). `Prefill` now also marks the reply active so a
   grounded turn suppresses the sim.
3. **Brighter listening.** The mic equalizer gain went from 0.85 to
   1.25 so the Listening scene reads with presence instead of a dim
   shimmer.

Gates green: `fmt`, `clippy -D warnings` (default + `backend-x11`),
full workspace tests (55 cortex lib tests incl. the new
`traceless_backend_simulates_after_grace_but_not_before`). Gallery
gains a `6b_traceless_moe_sim` scene.

## 2026-07-16 — Glas Cortex: transparent panel (black → transparent)

User feedback: the overlay's black pixels should be transparent, and
the status label ("ASSISTANT", "PONDERING", …) had an opaque scrim
behind it that should be transparent too. Made the Cortex style a
true transparent-panel visualisation:

1. **No stage backing.** `draw_cortex` dropped the near-black
   `0x000A0A12 @ 0.92` slab that covered the whole strip — unlit tiles
   now show the desktop through (`crates/fono-overlay/src/cortex.rs`).
2. **Brightness-keyed tile opacity.** Cells (settled field + sweep
   heads) render via `blend_rect` with `cell_alpha(v)` instead of the
   old opaque write, so the near-black ramp bottoms fade to
   transparent rather than painting solid black squares; bright tiles
   still reach full opacity and read as crisp LEDs. `fill_rect_opaque`
   retired.
3. **No charcoal panel for Cortex.** `redraw` skips the opaque
   `COLOR_BG` rounded-panel fill for the full-panel Cortex style
   (`crates/fono-overlay/src/renderer.rs`); the accent stripe stays.
4. **No label scrim.** `draw_status_label` dropped the `darken_rect`
   backing; legibility now comes from the soft drop shadow alone.
   `darken_rect` retired.

Gallery updated to start from a transparent buffer (matching the real
renderer) so the dark+bright composites reflect the floating-LED look.
All gates green (fmt, clippy default + `backend-x11`, 54 cortex tests,
full workspace tests).

## 2026-07-16 — Glas Cortex: fix keyframe starvation (real variety)

Live feedback: still "not a lot of variety." The daemon trace was
decisive — a 75-token reply captured only **2 real keyframes** (tokens
1 and 31), *fewer* than the 8 we got at the old 2% budget. So the
budget was never the effective lever; the sampling governor was
starving capture. Two root causes in `crates/fono-core/src/brain_tap.rs`:

1. **Expensive per-sample stats.** `logits_stats` computed confidence +
   entropy over the model's *entire* vocabulary (~256k logits for
   gemma) with **two** separate `exp()` passes in `f64` — several ms per
   sample, a big share of the measured surcharge. Fused into a single
   `exp()` pass (identical result, ~2× cheaper), so the governor allows
   more keyframes within the same budget.
2. **First-sample poisoning → starvation feedback.** The first sampled
   token carries one-time warmup cost (graph reservation, cold caches);
   it seeded the cost EMA far above steady state, the interval
   ballooned, and because we then sampled rarely the EMA never
   recovered — it stayed wide for the whole reply (explaining 3% giving
   *fewer* anchors than 2%). The governor now (a) excludes the first
   sampled token from the cost model, and (b) applies a hard
   `MAX_INTERVAL = 10` cap so capture density has a guaranteed floor
   (≈1 anchor / 10 tokens ⇒ ~7–8 per this reply).

The cap deliberately makes the overhead budget *soft* on genuinely slow
hardware (capture density is prioritised) — signed off by the user. The
fused stats keep the real per-sample cost modest, so in practice the
overhead stays well-behaved.

Tests: `governor_widens_interval_to_hold_budget` replaced by
`governor_caps_interval_and_ignores_warmup_sample` (warmup ignored +
cap enforced); `logits_stats` correctness tests unchanged and still
green. Full workspace gate green (234 fono-core tests).

## 2026-07-16 — Glas Cortex: steady pacing, flowing churn, denser capture

Three tuning changes after live feedback that the Speaking bar felt
"a bit better" but still lurched and looked like slow breathing in the
middle/tail of a long reply. The daemon trace was decisive: the reply
is generated *and* spoken in overlapping chunks, so `audio_total`
climbs 4.8 → 24.5s across four sentences and `total_tokens` (94) isn't
known until ~4s into playback; capture was also front-loaded (6 anchors
in the first 20 tokens, then 2 across the next 74).

1. **Steady pacing (renderer).** The Speaking morph is now driven by a
   *monotonic* playback cursor (`play_pos`, token space) in
   `crates/fono-overlay/src/cortex.rs`. Each beat it advances toward the
   best-known total at a velocity of "remaining tokens / remaining
   audio" (floored + capped), and it never moves backward. Because the
   audio length and token count climb mid-playback, the old raw-ratio
   position lurched forward then snapped back; the cursor now only ever
   eases its speed, so the bar reads as smooth progress start to finish.

2. **Flowing churn (renderer).** Between the sparse real anchors the
   equalizer used to crossfade uniformly (slow breathing). The
   per-column shimmer is now a deeper two-octave pattern that
   *translates* each beat, so long anchor gaps read as active compute.
   Brightness only — it never changes which columns/layers are lit, so
   the grounded shape is untouched.

3. **Denser capture (`brain_tap`).** `OVERHEAD_BUDGET` 2% → **3%** so the
   governor lets ~1.5× more real anchors through, keeping the sparse
   tail of a long reply populated. Still imperceptible (a 2 s reply →
   ~2.06 s) and still a hard governor-enforced ceiling.

Two new regression tests: `speaking_cursor_is_monotonic_when_audio_grows_midplayback`
(the exact growing-audio field scenario — cursor must never retreat and
must reach the reply length) and the existing morph/evolve tests still
hold. All 54 cortex tests + full workspace gate green.

## 2026-07-16 — Glas Cortex: continuous morph during speech

Field feedback: even with denser capture the Speaking bar still looked
dull — a couple of pulses then a frozen shape. The daemon trace
confirmed the cause was not the beat but the *content*: between the
sparse real anchors the renderer redrew the same held snapshot, so
every ~3/s sweep looked identical.

Fixed in `crates/fono-overlay/src/cortex.rs` by making the Speaking
equalizer **morph continuously through the real captures**:

- `beat_speaking` is now position-based: playback position in token
  space is derived from the real audio duration + real token count, and
  every beat launches a sweep whose dense shape is the *time-interpolated*
  frame at that position (`morph_norms_at`). The bar slides smoothly
  between anchors instead of snapping and freezing.
- Anchors we pass are still merged (routing / confidence / log-norm
  band) but no longer each fire their own sweep — the morph pulse
  carries the visible shape, so the show stays one continuous evolving
  equalizer for the whole utterance.
- `launch_decode_pulse` split into a `_with_norms` variant so the same
  sweep machinery renders either the merged held state (Thinking) or
  the interpolated frame (Speaking); MoE still reads revealed lanes.
- `spatial_fill` extracted and shared by `filled_held` + `morph_norms_at`.

Two new regression tests: `morph_norms_interpolates_in_time_between_anchors`
(temporal interpolation math) and `speaking_equalizer_shape_evolves_across_the_reply`
(the field profile must actually differ early vs late in playback).
All 53 cortex tests + full workspace gate green.

## 2026-07-16 — Glas Cortex: denser capture + local-only tap

Two follow-ups after the grounded-playback fix.

**1. Capture a bit more real data (fuller, evolving bar).** The daemon
trace showed the sample governor widening its interval so far on CPU
that a whole reply produced ~1 keyframe. Retuned the tap in
`crates/fono-core/src/brain_tap.rs` so the show is grounded in more
real snapshots at a still-imperceptible cost:

- `OVERHEAD_BUDGET` 1% → **2%** — the tap is purely observational, so a
  couple of percent (a 2 s reply → ~2.04 s) is unnoticeable while
  roughly doubling the keyframe rate.
- `LAYER_STRIDE` 4 → **8** — each keyframe observes an eighth of the
  stack instead of a quarter, halving the per-sample graph-scheduler
  cost so the budget buys ~2× more frames; the renderer already
  interpolates the unobserved layers and the phase rotates to cover the
  full stack over successive frames.
- `DEFAULT_BASE_INTERVAL` 3 → **2** — a finer floor so fast machines
  capture a denser stream (the governor remains the real throttle on
  slow ones).

Net: ~4× more grounded keyframes per reply, so the bar's *shape* now
evolves across the utterance instead of replaying one snapshot.

**2. Capture only on local turns, never over the network.** The
OpenAI/Ollama-compatible LLM server shares the same embedded assistant
`Arc`, so a remote client's `/v1/chat/completions` request was arming
the tap and lighting *this* computer's overlay. Fixed with an explicit
opt-in:

- New `AssistantContext::allow_brain_capture` (default `false`); the
  local F8 hotkey paths set it `true`, the LLM server's `make_context`
  leaves it `false` (network turns stay dark).
- The embedded backend gained a per-turn `capture_gate` (an atomic set
  under the model lock, so it can't race a concurrent turn, with an RAII
  guard that closes it on every exit). `tap()` returns the tap only when
  capture is enabled **and** the gate is open, so prewarm/diagnostic
  decodes stay dark too.
- Regression tests: `network_context_never_allows_brain_capture`
  (fono-net) and `capture_gate_guard_closes_on_drop` /
  `tap_stays_dark_until_the_gate_opens` (fono-assistant).

## 2026-07-16 — Glas Cortex: grounded, continuous playback pacing

Follow-up to the rewrite after live feedback that the bar animated in
"0–2 sparse bursts" and then went stale during speaking. The daemon
`debug` trace was decisive: a 27-token reply captured **one**
keyframe (`trace=1`) — the sample governor widened its interval to
hold the <1% decode budget on CPU — so the old queue-based replay had
essentially one data point and everything else was identical carry
sweeps of a sparse (strided) frame.

Fix (`crates/fono-overlay/src/cortex.rs`), keeping the ~3 pulses/s
metronome the user preferred but grounding it in the two signals we
always capture reliably — the real **token count** and real **audio
duration**:

- **Speaking is paced by real token count, not keyframe count.** The
  retained trace is revealed in `token_index` order, a keyframe
  captured at token K appearing K/`total_tokens` of the way through
  the reply audio; between real keyframes the beat carries the
  last-known state so a sweep fires on *every* beat for the whole
  utterance. `total_tokens` now comes from `ReplyEnd`.
- **Sparse strided layers are interpolated** (`filled_held`) so a
  single-frame reply renders as a full, readable equalizer instead of
  a dozen-column flicker — linear fill between real samples, flat at
  the ends, no fabricated structure.
- **Subtle per-token texture** on decode sweeps so repeated carries
  never look frozen (brightness micro-variation only; never changes
  which columns are grounded-on).
- Two new regression tests lock it: continuous animation across a
  27-token / 1-keyframe / 9.37 s reply, and the interpolation math.

## 2026-07-16 — Glas Cortex rewritten as the 6×46 LED grid

The Cortex waveform style ("watch it think") was rewritten from
scratch against the 2026-07 design prototype
(plan: `plans/2026-07-15-glas-cortex-rewrite-v1.md`, Tasks 1–14 done;
Task 15 speech-synced clock deferred pending separate sign-off):

- **New visual grammar.** A fixed **6×46 LED grid** (never resized to
  the model's layer count; `layer(col)` maps depth onto columns) with
  two fixed color ramps — cool indigo→cyan for intake (idle,
  listening, prefill) and the warm Fono ember for compute (decode).
  Pulses sweep left→right as crisp stepped cells (`1.0/0.66/0.42`
  tiers, no blur/bloom); dense models render a center-out equalizer
  per column, MoE models light expert lanes (`lane = id % 6`, budget
  adapted to the real routing ratio, co-activity gating,
  lane-collision bumping). Confidence brightens the pulse
  (`0.5 + 0.5·token_prob`); high entropy desaturates toward grey.
- **Grounded-replay clock.** Reworked after live feedback into a
  steady **metronome at ~3 pulses/s** (the web demo's human-relatable
  pace) that never stops while a reply is live or its audio plays:
  each beat fires the next real keyframe when due — consumed once, in
  `token_index` order, live during Thinking, spread across the reply
  audio during Speaking (the full trace is retained and reloaded at
  playback start) — otherwise a slightly dimmer *carry* sweep
  re-showing the last-known real state; waiting on the first token
  shows a slow cool scan. Nothing looped, nothing fabricated, but the
  rhythm is continuous through thinking → synthesising → speaking.
  Cortex lifecycle events log at `debug` (frames at `trace`) for
  live diagnosis.
- **Never dead.** Field decays `exp(-dt/0.30)`; a dim (~0.17)
  breathing resting field of last-known norms/routing covers real
  capture gaps; idle shows a slow drifting breath. Listening restyles
  the mic FFT onto the same grid grammar (cool equalizer).
- **Engine contract.** `BrainEvent::ReplyBegin` now carries
  `kind` (dense/moe) and optional `n_experts_total`/`n_experts_active`
  read from llama.cpp model metadata; mirrored through
  `CortexCmd::ReplyBegin` and `brain_trace_dump` JSON.
- **Retired.** ~1,800 lines of the old renderer (chart recorder,
  beads/sparks, constellation, MoE HUD, entropy skyline, `GlowAccum`
  bloom) plus the `brain_mockups` example. The gallery
  (`cortex_gallery`) was rebuilt for the new scenes with a
  deterministic clock (instant, no sleeps) and dark+bright desktop
  composites; new unit tests cover the grid bounds, layer mapping,
  play-once/ordering/clamping, never-black, MoE lane determinism and
  entropy desaturation.
- **Gates.** fmt / clippy / workspace tests / size budget all green
  (binary 21.57 MiB of the 25 MiB budget).

## 2026-07-15 — Local TTS engine picker + OpenAI-compatible `/v1/audio/speech`

The settings UI's Voice section grew a proper local-engine experience,
and Fono gained a standard speech-synthesis HTTP endpoint
(plan: `plans/2026-07-15-web-settings-local-tts-ux-v5.md`):

- **`tts.local.engine` config field** (`auto`/`piper`/`kokoro`/`supertonic`,
  default `auto` = today's language-aware routing). `auto`/`piper`/`kokoro`
  route through the catalog router with an engine filter; `supertonic`
  builds the shared Supertonic pack directly (previously implemented but
  unreachable). The daemon ensures the Supertonic pack at startup when the
  engine is pinned.
- **Voice routing resolver.** `Tts::resolve_speech_route(model)` maps an
  OpenAI-`model`-shaped route selector to a per-request backend: empty =
  the configured `[tts]` backend; `piper`/`kokoro`/`supertonic`/`local` =
  on-device; every cloud provider id (`gemini`, `elevenlabs`, …) selects
  that backend; a `provider/model` suffix (split on the first slash, so
  `openrouter/openai/tts-1` works) overrides the cloud model. Unit-tested.
- **`POST /v1/audio/speech`** (OpenAI Audio API shape) on the settings
  server, token-gated, returning `audio/wav` (default) or raw `pcm`. One
  daemon `SpeechFn` hook loads config + secrets fresh per request, resolves
  the route, builds the engine off the accept loop, synthesizes, and
  encodes via the new shared `fono_core::wav` helpers (promoted from
  `fono-stt` to avoid a `fono-net → fono-stt` edge). Works for local and
  cloud backends alike through the existing TTS factory.
- **Settings UI.** Local backend now shows an engine card row + a
  per-engine preset-voice dropdown (from a new `/api/meta` `tts_local`
  block), plus an inline "type a sentence, hear it" tester that plays the
  WAV through the **Web Audio API** — so preview works even when the
  daemon runs on a remote box. Cloud + Network segments got the tester
  too. `/api/meta` also exposes a `tts_cloud` block (per-provider key
  presence + voice palette; key values never leave the daemon).
- **Audio surface on the LLM server.** The OpenAI-compatible audio
  endpoints are now mounted on the LLM server (port 11434) too, not just
  the settings server: `POST /v1/audio/speech` reuses the same `SpeechFn`
  hook (shared routing/synthesis), and a new `POST /v1/audio/transcriptions`
  accepts a `multipart/form-data` upload, decodes the WAV in-process (new
  `fono_core::wav::decode_wav`, no audio-decode dependency), and drives the
  resolved STT backend. The transcription `model` field selects the backend
  per request (`groq`, `openai`, `openrouter/…`, `local`, …), else the
  configured `[stt]` backend. Both routes 404 cleanly when no audio hook is
  supplied. Multipart parsing is a tiny hand-rolled splitter — no new crate.
- **Cloud reach.** Both audio endpoints reach cloud providers through the
  existing TTS/STT factories with keys resolved from `secrets.toml`/env, so
  the gateway synthesizes/transcribes through the configured cloud when a
  key exists. The verbatim proxy fast-lane (native mp3/opus passthrough via
  a parallel audio-upstream) remains a deferred optimization; the adapter
  path already delivers cloud audio within the wav/pcm formats the endpoint
  encodes.
- **Gates.** fmt / clippy (default + `tts-local`) / workspace tests / size
  budget all green.

## 2026-07-15 — Web doctor: health icon + `#/doctor` page in the settings UI

`fono doctor` is now reachable from the browser settings page
(plan: `plans/2026-07-15-web-doctor-integration-v1.md`, all tasks done):

- **Structured doctor model.** `doctor::report()` was decomposed into
  `gather() -> DoctorReport` (typed `Severity` / `DoctorCheck` /
  `DoctorSection`, serde-serializable, aggregate = worst check) plus the
  text output built in the same pass — the CLI report is unchanged.
- **`GET /api/doctor`.** New token-gated route on the web settings
  server; the daemon hook runs `gather()` on a blocking task with an
  in-flight mutex (dedup, not cache — every call is a fresh run).
- **Hash router + shell.** The SPA now has two views sharing the header
  / toast / theme / token plumbing: `#/settings` (default, the existing
  editor untouched) and `#/doctor`. Hash routing preserves `?token=…`.
  This is the foundation for further planned pages.
- **Header icons.** The "Theme" text button became a `◐` glyph button,
  joined by a three-state health icon (green ✓ / yellow ⚠ / red ✕,
  CSS-colored text glyphs, tooltips + aria-labels) that links to the
  doctor page. One report fetch on page load sets it; no polling.
- **Doctor view.** Accordion sections with per-check severity dots,
  Warn/Fail sections auto-opened, last-run timestamp, "Re-run checks".
- **IPC `Request::Doctor` implemented.** The long-standing "not yet
  available" stub now returns the color-free rendered report from the
  same gather path.
- **Tests + gates.** New `web_settings_round_trip.rs` (401/200 token
  gate, report JSON shape, open assets), doctor model unit tests;
  fmt/clippy/test and the size-budget gate all green (21.44 MiB of
  25 MiB budget).

## 2026-07-15 — v0.16.0 Windows release build fixed end to end; ready to retag

The `v0.16.0` release run kept failing on `x86_64-pc-windows-msvc` while
CI stayed green (the CI `windows` job builds a cached `target\debug`;
the release job builds `release-slim` from scratch). Five distinct
clean-build failures were peeled off one by one — all are now fixed and
squashed into a single commit; docs/build-windows.md's failure log
(items #4–#8) has the full detail on each:

- **C1083 compiler probe** — the Linux-only `CFLAGS`/`CXXFLAGS` size
  flags from `.cargo/config.toml` `[env]` reached MSVC `cl` (cargo
  `[env]` is not target-scoped). Blanked on Windows in CI, release, and
  `scripts/win-remote.sh`.
- **`VCEnd`/`MSB8066` on the `vulkan-shaders-gen` install step** — the
  known Visual Studio-generator + nested-ExternalProject bug on clean
  builds (first mis-attributed to a poisoned rust-cache; the cache bump
  stays, harmless). Fixed by forcing the single-config Ninja generator
  (`CMAKE_GENERATOR=Ninja` + `ilammy/msvc-dev-cmd` for cl/INCLUDE/LIB).
- **Wrong linker under bash** — Git-for-Windows' `/usr/bin/link.exe`
  shadowed MSVC's in the release build step. The impostor is removed
  before building.
- **`C1041` PDB path over MAX_PATH** — the deeply nested
  `vulkan-shaders-gen` try-compile exceeded 260 chars under
  `target\…`. Fixed with a short `CARGO_TARGET_DIR=D:\t`.
- **`RC2136` on `manifest.rc`** — same nested path, one tool later:
  rc.exe chokes when the generated manifest reference is ~254 chars
  (known llama-cpp-python signature; ≤ ~246 passes). Fixed by dropping
  `--target x86_64-pc-windows-msvc` on Windows (the runner's host
  triple already is the release triple — identical artefact, 23 fewer
  chars in every nested path, ≈231 with margin). This also matches how
  the dev Windows host builds (`scripts/win-remote.sh` never passes
  `--target`), which is why the host never reproduced any of the
  path-length failures.

Also in the same commit: a Windows test fix (`agent_setup` assumed a
Linux home directory), and the CI + release Windows jobs now pin
`ORT_LIB_LOCATION` to the SHA-verified merged `onnxruntime.lib` from
the fono-voice mirror (`windows-defaults` includes `tts-local` /
`wakeword-onnx` since 0.16) instead of silently falling back to `ort`'s
unpinned CDN download — mirroring the dev host and every other release
row. Stale workflow/doc comments from the debugging spiral (ort-free
Windows-v1 claims, the poisoned-cache narrative) were rewritten to
match reality.

**Retag status.** Retag `v0.16.0` to the squashed head once the release
workflow's Windows row goes green end to end.

## 2026-07-14 — Supertonic Slice 3 DONE: runtime rebuilt, pack hosted, size measured

The remaining infrastructure landed — the Supertonic engine can now be
fetched, verified, and loaded end to end, and the binary-growth question
is answered with a real measurement.

- **Runtime rebuilt (all five triples).** Dispatched the `fono-voice`
  `build-onnxruntime` workflow (run `29347770258`) off the pushed
  `ops.config`; it rebuilt the minimal `libonnxruntime.a` /
  `onnxruntime.lib` for every triple and re-published them to the
  `onnxruntime-1.24.2` release. Re-pinned all five `raw_sha256` rows in
  `scripts/fetch-onnxruntime.sh`. The new lib is a strict superset of the
  wake-capable one (loads every existing voice + wake stack + Supertonic).
- **Pack hosted + graphs pinned.** Converted the four v3 int8 graphs to
  `.ort` and uploaded the seven-file pack (four graphs + `tts.json` +
  `voice.bin` + `unicode_indexer.bin`) to the `ort-1.24.2` mirror. Pinned
  the four graphs in `supertonic/mod.rs` from the *uploaded* bytes
  (conversion is not byte-reproducible, so pins come from the hosted
  artifact); `is_hosted()` is now true.
- **Measured binary growth — the headline number.** With the rebuilt
  runtime linked, the canonical `release-slim` `cpu` binary
  (x86_64-unknown-linux-gnu, default features) is **21.41 MiB
  (22,447,864 B)** — comfortably under the 25 MiB gate, `NEEDED` allowlist
  clean. Growth vs the ~21.64 MiB baseline is **negligible / within
  noise**: `--gc-sections` keeps the five light net-new kernels near-free.
  No budget bump needed. (Parakeet, if added later, folds into the same
  rebuild — its delta still needs its v3 model to measure.)

Slice 3 is complete. What's left for a user-visible voice: Slice 4
(catalog + router + config/UX wiring) and Slice 5 (deterministic E2E +
Python-oracle cross-check, now unblocked by the hosted pack + runtime).

## 2026-07-14 — Supertonic Slice 3 config done properly (real .ort conversion)

Redid the Slice 3 ops-config work the correct way — with the actual
conversion tooling instead of the earlier static hand-merge — and found
two things the hand-merge got wrong.

- Installed `onnxruntime==1.24.2` in a throwaway venv, converted the four
  Supertonic v3 int8 graphs to `.ort` with `scripts/gen-ort-models.sh`,
  and unioned the emitted type-reduced config into `fono-voice`'s
  `onnxruntime/ops.config` with `scripts/merge-ort-configs.py` (fono-voice
  commit `24fc906`, amended over the earlier hand-merge).
- **Correction 1 — a missed op that would break loading.** The graph
  optimizer introduces `com.microsoft;QLinearConv` when it fuses the int8
  Conv layers; it is invisible in the raw `.onnx`, so the hand-merge
  omitted it — a runtime would have failed to load Supertonic. The
  authoritative net-new set is `Erf`(13), `BatchNormalization`(15),
  `PRelu`(16), `QLinearConv` (contrib), plus `int64_t` widenings on
  `Clip`/`Div`/`Pow`. The hand-merge's `Constant`/`Tile`/`Reciprocal`
  guesses were folded away by the optimizer.
- **Correction 2 — graph pins are not reproducible.** `.ort` conversion
  is **not byte-deterministic**: two identical runs produce different
  bytes (different SHA-256) for all four graphs. So the four graph pins in
  `supertonic/mod.rs` must be taken from the *uploaded* artifacts, not a
  reproduced conversion — they correctly stay `UNPINNED`. The emitted
  `ops.config` **is** stable across runs (it's a semantic property), so
  the committed config is safe. Module docs updated to record this.

Slice 3's config half is now genuinely done and verified. Remaining is
pure infrastructure (not doable here): dispatch the `fono-voice`
`build-onnxruntime` workflow to rebuild `libonnxruntime.a`, re-pin the
per-triple SHAs in `scripts/fetch-onnxruntime.sh`, upload the converted
`.ort` pack, then pin the four graphs from the uploaded bytes — which
unblocks the size gate (Task 3.2), Slice 4, and Slice 5.

## 2026-07-14 — Supertonic local TTS: Slice 3 ops config merged; binary-growth measured

Finished the Supertonic-side work that can be done off the build
infrastructure, and answered the binary-growth question.

Binary-growth analysis (the "how much does the binary grow" question):
- Extracted the operator set from the four **Supertonic 3** int8 graphs
  (`sherpa-onnx-supertonic-3-tts-int8-2026-05-11`, confirmed genuinely
  v3: 31-language `opensource-multilingual` split; the `tts.json`
  `tts_version: v1.7.3` is the internal model-format version, not the
  product generation). 43 ops, all `ai.onnx` opset 19.
- Diffed against the shipped Piper+Kokoro+wake union in `fono-voice`'s
  `onnxruntime/ops.config`: only **six net-new ops** —
  `BatchNormalization, Constant, Erf, PRelu, Reciprocal, Tile`. Every
  heavy op (Conv/MatMul/Gemm/Softmax/LayerNormalization + all int8 quant
  kernels) is already linked.
- Estimated growth for Supertonic v3: the five real kernels (Constant is
  intrinsic) are light float elementwise/norm ops — under the +0.77 MiB
  Kokoro's heavier quant kernels cost — so **≈ a few hundred KB (~+0.4
  MiB)**. Parakeet v3 could not be measured: no model present locally and
  no onnx tooling installed (that is exactly the Parakeet plan's Task A4
  spike). Combined Supertonic+Parakeet is estimated ~+1–2 MiB, which
  would likely need the 25 MiB `cpu` gate bumped toward the 28 MiB hard
  cap (sign-off per ADR 0022).

Landed:
- **Slice 3 config half (Task 3.1, partial)**: merged the six net-new
  Supertonic ops into `fono-voice` `onnxruntime/ops.config`
  (fono-voice commit `9d6d4b2`), appended to the `ai.onnx;19` line
  without type constraints (all-types, matching the existing
  Reshape/Shape entries; can never strip a type a model needs).
  Structurally validated. This makes the runtime rebuild one
  `workflow_dispatch` away.

Remaining (infra, requires the build machine / model hosting — not doable
in this environment):
- Convert the four graphs to `.ort` (needs `onnxruntime==1.24.2` python),
  dispatch the `fono-voice` `build-onnxruntime` workflow, re-pin the
  per-triple SHAs in `scripts/fetch-onnxruntime.sh`, then run
  `./tests/check.sh --size-budget` to record the real delta (Task 3.2).
- Host the converted `.ort` pack and pin the four graphs' SHA-256 (they
  are `UNPINNED` today), which unblocks Slice 4 (catalog/router/config)
  and Slice 5 (E2E + oracle cross-check).

## 2026-07-14 — Supertonic local TTS: Slices 1–2 landed (engine core done)

Implemented the model-distribution and engine-core slices of the
Supertonic 3 local TTS plan
(`plans/2026-07-12-supertonic3-local-tts-engine-v1.md`). All work lives
under `crates/fono-tts/src/supertonic/` behind the existing `tts-local`
feature; zero new dependencies (confirmed: no `Cargo.toml`/`Cargo.lock`/
`deny.toml` lines touched — Slice 3 Task 3.3 done).

Landed (signed off, not pushed):
- **Slice 1 — distribution** (`mod.rs`): one shared ~140 MB pack fetched
  and SHA-256-verified from the voice mirror into its own `supertonic/`
  cache subdir, with a one-time OpenRAIL-M notice-on-download. The three
  format-stable files (`tts.json`, `voice.bin`, `unicode_indexer.bin`)
  are pinned with real checksums; the four graphs are named `.ort` and
  left `UNPINNED` until Slice 3 converts + hosts them (the wake
  `hey_fono` precedent). Corrected a plan error: the minimal ORT runtime
  loads only `.ort`, never raw `.onnx`.
- **Slice 2 — engine core**: `config.rs` (tts.json), `style.rs`
  (voice.bin, 10 speakers), `frontend.rs` (cleanup + 31-language NFKD via
  a committed generated table + Hangul + indexer + expressive-tag
  allowlist/stripping), `chunker.rs` (faithful `ChunkText` port),
  `engine.rs` (four `ort` sessions + the `Process`/concat pipeline, plus
  a hand-rolled MT19937 + Marsaglia-polar Gaussian unit-tested bit-exact
  against C++ `std::mt19937`). `TextToSpeech` implemented for
  `SupertonicLocal`. ~60 new unit tests; full fmt/clippy/test gate green.
- **Slice 4 Task 4.4 (frontend half)**: `strip_unknown_tags` +
  `EXPRESSIVE_TAGS` allowlist.

Blocked / next (a hard dependency chain, none doable in this
environment):
- **Slice 3 (3.1/3.2)** — extend the minimal-ORT ops config for the four
  int8 graphs, rebuild `libonnxruntime.a`, run the size gate. Needs
  `onnxruntime==1.24.2` Python tooling, the ONNX→`.ort` conversion, and a
  ~40 min runtime build. **Until this runs and the pack is hosted, no
  audio can be produced and the four graphs stay `UNPINNED`.**
- **Slice 4 (4.1/4.2/4.3/4.5)** — catalog/router/config/UI/docs. Must
  **not** land before Slice 3: the router's `load_engine` would try to
  download the not-yet-hosted `.ort` graphs at daemon startup and fail.
- **Slice 5 (5.2/5.3)** — deterministic E2E + oracle cross-check need the
  converted pack + linked runtime (E2E test is `#[ignore]`d meanwhile).
- Open routing decision for Slice 4.2: when Supertonic is enabled, does
  it take all languages (displacing Kokoro for English) or non-English
  only (Kokoro keeps English, Piper stays fallback for the 7 languages
  Supertonic lacks)?

## 2026-07-14 — Windows size budget raised to ≤ 75 MiB

Bumped the Windows binary budget in ADR 0022 (2026-07-14 amendment):
**enforced ≤ 75 MiB (78 643 200 B), hard cap ≤ 80 MiB**, superseding the
2026-07-13 ≤ 60 MiB / ≤ 64 MiB figures. Reason: those predated local TTS
+ wake-word landing on Windows, which added the embedded ONNX Runtime
(~3 MiB) and pushed `fono-vX.Y.Z-x86_64.exe` to ~72 MiB. The new ceiling
leaves ~3 MiB of headroom. Windows is a single no-choice download the
maintainer rarely tests, so this ceiling is a loose sanity bound, not a
tight ship-size target — Linux `cpu` (≤ 25 MiB) and macOS stay strict.
No CI enforcement yet: the dumpbin/size `windows`-job gate is still
deferred to Windows port Phase 14; when it lands it asserts ≤ 75 MiB.
Docs synced: ADR 0022, `docs/build-windows.md`, and the Phase 14 task in
`plans/2026-05-26-windows-port-v1.md`. Docs-only; Linux/macOS unaffected.

## 2026-07-14 — Release v0.16.0 (Windows support)

Cut the 0.16.0 release. Headline: **Windows support (experimental)** — a
single `fono.exe` joins the Linux and macOS builds, with the tray, F7/F8
push-to-talk + assistant, cursor injection (clipboard fallback), the
recording overlay, focused-app awareness, local STT/polish/TTS/wake-word,
and every cloud provider. One download, GPU-when-present with CPU fallback
(the Vulkan soft-load work), self-install/uninstall/update. Built and
exercised remotely, not daily-driven — shipped as experimental.

Also in the release:
- **Atomic, checksum-verified model downloads (all platforms).** An
  interrupted download no longer leaves a half-finished model that fails
  on every later start; downloads land in a temp file and are only moved
  into place once verified.
- **Glass Cortex overlay** style (opt-in, off by default, local models
  only) — a live view of the on-device LLM. Acknowledged still rough on
  real replies; flagged as a preview pending a rework.

Release checklist done: workspace version 0.15.0 → 0.16.0
(`Cargo.toml` + `Cargo.lock`), `CHANGELOG.md` `[0.16.0]` section,
`ROADMAP.md` (Recently shipped banner + Shipped list entry, Windows moved
out of On the horizon, header table Windows tile swapped for Local REST
API). The 14 feature commits since v0.15.0 were left intact (already clean,
user-friendly, and signed off); only the release commit was added.

Open item carried forward: Windows `fono.exe` is ~72 MiB. The budget was
reviewed and raised to ≤ 75 MiB (see the 2026-07-14 amendment note
above) before the Windows size gate goes blocking. Linux `cpu` binary
stays well under its 25 MiB budget.

## 2026-07-13 — Windows local TTS enabled (ONNX Runtime link fixed)

Local (offline) TTS + wake-word now ship in the Windows build, matching
Linux/macOS. Previously excluded because ONNX Runtime (`ort`) would not
link on Windows.

- **Root cause 1 — incomplete/bloated ORT lib (fixed in fono-voice).**
  The published Windows `onnxruntime.lib` was ~343 MiB (≈70% embedded
  CodeView debug info) *and* missing FetchContent deps. Fixed the
  `build-onnxruntime.yml` merge to strip debug info and reconstruct the
  archive from object members under **unique names** (MSVC `lib.exe`
  keeps duplicate member names, breaking on-demand resolution; macOS
  `libtool`/Linux `ar` dedup). Added a verify gate. Republished:
  ~117 MiB, complete, SHA `6bb2d9ac…fb50`, pinned in
  `scripts/fetch-onnxruntime.sh`.
- **Root cause 2 — MSVC single-pass archive resolution (fixed in fono).**
  Even with a complete, correctly-indexed archive, `link.exe` reported
  `LNK1120` for symbols that were present + indexed — the classic MSVC
  single-pass circular-dep failure. Fix: **double-link** the archive
  (reference `onnxruntime.lib` twice) via a new `crates/fono/build.rs`,
  gated on Windows + the ORT features. This is **size-safe** — dead-strip
  still runs, unlike `/WHOLEARCHIVE` (which also dragged in unresolvable
  test/interop objects). Enabling local TTS added only **~3 MiB**
  (~69→~72 MiB).
- **Verified on Windows 10:** clean `cargo build --features
  windows-defaults` (no manual link-arg), windowless GUI subsystem, no
  `vulkan-1.dll` import, GPU intact, and the local voice `af_heart`
  auto-downloaded + initialised ("tts local ready").
- **Gates green (Linux):** fmt clean, `clippy --workspace --all-targets
  -D warnings` exit 0, 36 test suites (0 failed). Committed `204df25`
  (signed off), on top of the fono-voice archive-fix commits.

Flags still open: Windows `fono.exe` is ~72 MiB — over ADR 0022's
current Windows cap; needs a budget review before the Windows size gate
goes blocking. The `release-slim` build needs a short `CARGO_TARGET_DIR`
(MAX_PATH); worth pinning in Windows CI/build docs.

## 2026-07-13 — Vulkan soft-load: Phase 3 (docs/gates) done — initiative complete

Closed out `plans/2026-07-12-vulkan-soft-load-single-build-v1.md`. The
soft-load initiative (single Windows build + Linux no-hard-link) is
complete through Phase 3; only the release-tag-time CHANGELOG/ROADMAP
"Shipped" move remains, deferred by design.

- **Phase 3 was mostly already done.** ADR 0022 (Task 3.1) and
  `docs/build-windows.md` (Task 3.3) landed in Phase 2; the
  windows-port forward-pointers (Task 3.2) were filed with the plan.
  Verified all present rather than re-editing.
- **ROADMAP Windows entry** now states Windows will ship a single
  GPU-accelerated download that falls back to CPU (forward-looking
  "On the horizon" phrasing, not a shipping claim), pointing at this
  plan. Fixed a stray typo on the `docs/install.md` GPU-version line.
- **README/install.md left as "planned, not shipping yet"** — no
  premature shipping claim; the user-facing CHANGELOG/ROADMAP-Shipped
  wording is deferred to release tag time per the project rules (the
  CHANGELOG has no Unreleased section).
- **Gates green (Linux):** `cargo fmt --all --check`, `clippy
  --workspace --all-targets -D warnings`, 36 test suites (0 failed),
  and `./tests/check.sh --size-budget` (cpu variant 21.36 MiB ≤ 25 MiB,
  NEEDED ⊆ 4-entry allowlist).

Remaining across the wider effort: the dumpbin/size CI assertion +
promoting the `windows` job to blocking (Windows port Phase 14), and the
CHANGELOG/ROADMAP-Shipped entries when the next release is tagged.

## 2026-07-13 — Vulkan soft-load: Phase 2 (Windows single build) done

Completed the Windows half of
`plans/2026-07-12-vulkan-soft-load-single-build-v1.md`. Windows now
ships **one** Vulkan-accelerated `fono.exe` that runs everywhere — GPU
when a driver's `vulkan-1.dll` is present, CPU fallback when it isn't.

- **Cross-platform shim.** Extended `crates/fono-core/src/vk_loader_shim.rs`
  with a Windows `sys` module (`LoadLibraryA`/`GetProcAddress`,
  `vulkan-1.dll`). All the interesting logic — the three `#[no_mangle]`
  forwarders and the error-stub fallback — is shared; only the loader
  open differs per-OS.
- **No `/DELAYLOAD` needed.** The Phase 0 hedge turned out unnecessary:
  because the shim defines ggml's three bare Vulkan symbols itself,
  MSVC satisfies them from our object and never pulls the import from
  `vulkan-1.lib`. Confirmed on the bench with `dumpbin /DEPENDENTS
  fono.exe` — **no `vulkan-1.dll` in the PE import table**, the exact
  Windows analogue of the Linux `--as-needed` result.
- **`accel-vulkan` added to `windows-defaults`** (`crates/fono/Cargo.toml`).
- **Verified end-to-end on the Windows 10 bench.** Loader present:
  `doctor` reports Vulkan detected (Intel HD 620), `fono-bench
  equivalence` transcribes on GPU (PASS). Loader absent (bogus
  `vulkan-1.dll` in the exe dir): transcription **exits 0, no crash**,
  CPU fallback, identical acc 0.0882. The error-stub fix works
  identically on Windows.
- **CI + release wiring.** Added a pinned LunarG Vulkan SDK install
  step (v1.4.350.0) to the `windows` job and the `release.yml` Windows
  row; fixed a Phase 1 regression in `release.yml` that still required
  the Linux GPU variant to link `libvulkan.so` (now both variants
  forbid it). Fixed the probe's hardcoded `libvulkan.so.1` message to
  say `vulkan-1.dll` on Windows.
- **Docs/ADR.** ADR 0022 gains a 2026-07-13 amendment (Windows ≤ 60 MiB,
  `vulkan-1.dll` must be absent from the import table);
  `docs/build-windows.md` documents the SDK prereq + the single-build
  decision.

Gates green on Linux: fmt, `clippy --workspace --all-targets`, 36 test
suites. Remaining: the dumpbin/size CI assertion + promoting the
`windows` job to blocking (Windows port Phase 14), and Phase 3 docs
tail (README/install wording, CHANGELOG/ROADMAP at release time).

## 2026-07-12 — Vulkan soft-load: Phase 0 spike + Phase 1 (Linux) done

Implemented and verified the Linux half of
`plans/2026-07-12-vulkan-soft-load-single-build-v1.md`. The Vulkan GPU
build no longer hard-links the loader: it launches everywhere and falls
back to CPU when Vulkan is absent.

- **Root cause (spike).** ggml already dispatches Vulkan calls through a
  runtime dispatcher; only **3 bare symbols** forced the hard link
  (`vkGetInstanceProcAddr`, `vkCmdCopyBuffer`,
  `vkGetPhysicalDeviceFeatures2`).
- **Fix.** A small in-tree shim (`crates/fono-stt/src/vk_loader_shim.rs`,
  gated on `accel-vulkan` + Linux) defines those 3 symbols as lazy
  `dlopen("libvulkan.so.1")` forwarders. With the workspace's existing
  `--as-needed`, the loader drops out of `NEEDED` — no ggml source patch,
  no `whisper-rs-sys` fork.
- **Critical catch.** A naive null-returning shim *segfaults* on
  loader-absent hosts: ggml calls `vk::enumerateInstanceVersion()`
  through the dispatcher before any guard. The shim now returns a
  non-null error stub (`VK_ERROR_INITIALIZATION_FAILED`) so Vulkan-Hpp
  throws, ggml catches it, and inference falls back to CPU cleanly.
- **Verified on the canonical artifact.** `fono --profile release-slim
  --features accel-vulkan` = 57 MiB, `NEEDED` is the 4-entry universal
  allowlist (`libvulkan.so.1` gone). Loader-present → GPU used
  (`fono doctor`: "Vulkan: detected (Intel LNL, llvmpipe)"); loader
  absent (bind-mount shadow in `unshare`) → launches (exit 0), CPU
  fallback, identical transcript (WER 0.0882 both ways).
- **CI gate tightened.** The `accel-vulkan` size-budget row in
  `.github/workflows/ci.yml` now sets `extra_needed: ""`, so the gate
  asserts the loader is *absent* from `NEEDED`.

Remaining: Windows single Vulkan-with-fallback build (Phase 2, needs the
Windows host for the `/DELAYLOAD` tolerance check + loader-absent smoke),
llama-only GPU build confirmation, and the docs/ADR pass (Phase 3,
including the ~60 MiB Windows budget amendment).

## 2026-07-12 — Decision + plan: soft-load Vulkan (next up)

New plan filed: `plans/2026-07-12-vulkan-soft-load-single-build-v1.md`.
This is what we work on next. Two maintainer decisions captured:

- **Windows ships a single Vulkan-accelerated `.exe` that falls back to
  CPU** when `vulkan-1.dll` (or a usable device) is absent. This
  reverses the "CPU-only Windows v1, GPU deferred" decision (Windows
  port plan Task 3.4). Reason: **simplicity** — one artefact, no
  variant matrix / runtime probe / self-update variant-switching to
  maintain on a target the maintainer rarely tests. Cost (a ~60 MB
  `.exe` for everyone) is accepted on Windows.
- **The Linux `fono-gpu` variant stops hard-linking `libvulkan.so.1`.**
  It soft-loads the loader and falls back to CPU, so it launches on
  Vulkan-less hosts and its NEEDED set shrinks back to the 4-entry
  universal allowlist. This is the long-deferred item from
  `plans/closed/2026-05-02-fono-cpu-gpu-variants-v1.md:323-325`.

Scope guards: Linux keeps the two-variant model (compact CPU default
stays — the 42 MB shader payload still violates the Linux size budget);
only the GPU variant becomes launch-safe. The shared enabler is making
ggml-vulkan load the loader softly (Linux: headers-only link / volk /
`VK_NO_PROTOTYPES` shim — spike decides; Windows: `/DELAYLOAD:vulkan-1.dll`).
ggml already falls back to the CPU backend when it enumerates zero
devices, so "fallback" is mostly the backend's existing behaviour once
the loader load is lazy. Forward-pointers added to the Windows port plan
(Task 3.4, Phase 14.3) and the superseded-decision banner at its top.

## 2026-07-12 — Windows port: release artefact (Phase 13)

The release workflow now builds and uploads a Windows binary, so the
next tagged release will carry a `fono-vX.Y.Z-x86_64.exe` asset (plus
its `.sha256` sidecar and a `SHA256SUMS` entry) alongside the Linux and
macOS binaries. This is what lights up the Windows self-update landed in
Phase 12.

- **New `windows-2022` build-matrix row** in `.github/workflows/release.yml`:
  CPU-only x86_64, built with `--profile release-slim
  --no-default-features --features windows-defaults` (the v1 feature set
  — default minus `tts-local`/`wakeword-onnx`, the only `ort` pullers).
  A new `no_default_features` matrix key drives the flag, and the Build
  step is now `shell: bash` so the argument-assembly runs under Git Bash
  on the Windows runner.
- **Windows-only prep steps** mirrored from the ci.yml windows job: git
  long paths before checkout, and `LIBCLANG_PATH` pointed at the
  runner's preinstalled LLVM. The onnxruntime fetch is skipped on
  Windows (no `ort` in the v1 graph). The `/FORCE:MULTIPLE` ggml-dedup
  link flag comes from `.cargo/config.toml`'s MSVC target block
  automatically.
- **Bare `.exe` only.** No MSI, no code signing, no distro-style package
  job (explicit v1 non-goals). Windows is excluded from the internal
  distro-staging tarball so its `x86_64` label can't clash with the
  Linux staging stem. The existing NEEDED/dylib verification steps are
  already OS-gated and skip Windows; the PE import-table + size gate is
  deferred to Phase 14.
- **Checksums already cover it.** The `SHA256SUMS` `find` and the
  per-asset `.sha256` sidecar loop already list `fono-v*-x86_64.exe`.
  `fono-update`'s asset-name test fixture (`.exe` suffix) was in place
  since Phase 1.7.

Verified over SSH: the release job's exact `release-slim` build command
links a working `fono.exe` — **16,443,392 B ≈ 15.7 MiB**, comfortably
under the Phase 14 ~30 MiB Windows budget — and `--version` prints
`fono 0.15.0`. YAML validated (5 matrix rows, Windows row correct).
Linux gate green: fmt, clippy `-D warnings`, `fono-update` tests. The
live end-to-end (a tag actually producing + uploading the asset) is
exercised on the next release tag.

## 2026-07-12 — Windows port: self-update (Phase 12)

`fono update` now works on Windows, using a rename-and-relaunch that
respects Windows' rule that a running `.exe` can't be overwritten (it
*can* be renamed).

- **Rename-based swap already cross-platform.** `apply_update`'s
  existing dance (download to a temp file in the same dir → verify
  SHA-256 → `rename(old → old.bak)` → `rename(tmp → old)`) works on
  Windows unchanged, because the running image can be renamed aside.
- **Windows `restart_in_place`.** Windows has no `execv`, so instead of
  replacing the process image it spawns the freshly-installed binary as
  an independent child (inheriting stdio + argv) and exits, releasing
  the renamed old image (the sibling `.bak`), which a later
  `fono update` cleans up. PID changes (unavoidable), but the command
  continues in the new binary.
- **Package-managed detection.** `is_package_managed` gained a
  `#[cfg(windows)]` branch: per-user installs under `%LOCALAPPDATA%\fono\`
  stay self-updatable; installs under `Program Files` are treated as
  managed (refuse up front rather than fail mid-swap on access-denied).
  `elevation_hint()` now returns a Windows-appropriate message
  (reinstall with `fono install`) instead of suggesting `sudo`.
- **`--bin-dir` fix.** The `--bin-dir` target override now appends
  `fono.exe` on Windows (not the extensionless `fono`).
- **Zero new dependencies; no Cargo.lock change.**

Verified on the box: full `fono.exe` builds; all 15 `fono-update` unit
tests pass (including the new Windows-gated `pkg_managed_paths_windows`
and the Unix-specific `pkg_managed_paths` gated `#[cfg(not(windows))]`);
`fono update --check` exercises the asset-name path and correctly reports
"no matching release asset" (no Windows release published yet — expected
until Phase 13). Linux gate green: fmt, clippy `-D warnings`, full test
suite; CPU↔GPU auto-switching and the Unix rename/`execv` path untouched.
The live download→swap→relaunch round-trip becomes exercisable once a
Windows release artefact ships (Phase 13).

## 2026-07-12 — Windows port: install and autostart (Phase 11)

Fono now installs itself on Windows with a plain `fono install` — no
administrator prompt — and starts automatically at the next login,
matching the per-user, no-elevation experience the macOS installer
already gives.

- **Per-user installer.** Added `crates/fono/src/install/windows.rs`.
  `fono install` copies the running binary to
  `%LOCALAPPDATA%\fono\fono.exe`, writes an autostart entry to
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\fono` (path
  stored quoted so a profile name with spaces still launches), and
  records `%LOCALAPPDATA%\fono\install_marker.toml` (version, install
  path, unix timestamp) so `fono doctor` can tell a self-managed
  install from an ad-hoc binary on PATH.
- **`fono uninstall`.** Deletes the Run value and the
  `%LOCALAPPDATA%\fono\` directory, deliberately keeping the user's
  config and history under `%APPDATA%\fono\` (mirrors Linux/macOS).
- **`--server` refused.** The headless Wyoming server + system service
  stays Linux-only in v1, with a message pointing that out (same as
  macOS).
- **Deviation from plan:** used the built-in `reg.exe` via subprocess
  rather than the `winreg` crate — mirrors the macOS installer's
  `launchctl`/`security` subprocess style, needs no `unsafe` FFI, and
  keeps the binary dependency-free (binary size is the top priority;
  `winreg` would have been new to the graph). **Zero new dependencies;
  no Cargo.lock change at all.**

Verified over SSH (registry writes work headless, unlike the
interactive-window-station APIs behind tray/hotkeys): `install
--dry-run`, a real `install`, then `uninstall`. Confirmed the Run
value, the escaped-path TOML marker, and the copied binary appear,
`doctor` reports "self-installed via `fono install`", and uninstall
removes the Run value + install dir while leaving `%APPDATA%\fono`
untouched. Windows-gated unit tests (marker TOML validity, quoted Run
value, `--server` refusal) pass on the box. Linux gate green: fmt,
clippy `-D warnings`, full test suite; the Linux/macOS installers are
separate cfg-gated modules and untouched (only `install/mod.rs` module
wiring changed). Live login-autostart is the manual desktop check.

## 2026-07-12 — Windows port: on-screen overlay (Phase 10)

Fono's recording overlay now paints on Windows, so the same
waveform indicator users see on Linux and macOS appears during
dictation on Windows too — a translucent, always-on-top strip that
never steals focus, never intercepts clicks, and stays out of
Alt+Tab.

- **New Win32 layered-window backend.** Added
  `crates/fono-overlay/src/backends/windows.rs`, a dedicated
  worker thread that owns a layered tool-window and blits the
  shared renderer's premultiplied-ARGB framebuffer via
  `UpdateLayeredWindow` (with `AC_SRC_ALPHA`). Window styles
  `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT |
  WS_EX_TOPMOST | WS_EX_LAYERED` give click-through,
  focus-passthrough, topmost, and Alt+Tab exclusion.
- **Deviation from plan:** dropped the planned winit+softbuffer
  approach — softbuffer blits through GDI `BitBlt`, which ignores
  per-pixel alpha, so the rounded-corner transparency would be
  lost. `UpdateLayeredWindow` is the only path that honours it. The
  backend mirrors the macOS worker-thread structure.
- **Selection + override.** Added `BackendId::Win32LayeredToolWindow`
  and `HostOs::Windows`; `candidate_list_with` returns
  `[Win32LayeredToolWindow, Noop]` on Windows (Linux/macOS tables
  unchanged). `FONO_OVERLAY_BACKEND` aliases (`win32` / `windows` /
  `win` / `layered` / `noop`) work on Windows; `parse` now trims
  whitespace so a stray trailing space from cmd.exe `set VAR=win32 `
  doesn't defeat the override. Anchors to the primary monitor's
  bottom-centre via `GetSystemMetrics`.
- **doctor.** `fono doctor` reports the overlay backend and its
  capabilities (`transparency=yes positioning=client
  focus-passthrough=yes click-passthrough=yes`).
- **No new dependency.** `windows-sys 0.59` was already in the graph;
  this adds a Windows-only edge to `fono-overlay` (single lockfile
  line), zero binary cost off-Windows.

Verified over SSH: the full `fono.exe` (`windows-defaults`) builds on
`x86_64-pc-windows-msvc`; `doctor` selects `win32-layered-toolwindow`
by default and honours the `noop` / `win32` overrides (including with
a trailing space). Linux gate green: fmt, clippy `-D warnings`, full
test suite; the wlr-layer-shell / X11 / noop backends and the macOS
panel are untouched. The overlay actually painting during recording
(correct anchoring, no focus-steal, no Alt+Tab entry) is the manual
desktop gate, like the tray/hotkey/typing/focus smokes.

## 2026-07-12 — Windows port: focused-window detection (Phase 9)

Fono now knows which Windows app you're dictating into, so its per-app
context rules (terminal shell vocabulary, code-editor hints, and
history suppression for password managers) work on Windows just as they
do on Linux and macOS.

- **Win32 foreground-window probe.** Added `windows_focus()` in
  `crates/fono-inject/src/focus.rs`, wired into `detect_focus()` under
  `cfg(target_os = "windows")`. It uses `GetForegroundWindow`,
  `GetWindowTextW` (title), `GetWindowThreadProcessId`, and
  `QueryFullProcessImageNameW` to return the focused window's title, pid,
  and bare executable name (e.g. `chrome.exe`) as the `window_class`. No
  formal `FocusBackend` trait — kept the existing per-OS function
  dispatch, consistent with the tray-backend decision in Phase 1.
- **Windows classifier rules.** Added `.exe` entries (chrome.exe,
  Code.exe, WindowsTerminal.exe, Discord.exe, KeePassXC.exe, …) to every
  built-in rule in `classifier.rs`, each gated
  `#[cfg(target_os = "windows")]` on the individual element so the
  Linux/macOS binary is byte-for-byte unchanged.
- **doctor `Focus` line.** `fono doctor` now shows the focused app and
  the matched context profile on every platform — a genuinely useful
  cross-platform diagnostic (this is the one Linux-visible change; it
  adds a diagnostic line, no behaviour change).
- **No new dependency.** `windows-sys 0.59` was already in the graph via
  cpal / fono-tray; this is a new edge only (single lockfile line),
  zero binary cost off-Windows.

Verified: the full `fono.exe` (`windows-defaults`) builds on
`x86_64-pc-windows-msvc`, the Windows-gated classifier test
(`windows_exe_names_classify`) passes on the box (chrome.exe → Browser,
Code.exe → CodeEditor, WindowsTerminal.exe → Terminal, KeePassXC.exe →
history-suppressed), and `doctor` renders the `Focus` line (reads "none
detected" over headless SSH — live population is a manual desktop gate,
like the tray/hotkey/typing smokes). Linux gate green: fmt, clippy
`-D warnings`, full test suite; x11rb focus path untouched.

## 2026-07-12 — Windows port: hotkeys + the daemon runs on Windows (Phase 8)

Push-to-talk hotkeys now resolve to the Win32 `RegisterHotKey` backend,
and — for the first time — the full daemon (not just `--version` /
`doctor`) runs on Windows. `global-hotkey` was already cross-platform;
the work was in the surrounding Linux-centric plumbing, plus a
Windows-only runtime crash:

- **Hotkey backend now resolves on Windows.** `detect_backend` only
  knew the Linux `DISPLAY` / `WAYLAND_DISPLAY` session signals, so on
  Windows it returned `Disabled` and the listener never started. The
  macOS special-case is now generalised to all non-Linux desktop
  targets. Confirmed live over SSH: `hotkey backend resolved: X11`.
- **`is_graphical_session` fixed.** Same Linux-env-var blind spot meant
  the daemon skipped the hotkey listener as "headless" on Windows. It
  now treats Windows (a user desktop app, never a session-0 service) as
  always having a graphical session.
- **Main-thread stack overflow fixed.** The first daemon run on Windows
  died with `thread 'main' has overflowed its stack` — the MSVC main
  thread defaults to 1 MiB vs 8 MiB on Linux/macOS. The entry point now
  runs on a big-stack worker thread on Windows, mirroring the macOS
  path. Linux/macOS unchanged.
- **Esc-to-cancel — no code change.** `listener.rs` drives the
  transient Esc registration entirely through `global_hotkey`'s
  cross-platform `register`/`unregister`, which resolve to the Win32
  backend on Windows.
- **No new dependency.** All code-only changes; `global-hotkey` was
  already in the graph.

Verified over SSH: the daemon starts, logs the resolved hotkey backend,
and reaches the `RegisterHotKey` call. Registration itself returns
`os error 1459` (non-interactive window station) over headless SSH —
the same limitation that blocks the tray icon over SSH — so the actual
key-press round-trip, like the tray and typing smokes, is a manual gate
for a human at the Windows desktop. Documented in the new "Hotkeys and
the daemon on Windows" section of `docs/build-windows.md`. Linux gate
green: fmt, clippy `-D warnings`, full test suite; Linux hotkey detect
+ portal Esc flow unchanged.

## 2026-07-11 — Windows port: text injection on Windows (Phase 7)

Dictated text now types into Windows apps. The `fono` crate enables
`fono-inject/enigo-backend` in its
`[target.'cfg(target_os = "windows")'.dependencies]` block — the same
per-target feature-seam pattern the macOS port used (Task 6.1) — so the
Windows binary types via enigo's Win32 `SendInput` path.

- **No new-to-project dependency.** enigo was already a workspace dep
  and already in `Cargo.lock`; its Windows backend pulls `windows 0.56`,
  which the lock already carried. No libxdo on Windows (that's the Linux
  X11 path). Target tables don't unify off-target, so the shipped
  Linux/macOS binaries are byte-for-byte identical.
- **Backend selection was already correct.** `Injector::detect_auto`
  short-circuits to `Enigo` on `not(target_os = "linux")` — landed in
  Phase 1's cfg refactor (Task 1.3). No X11/Wayland cascade is compiled
  on Windows. Confirmed live: `fono.exe doctor` reports
  `Injector : Enigo`.
- **Clipboard fallback works out of the box.** `fono-inject`'s
  non-optional `arboard` dep speaks the Win32 clipboard natively —
  `fono.exe doctor` reports `Clipboard : native (arboard)`. No new cfg
  or crate needed.
- **doctor cosmetic fix.** The doctor's clipboard-manager probe
  (ICCCM `CLIPBOARD_MANAGER` / `/proc` scan for clipit/parcellite/…) and
  its X11-specific "typed via XTEST" guidance are an X11/Wayland concept
  only — `detect_clipboard_manager` returns `None` off-Linux — so the
  whole block is now gated under `cfg(target_os = "linux")`. It no
  longer prints misleading X11 text on Windows/macOS `doctor` output.

Verified over SSH: the full `fono.exe` (`--features windows-defaults`)
compiles, links, and runs on `x86_64-pc-windows-msvc`, and `doctor`
reports the enigo injector + native clipboard. The end-to-end typing
smoke (Task 7.3 — Notepad, Chrome address bar, Discord/Slack) needs a
human at the interactive desktop (headless SSH has no focused window to
type into) and is handed to the user. Linux gate green: fmt, clippy
`-D warnings`, full test suite; Linux inject cascade unchanged.

## 2026-07-11 — Windows port: tray icon on Windows (Phase 6)

The Windows build now has a real notification-area tray. A new
`crates/fono-tray/src/backend_windows.rs` renders the shared tray menu
model (`crate::menu`) via the `tray-icon` crate + `muda`, slotting in
behind the Phase 1.1 `spawn` seam exactly like the Linux (`ksni`) and
macOS (`NSStatusItem`) backends. Full menu parity is automatic — every
backend consumes the same `menu::build` node tree.

- **Dedicated-thread pump.** `tray-icon`'s `TrayIcon`/`Menu` are `!Send`
  and need a Win32 message loop on their owning thread — but Windows
  (unlike macOS/AppKit) allows *any* thread, so the backend spawns a
  dedicated `fono-tray` OS thread running a `PeekMessageW` pump. No
  `fono::main` change was needed. The tokio poll task keeps the same 2 s
  snapshot-diff cadence and ships `MenuNode` trees (pure `Send` data)
  over a channel; menu clicks come back via `MenuEvent::receiver()`.
- **In-code icon, no PNG.** The plan's `assets/fono.png` never existed;
  the Windows backend generates the icon in code from `menu::state_color`
  (a 32×32 RGBA state-tinted circle via `Icon::from_rgba`), identical to
  the Linux/macOS approach — so no image crate is pulled in.
- **One new-to-project dep: `tray-icon` (Windows-only).** MIT/Apache-2.0,
  already allowlisted; `muda` is transitive via `tray_icon::menu`. A
  Windows-only `windows-sys` edge (already in the lock via cpal, net-zero)
  drives the pump. `deny.toml` already carried the gtk/libappindicator
  advisory ignores from tray-icon's earlier stint as the Linux backend,
  so no deny.toml change was needed.
- **Zero Linux/macOS impact.** tray-icon lives under
  `[target.'cfg(target_os = "windows")'.dependencies]`, so the shipped
  Linux/macOS binaries are byte-for-byte identical (target tables don't
  unify off-target). Linux stays on ksni.

Verified over SSH: `fono-tray` (with `tray-backend`) and the full
`fono.exe` (`--features windows-defaults`) both compile and link on
`x86_64-pc-windows-msvc`, and `fono.exe --version` runs with the tray
backend linked in. The visual gate — the icon actually appearing in the
notification area with the right menu — needs an interactive Windows
desktop session (not headless SSH) and is handed to the user.

Gate: Linux fmt clean; workspace tests green; clippy clean on
`--lib --bins --tests` (the one failing target, `fono-core`'s
`brain_trace_dump` example, is unrelated uncommitted brain-visual WIP,
not part of this change). Linux binary unaffected. Committed; not pushed.

## 2026-07-11 — Windows port: `fono.exe` links and runs (Phases 3 & 5) — first working Windows binary

The Windows binary now **links and runs**: `fono.exe --version` prints
`fono 0.15.0` and `fono.exe doctor` enumerates the WASAPI default input
device over SSH. Two remaining link blockers were cleared and the audio
backend was wired for Windows.

- **ort link blocker sidestepped for v1 (Task 5.1).** The pinned
  Windows `onnxruntime.lib` is not a self-contained merged archive (its
  protobuf/abseil/onnx/cpuinfo deps are unresolved — `LNK1120`, 157
  externals), so Windows v1 builds a new **`windows-defaults`** feature
  set: the Linux default minus `tts-local` and `wakeword-onnx`, the only
  two features that pull `ort`. Local whisper STT and local llama polish
  (no `ort`) are kept; local TTS and wake-word return once a merged
  static lib is hosted. Build with `cargo build -p fono
  --no-default-features --features windows-defaults`.
- **Duplicate-ggml link error fixed (Task 3.3 cont.).** With `ort` out
  of the graph, the link hit `LNK2005/LNK1169` — the two vendored ggml
  copies (`whisper-rs-sys` + `llama-cpp-sys-2`) colliding on plain C
  symbols. Fixed with `/FORCE:MULTIPLE` in a new
  `[target.x86_64-pc-windows-msvc]` rustflags block in
  `.cargo/config.toml` — the MSVC analogue of the GNU
  `--allow-multiple-definition` we already use.
- **WASAPI audio backend wired (Tasks 5.1/5.2/5.4).** Added a
  `[target.'cfg(target_os = "windows")'.dependencies]` block to
  `crates/fono/Cargo.toml` enabling `fono-audio`'s `cpal-backend`
  (mirrors the macOS block). `fono.exe doctor` lists the Windows input
  device via cpal — capture backend initialises and enumerates. Linux
  stays on parec, byte-identical.
- **CI `windows` job** now builds + tests the ort-free `windows-defaults`
  set (should go green); the onnxruntime fetch / `ORT_CXX_STDLIB` steps
  were dropped as unnecessary for this graph.

Still pending for the Phase 5 gate (need a human at the box, a cloud STT
key, and Windows text injection from Phase 7): the end-to-end voice →
cloud STT → injected-text smoke, and the WASAPI playback smoke.

Gate: Linux fmt clean; workspace tests green (180+); clippy clean on
`--lib --bins --tests` (the one failing target, `fono-core`'s
`brain_trace_dump` example, is unrelated uncommitted brain-visual WIP,
not part of this change). Linux binary unaffected (Windows-only target
deps + an unused-on-Linux feature). Committed; not pushed.

**Next:** Phase 6 — Windows tray icon (`tray-icon` crate) behind the
Phase 1 backend seam; and, when a human is at the Windows box, close the
Phase 5 voice/playback smoke.

## 2026-07-10 — Brain visualisation: fixed the real-data regressions (black Thinking panel, "ruled paper" Speaking)

The 2026-07-07 thinking/speaking redesign looked right in the synthetic
gallery but broke on its first live run: Thinking rendered as a black
panel with one lone column, Speaking as flat "ruled notebook paper"
lines (user screenshots `a/b/c.png`). Root-cause class: the renderer
degenerated on *real-shaped* capture data — hundreds of
near-constant-norm keyframes — which the gallery never fed it. Fixes in
`crates/fono-overlay/src/cortex.rs`; full analysis and task list in
`plans/2026-07-10-brain-real-data-visual-fix-v1.md` (all reproduced
offline first, then fixed against the same scenes).

- **RC1 — robust normalisation.** Replaced the running per-layer max
  scale with an outlier-trimmed band (`mean ± max(2σ, 2 % of mean)`,
  two-pass trim, recomputed per ingest): real ±1 % norm variation now
  spans the display ramp (textured cells instead of a flat slab), and a
  20× BOS attention-sink first frame clamps to 1 instead of crushing
  every later frame to black. Unit tests cover both data shapes.
- **RC2 — integer column binning.** Decode-trace columns are now a
  whole-pixel stride with a ≥ 1 px gap (28–96 columns); long replays
  bin multiple keyframes per column (max/mean aggregate, per-bin
  entropy skyline). No more sub-pixel columns merging into a slab with
  moiré seam lines; live decode keeps a fixed narrow stride so a young
  trace is a few crisp columns, never one giant slab.
- **RC3 — never-black Thinking.** While the live trace covers only part
  of the strip, the uncovered columns keep showing a dim prefill
  resting field (floored so it exists even without prefill events) —
  decode visibly "eats" the prompt instead of latching into a dead
  panel. A pixel-level unit test asserts the panel can never read as
  dead at decode latch.
- **RC4 — background-robust stage.** A near-opaque dark backing under
  the whole cortex area: lit cells read as light-emitting over any
  desktop, and bright-background renders keep the same hierarchy as
  dark ones (previously the translucent panel inverted polarity into
  the ruled-lines look over light content).
- **Gallery is now honest.** `cortex_gallery.rs` gained permanent
  real-shaped regression scenes (7a–7f: 200-frame reply early/late,
  first-token latch, live mid-decode, BOS outlier, 1.25× fractional
  scale), each composited over BOTH a dark and a bright desktop. The
  throwaway repro harness was folded in and deleted.
- **RC5 (open).** The ~76 % width hard-stop in `b/c.png` is
  mathematically impossible in the current code (all offline renders
  span the full strip); prime suspect is a stale running binary. The
  workspace is rebuilt — needs a live-desktop retest with a real
  local-LLM reply to close (plan Task 6).

Gate: `cargo fmt --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `cargo test --workspace --tests --lib` all green (52
fono-overlay tests incl. 4 new). Committed; not pushed.

**Next:** live retest of the overlay (Thinking + Speaking on a real
reply) to close RC5 / plan Task 6; then continue the Windows port.

## 2026-07-10 — Windows port: IPC unification (Task 4.1) + MSVC C++-runtime link fix (Task 3.3); `fono` now compiles on Windows

Drove the real Windows build over SSH (`scripts/win-remote.sh`, box now
up) and cleared the two things that were stopping the `fono` binary from
compiling and starting to link on `x86_64-pc-windows-msvc`.

- **Task 4.1 — IPC is now cross-platform.** `crates/fono-ipc` moved off
  `tokio::net::UnixListener`/`UnixStream` onto the `interprocess` crate's
  Tokio local sockets: a Unix-domain socket at the same filesystem path
  on Linux/macOS, a named pipe on Windows. Added
  `interprocess = { version = "2", features = ["tokio"] }` to workspace
  deps (0BSD OR Apache-2.0, already allow-listed in `deny.toml`; new
  transitive crates `recvmsg`/`widestring`/`doctest-file` likewise
  allow-listed). `fono-ipc` exposes `Stream`/`Listener`/`RecvHalf`/
  `SendHalf` and `accept()`/`split_stream()` helpers so `fono` and
  `fono-mcp-server` need no direct `interprocess` dep. **Zero behaviour
  change on Linux**, release-slim **21.34 MiB** (under budget, NEEDED
  allowlist clean — interprocess added ~0). This removes the old
  `fono-ipc` Unix-socket breakpoint, so the whole `fono` binary now
  compiles on Windows.
- **Task 3.3 — MSVC C++-runtime link fixed.** The full link then failed
  with `LNK1181: stdc++.lib`. Root cause: `.cargo/config.toml`'s
  `ORT_CXX_STDLIB=static:-bundle=stdc++` (needed for the Linux NEEDED
  allowlist) leaks to MSVC via cargo's non-target-scoped `[env]` table,
  where `ort-sys` turns it into a bogus `-lstdc++`. Fix: neutralise it to
  empty on Windows so ort-sys uses its correct MSVC default (no explicit
  C++ stdlib link). CI `windows` job exports `ORT_CXX_STDLIB=`;
  `scripts/win-remote.sh` passes `--config env.ORT_CXX_STDLIB=''`.
  Documented in `.cargo/config.toml` and `docs/build-windows.md`. (The
  anticipated OpenMP-on-MSVC issue was a non-event — `llama-cpp-sys-2`
  already gates `gomp` on gnu and links the MSVC CRT on Windows.)
- **Tasks 3.1/3.2/3.4 ticked** as satisfied along the way (native MSVC
  toolchain builds all vendored C++ with no CMake overrides; CPU-only
  Windows v1 already decided).

**Current Windows blocker (open):** with `stdc++` fixed the link reaches
onnxruntime and fails `LNK1120: 157 unresolved externals` from
`libort_sys` (protobuf/abseil/onnx/cpuinfo). The pinned Windows
`onnxruntime.lib` isn't self-contained like the Linux `.a`; its companion
static libs must be added, or build a CPU-only Windows variant without
`tts-local` so `ort` is never linked. Folded into Phase 5 (audio/ONNX).

Gate: `cargo fmt --all --check`, `clippy --workspace --all-targets -D
warnings`, and `cargo test --workspace --tests --lib` all green on Linux;
release-slim size gate passed (21.34 MiB, 4-entry NEEDED).

Next: resolve the onnxruntime-on-MSVC static link (companion libs or
`tts-local`-off CPU variant) to get a linked `fono.exe` (Tasks 3.5/3.6),
then Phase 4 continues (Task 4.2 locale via `sys-locale`).

## 2026-07-10 — Windows port Phase 2 complete: non-blocking Windows CI job

Phase 2 of `plans/2026-05-26-windows-port-v1.md` (Tasks 2.1–2.3): CI now
has a `windows` job (`windows-2022` runner) so Windows porting progress
is visible on every push — without ever blocking the pipeline.

- **Non-blocking by design**: `continue-on-error: true`; the job is
  *expected to fail* until later phases land (next known blocker:
  `fono-ipc`'s Unix sockets, plan Phase 4). A red Windows run never
  gates a PR; Linux/macOS rows are unaffected. Promotion to a blocking
  gate comes with the Windows release artefact (plan Phase 13/14),
  mirroring the macOS job's Phase 12 promotion.
- **Structured as a dedicated job** (like the `macos` job), not a
  matrix row — same Actions-UI visibility, cleaner separation from the
  Linux fmt/clippy/bench steps that don't apply yet.
- **Phase 0 findings baked into the runner setup**: git long paths
  enabled before checkout (vendored llama.cpp exceeds legacy
  `MAX_PATH`), `LIBCLANG_PATH` pointed at the image's preinstalled
  standalone LLVM (VS Build Tools has no `libclang.dll`), pinned
  `onnxruntime.lib` fetched through Git Bash — the same recipe
  `scripts/win-remote.sh` uses over MSYS.
- **Windows size/import gate explicitly deferred** (Task 2.2): the ELF
  `NEEDED` check is the structurally Linux-only `size-budget` job and
  macOS has its Mach-O sibling; the PE/dumpbin analogue is marked as
  Phase 14 work in the job header and `docs/build-windows.md`.

Also this session: verified the macOS build after the Phase 1 refactor
on the remote Mac (Apple Silicon) — `cargo check --workspace`, clippy
`-D warnings`, and the full test suite all green (1389 passed, 0
failed), confirming the refactor was macOS-neutral as designed.

Next: Phase 3 — first successful Windows cross-compile via `cargo-xwin`.

## 2026-07-10 — Windows port Phase 1 complete: Linux-only trait refactor

Phase 1 of `plans/2026-05-26-windows-port-v1.md` (Tasks 1.1–1.8) is done —
a zero-behaviour-change refactor that puts a platform seam in front of
every subsystem that will later need a Windows sibling. No Windows code
landed; the Linux binary is byte-for-byte the same size.

- **Audit first paid off**: roughly half the tasks were already
  discharged by earlier work. The overlay backend table (1.2), the
  hotkey and audio cfg splits (1.4/1.5), and the installer dispatch
  (1.6 — realised during the macOS port as cfg-dispatched
  `crates/fono/src/install/{mod,linux,macos}.rs` with an `unsupported`
  fallback that already covers Windows) needed only verification and
  documentation, not refactoring.
- **Task 1.1 (`fono-tray`)**: the platform-neutral seam already existed
  (the provider traits + `MenuNode` tree in `lib.rs`), so instead of
  inventing a `TrayBackend` trait the ksni/D-Bus code moved verbatim
  from `lib.rs` into a new `crates/fono-tray/src/backend_linux.rs`
  behind `cfg(target_os = "linux")` — a future `backend_windows.rs`
  slots in beside it.
- **Task 1.2 (`fono-overlay`)**: TODO marker added in
  `crates/fono-overlay/src/backend.rs` where the Windows
  `Win32LayeredToolWindow` candidate row will go.
- **Task 1.3 (`fono-inject`)**: the Unix-socket focus cascade in
  `focus.rs` (sway/i3 IPC probe and its `std::os::unix::net` import)
  is now gated `cfg(target_os = "linux")`, with a trivial passthrough
  on other platforms; `detect_auto`'s X11 early-return got the same
  gate. This was one of the two exact spots where the full Windows
  build broke at the end of Phase 0.
- **Task 1.7 (`fono-update`)**: `asset_name_for` gained a Windows stub
  (`fono-vX.Y.Z-<arch>.exe`) and `desired_asset_prefix` short-circuits
  to the CPU prefix on Windows (CPU-only in v1, same as macOS), with a
  matching cfg-gated test.
- **Gate (1.8)**: fmt, clippy `-D warnings`, and the full test suite
  green (1423 tests passed, 0 failed); `release-slim` binary size delta
  measured against a stashed baseline: exactly 0 bytes (well within the
  ±5 KB budget).

Next: Phase 2 — add a non-blocking Windows row to the CI build matrix.

## 2026-07-06 — Windows port Phase 0 complete: remote dev environment live

Phase 0 of `plans/2026-05-26-windows-port-v1.md` (Tasks 0.1–0.9) executed
end-to-end against a real Windows 10 box (build 19045/22H2) reachable over
SSH, and gate-verified beyond the original scope. `docs/build-windows.md` is
the new authoritative reference; see it for full detail.

- **Toolchain installed and verified**: OpenSSH Server + key auth (user-side,
  ahead of the session), Visual Studio Build Tools 2022 ("Desktop development
  with C++": MSVC v14.44/v143, Windows 11 SDK 10.0.26100.0, CMake, verified
  via `vswhere`), Rust 1.88 (`x86_64-pc-windows-msvc` host, clippy+rustfmt),
  rsync via MSYS2 (`pacman -S rsync openssh` — current Git for Windows no
  longer bundles `rsync.exe`, unlike the plan's assumption), `cargo-xwin` +
  the `x86_64-pc-windows-msvc` target on the Linux side.
- **Three gotchas found and fixed that the original plan didn't call out**:
  (1) VS Build Tools does not bundle `libclang.dll`, but bindgen
  (`llama-cpp-sys-2`/`whisper-rs-sys`) needs it — installed standalone LLVM,
  set `LIBCLANG_PATH` system-wide; (2) the VS-bundled CMake isn't on `PATH`
  outside a Native Tools prompt — added its bin dir to the system `Path`
  explicitly; (3) the vendored `llama.cpp` submodule checkout exceeds the
  legacy 260-char `MAX_PATH` — fixed via `git config --global core.longpaths
  true` **and** `LongPathsEnabled=1` under
  `HKLM\SYSTEM\CurrentControlSet\Control\FileSystem` (git's setting alone
  isn't sufficient). All three were done entirely over SSH with no GUI
  session; only the VS Build Tools installer itself needed a human at the
  keyboard (SSH sessions carry a UAC-filtered token even for admin accounts,
  confirmed by a failed `SYSTEM`-context scheduled-task workaround, exit code
  87 both ways).
- **`scripts/win-remote.sh` added**, modeled on `scripts/mac-remote.sh`
  (`push`/`check`/`build`/`test`/`cargo`/`sh` over rsync+ssh, `FONO_WIN_HOST`
  never committed), with one Windows-specific addition: it resolves
  `ORT_LIB_LOCATION` on every push-based command by running
  `scripts/fetch-onnxruntime.sh` remotely through MSYS bash (a bare
  `cmd.exe` session lacks `curl`/`xz`/`sha256sum`) — cheap, idempotent,
  cached after the first run. Also documented a `cmd.exe` `set` quoting trap:
  `set VAR=value && next` bakes the space before `&&` into the value.
- **Went beyond plumbing to a real native build**: `fono-core`, including the
  `llama-local` feature (full MSBuild/cmake C++ compile of the embedded
  llama.cpp backend), builds cleanly natively on Windows. Along the way, hit
  and fixed a genuine cross-platform ABI bug in
  `crates/fono-core/src/brain_tap.rs`: `(*tensor).type_` is a bindgen alias
  for a C enum's underlying integer type, which is ABI-dependent — Itanium
  (Linux/macOS) picks `unsigned int` for an all-non-negative enum, the
  Microsoft ABI (Windows/MSVC) always uses `int` — so the same header
  produces `u32` on Linux and `i32` on Windows. Fixed by comparing through
  `i64` (new `ggml_type_is()` helper, `i64::from()` on both sides) instead of
  direct equality against a fixed-signedness constant. Verified: all 7
  `brain_tap` tests pass on Linux; fmt/clippy clean (the file's one
  pre-existing `cognitive_complexity` lint on an unrelated test function
  predates this change — confirmed via `git stash`, left alone).
- **Confirmed the Phase 1 boundary**: `cargo build/check -p fono` (the full
  binary, not just `fono-core`) fails exactly where the design plan's
  Phase 1 trait-split targets — `fono-ipc` unconditionally imports
  `tokio::net::{UnixListener, UnixStream}`, and `fono-inject::focus`
  unconditionally imports `std::os::unix::net::UnixStream` (the sway/i3 IPC
  probe, missing the `#[cfg(unix)]`/`target_os = "linux"` gate the file's
  sibling modules already use). This is the environment doing its job, not a
  setup gap — Phase 1 is the next real work item.

Gate: Phase 0 marked complete in the plan with per-task provenance notes;
`docs/build-windows.md` created. Next: Phase 1 (Linux-only trait refactor —
zero Windows code, zero risk to the Linux build).

## 2026-07-05 — Brain visualization Phase 2 complete: the Glass Cortex renderer

Phase 2 of `plans/2026-07-05-brain-visualization-v1.md` (Tasks 2.1–2.6)
executed and gates green. The new `cortex` overlay style renders one
continuous scene across the whole voice pipeline — listening (mic FFT on the
layer grid), thinking (real prefill sweep + TTFT breathing), answering
(per-token layer activity replayed in sync with TTS playback). Zero new
crates, no assets.

- **Style + wiring (Task 2.1):** `WaveformStyle::Cortex` threaded through
  config, tray menu, daemon style cycling, web settings, session ambient
  driver, MCP voice I/O, and the renderer dispatch.
- **Renderer (Tasks 2.2–2.3):** `fono-overlay/src/cortex.rs` — horizontal
  layer spine (one ring per real transformer layer, count from `n_layer()`),
  grazing camera with parallax drift, additive two-lobe glows accumulated in
  a downsampled emissive buffer with dirty-region tracking, block-skipping
  separable-bilinear composite, heat trace, uncertainty ribbon, HUD arcs
  (tok/s + KV fill).
- **Replay engine (Task 2.4):** `BrainKeyframe`s recorded during the
  generation burst replay time-stretched against the TTS playback clock —
  bead crest ≈ spoken word; keyframe interpolation keeps sparse sampling
  fluid.
- **Phase machine (Task 2.5):** listen → think → answer morphs; prefill
  sweep driven by real batch-decode progress published from both embedded
  paths; breathing loop covers the TTFT gap and any data-free stretch.
- **Validation (Task 2.6):** capture gate 0.955 % ≤ 1 % (default dense
  model, 35/35 layers; second GGUF with different `n_layer` also verified);
  frame gate via `examples/cortex_frame_bench.rs` — 1.9 ms mean at the
  640×240 max panel vs terrain baseline 1.6 ms (~4 ms envelope), 4.3 ms at
  2× HiDPI (4× the reference pixels). Size budget 21.31 MiB ≤ 25 MiB,
  NEEDED allowlist clean. Live sync-feel check on the desktop remains a
  user-run item.

Gate green: fmt, clippy `-D warnings`, workspace tests, size budget. Next:
Phase 3 (MoE extras — contingent on an MoE model landing) or Phase 4 ship
polish (docs, tray label, trace persistence hook).

## 2026-07-05 — Brain visualization Phase 1 complete: capture spike proves the < 1 % budget

New plan `plans/2026-07-05-brain-visualization-v1.md` (the "Glass Cortex"
overlay style — a truthful visualization of the local LLM's forward pass);
Phase 1 (Tasks 1.1–1.5) executed and gate-PASSED. Zero new crates.

- **`fono_core::brain_tap` (Tasks 1.1–1.2):** a `cb_eval` shim that writes the
  eval callback directly into the `llama_cpp_sys_2::llama_context_params`
  behind `llama_cpp_2`'s `LlamaContextParams` (one contained unsafe block, no
  crate patch). `BrainKeyframe` carries per-layer hidden-state norms (rotating
  `LAYER_STRIDE` residue classes to cap per-sample graph splits), MoE routed
  expert IDs + weights, and sampler-side top-token probability + entropy, into
  a bounded drop-oldest ring that never blocks the decode thread.
- **Tensor matching (Task 1.3):** name-pattern rules (`l_out-<i>`,
  `ffn_moe_topk-<i>`, `ffn_moe_weights-<i>`) verified against the vendored
  llama.cpp graph sources; the bench validates 35/35 nonzero layer norms on
  the shipped Gemma E2B dense model.
- **Overhead gate (Task 1.4):** `examples/brain_tap_bench.rs` + a
  `SampleGovernor` (per-token EMA cost model, auto-widening sample interval).
  The enforced gate is the governor's within-run sampled-vs-plain estimate
  (immune to the reference laptop's ±20 % thermal drift): **amortized
  0.89–0.94 % ≤ 1 % budget — PASS** across repeated runs; warm-machine
  wall-clock active median +0.13 %, dormant ≈ 0.
- **Wiring (Task 1.5):** both embedded paths (assistant + polish
  `llama_local.rs`) install the tap via a shared `decode_token_with_tap`
  helper, gated on new config `[overlay] brain_capture` (default off ⇒ null
  callback, no allocation, zero cost).

Gate green: fmt, clippy `-D warnings`, workspace tests. Next: Phase 2 — the
Glass Cortex renderer (new overlay style, replay engine synced to TTS).

## 2026-07-04 — macOS: extra pre-push verification + honest README/CHANGELOG wording

Before pushing the 12-phase macOS port, ran a broader check pass on
the bench release artefact and tightened the user-facing wording so
the port isn't oversold:

- Multi-language equivalence fixtures (en/ro/es/fr/zh) all transcribe
  correctly on the release (Metal) artefact.
- Ran the daemon as the console user (`apple`, not root) via
  `launchctl asuser`, the closest headless SSH can get to a real
  login: tray, overlay, and hotkeys all start cleanly, and
  `CGWindowListCopyWindowInfo` confirms the tray's `NSStatusItem` and
  the overlay's `NSPanel` are registered with WindowServer and
  `onscreen=true`.
- Honestly recorded a bench limitation: this Mac's remote-desktop
  access path doesn't composite menu-bar status items into
  `screencapture` output at all (the entire right side of the menu
  bar is blank, including the system clock) — so the tray icon has
  been confirmed present via API introspection but never seen by a
  human eye. Left open on the deferred-GUI checklist.
- README and CHANGELOG reworded: brief, whole-project framing (not a
  session recap), explicit that macOS was only tested on a headless
  remote Mac, and an explicit invitation for Apple Silicon users to
  try it and file an issue either way.

## 2026-07-04 — macOS Phase 12 complete: CI gating + size budget — the port plan is done

Final phase of `plans/2026-07-03-macos-port-v1.md`. All 12 phases are
now complete at the headless tier; only the deferred-GUI checklist
(`docs/build-macos.md`) remains, to be run by whoever first sits at a
physical Mac.

- **Gating CI (Task 12.1):** the `macos` job in `ci.yml` lost
  `continue-on-error` — a red darwin build now fails PRs like the
  Linux rows.
- **`size-budget-macos` job (Tasks 12.2 + 12.3):** darwin analogue of
  the Linux size gate. Builds the exact ship artefact (`release-slim`,
  `aarch64-apple-darwin`, `accel-metal`, pinned onnxruntime,
  `ORT_CXX_STDLIB=c++`) and asserts size ≤ 18 MiB (18 874 368 B) plus
  an exact 17-entry `LC_LOAD_DYLIB` allowlist (13 system frameworks +
  4 `/usr/lib` system libs). The assert script was run verbatim on the
  bench artefact before landing: 16 143 328 B (15.40 MiB), GATE-PASS,
  17 imports all allowlisted. One bench lesson encoded: the step must
  run under `shell: bash` (process substitution; `sh` rejects it).
- **ADR 0022 amended (2026-07-04):** macOS joins the budget matrix —
  enforced ≤ 18 MiB, hard cap ≤ 20 MiB, allowlist recorded; the CI row
  and the ADR live in lockstep.
- **ROADMAP.md (Task 12.4, adapted):** the "macOS and Windows" horizon
  entry now says macOS is code-complete on `main` and ships with the
  next release (self-signed `Fono.app`, not the originally sketched
  signed `.dmg`); it moves to Shipped at tag time per the release
  rule.
- Gate: Linux fmt / clippy -D warnings / 36 test suites green (no Rust
  changes); ci.yml round-trips YAML (5 jobs, `continue-on-error`
  absent).

## 2026-07-04 — macOS Phase 11 complete: release workflow ships the darwin asset

Phase 11 of `plans/2026-07-03-macos-port-v1.md`. The next `v*` tag will
publish the first macOS release asset automatically; every
workflow-side piece was dry-run on the bench.

- **`release.yml` row (Task 11.1):** `macos-15` runner joined the build
  matrix — single **Metal** variant (`--features accel-metal`, per the
  Phase 3 artefact decision), `aarch64-apple-darwin`, pinned
  onnxruntime via `scripts/fetch-onnxruntime.sh`, `ORT_CXX_STDLIB=c++`
  (cancels the workspace `[env]` Linux-ism). Linux-only steps (apt
  deps, ELF `NEEDED` gate, size budget) gated on `runner.os == 'Linux'`;
  a new Mach-O gate asserts every `LC_LOAD_DYLIB` import is a system
  framework or `/usr/lib` system library. Asset:
  `fono-vX.Y.Z-aarch64-apple-darwin` + `.sha256`, included in
  `SHA256SUMS` — exactly the name Phase 10's updater resolves (darwin
  matched before the bare `aarch64-*` arm so it never collides with
  the Linux arm asset).
- **Bench dry-run:** the exact step sequence produced a 15.40 MiB
  (16,143,328 B) artefact; the dylib gate passed verbatim (17 imports,
  all allowlisted, incl. Metal/MetalKit/AppKit); the artefact
  transcribed the English fixture correctly on Metal with
  `large-v3-turbo`.
- **Docs (Task 11.2):** README "other ways to install" gained the
  macOS row (download + `fono install`, honest headless-tested
  caveat); CHANGELOG `[Unreleased]` section describes the port in
  user-facing terms; `docs/build-macos.md` gained the Phase 11
  dry-run section.
- **Universal binary (Task 11.3):** stays deferred on the
  `x86_64-apple-darwin` onnxruntime pin; arm64-only until then
  (decision recorded in the plan).
- **Task 11.4 closed:** all three pieces of the zero-cost grant-once
  pipeline are now live — install-side signing (Phase 9), the update
  re-sign hook (Phase 10), and the unsigned-asset release plumbing
  (this phase). No secrets in CI.

Gates: Linux fmt / clippy `-D warnings` / workspace tests green
(workflow + docs only — no Rust changes); darwin release build + smoke
over SSH. Remaining: Phase 12 (promote the CI row to gating + macOS
size budget) and the deferred-GUI checklist.

## 2026-07-04 — macOS Phase 10 complete: `fono update` with grant-preserving re-sign

Phase 10 of `plans/2026-07-03-macos-port-v1.md` at the headless tier.
Zero new crates; Linux update flow untouched.

- **Asset naming (Task 10.1):** `fono-update` gained the
  `current_asset_name()` darwin arm — `fono-vX.Y.Z-aarch64-apple-darwin`,
  the single Metal variant per the Phase 3 artefact decision. No
  cpu/gpu split on macOS, so the GPU-upgrade suggestion machinery is
  Linux-only by construction; unit tests pin both facts.
- **Grant-preserving self-replacement (Task 10.2):** after the updater
  swaps the binary, both apply sites (CLI `fono update` and the
  tray-triggered daemon path) call `install::resign_after_update()`,
  which re-signs the enclosing `Fono.app` with the persistent
  `fono-local-signing` identity. Bench-proven over SSH: the swap
  breaks the bundle seal, the re-sign restores it, and
  `codesign -d -r-` shows the designated requirement byte-identical
  before/after — the one-time Accessibility grant survives every
  update. Bare-binary installs skip the hook; re-sign failure warns
  with the recovery path instead of failing the update.
- **Headless smoke:** `fono update --check` resolves the darwin asset
  name and truthfully reports that no release carries it yet (Phase 11
  publishes the artefact). End-to-end swap-and-relaunch against a real
  release is on the deferred checklist in `docs/build-macos.md`.
- **Gates:** Linux fmt / clippy `-D warnings` / 36 test suites green
  (Task 10.3); darwin clippy clean, fono + fono-update tests green.

Next: Phase 11 — release workflow artefact (`release.yml` `macos-15`
row, Metal variant), then Phase 12 (promote CI to gating).

## 2026-07-04 — macOS Phase 9 complete: install, autostart, permissions onboarding

Phase 9 of `plans/2026-07-03-macos-port-v1.md` at the headless tier,
plus the install-side half of Task 11.4 (the zero-cost grant-once
signing pipeline). Zero new crates.

- **Installer split (Task 9.1 + Windows plan Task 1.6 discharged):**
  `crates/fono/src/install.rs` became `install/{mod,linux,macos}.rs` —
  `mod.rs` re-exports the per-OS implementation, `linux.rs` is the old
  file byte-identical (moved via `git mv`), `macos.rs` is the new
  ~600-line darwin installer.
- **`fono install` on macOS (per-user, no sudo):** assembles
  `~/Applications/Fono.app` around the current binary (bundle id
  `org.fono.app`, `LSUIElement`, `NSMicrophoneUsageDescription`),
  creates the `fono-local-signing` self-signed cert once in a
  dedicated always-unlocked keychain, signs the bundle, writes the
  `org.fono.daemon` LaunchAgent (`RunAtLoad`, crash-only `KeepAlive`,
  Aqua-only), symlinks `/usr/local/bin/fono`, and tries
  `launchctl bootstrap gui/$UID` (degrades to "starts at next login"
  headless). **Grant-once proven on the bench:** the designated
  requirement (`identifier "org.fono.app" and certificate leaf`) is
  byte-identical across re-installs, so the Accessibility grant
  survives updates. Bench facts encoded in the code: headless
  `security add-trusted-cert` needs GUI auth but `codesign` doesn't
  need trust at all; `find-identity -v` hides untrusted certs, so the
  probe drops `-v`.
- **`fono uninstall` (Task 9.2):** boots the agent out, removes plist
  + bundle + symlink + cache; keeps config, history, and the signing
  keychain so a re-install reuses the same identity.
- **Permissions onboarding (Task 9.3):** new
  `fono-inject::permissions` module — `AXIsProcessTrusted` /
  `AXIsProcessTrustedWithOptions(prompt)` FFI (ApplicationServices,
  zero new crates). The daemon prompts once per install marker in a
  graphical session; `fono doctor` gained `Install:` and
  `Accessibility:` rows with the System Settings deep link.
- `fono-update::is_package_managed` now recognises Homebrew prefixes
  (`/opt/homebrew`, `/usr/local/Cellar`, …) so self-update won't fight
  brew, with tests pinning both path families.
- Gates: Linux fmt / clippy `-D warnings` / 36 test suites green;
  darwin clippy clean, 36 suites 0 failed; full
  install → doctor → uninstall round-trip smoke over SSH.

## 2026-07-04 — macOS main-thread pump made event-driven (GCD main queue)

Follow-up to Phases 7–8, answering "why is the overlay capped at
~10 fps?": the cap was an artefact of the pump's 100 ms polling
`NSTimer`, not anything fundamental. The pump now ships jobs through
libdispatch's main queue (`dispatch_async_f` — two symbols from
libSystem, zero new crates), which the `NSApplication` run loop drains
as jobs arrive. Delivery is event-driven, matching the Linux backends'
waker model:

- **Overlay:** blit jobs run within the run loop's next turnaround, so
  the panel repaints at the producers' cadence (≈20–30 fps level/FFT
  ticks) instead of 10 fps; the newest-wins mailbox now only coalesces
  when the main thread is genuinely busy (e.g. menu tracking).
- **Tray:** menu repaints land immediately after the 2 s poll diff
  instead of up to 100 ms later.
- **Code shrank:** the `FonoPump` NSObject subclass, its `NSTimer`,
  and the job channel are gone; `install_main_pump` is now just an
  atomic installed/exited flag pair gating `dispatch`, so headless /
  non-daemon degradation semantics are unchanged (verified over SSH).
- The deferred-GUI smoothness caveat in `docs/build-macos.md` and the
  plan's Task 8.1 note were updated accordingly.

## 2026-07-04 — macOS Phase 8 complete: NSPanel overlay backend

The recording indicator now has a native macOS surface; Phase 8 is
complete at the headless tier. Zero new crates (objc2* were already
darwin edges of the graph).

- **New `fono-overlay::backends::macos`:** a borderless,
  non-activating, click-through, always-on-top `NSPanel`
  (`NSStatusWindowLevel`, all Spaces, excluded from the window
  cycler, clear background, no shadow) blitted from the same software
  renderer every other backend uses. A `fono-overlay-mac` worker
  thread owns the `RendererState` + command channel (identical
  command handling to the winit backend) and renders ARGB frames;
  the AppKit main thread wraps them via `NSBitmapImageRep` →
  `NSImage` → `NSImageView` and positions the panel bottom-centred
  (Cocoa's bottom-left origin maps `BOTTOM_OFFSET` directly). Frames
  cross threads through a newest-wins single-slot mailbox, so a slow
  pump tick skips straight to the latest frame instead of queueing.
  Backing scale is probed from `NSScreen` at spawn and re-synced from
  panel truth on every blit (retina maps 1 buffer px : 1 device px).
- **Selector:** the macOS candidate table `[MacPanel, Noop]` and the
  `mac|macos|mac-panel|nspanel` env aliases (landed with the Phase 8
  prep) are now backed by a real spawn; `fono doctor` reports the
  `mac-panel` capability row.
- **Wiring without a dependency edge:** `fono-overlay` doesn't depend
  on `fono-tray`; `fono::main` installs the tray pump's
  `dispatch_main` into `backends::macos::set_main_thread_dispatcher`
  at daemon startup. Headless / non-daemon invocations never install
  a dispatcher, so `try_spawn` returns a clean `NotAvailable` and the
  selector falls to `noop` — verified over SSH (daemon log shows the
  documented fall-through; dictation unaffected).
- **Gates:** Linux fmt / clippy `-D warnings` (default +
  `real-window`) / 36 test suites green, `Cargo.lock` gains
  darwin-scoped edges only; darwin clippy clean, 36 suites 0 failed,
  `fono` builds, doctor + daemon smoke over SSH.
- **Deferred-GUI:** visible painting, click-through, focus/dock
  checks, retina sharpness, and smoothness (the pump's 100 ms timer
  caps the panel at ~10 fps) — recorded in `docs/build-macos.md`.

Next: Phase 9 — install/autostart (LaunchAgent) + permissions
onboarding, or Phase 2's CI row hardening; both headless-provable.

## 2026-07-04 — macOS Phase 7 complete: native menu-bar tray (NSStatusItem)

Task 7.3 lands the macOS renderer over the shared menu model; Phase 7
is complete at the headless tier. Zero new crates.

- **New `fono-tray::backend_macos`:** interprets the `MenuNode` tree
  into a native `NSMenu` (~40-line recursive renderer, the mirror of
  the ksni one) attached to an `NSStatusItem`. Clicks route through a
  target/action bridge — an objc2-defined class whose registry maps
  `NSMenuItem` tags back to `TrayAction`s, swapped atomically with the
  menu on every render. Icon is a runtime-rasterised circle tinted by
  FSM state (same `menu::state_color` palette as Linux — deliberately
  not a template image, the tint carries information); tooltip carries
  the state line.
- **AppKit main-thread pump:** on darwin, a daemon invocation in a
  graphical session installs a job channel, moves the daemon to a
  worker thread, and parks the real main thread in
  `NSApplication::run()` with the `Accessory` activation policy (no
  Dock icon, no Cmd+Tab entry) + a 100 ms `NSTimer` that drains boxed
  closures with a `MainThreadMarker`. The poll/diff loop stays on
  tokio at the same 2 s cadence as ksni; unchanged ticks ship nothing
  to the main thread. The same pump is the designated host for
  Phase 8's NSPanel overlay.
- **Headless degradation verified on the bench:** over SSH
  `is_graphical_session()` is false → no pump → the daemon logs
  `tray icon : skipped (headless: no graphical session)` and keeps
  running (STT warmup, mDNS, Wyoming all normal). Non-daemon
  subcommands never install the pump.
- ksni backend gated to Linux; `objc2`/`objc2-app-kit`/
  `objc2-foundation` became direct darwin deps of `fono-tray`
  (already in the graph via Phase 6 — `Cargo.lock` gains edges only).
- Gates: Linux fmt/clippy (default + `tray-backend`)/36 suites green,
  behaviour untouched; darwin clippy `-D warnings` clean, 36 suites
  0 failed, `fono` builds, daemon smoke over SSH.
- Next: Phase 8 — NSPanel overlay on the same pump; the visible tray
  itself is on the deferred-GUI checklist in `docs/build-macos.md`.

## 2026-07-04 — macOS Phase 7 Task 7.2: platform-neutral tray menu model

Tray decision executed (Option C, Task 7.1 recorded the same day): the
menu is now defined once, platform-neutrally, and backends interpret.

- **New `fono-tray::menu` module:** declarative `MenuNode` tree
  (`Item`/`Check`/`Menu`/`Separator`) + a single shared `build()` that
  is a faithful transcription of the old ~600-line ksni builder — all
  ~10 submenus (recent, STT with discovered Wyoming peers, polish,
  assistant, TTS, microphones, preferences with radio groups and
  wake-phrase info rows, servers, conditional update/GPU rows).
  Compiles and unit-tests on every OS with zero backend types.
- **ksni backend became a ~40-line recursive interpreter**
  (`render_nodes`): Item→StandardItem, Check→CheckmarkItem,
  Menu→SubMenu, action:None→disabled row. Behaviour byte-identical by
  construction; future menu edits happen in one place for all OSes.
  Windows plan Task 1.1 is thereby discharged early.
- **Snapshot tests pin the tree:** top-level structure list plus
  load-bearing details (active `●` markers, checkmark states, disabled
  sentinel handling, empty-state rows, truncation, language summary
  tiers). These run on Linux, macOS, and (later) Windows CI, so
  cross-platform menu parity is tested, not hoped for.
- Gates: Linux fmt/clippy (default + `tray-backend`)/36 suites green;
  darwin clippy `-D warnings` clean, menu tests green, `fono` builds.
- Next: Task 7.3 — the objc2 `NSStatusItem` renderer over the same
  model (needs the AppKit main-thread event pump, designed together
  with Phase 8's NSPanel overlay).

## 2026-07-03 — macOS Phase 6 complete: text injection + focus via enigo/AppKit

Sixth same-day session (`plans/2026-07-03-macos-port-v1.md`); Phase 6
is complete at the headless tier.

- **Task 6.1 (dep check):** `enigo`'s darwin backend rides on
  `core-graphics`/`icrate`/`objc2` — all already in `Cargo.lock`, so
  no new-to-project crates. The focus prober adds darwin-only edges to
  the already-present `objc2-app-kit`/`objc2-foundation` (plus
  app-kit's `libc` feature for `processIdentifier`); the Linux
  artefact's graph is untouched (target-scoped tables).
- **Task 6.2:** `enigo-backend` is now default on macOS (target table
  in `crates/fono/Cargo.toml`, mirroring Phase 4's cpal pattern), and
  `detect_auto` short-circuits to Enigo on darwin (display env vars
  carry no signal). Injection errors name **System Settings → Privacy
  & Security → Accessibility** and note the clipboard fallback;
  clipboard failure messages are platform-appropriate (NSPasteboard +
  `pbcopy` on macOS instead of the wl-copy/xclip Linux-isms), as is
  the `Injector::None` explanation.
- **Task 6.3 (focus):** new darwin branch in `focus.rs` asks
  `NSWorkspace.frontmostApplication` for the frontmost app's
  name/bundle-id/pid (no Accessibility permission needed for
  app-level focus). Classifier gains macOS terminal bundle ids
  (Terminal, iTerm2, Alacritty, kitty, WezTerm, Ghostty…) so the
  terminal-vs-GUI injection rules work there.
- **Task 6.4 / headless-gate answers recorded:** over headless SSH as
  root, `Enigo::new()` + `text()` **return Ok** (CGEventPost accepts
  events without a session — the Accessibility denial path is
  unobservable from SSH and moves to the deferred-GUI checklist);
  NSPasteboard/`pbcopy` **fail cleanly** (pasteboard daemon is
  per-login) with per-tool diagnostics; frontmost-app probe returns an
  empty `FocusInfo`, no error. `fono test-inject` gained `pbpaste`
  readback and a truthful macOS message when the pasteboard daemon is
  absent.
- **Task 6.5:** Linux regression — fmt/clippy (default and
  `enigo-backend`)/36 suites green; darwin clippy clean, 36 suites,
  0 failures.

Next: Phase 7 (menu-bar tray) — backend decision (`tray-icon` vs
objc2 shim) needs the size sign-off; headless tier is graceful
no-WindowServer degradation.

## 2026-07-03 — macOS Phase 5 complete: global hotkeys via Carbon, zero new deps

Fifth same-day session (`plans/2026-07-03-macos-port-v1.md`); Phase 5
is complete at the headless tier.

- **Task 5.1 (decision):** `global-hotkey` — and it turned out to be
  free: the Linux X11 listener is already built on that crate, so it
  was never a new dependency. Zero Linux size cost, `Cargo.lock`
  unchanged, and the Carbon backend (`RegisterEventHotKey`) needs **no
  TCC permission**, unlike a CGEventTap shim (Input Monitoring prompt,
  untestable headless). Trade-off recorded: Carbon swallows the
  registered key.
- **Task 5.2:** the generic `listener.rs` runs unmodified on macOS
  (the Linux-only `x11-dl`/`ashpd`/portal gating had already landed in
  the Phase 1 squash). This session: `detect.rs` short-circuits to the
  `global-hotkey` listener on darwin (display env vars carry no signal
  there), and `is_graphical_session()` gained a real macOS probe —
  `CGSessionCopyCurrentDictionary()` via raw framework FFI (two
  symbols, zero new crates) — so the daemon's headless gate is truthful
  on darwin instead of always-false.
- **Task 5.3:** Esc-to-cancel needed no port — the transient
  `EnableCancel`/`DisableCancel` registration goes through the same
  `GlobalHotKeyManager` seam.
- **Headless gate answer recorded:** Carbon registration **succeeds
  over headless SSH as root** — the probe example registered F7/F8/Esc
  and unregistered cleanly; a WindowServer session is needed only for
  event *delivery* (deferred-GUI). The daemon correctly detects the
  SSH session as non-graphical and skips the listener.
- **Task 5.4:** Linux regression — portal/X11 code untouched (only
  cfg-gated); fmt/clippy/36 suites green.
- Also folded in: `hwcheck.rs` per-OS refactor (the darwin
  statvfs/Mach probe code split into cleaner per-OS modules).
- Gates: Linux fmt/clippy/36 suites green, `Cargo.lock` unchanged;
  darwin clippy `-D warnings` clean + 36 suites, 0 failures.

Next: Phase 6 (text injection + focus detection) — enigo/CGEvent
behind the existing `enigo-backend` feature, clipboard fallback via
arboard, Accessibility-TCC denial path as the headless test subject.

## 2026-07-03 — macOS Phase 4 complete: CoreAudio capture/playback via cpal

Fourth same-day session (`plans/2026-07-03-macos-port-v1.md`); Phase 4
is complete at the headless tier.

- **Task 4.1:** `cpal-backend` is now default on macOS via a
  `[target.'cfg(target_os = "macos")'.dependencies]` table in
  `crates/fono/Cargo.toml` — target tables don't unify off-target, so
  Linux stays byte-identical and `Cargo.lock` is unchanged. Two code
  fixes this forced: (a) the cpal capture stream is `!Send` on macOS
  (CoreAudio handles are thread-affine) — the stream now lives on a
  dedicated keeper thread; (b) the 7 pre-existing clippy-debt lints in
  the cpal playback worker (status.md 2026-06-17) are fixed
  (`let…else`, `map_or_else`, one precedented `too_many_lines` allow).
- **Tasks 4.2/4.3 (headless tier):** the dev Mac Studio has **no mic
  hardware at all** (`system_profiler` lists only speakers), which
  exercises the no-device failure path: `fono record` errors cleanly,
  no hang. Capture errors and doctor's empty-inputs hint now name
  System Settings → Privacy & Security → Microphone on macOS instead
  of the Linux pactl/wpctl advice. Live-mic round-trip is on the
  deferred-GUI checklist (needs a Mac that has a microphone).
- **Task 4.4:** playback round-trip proven — `fono speak stream`
  played synthesized speech to "Mac Studio Speakers" through the cpal
  ring worker, rc=0.
- **Task 4.5:** new `AudioStack::CoreAudio` variant: doctor reports
  the real stack, input enumeration routes to cpal, and **auto-mute
  now works on macOS** (system output mute via `osascript`, round-trip
  verified headless).
- Locale test cleanup: the localectl fixture test now calls the real
  parser on every OS (it's compiled under `any(linux, test)`) instead
  of a hand-inlined emulation.
- Gates: Linux fmt/clippy (default + `cpal-backend`)/36 test suites
  green; darwin clippy `-D warnings` clean + 36 suites, 0 failures.

Next: Phase 5 (global hotkeys) — backend decision needs a size
sign-off before any new dependency lands.

## 2026-07-03 — macOS Phases 0–3 complete: tests green on darwin, headless smoke, CI row

Third same-day session (`plans/2026-07-03-macos-port-v1.md`); Phases 0,
1, 2 and 3 are now all complete.

- **Task 0.7:** `scripts/mac-remote.sh` (push/check/build/test/cargo/sh
  against the sandbox; host exclusively from `FONO_MAC_HOST`, no default)
  + `docs/build-macos.md` (build requirements, remote loop, sandbox
  layout, pinned platform paths, headless-smoke results, deferred-GUI
  checklist). Lesson learned: rsync's `.gitignore` dir-merge filter did
  **not** protect the remote `target/` from `--delete` — one push wiped
  the build cache and the pinned onnxruntime lib; the script now has an
  explicit `/target` exclude.
- **Task 1.4:** darwin workspace check is zero-warning — cfg-gates on
  cfg-shadowed Linux-only items in `fono-core` (locale), `fono-audio`
  (capture/playback), `fono-inject` (terminal). Linux clippy unchanged.
- **Task 1.5:** `cargo test --workspace --tests --lib` green on darwin
  (36 suites, 0 failures). The run caught a **latent FFI bug**: hwcheck's
  hand-rolled `struct statvfs` used the Linux all-u64 layout on every
  unix, but Darwin's block counts are u32 — garbage product, multiply
  overflow. Fixed with a per-OS layout + checked multiply. Also fixed
  `read_meminfo`/`physical_cores` stubs (doctor claimed "0 GB RAM,
  unsuitable" on the 64 GiB Mac): both now use Mach sysctls /
  `host_statistics64` via a macOS-only `libc` edge (crate already in
  every target's graph — net-zero binary size).
- **Task 3.3 (headless smoke):** the full daemon starts and idles
  headless; local TTS voices auto-download; `fono speak stream --out` +
  `fono transcribe` round-trip works; Wyoming server listens and
  advertises TTS + wake-word; doctor/history/hwprobe/use/voices all fine;
  `record` and `test-inject` degrade gracefully with actionable errors.
  **Risk 5 closed:** macOS uses the same XDG-style dotfile paths as Linux
  (`~/.config/fono` etc.) — no `~/Library` drift.
- **Phase 2:** non-blocking `macos-15` job added to `ci.yml`
  (`continue-on-error: true`, `ORT_CXX_STDLIB=c++`, check `-D warnings`
  + workspace tests — the exact commands proven green on the dev Mac).

Next: Phase 4 (cpal audio on macOS) — the first phase with a deferred-GUI
residue (mic TCC grant); its headless gate is compile + unit tests +
graceful no-permission degradation.

## 2026-07-03 — macOS release-slim binary builds and runs; Metal-only single-artefact decision

Same-day follow-up to the bootstrap session below — plan Phase 3 Tasks
3.1/3.2 done (`plans/2026-07-03-macos-port-v1.md`).

- **Link fix:** the workspace `[env] ORT_CXX_STDLIB="static:-bundle=stdc++"`
  (a Linux-GNU NEEDED-allowlist fix) leaks into darwin builds and makes
  `ort-sys` emit `-lstdc++`, which ld64 can't find. Cargo `[env]` can't be
  target-scoped, so darwin builds export `ORT_CXX_STDLIB=c++` in the
  environment (inherited env beats `[env]`); recorded for the future CI and
  release rows. No repo change needed for Linux.
- **Sizes (release-slim, default features, arm64):** CPU-only 15.14 MiB;
  `accel-metal` 15.79 MiB — **Metal costs only +0.65 MiB (+4.3 %)**. Both
  run; dylib imports are system frameworks + libSystem/libc++/libiconv/
  libobjc only; ad-hoc linker signature confirmed via `codesign -dv`.
- **Benchmarks (30 s fixture, `fono transcribe --no-polish --stt local`):**
  small q8_0 — CPU 1.51 s wall / 5.67 s user vs Metal 1.10 s / 0.17 s;
  large-v3-turbo q8_0 — CPU 9.25 s / 39.68 s vs Metal **2.12 s / 0.23 s**
  (4.3× faster, ~170× less CPU time). `fono models install` +
  `fono transcribe` worked first try on the Mac (partial Task 3.3 smoke).
- **Decision (user call, confirmed by the numbers): macOS ships one
  variant only — Metal** — no cpu/gpu split; ggml falls back to its CPU
  backend at runtime if Metal init fails. Eventual ship shape: a single
  universal (lipo) binary of that one variant, once the
  `x86_64-apple-darwin` onnxruntime pin exists. Recorded in the plan
  (artefact-shape decision + Tasks 11.1/11.3).

## 2026-07-03 — macOS port started: remote Mac bootstrapped, workspace checks green on darwin

Kicked off the macOS port against a remote Mac Studio (arm64, macOS
15.6, Xcode 26.1.1, SSH as root; address kept out of the repo — see
`FONO_MAC_HOST` in the plan). New plan:
`plans/2026-07-03-macos-port-v1.md` (mirrors the never-executed Windows
port plan's phasing; Phases 0–1 largely executed same-day).

- **Sandboxed remote dev env** — everything on the Mac lives under one
  directory (`/var/root/fono-dev`: rustup + cargo homes, shallow repo
  clone, standalone CMake 3.31.6, `env.sh`), so cleanup is a single
  `rm -rf`. No brew formulae, no system-wide installs. Rust 1.88 via
  `rust-toolchain.toml`.
- **onnxruntime for `aarch64-apple-darwin`** — the hosted pin is correct,
  but stock macOS lacks `xz`/`sha256sum`; provisioned the verified lib
  from the Linux host and gave `scripts/fetch-onnxruntime.sh` a
  `shasum -a 256` fallback so its fast path verifies on macOS. Landmine
  documented in the script header: bsdtar's raw-xz mode silently
  truncates the multi-stream `.xz` (34,240,800 of 34,326,760 bytes) —
  never use it as an xz substitute.
- **First darwin compile probe → only two front-line failures, both
  fixed:**
  1. `fono-core::notify` called `notify_rust::Notification::hint()`,
     which only exists on `cfg(all(unix, not(macos)))` — the
     macOS/Windows arm could never have compiled on either target.
     Urgency is now accepted and ignored there (no such concept in
     those backends).
  2. `fono-overlay`'s graphical backends (winit/softbuffer/smithay/
     wayland-*/rustix/libloading) are Linux display-server stacks
     pulled in by `real-window`; moved them to a
     `[target.'cfg(target_os = "linux")'.dependencies]` table and gated
     the backend modules + `try_spawn` dispatch on
     `all(feature, target_os = "linux")`. On macOS the selector offers
     only `noop` until a native NSPanel backend lands (plan Phase 8).
- **Result: `cargo check --workspace` green on `aarch64-apple-darwin`**
  — all 19 crates, default features, llama.cpp + whisper.cpp compiled
  by Xcode clang, `tts-local` linked against the pinned static
  onnxruntime. ~20 dead-code warnings from cfg-shadowed Linux-only
  helpers remain (plan Task 1.4); `cargo test` on darwin is Task 1.5.
- **Gate green on Linux:** `cargo fmt --check`, `clippy --workspace
  --all-targets -D warnings`, `cargo test --workspace --tests --lib`.
  Overlay/notify changes are Linux-behaviour-neutral by construction
  (target-table moves + cfg tightening only).

## 2026-07-03 — Personal vocabulary (deterministic correction) shipped

Implemented `plans/2026-07-03-correction-with-memory-v3.md` (supersedes the
v2 plan): a user-editable `~/.config/fono/vocabulary.toml` deterministically
rewrites mishearings in every dictation before the text reaches the cursor.

- **Architecture: correct the transcript, not the final text.** Pure
  `correction::apply(text, &table)` runs on the raw STT result at the two
  post-STT sites (batch + live), so one-shot inject, the v0.10 word-by-word
  streaming inject, clipboard fallback, history, and overlay all see
  corrected text for free. Belt-and-suspenders idempotent pass on the
  non-streamed `final_text`.
- **Engine** (`fono-core::correction`): whole-word/whole-phrase Unicode
  matching, longest-match-first, case-insensitive with canonical-cased
  output, idempotent by construction via two load-time checks (to/from
  overlap, duplicate from). Malformed file → logged error, empty table,
  no crash. No new crates; no new config keys (file presence is the
  switch; reloaded per dictation — no hot-reload IPC).
- **ADR 0037** locks the `vocabulary.toml` schema (`[[vocabulary]]`
  entries, `from` list → `to` string).
- **Surfaces:** `fono vocabulary add/remove/list` CLI; `fono doctor`
  line (path, entry count, parse status); vocabulary section in the
  browser settings page (`GET/PUT /api/vocabulary`, server-side
  validation through the same loader); `docs/configuration.md` section.
- **Tests:** exhaustive engine unit tests (substring safety, case
  variants, multi-word, idempotency, validation rejections, diacritics)
  plus pipeline integration tests covering {batch, live} × {polish
  on/off} and the local streaming-cleanup path.
- Seeded the first user entry: `phono → Fono` (round-trip verified via
  the CLI).
- Deferred (separate slices): `fono vocabulary suggest` history mining;
  voice "fix that" hotkey.
- **Gates green:** fmt, clippy `-D warnings`, workspace tests.

## 2026-07-02 — Web settings UI shipped (browser config screen, zero new crates)

Implemented the full plan `plans/2026-07-02-web-config-ui-v2.md` — a
browser-based settings page covering every user-relevant config option,
based on the approved search-first accordion design handoff:

- **Config simplification first:** removed `audio.sample_rate`,
  `interactive.mode`, and `interactive.quality_floor` (reserved keys with a
  single implemented value each; unknown keys in old files are simply
  ignored — no back-compat per maintainer). `audio.vad_backend` stays (the
  tray VAD toggle rides it). Docs updated (`docs/interactive.md`,
  `docs/configuration.md`).
- **New `[server.web]` block** (off by default, `127.0.0.1:10808`,
  optional `auth_token_ref`) mirroring `ServerLlm`.
- **`fono-net::web_settings`** — hand-rolled hyper server (ADR 0036
  pattern, zero new crates): embedded `index.html`/`app.css`/`app.js`
  via `include_str!`, `GET/PUT /api/config`, `GET /api/meta`,
  `PUT /api/secret/{NAME}` (write-only; values never echoed), bearer
  token + loopback-only peer guard, 1 MiB body cap.
- **Frontend** ported from the handoff: 9-section accordion with live
  summaries, dirty-diff unsaved bar, `/` search, hotkey capture,
  provider card grids, master-toggle greying, dark/light themes,
  schema-driven rendering in vanilla JS (~790 lines, no framework).
- **Coverage test** (`config_coverage_ui_or_allowlist`) walks every leaf
  of a fully-populated `Config` and asserts it's bound in `app.js` or on
  a justified `FILE_ONLY` allow-list — new config keys can't silently
  miss the UI.
- **Entry points:** tray **Settings…** entry lazy-starts the listener
  (persisting `enabled = true`) and opens the browser;
  `fono config web` enables the flag, probes the port, and opens or
  prints restart guidance. Daemon saves route through
  `Config::save → orchestrator reload → wake reload` (same as `fono use`).
- **Gates green:** fmt, clippy `-D warnings`, workspace tests
  (incl. 5 new web_settings tests), size budget 21.22 MiB / 25 MiB.
- Deferred: PUT etag/version guard for concurrent tray-vs-browser edits
  (noted in the plan; disk re-read per request bounds the risk).

## 2026-07-02 — Roadmap audit + tidy (two stale horizon items cleared)

Audited `ROADMAP.md` against the tree; two "On the horizon" items were already
done and have been reconciled (docs-only change, no code):

- **Better Wayland hotkeys → Shipped (v0.8.1).** The
  `org.freedesktop.portal.GlobalShortcuts` backend
  (`crates/fono-hotkey/src/portal.rs`) has auto-registered the dictation +
  assistant hotkeys since commit `a3c7fe3` (2026-05-19; first tag v0.8.1, per
  `git tag --contains`). Removed from the horizon table + section; added a
  v0.8.1-badged Shipped entry.
- **Shared ggml size-reclaim spike → Shipped list as a closed investigation.**
  Outcome (deferred, reclaim ≈ 0 MiB, 2026-06-24) was already recorded but the
  item still sat under On the horizon; moved to the Shipped list with an
  `investigation` badge, section + table cell removed.
- **Hover-context injection** gained a real body section (the table's anchor
  was dangling): notes the focused-window half shipped in v0.8.2 and scopes the
  remainder as pointer-hover context.
- **Local REST API** section now notes the v0.13.0 `hyper` listener in
  `fono-net` is the HTTP foundation; remaining work is exposing IPC verbs.

Verified genuinely-unbuilt items (no vocabulary CLI/pass, no translate stage,
no OpenAI Realtime client, no AEC talk-over, no MCP client / voice actions,
no Modelship, no LLM-server model router). Next-work shortlist discussed with
the maintainer: personal vocabulary (highest daily value, plan
`plans/2026-06-03-correction-with-memory-v2.md`), voice actions via MCP
(biggest capability jump, plan `plans/2026-05-22-voice-actions-via-mcp-v1.md`),
multi-provider LLM-server routing, AEC barge-in.

## 2026-07-02 — v0.13.1 size-lever post-mortem: CI artefacts did NOT shrink; fixes landed

Inspected the released v0.13.1 binaries against v0.13.0 and found the
morning's two size levers changed nothing in what CI ships (cpu x86_64
byte-count identical at 23,192,712 B; gpu +4,096 B). Two distinct causes,
both now corrected:

- **Lever 1 (`--exclude-libs,ALL` + `--hash-style=gnu`) is inert on the
  release runners.** The released v0.13.0 binary *already* has exactly
  1 exported symbol and gnu-hash only — the ~1,011-export / SysV-hash
  bloat the lever removed is a NimbleX dev-box toolchain artefact, not a
  CI one. The flags stay (they pin the invariant across host toolchains
  and make local measurements track CI), but the docs/comments now state
  the scope honestly: local-only ~0.9 MiB, shipped artefacts unchanged
  (`.cargo/config.toml`, `docs/binary-size.md` §6).
- **Lever 2 (glslc `spirv-opt --strip-debug` shim) never executed.** The
  Swatinem rust-cache key was unchanged, so the release job reused the
  pre-shim ggml-vulkan shader objects and the generator never re-ran.
  Bumped the cache-key suffix to `-portable-shaderstrip1` in
  `release.yml` so the next release does one cold rebuild through the
  shim; the measured −0.75 MiB gpu shrink should materialise then. Rule
  recorded in the cache-key comment: bump the suffix whenever the shader
  toolchain changes.
- **Follow-up owed:** the 0.13.1 changelog's "smaller binaries" claim is
  only true for the anyhow/aarch64 noise (−12 KiB) until the next
  release actually re-generates the shaders; the next release notes
  should carry the real gpu number.

## 2026-07-02 — GPU (Vulkan) binary size audit + two zero-capability-loss levers

Audited the `gpu` (accel-vulkan) release-slim x86_64 artefact
(60,961,144 B = 58.14 MiB baseline) and shipped the two levers that cost
nothing in features or hardware support (full findings in
`docs/binary-size.md`, "The `gpu` (Vulkan) variant" section):

- **Composition:** 36.55 MB is 1,551 embedded SPIR-V shader blobs,
  18.08 MB `.text`, ~2.9 MB tables. The whisper/llama duplicate
  ggml-vulkan builds already dedup perfectly at link time (0 duplicate
  symbols, 0 byte-identical blobs); shader `-O` is already on except the
  upstream coopmat/bf16/rope driver-bug exclusions.
- **Lever 1 (wired, `.cargo/config.toml`):** `-Wl,--exclude-libs,ALL` +
  `-Wl,--hash-style=gnu` — hides ~1,011 leaked static-archive exports
  (985 libstdc++) and drops the legacy SysV hash. Measured
  **−934,344 B (−0.89 MiB)** on `gpu`; NEEDED allowlist verified intact
  on both variants, binary smoke-tested. CPU artefact shrinks similarly.
- **Lever 2 (wired, `release.yml` GPU row):** `glslc` shim runs
  `spirv-opt --strip-debug` (semantics-neutral) on every generated
  blob — measured **−785,052 B (−0.75 MiB)** across the surviving set;
  added `spirv-tools` to the GPU row's apt deps.
- **Measured but not adopted:** GPU-only `opt-level="z"` (−1.17 MiB,
  same vectorisation objection as on `cpu`); RELR (needs glibc ≥ 2.36,
  above the 2.35 floor). **Future big fish:** compress the SPIR-V
  payload (needs ggml patch + decompressor dep — flag first).
- **Gate green:** `cargo fmt --check`, `clippy --workspace --all-targets
  -D warnings`, `cargo test --workspace --tests --lib` (1,353 passed),
  and `./tests/check.sh --size-budget` with the new flags =
  **20.92 MiB / 25 MiB** (down from 21.82 MiB on 2026-07-01, −0.9 MiB
  from lever 1; four-entry NEEDED clean).

## 2026-07-01 — LLM server access log (one line per request)

Added a single human-readable access line per LLM-server request, emitted
at `debug` level on the existing `fono::llm::server` tracing target (so it
inherits the daemon's `FONO_LOG` filtering — no new machinery, no new
dependency, ~0.01 MiB). Content is **never** logged (metadata only), same
privacy posture as the owner-only history DB.

- **New `fono-net::llm_server::access_log` module:** `ReqLog` (built at
  dispatch) finalises non-streaming requests via `finish`; streaming
  requests hand a `StreamLog` to the body task via `defer`, which records
  time-to-first-token + an output-token count (adapter path only) and emits
  when the stream drains. Includes a compact `User-Agent` classifier
  (`compact_ua` — friendly names for Home Assistant / Open WebUI / ollama /
  OpenAI / httpx / curl etc., else first product token capped) and a
  `provider_label` for the `proxy→<provider>` mode tag.
- **Line shape:** `<surface>/<op> <status>  <mode>  <model>  [stream]
  ttft=… total=…  <N>tok @<tps>/s  via <ua>  [<peer>]`. `mode` is
  `proxy→<provider>` / `adapt` / `·`; `ttft` + token cluster appear only
  when available (adapter deltas ≈ tokens; the proxy byte-relay omits the
  count); `via <ua>` always shown (disambiguates clients on a shared local
  port); `<peer>` shown only for non-loopback callers.
- **Wiring:** peer `SocketAddr` threaded from `serve_conn` into `route()`;
  UA captured + timing started at dispatch; the OpenAI/Ollama handlers set
  mode+model and the streaming bodies (`messages.rs` SSE/NDJSON + `proxy.rs`
  relay) emit the completion line.
- **Tests:** 6 unit tests (UA classifier known/blank/fallback, provider
  label, full streaming line shape, minimal non-stream line with peer shown).
- **Gate green:** `cargo fmt --check`, `clippy --workspace --all-targets -D
  warnings`, `cargo test --workspace` all pass. **Size budget:**
  `./tests/check.sh --size-budget` = **21.82 MiB / 25 MiB** (glibc `cpu`,
  four-entry NEEDED clean).
- **Roadmap:** added **Multi-provider routing for the local LLM server** to
  *On the horizon* in `ROADMAP.md` (model-name router across all keyed
  providers, default-fallback model, allowlist).

## 2026-07-01 — LLM server cloud pass-through proxy — Phase 1 shipped

Executed Phase 1 (tasks 1.1–1.7) of
`plans/2026-07-01-local-llm-server-cloud-proxy-v1.md`. When the served
`[assistant]` backend is an **OpenAI-compatible cloud** provider (OpenAI,
Gemini, Groq, Cerebras, OpenRouter), the LLM server's OpenAI surface now
forwards the client's `/v1/chat/completions` request **verbatim** to the
provider (injecting the stored key) and streams the response back
unchanged — full model/tool/vision/parameter fidelity for free. Non-cloud
backends (embedded llama.cpp, Anthropic, and the whole Ollama-native
surface) keep using the built-in adapter. Recorded as **ADR 0036 decision 9**.

- **`fono-assistant`:** centralised the per-provider `/chat/completions`
  URLs into `chat_endpoint(backend)` (always-compiled in `factory.rs`; the
  `OpenAiCompatChat` constructors now consume them) — the single "is this
  backend proxyable?" decision point. Added `CloudUpstream` +
  `cloud_chat_upstream(cfg, override, secrets)` which reuses `resolve_cloud`'s
  key/model resolution; a Gemini-Live primary resolves through the
  flash-lite fallback to Gemini's compat endpoint (still proxied, no local
  client built).
- **`fono-net::llm_server::proxy`:** `forward_chat` (SSE + JSON relay,
  status/content-type preserved, default `model` injected only when the
  client omits it, key injected outbound) and `forward_models` (surfaces the
  provider's `/models` catalogue). Wired via a parallel `UpstreamProvider`
  closure alongside `AssistantProvider` (simpler + non-breaking vs. the
  plan's `ServeTarget` enum sketch); the OpenAI handlers check the upstream
  first, else adapt. `reqwest` added as a `fono-net` dep (already in graph →
  net-zero).
- **Orchestrator:** new `server_upstream` slot + `server_upstream_snapshot()`,
  computed in `new` and recomputed on `reload` alongside the assistant
  fallback, so a backend swap re-targets the proxy without restarting the
  listener.
- **Diagnostics:** `fono doctor` LLM line now states whether the OpenAI
  surface is proxied to the provider (full fidelity) or served via the local
  adapter.
- **Config:** `[server.llm]` **unchanged** — pass-through is automatic and
  the client's requested `model` is honoured (server `model` is only the
  omitted-field default). No new knobs.
- **Docs:** `docs/configuration.md` (cloud pass-through subsection + open-relay
  security note), `docs/home-assistant.md` (cloud tool-calling via the OpenAI
  surface today; Ollama translate-proxy is Phase 2), ADR 0036 decision 9.
- **Tests:** 5 new `fono-assistant` unit tests (`chat_endpoint` proxyable map;
  `cloud_chat_upstream` for openai/gemini-live-fallback/override/anthropic/
  disabled) + 3 new integration tests in `tests/llm_server_round_trip.rs`
  against a mock upstream hyper server (client model forwarded verbatim + key
  injected; default model injected when omitted; `/v1/models` surfaces the
  upstream catalogue). Round-trip test now 13 cases.
- **Gate green:** `cargo fmt --check`, `clippy --workspace --all-targets -D
  warnings`, `cargo test --workspace` all pass. **Size budget:**
  `./tests/check.sh --size-budget` = **21.81 MiB / 25 MiB** (glibc `cpu`,
  four-entry NEEDED clean) — ~0.02 MiB growth, as projected (reqwest/hyper/
  serde already present).
- **Phase 2 (deferred):** optional `model_allowlist` for exposed instances,
  and the Ollama-surface translate-proxy (Ollama↔OpenAI incl. `tools`) that
  unlocks Home Assistant device control against a cloud model.

## 2026-07-01 — Local LLM server (OpenAI + Ollama API) — Phase 1 MVP shipped

Executed Phase 1 of `plans/2026-07-01-local-llm-openai-ollama-server-v1.md`
(all of tasks 1.1–1.10). Fono can now serve its active `Arc<dyn Assistant>`
(embedded llama.cpp or a cloud backend) over an HTTP API that is both
**OpenAI-compatible** and **Ollama-native**, from one listener. Decision and
rationale recorded in **ADR 0036**.

- **Transport: raw `hyper 1.x`, no axum** (ADR 0036). `hyper`/`hyper-util`/
  `http-body-util`/`bytes` are already in the graph via `reqwest`'s client
  stack, so enabling hyper's `server`+`http1` features adds **no new crate**.
  New `fono-net` feature `llm-server` (in default set); `fono-assistant`
  added as a `fono-net` dep (net-zero — already in the binary graph).
- **New module `fono-net::llm_server`** (`mod.rs` server/lifecycle/router/auth,
  `messages.rs` shared message→`AssistantContext` split + reply-driver +
  streaming-body builder, `openai.rs`, `ollama.rs`). Endpoints:
  - OpenAI: `GET /v1/models`, `POST /v1/chat/completions` (SSE stream +
    single JSON).
  - Ollama: `GET /api/tags`, `POST /api/chat` (NDJSON stream + single JSON),
    `GET /api/version`.
  Both drive the one `Assistant::reply_stream`; a per-request
  `AssistantProvider` closure tracks `Reload`-driven backend swaps without
  restarting the listener.
- **Config `[server.llm]`** (`ServerLlm`): off by default, `127.0.0.1` bind,
  **port 11434** (Ollama's — drop-in for HA/Ollama clients), optional
  `auth_token_ref` bearer. Mirrors `[server.wyoming]`.
- **Daemon wiring:** `LlmControl`/`LlmRuntime` (hot-reloadable, mirrors
  `WyomingControl`) with `reconcile`/`is_running`, startup spawn (held for
  the daemon lifetime), `orchestrator.assistant_snapshot()` accessor, mDNS
  `_ollama._tcp` advert (new `PeerKind::Ollama`), `fono doctor` LLM-server
  line. **Tray toggle:** the unified "Servers" submenu gets a "Local LLM
  server (OpenAI + Ollama API)" checkmark (`TrayAction::ToggleLlmServer`)
  that flips `[server.llm].enabled` and hot-reloads the listener in place —
  no daemon restart, same as the Wyoming toggle. Backend swaps stay hot via
  the provider closure.
- **Tests:** offline unit tests for both wire encoders (SSE `[DONE]`, NDJSON
  `done:true`, message split, model/tags shapes) + a `reqwest` round-trip
  integration test (`tests/llm_server_round_trip.rs`, 10 cases: models/tags,
  chat stream+non-stream for both formats, 400/404/401). `[server.llm]`
  config serde round-trip test.
- **Gate green:** `cargo fmt --check`, `clippy --workspace --all-targets -D
  warnings`, `cargo test --workspace` all pass. **Size budget:**
  `./tests/check.sh --size-budget` = **21.79 MiB / 25 MiB** (`release-slim`
  glibc `cpu`, four-entry NEEDED clean) — comfortably inside budget.
- **Realtime backends fall back to a same-provider text sibling.** The LLM
  server exposes a *text* chat-completions API and can't front a *realtime*
  speech-to-speech backend directly. Instead of skipping, it now serves the
  same provider's default staged **text** model (Gemini Live → the catalogue
  `text_model`, `gemini-flash-lite-latest`), reusing the same API key — so a
  user keeps Gemini Live for F8 voice *and* gets a fast/cheap/smart text model
  on the API with zero config. Built in the orchestrator
  (`fono_assistant::build_server_assistant_override` → new
  `server_assistant_extra` slot; `server_assistant_snapshot()` prefers it,
  else reuses the primary staged assistant) and rebuilt on reload. An optional
  `[server.llm].model` override pins a specific staged model, winning over both
  the primary and the fallback. `/v1/models`, `/api/tags`, `fono doctor`, and
  the tray notification all report the model actually served. Rejected reusing
  the `[polish]` cleanup model (wrong trait, typically too small for chat/tools).
  Unit tests in `fono-assistant::factory` cover the resolver + model-name paths;
  ADR 0036 updated (decision 8).
- **Phase 2 (deferred):** tool/function-calling passthrough for the Home
  Assistant device-control path (HA emits tool calls, HA executes them),
  gated on whether that's a near-term target.

## 2026-06-24 — Shared-ggml size-reclaim spike → DEFER (reclaim ≈ 0 MiB)

Executed `plans/2026-06-23-shared-ggml-size-reclaim-spike-v1.md`. Outcome:
**defer the source-level shared-ggml dedup; keep the ADR 0018 link trick
as steady state.**

- **Phase A (re-baseline).** whisper-rs-sys 0.15.0 vendors whisper.cpp
  **v1.8.3**; llama-cpp-sys-2 0.1.150's bundled ggml is the newer superset
  (`ggml.h` 107927 B vs 102112 B). `struct ggml_tensor` is **byte-identical**
  and all `GGML_MAX_*` match; `GGML_TYPE_COUNT` 40→42 is tail-appended
  (safe). **Hazard:** `enum ggml_op` has a mid-enum insertion
  (`GGML_OP_GATED_DELTA_NET` before `GGML_OP_UNARY`) shifting later op
  values by +1 — already latent in today's mixed-survivor link, smoke-test
  gated. **A3:** llama-cpp-sys-2 0.1.150 now ships a `system-ggml`
  feature (`LLAMA_USE_SYSTEM_GGML`), new since the 2026-05-31 spike;
  whisper-rs-sys still has no knob. **A4:** whisper-rs GitHub is an
  archived mirror (live repo on Codeberg); issue #212 "Add USE_SYSTEM_GGML"
  is open + unimplemented. The dedup is asymmetric — only the whisper side
  needs forking.
- **Phase B (measure).** Canonical `release-slim` `linux-gnu` `cpu` build:
  **26.60 MiB**, four-entry `NEEDED`. A non-stripped relink shows
  `ggml_init` defined **once**, **zero** duplicated ggml globals (561
  distinct `ggml_` text symbols, each once); the only duplicated locals are
  C++ template clones from onnxruntime/STL. **Realised duplicated-ggml
  reclaim ≈ 0 MiB**, not ~7 MiB — `-ffunction-sections`/`-fdata-sections` +
  `--gc-sections` already collect the loser copy. Risk #2 materialised.
- **Decision (D).** Defer. A source-level shared ggml buys no binary size;
  only build time (ggml compiled twice), which the size budget doesn't
  count. Front-runner if ever revisited: upstream `system-ggml` (llama done;
  whisper Codeberg PR), triggered by correctness/build-time, not size.
- **Docs reconciled:** plan (findings + decision), `docs/binary-size.md` §4,
  ADR 0022 (amendment + "~7 MiB superseded"), ADR 0018 (steady-state
  amendment), `ROADMAP.md`. No code changes; link trick unchanged.

## 2026-06-24 — Wake reliability fixes + Wyoming wake parity

Two-part session. **Part 1 — wake detection reliability.** openWakeWord
detection was firing only intermittently. Root causes found and fixed, in
order: (1) capture f32 was fed to the melspectrogram at ±1.0 instead of the
int16 ±32768 scale the graph expects (~90 dB too quiet); (2) each 1280-sample
hop was fed to the melspec in isolation, missing openWakeWord's 480-sample
streaming lookback (5 frames/hop instead of 8, de-aligning the mel→embedding
rings); (3) the streaming buffers were not primed, so every post-session mic
re-open had a ~2 s dead zone; (4) `vad_pregate` was a pre-melspec frame-skipper
that broke streaming continuity — first reworked into an output gate, then
**removed entirely** (no backward-compat) once it was clear that for a streaming
model the gate can only ever tie or lose against no-gate while saving no CPU.
Also fixed two orchestration bugs surfaced along the way: a synchronous `armed`
fire-gate to stop repeated wake phrases stacking sessions, and tearing down the
batch `assistant_capture` slot on assistant stop (an orphaned silence-watch was
emitting a phantom `AssistantPressed` ~3 s later, causing stacked sessions and a
missing overlay). Scores now hit 0.8–0.9 and fire reliably.

**Part 2 — Wyoming wake parity (Option B).** Made openWakeWord serve over the
Wyoming server exactly like STT and TTS: automatic and capability-gated, with no
separate switch. `serve_wake` and the mDNS `wake` cap are now gated on
`wake::detection_available()` (the `wakeword-onnx` feature being compiled in —
a fetchable default model always exists), independent of the local always-on
listener `[wakeword].enabled`. A fresh install with no `[[wakeword.phrases]]`
serves the runtime default model via `effective_wake_config`; the daemon
background-fetches the model `.ort` files when serving even if the local
listener is off. The Wyoming server binds a per-connection local detector, so
audio stays on the machine. `DEFAULT_WAKE_MODEL = "hey_jarvis"` as a documented
stopgap until the clean-licence `hey_fono` artifact is trained/pinned (SHA-pin
guard test added). `[wakeword].wyoming` is demoted to **client-only** (the
opt-in, privacy-breaking direction); `WakeWyoming::is_server` removed. Tray
label now reads "Wyoming server (STT + TTS + wake)"; `fono doctor` reports
automatic wake serving + the client-direction privacy warning; configuration /
home-assistant / providers docs updated. Gate green: fmt, clippy
(`interactive,wakeword-onnx`), workspace + featured tests.

## 2026-06-22 — Realtime live conversation mode

Delivered tap-to-converse live mode for realtime providers (Gemini Live),
implementing `plans/2026-06-22-realtime-live-conversation-mode-v4.md`. F8 now
has two interaction modes:

- **Hold = push-to-talk** (preserved): buffer the held utterance, open the
  session on release, play the full reply to completion, then close. Pinned by
  regression tests so the live work can't silently change it.
- **Tap = live conversation**: lazily opens one persistent full-duplex session
  on demand (never at startup), streams the mic continuously, and runs many
  turns over the one socket until you leave (second tap / Escape) or it
  auto-closes. Server-side VAD owns the turn boundaries.

Behaviour:

- **Mute-while-speaking baseline.** Without acoustic echo cancellation the open
  mic re-captures the model's own audio and self-interrupts, so live mode gates
  the mic while the model holds the floor — reliable hands-free multi-turn
  conversation on any host. True talk-over barge-in needs AEC and is deferred to
  `ROADMAP.md`.
- **Floor-ownership overlay + real audio visualisation.** The overlay walks the
  existing palette — green (you) → amber (model formulating) → blue (model
  speaking) → green — and the configured waveform style animates from real audio
  in **both** directions, fed at realtime pace so reply bursts don't race ahead
  of playback.
- **Two complementary auto-closes.** Trailing local silence
  (`auto_stop_silence_ms`, reusing the dictation silence-watch + Pondering
  animation) and a model-driven `end_conversation` tool call; a
  `max_session_secs` cap is the backstop. Graceful ends are silent, unexpected
  ends notify; one INFO line on open and one on close (reason / turns /
  open-secs).

Plumbing: a `RealtimeMode { PushToTalk, FullDuplex }` seam on
`RealtimeAssistant::open_session`; a persistent `LiveSessionHandle` in
`AssistantSessionState`; an FSM tap/hold gesture split gated by an
`assistant_live_available` flag; a `[assistant.realtime]` config block. Kept
provider-agnostic at the trait/catalogue layer (OpenAI Realtime client still
planned). Realtime also no longer prewarms at startup — the dead prewarm
scaffolding (warmup wiring + `GeminiLive::prewarm` + trait method) was removed in
favour of strictly on-demand connect; a `## [Unreleased]` CHANGELOG section
records that removal. `crates/fono/examples/smoke_realtime_live.rs` is a
standalone live harness for exercising the realtime client without the daemon.

Verified iteratively against live Gemini (headphones) during development. Gate
green throughout: fmt, clippy (`--features realtime` + default-feature
staged-path guardrail), `cargo test --workspace --lib --tests --features
realtime` (new FSM / setup-JSON / reader / live-pump / config tests). No
dependency changes. Not committed (holding per instruction); the AEC talk-over
barge-in upgrade is tracked on the roadmap.

## 2026-06-19 — 0.11.0 size-gate release fix

Follow-up for the 0.11.0 release CI failure: the x86_64 CPU artefact was
28,033,384 B against the old 26 MiB gate (27,262,976 B). Investigation found
the growth was mostly executable code from realtime/provider work, plus
measurable unwind/frame metadata and llama/OpenMP contribution — not bundled
models or assets.

Fix kept all shipped features and OpenMP enabled. `release-slim` now disables
unused Rust/native unwind-table emission while keeping C++ exceptions intact,
and the strict CPU budget is raised to 27 MiB (28,311,552 B), still below the
ADR 0022 32 MiB CPU cap. Local x86_64 `release-slim` after the patch measured
27,398,344 B, leaving 913,208 B headroom under the new gate. Findings are
recorded in `docs/binary-size.md`.

## 2026-06-18 — Release 0.11.0

Cut the **0.11.0** release. Workspace version bumped `0.10.0 → 0.11.0`;
`CHANGELOG.md` `[Unreleased]` promoted to `## [0.11.0] — 2026-06-18` with the
full feature set (realtime Gemini Live assistant, single-key Gemini provider,
gapless cloud TTS, universal voice autodiscovery, per-program voices,
ElevenLabs + Speechmatics backends, two male English Kokoro voices, readable
turn traces, richer MCP logs) plus a `### Fixed` section (thinking-state
barge-in, Gemini Live prewarm, Kokoro operator-set load failure, HTTP-402
notification, 3-letter language-code normalisation). `ROADMAP.md` updated: the
realtime-voice-assistant item moved from *On the horizon* into *Shipped* under
v0.11.0, the recently-shipped badge list gained v0.11.0.

Final WIP folded into the release commit:

- **Gemini Live prewarm.** `GeminiLive` now implements `prewarm` — warms DNS +
  TCP + TLS + the WebSocket upgrade off the hot path, opening and immediately
  closing the upgrade connection without a setup message (no model turn, no
  quota). It was the only voice client missing the cheap-probe prewarm every
  STT/TTS client already has.
- **Atomic barge-in restart.** New `HotkeyEvent::RestartAssistant`: a re-press
  of the assistant hotkey while a reply is *thinking or speaking* stops the
  in-flight reply and starts a fresh recording in one step, history preserved.
  Replaces the old `StopAssistantPlayback` + `StartAssistant` pair, whose
  `ProcessingDone` raced the new `AssistantRecording` state back to `Idle`.
  Now also covers the thinking state, not just speaking.

Docs hygiene: scrubbed stale **F9 / F10** references from active code and
docs (the FSM/parse comments, the parser doc example and test, the
troubleshooting trace-tag table). Historical release records that narrate the
migration *away* from F9/F10 (the v0.7.1 / v0.2.0 / v0.1.0 CHANGELOG and
ROADMAP entries, the Debian changelog, archived `plans/closed/`, and earlier
status-log sessions) were left intact as dated records.

## 2026-06-18 — Realtime: screen vision on the Gemini Live path

Second half of the maintainer's request (the first half — staged Gemini
`fono_screen` — already worked: `build_gemini` builds an `OpenAiCompatChat`
whose `reply_stream` gates the screen tool on `prefer_vision &&
screen_capture.is_some()`, backend-agnostic). The realtime Live path,
however, shipped tools-less under Path B and hardcoded `screen_capture: None,
prefer_vision: false`, so the `open_session` vision frame never fired.

Wired the screenshot through to the Live session:

- `RealtimeTurnInputs` gains `prefer_vision: bool` + `screen_capture_fn:
  Option<ScreenCaptureFn>`, mirroring the staged `AssistantTurnInputs`.
- `run_realtime_turn` now populates `ctx.screen_capture` / `ctx.prefer_vision`
  from those inputs instead of the hardcoded `None`/`false`.
- The `session.rs` realtime branch builds the same `GrabberProbe`-based
  capture closure as the staged branch (gated on `prefer_vision &&
  backend_is_vision_capable`) and threads it in.
- `open_session` (already present from the prior increment) grabs the focused
  window via the closure, encodes it as a `realtimeInput.video` PNG blob
  (verified wire shape), and sends it once before any mic audio. Capture
  failures are non-fatal — the turn proceeds without vision.

Wire shape (`realtimeInput.video` image blob) verified against the Live API
reference; one live confirmation that the model uses the frame still wanted.

Pre-commit gate green: fmt --check, clippy -D warnings, workspace tests (34
suites).

## 2026-06-18 — Realtime: seed conversation history into Gemini Live sessions

Second live finding: the Live assistant worked but had **no memory of earlier
turns** — each F8 press opened an amnesiac session. Root cause:
`open_session` received `ctx.history` but `build_setup_json` only consumed
`system_prompt` + `voice`, so the rolling history was silently dropped on the
floor.

Fix (verified against the Live API reference, not guessed — the mediaChunks
lesson applies):

- `build_setup_json` gains a `seed_history: bool` that adds
  `historyConfig.initialHistoryInClientContent: true` to the setup message.
  The API requires this flag before it will accept `clientContent` seeding.
- New `build_client_content_json(turns)` maps the rolling history onto a
  `clientContent` message (`turns: [{role, parts:[{text}]}], turnComplete:
  true`). `User -> "user"`, `Assistant -> "model"`; `System` (lives in
  `systemInstruction`) and `Tool` (no Path-B equivalent) turns and empty-text
  turns are skipped.
- `open_session` maps `ctx.history`, and when non-empty: flags the setup,
  then after `setupComplete` sends the `clientContent` seed once (before any
  `realtimeInput` audio). Per the reference, a seed with `turnComplete: true`
  is recorded as context **without** triggering a reply, so the reader stays
  one-shot on the real audio turn. Empty history keeps the previous path
  unchanged (no `historyConfig`, no seed message).
- Four new offline tests: historyConfig presence/absence, role mapping +
  skip rules + ordering, empty-history shape.

Still wants live confirmation that multi-turn memory actually lands, but the
wire shape now matches the documented seeding contract.

Also clarified (no code change) the second half of the same request — screen
vision for Gemini. The **staged** Gemini path already supports `fono_screen`:
`build_gemini` builds an `OpenAiCompatChat`, whose `reply_stream` gates the
screen tool on `prefer_vision && screen_capture.is_some()` (backend-agnostic).
So staged Gemini vision works today with `[assistant].prefer_vision = true`.
The **realtime** Live path is the real gap (Path B shipped tools-less); adding
screen vision there is a separate increment (Live video-frame input or
tool-calling), scoped as a follow-up.

Gate green: fmt --check, clippy -D warnings, workspace tests.

## 2026-06-18 — Realtime: fix deprecated realtimeInput.mediaChunks (first live finding)

First real live-API result from the Gemini Live path (maintainer set
`[assistant.cloud].model = "gemini-3.1-flash-live-preview"` and ran a turn).
The WebSocket closed immediately with:

> `realtime_input.media_chunks is deprecated. Use audio, video, or text instead.`

This is exactly the wire-shape class the offline tests cannot catch. The
writer serialised mic PCM as `realtimeInput.mediaChunks: [ {mimeType, data} ]`;
the current Live API expects a single Blob at `realtimeInput.audio:
{mimeType, data}`. Fixed `encode_audio_chunk` (and its doc comment + test) in
`gemini_live.rs`. `audioStreamEnd` was not flagged and is unchanged. Setup,
reader, and event mapping were not implicated by this error; further live
verification still pending for the response half.

Gate green: fmt --check, clippy -D warnings, workspace tests.

## 2026-06-18 — Realtime: switch Live model to gemini-3.1-flash-live-preview

Per maintainer directive, switched the Gemini Live realtime profile from
`gemini-2.5-flash-native-audio-preview-09-2025` to
**`gemini-3.1-flash-live-preview`** (catalogue `RealtimeProfile::model`;
`gemini-2.0-flash-live-001` remains the known-GA 404 fallback). Audited
`gemini_live.rs` against the 3.1 Flash Live docs and confirmed we are **not**
doing anything the migration warns against:

- **Multi-part events** — the 3.1 docs warn a single `serverContent` event can
  carry audio *and* transcript parts simultaneously. Our reader already loops
  `for part in mt.parts` (handling inline audio + text per part) and reads
  `outputTranscription` in the same event, so no content is dropped.
- **Thinking** — 3.1 uses `thinkingLevel` (not 2.5's `thinkingBudget`) and
  defaults to minimal for lowest latency. Our setup sets neither field, so we
  inherit the low-latency default and avoid sending the wrong (2.5) knob.
- **Proactive audio / affective dialogue / async function calling** — not set
  (tools are deferred under Path B anyway), so nothing to remove.

Wire shapes still want one live round (key rotated). Gate green: fmt, clippy
(`-D warnings`), workspace tests (34 suites).

### Chirp 3 HD for regular TTS — flagged, NOT implemented (decision needed)

Investigated the request to use **Chirp 3 HD** for batch TTS. Finding: Chirp 3
HD is **not** part of the Gemini API — it is a **Google Cloud Text-to-Speech**
product (`texttospeech.googleapis.com`). Its free allowance (≈1M bytes/month)
is a *billing-tier* free quota that still requires a **GCP project with a
billing account attached** (credit card), unlike the AI Studio
`GEMINI_API_KEY` free tier which needs **no billing**. Adopting it would
re-introduce the exact Chirp/Cloud lane dropped in ADR 0034 and violate the
project's core "single key, no billing" requirement. Left unimplemented pending
a maintainer decision; the all-Gemini alternative is to keep
`gemini-3.1-flash-tts-preview` for batch TTS and use Gemini Live for
low-latency spoken replies.

## 2026-06-17 — Realtime assistant (Gemini Live), Path B inc.5a: barge-in interrupt

Landed the **safe, offline-testable slice of barge-in**: handling Gemini
Live's `serverContent.interrupted` signal. When the model's own VAD detects
the user speaking over the reply, it discards the rest of its spoken turn —
the client now forwards that as a new `RealtimeEvent::Interrupted`, and the
reply driver (`drive_realtime_reply`) aborts the playback sink immediately so
Fono stops talking over the user. A later `Audio` frame re-opens the gapless
session for a fresh reply. Two offline parse tests (`interrupted:true` parses;
defaults `false` when absent); fono-assistant realtime suite now 67 (+2).

**Deferred (inc.5b — needs a live key + a clear owner):** the heavier half of
Inc5 — *live-hold streaming* (open the Live session on F8 **press** and bridge
the cpal capture callback into `audio_in` frame-by-frame during the hold,
rather than buffering and sending the whole utterance after release). That
re-architects the interactive capture pipeline in `session.rs` and its
mid-stream/interrupt wire semantics can't be verified with the rotated key, so
it stays a documented follow-up. The current one-shot push-to-talk realtime
path (inc.4) already delivers the user's core win: one continuous voice + a
streaming reply, no per-sentence drift, no 6 s batch-TTS wait.

Pre-commit gate green: fmt --check, clippy -D warnings, workspace tests.

## 2026-06-17 — Realtime assistant (Gemini Live), Path B inc.1: catalogue + trait

Starting the **realtime / speech-to-speech assistant** arc to fix the two
remaining Gemini voice problems the staged path can't: per-sentence voice
drift and ~6 s/sentence batch-TTS latency (Gemini delivers each
`generateContent` TTS call as one terminal block — confirmed in a trace —
so streaming has nothing to release early). The Live API
(`BidiGenerateContent` WebSocket) synthesises the whole reply as one
continuous stream and emits audio incrementally, fixing both.

**Path B** (chosen with the user): land the **audio loop first**, defer
tool-calling until `fono-action` exists. Sequenced as increments behind the
pre-commit gate; the WebSocket protocol can't be live-verified (key rotated)
so wire shapes are offline-unit-tested and flagged for live verification —
same posture as the STT/TTS clients.

De-risk: **`tokio-tungstenite` is already in the binary graph** (via
`fono-stt`/`fono-net`/`fono-mcp-server`), so the Live client's WebSocket
dependency is net-zero on binary size — no new dependency.

**Increment 1 (this commit) — foundation, fully offline:**
- Catalogue (`provider_catalog.rs`): new `RealtimeProfile` struct +
  `RealtimeProtocol` enum + `Badge::Realtime`; additive
  `AssistantDefaults.realtime: Option<RealtimeProfile>` (no reshape of the
  existing `text_model`/`multimodal_model` slots — all other providers get
  `None`). Gemini gains a Gemini Live profile (16 kHz in / 24 kHz out). Model
  id `gemini-2.5-flash-native-audio-preview-09-2025` **needs live
  verification** (`gemini-2.0-flash-live-001` is the known-GA fallback);
  it's a single catalogue const and `fono doctor` surfaces the active id.
- Two catalogue invariant tests: realtime profiles are well-formed (wss URL,
  non-zero rates) and badge-consistent; Gemini keeps its Live profile.
- Trait (`fono-assistant/traits.rs`): `RealtimeAssistant` trait,
  `RealtimeSession` (mic-in mpsc + reply `events` stream), `RealtimeEvent`
  (`Audio`/`AssistantTextDelta`/`UserTextFinal`/`Done`). Tools deliberately
  absent for Path B; doc-noted as the `fono-action` follow-up.

Remaining increments: gemini_live.rs WS client → factory `AssistantHandle`
dispatch → orchestrator F8 short-circuit + `run_realtime_turn` → raw PCM
capture streaming → wizard/doctor/CLI/ADR.

**Increment 2 (this commit) — Gemini Live WebSocket client, offline-tested:**
- New `fono-assistant/src/gemini_live.rs` behind a `realtime` feature (in
  `default`). `tokio-tungstenite` added as an optional dep — net-zero (already
  in the graph). `GeminiLive` implements `RealtimeAssistant`: connects with
  `?key=` on the upgrade, sends the `setup` message
  (`responseModalities:["AUDIO"]`, voice, system instruction, input+output
  transcription), waits for `setupComplete` (bounded), then runs a reader task
  (`serverContent` → `RealtimeEvent`: inline PCM → `Audio`, output
  transcription → `AssistantTextDelta`, input transcription → `UserTextFinal`,
  `turnComplete` → `Done`, one-shot) and a writer task (mic PCM →
  `realtimeInput.mediaChunks`, `audioStreamEnd` on `audio_in` close).
- Mirrors the Deepgram-streaming idioms: manual `IntoClientRequest`, split
  read/write tasks, `serde(default)` envelope for forward-compat. Handles
  Gemini Live's quirk of sending JSON over **binary** frames. Reader/writer
  loops extracted to generic free fns to stay under the clippy line limit.
- 14 offline tests: setup-JSON shape (modality/voice/system/transcription,
  bare↔prefixed model, empty-prompt omission), audio-chunk encode,
  audioStreamEnd, PCM s16le round-trip + clamp, inline-PCM decode, rate parse,
  serverContent/setupComplete/turnComplete parse, unknown-kind tolerance.
- Wire shapes still **need live verification** (key rotated) — same posture.

Remaining: factory `AssistantHandle` dispatch → orchestrator F8 short-circuit
+ `run_realtime_turn` → raw PCM capture streaming → wizard/doctor/CLI/ADR.

**Increment 3 (this commit) — factory `AssistantHandle` dispatch, offline-tested:**
- New `AssistantHandle` enum in `fono-assistant/src/factory.rs`: `Staged(Arc<dyn
  Assistant>)` (every backend, the default) and `Realtime(Arc<dyn
  RealtimeAssistant>)` (gated on the `realtime` feature).
- `build_assistant_handle(cfg, secrets, dir)` dispatches: when the backend is
  Gemini **and** `[assistant.cloud].model` equals the catalogue's
  `RealtimeProfile::model`, it builds a `GeminiLive` client (key resolved from
  `api_key_ref`/`GEMINI_API_KEY`, reply voice = Gemini TTS `default_voice` →
  `Kore`) and returns `Realtime`; otherwise it delegates to `build_assistant`
  and wraps in `Staged`. `build_assistant` is unchanged (still used by MCP /
  examples).
- Selection is opt-in by model id: a blank/default model stays staged, so
  existing Gemini users are unaffected. Non-Gemini backends never select
  realtime even if the model string matches.
- 5 dispatch tests: realtime model → `Realtime`; default/no-cloud → `Staged`;
  non-Gemini + realtime id → `Staged`; missing key → clear `fono keys add`
  error.

Remaining: orchestrator F8 short-circuit + `run_realtime_turn` → raw PCM
capture streaming → wizard/doctor/CLI/ADR.

**Increment 4 (this commit) — orchestrator F8 short-circuit, end-to-end:**
- `session.rs`: store the realtime backend in a new `realtime_backend` slot
  (populated by `build_assistant_handle` in `new()`/`reload()`), add
  `current_realtime()`, and short-circuit `on_assistant_hold_release`: when a
  realtime backend is loaded, build `RealtimeTurnInputs` and dispatch
  `run_realtime_turn` *before* the staged STT/LLM/TTS path (which would
  otherwise warn "backend missing" because the staged slot is empty in
  realtime mode). Extracted the shared pump teardown into
  `spawn_assistant_pump` so both paths reuse the same clear-slot / stop-
  animation / hide-overlay / FSM-idle epilogue.
- `assistant.rs`: `run_realtime_turn` opens the Live session (errors
  classify+notify via `open_realtime_or_notify`), lazily ensures playback,
  resamples the captured mic PCM to the model's `native_input_rate` and streams
  it in ~50 ms chunks (`send_mic_to_session`, one-shot push-to-talk), then
  drives the reply through `drive_realtime_reply`: a `LocalPlaybackSink`
  gaplessly plays reply audio as it arrives, `FirstAudio` reports honest TTFA
  on the first frame, transcripts accumulate into history, and `notify`
  cancellation (Escape) aborts the sink. Emits the same `assistant:` summary
  line as the staged path.
- `fono` crate gains a `realtime` feature forwarding to
  `fono-assistant/realtime` (in `default`).

Remaining: raw PCM live capture streaming (mic during hold, not one-shot) →
wizard/doctor/CLI/ADR.

**Increment 6 (this commit) — discoverability: wizard, doctor, ADR, docs:**
- ADR 0035 records the Path B decision (audio loop first, tools deferred),
  the opt-in-by-model-id selection, the additive catalogue profile, and the
  net-zero WebSocket dependency.
- Wizard: when the chosen assistant provider advertises a Gemini Live profile,
  the fast path now offers "realtime speech-to-speech" (`offer_realtime`,
  default yes). On accept it repoints `[assistant.cloud].model` at the
  catalogue realtime id and skips the staged TTS picker (Live produces its own
  continuous-voice audio).
- Doctor: the assistant probe now goes through `build_assistant_handle` and
  labels the active mode — `assistant: … (staged)` vs
  `assistant: … (realtime speech-to-speech)`.
- `docs/providers.md`: new "Realtime (speech-to-speech)" subsection under the
  Gemini section; the capability line now lists realtime as wired.

Remaining: Increment 5 — raw PCM live mic streaming *during* hold (barge-in /
true full-duplex), an optimisation on top of the working one-shot path; and
the `fono-action` tool dispatcher to bring tool-calling to the realtime path.

## 2026-06-17 — Make record + playback obvious in turn traces

The `playback` and `capture` lanes existed in the trace taxonomy
(`turn_trace.rs`) but **nothing ever emitted on them**, so a `/tmp/fono-traces`
waterfall showed only the high-level `stt`/`tts` synthesis spans — you couldn't
see when audio actually *started reaching the device*. Instrumented the
`fono-audio` workers (and the capture backends) to emit on those lanes via the
ambient `current_instant` / `duration_between` helpers (no-op on untraced
turns — one relaxed atomic load — so the hot path pays nothing).

- **Playback lane** (`fono-audio/src/playback.rs`, both paplay and cpal
  workers): `playback.play` span for one-shot clips; `playback.stream_open`,
  `playback.first_audio` (the moment the player spawns on the first chunk),
  and the closing `playback.stream` span for streaming sessions. The paplay
  `StreamChunk` body was extracted into `handle_paplay_stream_chunk` to keep
  `spawn_worker` under the 100-line clippy limit.
- **Capture lane** (`fono-audio/src/capture.rs`, both process and cpal
  backends): `capture.open` (mic/tool spawned) and `capture.first_frame` (first
  PCM in).
- **`capture.input`** instant on the assistant turn trace (`assistant.rs`):
  device-level capture predates the turn's trace, so this surfaces the recorded
  input bounds (samples / duration_ms) on the turn timeline, making the
  record→STT→playback boundary obvious.
- `serde_json` added to `fono-audio` for the trace args.

Note: the `cpal-backend` feature carries pre-existing clippy debt (introduced
with the C2 gapless-playback work and not caught because CI's clippy step runs
default features only — paplay on Linux). The trace additions slightly grow the
cpal `spawn_worker` line count, but that feature was already clippy-red on HEAD
and is out of scope here. The default-feature gate is green.

Pre-commit gate green: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings` (default features, as CI), `cargo test --workspace
--tests --lib`.

## 2026-06-17 — Honest TTFA: fire first-audio mid-stream, not after the sentence

A Gemini assistant turn still logged `tts 8324ms ttfa` despite the streaming
work, because the metric (and the FSM/overlay flip to SPEAKING) only fired
*after* `synth_and_stream` returned — i.e. after the **entire first sentence**
finished streaming — rather than when the first PCM frame actually reached the
device. The audio was already playing early; the number lied.

- **`stream_utterance` now takes an `on_first_audio: FnMut()` callback**,
  invoked exactly once the moment the prebuffer releases and the first PCM is
  pushed to the sink (or on the tail flush for sub-prebuffer utterances). Two
  new tests assert it fires exactly once with audio and never without.
- **Assistant pump:** extracted a `FirstAudio` helper (idempotent, records TTFA
  relative to LLM start, flips FSM + overlay to SPEAKING). Streaming sentences
  fire it mid-stream via the callback; batch/local sentences fire it right
  after the first successful enqueue. `metrics.tts_ttfa_ms` now reflects the
  true time-to-first-frame.
- Non-streaming call sites (`fono speak`, MCP `fono.speak`) pass a no-op `|| {}`.

Pre-commit gate green: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, `cargo test --workspace --tests --lib`.

## 2026-06-17 — Gemini: drop prebuffer config, default 300 ms, switch to Flash-Lite

Follow-up tuning after the C1–C5 streaming work:

- **No prebuffer config.** Removed the `[tts] stream_prebuffer_ms` config field
  (and its serde default/skip helpers). The streaming driver now uses a fixed
  `DEFAULT_STREAM_PREBUFFER_MS = 300` constant in `fono-tts::streaming`. The
  `prebuffer_ms` parameter was dropped from `stream_utterance` and all call
  sites (assistant pump, `fono speak`, MCP `fono.speak`); `AssistantTurnInputs`
  lost its `stream_prebuffer_ms` field. 300 ms (up from the old 200) gives a
  little more jitter headroom.
- **Default model → `gemini-flash-lite-latest`** for STT, polish, and the
  staged assistant (text + multimodal), replacing `gemini-flash-latest`.
  Flash-Lite is the lower-latency/cheaper tier of the Flash family and the
  `-latest` alias tracks the current model. TTS stays
  `gemini-3.1-flash-tts-preview`. Single source of truth is the `gemini`
  catalogue entry; mirror sites (STT `DEFAULT_MODEL`, polish/assistant tests,
  docs) updated.

Pre-commit gate green: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, `cargo test --workspace --tests --lib`.

## 2026-06-17 — Latency: Gemini thinking knob + cloud streaming TTS (C1–C5)

Two latency fixes after a Gemini assistant turn measured 24.5 s (`llm 4577ms
ttfb`, `tts 10478ms ttfa`):

**Thinking fix (committed separately, `411359d`).** `gemini-flash-latest`
resolves to a Gemini 3.x Flash, which enables "thinking" by default — that
reasoning ran before the first token and inflated TTFT from ~800 ms to ~4.5 s.
On Gemini's OpenAI-compatible surface the knob is `reasoning_effort`; 3.x can't
disable thinking, but `"low"` pins it to the minimum. Applied to both
OpenAI-compat clients (polish treats `backend == "gemini"` as reasoning; the
assistant adds a `ChatReq.reasoning_effort` field set to `"low"` for Gemini,
`None` elsewhere).

**Cloud streaming TTS (C1–C5 of
`plans/2026-06-17-cloud-streaming-tts-v2.md`).** Play synthesised audio
gaplessly as it arrives instead of waiting for the whole clip:

- **C1** — `TtsChunk` + `synthesize_stream` (default wraps `synthesize`, one
  chunk) + `supports_streaming` on the `TextToSpeech` trait. Batch/local
  backends compile and behave unchanged.
- **C2** — gapless streaming append path in `fono-audio` playback (paplay +
  cpal backends): `begin_stream`/`push_stream`/`end_stream`, one resampler per
  utterance, no drain-between-chunks gap. Batch `enqueue` preserved.
- **C3** — `PcmSink` trait + `LocalPlaybackSink` in `fono-audio`
  (`crates/fono-audio/src/sink.rs`) so the driver is transport-agnostic for
  later server-mode network audio; both the daemon and MCP server reach it.
- **C4** — fixed-prebuffer driver (`fono_tts::stream_utterance`) + config
  `[tts] stream_prebuffer_ms` (default 200). Routed through the assistant pump,
  `fono speak`, and the MCP `fono.speak` tool. `supports_streaming() == false`
  ⇒ existing batch path.
- **C5** — Gemini `streamGenerateContent?alt=sse` override: SSE decoder +
  incremental `inlineData` PCM frames. Offline-tested; **live-verify with a
  real key still pending** (the in-session key was rotated).

Local engines stay batch (slow-machine RTF/underrun risk deferred to
`plans/2026-06-17-general-streaming-tts-v1.md`). C6/C7 (Cartesia, Deepgram/
ElevenLabs/OpenAI streaming overrides) remain pending.

Pre-commit gate green: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, `cargo test --workspace --tests --lib`.

## 2026-06-17 — Gemini default models: `gemini-flash-latest` (STT/LLM) + TTS preview

User directive: use the **documented `gemini-flash-latest` alias** for STT and
the LLM capabilities (not a pinned, invented version string), and
`gemini-3.1-flash-tts-preview` for TTS. Updated the single source of truth (the
`gemini` entry in `crates/fono-core/src/provider_catalog.rs`):

- **STT / polish / assistant text + multimodal** → `gemini-flash-latest`. The
  `-latest` alias always resolves to the current Flash model, so there is no
  version churn and no risk of an invented/incorrect pinned id.
- **TTS** → `gemini-3.1-flash-tts-preview` (per the explicit instruction; the
  slow `gemini-2.5-flash-preview-tts` was the cause of the ~4.3 s TTS
  time-to-first-audio reported earlier).

This supersedes the earlier same-day attempt that pinned `gemini-3.1-flash`
for STT/LLM — that bare name was an unverified extrapolation and has been
corrected to the alias.

Mirror sites updated to match: `fono-stt::gemini` `DEFAULT_MODEL`,
the `fono-tts::gemini` endpoint test, the `fono-polish::defaults` catalogue
test, and every `docs/providers.md` reference (capability matrix, polish/TTS/
assistant tables, wire-shape notes).

Note: could not live-verify the TTS preview id — the `GEMINI_API_KEY` pasted
earlier in-session has been rotated and now 401s on the model-list endpoint.
`gemini-flash-latest` is a documented stable alias; the TTS id follows the
explicit instruction. `fono doctor` reports the active id at runtime, so a
mismatch surfaces immediately.

Pre-commit gate green: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, and `cargo test --workspace --tests --lib`.

## 2026-06-17 — Google via Gemini API (single key): LLM polish + staged assistant + STT + native TTS

Executing `plans/2026-06-17-google-via-gemini-single-key-stt-tts-llm-realtime-v2.md`.
User decision: Google support is the **Gemini API (AI Studio)** on a **single
`GEMINI_API_KEY` with a free tier** — not Google Cloud Speech. The Chirp /
service-account / OAuth lane (and the planned `fono-net-google` crate) is dropped;
everything consolidates onto the existing `gemini` catalogue entry.

Landed this session (plan Sections A, E1, E2, C, D — all on the single key):

- **ADR 0034** (`docs/decisions/0034-google-via-gemini-single-key.md`) records the
  single-key/free-tier decision, why Cloud Speech was dropped, and the
  OpenAI-compat-reuse-vs-bespoke-client split per capability.
- **Polish (E1)** — replaced the runtime "Gemini polish not yet implemented" stub
  with a real client: `OpenAiCompat::gemini()` targets Gemini's OpenAI-compatible
  surface (`/v1beta/openai/chat/completions`, `Authorization: Bearer <key>`).
  `crates/fono-polish/src/openai_compat.rs`, `crates/fono-polish/src/factory.rs`.
  Polish default model bumped `gemini-1.5-flash` → `gemini-2.5-flash`.
- **Staged assistant (E2)** — new `AssistantBackend::Gemini`
  (`crates/fono-core/src/config.rs`), fully wired through
  `crates/fono-core/src/providers.rs` (str/parse/key-env/all-list, 7→8),
  the `gemini` catalogue entry gains `assistant: Some(..)` (text+multimodal
  `gemini-2.5-flash`, `google_search` declared), `OpenAiCompatChat::gemini()`
  constructor, `build_gemini()` factory arm, vision-capability check in
  `crates/fono/src/session.rs`, and the MCP summarize `FALLBACK_ORDER` (6→7).
  Note: the OpenAI-compat layer cannot inject Gemini's native `google_search`
  grounding tool, so the staged path ships without native web search (ADR 0034
  flags it as a follow-up on the `generateContent` endpoint).
- **Docs (A3)** — `docs/providers.md` Gemini section (single key, free-tier
  RPD/RPM + midnight-Pacific reset, preview-model caveat, STT batch/no-confidence
  note); capability matrix + polish/assistant/TTS tables refreshed.
- **STT (Section C)** — bespoke `fono-stt::gemini` audio-understanding client
  (`generateContent`, transcribe-only prompt, `x-goog-api-key`, no per-segment
  confidence, batch-only, one-shot rerun-unavailable warning). Added
  `SttBackend::Gemini` (config + providers str/parse/key-env/all-list), the
  `gemini` catalogue entry gains `stt: Some(..)` (`gemini-2.5-flash`), the
  `gemini` feature on `fono-stt` (+ base64), and the factory build arm. 8 client
  tests.
- **TTS (Section D)** — bespoke `fono-tts::gemini` native-speech client
  (`generateContent`, `responseModalities:["AUDIO"]`, base64 int16 LE → f32 PCM,
  `mimeType` rate parse w/ 24 kHz fallback, voice in body via
  `prebuiltVoiceConfig`). Added `TtsEndpoint::Gemini` + `TtsBackend::Gemini`
  (config + providers str/parse/key-env/requires-key/all-list 10→11), the
  `gemini` catalogue `tts: Some(..)` (`gemini-2.5-flash-preview-tts`, default
  voice `Kore`, gender-balanced 10-voice palette, multilingual), the `gemini`
  feature on `fono-tts` (+ base64, openai_compat warm-client gate), the factory
  build arm, and Gemini arms in the doctor/daemon/wizard `TtsBackend` matches.
  10 client tests.
- **Wizard** — removed the now-stale `!= "gemini"` guards in `is_polish_wired` /
  `is_assistant_wired` (E1/E2 wired both); Gemini now surfaces as a full
  primary candidate (STT/LLM/Assistant/TTS/Vision/Search all ✓). Updated the
  picker-table pin (col width 14→15, new "Google Gemini" row) and the
  candidate-set tests.
- **Assistant-turn STT errors now notify** — the STT stage inside
  `run_assistant_turn` (`crates/fono/src/assistant.rs`) previously propagated a
  backend failure raw (`r?`), so it surfaced only as a session-level `warn!`
  with no desktop popup. It now mirrors the LLM-stage handling: classify the
  error and fire one `critical_notify::notify(Stage::Stt, …)` for
  Auth/Payment/Network/Terms classes (e.g. a Gemini `403 PERMISSION_DENIED`),
  subject to the global session-cap suppression.
- **Live-API verification (Gemini single key)** — ran the diagnostic curls with
  a real `GEMINI_API_KEY`. `GET /v1beta/models` → **HTTP 200** (key authenticates,
  request shape correct), but `POST …:generateContent` → **HTTP 403
  PERMISSION_DENIED "Your project has been denied access. Please contact
  support."** — reproduced with the user's *own* raw curl, proving this is a
  Google account/project-side block (region/policy flag), **not** a Fono bug or
  a malformed request (that would be 400). Our STT/TTS wire shapes are validated
  to the extent the project allows; full content-generation verification awaits
  an unblocked project/key.

Pre-commit gate green: `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, and `cargo test --workspace --tests --lib` all pass.

Remaining (clearly scoped in the v2 plan, not yet landed):

- **Realtime + tools (Sections F, G)** — blocked on two unbuilt prerequisites:
  the `fono-action` tool dispatcher (`voice-actions-via-mcp`) and the catalogue
  `ModelEntry` reshape (realtime-v4 Phase 1). Then the Gemini Live
  (`BidiGenerateContent`) client reusing the same dispatcher.
- **Native web search (staged path)** — the OpenAI-compat layer can't inject
  `google_search`; wiring it needs the native `generateContent` endpoint.
- STT (C) and native TTS (D) are wired but their bespoke wire shapes are only
  unit-tested offline; they still want one round of **live-API verification**
  with a real key.

All changes staged locally, signed off, **not pushed**.

## 2026-06-16 — Per-program TTS voices (palette + gender + positional labels)

Executing `plans/2026-06-16-per-program-tts-voices-v4.md`. Fono now speaks with a
distinct, stable voice per calling program (coding agent vs. chat notifier vs. coach),
addressed by friendly positional gendered labels (`Female 1`, `Male 2`) instead of
cryptic backend-specific ids. Done so far (Tasks 1–9, 11):

- **Voices (Task 3b)** — added two male English Kokoro voices `am_michael` (en-us) and
  `bm_lewis` (en-gb), closing the all-female English local gap. Style packs published to
  the `fono-voice` `ort-1.24.2` release (byte-identical to upstream
  onnx-community/Kokoro-82M-v1.0-ONNX tensors); catalog + manifest + README indexed.
- **Palette (Tasks 1, 2, 3a)** — new `fono-core::voice_palette` (`Gender`, `PaletteVoice`,
  `PaletteEntry`, `Palette` with positional per-gender label render/parse). Cloud palette
  baked into `provider_catalog` per provider; local palette derived from the on-device
  catalog with a per-voice `gender` field (Kokoro derived from the `a?_`/`b?_` naming).
- **Identity (Task 4)** — the MCP `initialize` handshake's `clientInfo.name`, previously
  discarded, is captured into a shared `ClientIdentityHandle` and threaded through every
  tool.
- **Config (Task 5)** — `[mcp]` gains `voices` (program→label map), `voice_gender`, and
  `auto_assign_voices` (default true; skipped on serialize at default).
- **Resolver (Task 6)** — pure `fono-core::voice_resolver`: explicit voice → manual pin →
  stable FNV-1a auto-assignment (gender-filtered) → backend default; stale pins degrade to
  auto. 14 unit tests.
- **Wiring (Task 7)** — `voice_io::active_palette` + `resolve_program_voice` wired into
  `fono.speak`/`listen`/`confirm`/`summarize` and the `fono summarize` CLI (summarize keys
  on `source_app`, falling back to the MCP client identity).
- **Local override (Task 8)** — `LocalRouter` now honours an explicit per-call voice via
  `resolve_explicit_voice`, so on-device users get per-program voices too.
- **CLI (Task 9)** — `fono voices list/set/unset/gender/preview` manage everything by
  label, validated against the active backend.
- **Docs (Task 11)** — `docs/configuration.md` per-program-voices section;
  `docs/coding-agents.md` note. Resolver/palette/local-override unit tests landed with
  their respective tasks.

Remaining: Task 10 (optional cloud voice auto-discovery) is deferred. All commits staged
locally, signed off, **not pushed**.

## 2026-06-15 — TTS: automatic local fallback for English-only cloud voices

Executed `plans/2026-06-15-tts-language-capability-mismatch-v2.md`. English-only
cloud voices (Groq Orpheus `…-english`, Speechmatics preview, Deepgram
`aura-2-…-en`) phonemized non-English text as gibberish. Fixed with minimal
surface area: one catalogue boolean, no new config knobs, negligible latency on
the common path.

- New `english_only: bool` on `TtsDefaults`
  (`crates/fono-core/src/provider_catalog.rs:118`), default `false` so a new or
  unflagged provider fails safe as multilingual. Set `true` on Groq and
  Speechmatics; pinned per-provider by `tts_english_only_pinned` plus a
  `tts_backend_english_only_matches_catalogue` helper test.
- New `tts_backend_english_only(&TtsBackend) -> bool` helper
  (`crates/fono-core/src/provider_catalog.rs:635`) so consumers don't duplicate
  the lookup.
- New `crates/fono-tts/src/english_only_fallback.rs`: `EnglishOnlyFallback`
  wraps an English-only cloud backend. Per utterance it resolves the language
  (caller's hint, else `whatlang` constrained to `general.languages`); English
  or inconclusive text goes to the cloud backend unchanged (zero behaviour
  change on the common path), reliably non-English text is synthesized by the
  local multilingual Piper voice for that language (lazily built + cached). When
  no local engine is available it warns once and skips the utterance (empty PCM)
  rather than speaking gibberish.
- Factory wires it at one chokepoint: `maybe_wrap_english_only`
  (`crates/fono-tts/src/factory.rs:69`) wraps the built backend only when
  `tts-local` is compiled in and the catalogue flags the backend English-only;
  otherwise the cloud backend is returned as-is. Because the wrapper lives at
  the `synthesize` boundary, all callers (assistant, `fono speak --stream`, MCP
  `speak_text`) are covered without per-path plumbing.
- `load_engine` exposed `pub(crate)` from `local_router.rs` for reuse by the
  wrapper.
- Tests: catalogue pins; `route_language` (English→cloud, Romanian→local,
  hint-driven when detection inconclusive); synthesize paths (English→primary,
  non-English→skip-when-local-unavailable, empty→passthrough). Docs:
  `docs/providers.md` new "English-only voices and the automatic local fallback"
  section; CHANGELOG Unreleased entry.
- Gate: fmt + clippy + tests.

## 2026-06-13 — Summarize: cache the local system-prompt prefix across calls

Follow-up to the refusal/repetition fix below. The `fono.summarize` path on the
local backend was paying the full system-prompt prefill on every call: the MCP
tool rebuilt the assistant per request, and even within one process the prompt
cache evicted the shared prefix.

- The summarize MCP tool now holds the built assistant in a process-lifetime
  `OnceCell`, so the model and its prompt-state cache survive across calls
  instead of being dropped after each summary.
- One-shot requests (empty history — the summarize shape) now store *only* the
  shared system-prompt prefix checkpoint and skip the payload-specific
  completed-turn checkpoint. Previously the deeper completed-turn entry (which
  embeds that call's payload+reply, useless to the next differing payload)
  pruned the system-prefix entry we actually want to reuse. Threaded as
  `GenParams { max_new_tokens, one_shot }` through the prefix-cache decode path
  (kept under clippy's argument limit, both flags travel together).
- F8 multi-turn chat is unchanged: non-empty history still stores and restores
  the completed-turn checkpoint.
- New live regression test (ignored, model-gated):
  `repeated_prefix_prompt_restores_cached_system_prefix` proves call 2 reuses
  call 1's system prefix. Run the live cache tests with `--test-threads=1` (two
  models contend on the shared llama backend otherwise). Gate green: fmt,
  clippy (incl. `--features llama-local`), workspace tests. Not committed.

## 2026-06-12 — Summarize refusal/repetition fix: shared local generation policy

Executed `plans/2026-06-12-summarize-refusal-mitigation-v3.md` (all 12 tasks).
Root cause of the `fono summarize` 13 s refusal loop on local gemma-4-e2b: the
assistant backend never received the two F7 polish decode fixes — its stop
checks were dead code on this vocab (non-standard `<|turn>`/`<turn|>` control
tokens) and it sampled with bare greedy, so a safety refusal repeated verbatim
to the 384-token cap.

Fix, structurally shared so the next model switch can't reintroduce it in one
backend only:
- New `fono-core::llama_gen` module: deterministic `penalties(128, 1.3) +
  greedy` sampler chain, Control-attr stop predicate, textual stop-marker scan,
  UTF-8-safe stream split, and `warn_on_template_vocab_mismatch` — a load-time
  tripwire that warns when a template marker doesn't tokenize to a single
  control token (fires twice on gemma-4-e2b, silent on standard vocabs). Both
  the assistant and polish local backends now consume the same symbols.
- `AssistantContext.max_new_tokens`: optional per-request cap (clamped to the
  backend budget); summarize sets 96 so even a worst-case degenerate run is
  bounded to seconds. Cloud backends ignore it; F7/F8 chat unchanged (`None`).
- Summarize hardening: `default_summarize_prompt` now frames the model as a
  neutral relay with an explicit no-refusal directive; `summarize_with`
  collapses consecutive duplicate sentences and degrades a bare refusal to a
  deterministic metadata fallback ("Bogdan sent a message in test.").
- Prefix-cache interplay verified: live two-turn checkpoint store/restore test
  (ignored, model-gated) plus replay benches pass; `outputs_match` holds under
  penalized greedy.

Repro result: the profane payload now yields one neutral sentence in ~3.7 s
wall (incl. model load) with a control-token stop. Deferred follow-up (in the
plan's execution notes): render via the GGUF's embedded `tokenizer.chat_template`
to fully replace name-substring template dispatch — needs its own design pass
to preserve the prompt-state cache's textual prefix/suffix invariants. Gate
green: fmt, clippy (incl. `--features llama-local`), workspace tests. Not
committed.

## 2026-06-12 — v0.10.0 release prep + streaming local cleanup injection

Landed `plans/2026-06-12-streaming-cleanup-injection-v3.md`: local AI cleanup
now streams into the cursor word-by-word as the embedded model decodes, instead
of waiting for the whole pass. `TextFormatter` gains a `format_stream` default
(one-shot wrapper; only `LlamaLocal` overrides), the orchestrator buffers to a
first-sentence gate, runs all three cleanup guards on the buffered prefix, then
flushes whole words after the gate. Auto-falls-back to one-shot for cloud
backends, short utterances, and clipboard-fallback sessions. New
`[polish].stream_injection` flag (default `true`). Supporting changes:
`streaming_decode_threads()` reserves one core for the streaming consumer to
avoid the per-token barrier stall (recovered F7 ~13→26 tok/s; same trick wired
into the assistant), and the F8 decode loop now emits a single `llm.generate`
span with `ttft_ms`/`deltas` (per-token instants gated behind
`FONO_TRACE_TOKENS`).

Release: graduated the CHANGELOG `[Unreleased]` section to **`## [0.10.0] —
2026-06-12`**, bumped `[workspace.package] version` to `0.10.0`, and refreshed
`ROADMAP.md` (new Recently-shipped highlight + Shipped entry; moved the local
TTS roadmap item into Shipped). Version decision: stay on `0.x` (`0.10.0`, not
`1.0.0`) — the release is additive features/fixes and still adds config keys;
`1.0` is reserved for a stability commitment (cross-platform / preview-feature
graduation).

Pre-commit gate: see the verification block staged with the commit.

## 2026-06-09 — F7 polish: control-token stop (the definitive cleanup fix)

The repetition-penalty fix (below) stopped the verbatim *text* loop but a fresh
trace still showed garbage: `polish 2001ms … 20 → 5 chars`, output `model`. The
penalty had collapsed the old 256-token `<start_of_turn>model…` loop down to a
single `model`, but the underlying stop-detection was still broken.

**Definitive root cause (proven against the real `gemma-4-e2b.gguf`).** A
throwaway tokenizer/generation probe over the actual model file showed this
GGUF's control tokens are **non-standard**:
- token **105** renders as `<|turn>` — `control = true`, `eog = false` (the
  start-of-turn opener; renders **empty** under `special = false`).
- token **106** renders as `<turn|>` — `control = true`, `eog = true` (the
  end-of-turn closer).
- tokens 107/108 are `\n` / `\n\n` — `control = false` (ordinary text).

So the hand-rolled literals `<start_of_turn>` / `<end_of_turn>` tokenize as
**plain text** on this vocab and never match the model's real markers — both for
prompting and for stop detection. `single_token("<end_of_turn>")` returned
`None`, making **every** literal-string stop check dead code. The model emitted
its real opener (105, empty render) → `model`, with nothing to stop it; `is_eog`
alone would also have missed 105. The native chat template (option B) is not a
viable workaround here: `apply_chat_template` fails with `FfiError(-1)` on this
model's tool-enabled Jinja template.

**Fix.** `generate_from_prefilled` now stops on **any token tagged
`LlamaTokenAttr::Control`** (`model.token_attr(token).contains(Control)`),
replacing the dead `single_token` literal checks (helper removed). This is
model-agnostic and correct by construction — a single-shot cleanup must never
emit a turn marker, BOS/EOS, or end-of-generation token — and it catches 105,
106, eos and bos while letting newline tokens (107/108) flow. The repetition
penalty (for pure-text loops that emit no control token) and the textual
`first_stop_marker` scan (for markers that round-trip as plain text) remain as
complementary safety nets. Probe-confirmed: clean self-termination at ~23 tokens
in ~1.5 s on this model.

Latency unchanged from the note below: this is correctness, not speed. Embedded
CPU decode (~10–15 tok/s) puts a typical cleanup at ~1.5–3 s; sub-1s needs the
GPU build or the local-server / ollama polish backend. Gate green: `cargo fmt
--all -- --check`, `cargo clippy --workspace --all-targets --features
llama-local -- -D warnings`, `cargo test --workspace --tests --lib --features
llama-local` (0 failures). Not committed.

## 2026-06-09 — F7 polish: Gemma template support (fixes looping cleanup output)

A trace + log run surfaced a serious functional bug on the embedded polish path:
with a **Gemma** model configured for local polish, cleanup output looped the
same (correctly cleaned) sentence ~17× until the 256-token cap — `polish 28523ms
[app+adv] 34 → 645 chars`. Correct-text-repeated was the tell: the model cleaned
fine but never received a stop signal.

**Root cause.** The embedded polish backend (`crates/fono-polish/src/llama_local.rs`)
only ever emitted the **ChatML** template (`<|im_start|>…<|im_end|>`) and only
stopped on `eos` / `<|im_end|>`. Gemma uses `<start_of_turn>…<end_of_turn>` and
never emits `<|im_end|>`, so greedy decoding ran to `MAX_NEW_TOKENS`. This
surfaced now because Gemma polish previously routed to the ollama HTTP backend
(which applies Gemma's own template); a recent change wired Gemma into the
embedded `LlamaLocal` path, which had no Gemma support. The assistant backend
already dispatches Gemma vs ChatML by model name — polish did not.

**Fix.** Made the polish backend template-aware, mirroring the assistant:
- `template_for_model` + `build_prompt_split_for_model` dispatch Gemma vs ChatML
  (Qwen3 thinking-suppression preserved) by model-name substring.
- `build_gemma_prompt_split` renders `<start_of_turn>user\n{system}\n\n` /
  `{transcript}<end_of_turn>\n<start_of_turn>model\n` (Gemma has no system role,
  so the system prompt leads the user turn).
- `generate_from_prefilled` now also stops on `<end_of_turn>` (via a new
  `single_token` helper used for both stop markers).
- `base_prefix_for_model` frames the pinned base prefix to match the active
  template (`<start_of_turn>user\n{base}` for Gemma), so the prewarmed F7 base
  remains a genuine token-prefix of the live prompt; `format()` and
  `prewarm_prompt_cache` both route through it.
- New model-free tests: Gemma/Qwen template dispatch, Gemma split round-trip,
  Gemma base-prefix nesting.

**Follow-up (same session) — runaway-generation guard.** A trace after the
template fix showed the cache working perfectly (restored the 426-token Gemma
base, `cache_hits: 1`) but generation still ran ~24.6 s to the 256-token cap,
emitting a `<start_of_turn>`+`model` loop (the opener renders empty under
`special = false`, so the visible output was bare `model` lines). The loop never
closed with `<end_of_turn>`. Fix: `generate_from_prefilled` now also stops on
`<start_of_turn>` (a single-shot cleanup must never open a new turn) and runs a
textual `first_stop_marker` scan over the template markers as belt-and-braces
(for models that emit markers as plain text). Bounds runtime and prevents
injecting the looped output. NOTE: gemma-4-e2b at q4 appeared to degenerate from
the first generated token on this cleanup prompt under greedy decoding — the
guard stops the runaway, but if a model degenerates immediately the cleanup
falls back to raw text; a ChatML cleanup model (Qwen/SmolLM) or a cloud polish
backend is the better choice for low-tier local hardware.

Gate green: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
--features llama-local -- -D warnings`, `cargo test --workspace --tests --lib
--features llama-local` (0 failures). Not committed.

**Follow-up (same session) — repetition-penalty sampler (the actual root cause).**
The "degenerates from the first token" note above was wrong. A later trace showed
the embedded polish path producing the *correctly cleaned* sentence and then
repeating it verbatim ~6× until the 256-token cap — correct content, infinite
verbatim loop. Root cause: the embedded `LlamaLocal` cleanup sampler was bare
`LlamaSampler::greedy()` with **no repetition penalty**. Cleanup output closely
mirrors the input transcript, the worst case for greedy decoding: once the model
reproduces the near-echo input it keeps reproducing it and never emits
`<end_of_turn>`. The Gemma cleanup that "worked in benchmarks" ran through the
ollama/server path, which applies the model's default sampling stack (top_p +
repeat penalty); the embedded path never did. Fix: `generate_from_prefilled` now
uses `chain_simple([penalties(PENALTY_LAST_N=128, PENALTY_REPEAT=1.3, 0.0, 0.0),
greedy()])`. The penalty sampler only sees tokens passed to `sampler.accept()`,
and we accept *only generated* tokens (prefill uses `ctx.decode`), so it
penalises the model for repeating its own output without penalising faithful
reproduction of the transcript; output stays deterministic (argmax of penalised
logits). The `<start_of_turn>`/`<end_of_turn>`/`first_stop_marker` stops remain as
the safety net.

Latency caveat: this fixes correctness (23 s loop → one clean pass) but the
embedded CPU decode rate (~10–11 tok/s in traces) puts a ~35-token cleanup at
~1.5–3 s. Sub-1s cleanup (the ~0.55 s benchmark) was the local OpenAI-compatible
server path, not embedded — see `plans/2026-06-07-local-assistant-runtime-parity-v1.md`.
For sub-1s: use the GPU build, the local server / ollama polish backend, or close
the documented embedded-vs-server parity gap. Gate green (fmt, clippy
`--features llama-local -D warnings`, workspace tests, 0 failures). Not committed.

## 2026-06-09 — F8 cache: real root cause found (current-turn double-count) + flat-prefill fix

The 2026-06-09 longest-prefix work below made the machinery fire but a follow-up
trace run (`/tmp/fono-traces`, ~09:5x) showed it only ever restored the **static
78-token `f8_system` base** — never a prior turn's `F8ChatPrefix` checkpoint — so
prefill (and TTFB) still grew with conversation length (turn 4: 250 prefilled
tokens, 3003 ms TTFB). Investigated and found the true root cause; the earlier
"framing fix" addressed only the base.

**Investigation (conclusive, tokenizer-level).** A throwaway probe loaded the
real Gemma tokenizer (`ggml-vocab-gemma-4.gguf`, vocab-only) and replicated the
exact live store/lookup comparison. Clean append-only history nests perfectly
(boundary-merge hypothesis **disproven**); replicating the live daemon flow
breaks nesting every turn, diverging at the same place — the stored prefix ends
in tokens for `<start_of_turn>user\n` while the next turn has
`<start_of_turn>model\n{reply}…` there.

**Root cause.** `crates/fono/src/assistant.rs` pushed the current user turn into
`ConversationHistory` **before** snapshotting, so `ctx.history` already ended
with the in-flight turn — *and* the same text was passed as `user_text`. Every
backend's builder (`build_*_prompt_split` for local, `build_initial_messages` /
the anthropic message loop for cloud) treats `user_text` as the current turn and
renders it itself, so the user message was **double-counted** in the prompt, and
the local cache prefix ended in a volatile `<start_of_turn>user\n` marker that
the next turn overwrote with the model reply — defeating all prefix reuse.

**Fix (Option A — correctness + flat prefill).** Snapshot the **completed**
history first, then record the user turn for the next turn
(`crates/fono/src/assistant.rs`). `ctx.history` now excludes the in-flight turn,
matching the contract every backend builder already assumed (cloud backends
needed no change — they were fed bad input). This removes the duplicate user
message **and** restores prefix nesting: turn N+1 now restores turn N's
`F8ChatPrefix` checkpoint and prefills only the new exchange (flat per-turn cost,
independent of conversation length).

**Fix (Option C — skip re-prefilling the reply).** `generate_with_prefix_cache`
(`crates/fono-assistant/src/llama_local.rs`) now also checkpoints the
**post-generation** KV state (system + history + this turn's user + reply),
emitting `llm.prompt_cache_completed_turn`, so the next turn restores the whole
completed exchange and prefills only the new turn's framing.

**Correction (2026-06-09, later trace run).** The first cut of Option C stored the
raw sampled tokens and never matched — a trace run showed every turn still
restoring only the static 78-token `f8_system` base, with prefill/TTFB growing
(turn 4: 250 prefilled tokens, 3003 ms TTFB). A tokenizer probe pinned the cause:
the KV holds the *sampled* token ids, but next turn the reply re-tokenizes as part
of a longer prompt and BPE **merges the final reply token with the turn-closer**
(`<end_of_turn>` / `<|im_end|>`). So the stored sequence missed being a token
prefix by its trailing token(s), and `find_longest_prefix` rejected the whole
entry. (The leading-space hypothesis was disproven — divergence is at the tail.)
The salvage: store only the longest prefix of the generated sequence that the next
turn reproduces verbatim — the common prefix (`common_prefix_len`) with the
canonical "completed turn" rendering (reply trimmed + closer) — and truncate the
KV cache to that length via `clear_kv_cache_seq` so the saved state's position
count equals the recorded token count (the invariant every other checkpoint
holds). The trace now reports `reusable_tokens` / `dropped_tail_tokens`.
`generate_from_prefilled_context` returns the decoded reply tokens to enable this.

New regression tests (model-free): `cached_prefix_nests_across_turns_under_daemon_flow`
reproduces the exact push/snapshot ordering and asserts each turn's cache prefix
is a string-prefix of the next turn's (and that the current user text never leaks
into the prefix), for Gemma + ChatML; `common_prefix_len_stops_at_first_divergent_token`
locks the trim-to-shared-prefix behaviour. Both fail under the old logic.

**Pending verification.** The Option C salvage performs KV-cache surgery
(`clear_kv_cache_seq` + state save) that cannot be exercised in CI (no full model,
only the vocab-only GGUF). Verify on a real model: a trace run should show
`llm.prompt_cache_completed_turn` with `dropped_tail_tokens` ≈ 1, then turn N+1
restoring a `matched_tokens` count that *grows* with conversation length while
prefill stays flat.

Gate green: `cargo fmt --all -- --check`; `cargo clippy --workspace
--all-targets --features llama-local -- -D warnings`; `cargo test --workspace
--tests --lib`. Plan: `plans/2026-06-09-f8-current-turn-double-count-cache-fix-v1.md`.
Verify empirically by re-recording traces: turn 2+ should show
`llm.prompt_cache_restored` with a growing `matched_tokens` (not a flat 78) and
`llm_ttfb_ms` no longer growing with history.

## 2026-06-09 — Cache trace gaps closed + F8 cold-prefill fixed via longest-prefix restore

Acted on the first real trace run (`/tmp/fono-traces`, 2026-06-09 ~08:39–08:41),
which proved the F8 assistant cache was missing on **every** turn: each assistant
turn did an exact-key lookup only, missed, and cold-prefilled the whole prompt
from `start_pos=0` (`built` 974 ms / 1714 ms on turns 3/4 as history grew), while
the bases pinned at startup sat unused. No `prompt_cache_prefix_match` /
`prompt_cache_restored` ever fired on the assistant path.

- **Workstream A — assistant `turn.finish` scoreboard.** Folded
  `trace.cache_scoreboard()` into the `summary` of the assistant pump's
  `turn.finish` args (all exits, including early aborts) in
  `crates/fono/src/assistant.rs`, matching the dictation/startup paths so the
  most important path now ends with the `{cache_hits, cache_misses,
  cold_prefills, bytes_restored}` headline metric.
- **Workstream B — dictation STT/polish trace events.** Held the dictation
  `TurnTrace` current across the whole post-`key.release` pipeline (STT → polish
  → inject) in `crates/fono/src/session.rs` and added an `stt` lane span around
  the transcribe call, so the existing `polish.*` cache instrumentation
  (`crates/fono-polish/src/llama_local.rs`) finally records and the dictation
  waterfall shows STT timing instead of an empty gap.
- **Workstream C — the real fix (F8 cold-prefill → base restore).** The
  assistant live path (`generate_with_prefix_cache`,
  `crates/fono-assistant/src/llama_local.rs`) now mirrors the F7 polish design:
  - Every assistant checkpoint is inserted **with recorded tokens**
    (`PromptStateCacheEntry::with_tokens`) — both the live `F8ChatPrefix` build
    and the startup/hotkey prewarm — so they can participate in longest-prefix
    matching. Previously they used `::new` (no tokens) and were reachable by
    exact key only.
  - On an exact-key miss the path now calls
    `PromptStateCache::find_longest_prefix` over `[F8ChatPrefix, F8System]`,
    restores the deepest cached prefix (a prior turn's chat prefix — the prompt
    is append-only — or the pinned system base), emits
    `llm.prompt_cache_prefix_match` + `llm.prompt_cache_restored`, and prefills
    only the remaining tokens (`start_pos = matched_len`). A full cold prefill +
    `cold_prefill("no_prefix_match")` happens only when nothing matches.
  - **Framing fix:** the prewarmed `F8System` base was the *bare* `prompt_main`
    text, which is **not** a token-prefix of the live chat prompt (the chat
    prompt wraps the system block in `<start_of_turn>user\n…` / `<|im_start|>
    system\n…`). The new `assistant_base_prefix()` frames the base into the
    model's chat template — exactly mirroring the F7 `chatml_base_prefix` — so it
    is a genuine textual (and, modulo tokenizer boundaries the runtime guard
    catches, token) prefix. A new unit test
    (`assistant_base_prefix_leads_chat_prefix`) asserts this for Gemma + ChatML,
    with and without history, so a future prompt-layout change fails loud.
  - **Dead prewarm removed:** the deprecated `WindowContext` rebuild and the
    `F7System` warmup on the *assistant* backend (F7 polish runs on its own
    backend; the live reply path never restores either) are gone. The hotkey
    prepare now warms only the F8 base; an F7 trigger is a no-op there. The
    `F8System` and `AssistantTools` prewarm are kept.

`crates/fono-core/src/prompt_cache.rs` stays llama-agnostic (only its existing
`with_tokens` / `find_longest_prefix` public API is used; no new deps). Net
effect: turn 2+ restores a base (~tens of ms) instead of cold-prefilling the
whole growing prompt, and the assistant `turn.finish` scoreboard shows a
prefix-restore rather than a cold prefill every turn.

Gate green: `cargo fmt --all -- --check`; `cargo clippy --workspace
--all-targets --features llama-local -- -D warnings`; `cargo test --workspace
--tests --lib`. New test: `fono-assistant` `assistant_base_prefix_leads_chat_prefix`.

## 2026-06-08 — F7 prefix cache: restore-and-suffix + per-context + longest-prefix (plan tasks 19–21)

Completed the F7 (transcription cleanup) side of the layered cache design. The
polish backend had **no** prompt-state cache before this: `format()` built the
full prompt fresh and ran a cold prefill on every dictation.

- **F7 restore-and-suffix (Task 19).** Ported the llama.cpp build/restore glue
  into `crates/fono-polish/src/llama_local.rs`, mirroring the F8 reply path.
  `format()` splits the ChatML prompt into a stable prefix + transcript suffix
  (`build_chatml_prompt_split_*`); `run_inference_cached` restores the deepest
  matching checkpoint and decodes only the suffix. Two independent guards —
  exact `prefix+suffix == prompt` string equality and a token-level
  `starts_with` — make a wrong-state restore impossible; worst case is a safe
  full prefill. The pinned base `<|im_start|>system\n{base_system}` is built
  lazily on first use and pinned, then reused for every dictation.
- **F7 per-context layer (Task 20).** The full per-app system prefix
  (`base + rule_suffix[context]`) is cached under the new `F7Context` layer,
  keyed by content fingerprint, so each focused-app context (CLI / editor /
  browser / terminal-agent) gets its own checkpoint restored exactly on the
  next dictation into that app. `FormatContext::base_system_prompt()` exposes
  the pinnable, context-independent base distinct from the full prompt.
- **Longest-prefix matching (Task 21).** `PromptStateCache::find_longest_prefix`
  (fono-core) returns the deepest cached entry whose recorded tokens are a
  *proper* token-prefix of a new prompt, scoped by runtime + layer set. On an
  exact-key miss the F7 path restores the pinned base and decodes only the
  per-context delta instead of a cold prefill. Fallback chain: exact F7Context
  hit → longest-prefix (pinned base) → cold.

Per-utterance language directive and assistant window context remain dropped
from the cached prefixes per the design discussion.

Gate green: fmt, clippy `--workspace --all-targets --features llama-local
-D warnings`, `cargo test --workspace`. New tests: fono-core prompt_cache 10
(3 longest-prefix), fono-polish 44 (split-reproduction, base-is-a-prefix),
fono-polish traits base_system_prompt prefix invariants.

**Still open:** Task 13/16 quantification — an end-to-end F7/F8 cache-on vs
cache-off benchmark on the real model to put numbers on the warm-dictation win
(the machinery and guards are in; this is measurement, deferred to a hardware
run).

## 2026-06-08 — Cache pinning + shared machinery (plan tasks 17–18)

Executed the first two items of the v2 cache design (layered, per-context
caching with pinned bases).

- **Pinning (Task 17).** Context-independent base prefixes — the F7 cleanup
  base, the F8 system prompt, the tool prompt — are now protected from LRU
  eviction. `PromptStateCache::insert_pinned` marks them; `evict_over_budget`
  skips pinned keys and stops rather than dropping a protected checkpoint. Only
  the most-recent snapshot of a pinnable layer stays pinned: when the active
  prompt/runtime changes (new key) the stale pin is released so it ages out.
  This converts "usually warm under LRU" into a hard guarantee that the next use
  of a base is never a cold prefill, at the cost of ≤3 bounded slots.
- **Shared machinery (Task 18).** Lifted the whole bounded cache
  (`PromptStateCache`, key, entry, layer, LRU + byte budget + pinning) out of
  `fono-assistant` into `crates/fono-core/src/prompt_cache.rs` as a
  **llama-agnostic** data structure: it stores opaque `Vec<u8>` state blobs and
  carries no `llama-cpp-2` dependency, so the polish (F7) backend can reuse it
  without duplication. `fono-assistant` now imports it and keeps only the
  llama.cpp glue (content-fingerprint key, build/restore by prefilling tokens).
  Added an `F7Context` layer for the upcoming per-context (app) cache.

7 unit tests in `fono-core::prompt_cache` (LRU order, touch-bumps-MRU, byte
budget, pinned survives entry-count + byte-budget eviction, repin releases stale
pin, remove_layer clears pin). Gate green: fmt, clippy
`--workspace --all-targets --features llama-local -D warnings`, `cargo test`
(fono-core + fono-assistant 56).

**Next slice:** Task 19 (port the llama.cpp build/restore glue into the polish
backend and wire F7 restore-and-suffix), Task 20 (F7 per-context layer keyed by
the classifier bucket), Task 21 (longest-prefix matching). The design is locked
in the plan; assistant window context and the F7 language directive are both
dropped from the cached prefixes per the design discussion.

## 2026-06-08 — Multi-turn cache benchmark confirms the system-first fix

Added `fono-bench assistant-conversation-cache`: it walks a growing conversation
through the **real** `build_prompt_split` and replays uncached-vs-cached
generation per turn, so it measures the fixed Gemma layout end-to-end (not a
synthetic prefix). Ran a 6-turn conversation on `gemma-4-e2b.gguf` (ctx=4096,
threads=8, batch=4096, 2 iters/turn). Artifact:
`/tmp/fono-runtime-prompt-cache/conversation-cache.json`.

Result confirms the re-ordering pays off **on every turn**, not just turn 1
(which is all the old layout could cache on Gemma):
- State restore is flat ~15–39 ms across the whole conversation, regardless of
  the checkpoint growing 0.5 MB → 6.1 MB (prefix 31 → 333 tokens).
- The cache stands in a ~21 ms restore for a cold prefix prefill that climbs to
  ~4.5 s by turn 6 — the cost the uncached path re-pays every turn (its full
  latency climbs 2.0 s → 6.9 s).
- Cached time-to-first-token stays flat ~341–641 ms (it tracks the ~22–25-token
  suffix, not the growing prefix). Uncached first-token can't arrive until the
  whole prefix is prefilled, so it scales with conversation length.
- `outputs_match` 2/2 on 5 of 6 turns, 0/2 on turn 3 — sampling noise from both
  paths free-running to `MAX_NEW_TOKENS = 384` on synthetic prompts; restored KV
  state is correct. TTFB/restore/suffix-prefill are the stable metrics.

New public API: `LlamaLocalAssistant::replay_conversation_prefix_cache` +
`ConversationPrefixCacheReport`/`ConversationTurnReport`. Full table in the
plan's "Multi-turn benchmark" section. Gate (fmt / clippy
`--all-targets --features llama-local -D warnings` / `cargo test`) green.

## 2026-06-08 — Gemma prompt re-ordered to system-first (multi-turn cache fix)

The Gemma reply builder put the large, stable system/tool prompt in the
*per-turn tail* (`{system}\n\nUser request: {user}` inside the current user
turn) and the rolling history in the *cacheable head*. That is exactly
inverted for KV prefix caching: the expensive immutable text was re-prefilled
every turn while the cheap history was cached. On Gemma it also meant the
`F8ChatPrefix` checkpoint was only ever a valid token-prefix on turn 1 — from
turn 2 on, history preceded system and the cache fell back to a full prefill.

Fix (`crates/fono-assistant/src/llama_local.rs`): the system prompt is now
prepended to the **first** user turn (Gemma's trained convention — no system
role), so the rendered prompt is **strictly append-only**. Leading tokens
(system, then each completed turn) never change as the conversation grows, so
both a boot-built system checkpoint and a per-conversation checkpoint stay
valid token-prefixes turn after turn. The variable user text is the only thing
in the trailing suffix. `build_prompt` is now defined as `prefix + suffix`
(the split is the single source of truth), so the two can no longer diverge.

Regression guards added (all in the `tests` module):
- `gemma_system_leads_prompt_regardless_of_history` — system is a leading
  prefix with and without history, and appears exactly once.
- `gemma_conversation_is_append_only` / `chatml_conversation_is_append_only` —
  each turn's full prompt is an exact string prefix of the next turn's prompt
  across a simulated 3-turn conversation. This is the property that keeps the
  KV cache reusable multi-turn; if a refactor breaks ordering, these fail loud.
- `gemma_history_render_is_stable_across_turns` — a turn renders identically
  once it scrolls into history.

Gate: `cargo fmt --check`, `cargo clippy --workspace --all-targets --features
llama-local -D warnings`, and `cargo test --workspace` all green (56 tests in
`fono-assistant`, 4 new). Empirical multi-turn speedup re-measurement on the
real model is the next step.

## 2026-06-08 — Runtime prompt-state cache: benchmark results (Tasks 14 & 15 executed)

Ran the `assistant-cache-scaling` sweeps on `gemma-4-e2b.gguf` (ctx=4096,
threads=8, batch=4096, ubatch=512, 2 iters × 3 suffixes). Artifacts in
`/tmp/fono-runtime-prompt-cache/cache-scaling-{tools,window}.json`.

Headline: **cached time-to-first-token is flat and prefix-size-independent**
(~78–138 ms across both sweeps), while the uncached path reprocesses the whole
prefix and climbs to ~48–49 s at ~3,300 prefix tokens. State restore is a
near-constant ~15–28 ms; only the small per-turn suffix prefill (~76–132 ms) is
paid each turn. The win scales with prefix size — ~1.1–1.5× at zero
tools/lines, ~33–39× at 40 tools / 96 window lines. Largest checkpoints are
~60–62 MB, so the 256 MiB / 8-entry budget holds ~4 large checkpoints.

Note: the `cached_speedup_x` full-latency ratio and `outputs_match` counts are
noisy because both paths generate up to `MAX_NEW_TOKENS = 384` on synthetic
prompts with no natural stop; TTFB/restore/suffix-prefill are the stable
decision metrics. First full sweep aborted on the 40-tool prefix
(`GGML_ASSERT(n_tokens_all <= cparams.n_batch)`) because the ~3 k-token prefill
exceeded `--batch-size 2048`; rerun with `--batch-size 4096` succeeded.

Task 16 status: latency + memory acceptance criteria met (caching stable
prefixes should default on). Remaining gate is CPU contention — building a
~3,300-token checkpoint costs ~45 s, so large checkpoint builds must stay
low-priority and deferred while STT is CPU-bound. Task 13 (STT-contention
benchmark) closes that third axis; Task 16 stays open until it lands. Full
tables in the plan's "Benchmark Results" section.

## 2026-06-08 — Runtime prompt-state cache: Tasks 14 & 15 (cache scaling benchmarks)

Continued `plans/2026-06-07-2026-06-07-runtime-prompt-state-cache-v1.md`. Added
the `fono-bench assistant-cache-scaling` subcommand that quantifies how cached
prefixes scale along two dimensions, satisfying plan Tasks 14 and 15:

- `--dimension tools --sizes 0,5,10,20,40` sweeps tool/function descriptor count
  (Task 14); `--dimension window --sizes 0,8,32,96` sweeps active-window context
  size (Task 15).
- Each synthetic prefix ends at `User request:` so the per-turn suffix begins on
  a stable token boundary (the same split the live reply path uses) and replays
  through the existing `replay_raw_prompt_prefix_cache` path. Per size the JSON
  report (`assistant-cache-scaling-report-v1`) gives prefix chars/tokens, state
  bytes, one-time setup prefill, median uncached vs cached latency, median TTFB,
  median restore, median suffix prefill, output-match count, and
  `cached_speedup_x`.

Gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
--features llama-local -D warnings`, and `cargo test --workspace --tests --lib`
all pass.

Still open on the plan: Task 13 (STT-contention benchmark — needs the STT
pipeline harness, not just the assistant) and Task 16 (promote the cache policy
on the gathered evidence). Details in the plan's "Tasks 14 & 15 Implementation"
section.

## 2026-06-08 — Runtime prompt-state cache: Task 8 (transcript-ready prefix cache)

Resumed `plans/2026-06-07-2026-06-07-runtime-prompt-state-cache-v1.md`. The
embedded local-assistant reply path now *consumes* the prompt-state cache, not
just builds it — Task 8 ("restore the best available checkpoint and process
only the remaining suffix") is implemented and wired into `reply_stream`.

What landed (`crates/fono-assistant/src/llama_local.rs`):

- `build_prompt_split` splits the rendered reply prompt into a stable prefix
  (history + system framing) and a per-turn suffix (user text + closing
  template). `prefix + suffix` reproduces `build_prompt` byte-for-byte; new unit
  tests assert this for Gemma and ChatML, with and without a system prompt.
- `generate_with_prefix_cache` restores a cached `F8ChatPrefix` checkpoint when
  present (building it lazily on first use), prefills only the suffix tokens,
  then generates. Two independent guards — exact `prefix + suffix == prompt`
  string equality and a token-level `starts_with` check — make a wrong-state
  restore impossible; any incompatibility falls back to a full prefill having
  emitted nothing.
- Removed the previously dead-coded staged helpers (`prompt_prefix_cache_entry`,
  `try_run_inference_with_cached_prefix`, `run_inference_with_prompt_cache`) and
  the unused `remove_layers` WIP; replaced them with the live path above.

Gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D
warnings`, and `cargo test --workspace --tests --lib` all pass (llama-local is
in the default workspace graph, so this is exercised in the real binary).

Still open on the plan: Tasks 13–15 (STT-contention, tool-count, and
window-context benchmarks) and Task 16 (promote the policy on evidence).
Startup/hotkey pre-warm still builds the older raw-prompt checkpoints, which the
reply path no longer restores; pre-warming the exact `F8ChatPrefix` ahead of the
transcript is deferred (the reply-time history snapshot includes the pending
user turn, so the prefix can't be reproduced early) until the benchmarks justify
it. Details in the plan's "Task 8 Implementation" section.

## 2026-06-07 — Runtime prompt-state cache: initial benchmark slice

Started `plans/2026-06-07-2026-06-07-runtime-prompt-state-cache-v1.md`.
This slice added the embedded local-assistant prompt-state cache foundation and a
real-world-shaped benchmark for a cached stable prefix with changing suffixes.

What landed:

- Embedded cache layer types for F7 system, F8 system, assistant tools,
  active-window context, benchmark prefixes, and exact prompts.
- Strict cache keys derived from cache layer, model/runtime identity, prompt
  SHA-256, token SHA-256, and token count.
- A bounded in-memory LRU prompt-state cache for the embedded `llama.cpp` backend
  with an initial 8-entry / 256 MiB budget.
- `fono-bench assistant-prefix-cache`, which prefills one stable prefix once,
  saves the llama.cpp state, restores it for multiple changing suffixes, and
  compares cached vs uncached latency/output.

Benchmark run:

| Metric | Result |
|---|---:|
| Artifact | `/tmp/fono-runtime-prompt-cache/prefix-cache-controlled-release.json` |
| Prefix size | 783 chars / 181 tokens |
| State size | 3,340,938 bytes |
| One-time prefix prefill | 1,836 ms |
| Median restore | 9 ms |
| Median suffix prefill | 147 ms |
| Median cached TTFB | 227 ms |
| Median cached latency | 485 ms |
| Median uncached latency | 2,989 ms |
| Exact output matches | 6 / 9 |

Verification run:

| Step | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo check -p fono-bench --features llama-local` | clean |
| `cargo clippy -p fono-assistant --features llama-local --all-targets -- -D warnings` | clean |
| `cargo clippy -p fono-bench --features llama-local --all-targets --no-deps -- -D warnings` | clean |

Next steps: wire low-priority startup warming for stable F7/F8/tool checkpoints,
add hotkey/window-context restore/extension policy, then add STT-contention,
tool-count, and window-context scaling benchmarks before enabling any production
default policy.

## 2026-06-07 — Local assistant runtime parity: exact prompt replay

Resumed and completed `plans/2026-06-07-local-assistant-runtime-parity-v1.md`.
The benchmark harness can now compare byte-for-byte captured F8 assistant
prompts across Fono's embedded `llama.cpp` assistant runtime and local
OpenAI-compatible server runtimes.

What landed:

- `fono-bench extract-trace-prompt` reads a Chrome Trace / Perfetto JSON file,
  extracts the first event with `args.prompt`, and writes that prompt to a file
  or stdout while reporting prompt length and SHA-256.
- `fono-bench assistant-replay` accepts either a prompt file or trace file,
  records prompt source/length/SHA-256, and emits an
  `assistant-replay-report-v1` JSON report.
- Embedded replay uses the local assistant raw-prompt streaming helper, preserving
  TTFB and delta-count measurements for the in-process `llama.cpp` path.
- HTTP replay sends the raw prompt as a single user message to an
  OpenAI-compatible chat-completions endpoint with streaming enabled, then
  records total latency, time to first token, delta count, output length, and
  output text.
- Clippy cleanup folded the assistant build/runtime metadata argument lists into
  small option structs instead of suppressing `too_many_arguments`.

Verification run:

| Step | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo check -p fono-bench --features llama-local` | clean |
| `cargo test -p fono-bench --features llama-local --lib --bins --tests` | green |
| `cargo clippy -p fono-bench --features llama-local --all-targets --no-deps -- -D warnings` | clean |
| `cargo clippy -p fono-assistant --features llama-local --all-targets -- -D warnings` | clean |

The next runtime-parity step is to run paired embedded/server replays against the
same extracted prompt and compare TTFB, total latency, delta count, output, and
server-side prompt/eval stats where available.

## 2026-06-03 — Phase 4.1: Kokoro local English TTS (engine + router split)

Landed plan v3 task 4.1 — Kokoro is now the local TTS engine for English,
Piper for every other language (ADR 0033). Followed the de-risking-first
plan `plans/2026-06-02-kokoro-local-english-tts-v1.md` (Phases A–G).

**De-risking spike (Phase A, GO).** Converted Kokoro to `.ort` and proved
the load-bearing risk is clear: **zero control-flow ops** (`If`/`Loop`/`Scan`)
in both fp32 and the quantized variant — the exact blocker that omitted 7
Piper voices is absent. Built three throwaway minimal runtimes; the
quantized Piper+Kokoro **union** runtime loads `q8f16` (incl. the
`DynamicQuantizeLSTM` contrib op) **and** all Piper voices together. en-US
IPA mapped 50/50 chars against the embedded espeak core; synthesis produced
clean 24 kHz audio.

**Distribution (Phase B).** Ships the q8f16 variant
(`onnx-community/Kokoro-82M-v1.0-ONNX`, Apache-2.0) shared across four
voices — `af_heart` (en-us, default), `af_bella`, `af_nicole` (en-us),
`bf_emma` (en-gb) — each a 0.5 MiB raw f32 `[510,256]` style pack. Model +
style packs + merged `SHA256SUMS` published to the `ort-1.24.2` release on
`bogdanr/fono-voice` (Piper checksums preserved). `onnxruntime/ops.config`
in `fono-voice` regenerated to the union and its existing CI workflow
rebuilt all triples; `scripts/fetch-onnxruntime.sh` re-pinned per triple
(x86_64-darwin pending its CI job, falls through to source-build).

**Engine + router + schema (Phases C–E).** New
`crates/fono-tts/src/kokoro.rs` (`KokoroLocal`, embedded 178-entry phoneme
vocab, espeak accent per voice prefix, style row by token count, reads the
model's actual input name). Catalog schema extended: `Voice.config` is now
`Option` (Kokoro has no `.onnx.json`), plus optional `style` and
`espeak_voice`. Router (`local_router.rs`) cache generalized to
`Arc<dyn TextToSpeech>` and dispatches on `voice.engine`; English resolves
to `af_heart` via catalog ordering. `crates/fono/src/models.rs` handles the
optional config + style pack.

**Wizard + size (Phases F–G).** Wizard leaves `tts.local.voice` empty so
the router picks Kokoro for English automatically; comment de-Piper-ized.
Measured `release-slim --features tts-local` glibc binary at **25.22 MiB**
(up from the 24.45 MiB Piper-only baseline, +0.77 MiB for Kokoro's ops),
well under the 32 MiB `cpu` cap, with the four-entry `NEEDED` allowlist
intact. Recorded in `docs/binary-size.md`.

**Gate:** `cargo fmt --check`, `cargo clippy` (my crates clean; remaining
warnings are pre-existing fono-core lints flagged only by local clippy
1.96, not CI's pinned 1.88), full workspace tests green incl. an
end-to-end Kokoro synthesis run against the union runtime. ADR 0033's
design is now fully realized — no amendment needed.

**Remaining:** x86_64-apple-darwin union runtime SHA to pin once its CI
job finishes; otherwise 4.1 is complete.

## 2026-06-02 — `cargo build` works without ORT_LIB_LOCATION (dev fallback)

With `tts-local` now source-default, a bare `cargo build` (and rust-analyzer)
linked `ort` and failed `undefined reference to OrtGetApiBase` when
`ORT_LIB_LOCATION` was unset. Fix: re-enable `ort`'s `download-binaries` +
`tls-rustls` in the workspace `Cargo.toml`. `ort-sys` checks `ORT_LIB_LOCATION`
first, so CI/release (which export it via `scripts/fetch-onnxruntime.sh`) still
link our pinned static `libonnxruntime.a` unchanged; only env-less local builds
take the CDN fallback. Build-only deps added (`ureq`, `ureq-proto`, `socks`,
`hmac-sha256`, `lzma-rust2`, `utf8-zero`; rustls/ring/webpki-roots already
present via reqwest) — all permissive, none in the binary. Verified shipped
`release-slim` byte-identical: 26,038,648 B (24.83 MiB), four-entry `NEEDED`,
no leak. fmt/clippy/fono-tts tests green. ADR 0032 amended.

## 2026-06-02 — Local TTS: Romanian comma-below diacritics phonemized

Bug report: Piper cut Romanian words at comma-below `ș`/`ț` — reading "Ploie"
for `Ploiești`, and skipping `țara` entirely — while Home Assistant's Piper
handled the same model fine. Root cause is the vendored pure-Rust `espeak-ng`
0.1.2 port: it only understands the **cedilla** forms (`ş` U+015F, `ţ` U+0163),
not the modern **comma-below** forms (`ș` U+0219, `ț` U+021B). It truncates a
word at the first comma-below letter or drops it. Confirmed empirically with a
throwaway harness against the cached `ro_dict`: comma-below `Ploiești` → `plˈoje`,
cedilla `Ploieşti` → `plˈojeʃtˌʲ`. The real C espeak-ng normalizes comma-below →
cedilla internally; the port skips that step.

Fix: `espeak::normalize_diacritics` folds the four comma-below codepoints
(`Ș`/`ș`/`Ț`/`ț`) onto their cedilla equivalents, applied in
`PiperVoice::phonemize` before `text_to_ipa`. Returns a borrowed `Cow` (no-op)
for text without them, so non-Romanian text is untouched. No new dependency.
Unit-tested; verified all six failing words now phonemize fully. Caveat: the
port has shaky handling of codepoints ≥ U+0100 generally, so other languages
may have their own gaps — a broader audit is a separate task.

## 2026-06-02 — Local TTS: text language is authoritative for voice choice

Persisting bug report: Romanian replies were *still* spoken by the English
voice after the previous two fixes. Root cause was the selection *priority*, not
the detector. On the assistant path `synth_and_enqueue` passes
`metrics.language` — the language the STT engine detected for the **user's
speech** — as the `lang` hint. But the LLM reply can be in a different language
(English question → Romanian answer). The router honoured that hint over the
text, so a Romanian sentence tagged with an `en` input hint got the English
voice.

- **`LocalRouter::voice_for`** now treats the **text being spoken** as the
  authoritative signal: it runs `detect_base_lang(text, &langs)` first and only
  falls back to the caller's `lang` hint when detection is inconclusive (reply
  too short to fingerprint), then to the default voice. Priority is now
  text-detection → STT hint → primary voice (pin still overrides all).
- Added a `tracing::debug!(target: "fono_tts::local_router")` line logging
  `hint` / `detected` / `chosen_lang` / `voice` per utterance, so a recurrence
  can be diagnosed from `RUST_LOG=fono_tts::local_router=debug` rather than by
  guesswork.
- **Verified:** `cargo fmt --check`, `cargo clippy -p fono-tts --features
  tts-local --all-targets -D warnings`, `cargo test -p fono-tts --features
  tts-local` all green; debug `-p fono` builds clean. release-slim rebuild in
  progress.
- Operational reminder: the audio the user hears comes from whichever `fono`
  binary is actually running — the live `fono.speak` MCP channel was a *stale*
  build (compile-time `bundled-data-ro`). The fix only takes effect after the
  running daemon / `fono mcp serve` is rebuilt and restarted from this tree.

## 2026-06-02 — Local TTS text-based language detection (no-hint path)

Follow-up bug report: even after the per-utterance router landed and the
Romanian voice downloaded, replies were still spoken by the English voice. Root
cause: the live audio channel is the MCP `fono.speak` tool, whose `speak_text`
path (`crates/fono-mcp-server/src/voice_io.rs:475`) calls `synthesize` with
`lang = None`. With no hint the router fell back to the primary (English) voice.
The assistant path I wired earlier only helps when STT returns a language.

- **`crates/fono-tts/src/local_router.rs`** now identifies the language from the
  text itself when no `lang` hint is supplied. New pure, unit-tested
  `detect_base_lang(text, allowed)` runs `whatlang` constrained to the user's
  configured `general.languages` (mapped ISO 639-1 → `whatlang::Lang` via
  `whatlang_for_base`). It returns `None` — keeping the default voice — when
  there are fewer than two detectable candidates, when detection is unreliable
  (short text), or when the winner is unmapped. `LocalRouter::new` now takes the
  configured `languages` (deduped to base codes via `dedup_base_langs`).
- **`factory::build_local`** threads `languages` into `LocalRouter::new`.
- **`whatlang` 0.16** added to the workspace + `fono-tts` `tts-local` feature
  (MIT, pure-Rust trigram model, no network/system deps; license graph clean —
  all MIT/Apache/BSD, all already transitively present).
- **Priority:** explicit `lang` hint (STT on the assistant path) > text
  detection (MCP/no-hint path) > primary voice. A `[tts.local].voice` pin still
  short-circuits everything.
- **Verified:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -D
  warnings`, `cargo test --workspace`, `cargo test -p fono-tts --features
  tts-local` all green (6 new detection unit tests, incl. real ro/en sentences).
  `release-slim` rebuilt: 26,034,520 B (24.83 MiB) — under the 26 MiB budget
  (~1.2 MiB headroom), four-entry `NEEDED` unchanged. whatlang adds ~266 KiB.
- Note: the running MCP server binary is still stale (compile-time
  `bundled-data-ro`), so the live `fono.speak` channel needs a daemon rebuild +
  restart to exercise this; the code path is unit-covered.

## 2026-06-02 — Local TTS language router (per-utterance voice selection)

Bug report: with `tts-local` default, a bilingual user heard Romanian replies
spoken by the English voice. Cause: the local backend was monolingual —
`build_local` loaded exactly one `PiperLocal` (resolved from
`languages.first()`), and `PiperLocal::synthesize` ignored the `lang` hint. The
"language router (plan task 2.4)" was the deferred piece.

- **New `crates/fono-tts/src/local_router.rs` (`LocalRouter`).** A
  `TextToSpeech` that keys a lazily-populated `HashMap<voice_name, PiperLocal>`
  and, per `synthesize`, picks the voice for the utterance language via a pure,
  unit-tested `resolve_voice_for_lang` + `base_lang` (`en-US` → `en`). The
  primary voice loads eagerly (preserving the missing-voice error and the
  sample-rate hint); other languages load on first use. An explicit
  `[tts.local].voice` pin disables routing (Cartesia-style pin semantics).
- **`factory::build_local`** now returns a `LocalRouter` instead of a bare
  `PiperLocal`; the engine-load logic moved into the router.
- **`models::ensure_local_tts`** downloads a voice per configured language
  (deduped) when unpinned, so the router can switch voices offline; languages
  with no catalog voice warn and fall back to the primary.
- **`assistant.rs`** threads the STT-detected language (`metrics.language`)
  into `synth_and_enqueue` → `tts.synthesize(sentence, None, Some(lang))`. The
  wizard only writes the *cloud* `tts.voice`, never `tts.local.voice`, so
  routing is active by default for local users.
- **Verified:** `cargo fmt --check`, `cargo clippy --workspace --all-targets
  -D warnings`, `cargo test --workspace` + `cargo test -p fono-tts --features
  tts-local` all green (new router unit tests included). `release-slim` rebuilt:
  **25 788 632 B (24.59 MiB)**, still under the 26 MiB budget. CHANGELOG
  `[Unreleased]` Fixed entry added.

## 2026-06-02 — Fix: `en-us` voice phonemization (catalog dict fold)

Running the freshly-rebuilt `release-slim` binary surfaced
`WARN no espeak dictionary for language "en-us" in the catalog …`. Root cause:
`en_US-amy-medium.onnx.json` declares espeak voice `en-us`, but
`espeak::canonical_lang` passed `en-us` through unchanged, so `dict_for("en-us")`
found nothing (the catalog hosts a shared `en`/`en_dict`, same file espeak uses
for every English variant). The British voice already worked because
`en-gb-x-rp` was folded to `en`.

- **Fix:** `crates/fono-tts/src/espeak.rs` — `canonical_lang` now folds
  `"en-us" | "en-gb-x-rp" => "en"`. Doc comment + unit test updated (the
  pass-through assertion for `en-us` became a fold assertion); the
  catalog-coverage test `canonical_lang_targets_all_have_a_catalog_dict`
  (`voices.rs`) now includes `en-us`.
- This drives both the on-demand dict download (`voices::ensure_*`) and the
  Piper engine's runtime phonemizer (`piper.rs` `Translator::new`), so the
  warning and the downstream phonemization failure both clear.
- **Verified:** `cargo fmt --check`, `cargo clippy --workspace --all-targets
  -D warnings`, `cargo test --workspace --tests --lib` all green; `release-slim`
  rebuilt. CHANGELOG `[Unreleased]` Fixed entry added.

## 2026-06-02 — `tts-local` is now a DEFAULT feature

Flipped `tts-local` into the `fono` default feature set
(`crates/fono/Cargo.toml:36`), so the shipped `cpu`/`gpu` binaries do local
Piper TTS out of the box. Verified the full blast radius and wired every
build path.

- **Cargo:** `default = [… , "tts-local"]`. Default graph now pulls `ort`
  2.0.0-rc.12 + `espeak-ng` 0.1.2 (no `espeak-ng-data-*` crates — bundled-data
  stays off; the G2P core is embedded and dicts download at runtime).
- **Licensing (cargo-deny):** the new default-graph crates are all allowed —
  `espeak-ng` GPL-3.0-or-later, `ort`/`ort-sys` MIT OR Apache-2.0. No
  missing-license data crates, so the 2.2a `[licenses.clarify]` worry is moot.
  The `deny` job reads metadata only (no build), so it needs no lib.
- **CI (`ci.yml`):** the `test` job now fetches + pins `ORT_LIB_LOCATION`
  before fmt/clippy/test (every default build links `ort`). The `size-budget`
  job's per-row fetch is now unconditional; the redundant `cpu-tts-local` row
  was dropped and the `cpu`/`aarch64` budgets raised 24→26 MiB. `xz-utils`
  added where the fetcher runs.
- **Release (`release.yml`):** the `build` (all three variants) and
  `cloud-assistant` (`-p fono` example) jobs fetch + pin the lib; `xz-utils`
  added. `cloud-equivalence` is unaffected (`fono-bench` doesn't pull `ort`).
- **Verified locally with the lib pinned:** `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -D warnings`,
  `cargo test --workspace --tests --lib` all green (`fono-tts` 96 pass, 2
  ignored). Real `release-slim` `cpu` artifact: **25 768 120 B (24.57 MiB)**,
  under the 26 MiB budget, `NEEDED` = exactly the four-entry allowlist
  (`ld-linux`, `libc`, `libgcc_s`, `libm`) — onnxruntime + libstdc++ embedded.
- Docs: `tts-local` feature comments in both `Cargo.toml`s updated;
  CHANGELOG `[Unreleased]` Added entry.

**Next:** confirm a tagged release builds green end-to-end; rebuild/restart any
running `fono mcp serve` so its espeak path has the runtime per-language dict
fetch (the dev box's MCP binary predates the 2.2d dict refactor — `fono.speak`
still errors on `en-us` until that subprocess is replaced).

## 2026-06-02 — Phase 1.4: `tts-local` in the CI size gate; multi-triple ort libs pinned

The hosted minimal `libonnxruntime.a` is now exercised by CI, and the
fetcher is pinned for every triple the mirror hosts.

**Fetcher (`scripts/fetch-onnxruntime.sh`) — re-pinned from the live mirror.**
The `onnxruntime-1.24.2` release on `bogdanr/fono-voice` hosts four libs
(`x86_64`/`aarch64` Linux, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`),
each with a `sha-<triple>.txt` whose `raw_sha256` is the EXTRACTED-library
hash (verified: extracted x86_64 = `943bd160…`, size 56 412 710 matches
`raw_size`). Two fixes:

- The x86_64 pin was **stale** — `9b084ea5…` no longer matches the hosted lib
  (`943bd160…`); the lib was rebuilt for the static-libstdc++ fix and
  re-uploaded but the script was never updated. Left as-is, even the x86_64
  fetch (and the new CI row below) would fail SHA verification. Corrected.
- Added the three other triples: `aarch64-unknown-linux-gnu` (`e14d4e71…`),
  `aarch64-apple-darwin` (`3c60d45f…`), `x86_64-pc-windows-msvc` (`0731b033…`).
  All four confirmed by download+extract+sha here. Ran end-to-end on this
  x86_64 host: download → extract 56 MB lib → SHA verify → exit 0. `sh -n` clean.

**CI size gate (`.github/workflows/ci.yml`).** Added a `cpu-tts-local` row to
the `size-budget` matrix: a `fetch_ort`-gated step runs the fetcher, pins
`ORT_LIB_LOCATION` via `$GITHUB_ENV` (no CDN), then builds
`-p fono --features tts-local` and reuses the size + 4-entry `NEEDED` assert.
Budget 26 MiB (measured 24.45 MiB, under the ≤32 MiB `cpu` cap). A regressed
dynamic `libonnxruntime.so`/`libstdc++.so.6` leak now fails the PR. YAML parses
clean.

**Both prior "default-flip" blockers are now cleared:**

1. **aarch64 hosted lib — DONE.** The lib is hosted *and* pinned in the fetcher
   (above), so a `tts-local` aarch64 build no longer dies at the fetch step.
2. **Default English voice — DONE.** `en_US-amy-medium` is hosted in the
   `ort-1.24.2` release and the catalog hashes match exactly
   (`crates/fono-tts/voices/catalog.json:132,137` = hosted `SHA256SUMS`), so the
   ensure-at-startup download+verify path is sound. The earlier live
   `fono.speak` failure was an empty local model cache on the dev box, not a
   hosting/catalog gap.

**Next (now unblocked, but with real blast radius to handle deliberately):**
flipping `tts-local` into the default feature set means *every* `cargo build`
of `fono` — including the `test`/`clippy` jobs in `ci.yml` and all three
`release.yml` variants — would compile `ort` and require `ORT_LIB_LOCATION`.
So the default-flip must land together with the fetcher step added to those
jobs (and `release.yml`), not on its own. That wiring + the flip is the next
session's work.


## 2026-06-01 — Static libstdc++ linkage for `tts-local` (four-entry allowlist restored)

The `tts-local` ONNX build leaked a dynamic `libstdc++.so.6` into `NEEDED`
(5 entries), violating the linkage allowlist. Fixed by linking libstdc++
statically so the shipped artifact stays portable across glibc Linux hosts.

- **Root cause:** `ort-sys` emits its own `cargo:rustc-link-lib={ORT_CXX_STDLIB}`
  for the C++ runtime, independently of llama's `static-stdcxx`. With the
  previous empty value it fell back to a dynamic `-lstdc++`; with a plain
  `static=stdc++` rustc tried to *bundle* `libstdc++.a` into the `ort-sys`
  rlib at its own compile time, where no search path is visible → build error.
- **Fix:** set `ORT_CXX_STDLIB="static:-bundle=stdc++"` in `.cargo/config.toml`
  — the `-bundle` modifier defers the archive to the **final `fono` link**,
  where the `libstdc++.a` search path (emitted by a new feature-gated
  `crates/fono-tts/build.rs` via `gcc --print-file-name=libstdc++.a`, mirroring
  llama's approach) is present. No hardcoded paths.
- **Measured (`release-slim` glibc CPU artifact):**
  - default (no `tts-local`): 22.52 MiB, 4 `NEEDED`
  - `tts-local`, libstdc++ dynamic (old): 25.33 MiB, **5** `NEEDED` (leak)
  - `tts-local`, libstdc++ static (now): **24.45 MiB, 4 `NEEDED`** — both
    onnxruntime and libstdc++ statically embedded; ~0.9 MiB *smaller* than the
    leaky dynamic state (`--gc-sections` prunes the unused archive).
- **Verified with zero manual flags:** a plain `cargo build -p fono
  --profile release-slim --features tts-local` (only `ORT_LIB_LOCATION` set)
  yields the clean four-entry binary that runs. The build script is
  feature-gated, so default builds emit nothing and are unchanged.
- Gate green: `cargo fmt --check`, `cargo clippy --workspace --all-targets
  -D warnings`, `cargo test --workspace`; `-p fono-tts --features tts-local`
  clippy + tests clean with no manual `RUSTFLAGS`/`ORT_CXX_STDLIB`.
- Docs: ADR 0022 corrected (the prior "llama's static-stdcxx covers ort"
  claim was wrong) and `docs/binary-size.md` updated with the mechanism and
  measured numbers.

Two blockers remain before `tts-local` can become a default feature: wiring
the minimal-runtime build + size/`NEEDED` gate into CI (Phase 1.1/1.4) so a
clean build can obtain the pinned `libonnxruntime.a` automatically. The
libstdc++ leak — the other blocker — is now closed.

## 2026-06-01 — Phase 2.2e: per-language espeak dicts uploaded; lang canonicalization

All catalogued voice languages can now phonemize: the per-language espeak
dictionaries are live on the `fono-voice` mirror and the catalog references
them, closing the "mirror action required" item from 2.2d.

- **Mirror release `espeak-ng-1.52`** on `bogdanr/fono-voice`: 38 distinct
  `<lang>_dict` files (13.5 MiB total), extracted from the espeak-ng 1.52
  data set (GPL-3.0-or-later, via the `espeak-ng` crate's data). Files are
  named by their canonical espeak base code. Verified downloadable;
  `ro_dict` matches the catalog seed hash.
- **Catalog `dicts` array regenerated** to 38 entries (one per distinct
  base dictionary), each SHA-256 + size pinned. 42 voices → 40 distinct
  `espeak.voice` codes → 38 physical dicts (two pairs share a base).
- **Language canonicalization** (`crate::espeak::canonical_lang`): folds
  espeak voice *variants/aliases* onto the base dictionary that actually
  exists — `nb→no`, `zh→cmn`, `en-gb-x-rp→en`, `es-419→es`, identity
  otherwise. The espeak phoneme-table lookup needs the base language code,
  not the variant, so the canonical code is used both when choosing which
  dict to download (`ensure_voice_dict`) and when constructing the
  `Translator` (`phonemize`). Without this, variant/alias voices failed at
  the phoneme-table stage even with the dict present.
- **Verified end-to-end against the live mirror**: downloaded the German
  and Chinese voices + their dicts straight from the release, phonemized
  with the embedded core — German clean (`hˈaloː das ɪst aɪn tˈɛst`),
  Mandarin produces phonemes without error (espeak's Mandarin G2P is
  inherently rough — a downstream voice-quality matter, not a
  data-completeness one). All 40 codes phonemize with zero failures.
- **Tests**: three catalog guards added in `fono-tts::voices` — the
  Romanian seed, the full 38-dict well-formedness/`<lang>_dict` naming
  check, and that every `canonical_lang` target has a hostable dict.

**Gate:** `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, `cargo test --workspace` all green;
`-p fono-tts --features tts-local` → 48 pass, 2 ignored. Default `fono`
graph still excludes the feature.

**Regeneration:** `scripts/gen-espeak-dicts.sh` produces the dict assets +
manifest; re-run + re-upload to bump the espeak data version.

## 2026-06-01 — Phase 2.2d: espeak G2P core embedded; per-language dicts download

Removed the compile-time `bundled-data-ro` espeak dependency and moved
to a runtime model: a tiny shared phoneme core ships in the binary, and
each voice's language dictionary downloads from the `fono-voice` mirror
alongside the `.ort` voice — so all 38+ catalogued voices work without
bloating the binary with per-language data (measured ~14 MiB if all
bundled; Russian alone 8.5 MiB).

- **Upstream patch prepared** (`/tmp/espeak-ng-rs`, branch
  `phondata-optional`): `PhonemeData::load` no longer requires the
  ~550 KiB `phondata` synthesis blob when only phonemizing — a missing
  file is treated as "synthesis disabled" (tables load, rate defaults to
  22.05 kHz); a present-but-truncated header still errors. Committed
  under the maintainer-style identity with a plain commit message and a
  `PR_DESCRIPTION.md`. Verified: Romanian + English phonemize with **no**
  `phondata` present at all. This removes Fono's reliance on the 8-byte
  stub trick once it lands upstream.
- **Embedded G2P core** (`crates/fono-tts/assets/espeak-core`, ~104 KiB):
  real `phontab` (59K) + `phonindex` (43K) + `intonations` (2K) + an
  8-byte `phondata` header stub. Vendored with `scripts/gen-espeak-core.sh`
  for provenance. `crates/fono-tts/src/espeak.rs` materialises it into the
  voice data dir via `include_bytes!`.
- **Per-language dict download** (`fono-tts::voices`): catalog gains a
  `dicts` array (SHA-256 + size, seeded with `ro_dict` 68538 B);
  `ensure_dict` fetches `<lang>_dict` into `voices_dir/espeak/` through the
  pinned `fono-download` flow. `ensure_voice` boxed to satisfy
  `clippy::large_stack_frames`.
- **`PiperVoice::new`** drops `install_bundled_language`; it installs the
  embedded core then expects the language dict already staged in the data
  dir. `scripts/gen-espeak-dicts.sh` produces the dict assets + manifest
  for the mirror.

**Gate:** `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, and `cargo test --workspace` all green;
`-p fono-tts --features tts-local` → 45 pass, 2 ignored. Both ignored
end-to-end tests (Romanian text→IPA→ids, and full ONNX synthesis) pass
with the embedded core + a staged `ro_dict`, producing real audio.
Default `fono` graph still excludes the feature.

**Mirror action required:** upload per-language `<lang>_dict` assets
(run `scripts/gen-espeak-dicts.sh`) to the `fono-voice` mirror for every
catalogued voice's language, and add their SHA-256/size to the catalog's
`dicts` array. Only `ro_dict` is seeded so far — other languages will
fail `ensure_dict` until uploaded.

**Next:** populate the catalog `dicts` for all shipped voice languages;
open the espeak-ng-rs PR; then 4.1 (Kokoro for English) + the router
Kokoro-vs-Piper split.

## 2026-05-31 — Phase 2.4/2.5: local TTS now user-selectable

The local Piper engine is wired all the way through to config — a user
can now run `fono use tts local` and the daemon downloads, verifies,
caches, loads, and serves the voice. This closes the gap flagged in the
previous commit (the engine existed but wasn't reachable).

- **`TtsBackend::Local`** added to `fono-core` with a `[tts.local]`
  config block (`voice`, `base_url`). All exhaustive call sites updated:
  `parse_tts_backend`/`tts_backend_str`/`all_tts_backends`,
  `configured_tts_backends`, doctor's TTS provider listing, the wizard
  short-label, and the tray menu label.
- **Factory `Local` arm** (`fono-tts::factory::build_local`): resolves
  the catalog voice (explicit `[tts.local].voice`, else first voice for
  `general.languages[0]`), loads the cached `.ort` + `.onnx.json` via
  `PiperLocal`, materialising embedded espeak data. `build_tts` gained a
  `voices_dir` parameter, threaded through every caller (session, doctor,
  speak_stream, mcp-server, smoke example).
- **Auto-download at startup** (`fono::models::ensure_local_tts`, boxed
  to satisfy `clippy::large_futures`): when `[tts].backend = "local"`,
  `ensure_models` fetches the voice from the `fono-voice` mirror and
  verifies it against the committed catalog SHA-256 before the factory
  loads it — mirroring the whisper/LLM ensure flow.

**Gate:** `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings` (and `-p fono --features tts-local`), and
`cargo test --workspace` all green (153 pass, 1 ignored; `fono-tts
--features tts-local` 42 pass). Default `fono` graph still excludes the
feature.

**Next:** 2.6 (drop app-release `.sha256` sidecars; point `fono-update`
at `SHA256SUMS`), then 4.1 (Kokoro for English) which also lands the
router's Kokoro-vs-Piper split, and the espeak per-language dict fetch.

## 2026-05-31 — Phase 2.2b: PiperLocal ONNX inference + measured size

End-to-end local Piper synthesis works. With the support files dropped
into `./tmp` (a prebuilt **minimal** `libonnxruntime.a`, the converted
`ro_RO-mihai-medium.ort`, and a python venv with onnxruntime 1.24.2), I
unblocked the previously CI-gated inference path and validated the build
tooling.

- **`PiperLocal`** added to `crates/fono-tts/src/piper.rs`: builds an
  `ort::Session` from the `.ort` model (graph optimisation disabled via
  the `recover()` idiom for minimal-build compatibility), runs the
  standard single-speaker VITS signature (`input` ids, `input_lengths`,
  `scales[noise, length, noise_w]`) → f32 PCM at the voice sample rate.
  Implements `TextToSpeech`.
- **Verified end-to-end** (`#[ignore]`d test, run here with the real
  artefacts): synthesises >0.5s of Romanian audio, peak amplitude in
  range, against the minimal 10-operator VITS `libonnxruntime.a` + the
  converted `.ort` model.
- **Build tooling validated:** `scripts/gen-ort-models.sh` runs clean
  with the venv python (10-op `ops.config` + `.ort` produced);
  `scripts/build-onnxruntime-minimal.sh` updated with the three
  container/root build flags from the user's working `tmp/build-ort.sh`.
- **Measured size (the number Phase 1.4 was waiting on):** the minimal
  ONNX runtime adds only **~2.1 MiB** to a release binary (`opt-level=s`
  + LTO + strip + `--gc-sections`) for the Piper op set — far below the
  ~7–11 MiB estimate. The `.a` is ~50 MiB on disk but `--gc-sections`
  prunes everything the fixed op set never references. `NEEDED` = exactly
  the four-entry allowlist; onnxruntime statically embedded. ADR 0022,
  `docs/binary-size.md`, and plan v3 updated with the real figure.

**Gate:** `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, `cargo test --workspace --tests --lib` all
green; clippy + tests green for `-p fono-tts --features tts-local` (36
pass, 1 ignored). Feature absent from the default `fono` graph.

**Next:** Phase 1.4 (CI size gate building the minimal `.a` + asserting
the cap on the real `fono` binary), then 2.3 (voice download/cache),
2.4 (router), 2.5 (factory + Wyoming wiring for end-to-end playback).

## 2026-05-31 — Phase 2.2a: Piper front half (phonemize + id encoding)

Landed the deterministic, unit-testable front half of the local Piper
engine on the `tts-local` feature — everything up to (but not including)
the `ort` inference call:

- **`espeak-ng = 0.1.2`** added to `[workspace.dependencies]`
  (`default-features = false`, GPL-3.0-or-later — compatible). It is a
  **pure-Rust** eSpeak NG port: no system `libespeak-ng`, no C, language
  data embedded per-voice. `tts-local` enables `espeak-ng/bundled-data-ro`.
- **`crates/fono-tts/src/piper.rs`** (new, feature-gated):
  - `PiperConfig` — parses the `<voice>.onnx.json` sidecar (audio,
    espeak, inference, `phoneme_id_map`); unknown fields ignored.
  - `phoneme_ids` — canonical piper-phonemize layout (BOS, interspersed
    PAD, EOS; unmapped codepoints skipped), verified against the real
    `ro_RO-mihai-medium.onnx.json` (`_`=0, `^`=1, `$`=2).
  - `PiperVoice` — installs embedded espeak data once per voice, then
    `text → IPA → ids`.
- **De-risked for real:** the pure-Rust phonemizer compiles and produces
  correct Romanian IPA (`"Bună ziua" → "bˈunə zˈiwa"`). 6 unit tests
  incl. a Romanian end-to-end against `bundled-data-ro` — all green, no
  network, no system espeak.

**Gate:** `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, `cargo test --workspace --tests --lib` all
green; clippy + tests also green for `-p fono-tts --features tts-local`
(6/6 piper tests pass). Feature stays absent from the default `fono`
graph. Doctests skipped locally (no `rustdoc`; CI runs them).

**Licensing follow-up (recorded, not yet blocking):** the transitive
`espeak-ng-data-phonemes` / `espeak-ng-data-dict-ro` crates ship no
`license` field upstream (data is GPL-3.0-or-later). Not seen by CI
cargo-deny today (`all-features = false`, feature off); needs a
`[licenses.clarify]` entry before `tts-local` graduates to the checked
build. Tracked in plan v3 Phase 2.

**Next:** Phase 2.2b — feed the ids through an `ort` session
(`.ort` Piper model) to f32 PCM; needs the minimal-build runtime +
converted model from the CI build step.

## 2026-05-31 — Phase 1.2 verified: ort wired + static-link proven (plan v3)

Wired the ONNX Runtime into the workspace and **verified the static-link
invariant on real code** (not just the throwaway spike crate):

- **`ort 2.0.0-rc.12`** added to `[workspace.dependencies]` with
  `default-features = false` (drops `download-binaries`/`tls-native`/
  `copy-dylibs`): release builds link a pinned `libonnxruntime.a` via
  `ORT_LIB_LOCATION`, never the CDN. `api-24` matches onnxruntime 1.24.2.
- **`tts-local` feature** on `crates/fono-tts` (+ new `local` module:
  `RUNTIME_API_VERSION`, `ensure_runtime()`), propagated through the
  `fono` crate. **OFF by default** — `cargo tree -p fono -i ort` shows
  `ort` is absent from the default graph (zero bytes in the canonical
  binary); it appears only with `--features tts-local`.
- **Verification:** built the `fono-tts` test binary against the cached
  real 1.24.2 `libonnxruntime.a` (`ORT_LIB_LOCATION` + Fono's static-
  libstdc++ flags). Result: onnxruntime **statically embedded** (19,611
  `Ort*` symbols pulled in — genuine link, not a no-op), `NEEDED` =
  **exactly the four-entry allowlist** (`ld-linux`, `libc`, `libgcc_s`,
  `libm`; no `libstdc++.so.6`, no `libonnxruntime.so`), and the
  `ensure_runtime()` test runs. Confirms ADR 0032's core claim on real code.
- **Drive-by fix:** `factory.rs` test imports (`TtsCloud`/`TtsWyoming`)
  now cfg-gated to the features that use them — a latent unused-import
  that only surfaces in isolated (non-cloud) feature builds like
  `tts-local`.

**Gate:** `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -D warnings`, and `cargo test --workspace --tests --lib`
all green; clippy + tests also green for `-p fono-tts --features
tts-local`. Doctests skipped locally (no `rustdoc`; CI runs them).

**Environment note:** the 1.1 minimal onnxruntime build was **not** run
here — `protoc` missing, no python `onnxruntime`, cmake is 4.x (1.24.2
wants 3.28), `/tmp` has 5.2 G. Confirms it belongs in CI.

**Next:** run `scripts/build-onnxruntime-minimal.sh` in CI to produce +
pin the minimal `libonnxruntime.a`, enable `xnnpack`, then Phase 1.4 (CI
size gate, `cpu` cap → 32 MiB) and Phase 2.2 (`PiperLocal` engine).

## 2026-05-31 — Phase 1: minimal-build tooling + version pin (plan v3)

Started Phase 1 (minimal ONNX Runtime build infrastructure). Verified the
load-bearing version pin and landed the two foundation scripts:

- **Version pin corrected:** `ort 2.0.0-rc.12` → `ort-sys 2.0.0-rc.12`
  links **onnxruntime 1.24.2** (pyke `ms@1.24.2`, read from `ort-sys`'s
  `build/download/dist.txt`), **not 1.26** as the spike note said. The
  hand-built static lib must match this tag for ABI compatibility.
- **`scripts/gen-ort-models.sh`** (Task 1.3) — converts `.onnx` → `.ort`
  and emits `ops.config` via onnxruntime's `convert_onnx_models_to_ort`
  with type reduction; seeded with the Piper `ro_RO-mihai-medium` voice.
  The standing pipeline every future model plugs into.
- **`scripts/build-onnxruntime-minimal.sh`** (Task 1.1) — clones
  onnxruntime `v1.24.2`, runs the documented minimal/MinSizeRel build
  consuming `ops.config`, merges the per-target `.a` files into one
  `libonnxruntime.a` for `ORT_LIB_LOCATION`.

Both scripts are pinned, commented, and `sh -n` syntax-clean. They are
recipes that run in CI / on a capable host (~45-min networked compile);
they were not executed in this session.

**Not done (gated on the artefact above):** Phase 1.2 (`ort` wired via
`ORT_LIB_LOCATION`, `download-binaries` off) and Phase 1.4 (CI size gate
+ `cpu` cap → 32 MiB) need a real `libonnxruntime.a` to link/measure, so
they were deliberately left unwritten rather than shipped red or wired to
the forbidden full-CDN download. No Rust changed; the tree stays green.

**Next:** run `build-onnxruntime-minimal.sh` in CI to produce + pin the
artefact, then do 1.2 + 1.4 and measure the real `fono` size/`NEEDED`.

## 2026-05-31 — Voice stack pivots to ONNX Runtime (plan v3 + ADR 0032)

Followed the static-ONNX spike (below) with the owner's decision: **Fono
is a full local voice stack, and it runs on statically-linked ONNX
Runtime**, built minimally to stay small, with shared-ggml as a later
size offset.

**Spike (decisive evidence):** built a real binary on `ort 2.0.0-rc.12`
(onnxruntime 1.24.2 — corrected from the earlier "1.26" note).
onnxruntime links **statically** (no
`libonnxruntime.so` in `NEEDED`); with Fono's existing static-libstdc++
mechanism the binary presents **exactly the four-entry allowlist** and
runs. Full prebuilt adds **~19 MiB**; a custom **minimal build**
(`--minimal_build --include_ops_by_config` from our ORT-format model set,
pinned via `ORT_LIB_LOCATION`) targets **~7–11 MiB**. HA's Piper is the
same onnxruntime shipped dynamically in a container — no lighter engine
to copy. ONNX has **no Vulkan EP** (Dawn/WebGPU is dynamic → would break
the allowlist); voice models are CPU-realtime, so the runtimes split:
ggml-Vulkan for whisper-large + LLM, ONNX CPU-only (XNNPACK) for the
voice stack.

**Landed this session (docs/decisions foundation):**
- **ADR 0032** — ONNX Runtime as the voice-stack platform (new).
- **ADR 0022** amended — supersede the ggml-reuse TTS line; ONNX minimal
  build + dedup offset; `cpu` cap → **≤ 32 MiB**; allowlist unchanged.
- **ADR 0004** amended — per-model licensing (Piper GPL; Kokoro / Silero /
  Zipformer / KWS Apache); engines run on ONNX, not ggml.
- **`docs/binary-size.md`** (new) — the consolidated "keeping Fono small
  and capable" engineering guide (invariants, runtime split, size levers,
  the per-model `ops.config` discipline, add-a-capability checklist).
- **Plan v3** `plans/2026-05-31-local-tts-onnx-voice-stack-and-wyoming-server-v3.md`;
  v2 banner-superseded (retained for its spike evidence trail).

**Next:** Phase 1 — stand up the minimal onnxruntime static build in CI +
`ORT_LIB_LOCATION` pin + ORT-format/`ops.config` tooling, then Phase 2
(Piper-on-`ort`, Romanian first). Phase 2a Wyoming TTS server already
ships and is unaffected.

## 2026-05-31 — Local TTS: plan v2 + Wyoming TTS server (Phase 2a complete)

Audited and rewrote the local-TTS plan, then landed the first code phase.

**Plan/decision groundwork**
- New authoritative plan `plans/2026-05-31-local-tts-ggml-piper-kokoro-and-wyoming-server-v2.md`;
  v1 banner-deprecated. Direction: **ggml-reuse** substrate (small binary, rides the
  existing Vulkan backend) — TTS lands in the canonical CPU + Vulkan builds, **no separate
  variant**. Kokoro-ggml feasibility spike scheduled *after* Phase 2b, just before Kokoro work.
- ADR 0022 amended (dropped the `fono-tts` third-variant strategy; size reframed around the
  canonical binary). ADR 0004 corrected: Piper is now `OHF-Voice/piper1-gpl`, **GPL-3.0**
  (was MIT); fine to link for a GPL-3.0 project.

**Phase 2a — Wyoming TTS server endpoint (decoupled from any local engine; done):**
- Codec TTS types (`Synthesize`, `TtsProgram`, `TtsVoice`, `Info.tts`, `SYNTHESIZE`) were
  already in `fono-net-codec`.
- Server-side `handle_synthesize` + `dispatch_synthesize` stream `audio-start` →
  `audio-chunk*` → `audio-stop` (int16 LE mono) from any bound `TextToSpeech`;
  `build_info` advertises an `info.tts` program only when voices are configured.
  `WyomingServer::with_tts` / `with_fixed_tts` + `TtsProvider` mirror the STT provider.
- `[server.tts]` config block (`enabled`, `voices`, `default_voice`) in `fono-core`.
- Daemon wiring: binds the orchestrator's `tts_snapshot()` to the listener when
  `[server.tts].enabled`; mDNS `caps` gains `"tts"` via `wyoming_caps()`. TTS rides the
  existing `[server.wyoming]` listener (one port; Wyoming multiplexes by event type).
- Tests: synthesize framing/empty/full-scale round-trips, `build_info` tts-branch,
  `[server.tts]` config round-trip, `wyoming_caps`. `cargo fmt`, workspace `clippy -D
  warnings`, and the new tests all pass.
- **Remaining (2a.8):** live Home Assistant discovery + `tts.speak` verification, and the
  `docs/providers.md` note — needs a running HA instance.

**Phase 1 (shared-ggml) — feasibility spike done; DEFERRED (owner chose Option B):**
- No external-ggml CMake knob exists: `whisper-rs-sys-0.15.0/build.rs` unconditionally builds
  and links whisper.cpp's bundled ggml (`build.rs:312-316`). Only the fork-and-drop-ggml path
  is viable.
- The two ggml copies are **different revisions** — `ggml.h` differs by 77 lines (whisper
  102,112 B vs llama fork 104,314 B); the llama fork carries newer backends. Sharing one binary
  needs ABI reconciliation + a published `whisper-rs-sys` fork, not a flag flip.
- **Decision:** ship Piper first on the existing `--allow-multiple-definition` trick (temporary
  +~7 MB); land shared-ggml later as a pure size-reclaim pass. Plan + ADR 0018/0022 cross-refs
  updated; phase order reworked in the v2 plan.

**Phase 2b (Piper-on-ggml) — scope correction surfaced (not yet started):** verified three
prerequisites are net-new — no ggml binding is exposed to our code (no `ggml-sys`; `whisper-rs-sys`
has no `links` key), no espeak-ng crate, and Piper voices ship as **ONNX not GGUF** (needs
weight conversion + a hand-written VITS/HiFi-GAN graph). So 2b.2 is a model port of the same
risk class as Kokoro. Recommended a Piper-ggml micro-spike to gate it (documented in the v2 plan).

**Next:** run the Piper-ggml micro-spike (ggml-binding approach, ONNX→GGUF for one Romanian
voice, espeak-ng phonemization) before writing engine code; optionally complete 2a.8 (live HA
verification) when an HA instance is available.

## 2026-05-29 — Visual context for agents and assistant

Built the full visual-context feature end-to-end across `fono-core`, `fono-mcp-server`,
and the daemon/assistant layer.

**`fono-core::screen_capture`** — `GrabberProbe` with four probe ladders:
- Wayland-auto: portal (`xdg-desktop-portal`), `grim`, `scrot`, `maim`, `spectacle`,
  `gnome-screenshot`, `import` (Xwayland fallback)
- Wayland-interactive: portal with region, `grim+slurp`, `scrot -s`, `maim -s`,
  `spectacle -r`, `gnome-screenshot -a`
- X11-auto and X11-interactive mirrors of the above
- Rungs ordered lightest/fastest first; portal preferred on Wayland, scrot/maim on X11
- Privacy gate: blocks `Automatic` mode when the focused window is on the private-window
  list (by WM_CLASS / app-id); returns `CaptureError::PrivateWindow`
- PNG IHDR parser to extract dimensions without an image crate dependency
- Optional downscale via `magick convert` (configurable `max_dimension`)
- Terminal-text fast-path: for known terminal emulator classes (kitty, alacritty, wezterm,
  foot, xterm, gnome-terminal, konsole, …) captures the pane text via `tmux capture-pane`
  or GNU screen; avoids a pixel screenshot entirely when text suffices

**`fono.screen` MCP tool**:
- `mode`: `"automatic"` (no user gesture) or `"interactive"` (crosshair/region picker)
- Returns an MCP `image` content block (base64 PNG) plus a `metadata` JSON text block
  (dimensions, rung used, terminal_text if present, timestamp)
- Error handling: `PrivateWindow` → 403-style error text; `Cancelled` → user-cancelled
  message; `NoToolAvailable` → actionable install hint
- Tray flashes amber during capture; restores previous icon on completion
- Registered in `ToolRegistry` alongside existing tools

**`fono_screen` LLM tool**:
- Included in the assistant chat request when `prefer_vision = true` and the active
  provider is vision-capable (OpenAI, Anthropic, OpenRouter with vision models)
- Handles both OpenAI-compatible (`tool_call`) and Anthropic (`tool_use`) wire formats
- Model decides autonomously when to call it — no hardcoded trigger phrases
- First built-in action tool; the same plumbing underpins the upcoming Voice Actions phase

**`fono doctor` screen capture section**:
- Reports session type (Wayland/X11/unknown) and active compositor hint
- Per-rung availability: `✓ available` or `[missing: <binary>]` for each of the ~7 rungs
- Shows which rung would be selected for auto vs interactive capture

**Docs / meta**:
- ADR 0031 (`docs/decisions/0031-screen-capture-architecture.md`) — records probe-ladder
  design, privacy gate rationale, terminal-text fast-path, and why no image crate dep
- `docs/providers.md` — screen-capture tool requirements section added
- `CHANGELOG.md` — `[Unreleased]` section updated with all new items
- `ROADMAP.md` — visual context item moved from In Progress → Shipped

Pre-commit gate:

| Step | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --tests --lib` | green — all tests pass |

No new Cargo dependencies added.

## 2026-05-29 — screen_capture.rs pre-commit gate clean

`crates/fono-core/src/screen_capture.rs` was already implemented (tool-ladder probe,
privacy gate, terminal-text fast-path, PNG IHDR parser, `GrabberProbe::detect`,
`GrabberProbe::capture`, downscale via `magick`). This session ran the pre-commit gate
and fixed two clippy errors that were present:

- `and_then(|_| focused_pid)` → `and(focused_pid)` (unnecessary lazy eval)
- `terminal_text: terminal_text.clone()` → `terminal_text` (redundant clone — value
  was dropped immediately after)

`cargo fmt --all` was also run to fix two trailing-blank-line and one long-line diff in
`screen_capture.rs` and `session.rs`.

Pre-commit gate (all three steps):

| Step | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --tests --lib` | green — 14 screen_capture tests pass |

## 2026-05-27 — 3D overlay: Terrain + Blob landed (Phase 2 + 3)

Followed up the Lissajous slice with the remaining two 3D styles
from `plans/2026-05-27-3d-overlay-visualisations-v1.md`:

- **Terrain 3D** (`WaveformStyle::Terrain3d`) — a wireframe
  spectrogram landscape. Reuses the FFT capture tap and the
  heatmap colour ramp; renders a 28 × 24 vertex grid as two
  passes of depth-faded polylines (one per time slice, one per
  frequency column). No new audio plumbing. Synthetic idle
  ripple keeps the terrain alive during silence.
- **Blob 3D** (`WaveformStyle::Blob3d`) — a stretched 42-vertex
  icosphere with hand-baked vertex / triangle tables, filled
  triangles via the `r3d::draw_triangle_3d_filled` primitive,
  Lambert shading from the upper-left. Radius breathes with the
  live RMS level; spectral centroid tilts the lean along X.
  Unit tests guard the icosphere table size and confirm every
  vertex sits within 5 % of the unit sphere.

Both styles share the existing FFT / level taps in
`crates/fono/src/session.rs` (recording path) and
`crates/fono-mcp-server/src/voice_io.rs` (MCP visualizer task);
the assistant-thinking path pushes a slow synthetic FFT ridge
for terrain and a breathing centroid for blob.

Tray entries added with descriptive sub-labels
(`"Terrain 3D (spectrogram landscape)"`,
`"Blob 3D (audio-reactive orb)"`). Daemon index map extended to
6 / 7. Pre-commit gate green (fmt, clippy, all tests except the
pre-existing `resolve_auto_stop_falls_back_to_default` failure).

CHANGELOG updated under `[Unreleased]`. Default style remains
`Fft` so existing configs are unaffected.

## 2026-05-27 — 3D overlay: Lissajous wire (Phase 0 + 1)

First slice of the 3D overlay visualisations plan
(`plans/2026-05-27-3d-overlay-visualisations-v1.md`) is in. Phase 0
adds a small CPU 3D primitives module
(`crates/fono-overlay/src/r3d.rs`) — `Vec3`, `Mat4`, perspective +
look-at + rotation, point projection, AA line draw, polyline draw,
and a depth buffer — with unit tests. No new dependencies.

Phase 1 wires the **Lissajous 3D** waveform style end-to-end: new
`WaveformStyle::Lissajous3d` variant, recording-time PCM tap shares
the existing oscilloscope path, assistant-thinking synthetic
samples follow the oscilloscope pattern so the curve breathes
during silence and thinking, tray submenu picks it up. Software
rasterised, no GPU. Pre-commit gate green (fmt, clippy);
`cargo test` clean except for one pre-existing failure in
`fono-mcp-server` (`resolve_auto_stop_falls_back_to_default`,
unrelated to this work — present on `main` HEAD before the change).

Phases 2 (spectrogram terrain) and 3 (audio-reactive blob) are
gated on a live eyeball pass of Lissajous per the plan's checkpoint
schedule.

## 2026-05-26 — Voice loop for coding agents squashed; v0.9 prep

All 23 commits from this work day were squashed into a single commit on
`main`. The squash also dropped a `target-cpu/` build-artifact directory
that had been accidentally committed earlier in the day; `.gitignore` now
covers `target-cpu/` and `target-gpu/`.

The combined work lands as one user-facing feature in the `[Unreleased]`
CHANGELOG block: **voice loop for coding agents (early preview)**. The
MCP server (`fono-mcp-server` crate), the three voice tools
(`fono.speak`, `fono.listen`, `fono.confirm`), the one-shot
`fono agent-setup` helper, the overlay + tray integration, the
background-speech relevance filter, and the supporting docs/ADR all
ship together. Disabled by default; opt in with
`fono use mcp-server on`. Frame is **early preview** — we expect the
protocol, defaults, and tool surface to keep shifting between v0.9 and
the stable release.

Window-aware dictation already shipped in v0.8.2 last night; the
small `fix(context)` that went out today (focus capture at press
time + i3/XWayland WM_CLASS parsing) lands silently as part of the
squash, no changelog entry.

ROADMAP's "Voice loop for coding agents" section now says **early
preview, shipping in v0.9** and warns about breaking changes between
v0.9 and stable. The "Recently shipped" badge will move to v0.9 when
the release is cut.

**Where we are on v0.9:** close, not there yet. The feature surface is
in, the pre-commit gate is green, but the user wants another bug-fix
pass before tagging. Tag is **not** going out in this session.

## 2026-05-26 — MCP listen overlay + silence parity (v7 plan complete)

Landed `plans/2026-05-26-mcp-listen-overlay-and-silence-parity-v7.md` end to
end (Slices 0–8, nine commits on `main`):

- **Slice 0** — Extracted shared voice helpers into
  `crates/fono-mcp-server/src/voice_io.rs`; added `[mcp]` config block
  with `listen_silence_ms` (default 10 000), `listen_max_seconds`
  (default 45), `relevance_filter` (mode + LLM endpoint), and
  `daemon_ipc_candidates`.
- **Slice 1** — `fono.listen` now opens the same overlay window the
  hotkey path uses, scoped to the listen phase via an `OverlayGuard`
  RAII so it always tears down on early return / panic.
- **Slice 2** — Overlay shows the pondering animation between
  utterances and a walk-progress bar against `listen_max_seconds`.
- **Slice 3** — Multi-utterance loop: keep listening until silence
  ≥ `listen_silence_ms` accumulates after at least one captured
  utterance, with the cheap regex/keyword relevance heuristic
  dropping obvious off-topic chatter.
- **Slice 4** — Optional LLM relevance classifier (off by default,
  `relevance_filter.mode = "llm"`) sitting behind the heuristic for
  when the noise floor is too noisy for keywords alone.
- **Slice 5** — Added an `Ignoring` overlay state (dim grey badge)
  shown the moment the filter rejects an utterance so the user sees
  *why* their words didn't land.
- **Slice 6** — Daemon co-existence: MCP server probes the daemon
  IPC socket; if reachable, it uses the daemon's audio device lock
  instead of grabbing the mic directly, so push-to-talk and
  `fono.listen` no longer fight over ALSA.
- **Slice 7** — Tray feedback over IPC. New `McpPhase` enum
  (Listening / Speaking / Confirming) and
  `Request::{McpActivityStart, McpActivityEnd}` wire format. Daemon
  keeps a shared `(depth, baseline_state)`; 0→1 snapshots and flips
  the tray to `TrayState::Processing` (amber — reusing the existing
  STT/polish colour, no new variant per the v7 palette decision);
  →0 restores the baseline iff the tray is still amber (last-writer
  wins). `McpActivityGuard` RAII fires Start on construction and End
  on Drop, gated to no-op when the daemon socket is unreachable so
  the voice loop keeps working standalone. `speak_text` only flashes
  the tray for audio ≥ 1 s to avoid flicker on short prompts;
  `fono.confirm` wraps its listen-and-match span in a Confirming
  guard which nests cleanly with `listen_once`'s own Listening guard.
- **Slice 8** — Docs, voice preset, and CHANGELOG. The bundled
  `assets/agent-presets/voice.md` and the synced copies in
  `AGENTS.md` / `docs/coding-agents.md` now teach the agent to pass
  `context` on every `fono.listen` call and to prefer `fono.confirm`
  for bounded decisions. `docs/configuration.md` documents
  `[mcp].listen_silence_ms`, `[mcp].listen_max_seconds`, and the
  `[mcp.relevance_filter]` sub-table. CHANGELOG entries added under
  `[Unreleased]`.

Pre-commit gate green for both new commits (Slice 7 and Slice 8):
`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
-- -D warnings`, `cargo test --workspace --tests --lib` all pass.

**Next steps for maintainer:**

1. Restart any running coding-agent sessions to respawn `fono mcp
   serve` against the new binary so the overlay, relevance filter,
   and tray-feedback IPC come online.
2. Workspace version bump + CHANGELOG `[Unreleased]` graduation when
   the next release is cut.

## 2026-05-26 — Removed `fono agent-loop`; `fono.listen` / `fono.confirm` rebuilt

Two coupled changes this session:

1. **`fono agent-loop --agent <name>` removed.** The wrapper was a thin
   `Command::new(exe).status()` over an entry in `agents.toml` — it did not
   inject the voice preset, set env, or do anything `fono agent-setup` had
   not already done. After `fono agent-setup forge` writes the MCP JSON and
   appends the preset to `AGENTS.md`, running `forge` directly is
   indistinguishable from running `fono agent-loop --agent forge`. Removed:
   - `crates/fono/src/agent_loop.rs` (deleted).
   - `pub mod agent_loop;` from `crates/fono/src/lib.rs`.
   - `Cmd::AgentLoop` variant + dispatch in `crates/fono/src/cli.rs`.
   - All living-doc references in `CHANGELOG.md` (Unreleased — never shipped
     in a tagged release), `ROADMAP.md`, `docs/coding-agents.md`, the
     bundled `assets/agents.toml` comment block, and the docstrings in
     `crates/fono/src/agents.rs`.
   - The `Done. Start a voice session with: fono agent-loop …` line in
     `agent_setup.rs:119` now reads `Done. Start a voice session by
     launching <name> the way you normally do.`
   - ADR 0030 reference updated (`agent-loop` wrapper → `agent-setup`
     helper) at `docs/decisions/0030-fono-as-mcp-server-for-coding-agents.md:58`.
   - Bundled tests for the registry already live in `crates/fono/src/agents.rs::tests`,
     so no test coverage was lost when `agent_loop.rs` went away.
   - `plans/` and historical `docs/status.md` entries are left untouched as
     historical record per AGENTS.md.
2. **`fono.listen` / `fono.confirm` rebuild.** The MCP tool wiring landed in
   source on 2026-05-26 (`crates/fono-mcp-server/src/voice_io.rs` + the
   `listen.rs` / `confirm.rs` rewrites) but the binary at
   `target/release/fono` was still the older build that returned the
   `"standalone microphone capture is not yet available in this build"` /
   `"requires the fono.listen implementation which ships in the next Fono
   release"` stubs. Rebuilt this session — `strings target/release/fono |
   grep "standalone microphone"` is now empty, and `strings | grep
   voice_io::listen_once` resolves. The MCP server spawned by an already-
   running coding agent is still the old subprocess; restart the agent
   (e.g. exit and re-launch Forge / Claude Code) to pick up the new
   subprocess.

Pre-commit gate:

- `cargo fmt --all -- --check` ✓
- `cargo clippy --workspace --all-targets -- -D warnings` ✓
- `cargo test --workspace --tests --lib` ✓ — 0 failures across the
  workspace.

**Next steps for maintainer:**
1. Restart any running coding-agent sessions so they respawn `fono mcp serve`
   from the new binary and `fono.listen` / `fono.confirm` start serving real
   audio instead of the stub error.
2. Workspace version bump + CHANGELOG `[Unreleased]` graduation when the
   next release is cut.

## 2026-05-26 — `fono.listen` + `fono.confirm` audio capture (Phase 3 complete)

Plan: `plans/2026-05-25-fono-voice-loop-for-coding-agents-v1.md` Phase 3

Closes the deferred work from the 2026-05-26 voice-loop landing: the two MCP
tools that previously returned "not yet available" now run real audio.

What shipped:

- **`crates/fono-mcp-server/src/voice_io.rs`** — new module with shared
  helpers used by all three voice tools:
  - `speak_text(cfg, secrets, text, voice)` — TTS build + AudioPlayback +
    drain loop, extracted from the old inline `SpeakTool::call` body.
  - `listen_once(cfg, secrets, models_dir, max_seconds)` — opens
    `AudioCapture` with a forwarder that feeds both a `RecordingBuffer`
    and an `EnvelopeFollower` → `SilenceWatch` pair; loop ends on
    `SilenceEvent::Committed` or when `max_seconds` (capped by
    `[mcp].listen_max_seconds`) elapses; then runs the buffered PCM
    through the configured STT backend. Default total-silence window is
    2 s when the user has not configured `[audio].auto_stop_silence_ms`.
  - `match_choice(transcript, choices)` — pure function with five-rule
    matching ladder (exact match → option/letter phrasing → ordinals →
    unique substring) used by `fono.confirm`.
- **`fono.listen`** (`crates/fono-mcp-server/src/tools/listen.rs`) — now
  speaks the optional `prompt` via `speak_text`, calls `listen_once`,
  and returns `{"transcript": "...", "duration_ms": N, "reason":
  "silence"|"timeout"}`.
- **`fono.confirm`** (`crates/fono-mcp-server/src/tools/confirm.rs`) —
  composes "<question>? Choices: A, B, C.", speaks it, runs
  `listen_once`, and returns `{"choice": "A", "transcript": "..."}` on
  a confident match, `{"choice": "timeout"}` on silence, or `{"choice":
  "unmatched", "transcript": "..."}` when the spoken answer didn't fit.
- **`McpContext`** gained `whisper_models_dir: PathBuf`; the `fono mcp
  serve` dispatch arm in `crates/fono/src/cli.rs:657-661` passes
  `paths.whisper_models_dir()` into it.
- **`SpeakTool`** simplified to a thin wrapper over `speak_text` —
  ~60 lines removed.

Test coverage: 12 new unit tests (10 in `voice_io::tests` for the
matching ladder + auto-stop resolution, 2 in `confirm::tests` for
utterance composition). All run without touching real hardware. Pre-commit
gate:

- `cargo fmt --all -- --check` ✓
- `cargo clippy --workspace --all-targets -- -D warnings` ✓
- `cargo test --workspace --tests --lib` ✓ — full workspace green; the
  `fono-mcp-server` suite is now **25 tests** (up from 13).

**Next steps for maintainer:**
1. End-to-end smoke test: `fono use mcp-server on`, `fono agent-setup forge`
   in a real project, then `fono agent-loop --agent forge` and exercise
   the listen/confirm tools live.
2. Bump workspace version, graduate `[Unreleased]` in `CHANGELOG.md`,
   tag the release.

## 2026-05-26 — `fono agent-setup` — one-command agent integration

Plan: `plans/2026-05-26-fono-agent-setup-one-command-v1.md`

All 10 tasks complete. What shipped:

- **`crates/fono/src/agent_setup.rs`** — new module with three idempotent setup steps:
  1. Enable MCP server (`cfg.mcp.enabled = true`)
  2. Merge `mcpServers.fono` into the agent's `mcp.json` (other entries preserved)
  3. Append the voice-mode preset to `AGENTS.md` / `CLAUDE.md` (sentinel guards
     against re-injection; agents with `preset_injection = "none"` receive printed
     manual instructions instead)
  - `--dry-run` flag: prints what would happen, writes nothing.
  - `--list` flag: prints all registered agents in a table.
  - 12 unit tests covering all branches (idempotency, dry-run, JSON merge,
    sentinel dedup, tilde expansion, preset-file override).
- **`crates/fono/src/agents.rs`** — shared TOML loader extracted from `agent_loop.rs`
  (used by both `agent_loop` and `agent_setup`). `preset_file` field added to
  `AgentEntry` for user-controlled override of the injection target.
- **`crates/fono/src/cli.rs`** — `Cmd::AgentSetup` variant with positional `agent`,
  `--dry-run`, `--project-dir`, `--list`; dispatch arm wired.
- **`docs/coding-agents.md`** — "Quick setup" section added at the top with output
  sample, flag table, and `--list` example.

Pre-commit gate: `cargo fmt --check` ✓ · `cargo clippy -D warnings` ✓ ·
`cargo test --workspace --tests --lib` ✓ — **0 failures** (127 lib tests in `fono`,
12 new in `agent_setup`).

**Next steps for maintainer:**
1. `fono agent-setup forge` in a real project directory to verify end-to-end.
2. `fono agent-loop --agent forge` to confirm the voice session starts.
3. Bump version, graduate CHANGELOG, tag release.



Plan: `plans/2026-05-25-fono-voice-loop-for-coding-agents-v1.md`

All implementation phases (0–6b) are complete. Phase 7 pre-commit gate
verified clean this session:

- `cargo fmt --all -- --check` ✓
- `cargo clippy --workspace --all-targets -- -D warnings` ✓
- `cargo test --workspace --tests --lib` ✓ — **0 failures** across the
  full workspace (all crates, all lib and integration tests)

Remaining Phase 7 items (workspace version bump, CHANGELOG graduation,
binary-size delta) are deferred to the release tag per project convention.

**Next steps for maintainer:**
1. Verify `fono mcp serve` end-to-end with a real Forge / Claude Code session.
2. Run `fono agent-loop --agent forge` (after pasting the MCP snippet
   into `~/.forge/mcp.json`).
3. Record the screencap in `docs/screencasts/voice-loop-forge.webp`.
4. Bump `[workspace.package] version` in `Cargo.toml`, graduate
   `[Unreleased]` in `CHANGELOG.md`, and tag the release.


## 2026-05-26 — Voice loop for coding agents — Phases 2–6b

Plan: `plans/2026-05-25-fono-voice-loop-for-coding-agents-v1.md`

**Phases 2, 3 (partial), 4, 5, 6, and 6b** are complete. What shipped:

- **`crates/fono-mcp-server`** — new crate with full JSON-RPC 2.0 stdio transport
  (`StdioTransport`), `McpServer` request/dispatch loop, `ToolRegistry`, and three
  voice tools:
  - `fono.speak` — fully implemented: builds TTS from config+secrets, synthesises
    text, enqueues to `AudioPlayback`, drains until idle.
  - `fono.listen` — quality stub; returns clear error pending standalone audio
    capture path.
  - `fono.confirm` — quality stub; returns clear error pending `fono.listen`.
  Unit tests green: protocol round-trips, golden initialize→tools/list→tools/call flow.
- **Hotkey FSM** — `McpDriven { tool: ToolKind }` state in
  `crates/fono-hotkey/src/fsm.rs`. F7/F8/Escape barge-in cancels active tool call.
  `ToolKind` enum: `Speak`, `Listen`, `Confirm`.
- **Tray MCP submenu** — visible when `[mcp.server].enabled = true`; enable/disable
  toggle, last-connected timestamp, per-tool rows. Badge support wired.
- **`fono doctor`** — "Coding agents (MCP server)" section: enabled flag, tools
  advertised, transport.
- **`crates/fono/src/agent_loop.rs`** — generic `fono agent-loop --agent <name>`
  implementation. Reads `~/.config/fono/agents.toml` (user) with bundled
  `assets/agents.toml` fallback. No agent-specific code anywhere.
- **`assets/agents.toml`** — first-party entries: forge, claude-code, cursor,
  codex (untested), gemini (untested).
- **`assets/agent-presets/voice.md`** — shared voice-mode system prompt.
- **`docs/coding-agents.md`** — full integration guide: Forge, Claude Code, Cursor
  (all verified via config), plus best-effort docs for Codex CLI, Gemini CLI,
  Cline/Continue/Windsurf, and Goose. "Adding your own agent" section.
- **Wizard** — optional final step "Enable voice-driven coding agents?" (agent-neutral).

Pre-commit gate passed: `cargo fmt --check`, `cargo clippy -D warnings`,
`cargo test --workspace --tests --lib` all green.

**Phase 3 partial:** `fono.speak` fully implemented. `fono.listen` and `fono.confirm`
are quality stubs — standalone audio capture in the MCP server path requires wiring
`fono-audio`'s `CaptureHandle` + `SilenceWatch` outside the daemon context. Deferred
to next session.

## 2026-05-26 — Voice loop for coding agents — Phase 0 + Phase 1

Plan: `plans/2026-05-25-fono-voice-loop-for-coding-agents-v1.md`

**Phase 0 (decisions/roadmap/changelog)** and **Phase 1 (`fono speak --stream`)**
are complete. What shipped:

- **ADR 0030** `docs/decisions/0030-fono-as-mcp-server-for-coding-agents.md` —
  records the agent-agnostic design principle, three-tool MCP surface, and
  `agents.toml` registry design.
- **`fono speak --stream`** — new CLI subcommand in `crates/fono/src/speak_stream.rs`.
  Reads stdin, sanitises markdown (code fences, bold/em, headings, links, inline code,
  long URLs), sentence-segments with a 200-char hard cap, and speaks via the configured
  TTS backend. Includes 5-sentence backpressure and clean Ctrl-C cancellation.
  18 unit tests green.
- **`McpServer` config struct** added to `crates/fono-core/src/config.rs` with
  `enabled`, `mirror_to_stdout`, `listen_max_seconds`, `confirm_timeout_seconds`.
  Serialised only when non-default (`skip_serializing_if`).
- **`fono use mcp-server on|off`** — new `UseCmd::McpServer` arm toggles
  `cfg.mcp.enabled` and reloads the daemon.
- **Stub dispatch** for `fono mcp serve` (exits with a clear "Phase 2 not yet
  implemented" message + safety-gate error if `mcp.enabled` is false) and
  `fono agent-loop --agent <name>` (stub stub pointing at `docs/coding-agents.md`).
- **`docs/coding-agents.md`** created with the Phase 1 "Dictate-in, pipe-speak-out"
  section, MCP setup overview, per-agent config snippet stubs, and an
  "Adding your own agent" section.

Pre-commit gate passed: `cargo fmt --check`, `cargo clippy -D warnings`,
`cargo test --workspace --tests --lib` all green.

**Next: Phase 2** — `fono-mcp-server` crate skeleton + stdio transport.

## 2026-05-25 — Wizard recommendation accuracy fix (`.131` regression)

Hand-test session on `192.168.0.131` (i7-8550U Kaby Lake-R, 4c/8t, AVX2
+FMA, UHD 620 iGPU, CPU release variant) surfaced two compounding bugs
in the wizard's data-driven model picker:

1. **GPU multiplier credited to CPU-only builds.** The Vulkan probe set
   `host_gpu = Integrated` (UHD 620 reports `shaderFloat16`), which the
   affordability scorer (`HardwareSnapshot::affords_model`) multiplied
   into the formula as `2.0×`. The CPU release variant has no Vulkan
   inference backend, so this was a phantom speedup. Effective RTF for
   `large-v3-turbo` came out at `2.3 × 0.5 × 1.0 × 2.0 = 2.30` —
   crossing the `BATCH_REALTIME_MIN = 2.0` floor and getting
   "(recommended)". Measured batch RTF on this host is actually `0.77`
   (`docs/bench/calibration/matrix.md:127-141`).
2. **`small.en` registry anchor was off by 2×.** The comment at
   `crates/fono-stt/src/registry.rs:316-327` cites
   "ultra7-258v CPU q8_0: 3.30" but the matrix records `7.15`
   (`docs/bench/calibration/matrix.md:235`). A transcription error.
3. **Doctor and wizard disagreed.** Doctor used the static
   `tier.default_whisper_model()` (says `tiny` on Minimum tier); wizard
   walked `build_local_stt_shortlist` and said `turbo`.

Fixes shipped this session:

- **F1** — Added `HardwareSnapshot::for_inference(gpu_inference_available: bool)`
  in `crates/fono-core/src/hwcheck.rs:296-321`. Returns a snapshot
  clone with `host_gpu = HostGpu::None` when the caller declares that
  inference cannot use a GPU. Truthful display snapshot is preserved
  separately so `fono doctor` can still surface the
  "you have a Vulkan GPU but you're on the CPU variant" hint.
- **F1 wiring** — Every recommendation call site in the binary now
  passes `snap.for_inference(matches!(VARIANT, Variant::Gpu))` instead
  of the raw `snap`:
  `crates/fono/src/wizard.rs:1556-1564` (`pick_local_stt_model`),
  `crates/fono/src/cli.rs:1037-1046` (`compute_hwprobe_recommendation`),
  `crates/fono/src/cli.rs:1108-1116` (`hwprobe` JSON
  `default_whisper_model`), and
  `crates/fono/src/daemon.rs:84-92` (first-run config seed).
- **F1.5** — `small.en` `realtime_factor_cpu_avx2: 3.3 → 7.15` and
  comment fixed (`crates/fono-stt/src/registry.rs:326-328`). Pure data
  correction, formula unchanged.
- **F3** — `fono doctor` now uses
  `ModelRegistry::pick_default_local(&snap.for_inference(...))` rather
  than `tier.default_whisper_model()` so the diagnostic page and the
  wizard never disagree on the recommended model
  (`crates/fono/src/doctor.rs:61-98`).

Test coverage added:

- `for_inference_zeros_host_gpu_when_unavailable` unit test in
  `crates/fono-core/src/hwcheck.rs` pins the snapshot transform.
- `cpu_variant_view_of_iigpu_host_drops_turbo` integration test in
  `crates/fono/tests/wizard_selection.rs` reproduces the `.131` host
  shape and asserts the multilingual shortlist tops at `small` (not
  `turbo`) and the English-only shortlist tops at `small.en`.

User-visible effect on `.131`:

| Surface | Before | After |
|---|---|---|
| `fono doctor` | "recommends whisper-tiny" | "recommends whisper-small" |
| Wizard, multilingual | "Turbo (recommended)" | "Small (recommended)" |
| Wizard, English-only | "Turbo (recommended)" | "Small.en (recommended)" |

Explicitly **not** in scope this session (per user direction): no
runtime calibration clip, no shipping of `matrix.json` inside the
binary, no broader re-anchoring of `realtime_factor_cpu_avx2` away
from the Lunar Lake reference. The longer-term anchor-drift concern
("modern CPUs in 2 years won't be modern any more") is acknowledged
and remains open as a future tuning item, but is not addressed by
this PR.

Pre-commit gate: `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, and `cargo test --workspace --tests
--lib` all green (728 tests passing).

## 2026-05-25 — `HostGpu` taxonomy refresh: split `Integrated` into legacy and tensor-capable

Follow-up to the same `.131` regression. After probing the Vulkan
capability set on three calibration hosts (`192.168.0.131` UHD 620
Kaby Lake-R, localhost Iris Xe Alder Lake, `192.168.0.251` Lunar Lake
Xe2) we confirmed that on modern Mesa (>= 26.x) **neither
`shaderFloat16` nor `shaderInt8` discriminates a 2017 iGPU from a
2022 one**: all three hosts advertise both features. The flat `2.0×`
multiplier the wizard was applying to every `Integrated` host
over-promised on UHD 620 by ~70% (real Vulkan/CPU geomean ~1.2×) and
under-promised on Lunar Lake by ~50% (real ~3.0-3.5×).

The single Vulkan capability that **does** cleanly discriminate
Lunar Lake / Arc / Battlemage / RDNA3+ / Turing+ from the older
Iris Xe and UHD generations is the `VK_KHR_cooperative_matrix`
extension — and presence of that extension is causally linked to
whisper.cpp's ggml-vulkan dropping into its tensor matmul kernel,
which is the underlying reason for the 3-4× speedup.

Changes shipped this session:

- **HostGpu enum expanded** to four classes
  (`crates/fono-core/src/hwcheck.rs:56-94`):
  `None` (1.0×) / `Integrated` (1.3×, demoted from 2.0×) /
  `IntegratedTensor` (2.0×, new) / `Discrete` (4.0×). See ADR 0028
  amendment.
- **Vulkan probe extended** to query
  `VK_KHR_cooperative_matrix` extension presence on every device
  (`crates/fono-core/src/vulkan_probe.rs:284-310`). New
  `DeviceInfo.supports_cooperative_matrix` field; classifier returns
  `IntegratedTensor` when fp16 + coopmat are both present, else
  `Integrated` when only fp16.
- **Wire protocol** extended forward-compatibly: a fourth
  per-device flag (`coopmat`) on the subprocess probe's stdout
  payload. Old payloads decode with `coopmat = false`, which maps to
  the legacy `Integrated` class (the previous default).
- **Apple Silicon default** updated from `Integrated` to
  `IntegratedTensor` in `default_host_gpu_for_platform`
  (`crates/fono-core/src/hwcheck.rs:418-424`): Metal / CoreML on
  M-series exposes the same matmul-tensor fast path as
  cooperative_matrix-capable iGPUs.
- **`hwprobe` JSON** gained the new `"integrated-tensor"` value for
  `host_gpu` (`crates/fono/src/cli.rs:1121-1126`).
- **ADR 0028 amended** with the new taxonomy, empirical
  justification across the three calibration hosts, and wire-protocol
  compatibility note.

Test coverage added/updated:

- `host_gpu_multipliers_match_calibration_classes` and
  `affords_turbo_with_integrated_tensor_gpu` unit tests in
  `crates/fono-core/src/hwcheck.rs` pin the new multipliers.
- `acceleration_summary_integrated_tensor_says_tensor` unit test
  pins the new summary string for the IntegratedTensor class.
- `host_gpu_class_picks_best_present` in `vulkan_probe.rs` extended
  with an `xe2` case that asserts fp16 + coopmat → IntegratedTensor.
- `integrated_tensor_host_picks_turbo_on_multilingual` integration
  test in `crates/fono/tests/wizard_selection.rs` reproduces the
  Lunar Lake host shape and locks the wizard top pick.

Net effect: under the GPU release variant the wizard now picks
correctly on every calibration host (UHD 620 → small, Iris Xe →
turbo via CPU horsepower carrying the 1.3× iGPU credit, Lunar Lake
→ turbo via the 2.0× IntegratedTensor credit). The CPU variant case
remains as fixed in the preceding session.

Pre-commit gate: `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, and `cargo test --workspace --tests
--lib` all green.

## 2026-05-25 — Wizard model-selection heuristics refresh

Completed the wizard-selection refresh plan
(`docs/bench/calibration/summary/plans/2026-05-25-wizard-selection-heuristics-refresh-v5.md`):
dropped live-RTF gating, collapsed `Affordability` to `bool`, introduced
the data-driven `HostGpu` classifier (1×/2×/4× multipliers per ADR 0028),
refreshed `wer_by_lang` to Open-ASR-Leaderboard means, and unified
`default_quantization` on `q8_0` across the registry (per the ADR 0027
2026-05-25 amendment). New invariant tests pin the published `.en ≤
multilingual` WER ranking and the matrix-winners-within-1.5× behaviour;
new wizard-flow integration tests cover the three HostGpu classes.

## 2026-05-23 — en-self-* focused sweep + .en-vs-multi side report

Unattended ~2 h sweep of the two new first-person CC0 dictation fixtures
(`en-self-dictation`, `en-self-casual`) across the inventory grid, with
focused side report comparing `.en` vs multilingual whisper builds at
each model size tier.

| host | builds attempted | result |
|---|---|---|
| i7-1255u (localhost) | vulkan only (no cpu binary on disk) | **COMPLETE** — 63 reports, 21 cells × 3 iters |
| ultra7-258v (192.168.0.251) | cpu + vulkan | **PARTIAL** — full CPU build (63 reports, 21 cells × 3 iters); vulkan build was just starting at collection time |
| i7-7500u (192.168.0.112) | cpu + vulkan | **PARTIAL** — 41 CPU reports through `base.en-q8_0` iter2; Skylake CPU is too slow for the large-v3-turbo cells within the 2 h budget |
| ryzen-5950x (192.168.0.74) | cpu + vulkan | **FAILED** — host rebooted twice mid-sweep (NVIDIA driver mismatch 580→595); /tmp tmpfs wiped both times. Pre-reboot CPU build was complete (63 reports) but the run JSONs did not survive. No data collected this session. |
| i7-8550u (192.168.0.131) | — | **SKIPPED** at pre-flight — no `fono-bench` binary, no python rig, no models on disk; Ubuntu live host needs provisioning before it can participate. |

Total reports collected into `docs/bench/calibration/runs-self-fixtures/`:
**167** (63 + 63 + 41). Sidecar `*.time.json` files preserved alongside.

Merged into `docs/bench/calibration/runs/` (852 → **962** files):
- `appended-2`: 112 cells already in the main matrix had the two new
  fixtures' result entries appended idempotently (sha-keyed on fixture name).
- `copied-new`: 55 cells were new files (mostly iter3 entries that the
  original 2-iter cohort did not have).

Regenerated pages: `calibration3.html` (171 KB), `auto-select.html`
(195 KB) — both stamped 2026-05-23 21:20Z.

### Headline finding — `.en` vs multilingual on real dictation

Side report at `docs/bench/calibration/summary/self-fixtures-en-vs-multi.md`.
Accuracy is `stt_accuracy_levenshtein` (lower = better). Delta = `.en − multi`.

- **base tier**: `.en` wins on all 3 hosts with data
  (`delta ≈ −0.009`, ~50 % relative error reduction).
- **small tier**: `.en` wins decisively where measured
  (`delta = −0.13` on ultra7 cpu, `−0.21` on i7-1255u vulkan).
  Multilingual `small` produces significantly worse transcripts on
  these fixtures.
- **tiny tier**: multilingual is *better* (`delta = +0.018`,
  consistent across all 3 hosts) — the only tier where the older
  intuition "multi ≥ .en" holds.
- **turbo baseline** (multilingual only): `acc ≈ 0.010` — best in
  class, as expected.

So past results showing `.en` losing were almost certainly poisoned by
the now-removed `en-conversational` fixture (truncation bug noted in
the manifest). On clean first-person dictation, `.en` wins at `base`
and `small`, and the corpus now reflects that.

### Pointers
- New per-host runs: `docs/bench/calibration/runs-self-fixtures/`
- Side report: `docs/bench/calibration/summary/self-fixtures-en-vs-multi.md`
- Per-host sweep logs: `docs/bench/calibration/logs/self-sweep-*-2026-05-23*.log`
- Regenerated pages: `docs/bench/calibration/summary/calibration3.html`,
  `docs/bench/calibration/summary/auto-select.html`

### Gaps to close in a follow-up session
1. Re-run ryzen-5950x once the NVIDIA driver mismatch is fixed (likely
   a `nvidia-smi` userspace ↔ kernel module skew after the recent
   driver upgrade). Recommend pinning models on persistent disk and
   moving `runs-self-fixtures` off tmpfs before relaunch.
2. Finish ultra7-258v vulkan build (currently 0/63 vulkan cells).
3. Finish i7-7500u CPU base/small/turbo-post + entire vulkan build
   (currently ~22/63 CPU cells; vulkan untouched).
4. Provision i7-8550u (Ubuntu live) with the bench rig before
   including it in future sweeps.

---

## 2026-05-23 — ryzen-5950x gap-fill (complete matrix 210/210)

Pulled existing May 22 runs-ro data from ryzen-5950x (192.168.0.74), then ran
the 3 missing cpu fp16 `.en` models (tiny.en, base.en, small.en) today. Turbo
cells refreshed simultaneously. Matrix is now **210/210 cells** — 5 hosts ×
2 builds × 21 models, zero gaps.

Regenerated: `calibration3.html` (169 KB), `auto-select.html` (195 KB).

---

## 2026-05-23 — fp16 gap-fill sweep (post-optimization baseline restore)

Ran parallel fp16 benchmark sweeps on 3 hosts to restore the baselines deleted
when the pre-optimization cohort (commit `b4db59c`) was removed.

| host | builds | models | status |
|---|---|---|---|
| i7-1255u (localhost) | cpu + vulkan | 7 fp16 × 2 iters | **COMPLETE** |
| ultra7-258v (192.168.0.251) | cpu + vulkan | 7 fp16 × 2 iters | **COMPLETE** |
| i7-7500u (192.168.0.112) | cpu + vulkan | 7 fp16 × 2 iters | **COMPLETE** |
| ryzen-5950x | cpu fp16 | 7 fp16 × 2 iters | **PENDING** — unreachable this session |

Matrix after re-aggregation: **203 cells** (168 → 203).  
Remaining gap: `ryzen-5950x/cpu` fp16 baselines (7 cells). Vulkan fp16 already present.  
Generated: `calibration3.html` (166 KB), `auto-select.html` (190 KB).

**Next:** run fp16 cpu sweep on ryzen-5950x when reachable; close remaining 7 gaps.

---

## 2026-05-23 — Dropped pre-optimization bench cohort (commit b4db59c)

Removed **298 stale benchmark files** (149 run JSONs + 149 `.time.json`
sidecars) from `docs/bench/calibration/runs/`. All were introduced by
`b4db59c docs(bench): Phase 0 STT affordability calibration matrix`
(2026-05-15) — **predating the CPU performance optimization that
landed 4 days later** in:

> `ef557af feat(stt+polish): quantization ladder + rename LlmBackend -> PolishBackend` (2026-05-19)

The optimization shipped two wins on the CPU path:

1. **`set_audio_ctx()` on clips <30s** — "+70–160% CPU batch RTF with
   no measurable quality regression", hard-coded on in
   `crates/fono-stt/src/whisper_local.rs`.
2. **Thread default switched from logical-CPU count → physical cores**
   (clamped 1..16); Ryzen 5950X data showed `small` running at half
   speed with `t=32` vs `t=16` because SMT siblings contend on the
   256-bit FMA unit.

Together that's a ~1.7×–2.6× CPU speedup that the May-15 cohort never
saw. Vulkan unaffected, but removing the AC/battery + Vulkan rows from
the same cohort too because mixing pre- and post-optimization rows on
the same host poisons every cross-backend ratio (`cpu_vs_vulkan`,
`quant_vs_fp16`).

### What was dropped per host

| host | cells dropped | models covered (fp16 only) |
|---|---|---|
| `i7-1255u`    | 53 (cpu+vulkan, AC+battery) | tiny, tiny.en, base, base.en, small, small.en, large-v3-turbo |
| `i7-7500u`    | 19 (cpu only, AC)           | same fp16 set |
| `ryzen-5950x` | 21 (cpu only, AC)           | same fp16 set |
| `ultra7-258v` | 56 (cpu+vulkan, AC+battery) | same fp16 set |

`i7-8550u` had no May-15 runs — its data is fully post-optimization,
nothing removed.

### Regenerated artifacts

- `docs/bench/calibration/summary/matrix.json` — **168 cells**
  (down from 237). Per-host coverage:
  `i7-1255u: 28 cells (14+14 cpu/vk)`,
  `i7-7500u: 35 (14+21)`,
  `i7-8550u: 42 (21+21)`,
  `ryzen-5950x: 35 (14+21)`,
  `ultra7-258v: 28 (14+14)`.
- `docs/bench/calibration/summary/matrix.md` regenerated.
- `docs/bench/calibration/summary/calibration3.html` (150,579 bytes,
  6 speedup buckets, 42 coverage gaps — these gaps are the rebench
  TODO list).
- `docs/bench/calibration/summary/auto-select.html` (166,635 bytes).

### Follow-up — rebench the 4 affected hosts

The four reference hosts (`i7-1255u`, `i7-7500u`, `ryzen-5950x`,
`ultra7-258v`) have lost all their fp16 baselines. The auto-select
page's Section 6 "Data gaps under current policy" will now surface
those exact configs as missing. The natural next step is one bench
pass per host on the post-`ef557af` binary, covering at least the
fp16 + q8 + q5 set for tiny/tiny.en/base/base.en/small/small.en/turbo
(CPU + Vulkan where applicable). Until that lands, quant-uplift
ratios on those hosts will appear partial in `calibration3.html`
chart 3 and `auto-select.html` Section 3.

## 2026-05-23 — auto-select.html: worst-fixture gate + display cap + contrast

Three coupled changes to make the recommendation policy more honest in
the face of accuracy outliers and the charts more readable:

1. **Switched the accuracy gate from `accuracy_en_mean` to
   `accuracy_en_max`** (`scripts/bench-auto-select-page.py:48-57`).
   The mean was hiding catastrophic transcripts behind a friendly
   average — e.g. `i7-7500u/small/cpu` shows mean 0.285 (passes a 0.30
   ceiling) while its worst English fixture is **0.853** (74% wrong on
   one sentence). With the max-gate, that cell now correctly fails any
   reasonable ceiling. Default ceiling bumped 0.10 → 0.20 because max
   naturally runs higher than mean; slider range now `0.05 – 0.50`.
   Mean is still carried in the payload and shown as supporting
   context in the rec-card trace and Pareto tooltip
   (`scripts/bench-auto-select-page.py:721-726, 952`), so the reader
   sees both numbers and can judge the spread.
2. **Display cap at WER ≤ 0.30** in Pareto and Section-2 accuracy
   scatter (`scripts/bench-auto-select-page.py:56, 893-924, 964-968`).
   Cells with worst-fixture CER above the cap are dropped from the
   plot and counted in a yellow `+N off-scale` chip beside each host
   title; the Section 6 data-gap list still surfaces them.
   Pareto x-axis is hard-pinned to `[0, 0.30]` and the
   Section-2 worst-fixture-CER y-axis to `max: 0.30` so a single
   outlier can't stretch the axis and squash everything into a strip.
3. **Contrast bump across the board**: text `#e6edf3 → #f0f6fc`, muted
   `#8b949e → #b1bac4`, border `#30363d → #3d444d`, all chart accent
   colours bumped one notch toward saturation (greens, yellows, reds,
   blues); chart grid lines `#21262d → #30363d`; threshold dash lines
   went from `borderWidth:1, alpha 88` to `borderWidth:1.5` with full
   alpha + bold label text. Scatter point fills gained borders for
   legibility on overlapping clouds.

Also: presetStrict now `batch≥2.5, acc≤0.12`; presetRelaxed
`batch≥1.2, acc≤0.35` (calibrated for the new max-based gate).

Files: `scripts/bench-auto-select-page.py`,
`docs/bench/calibration/summary/auto-select.html` (regenerated, 213,553 bytes).

## 2026-05-23 — auto-select.html chart sizing + Pareto enlarge

Three follow-up fixes to `auto-select.html` after first eyeball pass:

1. **Section 1 quant-uplift chart squashed** — Chart.js was
   re-deriving aspect ratio from the canvas's content (long rotated
   `host/build` x-labels), collapsing the plot area to a thin strip.
2. **Section 3 quant-uplift chart resizing on every selector change** —
   same root cause: every filter rebuild triggered a new aspect-ratio
   recalculation against a freshly-laid-out canvas.
3. **Pareto frontier charts (Section 4) are the most informative
   views but too small to read in the 3-up grid.**

Root cause for (1)+(2): default `maintainAspectRatio:true` combined
with `responsive:true` forces Chart.js to keep deriving canvas height
from its width × CSS-driven aspect ratio, which depends on font
metrics of axis labels that change between renders. Fix:

- Added fixed-height wrappers (`canvas-wrap.h-uplift{height:300px}`,
  `h-scatter{height:220px}`, `h-pareto{height:260px}`) in
  `scripts/bench-auto-select-page.py:315-319` — canvas now fills a
  deterministic box via absolute positioning.
- Set `maintainAspectRatio:false` on all four chart configs
  (uplift, scatter ×6, pareto-grid, modal-pareto).

For (3): each Pareto chart-box now has an `⤢ enlarge` button
(`scripts/bench-auto-select-page.py:891-896`) that opens a `60vh`
modal (`canvas-wrap` flex-fill) with a freshly-instantiated Chart.js
instance built via `requestAnimationFrame` so the canvas measures its
true size before draw. Backdrop click + Close button both destroy the
modal chart cleanly.

Files: `scripts/bench-auto-select-page.py`,
`docs/bench/calibration/summary/auto-select.html` (regenerated, 211 KB).

## 2026-05-23 — Auto-Select Policy Explorer (`auto-select.html`)

New companion page next to `calibration3.html`:
`docs/bench/calibration/summary/auto-select.html`, generated by
`scripts/bench-auto-select-page.py`. Closes the gap between the
calibration matrix (diagnostic) and the runtime model picker (the
stale `LocalTier::default_whisper_model()` in
`crates/fono-core/src/hwcheck.rs:77-83`, which the page is designed to
replace).

Per plan `plans/2026-05-23-fono-auto-select-page-v1.md`.

### What the page does

- Eight live controls (batch RTF threshold, accuracy ceiling, stream
  RTF soft floor, memory budget, binary variant cpu_only/gpu_capable,
  language requirement, quant preference, power, arch). URL-hash
  backed so a particular policy state can be linked/bookmarked.
- **Section 1 — Recommendation walk per measured host.** Each card
  picks the qualifying candidate per the preference order (largest
  family > fp16 > q8 > q5; Vulkan only if ≥1.2× CPU on the same model)
  and shows the gate trace plus why the next-up alternative failed.
- **Section 2 — Feature vs outcome scatter.** 2×3 grid (cores/ram ×
  batch/accuracy/peak_rss); colour by VNNI capability, shape by
  quant, size by family. Where "data hides things we don't expect"
  becomes visible.
- **Section 3 — Quant uplift per host.** Median quant/fp16 batch RTF
  per (host, build); categorical tags (`large` / `moderate` / `none` /
  `regression`) so "Vulkan + quant = no uplift" pops without reading
  numbers.
- **Section 4 — Pareto frontier per host.** Accuracy vs batch RTF
  scatter; frontier highlighted; recommendation marked with reticle;
  threshold rule lines visible.
- **Section 5 — Policy JSON export.** Versioned `schema_version: 1`
  blob with `arch` × `cpu_flags.avx_vnni` rules, evidence_hosts per
  rule, and hard-coded fallback rows for `aarch64` and
  `apple_silicon` so the runtime never panics on unmeasured archs.
  Copy button.
- **Section 6 — Data gaps under current policy** (collapsed by
  default, bottom of page, per user feedback). Missing measurements,
  n<2 picks, picks with no accuracy data, arch coverage gaps,
  unmeasured backends. Each missing-measurement row carries a
  copy-to-clipboard bench command derived mechanically from the gap
  descriptor.

### Host feature schema (shared with future Rust runtime classifier)

`derive_host_features()` in `scripts/bench-auto-select-page.py:148`
emits per host: `arch ∈ {x86_64, aarch64, apple_silicon}`,
`released_year`, `physical_cores`, `ram_gb`,
`cpu_flags: {avx2, avx_vnni, avx512, avx512_vnni}`,
`gpu_present`, `gpu_class ∈ {none, integrated, discrete, apple_metal}`.
`cpu_model_str` is carried for human display only; the policy walk
and the policy JSON consume the flags, never the model string. All
five current hosts are tagged `x86_64`; ARM and Apple Silicon get
hard-coded fallback rules in the policy JSON.

### Verification

- `python3 -m py_compile scripts/bench-auto-select-page.py` clean.
- `python3 scripts/bench-auto-select-page.py` against the live matrix
  succeeds: 237 cells, 5 hosts, 237 accuracy entries, output is
  206,703 bytes.
- Structural smoke test: `rec-grid`, `scatter-grid`, `quant-uplift`,
  `pareto-grid`, `policy-json`, `gaps-block`, `f-arch` filter, and
  `walkHost()` function are all present in the rendered HTML.
- `calibration3.html` footer now links forward to
  `auto-select.html` (`scripts/bench-decision-page3.py:487-488`);
  `auto-select.html` footer links back to `calibration3.html`.

### Files

- `scripts/bench-auto-select-page.py` (new, 1225 lines including
  embedded HTML template).
- `docs/bench/calibration/summary/auto-select.html` (regenerated).
- `scripts/bench-decision-page3.py` — footer cross-link added.
- `docs/bench/calibration/summary/calibration3.html` — regenerated.
- `plans/2026-05-23-fono-auto-select-page-v1.md` — the strategic plan
  that drove this work (Tasks 1-12 + 9b for the data gaps section).

### Pre-commit gate

Not run. Change is Python + generated HTML only; no Rust touched.

### Follow-ups expected

1. **Rust consumer**: write `crates/fono-stt/src/auto_select.rs` that
   reads the page's exported policy JSON and replaces
   `LocalTier::default_whisper_model()`. Mirror
   `derive_host_features()` as a Rust function so the runtime
   classifier shares the schema.
2. **Bench data gaps**: open the page in a browser, eyeball the
   Section 6 list under default sliders, and run the suggested
   benches to fill rows where the walk currently rests on `n=1`
   cells.
3. **Auto-merge**: once the matrix grows to ≥20 hosts, replace the
   per-host rule emission in `buildPolicy()` with decision-tree
   induction so the policy JSON ships a compact tree rather than one
   rule per host.

## 2026-05-23 — Deepgram STT (Nova-3) batch + WebSocket streaming

`fono use stt deepgram` now works end-to-end. The catalogue, wizard,
secrets layer and `SttBackend::Deepgram` config variant have
advertised Deepgram STT since v0.8.0, but the factory dropped
through to the catch-all "not yet implemented" arm — picking
Deepgram in `fono setup` silently configured the user toward a
daemon-startup failure. This work landed both slices of
`plans/2026-05-23-deepgram-stt-nova-3-v1.md` in one session: the
batch REST backend (Slice 1) and the native WebSocket streaming
backend (Slice 2).

### What landed

- **`crates/fono-stt/src/deepgram.rs`** — batch client. Uploads WAV
  to `POST https://api.deepgram.com/v1/listen` with the literal
  `Authorization: Token <k>` header (pinned in a unit test — this
  is the historical footgun of the Deepgram TTS client too).
  Per-request settings (`model`, `language` or `detect_language`,
  `smart_format`, `punctuate`) go on the URL; response is parsed
  into a minimal `DeepgramListenResponse` with every field
  `serde(default)` for forward compat. Language allow-list rerun
  uses Deepgram's top-alternative `confidence` (Deepgram doesn't
  expose per-segment `avg_logprob`, so confidence is the
  Whisper-style tiebreak signal). `prewarm` does a cheap authed
  `GET /v1/projects` so the TCP+TLS handshake is paid off the hot
  path.
- **`crates/fono-stt/src/deepgram_streaming.rs`** — real WebSocket
  client against `wss://api.deepgram.com/v1/listen`. Streams 16 kHz
  s16le mono PCM as binary frames; maps `Results` with
  `is_final: false` → `Preview` and `is_final: true` → `Finalize`;
  routes `UtteranceEnd` VAD events into segment-index advancement
  so the overlay's pondering + auto-stop hook works without
  backend-specific code. Sends `{"type":"Finalize"}` on local
  `SegmentBoundary` (nudges Deepgram to flush) and
  `{"type":"CloseStream"}` on EOF.
- **Factory wiring** — `build_stt` Deepgram arm at
  `crates/fono-stt/src/factory.rs:104` constructs `DeepgramStt`;
  `build_streaming_stt` Deepgram arm at
  `crates/fono-stt/src/factory.rs:445` constructs
  `DeepgramStreaming` when `live_preview` is on
  (`[overlay].style = "transcript"`). New factory tests cover the
  env-key fallthrough, missing-key remediation, and live-preview
  routing — same shape as the Groq/Cartesia tests.
- **Catalogue default bumped** — `crates/fono-core/src/provider_catalog.rs`
  Deepgram STT default model changed from `nova-2` to `nova-3`.
  Wizard literal at `crates/fono/src/wizard.rs:1705` and the
  defaults-test assertion at `crates/fono-stt/src/defaults.rs:36`
  flipped to match. `nova-2` remains available as an override and
  is documented as the multilingual-fallback escape hatch in
  `docs/providers.md`.
- **Docs.** `docs/providers.md` STT table row already advertised
  streaming; new *Deepgram STT (Nova-3)* and *Deepgram streaming
  dictation (WebSocket)* subsections describe the wire format,
  auth-header gotcha, language stickiness behaviour, model menu,
  and the cost note that Deepgram bills by audio seconds (so the
  streaming path is *cheaper* than Groq's pseudo-stream, not the
  reverse). `CHANGELOG.md` `[Unreleased]` Added section entry.

### Pre-commit gate

All three steps green: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --tests --lib`. 27 Deepgram unit tests
(13 batch + 14 streaming) plus the new factory routing test pass
under `--features 'deepgram streaming groq cartesia openai
openrouter wyoming whisper-local'`.

## 2026-05-23 — Cartesia STT (Phase 1, batch)

`fono use stt cartesia` now works end-to-end. Until this slice the
catalogue, the wizard picker, the doctor, the tray submenu, the
`stt_key_env` lookup and the `SttBackend::Cartesia` config variant
were all already in place — the runtime failed at the factory's
explicit "not yet implemented" fallthrough. This slice adds the
client, wires the factory branch, and corrects a stale catalogue
default. Realtime `ink-2` over the turn-based WebSocket
(`wss://api.cartesia.ai/stt/turns/websocket`) is a Phase 2 streaming
slice — Cartesia's batch endpoint refuses anything outside the
`ink-whisper` family. Plan file:
`plans/2026-05-23-cartesia-stt-support-v2.md`.

### What landed

- **`crates/fono-stt/src/cartesia.rs`** — batch client modeled on
  `groq.rs`: multipart `POST https://api.cartesia.ai/stt`,
  `X-Api-Key` + `Cartesia-Version: 2026-03-01` headers (matches the
  existing TTS client at `crates/fono-tts/src/cartesia.rs:258`),
  language goes as a **query parameter** (not a form field) per the
  documented endpoint shape, response shape `{ text, language?,
  duration? }`. Uses `crate::groq::warm_client + encode_wav` so the
  feature pulls `groq` in transitively (same trick as `openrouter`).
- **Factory branch + `build_cartesia` helper** at
  `crates/fono-stt/src/factory.rs:103` — same `resolve_cloud`
  plumbing every cloud backend uses, including language-cache
  bootstrap.
- **Catalogue correction** — `SttDefaults { model: "sonic-transcribe" }`
  was stale (`ink-2` is realtime-only and the batch endpoint
  explicitly rejects it); changed to `"ink-whisper"` at
  `crates/fono-core/src/provider_catalog.rs:410`. The wizard's
  parallel literal at `crates/fono/src/wizard.rs:1706` was updated
  to match.
- **`cargo feature cartesia`** declared on `fono-stt` and enabled in
  the `fono` binary's default feature set.
- **Wizard validator auth header** — the `X-API-Key` outlier at
  `crates/fono/src/wizard.rs:1853` was unified to `X-Api-Key` so
  the wizard validator, the STT client and the TTS client all use
  the same spelling (HTTP header names are case-insensitive per
  RFC 7230 §3.2 so this is cosmetic but reduces diff noise).
- **Known limitation documented**: Cartesia's batch response carries
  no per-segment `avg_logprob` / `no_speech_prob`, so the Whisper-
  style language-mismatch rerun and the silence-hallucination filter
  are skipped. `cloud_rerun_on_language_mismatch = true` produces
  one warning per process and otherwise no-ops.

## 2026-05-22 — Assistant Pondering parity + key-held suppression

Brought the F7 Pondering UX to the F8 assistant flow so a long pause
during an assistant turn now shows the same "PONDERING" walking-letter
highlight (in the green assistant palette) and triggers the same
auto-stop commit as dictation. Hold-to-talk users are unaffected: the
silence-watch task consults a new `KeyHeldFlags` pair in
`fono-hotkey` and suppresses both the overlay flip and the auto-stop
emit while the key is physically held. This also fixes a latent bug
where F7 hold-and-pause showed PONDERING and committed because the
listener always emits `TogglePressed` on press (hold-vs-toggle is
decided retroactively on release) — the FSM's `RecordingMode::Hold`
was effectively dead code on the keyboard path. Plan file:
`plans/2026-05-22-assistant-pondering-parity-v1.md`.

### What landed

- **`KeyHeldFlags { dictation, assistant }`** in
  `crates/fono-hotkey/src/lib.rs` — pair of `Arc<AtomicBool>` flipped
  inside the listener's `map_event` (and the portal backend) on every
  `Pressed`/`Released`/`CancelPressed`. Re-exported from the crate
  root and threaded into `SessionOrchestrator` via `daemon.rs`.
- **`SilenceWatchFlavor { Dictation, Assistant { auto_stop_commit } }`**
  inside `crates/fono/src/session.rs` parameterises the existing
  `spawn_silence_watch_task` so the dictation call stays a one-line
  wrapper while the assistant paths get their own overlay-state
  constructor (`AssistantPondering`), their own held-flag, and an
  optional `HotkeyAction::AssistantPressed` on commit.
- **`OverlayState::AssistantPondering { db, walk_progress }`** in
  `crates/fono-overlay/src/lib.rs` plus matching dispatch in
  `renderer.rs` (`accent_color`, `state_label`, `state_has_vu_bar`,
  walking-letter draw, waveform draw) — green palette + "PONDERING"
  label so the user keeps the dictation-vs-assistant colour contract.
- **Shadow `RecordingBuffer` for the streaming assistant path** in
  `build_live_capture_pipeline`: the drain task now feeds a small
  shared buffer that the silence watch consumes, mirroring the batch
  path's data flow. `LiveCaptureSession` gained a `silence_task`
  field aborted in all four teardown sites.
- **Auto-stop commits in both assistant paths** (batch +
  streaming) with `auto_stop_commit: true`. The held-flag gate is the
  single source of truth for "is the user still holding F8?", so
  hold-to-talk releases run as before while quick-tap toggle
  sessions get the same "stop when you stop talking" behaviour as F7.

## 2026-05-22 — Config simplification: 14 inert keys removed

A workspace-wide audit of `fono_core::config` found that 14 fields
were either entirely write-only or only ever consumed in tests /
bench harnesses. The Unreleased changelog block lists every dropped
key in full. Highlights:

- **`general.always_warm_mic`** — latency-plan L1 was never wired in
  `fono-audio`; the tray's *Keep microphone always-on* preference
  checkbox went with the field (`PreferencesSnapshot`,
  `TrayAction::SetAlwaysWarmMic`, and the daemon's match arm).
- **All `interactive.commit_*` / `eou_*` / `resume_grace_ms`** —
  boundary-heuristic knobs that look user-tunable in `config.toml`
  but never reached `LiveSession::with_heuristics`. Defaults move
  to `HeuristicConfig::default` in `crates/fono/src/live.rs` with
  identical values; runtime behaviour is byte-identical.
- **`interactive.budget_ceiling_per_minute_umicros`,
  `max_session_seconds`, `max_session_cost_usd`** plus the orphan
  `fono::live::budget_for` helper that read the first of those.

Existing configs continue to load — a new regression test
(`legacy_interactive_keys_are_ignored_silently`) locks in serde's
unknown-field tolerance for the dropped keys. Plan file:
`plans/2026-05-22-config-simplification-prune-interactive-and-warm-mic-v1.md`.

## 2026-05-22 — `fono install` auto-detects headless hosts

`sudo fono install` on a server no longer silently writes desktop
artefacts the operator never wanted. The subcommand now inspects the
host for any active graphical session (caller's inherited DISPLAY /
WAYLAND_DISPLAY, loginctl `Type=x11/wayland` + `State=active` sessions,
known display-manager units, `/tmp/.X11-unix/X*` sockets, Wayland
sockets under `/run/user/*`) and, when none are found, falls back to
`systemctl get-default` — `multi-user.target` (or no systemd at all)
flips the default to server mode with a one-line banner naming the
trigger. Anything ambiguous keeps today's silent desktop default, so
workstations are unaffected.

### What landed

- **`InstallModeArg { Server, Desktop, Auto }`** in
  `crates/fono/src/install.rs`, plus a new `--desktop` CLI flag
  (mutually exclusive with `--server`). `Auto` is the value used when
  neither flag is passed, and it dispatches to a new
  `detect_headless()` helper.
- **`detect_headless() -> (bool, &'static str)`** behind a
  `HeadlessProbes` trait so the six probe sites (env, loginctl
  list/show-session, `systemctl is-active <dm>`, `/tmp/.X11-unix/X*`,
  `/run/user/*/wayland-*`, `systemctl get-default`) are unit-testable
  without touching the host. Ten new tests cover every branch (active
  loginctl session, DM active, X11/Wayland socket present, multi-user
  default, graphical default, no-systemd-no-graphical, closing
  sessions ignored).
- **`packaging/install.sh`** now passes `--desktop` explicitly when
  its own DISPLAY heuristic decides desktop, so the shell wrapper and
  the binary's auto-detect can't disagree on the same host.
- **ADR `0023-self-installer.md`** picked up a dated addendum
  documenting the new default; CHANGELOG `[Unreleased]` block records
  the change for the next release.

Plan file:
`plans/2026-05-22-fono-install-headless-autodetect-v1.md` (all 7
tasks ticked).

## 2026-05-22 — Auto-stop on silence, slice 4 (commit wired)

Slice 4 of `plans/2026-05-22-fono-auto-stop-silence-v1.md` is in.
The `audio.auto_stop_silence_ms` config knob is now wired all the
way through: when the user sets it to a non-zero value, the
silence-watch state machine fires an actual stop after the
configured silence window.

### What landed

- **`SilenceWatchConfig::auto_stop_silence_ms: Option<u32>`** and
  the new **`SilenceEvent::Committed`** variant. Commit fires from
  `Pondering` after `silence_ms` (genuinely-silent frames only,
  voiced impulses don't accrue) clears the configured total. On
  commit the watch resets to `Armed` so it's single-shot per
  recording session. Five new unit tests pin the semantics:
  `commit_fires_after_total_silence_window`,
  `commit_resets_to_armed_single_shot`,
  `silence_only_never_commits`,
  `impulse_during_pondering_does_not_cancel_commit`,
  `auto_stop_none_disables_commit`.
- **`spawn_silence_watch_task` consumes `Committed`** by sending
  `HotkeyAction::TogglePressed` through the orchestrator's
  existing `action_tx`. The daemon's central loop translates this
  the same way as a real hotkey press (including
  `live_preview_enabled` mapping to `LiveTogglePressed`), so
  auto-stop is observationally identical to manual stop — same
  FSM transition, same `on_stop_recording` call, same overlay
  transitions to Processing → Polishing. No parallel code path.
- **Tray presets renamed**: `Off / 0.8 s / 1.5 s / 3 s` →
  `Off / 3 s / 5 s`. The old chat-app-derived values were wrong
  for prose dictation cadence.
- **Config doc-comment rewritten** at `crates/fono-core/src/config.rs:230`
  to describe the semantics: toggle-only, voice-relative threshold,
  speech preamble required by construction, no noise-floor estimator.

### Honest scope cuts

- **No integration test** in `crates/fono/tests/live_pipeline.rs`.
  The wiring is a single `action_tx.send` call; the unit-test
  matrix already covers every commit-event semantics with
  deterministic frame inputs. An integration test would require
  ~200 lines of orchestrator + overlay-stub + capture-pump
  scaffolding to assert one line of glue. Deferred unless dogfooding
  surfaces a wiring bug.
- **No `audio.debug.write_pcm`** PCM-dump-on-cutoff feature. The
  persistent debug config section was killed in slice 1; if we
  want post-mortem PCM dumps later they belong behind a CLI flag
  like `fono debug levels`, not in `config.toml`.
- **No `audio.debug.log_pondering`** transition-log knob, same
  reason. Slice 4 logs `INFO fono::auto_stop "auto-stop committed
  after N ms"` unconditionally — single line per commit, cheap.

### Verification protocol

Manual today (the only way to test the full wiring):

1. `~/.config/fono/config.toml` → `[audio] auto_stop_silence_ms = 5000`.
2. Restart fono.
3. Quick-tap the dictation hotkey (toggle mode).
4. Speak a sentence. Watch the bar's amber tick — the silence
   threshold.
5. Stop talking. After 1 s the overlay flips to `PONDERING`. After
   5 s total silence the recording stops, processing runs, text
   gets injected.
6. With `auto_stop_silence_ms = 10000`, same flow but the wait is
   longer; the walking-letter highlight is slower.
7. With `auto_stop_silence_ms = 0` (Off), no auto-stop — manual
   stop required (current default behaviour, regression check).

---

## 2026-05-22 — Auto-stop on silence, slice 3 (VU-bar enum + Advanced annotations)

Slice 3 of `plans/2026-05-22-fono-auto-stop-silence-v1.md` is in.
No new audio decisions; this slice repurposes the existing right-
side VU bar so the silence-watch envelope's reference signals are
**observable** while the actual auto-stop commit (slice 4) is
still being designed.

### What landed

- **`[overlay] volume_bar` is now an enum.** Breaking schema
  change, no migration shim:
  - `volume_bar = "off"`      — no bar (was `false`).
  - `volume_bar = "simple"`   — current linear-fill bar (was `true`).
  - `volume_bar = "advanced"` — new diagnostic flavour.
- **Bar paints during `Recording` and `Pondering` overlay states**,
  not only `LiveDictating` / `AssistantRecording`. `state_has_vu_bar`
  expanded; the bar's text-style gate (transcript panels only) is
  preserved so the waveform / oscilloscope / heatmap / FFT panels
  are untouched.
- **`Advanced` flavour** overlays three live ticks on the existing
  bar:
  - **Green tick** at the recent voiced-RMS reference
    (`EnvelopeSnapshot::voiced_rms` from slice 1's follower).
  - **Amber tick** at the silence threshold = `voiced_rms − 12 dB`,
    i.e. the line the slice-2 `SilenceWatch` uses to decide a frame
    is silent.
  - **White dot** at the instantaneous RMS.
  All three positions use the same `level / WAVEFORM_AMPLITUDE_CEILING`
  scaling as the bar fill, so the annotations align pixel-perfect.
- **`OverlayHandle::push_gate_metrics(inst, voiced, silence)`** is
  the new producer-side API. Pushed at 10 Hz from
  `spawn_silence_watch_task` in `crates/fono/src/session.rs`, which
  already runs the envelope follower. Renderer stores them
  unconditionally but only forces a redraw when the bar is in
  Advanced mode — `Off` / `Simple` users pay nothing.
- **Backends updated**: `winit_x11` + `wayland_shm` handle the new
  `OverlayCmd::GateMetrics` variant; `noop` silently drops it.

### What was deferred

- **Tray submenu for `volume_bar`** (plan 3.3). Folded into the
  slice-4 tray work where the auto-stop presets land. `Advanced` is
  config-file-only on purpose: end users shouldn't see it.
- **Snapshot tests** (plan 3.4 in the original form). Replaced
  with smaller renderer unit tests on `state_has_vu_bar`,
  `set_volume_bar` change detection, and `GateMetrics` default.

### Pre-commit gate

| Step | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --tests --lib` | green |

### How to dogfood

Edit `~/.config/fono/config.toml`:

```toml
[overlay]
volume_bar = "advanced"
```

Run a dictation session. During recording, the bar to the right of
the transcript will show the green voiced-RMS line climbing into
your speech range, the amber silence-threshold line ~12 dB below
it, and a white dot tracking your instantaneous level. As you
pause, the dot drops below the amber line; if the pause continues,
`Pondering…` engages (slice 2). The annotations make it visible
that the threshold *adapts* to how loud you happen to be speaking
in this session.

### Next slice

**Slice 4** — actually wire `auto_stop_silence_ms` into the
recording loop. Tray preset rename + bump (0 / 3 s / 5 s). State-
machine `Committed` → synthetic stop-recording. Gate rules
(toggle-mode only, speech-preamble required). PCM dump on commit.

---

## 2026-05-22 — Auto-stop on silence, slice 2 (Pondering state machine, visual only)

Slice 2 of `plans/2026-05-22-fono-auto-stop-silence-v1.md` is in.
The state machine now drives a visible `Pondering…` overlay state
during long pauses in dictation. **No auto-stop fires yet** — that
stays in slice 4. This slice exists so we can dogfood the
transition heuristics for as long as we like before committing the
recording loop to an automated stop.

### What landed

- **`crates/fono-audio/src/envelope.rs`** — re-added `voiced_rms`
  (medium EMA, ~500 ms) gated on `inst_rms_dbfs > -55 dBFS` so it
  only tracks above-noise content. The slice-1 rollback removed it
  along with the floor; slice 2 needs it as the reference signal
  for relative silence detection. 6 unit tests, all green.
- **`crates/fono-audio/src/silence_watch.rs`** — new state machine:
  - States: `Armed → Speaking → Pondering` (and back; no `Committed`
    emitted yet).
  - `Armed → Speaking` after ≥ `speech_confirm_arm_ms = 100 ms`
    of contiguous frames whose `inst_rms_dbfs ≥ voiced_rms_dbfs −
    silence_gap_db (12)`. Rejects coughs/clicks/key-presses.
  - `Speaking → Pondering` after ≥ `pondering_visual_ms = 1000 ms`
    of contiguous "quiet" frames (same `silence_gap_db` test,
    inverted). Sentence-end pauses (~800 ms) never trigger.
  - `Pondering → Speaking` on a single qualifying voiced frame —
    snap restore, no resume confirmation. The asymmetry is
    deliberate: thinkers must not feel UI lag when resuming.
  - Pure function over `EnvelopeSnapshot`; no audio API
    dependencies; 5 unit tests covering each transition direction
    and the cough-rejection case.
- **`crates/fono-overlay/src/lib.rs`** — new `OverlayState::Pondering
  { db }`. Mirrors `Recording { db }` everywhere it appears
  (state machine, IPC, renderer match arms).
- **`crates/fono-overlay/src/renderer.rs`** — when the overlay is
  in `Pondering`:
  - Label text becomes `"Pondering…"`.
  - 1 s plain-text grace after the transition.
  - Then a single-letter highlight walks left-to-right across the
    9 letters of `"Pondering"` (the `…` stays static). Highlight
    = `+45°` hue shift in HSV with a `+15%` saturation bump and
    value held constant — visible but not alarming.
  - Letter cadence is `(auto_stop_silence_ms − 2000) / 9` ms; at
    the 5 s preset that's ~333 ms/letter, at 10 s ~889 ms/letter.
  - If the walk window collapses to ≤ 0 (i.e. user manually set
    `auto_stop_silence_ms ≤ 2000 ms` in config.toml), the walk is
    skipped and the label stays plain "Pondering…".
- **`crates/fono/src/session.rs`** — `spawn_silence_watch_task`
  runs alongside `spawn_waveform_level_task`. It feeds capture
  frames through `EnvelopeFollower` → `SilenceWatch::observe()`
  → overlay state transitions. **Only armed when**:
  - Recording mode is toggle (not hold-to-talk).
  - `audio.auto_stop_silence_ms > 0`.
  - The dictation flow path is the user-text path (not assistant
    hold-release, which has explicit boundaries).

### What did NOT land in slice 2

- **Auto-stop commit.** `SilenceWatch` returns its state but never
  asks the session to stop. That's slice 4's job, gated on
  dogfooding data from this slice.
- **Floor-too-high notification.** Dropped; the slice-1 rollback
  removed the floor estimator we'd have compared against. Will be
  revisited in slice 4 if/when the floor returns.
- **`live_pipeline.rs` integration test.** Deferred; the
  per-module unit tests cover the same transitions deterministically.

### Pre-commit gate

| Step | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --tests --lib` | green |

### How to validate

Set `auto_stop_silence_ms = 5000` in `~/.config/fono/config.toml`
(or pick `3 s` from the tray submenu), then dictate something with
a deliberate ≥ 2 s pause. You should see the overlay label change
from `Recording` to `Pondering…`, then after 1 s the first letter
of `Pondering` tint warm/amber and the highlight walk one letter
to the right at the cadence shown above. Resuming speech snaps
the label back to `Recording` in one frame.

### Next slice

**Slice 3** — `volume_bar` config bool → enum `Off | Simple |
Advanced` (breaking schema change, no shim), plus the vertical
dBFS meter widget in Advanced mode. Per-overlay visibility keys.

---

## 2026-05-22 — Auto-stop on silence, slice 1 (envelope follower)

Slice 1 of `plans/2026-05-22-fono-auto-stop-silence-v1.md` is in.
Pure-measurement layer, no behaviour change in the recording loop
yet. Lands the audio envelope follower and a one-shot CLI to
inspect it against a live mic.

### What landed

- **`crates/fono-audio/src/envelope.rs`** — three-channel envelope
  follower:
  - `inst_rms`   — fast EMA (~30 ms) of frame RMS.
  - `voiced_rms` — medium EMA (~500 ms) over frames above the open
    gate.
  - `floor_rms`  — 20th-percentile of frame RMS over a 3 s sliding
    window (NOT a plain EMA — a plain EMA tracks voice as much as
    silence and would lift on every utterance).
  - Hysteresis built-in: open gate at `floor + 11 dB`, close gate
    at `floor + 6 dB`. 5 dB hysteresis band prevents thrash on
    signals hovering near threshold.
  - Adaptive: thresholds are derived from the floor, so a noisier
    room produces a higher gate automatically.
  - O(N + W) per frame with N = frame length, W = floor window
    (~150 frames). Cheap enough for the capture thread.
  - 6 unit tests covering pure silence, speech burst, hysteresis
    ordering, floor warm-up, dBFS clamp, alpha monotonicity.
- **`fono debug levels [--seconds N]`** (hidden CLI subcommand).
  Captures `N` seconds (default 10) from the default input device,
  feeds it through the follower, and prints a noise-gate-engineer-
  flavoured summary:
  ```
  Floor RMS           :  -52.0 dBFS  (p20= -51.4, p50= -50.2, p80= -49.0)
  Voiced RMS          :  -53.5 dBFS  (EMA over frames above the gate)
  Speech gate (open)  :  -41.0 dBFS  (floor + 11.0 dB)
  Silence gate (close):  -46.0 dBFS  (floor + 6.0 dB)
  Auto-stop verdict   : OK — floor below -25.0 dBFS noise ceiling
  ```
- CHANGELOG `## Added` entry under `[Unreleased]`.

### Design decisions worth recording

- **No `[audio.debug]` config section.** Earlier draft had three
  persistent toggles for envelope log / pondering log / PCM dump.
  Dropped on review — the data is a one-shot diagnostic, not a
  durable preference. Slice 2's transition logs and slice 4's PCM
  dump will route through ad-hoc CLI flags or tracing targets
  (`RUST_LOG=fono::silence_watch=info`) rather than config, keeping
  the on-disk schema free of debug knobs.
- **Slice 1.2 (wire envelope into capture thread) deferred to
  slice 2.** Nothing inside the daemon consumes the follower yet,
  so wiring it through `session.rs` before `SilenceWatch` exists
  would be dead code. The standalone CLI is sufficient for slice 1.

### Verification

Manual:
```
$ cargo run -q --bin fono -- debug levels --seconds 3
fono debug levels: capturing 3s @ 16000 Hz mono ...
... done.

Frames observed     : 141 (2.82 s @ 20 ms/frame)
Voiced frames       : 32 (above the open gate)
Floor RMS           :  -52.0 dBFS  (p20= -51.4, p50= -50.2, p80= -49.0)
...
Auto-stop verdict   : OK — floor below -25.0 dBFS noise ceiling
```

Automated: pre-commit gate clean — `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --tests --lib` (all 6 new envelope tests
plus the existing suite pass).

### Next

Slice 2 of the same plan: `SilenceWatch` state machine (`Armed →
Speaking → Pondering → (Committed)`) with the **only** observable
effect being the overlay "Pondering…" label + state pill. No
auto-stop yet — that's slice 4, after slice 2 has been dogfooded.

---

## 2026-05-20 — Wayland overlay: pluggable backend layer, GNOME via Xwayland

Phase 0 + Phase 1 of
`plans/2026-05-19-overlay-backend-architecture-v1.md` plus the
GNOME placement follow-up
`plans/2026-05-20-overlay-gnome-prefer-xwayland-v1.md`. The user
reported on Ubuntu 24.04 GNOME that the existing
`winit + softbuffer` Wayland path produced an opaque charcoal
rectangle in the top-left corner that stole focus, and an interim
`xdg_toplevel` fix only resolved the transparency: Mutter still
treated the surface as a normal app window (Alt+Tab, no
always-on-top, compositor-chosen placement). Root cause is
protocol-level — `xdg_toplevel` is the protocol for "application
toplevels" and there is no client-side hint that overrides
Mutter's treatment. Reworked the overlay into a pluggable backend
layer with runtime selection driven by `WAYLAND_DISPLAY` /
`DISPLAY`.

### Architecture

`crates/fono-overlay/src/` now has two cleanly separated layers:

- **`renderer.rs`** — pure software-rasterised drawing into an
  ARGB premultiplied `&mut [u32]` framebuffer. No `winit`, no
  `softbuffer`, no `wayland-client`. Unit-testable. Owns the
  FFT / oscilloscope / heatmap / transcript / VU bar visualisations
  unchanged from the previous implementation.
- **`backend.rs`** + **`backends/`** — `BackendId`, `OverlayCmd`,
  `OverlayHandle`, and three windowing implementations:
  * **`backends/wayland_layer_shell.rs`** — `zwlr_layer_shell_v1`
    primary path via `smithay-client-toolkit 0.19` +
    `wayland-protocols-wlr 0.3`. `Layer::Top`, `Anchor::BOTTOM`,
    640 × dynamic-height surface anchored 48 px above the bottom
    edge. ARGB8888 `wl_shm` via SCTK's `SlotPool` (double-buffered),
    `keyboard_interactivity = None`, empty `wl_region` input
    region. Used on every wlroots-based compositor plus KDE Plasma
    5.27+, COSMIC, Wayfire, niri, labwc.
  * **`backends/winit_x11.rs`** — the original winit + softbuffer
    path, now X11-only after the winit Wayland strip. Override-
    redirect + `_NET_WM_WINDOW_TYPE_NOTIFICATION` so the window
    manager bypasses placement, stacking, and Alt+Tab handling.
    Also used on Wayland sessions via Xwayland (the GNOME /
    KDE-Wayland default).
  * **`backends/noop.rs`** — terminal sink. `spawn_overlay`
    always returns `Ok` so the daemon never aborts on a missing
    graphics environment.
  * **`backends/wayland_shm.rs`** — shared `SlotPool` framebuffer
    plumbing + self-pipe waker + `rustix::event::poll`-based
    event-loop multiplexer used by the Wayland backend.

### Selection table

`crates/fono-overlay/src/backend.rs::candidate_list_with` is the
single source of truth. Driven by env-var presence (the actual
protocol probe happens at each backend's `try_spawn` time):

| `WAYLAND_DISPLAY` | `DISPLAY` | Candidate order |
|---|---|---|
| set | set | `wlr-layer-shell` → `x11-override-redirect` → `noop` |
| set | unset | `wlr-layer-shell` → `noop` |
| unset | set | `x11-override-redirect` → `noop` |
| unset | unset | `noop` |

On GNOME the layer-shell `try_spawn` returns `NotAvailable` because
Mutter doesn't implement `zwlr_layer_shell_v1`, and selection falls
through to the X11 backend running under Xwayland. Mutter respects
Xwayland override-redirect: the overlay is client-positioned, stays
above normal windows, and is excluded from Alt+Tab and the
taskbar — same UX as on a native X11 session. Fractional HiDPI
scaling renders cleanly via Xwayland (live-verified on Ubuntu 24.04
GNOME / `192.168.0.112`). The `wayland-xdg-fallback` that briefly
existed in the design space is deliberately omitted from the
shipped backend set: `xdg_toplevel` cannot deliver a panel UX on
Mutter, and the rare Wayland-only-no-Xwayland case is better served
by `noop` + a `fono doctor` hint than by a degraded surface.

### `FONO_OVERLAY_BACKEND` override

Operator escape hatch with values `wlr` / `x11` / `noop` (case-
insensitive, plus a few aliases). Forced selection still falls
through to `noop` on failure so the daemon never aborts. Unknown
values fall through to automatic selection with a warning logged.

### `fono doctor` integration

`crates/fono/src/doctor.rs` reports the selected backend on the
`Overlay     :` line with its `BackendCapabilities` summary
(`transparency`, `positioning`, `focus-passthrough`,
`click-passthrough`). On a Wayland session that ends up on the
`noop` backend (no layer-shell, no Xwayland) doctor prints a hint
to install the distro's `xwayland` package.

### Test surface

`crates/fono-overlay/src/lib.rs::tests` exercises the candidate-
list logic under mocked env-var presence via
`backend::pick_backend_with`. Five unit tests cover the selection
table rows plus the forced-override + unknown-value behaviour.

### Win on dep graph

`Cargo.toml` workspace `winit` is now
`{ default-features = false, features = ["x11", "rwh_06"] }` —
winit's Wayland event-loop, the SCTK transitive deps it pulled, and
softbuffer's Wayland buffers are no longer compiled into the
binary. The Wayland-native protocol surface is now a direct
dependency of `fono-overlay` only, gated behind the `backend-wlr`
cargo feature. `cargo tree -p winit | grep -iE 'wayland|sctk|smithay'`
returns empty.

### Gate

`cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace --tests
--lib` — all green. `cargo build --profile release-slim -p fono`
≈ 21.24 MiB, under the 22 MiB CPU `size-budget` CI gate (see
`.github/workflows/ci.yml:184`).

## 2026-05-19 — Rename: `LlmBackend` → `PolishBackend`

The post-STT cleanup role was previously called "LLM backend", which
collided with the equally LLM-powered `AssistantBackend`. Both roles
are now named after what they do, not what's under the hood:
`AssistantBackend` (chat) and `PolishBackend` (post-STT cleanup). The
overlay already said `"Polishing…"` for this stage, so the rename
aligns code, config, and UI on the same word.

Mechanical sweep across the workspace (no behaviour changes):

- Crate `fono-llm` → `fono-polish`.
- Types: `Llm` → `Polish`, `LlmBackend` → `PolishBackend`,
  `LlmLocal/LlmCloud/LlmRegistry/LlmModelInfo/LlmDefaults` and
  `LLM_MODELS` follow suit.
- Functions: `build_llm`, `llm_backend_str`, `parse_llm_backend`,
  `configured_llm_backends`, `all_llm_backends`, `llm_key_env`,
  `llm_requires_key`, `ensure_local_llm` → `polish_*` /
  `build_polish` / `ensure_local_polish`.
- Config: `[llm]` / `[llm.local]` / `[llm.cloud]` / `[llm.prompt]`
  TOML sections become `[polish.*]`. Cache path
  `~/.cache/fono/models/llm/` → `~/.cache/fono/models/polish/`.
- CLI: `fono use llm <name>` → `fono use polish <name>`;
  `--llm` / `--no-llm` → `--polish` / `--no-polish`.
- Tray: `TrayAction::UseLlm` → `UsePolish`; submenu label
  `"LLM backend"` → `"Polish backend"`.
- Notifications: `Stage::Polish` now displays as `"Polish"` instead
  of `"LLM"`; `"Fono — LLM key rejected/unreachable/cleanup failed"`
  → `"Polish key rejected/unreachable/failed"`.
- Docs sweep: `README`, `ROADMAP`, `AGENTS.md`, `docs/architecture.md`,
  `docs/providers.md`, `docs/troubleshooting.md`, `docs/privacy.md`,
  `docs/inject.md`, `docs/interactive.md`,
  `.github/ISSUE_TEMPLATE/bug_report.md`. References to LLM as the
  *role* renamed; references to LLM as the *underlying technology*
  ("a small LLM", "chat-trained LLMs", "Groq LLM offering") left
  intact. Closed plans (`plans/closed/`), historical design plans
  (`docs/plans/`), ADRs (`docs/decisions/`), and `CHANGELOG.md`
  untouched as historical record.

Breaking config change accepted (no users yet per ADR 0026 pre-1.0
posture): existing `config.toml` files with `[llm]` sections and
GGUFs under `models/llm/` will silently re-resolve to defaults on
the next launch.

Gate: `cargo fmt --all`, `cargo clippy --workspace --all-targets
-- -D warnings`, `cargo test --workspace --tests --lib` all green
(576 tests passed, 0 failed).

## 2026-05-19 — STT quantization ladder (ADR 0027)

Landed Phases 1–5 of
`plans/2026-05-19-stt-perf-pass-v1.md`. Two days of perf-pass
sweeps on four reference hosts (i7-7500u, i7-1255U, ultra7-258v,
ryzen-5950x; AC; CPU + Vulkan where applicable) drove the design
of a 3-rung quantization ladder selected per ADR 0027. Pre-release
so no compat shim was needed.

Highlights:

- **`set_audio_ctx()`** on clips < 30 s gives +70–160 % CPU batch
  RTF with no measurable quality regression. Hard-coded on in
  `crates/fono-stt/src/whisper_local.rs`; debug-only env override
  retained for ablation runs.
- **Thread default** switched from logical-CPU count to physical
  cores parsed out of `/proc/cpuinfo`, clamped 1..=16. 5950X data
  showed `small` running at half the speed at `t=32` vs `t=16`
  because SMT siblings contend on the 256-bit FMA unit.
- **Registry rewrite** (`crates/fono-stt/src/registry.rs`): new
  `Quantization` / `QuantizationPref` types, `ModelInfo` carries
  `default_quantization` + `&[QuantVariant]`. Five user-facing
  names ship (T1 `tiny`/`tiny.en`, T2 `small`/`small.en`, T3
  `large-v3-turbo`); `base` / `base.en` removed entirely as
  dominated by T2. `large-v3-turbo` defaults to `q8_0`; `q5_0`
  variants dropped (catastrophic on `en-conversational`).
- **Config** (`crates/fono-core/src/config.rs`):
  `[stt.local].quantization = "auto"` is the new default and
  resolves through the registry. `auto | fp16 | q8_0 | q5_1`.
- **Wizard / CLI**: `fono models list` shows defaults +
  installable alternatives; `fono models install <name>
  --quantization <q>` resolves through the registry; `fono models
  remove <name>` deletes all variants of the named family. The
  existing `AccuracyBucket::Inaccurate` filter handles the `tiny`
  multilingual caveat (unusable for Romanian / Chinese / Japanese)
  via `wer_by_lang` thresholds — no new gating.
- **`scripts/bench-accuracy.py`** rewritten to surface per-language
  Δ accuracy. The non-English-fixture floor previously masked the
  `base-q8_0` regression on `en-narrative-pause` (0.114 → 0.513).
  Future sweeps catch this class of regression automatically.

Worst-case install footprint per language mode: ~1.1 GB English /
~1.3 GB multilingual (down from ~3 GB if a user previously fetched
several fp16 variants).

Pre-commit gate clean. Custom-quantized `large-v3-turbo-q5_1`
(would slot at ~548 MB between T2 and T3) deferred to the roadmap
as a research item.

## 2026-05-19 — mDNS browser robust against co-resident responder

Fixed a registry-drain bug surfaced after ~24 h of uptime on hosts
where `avahi-daemon` also listens on UDP 5353. Linux `SO_REUSEPORT`
load-balances incoming multicast across all listeners, so Fono's
`mdns-sd` browser misses roughly half of all responses; combined
with `mdns-sd`'s exponential retransmission backoff (1 s → 2 s →
…up to 1 h), peers age out of the registry under the 120 s TTL and
never come back until daemon restart.

Two changes in `crates/fono-net/src/discovery/`:

- `browser.rs`: added a 60 s `REBROWSE_TICK` that re-invokes
  `daemon.browse(ty)` for each active service type. This forces a
  fresh PTR query, resets the retransmission backoff, and replays
  the cache to a new listener — so even with REUSEPORT eating half
  the replies, ~5 attempts per `PEER_TTL` window keeps the registry
  populated indefinitely. Refactored `recv_first` to take an owned
  cloned snapshot of the receiver Vec so the canonical Vec can be
  mutated by the new select arm without borrow conflicts.
- `mod.rs`: `PEER_TTL` bumped from 120 s to 300 s for defence in
  depth.

No public API change; existing integration test
`tests/discovery_round_trip.rs` continues to pass. Live LAN verified:
both `fono-ai` (Whisper STT) and `piper-ai` (Piper TTS) remain in
`fono discover` indefinitely on a host where `avahi-daemon` is also
running.

Pre-commit gate clean: `cargo fmt`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace --tests --lib`.

## 2026-05-17 — Live preview folded into overlay style picker

Landed `plans/2026-05-17-live-transcript-as-overlay-style-v2.md`.
The old `[interactive].enabled` flag is gone; live preview is now the
fifth entry in the tray's waveform-style picker (`Bars |
Oscilloscope | Fft (default) | Heatmap | Transcript`). Picking
Transcript both swaps the overlay renderer to streaming text and
routes the dictation hotkey through the live pipeline — this fixes
the reported bug where live transcription only worked for the
assistant, not for dictation. `Fft` stays the first-run default
because live preview costs more CPU on local STT and more tokens on
streaming-capable cloud backends; the tray label
(`"Transcript (live preview — more CPU / tokens)"`) makes the cost
visible at the click site.

Internally:

- `WaveformStyle::Transcript` added (`crates/fono-core/src/config.rs`),
  `Interactive::enabled` field deleted, `Config::live_preview()`
  helper added as the single source of truth.
- `OverlayMode` collapsed into `WaveformStyle`; `RealOverlay::spawn`
  takes a `WaveformStyle` and the twin `spawn_waveform` /
  `enable_text_mode` / `enable_waveform_mode` entry points are gone.
- `translate_for_interactive` → `translate_for_live_preview`; factory
  parameter renamed `interactive_enabled` → `live_preview`. Every
  `cfg.interactive.enabled` reader now calls `cfg.live_preview()`.
- Wizard's live-mode prompt removed; tray is the single control.
  Doctor row prints `"live preview : enabled/disabled (style=…,
  mode=…)"` so users can diagnose "I picked Transcript and nothing
  happened" without debug logging.

Pre-commit gate clean: `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace --tests --lib`
(all suites green). ADR 0026 records the decision; CHANGELOG
`[Unreleased]` updated.

## 2026-05-15 — Local STT affordability calibration Phase 0 (AC sweep)

Phase 0 of `plans/2026-05-15-local-stt-affordability-recalibration-v4.md`
landed. Four hosts benched on AC, renamed to stable CPU-based IDs
spanning four CPU tiers from 2016 to 2024:
`ryzen-5950x` (AMD Ryzen 9 5950X, 16p/32l Zen 3 desktop, **rel. 2020-11,
high-end desktop**, 48 GiB; was `192.168.0.79`),
`ultra7-258v` (Intel Core Ultra 7 258V, 8p/8l Lunar Lake laptop,
**rel. 2024-09, current premium ultraportable**, 31 GiB; was
`192.168.0.251`),
`i7-1255u` (Intel i7-1255U, 2P+8E hybrid / 12 threads Alder Lake-UP3
15 W laptop, **rel. 2022-02, mid-range ultraportable**, 15 GiB; was
`localhost`),
`i7-7500u` (Intel i7-7500U, 2p/4l Kaby Lake 15 W laptop, **rel. 2016-08,
legacy ultraportable ~10 years old**, 15 GiB; was `192.168.0.112`).
Three iterations of the equivalence harness
per (host, model) cell except `large-v3-turbo` on the two slowest laptops
where a single iteration was enough to clear the `unsuitable` verdict.

Headline result: `large-v3-turbo` on CPU is `unsuitable` on every laptop
(batch RTF 0.21–0.61), `borderline` on the 16-core desktop (1.75).
`crates/fono-stt/src/registry.rs:194-219`'s current
`realtime_factor_cpu_avx2 = 2.5` for turbo is therefore overstated by
1.5–10× depending on host — the wizard's recommendation chain is built
on an over-optimistic single number. Peak RSS for turbo lands at ~3.6
GiB across hosts (current `min_ram_mb = 3400` is too tight).
`small`/`small.en` is `borderline` on every laptop and `comfortable` only
on the 16-core desktop; `base` and `tiny` are universally `comfortable`.

Artefacts under `docs/bench/calibration/`: per-host inventory JSONs, raw
per-iteration runs (with rusage sidecars), aggregated `summary/matrix.
json` + `matrix.md`, and a methodology README. Driver scripts under
`scripts/bench-*.{py,sh}`.

GPU (Vulkan) coverage added in a follow-up sweep the same day. Vulkan
SDK + `glslc` installed on Ubuntu host `i7-7500u` (`vulkan-tools`,
`libvulkan-dev`, `glslang-tools`, `spirv-tools`, `glslc`); `fono-bench`
rebuilt `--features 'accel-vulkan equivalence'` on `ultra7-258v`
(Intel Arc 130V/140V Xe2 Battlemage, 1m48s) and `i7-1255u` (Intel
Iris Xe Alder Lake-UP3 96 EUs, 3m54s). Headline GPU finding:
`large-v3-turbo` on **Arc Battlemage Vulkan jumps from batch RTF 0.61
(unsuitable) to 8.72 (comfortable)** — a 14× speedup, and the first
`comfortable` turbo cell in the matrix. Streaming RTF 0.20 → 3.16
(16×). On Iris Xe the same model goes 0.33 → 1.56 (5×, lifts out of
`unsuitable` to `borderline` but not to `comfortable`). The class
difference between two Intel iGPUs is large enough that Phase 1's
`accelerated()` predicate must differentiate GPU classes, not collapse
to a single boolean. Vulkan also drops host RSS by ~10× because most
state moves to GPU memory (~300 MiB vs ~3.6 GiB on CPU for turbo).

GPU coverage blockers that remain: `ryzen-5950x` RTX 4090 is **still
not benchable**. NVIDIA driver install attempted on the Proxmox host
(PVE 9.1.9, kernel `7.0.0-3-pve`) on 2026-05-15: Debian
`nvidia-kernel-dkms` 550.163.01-2 plus three NVIDIA `.run` installers
(575.57.08, 580.65.06, 580.95.05) all fail to build the kernel module.
Root cause is that PVE 9 renumbered the kernel from Linux 6.14 to
`7.0.0` in both the Makefile and `LINUX_VERSION_CODE` (458752 =
7×65536). NVIDIA's source uses `LINUX_VERSION_CODE` for compile-time
API selection; no driver recognises kernel 7.x and they all fall back
to the oldest code path, hitting the Linux 6.11 `__assign_str` macro
signature change and 6.14 VMA-locking changes. The host was left
clean (Proxmox VE 9.1.9 healthy, both LXCs running, broken dkms
registration removed, half-installed apt packages purged); build
deps `proxmox-headers-7.0.0-3-pve`, `dkms`, `build-essential`, and
the full CUDA 12.4 userland are retained for the next retry, and the
`.run` installers are cached under `/root/`. A status note is at
`/root/NVIDIA-INSTALL-STATUS.md` on the Proxmox host. LXC `ai`
(CT 107) at `/etc/pve/lxc/107.conf` keeps its existing passthrough
config (`/dev/nvidia*` bind-mounts + cgroup allow); the moment a
working `nvidia.ko` lands on the host, the container will see the
devices automatically. Retry paths: (1) wait for NVIDIA 585+ with
explicit PVE-7.0 detection; (2) boot `pve-kernel-6.8` (Proxmox still
publishes it); (3) apply the PVE-forum community patches to NVIDIA's
`nv-mm.h` / `nv-tracepoint.h`.

`i7-7500u` (Ubuntu, HD 620 Kaby Lake) had
the Vulkan SDK installed cleanly but the `whisper-rs 0.16.0` Vulkan
binding references symbols (`ggml_backend_vk_buffer_type`,
`ggml_backend_vk_get_device_count`, …) that have been renamed in the
current whisper.cpp upstream that `whisper-rs-sys` cmake-fetches;
build fails. Phase 1 should either pin whisper.cpp or upgrade
whisper-rs.

Battery half of the matrix still pending — unplug the three laptops,
Battery half of the matrix landed the same day. The two modern Intel
laptops (`i7-1255u` Alder Lake 2022, `ultra7-258v` Lunar Lake 2024)
were unplugged and re-benched on both CPU and Vulkan builds (1
iteration per cell across all 7 wizard-visible models — battery
budget too tight for 3 iter on turbo). All 26 AC↔battery cells were
power-validated via the rusage sidecar (`ac_online` and
`battery_pct` captured at run start and end) and confirmed
`BATTERY`-throughout. **Result: zero verdict bucket flips between AC
and battery on either laptop.** Batch RTF deltas are within ±10 % on
average (in the noise range of the 15–30 % stddev measured between
AC iterations for the same cells), and crucially Vulkan GPU
acceleration does NOT throttle on battery — Arc Battlemage on
`ultra7-258v` delivered turbo at 9.03 batch RTF on battery vs 8.72
on AC. **Phase 1 implication: the proposed battery-aware
affordability gate (plan v4 Task 1.5) can be dropped.** The
older `i7-7500u` (2016 Kaby Lake) and the desktop `ryzen-5950x`
were not battery-benched (no battery on the desktop; the legacy
laptop is not the user's daily driver and would mostly confirm
unsuitable-stays-unsuitable). Phase 1 (registry refit, predicate
changes) follows in a separate session.

## 2026-05-15 — Added `scripts/capture-overlay.sh` for README screencasts
Landed the overlay-screencast helper per
`plans/2026-05-15-overlay-screencast-script-v2.md`: a single bash script
that records the Fono overlay in three modes (`overlay`, `paste`,
`gallery`), detects X11/Wayland, encodes MP4 + GIF + WebP with size-
budget auto-tiering, and is documented under
`docs/troubleshooting.md` → "Capturing screencasts". Dev-only tooling;
no Rust/runtime changes.

## 2026-05-14 — Fix: cancel hotkey leaked after natural assistant completion

User reported Fono was holding a global grab on `Escape` even when no
dictation or assistant session was active. Root cause: the dynamic
`HotkeyControl::DisableCancel` was only sent from the FSM-event consumer
on explicit `Stop*` / `Cancel` events, but the assistant's
natural-completion path returns from `AssistantThinking` /
`AssistantSpeaking` to `Idle` via `HotkeyAction::ProcessingDone` alone
(`crates/fono-hotkey/src/fsm.rs:222-225`), which emits no `HotkeyEvent`.
After the first assistant turn finished on its own, the Escape grab
stayed live until the next cancel / barge-in. Fix is belt-and-braces:
the action dispatcher in `crates/fono/src/daemon.rs:733-770` now also
sends `DisableCancel` whenever the FSM transitions back to
`FsmState::Idle`, so every future code path that lands in Idle releases
the grab automatically. The existing event-driven `EnableCancel` /
`DisableCancel` arms are unchanged; this is purely an additional
safety-net release.

## 2026-05-13 — Release v0.8.0

Tagged-ready release wrapping six commits since v0.7.1 that together
land the Phase A–F roadmap of
`plans/2026-05-13-2026-05-13-wizard-catalogue-multimodal-and-multi-tts-issues-9-11-v2.md`
(issues #9 + #11). Phase G (release engineering) is complete; the
plan is fully executed.

- **Phase A — Cloud provider capability catalogue.** New
  `fono_core::provider_catalog::CLOUD_PROVIDERS` table is the single
  source of truth for which cloud providers offer STT / LLM /
  Assistant / Vision / Web search / TTS. The wizard, tray, `fono use
  cloud`, and `fono doctor` all consume it, eliminating five
  duplicated `match` blocks in the wizard. Recorded in
  `docs/decisions/0025-cloud-provider-catalogue.md`.
- **Phase B+F7 — Wizard cloud branch collapse (#9).** Picking OpenAI
  or Groq now configures STT, LLM cleanup, the assistant, and TTS
  from a single API-key prompt; picking Anthropic / Cerebras /
  OpenRouter configures LLM + Assistant and prompts for follow-ons
  only for capabilities the primary doesn't cover. Capability badges
  (`STT · LLM · Assistant · TTS · Vision · Search`) are derived from
  the catalogue at runtime. `PathChoice::Mixed` renamed to
  `Customize`. Re-runs reuse `secrets.toml` keys silently via
  `prompt_or_reuse_key`.
- **Phase E — Optional assistant extras.** Two new `[assistant]`
  toggles, `prefer_vision` and `prefer_web_search`, surface in the
  wizard's *Optional extras* MultiSelect when the chosen primary
  supports them (OpenAI / Anthropic / Groq / Gemini for vision;
  OpenAI / Anthropic / Gemini for web search). Defaults are `false`.
- **Phase F — Multi-provider TTS (#11).** Four new TTS backends ship
  alongside OpenAI and Wyoming: Groq (Orpheus `canopylabs/orpheus-v1-english`),
  OpenRouter (Kokoro `hexgrad/kokoro-82m`), Cartesia (`sonic-2`), and
  Deepgram (`aura-2-thalia-en`). Existing `CARTESIA_API_KEY` /
  `DEEPGRAM_API_KEY` from STT usage are reused automatically; the
  wizard's TTS picker orders providers with stored keys first.
- **Phases C + D — Documentation, integration tests, ADR.** Wizard
  rework integration tests, multi-TTS integration tests, and the
  catalogue ADR landed in commit `25c4dbc`.

Phase G mechanics: workspace version bumped 0.7.1 → 0.8.0,
`CHANGELOG.md` `[Unreleased]` renamed to `## [0.8.0] — 2026-05-13`
with a fresh empty `[Unreleased]` above it, ROADMAP table + Shipped
list updated. `cargo build --workspace`, `cargo test --workspace
--lib --tests`, and `cargo clippy --workspace --all-targets --
-D warnings` are all green. Tag/push deferred to the orchestrator.

## 2026-05-12 — Issue #8: cascade-capped critical notifications

Extended `fono_core::critical_notify` to cover every user-blocking
pipeline stage and added a **global cascade cap**: at most one
Critical-urgency desktop notification per dictation session, no
matter how many downstream stages fail off the same root cause.

- **New stages.** `Stage` gains `Tts`, `Assistant`, `Inject`
  variants (and is now `#[non_exhaustive]`). TTS auth/network
  failures during assistant playback, assistant chat stream-open
  and mid-stream errors, and text-injection failures all route
  through the same dedup surface as STT/LLM.
  (`crates/fono-core/src/critical_notify.rs:37-69`).
- **Cascade cap.** A new `SESSION_HAS_FIRED: Mutex<bool>` gate
  short-circuits `notify()` after the first fire; cleared by
  `reset_session_flag()` (already called at every recording start
  in `crates/fono/src/session.rs:1134` and `:2011`) and by the
  120 s `AUTO_RESET_AFTER` window.
  (`crates/fono-core/src/critical_notify.rs:148-260`).
- **LLM `Network` now notifies** alongside `Auth`, both batch
  (`crates/fono/src/session.rs:2510-2526`) and live-dictation
  (`crates/fono/src/session.rs:2206-2229`) paths.
- **Injection failures notify** at
  `crates/fono/src/session.rs:2533-2558`.
- **Assistant + TTS wired** at `crates/fono/src/assistant.rs:189-220`,
  `:255-280`, `:373-401`.
- **Daemon startup failure** fires a one-shot notification at
  `crates/fono/src/cli.rs:429-443` (bypasses the session cap; only
  one daemon-startup path can fail per process).

New unit tests lock the cascade cap and the post-reset re-arm
behaviour (`crates/fono-core/src/critical_notify.rs:481-549`). All
17 `critical_notify` tests pass; `cargo clippy --workspace
--all-targets -- -D warnings` is clean.

## 2026-05-06 — Hotkey behaviour: auto short/long-press

Removed the `[hotkeys].mode = "toggle" | "hold"` configuration knob.
The dictation and assistant hotkeys now decide their own behaviour
per press based on duration:

- **Short press** (< 1 s) — toggles recording on; the next short press
  stops it.
- **Long press** (≥ 1 s) — push-to-talk; recording stops on release.

Implementation: `fono_hotkey::listener::map_event` records the
press timestamp on every Pressed event and emits the corresponding
`TogglePressed` / `AssistantPressed` action immediately so the user
gets instant feedback. On Released it synthesises a second
press-action only when the elapsed time crosses
`LONG_PRESS_THRESHOLD` (1 s). `CancelPressed` clears both pending
press timestamps so a late key-up after Escape cannot re-arm the
FSM. The `HotkeyMode` enum, the `Hotkeys::mode` field, and the
listener's mode-driven dispatch table are gone; old configs with
`mode = "..."` still load (serde silently ignores unknown fields)
but the value has no effect. `fono doctor` and the wizard summary
no longer print a mode line. New unit tests in `listener.rs` cover
short press, long press (both keys), and the cancel-then-late-release
race. `cargo test -p fono-core -p fono-hotkey` is green.

## 2026-05-05 — Release v0.7.1

Tagged v0.7.1. Patch release: default hotkeys overhauled.

- **Dictation collapses to `F7`; voice assistant moves to `F8`.**
  Old defaults (F8 hold / F9 toggle / F10 assistant) collided with
  htop's kill / quit / nice bindings and, for F10, the GTK menubar
  shortcut. The two dictation keys merge into one and the assistant
  key drops down by two.
- **One global `[hotkeys].mode = "toggle" | "hold"`** replaces the
  two-key hold-vs-toggle split. `Toggle` (default) means press once
  to start, press again to stop, and now applies to the assistant
  too — no more holding a key through the multi-second STT → LLM →
  TTS round-trip.
- **`[hotkeys].toggle` renamed to `[hotkeys].dictation`** with a
  serde alias so old configs continue to parse. `[hotkeys].hold`
  field removed; push-to-talk is expressed as `mode = "hold"`.

CHANGELOG.md, ROADMAP.md updated; Cargo.toml + Cargo.lock bumped
0.7.0 → 0.7.1; packaging/slackbuild/fono/fono.info bumped.

## 2026-05-04 — Release v0.7.0

Tagged v0.7.0. Headline feature: a voice assistant alongside
dictation.

- **F10 hold-to-talk** captures audio, transcribes via the
  existing STT backend, asks a chat-capable LLM (independent
  backend selection from `[llm]` cleanup), streams the reply
  sentence-by-sentence into a TTS backend, and plays the audio
  through the speakers. First sentence starts speaking before
  the model has finished generating.
- **Two new crates** — `fono-tts` (Wyoming protocol client +
  OpenAI `/v1/audio/speech` + Piper-stub) and `fono-assistant`
  (streaming chat trait + Anthropic Messages API + the full
  OpenAI-compatible family). `fono-audio::playback` adds a
  paplay-based output worker on the Linux release variant.
- **`[assistant]` / `[tts]` config blocks**, multi-turn rolling
  history, cancellation (F10 again =
  barge-in, Escape = shut up). New CLI subcommands
  (`fono use assistant|tts`, `fono assistant {press,release,
  stop}`), new tray entries + backend submenus, wizard step,
  doctor coverage.
- **Overlay** paints green during assistant recording and amber
  during the post-release thinking phase, with per-style
  synthetic animations (FFT scanner, symmetric bars, harmonic-
  processing oscilloscope, neural-strands heatmap). Default
  `[overlay].style` flipped Bars → FFT.
- **Cloud model defaults refreshed** to current production
  models: Cerebras `llama3.1-8b` / `qwen-3-235b-a22b-instruct-2507`, Groq
  `openai/gpt-oss-20b` / `openai/gpt-oss-120b`, OpenAI
  `gpt-5.4-nano` / `gpt-5.4-mini`, Anthropic
  `claude-haiku-4-5-20251001`. OpenAI-compat client now uses
  `max_completion_tokens` (newer OpenAI models reject the
  legacy `max_tokens` field).
- **Release CI** gains a `cloud-assistant` gate running the new
  `smoke_assistant` example (`--ci` mode covers Groq + Cerebras;
  local devs run the full 4-cloud + OpenAI-TTS pass).

## 2026-05-03 — Release v0.6.1

Tagged v0.6.1. Patch release focused on headless / systemd
robustness:

- Vulkan probe moved into a disposable subprocess
  (`FONO_INTERNAL_VULKAN_PROBE=1`) so a broken ICD (Mesa `lvp`
  worker threads, etc.) can't segfault the daemon on shutdown via
  `dl_fini`. Result cached in a `OnceLock`; spawn / timeout /
  parse failures collapse to `Outcome::NotAvailable`.
- `fono_hotkey::spawn_listener` gated on `is_graphical_session()`
  to avoid `global-hotkey` 0.6.4's `XOpenDisplay(NULL)` ->
  `XDefaultRootWindow` segfault on hosts without `DISPLAY` /
  `WAYLAND_DISPLAY`.
- Implicit first-run wizard gated on `stdin().is_terminal()` so
  `fono.service` stops crash-looping on missing config; falls back
  to `Config::default()`. Explicit `fono setup` unchanged.
- `sudo fono install` now waits 2 s, runs `systemctl is-active`,
  and dumps the last 20 journal lines + the recommended follow-up
  command when the unit fails to stay up.
- `daemon --no-tray` flag removed (tray is already runtime-gated).
  CLI clients try `/var/lib/fono/fono.sock` before the per-user socket,
  so a system-wide `fono.service` is drivable from any account.
- `general.sound_feedback` config + tray "Start/stop chimes"
  toggle + chime playback action removed; the v0.6.0 audio-vis
  overlay covers the same UX role.
- `[overlay].waveform` defaults to `true` (was `false`); existing
  configs with an explicit value are unaffected.

CHANGELOG.md and ROADMAP.md updated; Cargo.toml + Cargo.lock bumped
0.6.0 -> 0.6.1.

## 2026-05-03 — Vulkan prewarm: silent decode at session start

`plans/2026-05-03-whisper-vulkan-prewarm-v1.md` landed.

Bench on `ai` (RTX 4090 + Vulkan, `large-v3-turbo`) revealed that the
first Vulkan fixture paid a 7.8 s pipeline-create stall while every
subsequent fixture finished in 0.1–0.2 s — the cost was
`whisper.cpp` lazily creating ~80–150 `VkPipeline` objects on the
first `state.full(...)` call. `WhisperLocal::prewarm()`
(`crates/fono-stt/src/whisper_local.rs:245-318`) was only mmapping
the model and constructing a `WhisperContext`; it never created a
`WhisperState` or ran inference, so all the pipeline work landed on
the user's first hotkey press.

`prewarm()` now additionally runs a 1 s silent decode through a
fresh `WhisperState` on GPU-accelerated builds (gated by a new
`GPU_PREWARM` constant covering `accel-vulkan` / `accel-cuda` /
`accel-metal` / `accel-hipblas` / `accel-coreml`). The dummy decode
runs on the same `tokio::task::spawn_blocking` thread that already
loads the model, holds the prewarm mutex briefly, and treats any
failure as best-effort (logged at `debug!` so a hypothetical driver
bug can't block real dictation). CPU-only builds skip the silent
decode entirely.

Bench result on `ai` after the change:

| backend | batch | stream | ttff | speedup vs CPU |
|---|---:|---:|---:|---|
| CPU | 68.05 s | 198.02 s | 6.16 s | (baseline) |
| Vulkan (RTX 4090) | **2.27 s** | **3.98 s** | **0.12 s** | **29.98× / 49.75× / 51.33×** |

The Vulkan first-fixture `batch_s` dropped from 7.8 s to 1.0 s
(7.8× drop on the user-visible cost), and the overall Vulkan batch
total dropped from 9.11 s to 2.27 s (4.0×). All ten fixtures still
PASS the tier-1 equivalence threshold on both backends.

Follow-up tracked but not landed in this slice: same prewarm pattern
for `fono-llm/src/llama_local.rs::prewarm` so the first LLM cleanup
call after session start doesn't pay the equivalent pipeline-compile
cost on Vulkan-accelerated hosts.

## 2026-05-02 — Release v0.5.0

Tagged v0.5.0. Headline changes:

- **Hardware acceleration on tap** (the big one). Two release
  binaries side-by-side: `fono-vX.Y.Z-x86_64` (compact CPU-only,
  ~18 MB) and `fono-gpu-vX.Y.Z-x86_64` (Vulkan-enabled, ~60 MB).
  `fono update` probes Vulkan and auto-picks the matching asset on
  every invocation. CPU build on a Vulkan-capable host gets switched
  to the GPU build on its next update; if the host later loses its
  GPU it switches back. Tray gains a single discoverable
  "Update for GPU acceleration" entry on a CPU build with a usable
  Vulkan host. `fono doctor` reports the running variant + the
  live Vulkan device list. Three slices of
  `plans/2026-05-02-fono-cpu-gpu-variants-v1.md` landed (PRs #3, #4,
  #5).
- **`fono install` / `fono uninstall` self-installer** (PR with
  commit `1d80ace`). Run `sudo fono install` to drop the binary at
  `/usr/local/bin/fono` plus desktop entry / autostart / icon /
  shell completions; `--server` writes a hardened systemd unit
  instead. `sudo fono uninstall` reverses it cleanly.
- **Bench tooling**: `tests/bench.sh` auto-discovers models and
  runs CPU-vs-GPU comparison (commit `da67a07`).

Release notes: `CHANGELOG.md` `[0.5.0]`.

## 2026-05-02 — CPU/GPU variants slice 3: auto-variant update + tray entry

Slice 3 of `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`. The
plan was simplified mid-implementation: instead of a wizard prompt
+ config flag + `--variant cpu/gpu` CLI + tray menu, we landed
**one decision in one place**.

`fono update` now probes Vulkan and auto-picks the right release
asset:

- CPU build on no-GPU host → `fono-vX.Y.Z-x86_64` (same variant,
  version bump only).
- CPU build on GPU+Vulkan host → `fono-gpu-vX.Y.Z-x86_64` (cross-
  variant switch, possibly + version bump).
- GPU build on a host that lost Vulkan capability → switches back
  to CPU on next update.

`fono_update::check` now takes the running binary's current asset
prefix and treats a prefix mismatch as "update available" even
when the version hasn't changed. That's how the tray's new
"Update for GPU acceleration" item lights up at the same version.

The tray entry is the single discoverable surface: shown only on a
CPU-variant build with a usable Vulkan host. Click → reuses the
existing `apply_update_via_tray` handler (which now picks the
right asset automatically). New `TrayAction::UpdateForGpuAcceleration`
+ `GpuUpgradeProvider` callback type in `fono-tray`.

`vulkan_probe` was moved from `crates/fono/src/` into
`crates/fono-core/src/` behind a `vulkan-probe` cargo feature
(off by default), so `fono` and `fono-update` both opt in without
forcing `ash` onto every other workspace consumer.

No wizard prompt. No `[update] gpu_upgrade_prompted` config flag.
No `--variant` CLI flag. Per the user-feedback memory
`feedback_centralize_decisions`: prefer one automatic decision in
one place over scattered prompts/toggles.

Files touched: `Cargo.toml` (ash workspace dep moved),
`crates/fono-core/{Cargo.toml, src/lib.rs, src/vulkan_probe.rs}` (probe
module + feature), `crates/fono-update/{Cargo.toml, src/lib.rs}`
(variant-aware check + asset selection), `crates/fono-tray/src/lib.rs`
(new action + provider + menu entry), `crates/fono/{Cargo.toml,
src/lib.rs, src/daemon.rs, src/cli.rs, src/doctor.rs}` (call sites
+ daemon plumbing; vulkan_probe module deleted).

`fono doctor` and the daemon log line continue to work, now sourcing
the probe from `fono_core::vulkan_probe` instead of
`crate::vulkan_probe`.

Verification: `cargo fmt`, `cargo clippy --workspace --all-targets
-- -D warnings`, `cargo test --workspace --all-targets` all green
locally. Smoke-tested `fono doctor` shows the "Compute backends"
section unchanged from slice 2; the new tray entry visibility was
not exercised live (no tray-host running on this Proxmox box) but
the daemon path compiles and dispatches correctly.

## 2026-05-02 — CPU/GPU variants slice 2: runtime Vulkan probe + doctor surfacing

Per slice 2 of `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`,
`fono doctor` now runtime-probes the host's Vulkan loader and reports
what it sees in a "Compute backends" section. On a CPU-variant binary
where a Vulkan-capable GPU is detected, doctor surfaces an upgrade
hint pointing at the `fono-gpu-vX.Y.Z-x86_64` release asset.

The probe lives in `crates/fono/src/vulkan_probe.rs` and uses `ash`'s
runtime-loaded bindings (`ash::Entry::load()` →
`dlopen("libvulkan.so.1")` via libloading) — so the CPU variant
keeps its strict 4-NEEDED-entry allowlist. Three states reported:

- `Vulkan: detected (<device names>)` — loader + ≥ 1 device.
- `Vulkan: loader present but no physical devices` — driver missing.
- `Vulkan: not available (<reason>)` — libvulkan not loadable.

The probe runs once at daemon startup (logged at info), and on every
`fono doctor` invocation. Cost: ~50–300 ms on Mesa, ~10 ms when the
loader is absent. No allocation of GPU memory; instance is destroyed
before the function returns.

Surfaced in the daemon startup log as the line `vulkan probe : ...`
right after `hw accel`.

**Slice 3 is next** — actual upgrade UX:

- `fono update --variant gpu` (and `--variant cpu` for the reverse).
- Tray menu: `SwitchToGpuBuild` / `SwitchToCpuBuild` actions.
- First-run wizard prompt when Vulkan is detected on the CPU variant.
- `[update] gpu_upgrade_prompted` config flag for "never ask again".

## 2026-05-02 — Two-variant release (CPU default + GPU optional), slice 1

Releases will now ship two binaries side-by-side: the default
`fono-vX.Y.Z-x86_64` (compact ~18 MB CPU-only build) and
`fono-gpu-vX.Y.Z-x86_64` (Vulkan-enabled ~60 MB build). Both built
from the same source; only the `accel-vulkan` cargo feature differs.

This was prompted by a local measurement: enabling `accel-vulkan`
in a single binary adds **+42 MB** (not the ~2 MB the initial
investigation estimated), driven by 150+ precompiled SPIR-V shaders
and ggml-vulkan C++ in `.text`. A single ~60 MB binary defeats the
"compact, runs on every Linux distro" promise; a single ~18 MB
binary defeats the "GPU acceleration available" promise. Two
variants is the honest answer.

This entry covers **slice 1** of
`plans/2026-05-02-fono-cpu-gpu-variants-v1.md`:

- `release.yml` build matrix expanded with `variant ∈ {cpu, gpu}`,
  feature/asset-prefix/cache-key cascading. CPU keeps full distro
  packaging (.deb / .pkg.tar.zst / .txz / .lzm); GPU ships raw
  binary + .sha256 only at this release.
- `ci.yml` size-budget job split into a `(cpu, gpu)` matrix. CPU
  keeps the strict 4-NEEDED-entry / 20 MiB gate. GPU adds
  `libvulkan.so.1` to the allowlist and a 64 MiB ceiling.
- New `crates/fono/src/variant.rs` with a build-time `VARIANT`
  constant gated by `accel-vulkan`. Surfaced in `fono doctor` and
  the daemon startup log.
- ADR 0022 second amendment, ROADMAP "Up next" entry, README
  install-table row, CHANGELOG `[Unreleased]` Added entries.

Slices 2 and 3 follow:

- **Slice 2** — Vulkan runtime detection (via `ash` dlopen),
  `fono doctor` "Compute backends" section.
- **Slice 3** — upgrade UX in three surfaces: first-run wizard
  prompt, tray menu item, `fono update --variant gpu` CLI.

## 2026-05-02 — `fono install` / `fono uninstall` self-installer

Release-asset users can now run `sudo ./fono-vX.Y.Z-x86_64 install`
to get a fully-integrated system install without writing a distro
package. Two modes via a single flag:

- **Desktop (default):** `/usr/local/bin/fono`, menu desktop entry,
  `/etc/xdg/autostart/fono.desktop` (auto-starts daemon on next
  graphical login), hicolor SVG icon, three shell completions.
- **Server (`--server`):** `/usr/local/bin/fono`, hardened
  `/lib/systemd/system/fono.service` running as a dedicated `fono`
  system user (created via `useradd --system`), enabled-and-started
  immediately, plus completions.

`--dry-run` previews actions without filesystem changes on either
mode. `sudo fono uninstall` reads `/usr/local/share/fono/install_marker.toml`
and removes exactly the recorded files; user config and history are
never touched. Re-running `install` against a different mode is
rejected with "run `fono uninstall` first".

Implementation: `crates/fono/src/install.rs` (~700 LOC, 5 unit
tests). Embedded assets at `packaging/assets/{fono.desktop,fono.svg,fono.service}`
(single source of truth for the embedded copy and any future
distro-recipe consumer). `fono doctor` gained an Install section.

ADR: `docs/decisions/0023-self-installer.md`. Plan:
`plans/2026-05-02-fono-install-subcommand-v3.md`. CHANGELOG entry
under `[Unreleased]`.

## 2026-05-02 — Release v0.4.0

Tagged v0.4.0. Headline changes:

- **Wyoming Home Assistant wire compliance** + **discovered-server tray
  UX** (~600 LOC; PR #1). Frame format aligned with upstream Python
  Wyoming, `info.asr` array shape, queued-transcribe HA flow, multi-
  channel PCM decode, mDNS auto-addresses, tray submenu for picking a
  remote Wyoming server with hot-reload.
- **CI size-budget gate** pivoted from static-musl to glibc-dynamic +
  NEEDED allowlist (~20 MiB budget; measured at release: 18.08 MB).
- **Artefact-producing runners** pinned to ubuntu-22.04 (glibc 2.35)
  so the binary runs on Ubuntu 22.04+, Debian 12+, Fedora 36+.
- **CI cache key** suffixed with the runner image to prevent
  cross-glibc contamination of cached build-script binaries.
- **CI job names** rewritten for UI clarity (Build & test, Binary
  size & deps audit, License & advisory audit, Release binary).
- **Phase 2.4 (musl ship)** formally deferred. Resurrection path
  documented in ADR 0022 amendment + CHANGELOG.

Release notes: `CHANGELOG.md` `[0.4.0]`.

## 2026-05-02 — Pin build runners to ubuntu-22.04 for older-distro glibc compat

`size-budget` (`.github/workflows/ci.yml`) and the release build matrix
(`.github/workflows/release.yml`) now both pin `runs-on:` to
**`ubuntu-22.04`** (glibc 2.35) instead of `ubuntu-latest` (24.04 →
glibc 2.39). The shipped binary's `GLIBC_2.X` symbol versions are
stamped at link time by the build host's glibc; staying on the older
image keeps the binary compatible with Ubuntu 22.04+, Debian 12+,
Fedora 36+, and any host with glibc ≥ 2.35. The previous
`ubuntu-latest` floor would have silently excluded ~3 years of
supported distros.

The `test` job in `ci.yml` stays on `ubuntu-latest` so we still get
newer-environment regression coverage. Only artefact-producing jobs
need the older glibc pin.

ADR 0022's "Glibc symbol-version surface" note (formerly a
follow-up TODO) is updated to reflect the pinned state.

## 2026-05-02 — CI size-budget pivots from static-musl to glibc-dynamic + NEEDED allowlist

The `size-budget` CI job no longer tries to build a fully-static
`x86_64-unknown-linux-musl` artefact. Eleven post-v0.3.7 commits
(`901e41d..29cc577`, excluding `01e9411`'s unrelated Node 24 bump)
chased a chain of toolchain breakage in `messense/rust-musl-cross`'s
`libgomp.a` — non-PIC archive (vs `-static-pie`), glibc-only `memalign`
and `secure_getenv`, plus link-order-dependent POSIX symbols
(`gethostname`, `strcasecmp`, `getloadavg`) — and abandoned. Each shim
exposed the next layer; the libgomp.a in available musl-cross images
is unfit for purpose without a custom build.

The replacement gate builds `x86_64-unknown-linux-gnu` `release-slim`
on `ubuntu-latest` (mirroring `release.yml`) and asserts:

1. Size ≤ 20 MiB (20 971 520 bytes); measured today: **18 957 120 bytes
   (≈ 18.08 MB)**, ~2 MB headroom.
2. `NEEDED` set is exactly `libc.so.6 libm.so.6 libgcc_s.so.1
   ld-linux-x86-64.so.2`. Modern glibc (≥ 2.34) merges
   `libpthread/librt/libdl` into `libc.so.6` so they don't appear
   separately. Anything else (libgtk, libstdc++, libgomp, libayatana,
   libxdo, libasound, libxkbcommon, libwayland-*) fails the gate.

The dedup invariant (single ggml copy) stays enforced at link time by
`--allow-multiple-definition` in `.cargo/config.toml` (ADR 0018);
release-slim's `strip = "symbols"` removes runtime symbol info, so a
post-strip `nm` check is not possible. Breaking dedup yields
multiple-definition link errors, not silent passes.

Phase 2.4 of `plans/2026-04-30-fono-single-binary-size-v1.md` (musl
ship) is **deferred**. Resurrection path: switch the `llama-cpp-2`
fork to llvm-openmp (libomp is PIC-friendly) **or** pin a PIC-built
`libgomp.a` from GCC sources in our own minimal cross image.

Files: `.github/workflows/ci.yml` (size-budget job rewritten to
glibc/native, with positive NEEDED allowlist), `.cargo/config.toml`
(musl rustflags block deleted), `crates/fono/src/main.rs` (`memalign`
and `secure_getenv` shims deleted),
`plans/2026-04-30-fono-single-binary-size-v1.md` (Tasks 2.3/2.4,
verification criteria, outcome table updated),
`docs/decisions/0022-binary-size-budget.md` (status amended;
Decision/Verification/Trade-offs reframed for glibc-dynamic +
allowlist).

Verification: local `cargo build -p fono --profile release-slim
--target x86_64-unknown-linux-gnu` produced an 18 957 120-byte ELF
with the expected NEEDED set. The gate's bash logic was exercised
locally in both pass (full allowlist) and fail (deliberately tightened
allowlist) paths against that binary.

## 2026-05-01 — Alpine size-budget preserves Rust image PATH

The Alpine-backed size-budget command no longer starts a login shell that can
reset the Docker image PATH before invoking `rustc`. The job now passes the Rust
image toolchain path explicitly and uses a non-login shell, so `rustc`, `cargo`,
`cargo fmt`, and `cargo clippy` resolve before the size-budget script runs.

Verification: `.github/workflows/ci.yml` YAML parsing, extracted shell syntax
validation, and `git diff --check` pass on the current Linux host. A local Docker
smoke test could not run because the Docker daemon is unavailable here.

## 2026-05-01 — GitHub Actions now target Node 24

The CI and Release workflows no longer rely on JavaScript actions that run on the
Node 20 runtime. Cache, upload-artifact, download-artifact, and release-publishing
actions were advanced to their Node 24 majors while checkout was already on the
Node 24-compatible major.

Verification: workflow YAML parsing and `git diff --check` pass on the current
Linux host.

## 2026-05-01 — Alpine size-budget no longer assumes rustup

The first Alpine-backed size-budget run failed before the build because the
`rust:1.88-alpine` image provides the Rust toolchain directly, but not `rustup`.
The job no longer tries to add components with `rustup`; it prints `rustc`,
`cargo`, `cargo fmt`, and `cargo clippy` versions before running the size-budget
script so missing tools fail with a direct diagnostic.

Verification: `git diff --check`, `.github/workflows/ci.yml` YAML parsing, and
shell syntax validation for the Alpine size-budget step pass on the current Linux
host.

## 2026-05-01 — CI musl size-budget now runs in Alpine

The third `main` CI attempt failed in the install-step smoke test because the
Ubuntu host `libstdc++` headers are glibc-configured and are not safe to combine
with `musl-gcc.specs`; `<array>` pulled in glibc-only preprocessor checks before
the actual size-budget build could start. The size-budget job now runs the gate
inside `rust:1.88-alpine`, installing Alpine's native musl C/C++ build toolchain
so C, C++, libstdc++, and the Rust musl target all agree on musl from the start.

Verification: `git diff --check`, `.github/workflows/ci.yml` YAML parsing, and
shell syntax validation for the Docker-backed size-budget step pass on the
current Linux host. A local Docker smoke test could not run because the Docker
client is installed but the daemon is not running in this environment.

## 2026-05-01 — CI musl C++ wrapper now restores standard headers

The follow-up `main` CI run for the v0.3.7 release fix advanced past CMake's
missing `x86_64-linux-musl-g++` probe, then failed while compiling whisper.cpp
because the musl specs file removes the default C++ header search path and
`ggml.cpp` could not include `<array>`. The CI wrapper now keeps the musl specs
file and explicitly restores the host libstdc++ include directories, with an
install-step smoke compile for `<array>` so this failure is caught before the
full size-budget build.

Verification: `git diff --check`, `.github/workflows/ci.yml` YAML parsing, and
shell syntax validation for the patched musl install step pass on the current
Linux host. Full musl size-budget validation remains CI-only here because this
host lacks the musl Rust standard library and musl C/C++ toolchain.

## 2026-05-01 — Live fallback stop now completes batch transcription

When live dictation is enabled but the active STT backend is batch-only, Fono starts
the normal batch capture path as a fallback. The daemon still receives the matching
live-stop event, so the interactive stop handler now checks for and stops that batch
fallback capture instead of immediately marking processing done. This fixes the
"falling back to batch path" case where recording stopped but no transcript was
injected.

The Wyoming server now advertises its ASR program/attribution as `Fono`, matching the
product name, and logs each remote transcription request at INFO level when processing
starts and when the backend returns.

Verification: `cargo fmt --all -- --check`, `cargo test -p fono-net --test
wyoming_server_round_trip`, `cargo test -p fono-net
wyoming::server::tests::build_info_advertises_models`, `cargo check -p fono
--features interactive`, and `git diff --check` pass on the current Linux host.

## 2026-05-01 — Wyoming ASR flow now matches Home Assistant event ordering

Home Assistant's Wyoming ASR client sends `transcribe` first to select the
language/model, then streams `audio-start` / `audio-chunk` events, and expects the
`transcript` response when `audio-stop` arrives. Fono previously treated
`transcribe` as the terminal event, so it invoked Whisper immediately with zero
collected samples and closed the connection with `Input sample buffer was empty`.

The Wyoming server now queues an early `transcribe` request until `audio-stop`,
continues to support Fono's existing audio-first flow, accepts audio chunks even
when a client omits `audio-start`, and decodes int16 LE mono/stereo payloads using
the format fields from each `audio-chunk`. The probe's optional ASR flow now sends
the Home Assistant ordering so it catches this compatibility issue.

Verification: `cargo fmt --all -- --check`, `python3 -m py_compile
tests/wyoming_protocol_probe.py`, `cargo test -p fono-net --test
wyoming_server_round_trip`, `cargo test -p fono-net-codec -p fono-net -p fono-stt
wyoming`, and `cargo check -p fono-net-codec -p fono-net -p fono-stt` pass on the
current Linux host. The deployed server at `192.168.0.79:10300` still times out on
the updated Home Assistant-style probe until rebuilt/restarted with this patch.

## 2026-05-01 — Wyoming describe/info is now Home Assistant-compatible

Home Assistant's Wyoming loader sends `describe`, waits for an `info` event, and
parses `info.asr`, `info.tts`, `info.wake`, `info.handle`, `info.intent`,
`info.mic`, and `info.snd` as service arrays. Fono's Wyoming server previously
returned `asr` as a single object and omitted the empty service families, which
made Home Assistant's `Info.from_event` reject the response. The codec now writes
canonical Wyoming frames with `version` and `data_length` data blocks, and the
server now advertises ASR as an installed program with models under
`info.asr[]`, plus empty arrays for the unsupported service families.

A new `tests/wyoming_protocol_probe.py` script sends the same describe/info
handshake and validates the returned info shape against Home Assistant's schema.
The currently deployed server on `192.168.0.79:10300` still reports the old shape
until rebuilt/restarted, and the probe correctly flags that mismatch.

Verification: `cargo fmt --all -- --check`, `python3 -m py_compile
tests/wyoming_protocol_probe.py`, `cargo test -p fono-net-codec -p fono-net -p
fono-stt wyoming`, `cargo test -p fono-net --test wyoming_server_round_trip`,
`cargo test -p fono-stt --test wyoming_round_trip`, and `cargo check -p
fono-net-codec -p fono-net -p fono-stt` pass on the current Linux host.

## 2026-05-01 — Tray now exposes remote mDNS Wyoming servers

The tray backend now appends live mDNS-discovered Wyoming servers to the existing
"STT backend" submenu, using the same discovery registry as `fono discover`. The
daemon filters out its own local Wyoming advertisement before passing labels to
the tray, so the menu contains only remote, actionable servers. Selecting a
discovered server writes `[stt.wyoming].uri`, switches `[stt].backend` to
`wyoming`, and hot-reloads the orchestrator.

Verification: `cargo fmt --all -- --check`, `cargo check -p fono-tray --features
tray-backend`, `cargo check -p fono`, `cargo test -p fono
daemon::tests::tray_wyoming_peers_filter_local_fullname`, `cargo build -p fono`,
and `git diff --check` pass on the current Linux host.

## 2026-05-01 — mDNS Wyoming advertisements now publish host addresses

Manual Wyoming connections to the remote `ai` host worked, but automatic
mDNS discovery resolved the Fono advertisement with no A/AAAA records. The
advertiser now calls `mdns-sd` address auto-detection when no explicit publish
addresses are configured, so `_wyoming._tcp.local.` registrations include the
current non-loopback host addresses and stay updated as interfaces change.

Verification: `cargo test -p fono-net discovery::advertiser` and `cargo build
-p fono` pass. A patched debug binary copied to `ai` advertised
`fono-ai-mdns-fixed._wyoming._tcp.local.` on port 10309; local
`avahi-browse -rt _wyoming._tcp` resolved both IPv4 and IPv6 addresses, and
`./target/debug/fono discover --json` listed the remote Wyoming peer.

## 2026-04-30 — CI musl size-budget toolchain fix

The v0.3.7 Release workflow published successfully, but the `main` CI run failed
in the `size-budget (musl, release-slim)` job because Ubuntu's `musl-tools`
package provides `x86_64-linux-musl-gcc` but no matching
`x86_64-linux-musl-g++` executable. The CI musl dependency setup now installs a
small wrapper at `/usr/local/bin/x86_64-linux-musl-g++` so whisper.cpp's CMake
compiler probe can resolve the C++ compiler name it requests.

Verification: `git diff --check`, workflow YAML parsing via Python `yaml`, and
`cargo fmt --all -- --check` pass on the current Linux host. Full musl
size-budget validation remains CI-only on this host because the local NimbleX
environment still lacks the musl Rust standard library and musl C toolchain.

## 2026-04-30 — v0.3.7 release prep

Prepared the v0.3.7 release metadata: workspace and lockfile versions are now
0.3.7, `CHANGELOG.md` has a `## [0.3.7] — 2026-04-30` section, and
`ROADMAP.md` lists the Wyoming + mDNS network foundations and binary-size prep
as recently shipped.

Verification: `cargo fmt --all -- --check`, `cargo check -p fono`,
`./tests/check.sh`, and the Rust-source SPDX header audit pass on the current
Linux host. `./tests/check.sh --size-budget --no-test` passes the build,
dependency, format, and clippy portions, then stops at the size-budget
preflight because this host lacks the `x86_64-unknown-linux-musl` Rust standard
library under `/usr`; CI/release runners remain responsible for the canonical
musl artefact gate.

## 2026-04-30 — Tray left-click now shows status under snixembed

The SNI tray backend now handles `Activate` by dispatching the existing
`ShowStatus` tray action. This gives snixembed and other hosts that call
`org.kde.StatusNotifierItem.Activate` a useful left-click path, while the normal
right-click D-Bus menu path remains unchanged.

The libdbusmenu warning seen under snixembed was traced to the upstream `ksni`
D-Bus menu layout builder adding `children-display = "submenu"` to the root
layout item. The root is the menu container rather than a visible submenu item,
so libdbusmenu-gtk warns even though Fono's actual submenu items are populated.

Verification: `cargo fmt --check`, `cargo check -p fono-tray --features
tray-backend`, `cargo test -p fono-tray --lib`, and `cargo clippy -p fono-tray
--features tray-backend -- -D warnings` pass on the current Linux host.

## 2026-04-30 — Discovery and bind config cleanup

Removed the unreleased `[network].autodiscover`, `[network].advertise`, and
`[server.wyoming].allow_public` config fields entirely. Discovery browsing is
always on while the daemon is running, Wyoming advertising is automatic only
when `[server.wyoming].enabled = true`, and `[server.wyoming].bind` is now the
sole network exposure control. The network plan and unreleased changelog were
updated to match the simplified config surface.

Verification: `cargo fmt --check`, `cargo test -p fono-core config::tests`,
and `cargo check -p fono` pass on the current Linux host.

## 2026-04-30 — Missing tray watcher now raises a desktop notification

When the SNI tray backend fails because the session bus has no
`org.kde.StatusNotifierWatcher`, Fono now sends a critical desktop
notification titled "Fono tray unavailable" with a 20-second requested
expiry. The notification now uses a short body that fits typical notification
popups while telling the user to start a tray host such as Waybar tray, KDE
tray, xfce4-panel, or snixembed before restarting Fono. The existing warning
log keeps the longer explanation for terminal/service diagnostics.

Verification: `cargo fmt --check`, `cargo test -p fono-tray --lib`, `cargo
check -p fono-tray --features tray-backend`, `cargo clippy -p fono-tray
--features tray-backend -- -D warnings`, and `cargo check -p fono --features
tray,interactive` pass on the current Linux host.

## 2026-04-30 — mDNS discovery is always-on

Discovery browsing is not controlled by a config toggle, and server
advertising is not controlled by a config toggle. The daemon now always starts
the mDNS browser when it can create the mDNS service daemon, and advertises
Wyoming automatically whenever `[server.wyoming].enabled = true`.
`[network].instance_name` remains as the optional friendly-name override.

Verification: `cargo fmt --check`, `cargo test -p fono-core config::tests`,
and `cargo check -p fono` pass on the current Linux host.

## 2026-04-30 — Tray watcher absence now degrades cleanly

NimbleX/i3-style sessions without an SNI StatusNotifierWatcher now get an
actionable tray warning instead of the raw `ksni::Tray::spawn` error. Fono
continues hotkeys, dictation, and overlay operation without a tray icon, and
points the user at a tray host/watcher such as KDE Plasma's tray, waybar tray,
xfce4-panel, or snixembed.

Overlay startup now reports early winit event-loop failures back to the caller
instead of returning a handle whose wake proxy is missing. This makes overlay
startup failures visible at daemon startup rather than silently dropping later
`set_state` / `update_text` commands.

Verification: `cargo fmt --check`, `cargo test -p fono-tray --lib`, `cargo test
-p fono-overlay --lib`, `cargo check -p fono-tray --features tray-backend`,
`cargo check -p fono-overlay --features real-window`, `cargo clippy -p
fono-tray --features tray-backend -- -D warnings`, `cargo clippy -p
fono-overlay --features real-window -- -D warnings`, and `cargo check -p fono
--features tray,interactive` pass on the current Linux host. A broader `cargo
test -p fono-tray -p fono-overlay` was also attempted but this host cannot run
the overlay doctest because `rustdoc` is unavailable in `PATH`.

## 2026-04-30 — Default Linux audio no longer links ALSA/libasound

Moved Linux default microphone capture off `cpal` and onto a process-backed
PulseAudio/PipeWire path (`parec` raw mono s16le at the target sample rate),
so the default Fono binary no longer pulls `cpal`, `alsa`, or `alsa-sys` into
the dependency graph. `cpal` remains available behind `fono-audio`'s
`cpal-backend` feature for macOS, Windows, and explicit bare-ALSA Linux builds.

Release/CI guardrails now reject regressions: `tests/check.sh` fails if the
default Linux dependency tree includes `cpal`, `alsa`, or `alsa-sys`, the
musl size-budget gate already requires zero `NEEDED` entries, and the release
workflow rejects Linux artifacts with `libasound.so` or `libgomp.so` in
`NEEDED`. CI/release package installs no longer install `libasound2-dev`.

Verification: `cargo check -p fono`, `cargo check -p fono-audio`,
`cargo check -p fono-audio --features cpal-backend`, `cargo test -p
fono-audio --lib`, `cargo test -p fono-audio --lib --features
cpal-backend`, `cargo clippy -p fono-audio --all-targets -- -D warnings`,
`cargo fmt --all -- --check`, and `./tests/check.sh --quick --no-test` all
pass on the current Linux host. `./tests/check.sh --size-budget --no-test`
passes build/clippy/dependency checks, then stops at the preflight because this
host still lacks the `x86_64-unknown-linux-musl` Rust standard library under
`/usr`.

## 2026-04-30 — Release GNU no longer links libgomp/libstdc++ dynamically

User reported that `cargo build --release -p fono` still produced a GNU
binary with `libgomp.so.1` in `NEEDED`, and that the musl build does not
start locally. Root cause: late `.cargo/config.toml` `link-arg` flags do
not override `cargo:rustc-link-lib=gomp` / `dylib=stdc++` emitted by
`llama-cpp-sys-2`'s build script. Fixed on fork branch
`bogdanr/llama-cpp-rs:feature/static-runtime-linkage` (commit
`e9f5cc12`) by adding `static-openmp` and Linux-capable `static-stdcxx`
features that make the sys crate emit `static=gomp` / `static=stdc++` at
the right point in the link line, including compiler-discovered archive
search paths.

Fono now pins `[patch.crates-io]` to that branch and enables
`llama-cpp-2` features `openmp`, `static-openmp`, and `static-stdcxx`.
Verification: `cargo build --release -p fono` succeeds, and `ldd
target/release/fono` / `readelf -d` show no `libgomp.so.1` and no
`libstdc++.so.6`. Remaining GNU `NEEDED`: `libasound.so.2`,
`libgcc_s.so.1`, `libm.so.6`, `libc.so.6`, `ld-linux-x86-64.so.2`.
Those are expected until the canonical musl artefact builds.

Musl recheck still fails before any C/C++ linkage with Rust error E0463:
this NimbleX host has distro `rustc`/`cargo` but no `rustup`, no
`x86_64-unknown-linux-musl` Rust standard library, and no musl C/C++
cross compiler in `PATH`. `tests/check.sh --size-budget` now detects the
missing Rust std cleanly on non-rustup hosts instead of assuming `rustup`
exists. CI musl deps were also cleaned up to drop obsolete GTK packages.

## 2026-04-30 — Task 2.1 complete: GTK gone, pure-Rust SNI tray

Phase 2 Task 2.1 of `plans/2026-04-30-fono-single-binary-size-v1.md`.
Replaced `tray-icon`'s libappindicator + GTK3 backend with a
pure-Rust StatusNotifierItem (SNI) implementation via `ksni 0.3`
(Unlicense, public-domain) talking `zbus`. Confirmed via
`cargo tree -p fono --features tray`: `tray-icon`, `gtk`, `gdk`,
`cairo-rs`, `pango`, `gdk-pixbuf`, `glib`, and every `*-sys` shim
(`gtk-sys`, `gdk-sys`, `pango-sys`, `glib-sys`, `gobject-sys`,
`cairo-sys-rs`, `gdk-pixbuf-sys`) have left the dep tree. The new
`fono-tray` keeps the public API identical (`Tray::set_state`,
`spawn`, the four `*Provider` aliases, `TrayAction`); the daemon's
spawn site at `crates/fono/src/daemon.rs:328` was unchanged.

Internally the backend now spawns a tokio task instead of a
dedicated GTK thread, owns a `KsniTray` model implementing
`ksni::Tray`, and pushes provider snapshots into the model every
two seconds via `Handle::update`. Menu rebuild is declarative —
`menu()` returns the current `Vec<MenuItem<KsniTray>>` and ksni
diffs against the last snapshot, so we no longer maintain
pre-allocated slot arrays + ID maps. Icon is still the in-code
ARGB32 circle (byte order corrected for SNI: `[A, R, G, B]` not
`[R, G, B, A]`).

`cargo check -p fono --features tray` clean. `cargo clippy -p
fono-tray --features tray-backend` clean. The five
`graphical_session` unit tests still pass (no behaviour change at
the daemon's runtime gate).

`deny.toml` updated to allow the `bogdanr/llama-cpp-rs.git` git
source consumed via `[patch.crates-io]`.

Task 1.2 (source-level shared ggml on a second `bogdanr/llama-cpp-rs`
branch) remains the next blocker.

## 2026-04-30 — Task 1.1 wired into Fono via fork

Upstream PR submitted: [utilityai/llama-cpp-rs#1015](https://github.com/utilityai/llama-cpp-rs/pull/1015).
Fork branch `feature/optional-common-build` on
`github.com/bogdanr/llama-cpp-rs` is now consumed via
`[patch.crates-io]` in `Cargo.toml`. Fono's existing
`default-features = false, features = ["openmp"]` declaration on
`llama-cpp-2` means we automatically opt out of the new `common`
feature, so building Fono today drops `libcommon.a` (~14 MB) and the
`wrapper_common`/`wrapper_oai` shim archives (~10 MB) from the link
line — a ~24 MB raw archive saving, expected to land as ~6–10 MB of
`.text` after LTO + `--gc-sections`. `cargo check -p fono` clean. Task
1.1 closed; Task 1.2 (source-level shared ggml) is the next blocker.

## 2026-04-30 — Binary-size pass kickoff: single 20 MiB static-musl ELF

Plan: `plans/2026-04-30-fono-single-binary-size-v1.md`. ADR:
`docs/decisions/0022-binary-size-budget.md` (supersedes 0018 once Task
1.2 lands).

User feedback: the release artefact had drifted to ~25–30 MiB stripped
and was dynamically linked to GTK 3 + glib + cairo + libstdc++ + libgomp
+ glibc — both contradicting the v1 design plan's "single static-musl
ELF, `ldd` not a dynamic executable" promise. Target rolled back to
**≤ 20 MiB with all features**, **one binary** (no
desktop/server/cloud-only flavours; graphical surfaces runtime-gated on
`DISPLAY`/`WAYLAND_DISPLAY`), and **zero `NEEDED` shared libraries**.

What landed this session (prep work; the structural wins are next):

- `Cargo.toml` — removed unused workspace deps (`ort`, `rodio`,
  `swayipc`, `hyprland`). Confirmed zero `use` sites; cosmetic cleanup.
- `.cargo/config.toml` — added dead-code link flags
  (`-Wl,--gc-sections`, `-Wl,--as-needed`) and C/C++ size flags
  (`-Os -ffunction-sections -fdata-sections`) for every supported
  target. Added `-static-libstdc++`, `-static-libgcc`,
  `-l:libgomp.a` for the musl target so the final ELF has no C++/OMP
  `NEEDED`. The legacy `--allow-multiple-definition` flag stays until
  Task 1.2 lands the source-level shared ggml; both flags now coexist
  with documented retirement path in the file's header comment.
- `crates/fono/src/daemon.rs:232-247` — tray spawn now runtime-gated
  on `DISPLAY`/`WAYLAND_DISPLAY`. Headless hosts get a `debug!` log
  line and an empty tray channel; the rest of the daemon runs
  unmodified. This is the architectural keystone of the
  one-binary-many-roles contract.
- `tests/check.sh --size-budget` — new gate that builds
  `release-slim x86_64-unknown-linux-musl` and asserts (a) binary
  size ≤ 20 971 520 bytes, (b) `ldd` reports "not a dynamic
  executable", (c) `nm` shows exactly one `ggml_init` symbol. Skips
  cleanly when the musl target isn't installed.
- `plans/2026-04-30-llama-cpp-sys-2-strip-common.patch.md` — the
  upstream / fork patch ready to apply for Task 1.1 (kill 24 MB of
  unused llama.cpp `common/`). Two application paths documented
  (vendored fork at `vendor/llama-cpp-sys-2/` vs git fork on GitHub);
  blocked on operator choice.
- ADR 0022 published; ADR 0018 will be marked Superseded once Task
  1.2 lands.

Next-session blockers (operator decisions):

1. **Task 1.1 application path.** Vendor 22 MiB of patched
   llama-cpp-sys-2 into `vendor/` (option A), or push a fork to
   GitHub and reference it via `[patch.crates-io]` git URL (option
   B)? Patch contents are the same either way.
2. **Task 2.1 tray library swap.** Replace the libappindicator/GTK
   backend of `tray-icon` with a pure-Rust `ksni` SNI implementation.
   Drops every GTK / glib / cairo `NEEDED` from the ELF; adds the
   `ksni` + `zbus` deps. Worth confirming the SNI compatibility with
   the operator's panel before swinging the change.

Once both decisions land the path forward is mechanical: apply
patch → build → measure → repeat. Phase 4 Rust trims held in reserve
in case Phases 1 + 2 + 3 don't already hit budget.

## 2026-04-29 — Slice 4: mDNS LAN autodiscovery

Plan: `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`

Slice 4 lights up the *Discovered on LAN* surface that Slices 5–7 will
build on. Concrete deliverables:

- New crate-internal module `fono_net::discovery` with `Browser`,
  `Advertiser`, `Registry`, and `DiscoveredPeer`. One passive `tokio`
  task per service type (`_wyoming._tcp.local.`, `_fono._tcp.local.`)
  feeds an `Arc<RwLock<HashMap<fullname, DiscoveredPeer>>>`; peers
  stale after 120 s and are evicted on a 15 s sweep.
- New `[network]` config block: only `instance_name` remains as a
  cosmetic override (empty ⇒ `fono-<hostname>`). Discovery browsing is
  always on while the daemon is running; advertising happens
  automatically for enabled servers.
- Daemon hooks: spawn browser + (optional) advertiser at startup; hold
  handles for the daemon's lifetime so `unregister` fires goodbye
  packets on `Drop`.
- IPC `Request::ListDiscovered` / `Response::Discovered(Vec<DiscoveredPeer>)`
  surfaces the live registry to clients.
- New CLI `fono discover [--json]` prints the registry as a fixed-width
  table or pretty JSON.
- Integration test (`crates/fono-net/tests/discovery_round_trip.rs`)
  drives two independent `ServiceDaemon` instances over loopback
  multicast and asserts the TXT round-trip lands in the registry
  within 5 s. Skips cleanly on sandboxes without multicast.
- Single new dependency: `mdns-sd 0.13` (pure-Rust, dual MIT/Apache-2.0,
  no Avahi/Bonjour FFI).

Verification: `cargo build --workspace`, `cargo test --workspace --lib`,
`cargo test -p fono-net --tests --features discovery`,
`cargo test -p fono-stt --tests`, `cargo clippy --workspace --all-targets
-- -D warnings -A dead_code`, `cargo fmt --all -- --check` all green.

Tray *Discovered on LAN* submenu population is split off into Slice 7
(tray polish) per the v2 plan; the IPC contract is in place so the
tray can read from a single source when that lands.

Next up: **Slice 5 — Fono-native protocol design + `FonoLlm`/`FonoStt`
client over WebSocket.**

## 2026-04-29 — OS-delegated microphone selection (PulseAudio-first + config purge)

Plans (combined execution):
- `plans/2026-04-29-pulseaudio-first-microphone-enumeration-v1.md`
- `plans/2026-04-29-drop-input-device-config-knob-v1.md`

Pivot triggered by two follow-up issues against the v2 recovery work
shipped earlier today: (a) the tray "Microphone" submenu was full of
ALSA plugin pseudo-devices (`pulse`, `oss`, `speex`, `default`,
`surround51`, …) and the daemon spammed `snd_pcm_dsnoop_open: unable
to open slave` because cpal's ALSA host enumerates every PCM in
`asound.conf`; (b) the user — a sample size of one but a strong one —
correctly observed that `[audio].input_device` was the wrong place to
solve "which microphone?" because every modern OS already owns that
question.

End-state: Fono no longer keeps a microphone override. The OS layer
is the source of truth.

- **PulseAudio-first enumeration.** New `crates/fono-audio/src/pulse.rs`
  shells to `pactl list sources [short]` and `pactl get-default-source`
  / `pactl set-default-source`, mirroring the `mute.rs` shell-out
  pattern. `crates/fono-audio/src/devices.rs` dispatches on
  `AudioStack::detect()`: `PulseAudio` / `PipeWire` → `pulse`,
  `Unknown` → cpal. Sink monitors are dropped at the source on the
  Pulse branch; the `is_likely_microphone` heuristic only matters on
  the cpal fallback. `InputBackend::{Pulse{pa_name}, Cpal{cpal_name}}`
  carries the backend-specific identifier through to the daemon.
- **Tray "Microphone" submenu rewired** to `pactl set-default-source`.
  Clicking a row mutates Pulse's default-source system-wide (visible
  to `pavucontrol`, GNOME / KDE settings, every other app), then
  triggers `Request::Reload` so cpal re-opens its default-source
  stream on the new endpoint. Submenu hidden on `Unknown` hosts —
  the OS owns the UI there.
- **Config purge.** `[audio].input_device` removed (no migration —
  no released users yet). `[general].language`, `[stt.local].language`
  (deprecated language scalars superseded by `languages: Vec<String>`)
  and `[general].cloud_force_primary_language` (superseded by the
  in-memory language cache) all gone. `cloud_force_primary` builder /
  struct field / dead first-pass branch removed from `GroqStt`,
  `GroqStreaming`, `OpenAiStt`. Schema migration block in
  `Config::migrate` collapsed to the version check.
- **Recovery hook reworded** — body now points at "the tray Microphone
  submenu" + `pavucontrol` / OS sound settings; the deprecated
  `fono use input "<name>"` advice is gone (test pinned).
- **CLI / wizard / doctor cleanup.** `fono use input` removed.
  Wizard microphone picker removed. `fono doctor` "Audio inputs:"
  is informational — flat list with one row marked as the OS default,
  no override-aware highlight.
- **Tray surface trimmed.** `TrayAction::ClearInputDevice` removed
  (no override to clear); the "Auto (system default)" entry stays
  as informational only (disabled, no menu-event ID bound).

Status: implementation complete. `tests/check.sh` (full matrix —
fmt, build × default + interactive, clippy × default + interactive,
test × default + interactive) green. CHANGELOG `[Unreleased]`
section reorganised into Added / Changed / Removed reflecting the
new design.

## 2026-04-29 — Empty-transcript microphone recovery (plan v2)

Plan: `plans/2026-04-29-empty-transcript-microphone-recovery-v2.md`.
Triggered by a real-world dock complaint: external dock advertises a
passive capture endpoint with no microphone wired to it, the OS elects
it as `@DEFAULT_SOURCE@`, and Fono's recordings come out flat-line
silent — Whisper hallucinates or returns empty, and the user is left
without an actionable signal.

Three layers, all stacked behind the existing `STT returned empty
text` signal at `crates/fono/src/session.rs` (no new RMS/peak detector
needed):

- **Phase 1 — empty-transcript notification.** New
  `crates/fono/src/audio_recovery.rs` fires a critical desktop toast
  when capture ≥ 5 s and the transcript is empty. Body names the
  silent device, the recording duration in seconds, and the recourse:
  "switch to '<name>'" + `fono use input` CLI when exactly one
  non-loopback alternative is detected, or "open tray Microphone
  submenu" when 2+ alternatives exist. The user's
  `[audio].input_device` override is never silently rewritten. Five
  unit tests cover the body composer.
- **Phase 2 — tray "Microphone" submenu.** Mirrors the existing STT/
  LLM/Languages pattern at `crates/fono-tray/src/lib.rs`. `Auto` plus
  a row per cpal device, active-marked. Clicking writes
  `[audio].input_device` and triggers `Request::Reload` so the next
  capture opens the new endpoint without restarting. New
  `TrayAction::SetInputDevice(u8)` / `ClearInputDevice` + a
  `MicrophonesProvider` polled every ~2 s by the tray refresh loop.
- **Phase 3 — wizard probe + doctor row + `fono use input` CLI.**
  First-run wizard offers a microphone picker only when 2+ devices
  are visible (single-mic laptops skip the prompt). `fono doctor`
  gains an "Audio inputs:" matrix with the active marker and surfaces
  "configured device not currently visible" when the override is
  unplugged. `fono use input <name>` (and `auto` to clear) is
  symmetric with `fono use stt` / `fono use llm`, with
  case-insensitive name matching.

Status: implementation complete. `tests/check.sh` (full matrix —
fmt, build × default + interactive, clippy × default + interactive,
test × default + interactive) green on the work branch. CHANGELOG
[Unreleased] section updated with the four user-visible additions;
will graduate to a versioned section at next release.

## 2026-04-28 — v0.3.0 release

Tagged v0.3.0. Bundles three user-visible fixes plus the release-time
cloud quality gate:

- LLM cleanup clarification fix (universal across all backends).
- In-memory cloud-STT language stickiness, peer-symmetric.
- Live Groq equivalence gate at release time (~0.5 % of free-tier
  daily cap per release).

Baseline `docs/bench/baseline-cloud-groq.json` bootstrapped by the
maintainer; all 10 fixtures (en × 4, ro × 3, es, fr, zh) passing.
CHANGELOG promoted from `[Unreleased]` to `[0.3.0]`. ROADMAP entries
moved into Shipped with the v0.3.0 tag and date. Workspace version
bumped to 0.3.0 in `Cargo.toml`.

## 2026-04-28 — Wave 3 Slice B1 Thread C: live Groq equivalence gate

Plan: `plans/2026-04-28-wave-3-slice-b1-thread-c-live-groq-v2.md`
(supersedes the cloud-mock approach in v1 Tasks C1–C9). User pushed
back on mocks: they catch our regressions but not upstream Groq
schema/behaviour changes, and the maintenance cost of refreshing
recordings is recurring.

What landed:

- `fono-bench equivalence --stt groq` arm at
  `crates/fono-bench/src/bin/fono-bench.rs:327-364`. Reads
  `GROQ_API_KEY` from env (exits with code 2 + bootstrap-friendly
  message when missing). Default model `whisper-large-v3-turbo`,
  overridable via `--model`. `caps.english_only = false`
  (multilingual).
- `--rate-limit-ms <ms>` flag with provider-aware default (250 ms for
  Groq, 0 otherwise). 429 detection + hard-fail with code 3 and a
  named-fixture message; never retried.
- `.github/workflows/release.yml` gains a `cloud-equivalence` job
  that runs **before** the build matrix. Auto-skipped when
  `GROQ_API_KEY` is empty (forks; bootstrap tags) or the tag carries
  the `-no-cloud-gate` suffix (operator escape hatch). `build` job
  uses `if: always() && (success || skipped)` so skip propagates
  cleanly without blocking releases that pre-date the secret.
- `.github/scripts/diff-cloud-bench.py` — exit code 1 on verdict
  divergence, exit code 2 on missing baseline (with the exact
  bootstrap command printed to stderr), exit code 0 on match.
- ADR `docs/decisions/0021-cloud-equivalence-via-real-api.md`
  records the live-vs-mock decision and the cost-shape analysis (10
  fixtures, ~110 audio-seconds, < 0.5 % of free-tier daily cap).
- `docs/dev/release-checklist.md` — bootstrap command, regenerate
  conditions, override-tag instructions, manual-rerun-after-outage
  steps.
- `CHANGELOG.md` Unreleased Added entries; `ROADMAP.md` In progress
  flipped to "bootstrap the baseline" + new Shipped entry.

Operator owes (one-time): bootstrap the baseline locally. The diff
script prints the command on the first CI run if you'd rather see it
fail-soft once before running locally:

```sh
GROQ_API_KEY=gsk_... \
  cargo run --release -p fono-bench --features equivalence -- \
  equivalence --stt groq \
    --output docs/bench/baseline-cloud-groq.json \
    --baseline --no-legend
```

Sanity-check the resulting JSON, commit it, and `v0.3.0` is ready to
tag.

Build verified: `cargo build -p fono-bench --features equivalence`
compiles clean.

## 2026-04-28 — Multi-language STT, no primary, in-memory stickiness

Plan: `plans/2026-04-28-multi-language-stt-no-primary-v3.md`. User
report: Groq's `whisper-large-v3-turbo` frequently misclassifies the
user's accented English as Russian. Wanted a fix that (a) keeps Fono
lightweight on cloud-only builds, (b) handles bilingual switchers
without breaking them, (c) avoids a "primary / secondary" UX, (d) uses
OS hints rather than asking the user.

Three earlier plan iterations explored and rejected: a local-Whisper
"language bridge" (v1, contradicts cloud users' lightweight constraint),
a cache-as-first-call-force (v2, breaks switchers — once stickiness
pins the wrong language every following call is mangled), and a
file-persisted cache (v2, marginal cold-start benefit + active harm
when stale). v3 (executed here) is **rerun-target only, in-memory
only, peer-symmetric**.

What landed:

- **`crates/fono-stt/src/lang_cache.rs`** — `LanguageCache` with
  `record` / `get` / `seed_if_empty` / `clear`, keyed by backend
  `&'static str`. Process-wide singleton via `LanguageCache::global()`
  shared across batch + streaming variants. 8 unit tests.
- **`crates/fono-core/src/locale.rs`** — POSIX → BCP-47 alpha-2 parser
  (`LANG=ro_RO.UTF-8` → `Some("ro")`, `C` / `POSIX` / empty → `None`).
  Used by both the cache bootstrap and the wizard.
- **`LanguageSelection::primary()` renamed to `fallback_hint()`**
  with a doc-comment that scope-restricts callers to single-language
  transports. The old name is kept as `#[deprecated]` for one release.
- **`groq.rs`, `openai.rs`, `groq_streaming.rs`** — first call is
  unforced; the response's detected language is checked against the
  allow-list; in-list → `cache.record()`; banned + cache populated +
  rerun knob on → re-issue with `language=<cached>`; banned + cache
  empty → accept unforced response, debug-log the skip.
- **`cloud_rerun_on_language_mismatch` default flipped to `true`** in
  `crates/fono-core/src/config.rs`. Combined with the cache, cloud STT
  self-heals from one-off Turbo misfires after the first correctly
  detected utterance per session (or immediately on cold start when OS
  locale ∈ allow-list).
- **`cloud_force_primary_language` deprecated** with a `#[deprecated]`
  attribute on the field. Removed in v0.5.
- **Wizard rework** in `crates/fono/src/wizard.rs` — checkbox-style
  "Languages you dictate in" picker with English pre-checked but
  freely uncheckable. Detected OS locale gets pre-checked alongside.
  No "primary" anywhere in the copy.
- **Tray Languages submenu** in `crates/fono-tray/src/lib.rs` —
  read-only peer-list display + "Clear language memory" action that
  emits `TrayAction::ClearLanguageMemory`; the daemon dispatcher at
  `crates/fono/src/daemon.rs:524-530` calls
  `LanguageCache::global().clear()`.
- **ADR
  [`docs/decisions/0017-cloud-stt-language-stickiness.md`](decisions/0017-cloud-stt-language-stickiness.md)**
  records the rejection rationale for local-bridge / file-persisted /
  cache-as-first-call / primary-secondary alternatives, so future
  agents don't regress to one of them.
- **`docs/providers.md`** — new "Multilingual STT and language
  stickiness" section.
- **`docs/troubleshooting.md`** — new "Cloud STT keeps detecting the
  wrong language" section explaining cache, rerun, tray clear, config
  edit recourses.
- **`CHANGELOG.md`** — `Added` / `Changed` / `Deprecated` entries.

### Switcher safety guarantee

Two configs `general.languages = ["ro", "en"]` and `["en", "ro"]`
behave identically at runtime — config order is consulted nowhere in
the request path. The cache reflects what was last heard. Trace with
`ro → en → en → ro` produces three correct transcripts and zero
reruns; the switching cost is whatever the cloud provider's
auto-detect already absorbs.

### Owed verification (no Rust toolchain in this environment)

```sh
cargo test -p fono-stt -p fono-core -p fono
cargo test --no-default-features --features tray,cloud-all -p fono-stt
cargo clippy --workspace --all-targets -- -D warnings
```

The `--no-default-features --features tray,cloud-all` invocation
verifies the slim cloud-only build still compiles without
`whisper-rs`. Once green, commit with `git commit -s` per AGENTS.md
DCO rule.

### Deferred follow-ups (not blocking the user's bug fix)

- **HTTP-mock switcher integration test for `groq.rs` and
  `openai.rs`.** `groq_streaming.rs` already has `with_request_fn`
  closure injection (Wave 3 Thread B); adding the same hook to the
  batch backends is a small but separate refactor. Cache invariants
  are already covered by the 8 unit tests in `lang_cache.rs`.
- **Desktop toast on rerun.** Currently a `tracing::warn!` line ("groq
  returned banned language … re-issuing with cached
  language=<code>"). Promoting it to a `notify-rust` toast requires
  adding `notify-rust` to `fono-stt` (it currently lives only in
  `fono`); deferred to keep `fono-stt` notification-free.
- **One-shot tray "Force next dictation as: <language>" radio.** The
  Languages submenu currently exposes the read-only checkboxes and
  "Clear language memory"; the per-utterance force radio (plan task
  8 sub-bullet) is design-complete but unwired.

## 2026-04-28 — LLM cleanup clarification-refusal fix

Bug report: a short utterance dictated through the cloud cleanup
provider sometimes injected a chat-style clarification reply
(*"It seems like you're describing a situation, but the details are
incomplete. Could you provide the full text you're referring to, so I
can better understand and assist you?"*) rather than the cleaned
transcript. Investigation showed:

- The hotkey is irrelevant. F8 (`HoldPressed`) and F9 (`TogglePressed`)
  share the same cleanup pipeline at
  `crates/fono/src/session.rs:1213-1276`. F8 just correlates because
  push-to-talk produces shorter recordings.
- The provider is irrelevant. Reproducible on Cerebras, Groq, OpenAI,
  OpenRouter, Ollama, Anthropic, **and** the local llama.cpp backend;
  the failure mode is a property of how chat-trained LLMs interpret a
  bare short utterance.

The fix is therefore universal — applied identically to every
`TextFormatter` impl. Plan:
`plans/2026-04-28-llm-cleanup-clarification-refusal-fix-v1.md`. Three
layers of defence shipped:

1. **Hardened default prompt** in
   `crates/fono-core/src/config.rs:402-415` — explicit hard rules:
   never ask for clarification, never respond with a question or
   meta-comment, return the transcript verbatim if it's short / empty /
   already clean. Same prompt for every backend.
2. **User-message framing** via new `fono_llm::traits::user_prompt`
   helper that wraps the raw transcript in `<<<` / `>>>` fences,
   referenced by all three backend impls (`OpenAiCompat` — used by
   Cerebras / Groq / OpenAI / OpenRouter / Ollama, `AnthropicLlm`,
   `LlamaLocal`).
3. **Refusal detector** `fono_llm::traits::looks_like_clarification`
   matches case-insensitive opener phrases AND a corroborating
   clarification fragment (low-false-positive heuristic). On a hit,
   the backend returns `Err`; the existing pipeline fallback at
   `crates/fono/src/session.rs:1264-1273` then injects raw STT text.
   Identical wiring in every backend.

Plus `Llm::skip_if_words_lt` default raised from `0` to `3` so
one- and two-word captures bypass the LLM entirely on every backend
(saves 150–800 ms; eliminates the failure mode at the source).

Tests: 5 new unit tests in `crates/fono-llm/src/traits.rs` for the
detector and framing helper; 2 new integration tests in
`crates/fono/tests/pipeline.rs`
(`pipeline_falls_back_to_raw_when_llm_rejects_clarification`,
`pipeline_skips_llm_for_short_capture_under_default_threshold`). The
existing `pipeline_produces_history_row_and_injects_cleaned_text` was
updated to set `skip_if_words_lt = 0` because its 2-word fixture would
otherwise trip the new skip default.

Docs: `CHANGELOG.md` Unreleased gets a `Fixed` and `Changed` bullet
(both phrased universally, naming every backend); `docs/troubleshooting.md`
gets a new "LLM responds with a question" section that explicitly
flags the failure mode as not provider-specific; `docs/providers.md`
gets a "Short-utterance handling" subsection covering all backends.

`cargo test` / `cargo clippy` were not run in this session (no rust
toolchain available in the agent environment) — the operator should
run `cargo test -p fono-llm -p fono` and
`cargo clippy --workspace --all-targets` before tagging the next release.

## 2026-04-28 — Wave 3 (Slice B1) — Threads A + B shipped; Thread C deferred

Two DCO-signed commits delivered the user-visible half of Slice B1
(driven by `plans/2026-04-28-wave-3-slice-b1-v1.md`); Thread C
(equivalence harness cloud rows) is deferred to a follow-up.

| Thread | SHA | Subject |
|---|---|---|
| A | `1e5682f` | `feat(fono-audio): cpal-callback push for live capture (Thread A / R10.x)` |
| B | `eaf46a3` | `feat(fono-stt): Groq streaming pseudo-stream backend (R4.2)` |
| C | _deferred_ | cloud-mock equivalence rows + recorded-HTTP Groq fixtures (R18.12) |

**Thread A** replaces the 30 ms-poll `RecordingBuffer` drain at
the live-dictation hot path with a true cpal-callback push pipeline:
each cpal data callback resamples to mono f32 and `try_send`s its
slice into a bounded(64) crossbeam SPSC; a dedicated `fono-live-bridge`
std::thread forwards into a tokio mpsc; the drain task pulls
straight into the streaming `Pump`. No 30 ms tick, no
`Mutex<RecordingBuffer>` middleman for live sessions. The batch
path (`run_oneshot`) still uses `RecordingBuffer` unchanged. New
unit test `forwarder_receives_every_callback_in_order` drives a
synthetic cpal stand-in 100x without a real device. Phase A4
manual latency measurement
(`live.first_partial < 400 ms` on the reference machine) cannot be
produced from a headless agent and is left for the operator to
record post-merge.

**Thread B** adds an opt-in Groq streaming STT backend implemented
as a "pseudo-stream": every 700 ms the streaming task re-POSTs the
trailing 28 s of buffered audio to Groq's existing batch endpoint,
pipes each decode through `LocalAgreement` to extract a stable
token-prefix preview, and emits a single finalize decode on
`SegmentBoundary` / `Eof`. In-flight cap = 1 (drop on overlap;
counted in `preview_skipped_count`). New ADR
`docs/decisions/0020-groq-pseudo-stream.md` captures the design
trade-offs (no Groq WebSocket today, 700 ms cadence trade-off,
~25-40× cost overhead vs single batch POST). Selectable via
`fono use stt groq` + `[interactive].enabled = true` +
`[stt.cloud].streaming = true`; the wizard prompts for the third
knob when the first two are set. `docs/providers.md` updated. The
backend takes a `GroqRequestFn` closure for production HTTPS, tests,
and the future cloud-mock equivalence path — keeping the Thread C
hook free.

**Thread C** is deferred. Scope:
1. New `--stt cloud-mock --provider groq` mode in
   `fono-bench equivalence` that swaps the real Groq client for a
   recorded-HTTP closure injected via
   `GroqStreaming::with_request_fn`.
2. Recording format (one JSON file per fixture per provider with
   `(request_audio_sha256, response_body)` exchange list) and at
   least one committed recording.
3. Second per-PR CI gate that runs the cloud-mock lane against a
   sibling baseline anchor (`docs/bench/baseline-cloud-mock-groq.json`).

Why deferred: Thread C is test infrastructure that doesn't block
users. The plumbing alone (mock client + recording format + JSON
fixture + manifest threshold extension + CI workflow change) is a
focused session in its own right; landing it half-done would leave
the equivalence report shape inconsistent. The `GroqRequestFn`
closure injection in Thread B's `groq_streaming.rs` already
preserves the hook Thread C will use, so deferring costs nothing
architecturally. Tracked as the next-session focus.

### Verification gate

`tests/check.sh` (full matrix incl. slim cloud-only build):
- `cargo fmt --check` — clean
- `cargo build` (default + default+interactive + slim + slim+interactive) — clean
- `cargo clippy` (same matrix) — clean
- `cargo test` (same matrix) — green (incl. new
  `forwarder_receives_every_callback_in_order` and
  `groq_streaming::tests::*`)

### Recommended next session

**Wave 3 Thread C** — drop in the cloud-mock equivalence lane.
Plan: `plans/2026-04-28-wave-3-slice-b1-v1.md` Thread C (Tasks
C1-C9). The closure-injection hook is already in
`crates/fono-stt/src/groq_streaming.rs::GroqStreaming::with_request_fn`;
the manifest threshold types are already typed (Wave 2). The work
is scoped to:
1. `crates/fono-bench/src/cloud_mock.rs` — recording loader +
   `SpeechToText` / `StreamingStt` impls keyed by request-WAV SHA.
2. `tests/fixtures/cloud-recordings/groq/<fixture>.json` recording
   fixture format + 1-2 committed recordings (real-key capture
   preferred; placeholder via local-Whisper output is the
   documented fallback).
3. `--stt cloud-mock --provider groq` flag wiring at
   `crates/fono-bench/src/bin/fono-bench.rs:288-333` and
   `:659-684`.
4. Sibling baseline `docs/bench/baseline-cloud-mock-groq.json` and
   second CI job in `.github/workflows/ci.yml`.

Once Thread C lands, the `v0.3.0` release tag becomes appropriate
(Slice B1 fully delivered; CHANGELOG entry + `release.yml`
auto-extracts CHANGELOG sections per `4577dd7`).

## 2026-04-28 — Wave 2: half-shipped plans closed out + real-fixture CI gate

Three DCO-signed commits delivered the trust-restoration leg of the
revised strategic plan (driven by
`plans/2026-04-28-wave-2-close-out-v1.md`).

| Thread | SHA | Subject |
|---|---|---|
| A | `76b9b08` | `feat(fono-bench): typed ModelCapabilities + split equivalence/accuracy thresholds` |
| B | `87221a2` | `feat(fono-update): per-asset sha256 sidecar verification + --bin-dir` |
| C | _this commit_ | `ci(fono-bench): real-fixture equivalence gate with tiny.en + baseline JSON anchor` |

**Thread A** lifted the inline `english_only` boolean
(`crates/fono-bench/src/bin/fono-bench.rs:339` pre-wave) into a typed
`ModelCapabilities` value at `crates/fono-bench/src/capabilities.rs`
with `for_local_whisper` / `for_cloud` resolvers, split the conflated
single threshold into `equivalence_threshold` and `accuracy_threshold`
on `ManifestFixture`, and added a typed `SkipReason` (`Capability` /
`Quick` / `NoStreaming` / `RuntimeError`) so `overall_verdict` no
longer needs to substring-match notes. New mock-STT capability-skip
integration test asserts `transcribe` is never invoked.

**Thread B** closed the supply-chain gap in `apply_update`: per-asset
`.sha256` sidecars are now fetched and verified during
`fetch_latest` / `apply_update`, with a `parse_sha256_sidecar` helper
covering bare-digest, text-mode, binary-mode, and multi-entry
sidecars. `--bin-dir <path>` is exposed on `fono update` for
non-default install layouts. Release workflow emits a `<asset>.sha256`
file per artefact alongside the aggregate `SHA256SUMS`.
`docs/dev/update-qa.md` carries the ten-scenario manual verification
checklist (bare-binary, `/usr/local/bin`, distro-packaged, offline,
rate-limited, mismatched sidecar, prerelease, `--bin-dir`, rollback).

**Thread C** replaced the compile-only `cargo bench --no-run` step at
`.github/workflows/ci.yml:64-68` with a real-fixture equivalence gate:
the workflow fetches the whisper `tiny.en` GGML weights (cached via
`actions/cache@v4` keyed on the model SHA, integrity-checked against
`921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f`),
runs `fono-bench equivalence --stt local --model tiny.en --baseline
--no-legend`, and diffs per-fixture verdicts against
`docs/bench/baseline-comfortable-tiny-en.json`. The `--baseline` flag
strips absolute timings (`elapsed_ms`, `ttff_ms`, `duration_s`) from
the JSON so the committed anchor is deterministic across CI runners.
Regeneration procedure + flapping-fixture mitigation documented in
`docs/bench/README.md`. R5.1 and R5.2 in
`docs/plans/2026-04-25-fono-roadmap-v2.md` now ticked as fully shipped.

Bonus: `tests/check.sh` lands as a single command that mirrors the CI
build/clippy/test matrix locally (full / `--quick` / `--slim` /
`--no-test` modes) so contributors can run the same gate before
pushing.

Verification (this session):

| Command | Result |
|---|---|
| `cargo build --workspace --all-targets` | clean |
| `cargo test --workspace --lib --tests` | green (all suites incl. new `parse_sidecar_*` tests) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |

## 2026-04-28 — Doc reconciliation pass

Pure-doc pass driven by `plans/2026-04-28-doc-reconciliation-v1.md`. No
Rust source touched. Highlights:

- **`crates/fono/tests/pipeline.rs` is not broken on `main`.** The earlier
  status entry below (line ~50) calling out an `Injector` signature
  mismatch was stale: the signatures align in the current source
  (`crates/fono/src/session.rs:140-142` vs
  `crates/fono/tests/pipeline.rs:54-58`) and the workspace test gate runs
  green. Verified this session: `cargo build --workspace`,
  `cargo test --workspace --lib --tests`, and `cargo clippy --workspace
  --no-deps -- -D warnings` are all clean.
- **Self-update plan `plans/2026-04-27-fono-self-update-v1.md`** —
  ~85% landed in commit `3e2c742` (2026-04-22) without ever being
  reflected in the plan tree. This pass ticks Tasks 1–11, 13–15
  (partial), 17–19 and adds an explicit Status header + Open
  follow-ups list. Remaining work (Tasks 12, 16, 20–22) carried
  forward as Wave 2 Task 8.
- **Equivalence accuracy gate plan
  `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md`**
  — ~50% landed in commits `b6596c0` and `7db29b5` (2026-04-28) as
  inline behaviour (`english_only = args.stt == "local" &&
  args.model.ends_with(".en")` at
  `crates/fono-bench/src/bin/fono-bench.rs:339`,
  `Metrics.stt_accuracy_levenshtein` at
  `crates/fono-bench/src/equivalence.rs:113-114`), without the typed
  `ModelCapabilities` API the plan describes. This pass ticks Tasks 7,
  8, 12, 17, 18 with annotations and carries the typed-API refactor
  forward as Wave 2 Task 7.
- **R3.1 in-wizard latency probe** shipped in commit `7bea0a9`
  (`crates/fono/src/wizard.rs:72, 720, 725`). The same commit advertised
  a "R5.1 CI bench gate" but only added `cargo bench --no-run`
  compile-sanity at `.github/workflows/ci.yml:64-68`; the real-fixture
  equivalence-harness gate is carried forward as Wave 2 Task 9.
  `docs/plans/2026-04-25-fono-roadmap-v2.md` Tier-1 reconciled to
  reality (R2.1, R3.1, R3.2, R3.3, R4.1, R4.2, R4.3, R4.4 ticked; R5.1
  demoted to partial).
- **Three obsolete plans superseded** by the
  `--allow-multiple-definition` link trick already live in
  `.cargo/config.toml:21-28`:
  `plans/2026-04-27-candle-backend-benchmark-v1.md`,
  `plans/2026-04-27-llama-dynamic-link-sota-v1.md`, and
  `plans/2026-04-27-shared-ggml-static-binary-v1.md` were moved to
  `plans/closed/` with `Status: Superseded` headers. None of the three
  was ever executed; the linker workaround landed first.
- **ADR backfill.** `docs/decisions/` previously listed only
  `0001`–`0004`, `0009`, `0015`, `0016` while plan history and status
  entries referenced `0005`–`0008` and `0010`–`0014`. Reconstructed
  stubs for the missing numbers landed this pass with `Status:
  Reconstructed (original lost in filter-branch rewrite)` headers, plus
  three new ADRs: `0017-auto-translation.md` (forward-reference for the
  pending feature), `0018-ggml-link-trick.md` (active `--allow-multiple-definition`
  decision), and `0019-platform-scope.md` (v0.x Linux-multi-package
  scope).

Verification (this session, `4517133` + doc edits only):

| Command | Result |
|---|---|
| `cargo build --workspace` | clean |
| `cargo test --workspace --lib --tests` | green |
| `cargo clippy --workspace --no-deps -- -D warnings` | clean |

## 2026-04-28 — Language allow-list (constrained Whisper auto-detect)

User reported: *"A lot of the people will use fono in more than one
language. But whisper might autodetect some of the other languages.
We need to be able to specify a list of languages that should be
considered and the others should essentially be banned."*

Plan: `plans/2026-04-28-stt-language-allow-list-v1.md`.

**Schema** — `[general]` and `[stt.local]` gain a new `languages:
Vec<String>` field. Empty = unconstrained Whisper auto-detect (today's
default); one entry = forced single language (today's `language = "ro"`);
two-or-more = constrained auto-detect: Whisper picks from the allow-list,
every other language is **banned**. The legacy scalar `language: String`
is still accepted on read and migrated into `languages` on first save
(`skip_serializing_if = "String::is_empty"` drops it from disk).

**Local Whisper** (`crates/fono-stt/src/whisper_local.rs`) — when an
allow-list is in effect, run `WhisperState::lang_detect` on the prefix
mel, mask probabilities to allow-list members only, argmax → run
`full()` with the picked code locked. Forced and Auto paths preserve
the previous one-pass behaviour (no extra cost).

**Cloud STT** (`groq.rs`, `openai.rs`) — banning is impossible at the
provider API. Two opt-in knobs on `[general]`:
`cloud_force_primary_language` (sends `languages[0]` instead of `auto`)
and `cloud_rerun_on_language_mismatch` (one extra round-trip when the
returned `language` is outside the allow-list). Defaults preserve the
current cost profile.

**New module** `crates/fono-stt/src/lang.rs` carries the
`LanguageSelection` enum (`Auto` / `Forced(code)` / `AllowList(Vec)`)
and the parser, so backends never compare sentinel strings like
`"auto"` directly.

**Wizard** — both `configure_cloud` and `configure_mixed` now persist
their language prompt (previously discarded into `_lang`) into
`general.languages` via `LanguageSelection::parse_csv`.

**Verification** — `cargo build --workspace`, `cargo test --workspace
--lib`, and `cargo clippy -p fono-stt -p fono-core -p fono --lib --bins
-- -D warnings` all green. New tests in `lang.rs` cover the parser /
normaliser; `config.rs::languages_round_trip_drops_legacy_field` and
`explicit_languages_wins_over_legacy_scalar` lock the migration.

The pre-existing `crates/fono/tests/pipeline.rs` `Injector` signature
mismatch is unrelated to this change and was already broken on
`main`.

## 2026-04-28 — Overlay focus-theft eliminated (X11 override-redirect)

User reported: *"The overlay window still seems to be stealing focus
twice; when it appears in live mode and when it does cleanup."*

The previous mitigation (`.with_active(false)` +
`WindowType::Notification`, landed in `1f23194`) is correct in spirit,
but X11 window managers disagree about how aggressively to honour
those hints across multiple map cycles. The overlay is shown → hidden
→ shown again twice per dictation (live state, then
processing/finalize state), and many WMs default to "give focus on
map" on the second-and-subsequent map even for notification toplevels.
Net result was that every overlay state transition re-stole focus
from the user's editor / terminal / browser, and the synthesized
`Shift+Insert` paste then landed in the overlay itself rather than
the original target window.

**Fix landed in `d2823f1`** (`crates/fono-overlay/src/real.rs:488-494`):
add `.with_override_redirect(true)` to the X11 window attributes on
top of the existing `.with_active(false)` and
`WindowType::Notification` hints. Override-redirect windows are
completely outside WM management — the X server never asks the WM
about focus, mapping, or stacking for them. This is what tooltips,
dmenu, and rofi all do; it makes focus theft physically impossible
on X11 regardless of WM behaviour.

**Trade-offs**

- WM-managed always-on-top is lost. Mitigation: borderless
  override-redirect windows naturally stack above normal toplevels
  because the WM never moves them on focus changes; no observable
  regression vs the prior `WindowLevel::AlwaysOnTop` hint.
- Compositor-managed transparency varies slightly across compositors
  for OR windows. picom honours it; KWin and Mutter compose it
  correctly. The solid-charcoal fallback at `COLOR_BG = 0xEE17171B`
  still applies if the compositor refuses the alpha channel.

**Wayland deferred to Slice B.** On Wayland the compositor controls
focus completely; the proper solution is `xdg_activation_v1` /
`wlr-layer-shell` from a dedicated overlay subprocess, which is the
Slice B subprocess-overlay refactor (ADR 0009 §5). For Slice A this
X11-only fix matches the dominant target environment.

**Verification**

| Command | Result |
|---|---|
| `cargo build  -p fono-overlay --features real-window` | clean |
| `cargo clippy -p fono-overlay --features real-window -- -D warnings` | clean |
| `cargo test   -p fono-overlay --lib` | 2/0 |

(Workspace clippy currently reports unrelated in-flight bench errors
from the v7 equivalence-fixtures swap; tracked separately.)

## 2026-04-27 — Slice A v7 delta landed (boundary heuristics)

Plan v7 (`plans/2026-04-27-fono-interactive-v7.md`) extends Slice A with
boundary-quality heuristics. Four DCO-signed commits on top of v6 Slice A:

| SHA       | Title |
|-----------|-------|
| `ce6a21e` | fono-core(config): v7 `[interactive]` keys (boundary heuristics) |
| `d0e21a0` | fono(live): R2.5 prosody/punct chunk-boundary + R7.3a hold-on-filler drain |
| `beae861` | fono-bench(equivalence): pin v7 boundary knobs + A2 row variants |
| `6a6c6c1` | docs: ADR 0015 + interactive.md tuning section |

**What landed**

- R9.1 — `[interactive]` config grew from 4 keys to 18, covering the v6
  carryover (`mode`, `chunk_ms_initial/steady`, `cleanup_on_finalize`,
  `max_session_seconds/cost_usd`) and the v7 heuristic knobs
  (`commit_use_prosody`, `commit_use_punctuation_hint`,
  `commit_hold_on_filler`, `commit_filler_words`,
  `commit_dangling_words`, plus matching `*_ms` extensions). Reserved
  `eou_adaptive` / `resume_grace_ms` defined but inert until Slice D.
- R2.5 — prosody pitch-tail tracker (hand-rolled time-domain
  autocorrelation, no FFT dep) wired into the FrameEvent → StreamFrame
  translator; punctuation-hint pure function shipped, full wiring
  deferred to Slice B (translator can't yet see preview text).
- R7.3a — filler/dangling-word suffix detection; ships as informational
  signal on `LiveTranscript` rather than a true drain extension to
  avoid an >80 LoC pump refactor. Daemon can act on the flags now;
  Slice D's adaptive-EOU work will make the extension first-class.
- R10.5 / R10.6 — tracing fields on `live.first_stable` + 13 new
  heuristic-isolation unit tests + 2 new equivalence-harness tests.
- R18.10 / R18.23 — pinned heuristic knobs in equivalence reports;
  four A2 row variants (`A2-no-heur`, `A2-default`, `A2-prosody`,
  `A2-filler`); `A2-default` gates Tier-1 + Tier-2.
- ADR 0015 — boundary-heuristics architecture, additive-only invariant,
  forward-reference to adaptive EOU in Slice D.

Verification gate (slim + `interactive` feature): build clean, clippy
clean with `-D warnings`, all tests green (no regressions).

## 2026-04-27 — Slice A landed (interactive / live dictation)

Plan v6 (`plans/2026-04-27-fono-interactive-v6.md`) Slice A is in.
Five commits on `main`, each DCO-signed:

| SHA       | Title |
|-----------|-------|
| `7fbf974` | Slice A checkpoint: streaming primitives, overlay, budget, live session |
| `92d4cc3` | Slice A: live pipeline integration tests (plan v6 R10.2) |
| `074a6c7` | Slice A: equivalence harness foundation + 2 fixtures (plan v6 R18) |
| `c3f2b68` | Slice A: ADR 0009 + interactive.md user guide (plan v6 R11) |
| (this)    | Slice A: docs/status.md — Slice A complete, Slice B queued |

The four Forge follow-up commits to `7fbf974` cover deliverables R10.2,
R18 (foundation), R11.1, R11.2, and R17 (status update).

### What Slice A actually ships

- **R1 / R3** — `fono-stt::StreamingStt` trait + `LocalAgreement`
  helper + dual-pass finalize lane on top of `WhisperLocal`. Gated
  behind the `streaming` cargo feature on `fono-stt`.
- **R2** — `fono-audio::AudioFrameStream` + `FrameEvent` enum + VAD-
  driven segment-boundary heuristic. Gated behind `fono-audio/streaming`.
- **R5** — Live overlay (`fono-overlay::OverlayState::LiveDictating`
  + `RealOverlay` winit window) painting preview / finalize text.
  In-process; sub-process refactor deferred to Slice B (see ADR 0009 §5).
- **R7.4 / R10.2** — `fono::live::LiveSession` orchestrator that wires
  `Pump` → `AudioFrameStream` → `StreamingStt` → overlay. Two new
  integration tests (`crates/fono/tests/live_pipeline.rs`) drive it
  with a synthetic `StreamingStt` and assert (a) two-segment
  concatenation under preview→finalize lanes and (b) clean
  cancellation when no voiced frames arrive.
- **R10.4** — `fono record --live` CLI — record-then-replay-through-
  streaming. Realtime cpal-callback push lands in Slice B.
- **R11.1** — ADR `docs/decisions/0009-interactive-live-dictation.md`
  capturing the six locked architectural decisions for Slice A.
- **R11.2** — User-facing guide `docs/interactive.md` covering
  `[interactive].enabled`, the `interactive` cargo feature, the
  `fono record --live` and `fono test-overlay` flows, and the two
  known issues (hostile compositors, Wayland focus theft).
- **R12** — `fono-core::BudgetController` (price table + per-minute
  ceiling + `BudgetVerdict::{Continue, StopStreaming}`) wired into
  `LiveSession::run`. Gated behind `fono-core/budget`.
- **R17.1 / R18 (foundation)** — Streaming↔batch equivalence harness
  in `crates/fono-bench/src/equivalence.rs` + `fono-bench equivalence`
  subcommand + two synthetic-tone WAV fixtures
  (`tests/fixtures/equivalence/{short-clean,medium-pauses}.wav`,
  ~410 KB total). 7 new unit tests cover the levenshtein
  normalization, JSON round-trip, overall-verdict aggregation, and
  manifest parsing. End-to-end smoke (`--stt local --model tiny.en`)
  produced PASS on both fixtures.

### Bug fixed in passing

`LiveSession::run` previously called `pump.subscribe()` *after* the
caller had pushed PCM and called `pump.finish()` — which loses every
frame because `tokio::sync::broadcast` does not deliver pre-subscribe
messages to fresh subscribers. `Pump` now pre-subscribes a primary
receiver at construction and exposes it via
`Pump::take_receiver()`; `LiveSession::run` takes a
`broadcast::Receiver<FrameEvent>` directly, and `fono record --live`
spawns the run task before pushing so the broadcast buffer drains
between pushes. Caught while landing the live integration tests; not
in scope of `7fbf974` itself.

### Build matrix (verified this session)

| Command | Result |
|---|---|
| `cargo build --workspace` | ✅ |
| `cargo build --workspace --features fono/interactive` | ✅ |
| `cargo clippy --workspace --no-deps -- -D warnings` | ✅ |
| `cargo clippy --workspace --no-deps --features fono/interactive -- -D warnings` | ✅ |
| `cargo test --workspace --lib --tests` | ✅ 110 ok, 0 fail (was 103 at HEAD) |
| `cargo test --workspace --lib --tests --features fono/interactive` | ✅ 126 ok, 0 fail |
| `cargo run -p fono-bench --features equivalence,whisper-local -- equivalence --stt local --model tiny.en --output report.json` | ✅ both fixtures PASS |

### Deferred to Slice B (next session candidates)

- **R4 / R8 / R10.4 (realtime)** — Cloud streaming providers (Groq,
  OpenAI realtime, Deepgram, AssemblyAI) and the realtime cpal-
  callback audio push so the overlay paints text *while* you speak.
- **R5.6** — Overlay sub-process refactor for crash isolation.
- **R18 cloud rows** — Cloud-streaming equivalence rows of R18
  (`--stt groq` and friends). Requires the cloud-mock recordings
  pipeline that the v6 plan R18.12 sketches.
- **R18 Tier-2** — With-LLM equivalence comparison (`--llm local
  qwen-0.5b`). The Tier-1 (whisper-only) gate is in; Tier-2 needs
  the deterministic-LLM scaffolding (n_threads=1 + seed-pinning) to
  produce stable outputs.
- **R18.6 fixture set completion** — The remaining 10 fixtures of the
  curated 12-fixture set (long-monologue, noisy-cafe, accented-EN,
  numbers/commands, whispered, with-music, multi-speaker,
  code-dictation, long-with-pauses, short-noisy-quick). Needs real
  CC0 audio sources.
- **R16** — Tray icon-state palette refactor.

### Recommended next session

1. **Slice B kickoff** — wire the realtime cpal-callback push and the
   first cloud streaming provider (Groq's faster-whisper streaming
   endpoint is the obvious first target — same auth flow as the
   existing Groq batch backend).
2. **Or, if Slice B is too big a chunk to start cold:** drop the
   remaining 10 R18 fixtures into `tests/fixtures/equivalence/` from
   real CC0 LibriVox / Common Voice clips, recompute SHA-256s, set
   `synthetic_placeholder = false` in the manifest, and tighten
   `TIER1_LEVENSHTEIN_THRESHOLD` from `0.05` back to the v6 plan's
   strict `0.01` in the same commit. Self-contained, fast feedback.

## Hotkey ergonomics — single-key defaults

Default hotkeys switched from three-key chords to single function keys:

- `toggle = "F9"` (was `Ctrl+Alt+Space`)
- `hold = "F8"` (was `Ctrl+Alt+Grave`)
- `cancel = "Escape"` (unchanged — only grabbed while recording)
- `paste_last` hotkey **removed**. The tray's "Recent transcriptions"
  submenu and the `fono paste-last` CLI cover the same need with a
  better UX (re-paste any of the last 10, not just the newest).

Touched: `crates/fono-core/src/config.rs`, `crates/fono-hotkey/{fsm,listener,parse}.rs`,
`crates/fono-ipc/src/lib.rs` (kept `Request::PasteLast` for CLI), `crates/fono/src/{daemon,wizard}.rs`,
`crates/fono-tray/src/lib.rs`, `README.md`, `docs/troubleshooting.md`, `docs/wayland.md`.

`Request::PasteLast` now routes directly to `orch.on_paste_last()` instead of
through the FSM, since there is no longer a hotkey path for it.

## Single-binary local STT + local LLM (ggml symbol collision resolved)

Default builds now ship **both** local STT (`whisper-rs`) and local LLM
(`llama-cpp-2`) statically linked into one self-contained `fono` binary —
the previous `compile_error!` guard in `crates/fono/src/lib.rs` is gone, and
`crates/fono/Cargo.toml` re-enables `llama-local` in `default`.

The `ggml` duplicate-symbol collision (each sys crate vendors its own static
`ggml`) is resolved at link time via `-Wl,--allow-multiple-definition` in
the new `.cargo/config.toml`. Both crates' `ggml` copies originate from the
same `ggerganov` upstream and are ABI-compatible; the linker keeps one set
of symbols and discards the duplicate. Verified post-link with
`nm target/release/fono | grep ' [Tt] ggml_init$'` → exactly one entry.

A new smoke test `crates/fono/tests/local_backends_coexist.rs` constructs a
`WhisperLocal` and a `LlamaLocal` in the same process to guard against
runtime breakage from any future upgrade of either sys crate.

### Hardware acceleration banner

Every daemon start now logs an `info`-level summary of the actual
accelerator path the binary will use, e.g.:

```
hw accel     : CPU AVX2+FMA+F16C
```

Implemented in `crates/fono/src/daemon.rs::hardware_acceleration_summary`.
GPU backends are wired through opt-in cargo features
(`accel-cuda` / `accel-metal` / `accel-vulkan` / `accel-rocm` /
`accel-coreml` / `accel-openblas`) on `fono`, `fono-stt`, and `fono-llm`;
flipping any of them prepends the matching label (e.g. `CUDA + CPU AVX2`).
The default ship build stays CPU-only — single binary, runs everywhere,
auto-picks the best SIMD kernel ggml has compiled in.

## H8 landed — real local LLM cleanup via `llama-cpp-2`

`crates/fono-llm/src/llama_local.rs` is no longer a stub. The `llama-local`
feature now runs honest GGUF inference: process-wide `LlamaBackend` cached in
a `OnceLock`, lazy model load via `Arc<Mutex<Option<LlamaModel>>>` (mirrors
`WhisperLocal`), greedy sampling, ChatML prompt template that fits both
Qwen2.5 and SmolLM2, `MAX_NEW_TOKENS = 256`, EOS + `<|im_end|>` stop tokens,
and a `tokio::task::spawn_blocking` boundary so the async runtime keeps
moving while llama.cpp grinds. The factory grew an `llm_models_dir` parameter
that resolves `cfg.local.model` (a name) to `<dir>/<name>.gguf` — the
existing scaffold's "model NAME passed as a path" bug is gone.

A cleanup that takes > 5 s emits a `warn!` recommending the user pick a
cloud provider (`fono use llm groq` / `cerebras`) or a smaller model. CPU-only
Q4_K_M inference of a 1.5B-parameter model is on the order of 5–15 tok/s on
a laptop, so this matters: the wizard continues to default-skip the local
LLM for tiers ≤ `Recommended`. Local LLM model auto-download (H9 / H10) is
still open — follow-up.

**Build constraint.** `whisper-rs-sys` and `llama-cpp-sys-2` each statically
link their own copy of ggml; combining both in one binary collides on every
`ggml_*` symbol. We keep the static-binary stance (no sidecar `libllama.so`)
by guarding the combo with a `compile_error!` in `crates/fono/src/lib.rs`.
Default-features build (whisper-local + cloud LLM) works as before. Users
who want local LLM cleanup build cloud-STT instead:

```
cargo build --release --no-default-features --features tray,llama-local,cloud-all
```

Lifting this constraint requires moving llama.cpp to a shared library
(`llama-cpp-sys-2/dynamic-link`), which is **not** the path forward — fono
ships as a single self-contained binary.

## Recent fix — silenced GTK/GDK startup warnings

User reported a `Gdk-CRITICAL: gdk_window_thaw_toplevel_updates: assertion ...
freeze_count > 0 failed` line at startup. This is a benign assertion fired by
libappindicator/GTK3 when the indicator first paints on KDE's StatusNotifier
host; the tray works correctly. The tray thread now installs `glib`
log handlers for the `Gdk`, `Gtk`, `GLib-GObject`, and `libappindicator-gtk3`
domains and demotes their warning/critical messages to `tracing::debug`, so
default startup is clean.

## Recent fix — cancel hotkey only grabbed while recording

User reported Fono was holding a global grab on `Escape`, blocking it in other
apps. The cancel hotkey is now registered with the OS only when entering the
Recording state and unregistered as soon as recording stops or is cancelled.
Implemented via a new `HotkeyControl` channel between the daemon's FSM event
loop and the `fono-hotkey` listener thread, plus an `unregister(...)` call in
the listener using the existing `global-hotkey` API.

## Recent fix — quieter whisper logging

User reported there were still too many startup messages coming from whisper.
The default CLI log filters now keep `whisper-rs` whisper.cpp/GGML `info`
chatter hidden behind explicit module-level `FONO_LOG` overrides while keeping
warnings and errors visible.

## Recent fix — quieter daemon startup logging

User reported too many `info` messages when starting Fono. Startup-only details
such as XDG paths, tray/hotkey internals, model-present checks, warmup timings,
inject backend discovery, and paste-shortcut setup now log at `debug`; default
`info` startup keeps only the concise daemon start/ready lines and warnings.

## Recent fix — setup wizard API key paste feedback

User reported that pasting a cloud LLM API key gave no immediate visual
indication that the paste landed. The wizard now reads API keys with a masked
prompt that prints one `*` per accepted character, then reports the received
character count before validation. The key contents remain hidden.

## Recent fix — setup wizard nested Tokio runtime panic

User reported a setup crash after adding a Groq key:
`Cannot start a runtime from within a runtime` at `crates/fono/src/wizard.rs:627`.
Root cause: the local-STT latency probe built a new Tokio runtime and called
`block_on()` while the setup wizard was already running inside Tokio. The probe
is now async and awaits `stt.transcribe(...)` on the existing wizard runtime.

## Recent fixes — tray menu hardening (env-var leak + stale binary)

User reported: "I can still see backends that aren't configured for STT and
LLM and switching through them doesn't seem to dynamically switch while the
software is running." Two distinct issues; both fixed.

1. **Env-var leak into the tray submenu.** The previous filter used
   `Secrets::resolve()` which falls through to the process environment.
   On a typical dev machine with `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`
   etc. exported in the shell, every one of those backends was wrongly
   marked "configured" and listed in the menu — clicking them then
   produced a 401 on the next dictation. New strict filter:
   `crates/fono-core/src/secrets.rs` exposes `has_in_file()` /
   `resolve_in_file()` and `crates/fono-core/src/providers.rs:178-218`
   (`configured_stt_backends` / `configured_llm_backends`) only consult
   `secrets.toml`. Two regression tests
   (`configured_filter_ignores_env`, `configured_filter_includes_explicit_keys`)
   pin the new contract.
2. **Stale release binary.** The binary at `target/release/fono` was
   older than the daemon's tray-filter source — the user was running
   the pre-fix version and the menu still listed every backend. Rebuilt
   so the live binary matches the source.

## Recent fixes — tray polish + whisper log noise + repo URL

- **Tray menu trimmed.** Removed the broken `Open history folder` entry
  (`xdg-open` on the data directory just opened the parent in Dolphin and
  was useless). The `Recent transcriptions` submenu is the supported way to
  revisit history.
- **Provider submenus restricted to configured backends.** STT/LLM submenus
  now only list backends whose API key is present in `secrets.toml` (plus
  `Local` and `None`). New helpers in `crates/fono-core/src/providers.rs`:
  `configured_stt_backends` / `configured_llm_backends`. Eliminates the
  "click OpenAI in tray, get a 401 on next dictation" trap.
- **Whisper.cpp log noise silenced.** `whisper-rs 0.16` ships a
  `whisper_rs::install_logging_hooks()` redirector that funnels GGML and
  whisper.cpp logs through `tracing`. Enabled via the new `log_backend`
  feature in workspace `Cargo.toml` and a `Once` guard in
  `crates/fono-stt/src/whisper_local.rs`. With the default `info` filter
  the formerly noisy timing dumps stay silent; `FONO_LOG=whisper_rs=debug`
  re-enables them when needed.
- **Repo URL → `bogdanr/fono`.** Replaced every reference in `Cargo.toml`,
  `README.md`, `CHANGELOG.md`, `packaging/**`, and systemd units with
  `github.com/bogdanr/fono`.

## Recent fixes (Tier-1 roadmap pass — wizard + docs polish)

- **Wizard rewrite** (`fono/src/wizard.rs`): now offers four explicit
  paths instead of a binary local/cloud choice — `Local`, `Cloud`,
  `Mixed (Cloud STT + Local LLM)`, `Mixed (Local STT + Cloud LLM)`. Path
  recommendation order is hardware-tier aware (Recommended/High-end →
  local first; Minimum → cloud first; Unsuitable → cloud only).
- **Cloud key validation** (R3.2): every API key entered in the wizard
  is hit against the provider's `/v1/models` endpoint with a 5 s
  timeout before persistence. 401/403 responses re-prompt for the key;
  network errors warn but allow override (offline-first install).
- **`docs/inject.md`** — full reference for the injection stack: priority
  table, paste-shortcut precedence, per-environment recipes (Wayland /
  KDE-Wayland / X11 / terminals / Vim / tmux), and troubleshooting.
- **`docs/troubleshooting.md`** — symptom-first guide covering hotkey,
  pipeline, STT, latency, tray, audio, provider switches, and bug
  reporting checklist.

## Recent fixes (Tier-1 roadmap pass — provider-switching tray + docs)

- **Tray STT/LLM submenus** (`fono-tray/src/lib.rs`, `fono/src/daemon.rs`).
  Right-click the tray icon → `STT: <active> ▸` or `LLM: <active> ▸` shows
  every backend with the active one ticked; click another item to hot-swap.
  Same code path as `fono use stt … / llm …` (atomic config rewrite +
  orchestrator `Reload`); tray notification confirms the switch.
- **README v0.1.0 pass** — added CLI cheatsheet entries for `fono use`,
  `fono keys`, `fono test-inject`, `fono hwprobe`, plus a tray-menu visual
  reference and a Text-Injection section explaining the Shift+Insert default
  + override layers.
- **CHANGELOG v0.1.0 entry** drafted (`CHANGELOG.md`) — pipeline, providers,
  hardware tiers, injection, tray, observability, bench harness, model
  matrix, known limitations.

## Recent fixes (delivery path — clipit/Wayland)

- **Default paste shortcut → Shift+Insert** (`fono-inject/src/xtest_paste.rs`).
  Was Ctrl+V — captured by shells/tmux/vim normal mode/terminal verbatim-
  insert bindings. Shift+Insert is the X11 legacy paste binding hard-coded
  into virtually every toolkit (xterm/urxvt/st PRIMARY, GTK/Qt CLIPBOARD,
  VTE-based PRIMARY, alacritty/kitty CLIPBOARD, Vim/Emacs in insert mode);
  fono populates **both** PRIMARY and CLIPBOARD on every dictation so the
  toolkit's selection choice is invisible. Net effect: text now lands in
  terminals as well as GUI apps.
- **`PasteShortcut` enum** with `ShiftInsert` (default), `CtrlV`,
  `CtrlShiftV`. Generalized XTEST sender: presses modifiers in order,
  presses key, releases in reverse, with `Insert` ↔ `KP_Insert` keysym
  fallback for exotic keymaps.
- **Two override layers** for the rare app that needs a different binding:
  - `[inject].paste_shortcut = "ctrl-v"` in `~/.config/fono/config.toml`
    (validated at startup; typos surface as a warn-level log line).
  - `FONO_PASTE_SHORTCUT=ctrl-v` env var (highest precedence; useful for
    one-shot testing without editing config).
  - `fono test-inject "..." --shortcut ctrl-v` flag for the smoke command.
- **Diagnostic surfaces**:
  - `fono doctor` now prints `Paste keys  : Shift+Insert (config="..."  env=...)`.
  - `fono test-inject` prints the active shortcut at the top.
  - Inject path logs `xtest-paste: synthesizing Shift+Insert (mod_keycodes=...)`
    so users can confirm what was actually sent.
- **Pure-Rust XTEST paste backend** (`fono-inject/src/xtest_paste.rs`,
  `x11-paste` feature, **on by default**). Synthesizes the configured
  shortcut against the focused X11 / XWayland window after writing to the
  clipboard. **No system tools required** — works on any X session even
  without `wtype`/`ydotool`/`xdotool`/`enigo`. Auto-selected by
  `Injector::detect()` on X11 when no other backend is available; verified
  live: `typed via xtest-paste in 15ms`.
- **`FONO_INJECT_BACKEND=xtest|paste|xtestpaste`** override for forcing
  the backend during testing.

- **Multi-target clipboard write** (`fono-inject/src/inject.rs`) — new
  `copy_to_clipboard_all()` writes to **every** detected backend
  (wl-copy + xclip clipboard + xsel + xclip primary) so X11-only managers
  like clipit catch the entry on Wayland sessions, and Wayland-native
  managers like Klipper catch it on hybrid setups.
- **Per-tool stderr capture** — silent failures (no `DISPLAY`, missing
  protocol support, non-zero exit) are now surfaced in logs and in
  `fono test-inject` output instead of being swallowed.
- **`Injector::Xdotool` subprocess backend** — independent of the
  `libxdo` C dep; XWayland fallback for KWin sessions where `wtype` is
  accepted but silently dropped.
- **`FONO_INJECT_BACKEND=…` override** — forces a specific injector for
  testing.
- **`fono test-inject "<text>"`** — bypasses STT/LLM, prints per-tool
  diagnostic + clipboard readback verification.
- **readback_clipboard `.ok()?` short-circuit fix** — verifier no longer
  aborts when the first read tool isn't installed.

## Current milestone

**v0.1.0-rc: provider switching without daemon restart.** Local-models
default + hardware-adaptive wizard (previous slice) plus a one-command
provider-switching UX: `fono use stt groq`, `fono use cloud cerebras`,
`fono use local`, plus `fono keys add/list/remove/check` and per-call
`fono record --stt … --llm …` overrides. All flips hot-reload through a
new `Request::Reload` IPC; the orchestrator hot-swaps STT/LLM behind a
`RwLock<Arc<dyn _>>` and re-prewarms on every reload.

## Active plans

| Plan | Status |
|---|---|
| `docs/plans/2026-04-24-fono-design-v1.md` (Phases 0–10) | ✅ Phases 0–10 landed |
| `docs/plans/2026-04-25-fono-pipeline-wiring-v1.md` (W1–W22) | ✅ 22/22 |
| `docs/plans/2026-04-25-fono-latency-v1.md` (L1–L30) | ✅ 17/30 landed, 13 deferred-to-v0.2 |
| `docs/plans/2026-04-25-fono-local-default-v1.md` (H1–H25) | ✅ 11/25 landed, 14 deferred-to-v0.2 |
| `docs/plans/2026-04-25-fono-provider-switching-v1.md` (S1–S27) | ✅ 16/27 landed, 11 deferred-to-v0.2 |
| `plans/2026-04-27-fono-self-update-v1.md` | ~85% landed in `3e2c742`; finishing pass tracked as Wave 2 Task 8 |
| `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md` | ~50% landed in `b6596c0`/`7db29b5`; typed-API refactor tracked as Wave 2 Task 7 |
| `plans/2026-04-28-fono-auto-translation-v1.md` | Not started (Wave 4 of revised strategic plan) |
| `plans/closed/` (candle / dynamic-link / shared-ggml) | Superseded by `--allow-multiple-definition` link trick (ADR 0018) |

## Phase progress

| Phase | Description                                                        | Status |
|-------|--------------------------------------------------------------------|--------|
| 0     | Repo bootstrap + workspace + CI skeleton                           | ✅ Complete |
| 1     | fono-core: config, secrets, XDG paths, SQLite schema, hwcheck      | ✅ Complete |
| 2     | fono-audio: cpal capture + VAD stub + resampler + silence trim     | ✅ Complete |
| 3     | fono-hotkey: global-hotkey parser + hold/toggle FSM + listener     | ✅ Complete |
| 4     | fono-stt: trait + WhisperLocal + Groq/OpenAI + factory + prewarm   | ✅ Complete |
| 5     | fono-llm: trait + LlamaLocal stub + OpenAI-compat/Anthropic + factory + prewarm | ✅ Complete |
| 6     | fono-inject: enigo wrapper + focus detection + warm_backend        | ✅ Complete |
| 7     | fono-tray (real appindicator backend) + fono-overlay stub          | ✅ Complete |
| 8     | First-run wizard + CLI (+ tier-aware probe + `fono hwprobe`)       | ✅ Complete |
| 9     | Packaging: release.yml + NimbleX SlackBuild + AUR + Nix + Debian   | ✅ Complete |
| 10    | Docs: README, providers, wayland, privacy, architecture            | ✅ Complete |
| W     | Pipeline wiring (audio→STT→LLM→inject orchestrator)                | ✅ Complete |
| L     | Latency optimisation v0.1 wave (warm + trim + skip + defaults)     | ✅ Complete |
| H     | Local-models out of box + hardware-adaptive wizard (v0.1 slice)    | ✅ Complete |
| S     | Easy provider switching: `fono use`, `fono keys`, IPC Reload, hot-swap | ✅ Complete |

## What landed in this session (2026-04-25, provider switching)

* **S1/S2/S3** — `crates/fono-core/src/providers.rs` central registry of
  every backend's CLI string + canonical env-var name + paired-cloud
  preset. Factories in `fono-stt` / `fono-llm` now resolve a missing
  `cloud` sub-block by falling through to the canonical env var, so the
  smallest valid cloud config is just `stt.backend = "groq"` plus a key
  in `secrets.toml` or env.
* **S4/S5/S6** — `fono use stt|llm|cloud|local|show` subcommand tree in
  `crates/fono/src/cli.rs`; per-call `--stt` / `--llm` overrides on
  `fono record` and `fono transcribe` clone the in-memory config, never
  persist. `set_active_stt` / `set_active_llm` clear the stale `cloud`
  sub-block but preserve every unrelated user customisation.
* **S7** — `fono keys list|add|remove|check`. Atomic 0600 writes;
  `check` runs the same 2-second reachability probe as `fono doctor`.
* **S11/S12/S13** — new `Request::Reload` IPC variant; orchestrator
  holds STT + LLM + Config each behind a `RwLock<Arc<…>>`; `reload()`
  re-reads config + secrets, rebuilds via factories, swaps in place,
  and re-runs `prewarm()` so the first dictation after a switch is
  warm. `fono use` automatically calls Reload on the running daemon.
* **S18** — `fono doctor` Providers section: per-row marker for the
  active backend, key-presence flag, resolved model string, hint to
  switch via `fono use`.
* **S20/S21/S23** — new tests: `crates/fono-stt/src/factory.rs` covers
  cloud-optional resolution; `crates/fono/tests/provider_switching.rs`
  asserts `set_active_stt` / `set_active_llm` preserve unrelated fields,
  TOML round-trip survives swap, and provider-string parsers form a
  bijection with their printers.
* **S24/S25/S27** — `docs/providers.md` rewritten around the new flow;
  README has a "Switching providers" subsection; status.md updated.

## Hotfix this session (2026-04-25, tray Recent submenu + clipboard safety net)

User reported two issues after a real dictation on KDE:

1. *"I can't see any notification or anything in the clipboard after
   doing my last recording"* — root cause was a **subprocess-stdin
   deadlock**: `copy_to_clipboard` borrowed `child.stdin.as_mut()` but
   never closed the pipe, so `xsel`/`xclip`/`wl-copy` (all of which
   read stdin to EOF before daemonizing) hung forever waiting for EOF
   that never came. `child.wait()` then deadlocked, the pipeline
   returned without populating the clipboard, and any notification
   that depended on the outcome never fired. Compounding it: KDE
   Wayland's KWin doesn't implement the wlroots virtual-keyboard
   protocol that `wtype` uses, so even when the inject log read
   `inject: 27ms ok`, no keys actually reached the focused window.
2. *"OpenHistory tray action … should work in a similar fashion to
   clipit"* — clicking the tray entry only opened the parent dir;
   recent dictations weren't visible at all from the tray.

Fixes:

* **`crates/fono-tray/src/lib.rs`** — replaced single `OpenHistory`
  entry with a **"Recent transcriptions" submenu** holding 10
  pre-allocated slots refreshed every ~2 s by a `RecentProvider`
  closure (passed in by the daemon). Click any slot to re-paste that
  dictation. Clipit-style. Slots refresh in place via `set_text` to
  avoid KDE/GNOME indicator flicker. Added `OpenHistoryFolder` as a
  separate entry for power users. New `TrayAction::PasteHistory(usize)`
  carries the slot index.
* **`crates/fono/src/daemon.rs`** — provides the `RecentProvider` that
  reads `db.recent(10)` and returns the cleaned (or raw) labels.
  Handles `PasteHistory(idx)` by fetching the row and calling
  `fono_inject::type_text_with_outcome` on the blocking pool, with a
  notify-rust toast on `Clipboard` outcome.
* **`crates/fono-core/src/config.rs`** — two new `[general]` knobs,
  both default `true`:
  - `also_copy_to_clipboard` — every successful pipeline also copies
    the cleaned text to the system clipboard so the user can Ctrl+V
    even when key injection silently no-op'd.
  - `notify_on_dictation` — every successful pipeline pops a
    notify-rust toast with the dictated text (truncated to 240 chars).
* **`crates/fono-inject/`** — `copy_to_clipboard` made `pub` and
  re-exported so the orchestrator can call it directly.
* **`crates/fono/src/session.rs`** — pipeline now copies-to-clipboard
  + notifies after every successful inject; gives the user reliable
  feedback even on KDE Wayland.

User saw `WARN inject failed: no text-injection backend available` on a
host without `wtype`/`ydotool` and without the `enigo-backend` feature
compiled in. Cleaned text was lost.

* **`crates/fono-inject/src/inject.rs`** — added `Injector::Clipboard`
  fallback that shells out to `wl-copy` (Wayland) → `xclip` → `xsel`
  (X11) and a `wtype --version` page-cache warm step. New
  `InjectOutcome { Typed, Clipboard, NoBackend }` returned from
  `type_text_with_outcome()` so callers can tell the user which path
  ran. `wtype`/`ydotool` failures now fall through to the clipboard
  rather than swallowing the text.
* **`crates/fono/src/session.rs`** — pipeline calls
  `type_text_with_outcome`; on `Clipboard` shows a toast "Fono — text
  copied to clipboard, paste with Ctrl-V"; on `NoBackend` shows a toast
  with a one-line install hint (`pacman -S wtype` / `apt install xsel`).
  The toast prevents a "press hotkey, nothing happens" failure mode
  even when no injector + no clipboard tool exists.
* **`crates/fono/src/doctor.rs`** — Injector section now also lists the
  detected clipboard tool (or "none — text will be lost"); printed near
  the active injector to make the gap obvious.

### Deferred to v0.2 (documented in the plan)

* **S8** wizard multi-key (S7 already lets users add keys post-wizard).
* **S9/S10** named profiles + cycle hotkey (hold for real demand).
* **S14** auto-reload on file change (notify watcher).
* **S15/S16/S17** tray submenu for switching (depends on tray-icon API).
* **S19** dedicated `fono provider list` (covered by `fono use show` + doctor).
* **S22** full reload integration test (covered by S20 unit tests +
  manual; deferred until profiles arrive).
* **S26** ADR `0009-multi-provider-switching.md` (rationale captured in
  this plan + commit messages).

## Build matrix (verified this session, provider switching)

| Command | Result |
|---|---|
| `cargo build --workspace` | ✅ |
| `cargo test --workspace --lib --tests` | ✅ **79 tests pass** (66 unit + 13 hwcheck), 2 ignored (latency smoke) |
| `cargo clippy --workspace --no-deps -- -D warnings` | ✅ pedantic + nursery clean |
| `fono use show` | (manual) prints active stt + llm + key references |
| `fono keys list` | (manual) masked listing |

## What landed in this session (2026-04-25, local-default + hwcheck)

### Tasks fully landed (11 of 25 from the local-default plan)

* **H1** — `crates/fono/Cargo.toml:22-32`: default features now include
  `local-models` (transitively `fono-stt/whisper-local`) so the released
  binary runs whisper out of the box. Slim cloud-only build available
  via `--no-default-features --features tray`.
* **H5/H6/H21** — new `crates/fono-core/src/hwcheck.rs` (478 lines, 13
  unit tests). `HardwareSnapshot::probe()` reads `/proc/cpuinfo`,
  `/proc/meminfo`, `statvfs`, and `std::is_x86_feature_detected!` to
  produce a `LocalTier` ∈ { Unsuitable, Minimum, Comfortable,
  Recommended, HighEnd } with documented thresholds (`MIN_CORES = 4`,
  `MIN_RAM_GB = 4`, `MIN_DISK_GB = 2`, etc.) duplicated as `pub const`
  so docs and tests stay in sync.
* **H11/H12/H13** — wizard rewritten around the tier:
    * `crates/fono/src/wizard.rs` prints the hardware summary up-front.
    * `Recommended`/`HighEnd`/`Comfortable` → local first, default.
    * `Minimum` → cloud first ("faster on your machine"), local kept
      as the second option with a "~2 s" warning.
    * `Unsuitable` → local hidden behind a `Confirm` showing the
      specific failed gate (e.g. "only 2 physical cores; minimum is 4").
    * Local-model menu narrowed to the tier's recommended model + one
      safer fallback (no longer shows whisper-medium on a 4-core box).
* **H16** — `fono doctor` now prints the hardware snapshot and tier
  alongside the existing factory probes, so users see at a glance
  whether their config matches their hardware.
* **H17** — new `fono hwprobe [--json]` subcommand:

  ```
  cores : 10 physical / 12 logical  (AVX2)
  ram   : 15 GB total · disk free : 11 GB · linux/x86_64
  tier  : comfortable (recommends whisper-small)
  ```

  JSON output is consumable by packaging scripts and the bench crate.
* **H20** — `README.md` reflects v0.1.0-rc reality: default release
  bundles whisper.cpp, build-flavour matrix, `fono hwprobe` mention.
* **H24/H25** — plan persisted at
  `docs/plans/2026-04-25-fono-local-default-v1.md`; this status entry.

### Toolchain bumps

* `Cargo.toml:73` — `whisper-rs = "0.13" → "0.16"` (0.13.2 had an
  internal API/ABI mismatch with its sys crate; 0.16 is the current
  upstream and is what whisper.cpp tracks).
* `crates/fono-stt/src/whisper_local.rs:84-92` — adapt to the 0.16
  segment API (`get_segment(idx) -> Option<WhisperSegment>` +
  `to_str_lossy()`).

### Tasks intentionally deferred to v0.2 (all annotated in plan)

* **H8** — Real `LlamaLocal` implementation against `llama-cpp-2`.
  `llama-cpp-2 0.1.x` exposes a low-level API that needs several hundred
  lines of safe-wrapper code; the v0.1 slice ships local STT only with
  optional cloud LLM cleanup. New ADR
  `docs/decisions/0008-llama-local-deferred.md` captures the rationale.
* **H2/H3** — Release CI matrix (musl-slim + glibc-local-capable
  artifacts) — Phase 9 release work, separate from this slice.
* **H4** — OpenBLAS / Metal compile flags (would speed local inference
  another 2–3× on capable hosts) — opt-in v0.2 work.
* **H7/H14/H22** — In-wizard smoke bench + tier-profile bench in
  `fono-bench` — static rule + `fono doctor` are sufficient for v0.1.
* **H15/H18/H19** — Persisting tier in config + flipping
  `LlmBackend::default()` to Local + auto-migration — blocked on H8.
* **H23** — Wizard tier-decision unit test — covered by H21 tier tests
  + manual run; full `dialoguer` mock not worth the dependency.

## Build matrix (verified this session)

| Command | Result |
|---|---|
| `cargo build -p fono` (default features) | ✅ — bundles whisper.cpp |
| `cargo build -p fono --no-default-features --features tray` | (slim, cloud-only — covered by H1's feature graph) |
| `cargo test --workspace --lib --tests` | ✅ **67 tests pass** (54 unit + 13 hwcheck), 2 ignored (latency smoke) |
| `cargo clippy --workspace --no-deps -- -D warnings` | ✅ pedantic + nursery clean |
| `cargo run -p fono -- hwprobe` | ✅ classified host as `comfortable` (10c/16GB/AVX2) |
| `cargo run -p fono -- hwprobe --json` | ✅ structured snapshot + tier |

## Recommended next session

> Recommended next session: execute **Wave 3** of the revised strategic
> plan (Slice B1 — realtime cpal-callback push + first cloud streaming
> provider). Wave 2 landed in three DCO-signed commits:
> `76b9b08` (typed `ModelCapabilities` + split equivalence/accuracy
> thresholds), `87221a2` (per-asset `.sha256` sidecar verification +
> `--bin-dir` CLI flag), and the Thread-C CI gate commit (real-fixture
> `fono-bench equivalence` run against
> `docs/bench/baseline-comfortable-tiny-en.json` on every PR).
>
> Wave 3 concretely:
>
> 1. **Realtime cpal-callback push** (R4 / R10.4 of
>    `plans/2026-04-27-fono-interactive-v6.md`). Replace the
>    record-then-replay live path so the overlay paints text *as the
>    user speaks*. The `Pump` / `broadcast` plumbing landed in
>    Slice A; this is now scope-bounded.
> 2. **Groq streaming STT backend** (R8). Same auth path as the
>    existing Groq batch backend; the `StreamingStt` trait already
>    lives at `crates/fono-stt/src/streaming.rs`. Selectable via
>    `fono use stt groq` with `[interactive].enabled = true`.
> 3. **Equivalence harness cloud rows** (R18.12). Mocked-HTTP
>    recordings so the CI gate runs offline; extend
>    `docs/bench/baseline-comfortable-tiny-en.json` (or sibling) once
>    cloud rows produce stable verdicts.

### Earlier next-session notes (preserved for context)

1. Implement **H8** (`LlamaLocal` against `llama-cpp-2`) so the local
   path also covers LLM cleanup. Keep behind `llama-local` feature flag
   until proven; flip the wizard's local LLM offer back on once H9's
   integration test passes.
2. Land **L7+L8** (streaming LLM + progressive injection) — the next
   biggest perceived-latency win.
3. Pin real fixture SHA-256s via
   `crates/fono-bench/scripts/fetch-fixtures.sh` and commit
   `docs/bench/baseline-*.json` for CI regression gating.
4. Tag `v0.1.0` once `fono-bench` passes on the reference machine.
