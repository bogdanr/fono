# TTS Backend Benchmark (Piper / Kokoro / Supertonic) — EN + RO

## Objective

Produce a reproducible benchmark that compares Fono's three local TTS backends —
**Piper**, **Kokoro**, and **Supertonic** — across **English** and **Romanian**,
using deliberately difficult sentences and 3–4 voices per language, so the
maintainer can decide whether to change the default local TTS engine/voice.

The benchmark must:
- Measure **performance**: cold-start model load, warm synthesis time, real-time
  factor (RTF), and peak memory (RSS) per backend/voice.
- Capture **quality inputs**: write **raw, un-normalized** WAV files with
  descriptive filenames plus a listening index so the maintainer can play, compare,
  and rank (volume differences are preserved on purpose); add an **STT round-trip
  WER/CER** objective proxy to flag gross mispronunciation/dropped words.
- Record **decision context**: per-backend disk/download footprint, output sample
  rate, speech rate, run-to-run variance, and robustness/failure notes.
- Be a first-class, repeatable tool (a new `fono-bench tts` subcommand), not a
  one-off script.

## Key Facts (from codebase research)

- All three backends implement `TextToSpeech` (`crates/fono-tts/src/traits.rs:34`),
  batch-only locally: `synthesize(text, voice, lang) -> TtsAudio { pcm, sample_rate }`.
- Piper voices are per-language: EN `en_US-amy-medium`, `en_GB-alan-medium`; RO
  `ro_RO-mihai-medium` (`crates/fono-tts/voices/catalog.json`).
- Kokoro is **English only**, 6 voices (`crates/fono-tts/src/voices.rs:377`), 24 kHz.
- Supertonic is one shared pack, **EN + RO both supported**
  (`crates/fono-tts/src/supertonic/frontend.rs` `AVAILABLE_LANGS`), 10 speakers,
  deterministic seeded RNG (`crates/fono-tts/src/supertonic/engine.rs:42`).
- Engines are constructed by `load_engine` (Piper/Kokoro,
  `crates/fono-tts/src/local_router.rs:329`) and `build_supertonic`
  (`crates/fono-tts/src/factory.rs:140`). Bypass `LocalRouter` for direct,
  fair per-backend testing.
- Reuse `fono-bench` report/wav conventions; add a `tts` subcommand in
  `crates/fono-bench/src/bin/fono-bench.rs`.

## Test Matrix (assumptions — documented, not asked)

- **English**: Piper `en_US-amy-medium`; Kokoro `af_heart` (default) + `am_michael`
  (male); Supertonic speaker 0 (and 1 optional). → 3–4 voices.
- **Romanian**: Piper `ro_RO-mihai-medium`; Supertonic speakers 0, 1, 2 (Kokoro
  excluded — no RO). → up to 4 voices via Supertonic speaker variety.
- Voice list is CLI-overridable so the maintainer can swap picks without a rebuild.

## Difficult Sentence Set (fixtures)

Author a TOML fixture file (e.g. `tests/fixtures/tts/sentences.toml`), tagged by
language, covering categories chosen to expose backend differences:
- Numbers / dates / currency / time ("$1,299.99 on 3/14 at 09:45").
- Acronyms & initialisms (NASA, GPU, HTTP, SQL, i3/sway).
- Code / URLs / emails / paths (`cargo build --release`, `git@github.com`).
- Proper nouns & foreign names.
- Homographs / stress-sensitive words (EN: "read/lead/tear"; RO minimal pairs).
- Long compound sentences with nested clauses & heavy punctuation.
- Questions / exclamations (prosody/intonation stress).
- **Mixed-language**: English tech terms embedded in Romanian sentences (the
  coding-agent use case).
- **Romanian diacritics**: sentences dense in ă / â / î / ș / ț.

## Implementation Plan

- [ ] Task 1. **Add a `tts` subcommand to `fono-bench`.** Extend the `Cmd` enum
  and add a `TtsArgs` struct in `crates/fono-bench/src/bin/fono-bench.rs`, gated
  on the `tts-local` feature. Args: `--languages`, `--backends`
  (piper,kokoro,supertonic), `--voices` (per-backend overrides), `--fixtures`,
  `--iterations`, `--warmup`, `--voices-dir` (cache dir), `--out` (report JSON),
  `--wav-dir`, `--threads`, `--seed`, `--stt`/`--stt-model` (round-trip),
  `--machine-label`. Rationale: reuses the established bench CLI/report/wav
  plumbing and feature gating.

- [ ] Task 2. **Fixture format + loader.** Add a TOML fixture schema
  (id, language, category, text) and a loader in `crates/fono-bench/src`
  (mirroring `polish_text`/`assistant_factual` manifest loaders). Author the
  difficult-sentence set above. Rationale: editable, versioned, language-tagged
  fixtures matching existing bench conventions.

- [ ] Task 3. **Direct per-backend engine construction.** In the harness, build
  each backend directly — Piper/Kokoro via `voices::by_name` + `load_engine`,
  Supertonic via `supertonic::engine::SupertonicLocal::load` — resolving assets
  from the voices cache. **Do not** route through `LocalRouter` (it auto-routes
  EN→Kokoro). Fail with an actionable error when a pack/voice is not cached,
  pointing at daemon startup download. Rationale: guarantees each measurement is
  the intended backend/voice.

- [ ] Task 4. **Asset pre-flight / ensure step.** Before measuring, verify (or
  optionally trigger) the presence of each backend's assets via the existing
  `ensure_voice` / Supertonic pack ensure path, and record each backend's
  **on-disk footprint** (sum of its cached asset sizes). Rationale: footprint is
  a decision input; missing assets must be a clear, early failure.

- [ ] Task 5. **Performance measurement.** For each (backend, voice, sentence):
  measure **cold-start load time** (first engine construction, discarded from
  synth stats), then run `--warmup` discarded synths, then `--iterations` timed
  synths. Record wall-clock synth time, output sample count, derived
  **audio_duration** and **RTF = synth_time / audio_duration**, and p50/p95 across
  iterations. Pin `--threads` (ORT intra-op) and `--seed` for comparability.
  Rationale: RTF + cold-start are the decision-critical latency metrics.

- [ ] Task 6. **Memory measurement with per-engine isolation.** Attribute peak
  RSS to a single backend by running **one backend per process invocation**
  (harness orchestrates subprocess-per-backend, or documents running the
  subcommand once per `--backends` value), reading peak RSS from
  `/proc/self/status` `VmHWM` (Linux, no new dependency). Record baseline RSS
  before load and peak after load+synth; report the delta. Rationale: loading all
  engines in one process conflates memory; isolation gives clean per-backend
  numbers.

- [ ] Task 7. **Raw WAV output + descriptive naming + listening index.** Write
  each utterance to a WAV via `fono_bench::wav` / `fono_core::wav`, **without any
  loudness normalization** — the maintainer explicitly wants to hear each backend's
  native volume as part of the assessment. Name files descriptively
  (e.g. `<lang>__<backend>__<voice>__<sentence-id>.wav`) so clips are easy to find
  and compare; no blind/anonymized naming (label bias is a non-issue here).
  Provide a grouped listening **index** (Markdown or minimal HTML) that lets the
  maintainer play clips per sentence across backends. Rationale: the human quality
  assessment is the primary quality signal, and volume is part of that judgement.

- [ ] Task 8. **STT round-trip objective quality proxy.** Optionally
  (`--stt local|groq|...`) transcribe each generated WAV back through an existing
  STT backend and compute **WER + CER** vs the input text using
  `fono_bench::wer`. Report per-utterance and aggregated per (backend, language,
  category). Rationale: an objective anchor that flags dropped/garbled words and
  gross mispronunciation to corroborate subjective ranking; reuses existing WER
  tooling and STT features already in the graph.

- [ ] Task 9. **Determinism & robustness checks.** For Supertonic, run one
  sentence twice with the same seed and assert byte-identical PCM (records the
  deterministic guarantee). Capture and log any synth errors, empty outputs, or
  anomalous durations per backend without aborting the whole run. Rationale:
  robustness on hard inputs is part of the default decision.

- [ ] Task 10. **Structured JSON report + human summary.** Emit a
  `serde`-serializable report (schema-versioned, like the other bench reports)
  containing, per (backend, language, voice, sentence): synth p50/p95, RTF,
  cold-start, sample rate, audio duration/speech rate, RSS delta, disk footprint,
  optional WER/CER, and error/robustness notes; plus a machine label + build/env
  metadata block. Print a compact human-readable table and write JSON to `--out`.
  Rationale: comparable, archivable results and an at-a-glance verdict.

- [ ] Task 11. **Documentation + invocation recipe.** Add a short usage section
  (per-backend invocation for clean memory isolation, EN and RO runs, the listen
  index, how to interpret RTF/WER) to the bench docs/README. Rationale: makes the
  benchmark repeatable by the maintainer and future sessions.

- [ ] Task 12. **Gates.** Run `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`, and (since the shipped artefact is
  unaffected by a bench-only, feature-gated subcommand) confirm no new dependency
  is added to the shipped `fono` graph. Rationale: project pre-commit gate.

## Verification Criteria

- Running `fono-bench tts --languages en --backends piper,kokoro,supertonic`
  produces timed synths, WAVs, and a JSON report for all three English backends.
- Running `--languages ro --backends piper,supertonic` produces Romanian results;
  the harness cleanly reports Kokoro as unsupported for Romanian rather than
  silently substituting a voice.
- Report contains per (backend, voice, sentence): synth p50/p95, RTF, cold-start,
  sample rate, audio duration, RSS delta, and disk footprint.
- WAV files are written **raw (un-normalized)** with descriptive filenames; a
  listening index groups clips by sentence across backends.
- With `--stt` set, WER/CER is computed per utterance and aggregated per backend.
- Supertonic determinism check passes (identical PCM for identical seed).
- Each backend's memory number is attributable to that backend alone
  (subprocess-per-backend isolation documented and working).
- All four gates in Task 12 pass; the shipped binary dependency graph is unchanged.

## Potential Risks and Mitigations

1. **Kokoro has no Romanian voice, making the matrix asymmetric.**
   Mitigation: explicitly exclude Kokoro from the RO matrix and surface it as an
   informational line; compare RO on Piper vs Supertonic only.
2. **Loading all engines in one process conflates peak memory.**
   Mitigation: subprocess-per-backend isolation (Task 6); document one invocation
   per `--backends` value for clean RSS attribution.
3. **STT round-trip introduces its own errors, misattributed to TTS quality.**
   Mitigation: treat WER/CER as a relative anchor across backends (same STT for
   all), not an absolute quality score; keep human listening as primary.
4. **Assets not present in the voices cache cause confusing failures.**
   Mitigation: explicit pre-flight ensure/verify step with actionable errors
   (Task 4) pointing at daemon startup download.
5. **Cold-start numbers polluted by OS disk cache warmth.**
   Mitigation: report cold-start as best-effort, note cache state, and separate it
   from warm p50/p95; optionally document a cache-drop step the maintainer can run.
6. **Peak-RSS via `/proc` is Linux-only.**
   Mitigation: acceptable — the benchmark is a maintainer tool run on Linux;
   guard the read and degrade to "unavailable" elsewhere.

## Alternatives Considered

1. **Shell script over the `/v1/audio/speech` HTTP endpoint** (`Tts::resolve_speech_route`).
   Trade-off: simplest to stand up, but routes through daemon config, can't isolate
   per-backend memory or cold-start, and adds HTTP/encoding noise to timings.
   Rejected as the primary tool; could be a convenience wrapper later.
2. **Standalone example under `crates/fono-tts/examples/`.**
   Trade-off: closest to the engines, but reimplements report/wav/WER plumbing that
   `fono-bench` already provides. Rejected to avoid duplication.
3. **`criterion`-based microbenchmark.**
   Trade-off: great for statistical timing, but awkward for producing listenable
   WAVs, memory attribution, and STT round-trip; wrong tool for a quality-oriented
   comparison. Rejected.
