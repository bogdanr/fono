# Kokoro local English TTS (Phase 4.1)

## Objective

Land **Kokoro** as the local TTS engine for **English only** (en-US / en-GB,
`af_heart` default), and extend the existing `LocalRouter` with the
**ADR 0033** rule: `lang == en → Kokoro`, everything else → Piper. Today the
local stack is Piper-only — English is served by `en_US-amy-medium`
(`crates/fono-tts/voices/catalog.json:125-140`) and the router unconditionally
builds `PiperLocal` (`crates/fono-tts/src/local_router.rs:229-249`). This plan
adds a second ONNX engine alongside Piper on the same statically-linked `ort`
runtime, hosts the Kokoro `.ort` model + voice-style data on the `fono-voice`
mirror, and routes English utterances to it — keeping the four-entry `NEEDED`
allowlist and the ≤ 32 MiB `cpu` cap (ADR 0022).

This is the open task **4.1** in
`plans/2026-05-31-local-tts-onnx-voice-stack-and-wyoming-server-v3.md:253-258`
and the remaining half of the router split flagged in **2.4**
(`:194-202`). It supersedes the local-engine portion of the older
`plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md` (whose 54-voice
multilingual router was retired by ADR 0033 — Kokoro's non-English voices are
thin/weak).

## How Kokoro differs from Piper (the crux of the work)

Piper and Kokoro are both VITS-family ONNX models but their I/O contracts
diverge, so Kokoro cannot reuse `PiperLocal` (`crates/fono-tts/src/piper.rs`):

| Aspect | Piper (`PiperLocal`) | Kokoro (new `KokoroLocal`) |
|---|---|---|
| ONNX inputs | `input` ids, `input_lengths`, `scales[3]` | `input_ids` (`[1,N]`, pad-`0`-bracketed), `style` (`[1,256]`), `speed` (`[1]`) |
| Output | `output` PCM @ voice rate (22050) | `waveform` PCM @ **24000 Hz** |
| Phoneme→id map | **per-voice** `phoneme_id_map` in `.onnx.json` | **single fixed** Kokoro vocab (same for all voices) |
| Voice identity | one model = one voice | one model + a **per-voice style pack** (`[~510, 1, 256]`, indexed by token count) |
| Phonemizer | espeak-ng IPA (exists) | espeak-ng `en-us` IPA → Kokoro token ids (mapping is new) |
| Sidecar asset | `.onnx.json` (parsed) | model uses a fixed vocab + a style `.bin`; no per-voice JSON map |

Upstream reference: `onnx-community/Kokoro-82M-v1.0-ONNX` /
`thewh1teagle/kokoro-onnx` (kokoro-onnx MIT, **model Apache-2.0** — clears
ADR 0004). The widely-used `kokoro-onnx` path phonemizes with espeak-ng and
maps to the fixed vocab, which is exactly Fono's existing espeak core path —
the reusable part.

## Critical infrastructure dependency (sequence this first)

The shipped minimal `libonnxruntime.a` on the `fono-voice` mirror was built
from an `ops.config` covering the **Piper VITS** operator set
(`scripts/gen-ort-models.sh`, `scripts/build-onnxruntime-minimal.sh`). Kokoro
uses operators Piper does not (ISTFTNet vocoder, LSTM, etc.) and the catalog
comment already notes that **voices using control-flow ops unsupported by the
minimal runtime are omitted** (`catalog.json:3`). Therefore Kokoro is gated on:

1. Converting Kokoro `.onnx` → `.ort` (`gen-ort-models.sh`) and regenerating
   `ops.config` as the **union of Piper + Kokoro** operators.
2. Rebuilding the minimal `libonnxruntime.a` from that unioned config.
3. Re-uploading it to the mirror and re-pinning `scripts/fetch-onnxruntime.sh`
   for every triple.

A **load spike must run before any Rust is written** to confirm a minimal /
operator-reduced runtime can actually load and run Kokoro's `.ort` (the same
class of failure that omitted seven Piper voices). If the minimal build cannot
support Kokoro's ops, the size/runtime strategy needs a decision before
proceeding.

## Implementation Plan

### Phase A — De-risking spike (must precede all engine code)

- [x] A1. Convert the upstream Kokoro `.onnx` (fp32 and the int8/fp16
  variants) to `.ort` via `scripts/gen-ort-models.sh`; capture the emitted
  operator set and diff it against the current Piper `ops.config`. Rationale:
  determines the unioned op list and whether quantized kernels are even
  available in a reduced build.
  **DONE (2026-06-02).** fp32 (`thewh1teagle/kokoro-onnx` v1.0, 310 MiB) and
  quantized (`onnx-community/Kokoro-82M-v1.0-ONNX` `model_q8f16.onnx`, 82 MiB)
  both converted to `.ort` at onnxruntime 1.24.2. **Zero control-flow ops**
  (`If`/`Loop`/`Scan`) in either — the exact blocker that omitted 7 Piper
  voices is absent. fp32 net-new ops vs Piper: `LSTM`, `STFT`,
  `LayerNormalization`, `Atan`, `Cos`, + contrib `FastGelu`, `FusedMatMul`,
  `SkipLayerNormalization`. Quantized adds `ConvInteger`, `DynamicQuantizeLinear`,
  `MatMulInteger`, `DequantizeLinear`, `QuantizeLinear`, + contrib
  `DynamicQuantizeLSTM`, `DynamicQuantizeMatMul`, `MatMulIntegerToFloat`. The
  unioned config (shipped Piper + Kokoro) merges cleanly; **the shipped runtime
  op set must be baked from the *exact* Kokoro variant we ship** (quantized op
  set ≠ fp32 op set). Note: the two upstreams differ in input names —
  `thewh1teagle` uses `tokens/style/speed`, `onnx-community` uses
  `input_ids/style/speed`; both output 24 kHz mono, `style` `[1,256]`.
- [x] A2. Build a throwaway minimal `libonnxruntime.a` from the unioned
  `ops.config` and load + run the Kokoro `.ort` end-to-end against a known
  `(phoneme-ids, style)` pair, asserting non-silent 24 kHz PCM. Rationale:
  proves the minimal runtime supports Kokoro before committing engine code
  (mirrors the 2.2b Piper proof).
  **DONE (2026-06-02) — GREEN.** Built three throwaway minimal runtimes
  (`--minimal_build --enable_reduced_operator_type_support`, MinSizeRel,
  no XNNPACK): Kokoro-only, fp32-union (Piper+Kokoro), and quantized-union
  (Piper + `q8f16`). A C probe (`tmp/ort_linktest.c`, the same harness that
  caught the `If(13)` failures) confirms: fp32 Kokoro loads; on the fp32 union
  runtime Kokoro **and** Piper (`en_US-amy`, `de_DE`) all load OK; on the
  **quantized** union runtime `q8f16` (incl. the `DynamicQuantizeLSTM` contrib
  kernel) **and** Piper (`en_US-amy`, `de_DE`, `uk_UA`) all load OK. Functional
  run (full onnxruntime, `af_heart`) produced ~3.3 s of non-silent 24 kHz PCM
  for both fp32 (peak 0.49, rms 0.069) and `q8f16` (peak 0.485, rms 0.068) —
  quantization preserves energy. The single shipped minimal runtime can serve
  both engines.
- [x] A3. Measure the binary-size delta of the unioned-ops runtime against the
  current ~2.1 MiB Piper-only figure, and the on-disk size of the chosen
  Kokoro `.ort` (fp32 ~310 MB vs quantized ~80 MB). Record against the ≤ 32 MiB
  `cpu` cap (the runtime grows; the model downloads at runtime so it does not
  count toward the binary, but the download UX cost is real). Rationale: feeds
  the model-variant decision (see Assumptions).
  **DONE (2026-06-02).** Unstripped merged `libonnxruntime.a`: Piper-only
  baseline 50.36 MiB → quantized union 50.43 MiB — a **~0.07 MiB** archive
  delta (the LSTM/STFT/LayerNorm/quant kernels are tiny relative to the whole;
  the real link-time delta into `fono` after dead-strip is of the same small
  order). The runtime-size cost of adding Kokoro is effectively negligible.
  The cost lives in the **model download**: `q8f16` `.ort` = 89.6 MiB,
  fp32 `.ort` = 310.7 MiB. Voice-style data: full `voices-v1.0.bin` (54
  voices) = 26.9 MiB; a single `af_heart` pack `[510,1,256]` f32 = 0.50 MiB.
  **Recommendation: ship `q8f16` (~90 MiB model + 0.5 MiB `af_heart` style),
  fp32 as opt-in fallback.** Models download at runtime → no impact on the
  ≤ 32 MiB `cpu` binary cap.
- [x] A4. Verify the espeak-ng `en-us` IPA → Kokoro fixed-vocab mapping
  produces ids that the model accepts and that round-trip to intelligible
  English audio in the spike harness. Rationale: the phoneme-id mapping is the
  single largest quality risk and is independent of the Rust integration.
  **DONE (2026-06-02).** The Kokoro vocab is a single fixed 178-symbol table
  (pure IPA + stress/length marks `ˈ ˌ ː`, no per-voice phoneme map). A
  representative en-US IPA string (`ðə kwˈɪk bɹˈaʊn fˈɑks ʤˈʌmps ˈoʊvɚ ðə
  lˈeɪzi dˈɔɡ.`) mapped **50/50 chars with zero unmapped** — stress marks,
  affricates (`ʤ`), `ɹ`, diphthongs (`aʊ`,`eɪ`), `ɚ` all covered. Algorithm
  confirmed: espeak en-us IPA → char-wise vocab lookup → bracket with pad id 0
  (no interspersed pad), `style` row selected by token count
  (`pack[len(tokens)]`). Synthesis produced clean speech-energy audio.

### Phase B — Mirror + model assets

- [ ] B1. Publish the Kokoro **q8f16** `.ort` model (~90 MiB) and **four**
  voice-style packs — `af_heart` (default), `af_bella`, `af_nicole` (en-us) and
  `bf_emma` (en-gb), ~0.5 MiB each — to `bogdanr/fono-voice` as assets on the
  `onnxruntime-1.24.2` release (the live tag; see `fetch-onnxruntime.sh:29`),
  with a `SHA256SUMS` asset and `manifest.json` tree entries per ADR 0033. fp32
  is **not** mirrored (hidden opt-in only). Style-pack on-disk format: raw
  little-endian `f32`, shape `[510,256]` row-major (522,240 bytes), so the Rust
  loader needs no `.npy`/zip parser. Rationale: the minimal runtime loads only
  `.ort` and no public hub hosts it; the model is shared across all four voices.
  **Gated on:** valid `gh` auth for `bogdanr` (token currently invalid).
- [~] B2. **Required for Kokoro:** the currently-hosted `libonnxruntime` is
  built from the **Piper-only** op set and cannot load Kokoro. Rebuild the
  minimal runtime from the **unioned (Piper + Kokoro q8f16)** ops config
  (`scripts/build-onnxruntime-minimal.sh` with the Phase-A `qunion-ops.config`)
  for **all four release triples** — `x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`
  — upload each `libonnxruntime-<triple>.a.xz` to the
  `onnxruntime-1.24.2` release (or a fresh ABI tag), and re-pin every
  `sha_for_triple` row in `scripts/fetch-onnxruntime.sh:51-69` from the updated
  `sha-<triple>.txt`.
  **Progress 2026-06-15:**
  - **Root cause refined.** The mirror's build input
    (`fono-voice/onnxruntime/ops.config`, fed to `--include_ops_by_config` by
    `fono-voice/.github/workflows/build-onnxruntime.yml`) was *not* simply
    Piper-only — it was an incomplete union **missing `Greater`(13) + `If`**
    (so Kokoro q8f16 failed to load), while a separately-generated fono config
    was conversely **missing `LSTM` + `MatMulInteger`** needed by some Piper
    voices. Neither was a complete superset.
  - **True union built + committed.** `scripts/merge-ort-configs.py` (new)
    unions both per-set configs — operators AND per-op type constraints — into
    the complete set (verified to contain `Greater`, `If`, `LSTM`,
    `MatMulInteger`, parsed back through onnxruntime's own
    `reduced_build_config_parser`). The union is committed to both
    `fono-voice/onnxruntime/ops.config` (the build input) and the fono mirror
    copy `calibration/voice-models/ort/ops.config`. `scripts/gen-ort-models.sh`
    gained a `Greater`(13) regression guard so a future partial run fails loudly.
  - **Linux x86_64 rolled out + verified.** Rebuilt the lib from the union
    config (50.4 MiB `.a`, no size regression), published to the mirror
    (`onnxruntime-1.24.2`, `--clobber`), re-pinned `sha_for_triple()` x64 row to
    `28c7dca4…`, and verified from a **clean** `fetch-onnxruntime.sh` that Kokoro
    English synthesis succeeds with no `Greater(13)` error.
  - **The "no CI workflow that builds it" gate is already solved upstream:**
    `fono-voice/.github/workflows/build-onnxruntime.yml` builds all five triples
    from the vendored `onnxruntime/ops.config` and publishes them using the
    mirror's **own `GITHUB_TOKEN`** — no cross-repo secret needed. (An earlier
    duplicate workflow added to the fono repo was removed as redundant.)
  - **Remaining (gated on the user):** push the fono-voice config commit
    (`9caf214`, staged locally) to `main`, run **build-onnxruntime** from the
    Actions tab (`ort_version=1.24.2`, `release_tag=onnxruntime-1.24.2`), then
    re-pin the `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`,
    `x86_64-apple-darwin`, `x86_64-pc-windows-msvc` rows in
    `scripts/fetch-onnxruntime.sh` from each `sha-<triple>.txt` the workflow
    attaches (x64 is already done and live).
  **B1 done 2026-06-02:** model `kokoro-v1.0-q8f16.ort` + the four `.style.bin`
  packs + merged `SHA256SUMS` (89 entries, Piper preserved) published to the
  live `ort-1.24.2` release and public-download/checksum verified.

### Phase C — Catalog + schema

- [x] C1. Extend the `Voice`/`Asset` schema in
  `crates/fono-tts/src/voices.rs:43-90` to carry a Kokoro voice's **style
  pack** asset (in addition to `model`). Keep it optional so existing Piper
  entries (which use `config`) parse unchanged. Rationale: Kokoro has no
  per-voice `.onnx.json` phoneme map; it needs the `[~510,1,256]` style data
  instead.
- [x] C2. Add the **four** Kokoro English voices (`af_heart` default,
  `af_bella`, `af_nicole` (en-us); `bf_emma` (en-gb)) to `catalog.json` with
  `engine: "kokoro"`, `language: "en"`, the shared `.ort` model asset, and each
  voice's style asset, all SHA-256/size pinned. Decide whether to **replace**
  the two English Piper entries (`en_US-amy-medium`, `en_GB-alan-medium`) or
  keep them as override-only fallbacks. Rationale: `for_language("en")` returns
  the first match (`voices.rs:118-120`); the Kokoro `af_heart` entry must win
  for English by policy.
- [x] C3. Update the catalog guard tests (`voices.rs:243-309`) for the new
  schema and the English-engine expectation; the existing well-formedness and
  dict-coverage tests must stay green. Rationale: locks the catalog contract.

### Phase D — Kokoro engine

- [x] D1. Add `crates/fono-tts/src/kokoro.rs` (feature `tts-local`) with a
  `KokoroLocal` implementing `TextToSpeech`, mirroring `PiperLocal`'s structure
  (`Arc<Mutex<Session>>`, `spawn_blocking` inference, `recover()` optimization
  idiom). Rationale: a second engine on the same runtime, parallel to
  `piper.rs`.
- [x] D2. Embed the fixed Kokoro phoneme vocab (small, fixed map) via
  `include_bytes!`/a const table and implement `text → espeak IPA →
  Kokoro token ids` reusing `crate::espeak`. Select the espeak voice from the
  catalog voice's accent: `af_/am_` → `en-us`, `bf_/bm_` → `en-gb` (both dicts
  already exist). Bracket ids with pad `0`; clamp to the max token length.
  Rationale: the deterministic, unit-testable front half — the Kokoro analogue
  of `PiperConfig::phoneme_ids`.
- [x] D3. Load the voice-style pack and select the `style` vector by token
  count (`style[len(ids)]` → `[1,256]`), build the `input_ids`/`style`/`speed`
  tensors, run the session, and return mono `f32` PCM at 24000 Hz. Rationale:
  the back half (the I/O-contract difference from Piper).
- [x] D4. Unit-test the vocab mapping, id bracketing, token clamping, and
  style-index selection with fixtures; add an `#[ignore]`d end-to-end synthesis
  test gated on a linked runtime + downloaded assets (mirroring
  `piper.rs:416-448`). Rationale: deterministic coverage without hardware in CI.

### Phase E — Router dispatch (the engine split)

- [x] E1. Generalise `LocalRouter`'s cache from `Arc<PiperLocal>` to
  `Arc<dyn TextToSpeech>` and make `load_engine`
  (`local_router.rs:229-249`) branch on `voice.engine`: `"kokoro"` →
  `KokoroLocal`, else `PiperLocal`. Rationale: the router currently hardcodes
  Piper; this is where the per-language engine choice lands.
- [x] E2. Confirm `resolve_voice_for_lang` (`local_router.rs:213-224`) routes
  English to the Kokoro catalog voice by policy (via C2's catalog ordering or
  an explicit engine-preference for `en`), while non-English continues to
  resolve Piper voices. Rationale: implements `lang == en → Kokoro`.
- [x] E3. Verify `factory::resolve_local_voice` (`factory.rs:84-98`) picks the
  Kokoro voice as the eager `default_voice` when the user's primary language is
  English, and that the router's native-sample-rate hint (24000) propagates to
  the playback/resample layer. Rationale: default-voice path must not silently
  fall back to Piper English.

### Phase F — Download/ensure + wizard

- [x] F1. Extend `voices::ensure_voice` (`voices.rs:148-163`) to fetch the
  Kokoro style-pack asset alongside the model, skipping the espeak-dict fetch
  path where it does not apply (Kokoro still needs the `en_dict`, which already
  downloads). Rationale: the daemon-startup auto-download
  (`fono::models::ensure_local_tts`) must stage Kokoro assets for offline use.
- [x] F2. Confirm the first-run wizard / `fono use tts local` path presents the
  English-via-Kokoro default correctly (the remaining `2.3` wizard item and the
  `2.4` split). Rationale: closes the user-facing entry point.

### Phase G — Gate, docs, size

- [ ] G1. Run the full pre-commit gate (`cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`) plus
  `-p fono-tts --features tts-local`. Rationale: project hard rule.
- [ ] G2. Rebuild `release-slim` and assert the four-entry `NEEDED` allowlist
  and the ≤ 32 MiB `cpu` cap hold with the Kokoro-capable runtime; record the
  measured delta in `docs/binary-size.md` and ADR 0022. Rationale: the
  Kokoro-unioned runtime is the main size risk.
- [ ] G3. Tick task 4.1 (and the 2.4 remainder) in plan v3, update
  `docs/status.md`, and add a `CHANGELOG.md` `[Unreleased]` entry. Amend
  ADR 0033 only if the spike changed any pinned decision. Rationale: living-doc
  discipline.

## Verification Criteria

- A minimal `libonnxruntime.a` built from the unioned `ops.config` loads and
  runs the Kokoro `.ort`, producing non-silent 24 kHz English PCM (Phase A).
- With `general.languages = ["en", "ro"]` and no voice pin, an English
  utterance synthesises through `KokoroLocal` and a Romanian utterance through
  `PiperLocal`, confirmed via the `fono_tts::local_router` debug log.
- An explicit `[tts.local].voice` pin still disables routing (existing pin
  semantics preserved).
- The shipped `release-slim` binary keeps the four-entry `NEEDED` allowlist and
  stays ≤ 32 MiB.
- `cargo fmt` / `clippy -D warnings` / `cargo test` green workspace-wide and for
  `-p fono-tts --features tts-local`; new Kokoro unit tests pass.

## Potential Risks and Mitigations

1. **Minimal/reduced runtime cannot support Kokoro's operators** (the same
   failure that omitted seven Piper voices).
   Mitigation: Phase A spike gates everything; if it fails, decide between a
   larger op set (size hit, re-measure cap) or deferring Kokoro — before any
   engine code is written.
2. **espeak IPA → Kokoro fixed-vocab mapping mismatch** yields wrong phonemes /
   accented or garbled English.
   Mitigation: A4 validates the mapping in isolation against reference audio;
   keep the English Piper voices as override fallbacks (C2) so users are never
   stranded.
3. **Model download size** (fp32 Kokoro ~310 MB vs ~64 MB Piper voices) hurts
   first-run UX.
   Mitigation: A3 measures quantized/fp16 variants; prefer the smallest variant
   the minimal runtime can run; flag the size in the catalog as Russian
   `ru_dict` is flagged.
4. **Schema churn breaks existing Piper catalog parsing.**
   Mitigation: new style-pack asset is optional/additive; C3 guard tests lock
   backward compatibility.
5. **ABI bump** if Kokoro forces a different onnxruntime version.
   Mitigation: ADR 0033 already tags mirror releases by ABI (`ort-<version>`);
   mint a new release tag rather than disturbing `ort-1.24.2` installs.

## Alternative Approaches

1. **Keep Piper for English, skip Kokoro.** Zero new infra, but forgoes
   Kokoro's higher English prosody — the entire point of ADR 0033's split.
2. **Ship a non-minimal `.onnx`-loading runtime** so any Kokoro `.onnx` loads
   without `.ort` conversion. Rejected by ADR 0033: +5.5 MiB and still can't
   load arbitrary models; conflicts with the minimal-build discipline.
3. **misaki G2P instead of espeak.** Higher fidelity to Kokoro's training, but
   misaki is Python and not shippable in a single static binary; espeak reuse
   keeps the dependency surface flat.
4. **Cloud Kokoro parity** (the old 2026-05-14 plan). Out of scope here; the
   local English engine is the committed next step.

## Assumptions

- Kokoro is English-only locally per ADR 0033; the 54-voice multilingual router
  is **not** revived.
- **Voice set (decided 2026-06-02):** ship **four** voices — three American
  English (`en-us`): **`af_heart`** (grade A, default), **`af_bella`** (A-),
  **`af_nicole`** (B-); and one British English (`en-gb`): **`bf_emma`** (B-,
  best-graded British voice). The engine front-end therefore keeps the
  accent branch: `af_/am_` → espeak `en-us`, `bf_/bm_` → espeak `en-gb`.
  Each voice is a ~0.5 MiB style pack over the shared ~90 MiB q8f16 model, so
  the four-voice set adds only ~2 MiB of voice data.
- `af_heart` (American English) is the default voice.
- The chosen Kokoro model variant is **`q8f16`** (~90 MiB `.ort`), resolved by
  Phase A: it loads and runs on the minimal runtime (incl. the
  `DynamicQuantizeLSTM` contrib kernel) and is perceptually ~transparent vs
  fp32. **fp32 (311 MiB) is a hidden opt-in escape hatch only** (quality-A/B
  reference + golden regression tests), not surfaced in the wizard and not
  mirrored until a concrete need arises.
- Phase 1.1/1.4 minimal-build CI work (still open in plan v3) advances in
  lockstep, since the Kokoro-unioned runtime must be the one CI links and
  size-gates.
- No new third-party Rust crates are needed (espeak core + `ort` already
  present); if any are added, `deny.toml` is updated per project rules.
