# Parakeet-TDT v3 STT Re-evaluation + CrispASR-Inspired Follow-ups

## Objective

Re-evaluate NVIDIA Parakeet-TDT 0.6b **v3** as a local STT engine for Fono, superseding
ADR 0004's exclusion of Parakeet ("~600 MB quantised and English-only" — true for
v1/v2, no longer for v3), and land the small CrispASR-inspired documentation/roadmap
updates agreed on 2026-07-12. This is a **spike-first** plan: Phase A produces
measurements and a go/no-go memo; Phase B (integration) executes only on a "go".

## Why re-evaluate now

- **v3 changed the facts:** `nvidia/parakeet-tdt-0.6b-v3` is multilingual — 25
  European languages *including Romanian* — with automatic language detection,
  licensed **CC-BY-4.0** (OSI-compatible attribution license, default-eligible under
  ADR 0004). It leads the Open ASR leaderboard in its class, and TDT decoding is
  substantially faster than Whisper's autoregressive decoder — directly serving
  Fono's latency-first, en/ro-primary user base.
- **Feasibility is de-risked twice over:** CrispASR
  (<https://github.com/CrispStrobe/CrispASR>) runs it on ggml on CPU, and sherpa-onnx
  publishes ONNX (incl. int8) exports — meaning it can run on the minimal static
  `ort` runtime Fono already ships (ADR 0032), the same platform the Supertonic plan
  (`plans/2026-07-12-supertonic3-local-tts-engine-v1.md`) builds on.
- ADR 0004 already anticipated a transducer on ort (the Zipformer streaming-STT entry
  in the voice-stack list). Parakeet-TDT is a FastConformer transducer — the same
  architectural family — and TDT greedy decode is a small, well-documented loop
  (encoder → decoder (prediction net) → joiner sessions + label/duration argmax).

## Assumptions

- Integration target, if approved, is a **new opt-in engine on the shared minimal
  `ort` runtime** (Rust TDT decode loop over encoder/decoder/joiner ONNX sessions) —
  NOT linking sherpa-onnx (rejected for size in ADR 0012) and NOT adopting the
  CrispASR fork (it would duplicate/replace the whisper.cpp stack Fono already links;
  CrispASR is used as inspiration and a feasibility reference only).
- Whisper remains the default STT until benchmarks justify a promotion decision;
  Parakeet would first target a "high-accuracy European-language" opt-in tier.
- The spike uses external harnesses (sherpa-onnx Python/CLI, the CrispASR binary) on
  a scratch machine — zero Fono code changes in Phase A.

## Phase A — Investigation spike (no Fono changes)

- [ ] Task A1. Verify model facts from primary sources: v3 language list (confirm
      `ro`), license text (CC-BY-4.0, no rider), exact artifact sizes for fp16 and
      int8 ONNX exports (sherpa-onnx release packs), memory footprint at load.
- [ ] Task A2. Benchmark WER/CER vs Fono's current local ladder (whisper `small`
      multilingual default, `large-v3-turbo` upper tier) on:
      (a) Fono's existing multilingual release-gate fixtures (en, ro, es, fr),
      (b) a fresh Romanian diacritic-heavy dictation-style set.
      Harness: sherpa-onnx offline-transducer CLI; cross-check one configuration on
      CrispASR to catch harness-specific artefacts.
- [ ] Task A3. Benchmark latency: RTF on the 4-core CPU reference floor (ADR 0004)
      and on the dev machine, int8 vs fp16; measure cold-load time. Compare against
      whisper small/turbo numbers from the existing calibration matrix
      (`calibration/`).
- [ ] Task A4. Assess ort compatibility: extract the op/type set of the v3 ONNX
      graphs (`create_reduced_build_config.py`), diff against Fono's current minimal
      `ops.config` plus the planned Supertonic additions; estimate the
      `libonnxruntime.a` growth. Scope batch mode first (streaming/cache variants add
      ops).
- [ ] Task A5. Document the TDT greedy-decode I/O contract from the sherpa-onnx
      offline-transducer implementation (tensor names, shapes, duration-head
      semantics, token-table/BPE vocab loading) as the port reference. Verify that
      punctuation + casing come out of the model natively, per language.
- [ ] Task A6. Write the go/no-go memo into this file: decision matrix of accuracy
      delta vs whisper-small and turbo, RTF delta, download size, ort growth, port
      effort. Suggested "go" bar: WER improves ≥ 20 % relative on en+ro at
      RTF ≤ whisper-small, with ort growth inside the size budget.

## Phase B — Integration (gated on a Phase A "go")

- [ ] Task B1. `fono-stt`: new `parakeet` module behind an `stt-local-onnx` feature —
      mel/fbank frontend (port the exact featurizer params from the export), three
      `ort` sessions, TDT greedy decode, token-table loader. Mirrors the
      `kokoro.rs` / planned-Supertonic session pattern; shares the ort runtime.
- [ ] Task B2. Language handling: v3 auto-detects; constrain to `general.languages`
      where possible and thread the detected language into the existing polish/TTS
      lang-hint plumbing.
- [ ] Task B3. Catalog + wizard: add Parakeet v3 (int8 default, fp16 optional) as the
      high-accuracy European-language tier; runtime download from Fono's mirror with
      the CC-BY-4.0 attribution recorded in model metadata.
- [ ] Task B4. Ops-config rebuild + `./tests/check.sh --size-budget`; coordinate with
      the Supertonic slice so the minimal ORT is rebuilt once with both op unions.
- [ ] Task B5. Quality gates: wire Parakeet into the existing STT fixture tests; full
      pre-commit gate; `docs/providers.md` + `docs/status.md` updates.

## CrispASR-inspired documentation follow-ups

- [x] Task C1. ADR 0004 amendment (same doc commit as the OpenRAIL-M license-tier
      change from the Supertonic plan, Slice 0): replace the Parakeet exclusion
      bullet — v1/v2 rationale obsolete, v3 under re-evaluation per this plan.
- [ ] Task C2. `docs/providers.md`: add a short "self-hosted engines" note listing
      CrispASR's HTTP server as an opt-in STT endpoint reachable via the
      OpenAI-compatible base-URL override — **only after confirming** its
      `/v1/audio/transcriptions` compatibility hands-on (mark experimental).
- [x] Task C3. `ROADMAP.md` Personal-vocabulary section: note a later decoder-level
      phase — hotword/contextual biasing (CTC/TDT biasing and Whisper
      `initial_prompt` injection, as demonstrated by CrispASR `--hotwords`) — layered
      under the deterministic substitution pass, not replacing it.

## Verification criteria

- Phase A memo contains reproducible WER/RTF tables for en+ro vs whisper-small and
  large-v3-turbo, artifact sizes, the ort op-diff, and an explicit go/no-go.
- No Fono binary/code change ships before the go decision; the doc commit (C1/C3 +
  Supertonic Slice 0) is self-contained and signed off.
- If Phase B proceeds: size gate green, four-entry NEEDED allowlist intact, fixture
  quality gates pass, Parakeet selectable via `fono use stt` and the wizard.

## Risks

1. **Featurizer mismatch** (NeMo fbank vs a naive mel frontend) silently degrading
   accuracy — mitigate by diff-testing frame outputs against the sherpa-onnx harness.
2. **ort op growth** beyond budget when combined with Supertonic — measure the union
   early (Task A4); int8 transducer kernels overlap heavily with existing quantized
   ops.
3. **Model size (~600 MB int8)** pushes the download story — keep it an explicit
   opt-in tier; the wizard only offers it on capable hardware.
4. **25-language ceiling** (no zh/ja/ko/ar/hi) — Parakeet complements, never
   replaces, multilingual Whisper; the router/wizard must keep Whisper for
   out-of-set languages.
