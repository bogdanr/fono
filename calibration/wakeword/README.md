<!-- SPDX-License-Identifier: GPL-3.0-only -->
# Clean-license "hey fono" wake-word model — build & provenance

This directory holds the **one-time, offline training pipeline** for Fono's
default wake-word model, `hey_fono`. It is **host tooling only** — nothing
here ships in the binary, and **no trained model or SHA-256 is committed**.
The pipeline turns the eventual training into a near-one-command job; the
actual run, hosting, and SHA pinning are **manual operator steps** (see
[Operator steps](#operator-steps)).

Phase B of `plans/2026-06-23-wake-word-openwakeword-v2.md`.

> **Status: NOT yet trained.** No model artifact exists in the repo, and the
> registry pins (`crates/fono-audio/src/wake_registry.rs`) are still the
> all-zeros `UNPINNED` sentinel. Do not invent a hash; run the pipeline,
> host the output, then pin the real SHA-256.

## Fono synthesizes its own positives

The pipeline does **not** clone a separate TTS tool. Positive clips of the
wake phrase are produced by **`fono speak` itself**, across the **active TTS
backend's** palette voices — the very engine you ship is the one that makes
the training audio. The driver classifies the run from that backend:

- **Local backend** (`fono use tts local`, on-device Piper/Kokoro) → **CLEAN**:
  on-device, no network, clean-license. This is the only path eligible to
  train the shippable default `hey_fono`.
- **Cloud backend** (anything needing an API key — `openai`, `gemini`,
  `elevenlabs`, …) → **PRIVATE**: proprietary, ToS-bound audio that must
  **never** feed `hey_fono`. The model is stamped PRIVATE and you must accept
  the provider terms (`CLOUD_TTS_ACCEPT_TERMS=1`).

Switch the synthesizer with Fono's own config:

```sh
fono use tts local       # clean, on-device Piper/Kokoro  -> may build hey_fono
fono use tts openai      # cloud  -> PRIVATE custom keywords only
```

## Why a freshly-built model

The upstream openWakeWord pretrained phrases are **CC-BY-NC-SA-4.0
(NonCommercial)** — they cannot be a default or be bundled in a GPL-3.0
release. So, exactly as Fono does for Kokoro and the espeak core, the
*default* is a freshly-built **clean-license** artifact assembled entirely
from Apache-2.0 graphs + on-device-synthesized / openly-licensed data.

## License chain (the provenance record)

Every input to the `hey_fono` artifact is OSI-approved / openly licensed and
GPL-3.0-compatible. Record the exact versions/commits you used when you run it.

| Component | Artifact | License | Source |
|---|---|---|---|
| Melspectrogram graph | `melspectrogram.onnx` | **Apache-2.0** | openWakeWord v0.5.1 shared graph (`dscripka/openWakeWord`) |
| Embedding backbone | `embedding_model.onnx` (Google `speech_embedding`) | **Apache-2.0** | openWakeWord v0.5.1 shared graph / Google `speech_embedding` |
| Positive samples | synthetic "hey fono" clips | **synthetic** (Fono's on-device Piper/Kokoro voices; openly-licensed) | `fono speak` with `fono use tts local` |
| Negative / background | operator-supplied corpus | **openly-licensed only** (see below) | you fetch + verify |
| Trained classifier | `hey_fono.onnx` → `hey_fono.ort` | **Apache-2.0** (derives only from the above) | this pipeline |

Only the two shared graphs (`melspectrogram`, `embedding`) and the **clean**
`hey_fono` classifier are eligible to be a default / bundled. The community
phrases (`hey_jarvis`, `alexa`, `hey_mycroft`) are NonCommercial and are
fetched on demand from the registry (their license shown as a notice at
download) — they are **not** built here.

### Approved negative corpora (you fetch and verify the license)

For the clean default the pipeline **never downloads negatives**: the upstream
openWakeWord precomputed-feature bundles include ACAV100M / AudioSet-derived
material whose licensing is **not uniformly commercial-clean**. Use only
corpora you have verified, e.g.:

- **Free Music Archive (FMA)** — per-track CC licenses; keep only
  CC-BY / CC-BY-SA / CC0 tracks, drop NC/ND. <https://freemusicarchive.org>
- **Mozilla Common Voice** clips — CC0. <https://commonvoice.mozilla.org>
- **MUSAN** (noise / music / speech) — CC-BY-4.0. <https://www.openslr.org/17/>
- **Freesound** CC0 / CC-BY ambient, room-tone, and TV/speech-babble packs.

Record which sets (and which license subset) you actually used.

## Files in this pipeline

- `../../scripts/train-wakeword-model.sh` — the driver. Detects the built
  `fono` binary, resolves + classifies the active TTS backend, **synthesizes
  positives with `fono speak`**, resolves negatives, computes features through
  the frozen Apache graphs, trains the classifier, and converts the result to
  `.ort` via the existing `scripts/gen-ort-models.sh`.
- `../../scripts/wakeword_train.py` — the train/export glue: loads the
  Fono-synthesized positives, **augments** them (speed + gain) up to
  `N_POSITIVE`, extracts features through the frozen Apache graphs, trains a
  small Torch classifier head, and exports ONNX in the runtime's
  `[1,16,96] -> score` contract.
- `graphs/` *(auto-fetched)* — the frozen Apache `melspectrogram.onnx` and
  `embedding_model.onnx`. These are Apache-2.0, so the driver downloads them
  automatically if absent.
- `positives/`, `negatives/`, `work/`, `out/` *(generated)* — synthesized
  clips, scratch and outputs. **All git-ignored; do not commit.**

## What is automated vs. a manual operator step

| Step | Automated by the pipeline | Manual operator step |
|---|---|---|
| Synthesize positive wake-phrase clips | ✅ via `fono speak` across the active backend's voices | build/point at a `fono` binary (`FONO_BIN`, or `cargo build`) |
| Pick clean vs PRIVATE | ✅ derived from `fono use show` | choose the backend (`fono use tts local` / a cloud one) |
| Provide frozen Apache graphs | ✅ auto-fetched (Apache-2.0) | — |
| Assemble negatives (clean default) | — | **fetch + license-verify** an open corpus; set `NEGATIVE_AUDIO_DIR` |
| Assemble negatives (PRIVATE keyword) | ✅ auto-downloaded TESTING corpus | supply a verified corpus for anything you rely on |
| Feature extraction + classifier training | ✅ fully automated (Torch head over the frozen Apache embeddings) | run on a machine with the corpus + compute (`openwakeword` + `torch` venv) |
| `.onnx` → `.ort` (op-set stays in the shipped union) | ✅ via `gen-ort-models.sh` | — |
| Upload artifacts to the release host | — | **upload** the three `.ort` files |
| Pin real SHA-256 in `wake_registry.rs` | — | **pin** (replace the `UNPINNED` zeros) |

The training Python deps (`openwakeword`, `torch`, `onnx`, a venv) are **never
auto-installed**: the script detects them and prints the exact `pip`/`uv`
command for you to run. It auto-detects `.venv-wakeword/` if present, so once
you create it the next run just works with no activation.

## Operator steps

### Clean, shippable default `hey_fono` (on-device voices)

```sh
# 0. Create the training venv (NOT installed by the scripts); auto-detected:
python3 -m venv .venv-wakeword
.venv-wakeword/bin/pip install openwakeword torch onnx onnxruntime==1.24.2

# 1. Build a fono with on-device voices and point it at local TTS:
cargo build --release --features tts-local
fono use tts local

# 2. Fetch + license-verify an OPENLY-LICENSED negative corpus (see above),
#    then run the pipeline (the Apache graphs auto-fetch if absent):
NEGATIVE_AUDIO_DIR=/path/to/open-licensed-negatives \
  sh scripts/train-wakeword-model.sh
```

### Easiest PRIVATE custom keyword (uses your cloud backend)

```sh
fono use tts openai            # (once) point Fono at a cloud TTS backend
MODEL_ID=house PHRASE="house" CLOUD_TTS_ACCEPT_TERMS=1 \
  sh scripts/train-wakeword-model.sh
```

For PRIVATE models the driver also **auto-downloads a negative corpus** (Google
Speech Commands test set, ~112 MB) into `calibration/wakeword/negatives/` when
none is supplied. That corpus is fetched **for TESTING only — its license is
not verified**, so never ship or pin a model trained on it; supply your own
verified `NEGATIVE_AUDIO_DIR` for a real detector. Override the source with
`NEGATIVES_URL=<your .tar.gz of audio>`. None of these conveniences apply to the
clean default `hey_fono`, which always requires a real, license-verified
negative corpus.

### Preview only (no synthesis, no training)

```sh
MODEL_ID=house PHRASE="house" DRY_RUN=1 sh scripts/train-wakeword-model.sh
```

Both real runs produce, under `calibration/wakeword/out/ort/`:

- `melspectrogram.ort` — shared Apache graph
- `embedding.ort` — shared Apache backbone
- `<MODEL_ID>.ort` — the trained classifier

### Env-var knobs (defaults in parentheses)

- `PHRASE` (`"hey fono"`), `MODEL_ID` (`hey_fono`) — phrase + registry id /
  output basename. Any id other than `hey_fono` is a PRIVATE model.
- `FONO_BIN` (auto) — the built `fono` used to synthesize positives;
  auto-detects `target/release/fono`, `target/debug/fono`, then `fono` on PATH.
- `FONO_VOICES` (all palette) — `;`/`,`-separated voice labels to synthesize
  with (e.g. `"Female 1;Male 2"`); defaults to every voice in `fono voices list`.
- `CLOUD_TTS_ACCEPT_TERMS=1` — required when the active backend is a cloud one.
- `FONO_TTS_CLEAN=1` — treat a `wyoming` backend as a local, clean relay.
- `N_POSITIVE` (2000), `N_VALIDATION` (200) — positive target after
  augmentation / held-out count for the metrics bar.
- `NEGATIVE_AUDIO_DIR` / `NEGATIVE_FEATURES_DIR`, `NEGATIVES_URL`,
  `OWW_GRAPHS_DIR`, `WORK_DIR`, `OUT_DIR`, `PYTHON`, `DRY_RUN`.

### Quality bar (Phase B)

Tune until the held-out validation hits **< ~0.5 false-accepts/hour** and
**< ~5% false-reject**; an always-on detector that false-fires on TV/music is
unusable. Model quality depends almost entirely on the **number and diversity
of voices** Fono synthesizes (use more palette voices, or a cloud backend with
a richer palette) and the **size/realism of the negative corpus**.

### After training — host + pin

The output filenames are chosen to match the Phase-G registry
(`crates/fono-audio/src/wake_registry.rs`) exactly, so the only remaining
human work is:

1. **Upload** `melspectrogram.ort`, `embedding.ort`, and `hey_fono.ort` to the
   `fono-voice` release mirror under the `ort-<version>` tag (the same mirror
   and ABI-tagged release the local voices use, ADR 0033) referenced by
   `MELSPEC`, `EMBEDDING`, and the `hey_fono` entry.
2. **Compute** each file's SHA-256 (`sha256sum <file>`) and **pin** it in
   `wake_registry.rs`, replacing the `UNPINNED` all-zeros sentinel and the
   `TODO(phase B/G)` markers. Leave the community (NonCommercial) entries'
   pins to their own follow-up.
3. **Record** the exact graph versions, the backend + voices used, negative-corpus
   sets + license subset, sample counts, and training config used, for the
   ADR/licensing record (Phase K).

## PRIVATE models and licensing guardrails

When the active backend is a cloud one, the model is **PRIVATE / non-shippable**.
The guardrails are enforced by both the driver and the trainer:

- `MODEL_ID=hey_fono` with a cloud backend is **rejected** — the clean-license
  default must never be trained on cloud audio.
- A real cloud run requires `CLOUD_TTS_ACCEPT_TERMS=1`; you accept that the
  output is non-clean-license, ToS-bound, and the resulting model is **PRIVATE**.
- The run writes `out/PROVENANCE.txt` stamping the model PRIVATE and listing the
  backend + voices used. **Do not ship, redistribute, or pin a cloud-built
  model** as a default; each provider's terms govern the audio and many forbid
  training on it.

No artifact is committed to the repo; the binary never ships these bytes —
they are fetched + SHA-verified at runtime via `fono_download::download`.
