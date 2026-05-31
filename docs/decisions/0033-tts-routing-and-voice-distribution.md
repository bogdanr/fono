# ADR 0033 — Local TTS engine routing, ORT voice distribution, and embedded phoneme data

## Status

Accepted 2026-05-31.

Builds on **ADR 0032** (ONNX Runtime as the voice-stack platform),
**ADR 0022** (binary-size budget + `NEEDED` allowlist), **ADR 0005**
(single static binary), **ADR 0004** (per-model licensing), and
**ADR 0016** (language allow-list, the `general.languages` key that keys
on-demand fetching). Refines the
`plans/2026-05-31-local-tts-onnx-voice-stack-and-wyoming-server-v3.md`
"Engine per language" row and resolves a contradiction between its
tasks 2.2b (`.ort`) and 2.3 (`.onnx`).

## Context

ADR 0032 committed Fono to a **minimal** static onnxruntime build. Three
downstream questions were left open and are settled here, each grounded
in measurement done 2026-05-31 (scratch work under `tmp/`, NimbleX host):

1. **Which engine speaks which language?** The plan said "Kokoro where
   trained, Piper fallback." Kokoro v1.0's own `VOICES.md` grades only
   **US/UK English** well (en-US **A**, en-GB **B-**); everything else is
   thin or weak — Japanese **C+**, Mandarin **all D**, French a single
   <11 h voice **B-**, Spanish/Portuguese ungraded, Hindi/Italian **C** —
   with the upstream caveat that non-English support "may be absent or
   thin." Piper, by contrast, has many `medium`/`high` voices across
   ~50 languages. So Kokoro is decisively better **only for English**.

2. **How do voices reach users?** A `--minimal_build` runtime loads
   **only `.ort`** (optimized flatbuffer), never `.onnx` — that is the
   whole reason `scripts/gen-ort-models.sh` exists. **No public hub hosts
   `.ort`**: HuggingFace `rhasspy/piper-voices`, `hexgrad/kokoro`,
   `snakers4/silero` all ship plain `.onnx`. So "users download `.ort`
   from upstream" does not exist as an option.

   We measured the cost of *avoiding* `.ort` by building a **non-minimal,
   operator-reduced** runtime that can load `.onnx` directly. Link-probe,
   stripped, `--gc-sections`:

   | Runtime | Loads | Probe size |
   |---|---|---|
   | `--minimal_build` (current) | `.ort` only | 2.01 MiB |
   | non-minimal, op-reduced | `.onnx` + `.ort` | 7.49 MiB |
   | **delta** | | **≈ 5.5 MiB** |

   And it does **not** buy "load any upstream voice": the op-reduced
   build *also failed* to load the raw upstream Piper `.onnx`
   (`Could not find an implementation for Relu(14)`), because `ops.config`
   is generated from the **optimized** `.ort` graph. Operator reduction
   and "load arbitrary upstream `.onnx`" are mutually exclusive; only the
   **full ~19 MiB** runtime loads arbitrary models, and that busts the
   32 MiB cap.

3. **How big is the phonemizer data, and can the shared part live in the
   binary?** Piper phonemizes via the pure-Rust `espeak-ng` crate
   (ADR 0032 / plan). Its data splits into a **shared phoneme set** plus
   **per-language dicts**. Measured from the published crates:

   - shared `espeak-ng-data-phonemes`: 0.37 MiB download / 2.3 MiB on disk;
   - all 114 `espeak-ng-data-dict-*`: 8.58 MiB download total, but
     dominated by **Russian `ru_dict` at 4.5 MiB (53 % of the total)**;
     the other 113 sum to ~4 MiB (en 106 KiB, ro 38 KiB, most 20–80 KiB).

   The shared crate's 2.3 MiB is mostly `phondata` (554 KB acoustic /
   formant data for espeak's *own* synthesizer) plus `voices/` and
   `mbrola_ph/` — none of which the text→IPA (G2P) path uses. The G2P set
   Fono needs is `phontab` (58 KB) + `phonindex` (43 KB) + `intonations`
   (2.3 KB) + an 8-byte `phondata` **header** (version magic + sample
   rate). Generic compression of the full blob bottoms out at ~283 KiB
   (xz/brotli) — over a 200 KiB target — so the lever is *what* we embed,
   not the compressor.

   **Empirically verified 2026-05-31** (`tmp/espeak-phondata-test/`,
   real `espeak-ng` 0.1.2): `text_to_ipa("Bună ziua", …)` returns
   byte-identical Romanian IPA (`bˈunə zˈiwa`) whether given the full
   554 KB `phondata` or an **8-byte stub** (bytes 0-7 only), and errors
   cleanly (`InvalidData("phondata too short")`) when the file is empty.
   Confirmed by code: the spectral `phondata` body is read only by the
   synthesizer (`synthesize/mod.rs`), never the IPA path; the
   `i_IPA_NAME` bytecode the IPA renderer consumes lives in `phonindex`
   (`load.rs:208`, `mod.rs:850`). At load, only `phondata[0..8]`
   (`VERSION_PHDATA = 0x01_48_01` + sample_rate) is checked.

   A runtime decompressor (`lzma-rs`/`ruzstd`) was considered to shrink
   the embed further: it would save ~37 KiB → vs the decoder's own
   ~50–100 KiB of code plus a `deny.toml`/license entry. Net loss; there
   is no other embedded blob to amortise it across (the codebase has
   **zero** `include_bytes!` today; model weights download at runtime).

## Decision

### 1. Engine-per-language routing

- **Kokoro serves English only** (en-US / en-GB, `af_heart` default).
- **Piper serves every other configured language** — including locales
  Kokoro nominally supports but renders poorly (es, fr, it, hi, pt, zh,
  ja, …). The router resolves: `lang == en → Kokoro`, else `→ Piper`.
- **No fixed Piper voice catalogue is pre-curated.** Voices are fetched
  on demand keyed off `general.languages` (ADR 0016). Romanian
  (`ro_RO-mihai-medium`) is the seed/demo voice; nothing else is bundled.

### 2. Voice / model distribution — keep minimal, host `.ort` ourselves

- **Keep the `--minimal_build` runtime** (ADR 0032). Do **not** pay the
  ~5.5 MiB for a non-minimal `.onnx`-loading runtime: it still requires
  enumerating + op-reducing the exact same model list, so it buys no
  "arbitrary upstream voice" capability — only size.
- Because the minimal runtime loads only `.ort` and no public hub hosts
  `.ort`, **Fono publishes its own `.ort` models** from a **dedicated
  repository, [`bogdanr/fono-voice`](https://github.com/bogdanr/fono-voice)**,
  **not** the main `fono` release page (keeping the app release's asset
  list small). Models are **GitHub release assets** in `fono-voice`,
  tagged by **onnxruntime ABI** (`ort-<version>`, e.g. `ort-1.24.2`) so
  an ABI bump mints a new release without disturbing old installs. The
  git tree of `fono-voice` holds **no binaries** — only a README and a
  machine-readable `manifest.json` (per-voice `sha256`, size, upstream
  `.onnx` URL + its `sha256`, license). They are derived/attributed
  mirrors of the upstream `.onnx` voices.
- **`fono-download` keeps a small committed catalog** in the main repo
  (asset path + `sha256` + onnxruntime version) with a **configurable
  base URL**, so a fork or self-hoster can re-point at their own mirror.
  Voices verify against that catalog — **no per-asset `.sha256` sidecars**
  (the `fono-voice` releases ship a single `SHA256SUMS` for humans).
- Each `.ort` voice download carries its **`.onnx.json` phoneme sidecar**
  and the matching **espeak per-language dict** (see §3).
- Conversion is **per-model, once per onnxruntime version bump**
  (1.24.2 today), via `scripts/gen-ort-models.sh`. Steady state: a
  handful of fixed-infra models (Kokoro, Silero VAD, Zipformer
  enc/dec/joiner, KWS) ≈ 6–8, **plus one per shipped voice** — re-run
  only on an ABI bump, never per app release.
- This resolves the plan contradiction: **task 2.3 downloads `.ort`**
  (+ `.onnx.json` + dict), **not `.onnx`** — task 2.2b already feeds the
  ids through an `.ort` session.

*Bootstrapped 2026-05-31:* `bogdanr/fono-voice` created, README +
`manifest.json` pushed, and the first release **`ort-1.24.2`** published
with `ro_RO-mihai-medium.ort` (+ `.onnx.json`, `SHA256SUMS`); public
download + checksum verified.

### 3. espeak phoneme data — embed the shared G2P set, download dicts

- **Embed the shared G2P set in the binary** via `include_bytes!`:
  `phontab` + `phonindex` + `intonations` + an **8-byte `phondata`
  stub** ≈ **102 KiB raw**, no compression, **no decompressor
  dependency**. Comfortably under the 200 KiB target and negligible
  against the 32 MiB cap.
- The 8-byte stub must carry the correct little-endian `VERSION_PHDATA`
  (`0x01_48_01`) in bytes 0-3; bytes 4-7 are `sample_rate`. **Generate it
  from the real `phondata` header at build time** so it tracks any future
  `VERSION_PHDATA` bump rather than silently mismatching.
- **Per-language dicts download on demand** alongside the voice (en
  106 KiB, ro 38 KiB, most tens of KiB). espeak loads them from the
  download-cache dir via `Translator::new(lang, Some(cache_dir))`.
- **Flag Russian** in the download catalog: `ru_dict` is the lone
  heavyweight at 4.5 MiB.
- This replaces the `espeak-ng/bundled-data-<lang>` cargo-feature
  approach for production: Fono embeds its own G2P blob and reads dicts
  from cache, rather than compiling each language's data into the binary.

## Consequences

- **Binary:** +~102 KiB embedded phoneme data (first `include_bytes!` in
  the tree); no new runtime decoder dep; `NEEDED` allowlist unchanged.
- **Distribution:** Fono operates a SHA-256-pinned `.ort` voice mirror
  (GitHub Releases / HF). One-time-per-release CI conversion via
  `gen-ort-models.sh`; re-run only on onnxruntime version bumps.
- **Routing:** English → Kokoro, everything else → Piper, fetched per
  `general.languages`. The earlier "Kokoro where trained" wording is
  retired as overstating Kokoro's non-English quality.
- **A typical user** (English via Kokoro + Romanian via Piper + one more)
  pulls < 1 MiB of espeak data total; the per-language footprint is
  dominated by the Piper voice `.onnx`/`.ort` (~20–60 MiB), not espeak.
- **Licensing:** the `.ort` mirror redistributes Piper (GPL-3.0) and
  Apache-2.0 voices — attributed per ADR 0004. The embedded espeak G2P
  data is espeak-ng's (GPL-3.0-or-later, compatible); the missing
  `license` metadata on the data crates (plan licensing follow-up) is
  sidestepped for the embedded blob since we vendor the bytes directly,
  but the on-demand dict crates still need the `[licenses.clarify]`
  resolution before `tts-local` enters the cargo-deny-checked build.

## Alternatives rejected

- **Non-minimal `.onnx`-loading runtime (+5.5 MiB, measured).** Removes
  only the `.ort` conversion/hosting step, and only for models we still
  must enumerate + op-reduce — it does **not** enable arbitrary upstream
  voices. Pure cost, no benefit.
- **Full ~19 MiB runtime** (loads any `.onnx`). Busts the 32 MiB cap once
  ggml is also linked.
- **On-device `.onnx → .ort` conversion at first run.** Needs Python
  `onnxruntime.tools` + `onnx` — not shippable in a single static binary.
- **Kokoro beyond English.** Its non-English voices are thin/weak per its
  own grading; Piper is better there.
- **Runtime decompressor (`lzma-rs`/`ruzstd`) for the phoneme blob.**
  Decoder code (~50–100 KiB) exceeds the ~37 KiB it would save; nothing
  else embedded to amortise it.
- **Embedding full `phonemes` (2.3 MiB) or full G2P with real `phondata`
  (665 KiB).** The 554 KB spectral `phondata` body is unused on the IPA
  path (verified); an 8-byte stub suffices.

## Surviving artefacts

- `scripts/gen-ort-models.sh` — `.onnx → .ort` + `ops.config` pipeline.
- `scripts/build-onnxruntime-minimal.sh` — minimal static runtime.
- `docs/binary-size.md` — amended: embedded G2P lever + `.ort` mirror.
- `plans/2026-05-31-local-tts-onnx-voice-stack-and-wyoming-server-v3.md`
  — amended: routing row, task 2.3 (`.ort`), espeak embed, task 4.1.
- `docs/decisions/0032-onnx-voice-stack-runtime.md` — the runtime ADR.
