# Supertonic 3 Local TTS Engine for Fono

## Objective

Integrate Supertonic 3 (99M-param, 44.1 kHz, 31-language, 10-speaker ONNX TTS with
expressive tags) as a first-class engine in Fono's local voice stack (`fono-tts`,
feature `tts-local`), running on the already-shipped statically-linked `ort` runtime —
no new native dependencies, no Python, weights downloaded at runtime. In the same
slice, amend Fono's model-license policy so OpenRAIL-M–class licenses are eligible
as defaults, and publish the feature on the roadmap.

Reference implementations available locally:
- sherpa-onnx C++ pipeline: `sherpa-onnx/sherpa-onnx/csrc/offline-tts-supertonic-impl.cc`,
  `offline-tts-supertonic-model.cc`, `offline-tts-supertonic-unicode-processor.cc` (all in
  `/mnt/live/memory/data/Work/tts-testing/`)
- NFKD table + generator (our own upstream PR #3750): `offline-tts-supertonic-nfkd-table.h`,
  `sherpa-onnx/scripts/supertonic/generate_nfkd_table.py`
- Working Python oracle + test corpus: `test_supertonic.py`; findings in `TTS_ROMANIAN_LEARNINGS.md`
- Model pack (int8, ~139 MB): `models/sherpa-onnx-supertonic-3-tts-int8-2026-05-11/`
  (`text_encoder` 35 MB, `vector_estimator` 75 MB, `vocoder` 25 MB, `duration_predictor`
  3.6 MB, `voice.bin` 508 KB, `unicode_indexer.bin` 256 KB, `tts.json` 12 KB)

## Assumptions (documented per planning policy)

- **License stance (per maintainer decision this session):** OpenRAIL-M–class licenses
  (free use/modification/redistribution/commercial use; behavioral-use restrictions only)
  are acceptable for *default* models. This is a policy change from ADR 0004's current
  "no extra use restrictions" default bar and is codified via ADR amendment (Task 1).
- **GPU:** Fono's `gpu` (Vulkan) artefact accelerates only the ggml stack; `ort` has no
  Vulkan EP, and the minimal static `libonnxruntime.a` is CPU-only by design (ADR 0032).
  Supertonic runs on CPU in *both* the `cpu` and `gpu` (Vulkan) builds — same as
  Piper/Kokoro. At RTF ≈ 0.2–0.3 on 4 CPU threads this meets the sentence-streaming
  latency budget with margin; no ort GPU EP work is in scope.
- **Rollout:** ship first as an opt-in engine (`fono use tts supertonic` /
  `[tts.local].engine = "supertonic"`), then promote to the default non-English engine
  (superseding per-language Piper voices) in a follow-up release once quality/latency
  is validated on low-end hardware. Kokoro remains the English default until an
  explicit A/B decides otherwise.
- Model hosting: the int8 pack is mirrored on Fono's existing voice mirror
  (the `fono-download` infrastructure used by Piper/Kokoro/espeak dicts), repacked as
  individually-checksummed files, not a tarball.

## Implementation Plan

### Slice 0 — Policy and roadmap (docs-only commit)

- [ ] Task 0.1. Amend `docs/decisions/0004-default-models.md` (or add a new ADR
      cross-referenced from 0004) defining a three-tier license policy:
      (a) OSI/GPL-compatible → default-eligible, unchanged; (b) **RAIL-class
      behavioral-restriction licenses (OpenRAIL-M etc.) → default-eligible**, provided
      restrictions are behavioral-only (no commercial limits, no field-of-use bans, no
      MAU caps), with the license linked in the model's download notice; (c) commercial /
      field restrictions (Llama Community License, CC-NC) → opt-in only, unchanged.
      Record the rationale: weights are runtime-downloaded data, never linked into or
      bundled with the GPL binary; RAIL restrictions largely restate applicable law.
      Note Supertonic 3 (code MIT, weights OpenRAIL-M) as the motivating case.
- [ ] Task 0.2. Add a Supertonic 3 entry to `ROADMAP.md` under **Up next** following the
      house style (blockquote hook + short prose), e.g. heading "Natural local
      voices in 31 languages": one 99M ONNX model replacing per-language voice downloads,
      44.1 kHz output, 10 speakers, expressive tags (`<laugh>`, `<breath>`) for the
      assistant, running on the ONNX runtime already in the binary, CPU-only and offline.
      Also add it to the "Up next" cell of the summary table at the top of the file.
      Batch Tasks 0.1–0.2 into a single signed-off commit per Fono's doc-commit rule.

### Slice 1 — Model distribution

- [x] Task 1.1. Repack `sherpa-onnx-supertonic-3-tts-int8-2026-05-11` for the Fono voice
      mirror: the four graphs plus `tts.json`, `unicode_indexer.bin`, `voice.bin`,
      and the upstream `LICENSE`; per-file SHA-256 in the mirror manifest, following the
      existing Piper/Kokoro layout consumed by `fono-download`.
      **CORRECTION (2026-07-14):** the four `.onnx` graphs must be converted to `.ort`
      first — Fono's minimal onnxruntime (ADR 0032/0033) loads **only** `.ort`
      flatbuffers, never plain `.onnx`. The pack descriptor (`crates/fono-tts/src/`
      `supertonic/mod.rs`) names them `*.ort` and leaves their pins `UNPINNED` until
      `scripts/gen-ort-models.sh` converts them and they are uploaded (the wake
      `hey_fono` precedent); the conversion is what feeds Slice 3's ops-config. The
      three format-stable files (`tts.json`, `voice.bin`, `unicode_indexer.bin`) are
      pinned now with real SHA-256s from the upstream int8 pack.
- [x] Task 1.2. Wire the pack into the download layer with a **notice-on-download** step
      (mirroring the wake-word community-model notice pattern from v0.12.0): one-line
      "Weights licensed OpenRAIL-M (behavioral-use restrictions) — <link>" shown at fetch
      time and recorded in the model metadata.
- [x] Task 1.3. Decide and document eviction/coexistence semantics: the ~139 MB pack is
      shared across all 31 languages and 10 speakers (one download, not per-voice), and
      supersedes the need for new Piper per-language downloads when the engine is active.

### Slice 2 — Engine core (`crates/fono-tts/src/supertonic/`)

- [ ] Task 2.1. `config.rs`: deserialize `tts.json` (serde) — the fields the pipeline
      actually needs: `ae.sample_rate` (44100), `ae.base_chunk_size` (512),
      `ttl.latent_dim` (24), `ttl.chunk_compress_factor` (6). Validate `n_langs: 0`
      (char-level, language-agnostic acoustic model).
- [ ] Task 2.2. `style.rs`: parse `voice.bin` — 6×i64 header (ttl shape [S,·,·] + dp
      shape [S,·,·]) followed by two f32 payloads; port the overflow/size validation from
      `ParseVoiceStyleFromBinary` (sherpa impl lines 80–161). Expose per-sid slice views
      (ttl `[1,·,·]`, dp `[1,·,·]`) and `num_speakers()`.
- [ ] Task 2.3. `frontend.rs`: text → token ids.
      (a) Expressive-tag pass: port the tag→reserved-codepoint substitution from
      `offline-tts-supertonic-unicode-processor.cc` (`ReplaceString` mappings) verbatim,
      so `<laugh>`, `<breath>`, and the other upstream tags survive normalization.
      (b) NFKD decomposition: generate a Rust `const` BMP table with an adapted
      `generate_nfkd_table.py` (build-time-committed artifact, ~few tens of KB) plus the
      algorithmic Hangul decomposition — identical semantics to our sherpa PR #3750.
      Do NOT skip this: precomposed diacritics (ă/â/î/ș/ț, č, ą, …) otherwise map to
      -1 and are silently dropped.
      (c) `unicode_indexer.bin` lookup: flat `int32[65536]` BMP table; drop -1 entries;
      build `text_ids` + the `[1,1,len]` text mask.
      (d) Language gate: validate against the 31-language allowlist (from the sherpa
      impl's `kSupertonicAvailableLangs`); chunking limits `max_len` 300 (120 for ko/ja).
- [ ] Task 2.4. `chunker.rs`: port `ChunkText` sentence/length chunking and the
      inter-chunk silence concat (default 0.3 s) from `ProcessChunksAndConcatenate`.
- [ ] Task 2.5. `engine.rs`: the four `ort::Session`s (same
      `GraphOptimizationLevel`/session pattern as `kokoro.rs`) and the inference pipeline
      ported from `Process()` (sherpa impl lines 281–545):
      duration predictor (text_ids, style_dp, text_mask → scalar duration; apply
      `speed`, floor 0.1 s) → text encoder (text_ids, style_ttl, text_mask → text_emb) →
      latent-length math (`chunk = base_chunk_size × chunk_compress_factor`, cap 10 000)
      → Gaussian init of `xt` masked by the latent mask → flow-matching loop
      (`num_steps`, default 5–8: vector_estimator(noisy_latent, step, text_emb,
      style_ttl, latent_mask, text_mask, total_steps)) → vocoder(latent → f32 wav,
      trimmed to predicted length) at 44 100 Hz. Seeded RNG parameter for reproducible
      tests (port `NormalDataGenerator` semantics: N(0,1), fixed seed).
- [ ] Task 2.6. Implement `TextToSpeech` (`traits.rs`) for `SupertonicLocal`, emitting
      chunk-by-chunk audio through the existing streaming path so long assistant replies
      start playing after the first chunk; report native rate 44 100 for the playback
      warmup hint.

### Slice 3 — Runtime build, size gate, and binary budget

- [ ] Task 3.1. Extend the minimal-ORT ops config (`ops.config`, per ADR 0032 /
      `docs/binary-size.md`) with the operator/type union of the four int8 Supertonic
      graphs (extract with onnxruntime's `create_reduced_build_config.py` against the
      model pack); rebuild the pinned `libonnxruntime.a` via
      `scripts/build-onnxruntime-minimal.sh` and re-pin in `scripts/fetch-onnxruntime.sh`.
- [ ] Task 3.2. Run `./tests/check.sh --size-budget` and record the delta. Expected
      growth: modest (extra ORT kernels + ~50 KB Rust/NFKD table). If the `cpu` budget
      (25 MiB) is exceeded, trim first (dedupe kernels, verify int8 op reuse with
      Piper/Kokoro ops); only with explicit maintainer sign-off bump the budget row in
      `ci.yml` + ADR 0022 (hard cap 28 MiB), in lockstep.
- [ ] Task 3.3. Confirm no new crate enters the dependency graph (NFKD is a generated
      table, not the `unicode-normalization` crate; serde/ort/anyhow all pre-existing).
      No `deny.toml` change should be needed — verify.

### Slice 4 — Catalog, routing, config, UX

- [ ] Task 4.1. `voices.rs` catalog: register the pack as one model with 10 named voices
      (friendly gendered labels mapped to `sid` 0–9, consistent with the per-program
      voice picker), each advertising the full 31-language set.
- [ ] Task 4.2. `local_router.rs`: teach the router a Supertonic arm — when the engine is
      enabled, non-English (and optionally English) utterances route to `SupertonicLocal`
      with the detected `lang` hint threaded through; Piper remains the fallback for the
      7 Piper languages Supertonic lacks; explicit `[tts.local].voice` pin semantics
      unchanged.
- [ ] Task 4.3. Config surface: `[tts.local].engine = "auto" | "piper" | "kokoro" |
      "supertonic"` (default `auto` = current behavior until promotion), plus
      `[tts.local.supertonic]` keys: `voice` (sid label), `num_steps` (default 8),
      `speed` (default 1.0), `silence_duration`. Expose in the web settings UI section
      and `fono use tts supertonic`.
- [ ] Task 4.4. Expressive tags policy: allow tags only on the assistant/`fono.speak`
      path (append a one-line capability note to the assistant system prompt so the LLM
      may emit `<laugh>`/`<breath>` sparingly); strip unknown angle-tags defensively in
      the frontend so stray markup never leaks into audio.
- [ ] Task 4.5. Docs: `docs/providers.md` TTS row (+ language/speaker matrix, OpenRAIL-M
      note), `docs/configuration.md` keys, `docs/binary-size.md` ops-config note.

### Slice 5 — Tests and quality gates

- [ ] Task 5.1. Unit tests: `voice.bin` parser (valid + malformed/oversized), NFKD
      frontend (the 8 cases from our sherpa gtest, incl. ă/ș/ț, Hangul, compat forms),
      indexer -1 drops, expressive-tag substitution, chunker boundaries (en vs ko/ja).
- [ ] Task 5.2. Deterministic E2E test (seeded RNG, `#[ignore]`d unless the model pack is
      present, following the existing local-model test pattern): synthesize the Romanian
      diacritic sentence and an English sentence; assert nonzero duration, 44 100 Hz,
      spectral flatness < 0.05 (clean-speech threshold from `TTS_ROMANIAN_LEARNINGS.md` §4).
- [ ] Task 5.3. Cross-check against the Python/sherpa oracle: same text + seed →
      comparable duration and audible parity by ear; document RTF on the dev machine and
      on a 4-core reference CPU (the ADR 0004 hardware floor).
- [ ] Task 5.4. Full pre-commit gate (fmt, clippy `-D warnings`, tests) and the size
      gate; update `docs/status.md` session log. Do not push without explicit instruction.

## Verification Criteria

- Romanian sentence "Acesta este un test de sinteză vocală…" renders all diacritic
  sounds (ASR round-trip diff shows no dropped syllables — the §6 method).
- `<laugh>` / `<breath>` tags produce audible non-speech events, and unknown tags are
  stripped silently.
- RTF < 0.5 on a 4-core CPU at `num_steps = 8`; first audio chunk of a multi-sentence
  assistant reply starts before full synthesis completes.
- `./tests/check.sh --size-budget` green (or a signed-off, documented budget bump ≤ 28 MiB);
  `NEEDED` allowlist still exactly four entries.
- One ~139 MB download serves all 31 languages / 10 speakers; download shows the
  OpenRAIL-M notice; nothing OpenRAIL-M-licensed is embedded in the binary.
- `fono use tts supertonic` hot-reloads without restart; `fono doctor` reports the engine.

## Potential Risks and Mitigations

1. **Minimal-ORT build lacks ops/types used by the int8 Supertonic graphs** (e.g.
   DynamicQuantizeLinear variants), inflating the runtime more than expected.
   Mitigation: generate the reduced-ops config directly from the four graphs before any
   Rust work; measure the `libonnxruntime.a` delta first; int8 kernels largely overlap
   with existing quantized-model support.
2. **Binary growth breaches the 25 MiB cpu budget.** Mitigation: size gate run early
   (Slice 3 before Slice 4); trim kernel/type combinations; documented sign-off path to
   28 MiB hard cap exists but is last resort.
3. **Expressive-tag mapping mismatch** (tags rendered as spelled-out text or dropped).
   Mitigation: port the sherpa `ReplaceString` tag table verbatim and lock it with unit
   tests against the oracle's output length.
4. **Latency on low-end CPUs** (flow-matching loop × num_steps). Mitigation: `num_steps`
   configurable (5 already acceptable per sherpa default); chunk-level streaming hides
   tail latency; keep opt-in until validated on the 4-core floor.
5. **License-policy blowback** (users objecting to a non-OSI default). Mitigation: ADR
   makes the tiering explicit; notice-on-download; weights never bundled; trivially
   revertible to opt-in since Piper/Kokoro remain in the binary.
6. **Upstream model revisions** changing tensor shapes/IO names. Mitigation: pin the
   exact dated pack on Fono's own mirror with checksums; `tts.json` validation fails
   loudly on contract drift.

## Alternative Approaches

1. **Link sherpa-onnx instead of reimplementing:** rejected — sherpa-onnx bundles its own
   full ONNX runtime; ADR 0012 already rejected exactly this for the wake-word engine on
   size-gate grounds.
2. **fp32/fp16 pack instead of int8:** better fidelity headroom but ~4× the download and
   slower CPU inference; int8 output already measures clean (flatness 0.037 baseline).
   Could be offered later as an opt-in "quality" pack via the same catalog.
3. **Keep Supertonic opt-in permanently:** lowest risk, but forfeits the main win
   (one download replacing dozens of per-language Piper voices + espeak dicts). The
   staged opt-in → promote path captures both.
