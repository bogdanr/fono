# Local TTS + voice stack on ONNX Runtime, and Fono-as-a-Wyoming-TTS-Server — v3

> **Supersedes** `plans/2026-05-31-local-tts-ggml-piper-kokoro-and-wyoming-server-v2.md`.
> v2 chose a **ggml-reuse** substrate (hand-port Piper/Kokoro graphs onto
> the shared ggml runtime). That was reversed on 2026-05-31 once the
> project committed to a **full local voice stack** (TTS + wake-word +
> streaming STT + neural VAD + speaker-ID). Per **ADR 0032**, the stack
> runs on **statically-linked ONNX Runtime** (`ort`) — one Apache-2.0
> runtime for all those model classes — built **minimally** so the binary
> stays small, with **source-shared ggml** (ADR 0022 Task 1.2) scheduled
> afterwards to offset the size. The static-ONNX spike (2026-05-31) proved
> it links statically, keeps the four-entry `NEEDED` allowlist, and runs.
> See the v2 plan's appended spike sections for the full evidence trail.

## Objective

Make Fono speak — locally, multilingually, including Romanian — in the
single-binary spirit of today's local STT and LLM, on a runtime that
**also** carries the rest of the roadmap's voice features. Then expose
TTS on the network so Home Assistant auto-discovers Fono as a Wyoming TTS
service (the server glue already shipped — Phase 2a below).

## Pinned decisions (2026-05-31)

| Decision | Choice |
|---|---|
| Inference substrate | **ONNX Runtime, static, via `ort`** (ADR 0032). One runtime for the whole voice stack. No ggml-TTS, no candle. |
| Keep it small | **Custom minimal onnxruntime build** (`--minimal_build` + `--include_ops_by_config` from our ORT-format model set), pinned via `ORT_LIB_LOCATION`. ~19 MiB full → **measured ~2.1 MiB** for the Piper op set. See `docs/binary-size.md`. |
| Release packaging | **No new variant.** Voice stack absorbed into the existing **CPU** + **Vulkan** builds, behind a `tts-local`/`voice-local` feature (off in source default, on in shipped artefacts). |
| Acceleration | **CPU-only** (XNNPACK EP). ONNX has no Vulkan EP; voice models are CPU-realtime. ggml-Vulkan keeps whisper-large + LLM. |
| Romanian support | **Required.** Drives Piper inclusion (`ro_RO-mihai-medium`). |
| Engine per language | **Kokoro = English only; Piper = every other language** via router (ADR 0033). Kokoro's non-English voices are thin/weak per its own grading, so Piper — not Kokoro — serves es/fr/it/hi/pt/zh/ja too. |
| Voice format & hosting | **Minimal runtime loads `.ort` only**, and no public hub hosts `.ort`, so **`.ort` voices are published from a dedicated repo, [`bogdanr/fono-voice`](https://github.com/bogdanr/fono-voice)** (releases tagged by onnxruntime ABI, e.g. `ort-1.24.2`), **kept off the main `fono` release page**. `fono-download` fetches on demand keyed off `general.languages`, verified against a small committed catalog with a **configurable base URL** (ADR 0033). First release live 2026-05-31 with the Romanian seed. A non-minimal `.onnx`-loading runtime costs ~5.5 MiB (measured) and still can't load arbitrary upstream voices — rejected. |
| Phonemization | **Pure-Rust `espeak-ng` crate** (GPL-3.0-or-later). **Embed the shared G2P set in the binary** (`phontab`+`phonindex`+`intonations`+8-byte `phondata` stub ≈ **102 KiB**, no decompressor — ADR 0033); **per-language dicts download on demand** alongside the voice. No system lib, no `NEEDED` entry. (Spike-verified for `ro`; 8-byte `phondata` stub yields byte-identical IPA, verified 2026-05-31.) |
| Voices / weights | **Downloaded at runtime**, SHA-256-pinned, via `fono-download`. Never bundled. Russian `ru_dict` is the 4.5 MiB outlier — flag it in the catalog. |
| Size offset | **Source-shared ggml** (ADR 0022 Task 1.2) — reclaims ~7 MiB. Scheduled *after* Piper ships; **no longer a blocker for anything**. |
| Cap | ADR 0022 `cpu` cap raised to **≤ 32 MiB**, re-measured after minimal-build + Piper, and again after dedup. `gpu` ≤ 64 MiB. |

## Background — what already ships (re-verified 2026-05-31)

- **TTS trait + factory** — `crates/fono-tts/src/traits.rs:17-46`,
  `crates/fono-tts/src/factory.rs:40-54`. A local engine is just another
  `TextToSpeech` backend.
- **Wyoming TTS server — DONE (Phase 2a, this work).**
  `crates/fono-net/src/wyoming/server.rs`: `handle_synthesize` /
  `dispatch_synthesize` stream `audio-start → audio-chunk* → audio-stop`
  from any bound `TextToSpeech`; `build_info` advertises `info.tts` when
  voices are configured; daemon binds it via `[server.tts]`. Five tests
  green. **Home Assistant discovery works today over any backend** (cloud
  / Wyoming-client), independent of the local engine.
- **Generic SHA-256-pinned downloader** — `crates/fono-download/src/lib.rs`.
  Reused for Piper voices, Kokoro weights, ORT-format models.
- **Audio resample/playback** — `rubato`, `cpal`.
- **mDNS `caps` TXT tag** — adding `"tts"` is non-breaking.

## Implementation Plan

### Task 0 — ADRs and docs — DONE (2026-05-31)

- [x] **0.1** ADR 0032 — ONNX Runtime as the voice-stack platform.
- [x] **0.2** ADR 0022 amended — supersede the ggml-reuse TTS amendment;
  ONNX minimal build + dedup offset; `cpu` cap → ≤ 32 MiB.
- [x] **0.3** ADR 0004 amended — per-model licensing (Piper GPL; Kokoro /
  Silero / Zipformer / KWS Apache).
- [x] **0.4** `docs/binary-size.md` — the consolidated "small and capable"
  engineering guide.

### Phase 1 — Minimal ONNX Runtime build infrastructure

The size-discipline foundation. Must land before (or with) the first
engine so we never ship the full ~19 MiB runtime.

- [ ] **1.1** Stand up an onnxruntime **minimal static build** in CI:
  `--config MinSizeRel --minimal_build --include_ops_by_config <ops.config>
  --enable_reduced_operator_type_support --disable_ml_ops
  --disable_exceptions --disable_rtti --skip_tests`. Produce
  `libonnxruntime.a` as a release artefact.
  *(Tooling landed: `scripts/build-onnxruntime-minimal.sh`, pinned to
  onnxruntime v1.24.2 to match `ort-sys` 2.0.0-rc.12. Running it — a
  ~45-min networked compile — and pinning the artefact in CI is the open
  step.)*
- [~] **1.2** Wire `ort` to consume it: pin via `ORT_LIB_LOCATION`,
  disable `download-binaries`, enable `xnnpack`. Confirm static link +
  four-entry `NEEDED` in the real `fono` build (not just the spike crate).
  *(Landed & verified 2026-05-31: `ort 2.0.0-rc.12` workspace dep,
  `default-features = false` (no `download-binaries`); `tts-local` feature
  on `fono-tts` (+ `local` module: `RUNTIME_API_VERSION`, `ensure_runtime`)
  propagated through the `fono` crate, OFF by default — `ort` is absent
  from the default `fono` graph. Linked the fono-tts test binary against a
  real 1.24.2 `libonnxruntime.a` via `ORT_LIB_LOCATION`: onnxruntime
  statically embedded (19,611 `Ort*` symbols), `NEEDED` = exactly the
  four-entry allowlist, `ensure_runtime()` test passes. Remaining:
  `--use_xnnpack` (lands with the 1.1 minimal build that compiles the EP
  in) and confirming on the full `fono` binary in the 1.4 CI job.)*
- [x] **1.3** ORT-format model conversion + `ops.config` generation
  tooling (script in `scripts/`), seeded with the Piper `ro` voice. This
  is the standing pipeline every future model plugs into.
  *(Landed: `scripts/gen-ort-models.sh` — `convert_onnx_models_to_ort`
  with type reduction, seeds `ro_RO-mihai-medium`. Needs a host with
  `pip install onnxruntime==1.24.2` to run.)*

Note: Phase 2.1 (`tts-local` feature scaffold) was pulled forward and
landed as part of 1.2 above.

### Phase 1.4 — CI size gate (open)

- [ ] **1.4** Extend `tests/check.sh --size-budget` / CI to build the
  voice-stack feature and assert the ≤ 32 MiB cap + allowlist. Gated on
  the 1.1 minimal `libonnxruntime.a` artefact existing in CI.
  *(Pre-measured 2026-05-31 with the minimal lib in hand: a release
  static-`ort` binary adds ~2.1 MiB and keeps the four-entry `NEEDED`;
  the CI job still needs to build the artefact and assert against the
  real `fono` binary.)*

### Phase 2 — Piper on ONNX (first consumer)

- [x] **2.1** `tts-local` feature on `crates/fono-tts` (off in source
  default; on in shipped CPU + Vulkan builds). Pulls `ort` + the
  pure-Rust `espeak-ng` crate (`bundled-data-ro` to start).
  *(Landed: `ort` half in 1.2; `espeak-ng = 0.1.2` added to the workspace
  + `tts-local` enables `espeak-ng/bundled-data-ro`. Feature stays absent
  from the default `fono` graph — verified via `cargo tree -i`.)*
  *(ADR 0033 supersedes the `bundled-data-*` approach for production:
  embed the shared G2P set (`phontab`+`phonindex`+`intonations`+8-byte
  `phondata` stub ≈ 102 KiB) via `include_bytes!` and load per-language
  dicts from the download cache via `Translator::new(lang, Some(dir))`.
  `bundled-data-ro` stays only for the dev/demo build. New task 2.2d.)*
- [x] **2.2a** Piper front half — `crates/fono-tts/src/piper.rs`:
  `PiperConfig` sidecar parser, `phoneme_ids` (canonical BOS / interspersed
  PAD / EOS layout, verified against `ro_RO-mihai-medium.onnx.json`), and
  `PiperVoice` (espeak data install + `text → IPA → ids`).
  *(Landed + verified: pure-Rust espeak-ng produces correct Romanian IPA
  (`"Bună ziua" → "bˈunə zˈiwa"`); 6 unit tests incl. a Romanian
  end-to-end against the embedded `bundled-data-ro`, all green. No system
  espeak, no network.)*
- [x] **2.2b** `PiperLocal` engine implementing `TextToSpeech`: feed the
  ids from 2.2a through the `ort` session (`.ort` model) → f32 PCM at the
  voice sample rate.
  *(Landed & verified 2026-05-31: `PiperLocal` in `crates/fono-tts/src/piper.rs`
  builds an `ort::Session` from the `.ort` model (graph-opt disabled via
  `recover()` for minimal-build compatibility), runs the standard
  single-speaker VITS signature (`input` ids, `input_lengths`, `scales[3]`)
  → f32 PCM. End-to-end `#[ignore]`d test synthesises >0.5s of Romanian
  audio against the **minimal** `libonnxruntime.a` (10-op VITS build) +
  the converted `ro_RO-mihai-medium.ort`, peak amplitude in range.
  **Measured: minimal ONNX runtime adds only ~2.1 MiB** to a release
  binary, `NEEDED` = exactly the four-entry allowlist, onnxruntime
  statically embedded.)*
- [ ] **2.2d** Embed the shared espeak G2P set via `include_bytes!`
  (`phontab`+`phonindex`+`intonations`+8-byte `phondata` stub, ≈ 102 KiB;
  stub generated from the real `phondata` header at build time so it
  tracks `VERSION_PHDATA`). Load per-language dicts from the download
  cache dir. Retires `bundled-data-*` for production (ADR 0033).
- [ ] **2.3** Voice download + cache (**`.ort` model** + `.onnx.json`
  sidecar + matching espeak per-language **dict**) from the
  **`bogdanr/fono-voice`** repo's `ort-<version>` release, verified
  against a committed catalog (asset path + `sha256` + ort version) with
  a configurable base URL; first-run wizard entry. (Minimal runtime
  loads `.ort`, not `.onnx` — ADR 0033. Repo + `ort-1.24.2` release with
  the Romanian seed already live, 2026-05-31.)
  - [x] Committed catalog (`crates/fono-tts/voices/catalog.json`) +
    cache-aware resolver (`crates/fono-tts/src/voices.rs`:
    `catalog`/`by_name`/`for_language`/`ensure_voice`) for the
    **`.ort` model + `.onnx.json`**, SHA-256-verified with a configurable
    base URL (`DEFAULT_BASE_URL`); cache hit skips the network. New
    `Paths::voices_dir()`; `fono_download::sha256_file` made public.
    Unit-tested (catalog parse, language lookup, URL join, cache-hit,
    malformed-sha). *(2026-05-31.)*
  - [ ] Remaining: espeak per-language **dict** fetch + first-run wizard
    entry (lands with the router/factory wiring, 2.4/2.5).
- [~] **2.4** Router scaffold (language → voice); Romanian → Piper;
  English → Kokoro (once 4.1 lands), everything else → Piper.
  - [x] Voice resolution by configured language
    (`fono_tts::factory::resolve_local_voice` /
    `fono::models::ensure_local_tts`): explicit `[tts.local].voice`
    wins, else first catalog voice for `general.languages[0]`.
    *(2026-05-31.)*
  - [ ] Remaining: the Kokoro-vs-Piper split itself lands with 4.1
    (only Piper voices exist in the catalog today).
- [x] **2.5** Wire into the factory + the (already-shipping) Wyoming
  server so the local engine answers HA directly.
  - [x] `TtsBackend::Local` variant + `[tts.local]` config block
    (`voice`, `base_url`); `parse_tts_backend`/`tts_backend_str`/
    `all_tts_backends`/doctor/wizard/tray menu all handle it;
    `fono use tts local` selectable.
  - [x] `build_tts` `Local` arm loads the cached `.ort` + `.onnx.json`
    via `PiperLocal`; daemon startup `ensure_models` auto-downloads the
    voice (`ensure_local_tts`, boxed future) before the factory loads
    it, mirroring the STT model-ensure flow.
  - [x] Engine verified end-to-end: Romanian Piper synthesis produces
    real PCM against the minimal runtime (ignored test
    `piper_local_synthesizes_real_audio`). *(2026-05-31.)*
  - [ ] Remaining: live daemon HA playback smoke + the espeak
    per-language **dict** fetch (Romanian data is embedded today).
- [ ] **2.6** De-clutter the **app** release artifacts (ADR 0033 side
  effect): drop the per-asset `<asset>.sha256` sidecars
  (`release.yml:601-610`) now that voices live in `fono-voice`; migrate
  `fono-update` to verify against the single `SHA256SUMS` asset — point
  `sha256_url` at `SHA256SUMS` (`crates/fono-update/src/lib.rs:314-320`);
  `parse_sha256_sidecar` (`:633`) already filters by filename, so it
  handles the combined file unchanged. Keeps the back-compat
  `None`-sidecar path for old releases.

> **Licensing follow-up (blocks `tts-local` graduating to a cargo-deny-checked
> set).** The transitive data crates `espeak-ng-data-phonemes` and
> `espeak-ng-data-dict-ro` (v0.1.0) ship **no `license` field and no license
> file** upstream — the data is espeak-ng's, i.e. GPL-3.0-or-later, fully
> compatible with Fono, but the metadata is missing. Not triggered today
> (`deny.toml` is `all-features = false` and `tts-local` is off by default),
> so CI cargo-deny stays green. Before `tts-local` ships in the
> cargo-deny-checked build, resolve with a `[licenses.clarify]` entry (or an
> upstream PR adding the field). Recorded 2026-05-31.

### Phase 3 — Source-shared ggml (size offset)

- [ ] **3.1** Fork `whisper-rs-sys`; build it against an external ggml so
  it shares `llama-cpp-sys-2`'s copy. Reconcile the ggml ABI drift
  (measured 77-line `ggml.h` divergence, 2026-05-31). Pin via a remote
  git `[patch.crates-io]`.
- [ ] **3.2** Retire `--allow-multiple-definition`; assert a single
  `ggml_init`. Re-measure the `cpu` cap and record it in ADR 0022.
  (Mark ADR 0018 superseded once this lands.)

### Phase 4+ — Growth on the same runtime (no new integration)

Each is "wire a model into ONNX + regenerate `ops.config`", not a new
engine:

- [ ] **4.1** **Kokoro** TTS for **English only** (its sole strong
  locale, en-US/en-GB); extend the router with the rule **English →
  Kokoro, everything else → Piper** (ADR 0033 — Kokoro's non-English
  voices are thin/weak). (`onnx-community/Kokoro-82M-v1.0-ONNX`,
  `af_heart`.) Convert to `.ort` + regenerate `ops.config`; publish to
  the Fono `.ort` mirror.
- [ ] **4.2** **Silero VAD** — neural VAD upgrade over the energy
  envelope (`crates/fono-audio/src/vad.rs` `SileroVad` slot exists).
- [ ] **4.3** **Wake-word** via transducer KWS (ADR 0012 engine choice;
  custom phrase by tokens, no per-word training).
- [ ] **4.4** **Streaming STT** via Zipformer transducer — true low-latency
  live dictation (whisper.cpp cannot stream).
- [ ] Later: punctuation restoration, speaker-ID / diarisation.

## Acceptance

- **Phase 1:** a `fono` CPU build with the voice-stack feature links a
  minimal static onnxruntime, presents the four-entry `NEEDED` allowlist,
  and is ≤ 32 MiB. CI gate enforces it.
- **Phase 2:** `fono` speaks Romanian locally; HA synthesises TTS over the
  local engine; `cargo fmt` / `clippy -D warnings` / `cargo test` green.
- **Phase 3:** one `ggml_init`, link trick gone, `cpu` cap re-measured.
- **Phase 4+:** each capability lands by adding a model + `ops.config`
  entry, with the size gate still green.

## Risks

1. **Minimal-build pipeline is new release-engineering.** Mitigation:
   Phase 1 is explicitly first; the path is well-trodden (ORT Mobile).
2. **`ORT_LIB_LOCATION` reproducibility.** Pin the built `.a` as a CI
   artefact; never fall back to the CDN in release builds.
3. **ggml ABI reconciliation (Phase 3)** is the same hard fork it always
   was — but it is now a *non-blocking* offset, so it can take its time.

## Surviving artefacts

- `docs/decisions/0032-onnx-voice-stack-runtime.md`
- `docs/decisions/0033-tts-routing-and-voice-distribution.md`
- `docs/decisions/0022-binary-size-budget.md` (amended)
- `docs/decisions/0004-default-models.md` (amended)
- `docs/binary-size.md`
- `crates/fono-net/src/wyoming/server.rs` (Phase 2a, shipped)
