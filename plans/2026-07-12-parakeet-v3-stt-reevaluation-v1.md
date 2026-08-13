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

- [x] Task A1. Verify model facts from primary sources: v3 language list (confirm
      `ro`), license text (CC-BY-4.0, no rider), exact artifact sizes for fp16 and
      int8 ONNX exports (sherpa-onnx release packs), memory footprint at load.
- [x] Task A2. Benchmark WER/CER vs Fono's current local ladder (whisper `small`
      multilingual default, `large-v3-turbo` upper tier) on:
      (a) Fono's existing multilingual release-gate fixtures (en, ro, es, fr),
      (b) a fresh Romanian diacritic-heavy dictation-style set.
      Harness: sherpa-onnx offline-transducer CLI; cross-check one configuration on
      CrispASR to catch harness-specific artefacts.
- [x] Task A3. Benchmark latency: RTF on the 4-core CPU reference floor (ADR 0004)
      and on the dev machine, int8 vs fp16; measure cold-load time. Compare against
      whisper small/turbo numbers from the existing calibration matrix
      (`calibration/`).
- [x] Task A4. Assess ort compatibility: extract the op/type set of the v3 ONNX
      graphs (`create_reduced_build_config.py`), diff against Fono's current minimal
      `ops.config` plus the planned Supertonic additions; estimate the
      `libonnxruntime.a` growth. Scope batch mode first (streaming/cache variants add
      ops).
- [x] Task A5. Document the TDT greedy-decode I/O contract from the sherpa-onnx
      offline-transducer implementation (tensor names, shapes, duration-head
      semantics, token-table/BPE vocab loading) as the port reference. Verify that
      punctuation + casing come out of the model natively, per language.
- [x] Task A6. Write the go/no-go memo into this file: decision matrix of accuracy
      delta vs whisper-small and turbo, RTF delta, download size, ort growth, port
      effort. Suggested "go" bar: WER improves ≥ 20 % relative on en+ro at
      RTF ≤ whisper-small, with ort growth inside the size budget.

## Phase A results and go/no-go memo

**Decision: NO-GO for now.** Parakeet-TDT v3 is dramatically faster than every
whisper tier we ship and is a genuine license/language fit, but on Fono's own
fixtures it does **not** beat whisper-small on accuracy (the go bar asked for
≥ 20 % relative improvement on en+ro; measured is 7 % *worse*), it costs a
670 MB download, and it cannot be told which language to expect. Re-open on the
triggers listed at the end.

It is also **not** the answer for weak machines, which was the most plausible
reason to ship it anyway: it peaks at ~1 GB RSS against whisper-small's 585 MB.
It trades RAM for CPU, and low-end desktops are short of RAM.

A front-end denoiser narrows this — see "Model-specific tricks" below; it fixes
the noise objection and gets the model to ~17 % better than whisper-small on
en+ro, just under the bar. The no-go rests on two things no trick touched: the
language cannot be pinned, and utterances under ~2 s return nothing. The same
denoiser measured **zero** benefit for whisper, so it is not a general win we
should pick up separately.

### Reproduction

Measured on `ultra7-258v` (Intel Core Ultra 7 258V, 8 cores), Linux, all runs
`nice -n 10`. Model: `csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8`
(and the fp32 sibling repo) driven through the `sherpa-onnx` 1.12.x Python
`OfflineRecognizer.from_transducer(model_type="nemo_transducer")` in a scratch
venv — no Fono code was built or changed. Whisper columns are the **batch**
transcripts already recorded in `docs/bench/calibration/runs/ultra7-258v__ac__cpu__*`
(fono 0.8.1), re-scored by the same scoring code so every cell is comparable.
Metrics: word-level WER on case-folded, punctuation-stripped text, and the
case-folded / whitespace-collapsed normalized character Levenshtein distance
that `fono-bench` reports (`crates/fono-bench/src/equivalence.rs:493`).
References are the fixture manifest's ground truth
(`tests/fixtures/equivalence/manifest.toml`).

The planned CrispASR cross-check was dropped: running the same fixtures through
the **fp32** graphs on the same harness is a stronger control for the failures
that actually showed up (it separates model behaviour from int8 damage), and
upstream's own ggml runtime has since replaced CrispASR as the reference.

### A1 — Model facts (all confirmed)

- 25 European languages **including Romanian**, no language input: the model
  detects the language implicitly. License is plain **CC-BY-4.0**, no rider.
- Punctuation and capitalization are native, per language, and were emitted
  correctly in every non-failing fixture.
- Vocab: unified SentencePiece, 8,192 tokens + `<blk>`; token table 94 KB.
- Artifact sizes: int8 ONNX **670 MB** total (encoder 652 + decoder 11.8 +
  joiner 6.4). fp32 is 2.55 GB. **There is no fp16 export** — the
  `…-v3-fp16` mirror repo is empty, so the shippable choice is int8 or nothing.
- Upstream also now ships an official `q8_0` **GGUF (714 MB)** plus a native C++
  runtime, `NVIDIA/NeMo-Speech.cpp`. That supersedes CrispASR as the ggml
  reference and is a second viable integration route (see re-open triggers).
- Load footprint: **+770 MB RSS** just to load the int8 stack; cold session load
  1.2 s (int8) / 1.8 s (fp32), versus ~2 GB RAM quoted upstream for the NeMo
  path. See the memory section below — this is the finding that settles the
  "good for weak machines" question.

### A2 — Accuracy (mean over fixtures, lower is better)

| group | metric | whisper small-q8_0 | whisper turbo-q8_0 | parakeet int8 | parakeet fp32 |
|---|---|---|---|---|---|
| en (n=5) | WER | 0.053 | **0.030** | 0.059 | 0.213 |
| ro (n=5) | WER | 0.301 | **0.202** | 0.322 | 0.249 |
| en+ro (n=10) | WER | 0.177 | **0.116** | 0.190 | 0.231 |
| en+ro (n=10) | char lev | 0.088 | **0.077** | 0.104 | 0.171 |
| es+fr (n=2) | WER | 0.137 | **0.062** | 0.481 | 0.497 |
| zh (out of set) | WER | **0.833** | 1.000 | 4.333 | 4.667 |

Per-fixture WER, the rows that decide it:

| fixture | small-q8_0 | turbo-q8_0 | parakeet int8 | parakeet fp32 |
|---|---|---|---|---|
| en-multi-sentence | 0.075 | 0.025 | **0.000** | **0.000** |
| en-self-dictation | **0.000** | **0.000** | 0.125 | **0.000** |
| ro-man | 0.250 | 0.125 | 0.125 | **0.094** |
| ro-woman | 0.172 | **0.034** | 0.138 | 0.138 |
| ro-bogdan-10s (music) | 0.500 | 0.458 | 0.417 | **0.250** |
| ro-bogdan-30s (radio) | **0.032** | 0.048 | 0.413 | 0.349 |
| fr-gide-symphonie | 0.097 | **0.065** | 0.903 | 0.935 |

Where it wins: clean long-form English and Spanish, and short clean Romanian —
it matches or beats whisper-small there, and the punctuation is better placed.

Where it loses, and why this is a no-go:

1. **Background noise.** On the 30 s Romanian clip with a radio playing,
   whisper-small scores 0.032 WER and parakeet 0.413. That fixture is the
   closest thing in the corpus to how Fono is actually used. (Largely fixable —
   a denoiser takes it to 0.111; see the tricks section.)
2. **Language misfire.** The French fixture returns English-shaped nonsense
   ("Gertrude, I did appear des Aveugles") in **both** int8 and fp32, so it is
   the model's implicit language ID failing, not quantization. Because the graph
   takes no language input, there is no way to pin `fr` and recover — whisper's
   `language=` does exactly that. This alone blocks a default promotion for a
   multilingual user base.
3. **Short utterances.** Under ~2 s the model returns an empty string
   (1.5 s slices of two fixtures: `''` int8, one word fp32). 3 s and up is
   fine. Dictation produces plenty of sub-2 s utterances.
4. **Out-of-set languages** degrade to phonetic transliteration
   (Chinese → "Lau Jing Yu and Dalai Shu Ran"), far worse than whisper's
   wrong-but-Chinese output. Whisper must stay for anything outside the 25.
5. **int8 is not free.** Romanian WER goes 0.249 → 0.322 from fp32 to int8, and
   fp32 is not shippable at 2.55 GB. So we would ship the weaker of the two.

### A3 — Speed (batch RTF, elapsed / audio duration)

| | whisper small-q8_0 | whisper turbo-q8_0 | parakeet int8 | parakeet fp32 |
|---|---|---|---|---|
| median RTF | 0.118 | 0.830 | **0.041** | 0.075 |
| cold load | — | — | 1.2 s | 1.8 s |

Parakeet int8 is ~2.9× faster than whisper-small-q8_0 and ~20× faster than
large-v3-turbo-q8_0 on the same host. Pinned to a **single thread** it still
returns median RTF 0.053 (verified genuinely single-threaded: 104 % CPU over
the whole 13-fixture run), which means it clears the 4-core reference floor with
room to spare — the one result that makes this worth revisiting.

### Memory, and whether this helps resource-constrained machines

Measured peak RSS on the same host, same fixtures. Parakeet numbers are a
Python process whose own baseline (interpreter + numpy + libs) is 53 MB, so the
model-attributable figures are the deltas. Whisper numbers are the whole
`fono transcribe` process.

| | download | RSS after load | peak RSS, short clip | peak RSS, 30 s clip |
|---|---|---|---|---|
| whisper small-q8_0 | 264 MB | — | — | **585 MB** (whole process) |
| parakeet int8 | 670 MB | 822 MB (+770) | 875 MB | **1,063 MB** |

So Parakeet costs **~1.8× whisper-small's peak memory and ~2.5× its download**,
and memory grows with clip length faster (+188 MB going from a 2 s clip to a
30 s one — the encoder holds `[N,1024,T/8]` activations for the whole utterance,
where whisper.cpp works in fixed 30 s windows with a fixed graph).

That inverts the intuition. Parakeet is fast **because it is a big model running
an efficient architecture**, not because it is small. It buys CPU time by
spending RAM. On a machine that is constrained by cores it is a clear win
(RTF 0.053 single-threaded); on a machine constrained by RAM — which is what
"low-end laptop" usually means, and what Fono's tiny tiers exist for — it is
strictly worse than what we ship. There is no configuration where it is the
light option: the actual light tier is whisper `tiny.en-q8_0` at a 43 MB
download, 15× smaller than Parakeet's.

A 1 GB working set also means it cannot be held resident between dictations on a
4–8 GB machine, so such a machine pays the 1.2 s cold load on every utterance —
which eats most of the RTF advantage on the short utterances that dominate
dictation, the same ones the model returns nothing for below ~2 s.

### A4 — Minimal ORT compatibility (cheap)

All three int8 graphs convert cleanly to `.ort` via `scripts/gen-ort-models.sh`
against the pinned onnxruntime 1.24.2. Their op set is 39 (op, opset) pairs;
unioned against the mirror's current 111-pair `ops.config` the diff is
**three net-new operators** plus four type widenings:

- `ai.onnx;13;DequantizeLinear`
- `ai.onnx;16;Identity`
- `com.microsoft;1;QuickGelu`
- widenings: `ConstantOfShape(9)` +bool, `Gather(13)` +uint8, `Slice(13)` +bool,
  `Transpose(13)` +bool

Everything else — `ConvInteger`, `DynamicQuantizeLinear`, `MatMulIntegerToFloat`,
`DynamicQuantizeMatMul`, `SkipLayerNormalization`, `LayerNormalization` — is
already compiled in for the Piper int8 and Supertonic voices. So the runtime
growth is small (three kernels; a rebuild would confirm, not yet measured) and
the binary size budget is **not** the obstacle here. The 670 MB model download
is.

### A5 — TDT decode contract (recorded for a future Phase B)

Read off the graphs themselves; enough to port without sherpa-onnx.

- **Featurizer:** 16 kHz mono, **128** log-mel filters, `normalize_type =
  per_feature` (per-utterance, per-bin mean/var), NeMo defaults 25 ms window /
  10 ms hop, and the encoder applies `subsampling_factor = 8`.
- **encoder**: in `audio_signal [N,128,T] f32`, `length [N] i64`;
  out `outputs [N,1024,T/8] f32`, `encoded_lengths [N] i64`.
- **decoder** (prediction net, 2-layer LSTM, `pred_hidden = 640`):
  in `targets [N,U] i32`, `target_length [N] i32`, plus the two LSTM state
  tensors `[2,N,640] f32`; out `outputs [N,640,U] f32` and the next states.
- **joiner**: in `encoder_outputs [N,1024,T'] f32`, `decoder_outputs [N,640,U]
  f32`; out `[N,T',U,8198] f32` = 8,193 label logits (8,192 vocab + `<blk>` at
  id 8192) followed by **5 duration logits**. Greedy TDT: argmax the label part,
  argmax the duration part, emit non-blank labels, advance the time index by the
  predicted duration (0–4 frames), advance the prediction net only on a
  non-blank. Metadata confirms `vocab_size = 8192`, `model_type =
  EncDecRNNTBPEModel`, "only the transducer branch is exported".
- Token table is a plain `token<space>id` text file; ids map straight to
  SentencePiece pieces (`▁` word marks), so detokenizing is a string join.

Port effort estimate: the decode loop and token table are a day; the NeMo
featurizer is the risk (128 mel, per-feature normalization, dither/preemph
defaults) and needs frame-level diffing against this harness.

### Model-specific tricks, tested

Everything below was measured on the same fixtures, int8, greedy unless stated.

**A 0.5 MB denoiser is the big one.** Running GTCRN speech enhancement
(`gtcrn_simple.onnx`, 536 KB, k2-fsa release) over the input first:

| fixture | raw | denoised | denoised + peak-normalized |
|---|---|---|---|
| ro-bogdan-30s (radio) | 0.413 | 0.127 | **0.111** |
| ro-bogdan-10s (music) | 0.417 | 0.375 | 0.375 |
| ro-talcuirea-matei | 0.517 | 0.448 | 0.483 |
| fr-gide-symphonie | 0.903 | 0.903 | 0.419 |
| ro mean | 0.322 | **0.236** | 0.246 |

(The peak-normalized column moves things around, but see below — that is noise,
not a mechanism.)

That turns the single worst result in the whole spike — the radio fixture, at
3.3× whisper's error — into roughly whisper's neighbourhood. Cost: ~600 ms
per 10 s of audio (RTF ~0.05), so total RTF ~0.09, still under
whisper-small-q8_0's 0.118.

**How it works.** GTCRN is a mask-based enhancer over the complex spectrum, run
**one frame at a time**: inputs are `mix [1,257,1,2]` — a single 512-point STFT
frame as (real, imag) — plus three recurrent cache tensors; outputs are
`enh [1,257,1,2]`, the cleaned complex frame, plus the updated caches. So the
network does not "remove noise from a file"; it predicts, per frequency bin and
per 16 ms frame, how much of that bin is speech, applies that as a complex mask
(scaling *and* phase-correcting the bin), and you inverse-STFT back to a
waveform. Inside, the op mix tells the story: a small Conv / ConvTranspose
U-net over the spectrogram, 14 GRUs carrying the temporal state, and
Gather/Slice/ScatterND doing ERB-band grouping so the network works on ~33
perceptual bands rather than 257 raw bins. 467 KB of weights.

Two consequences that matter: it is **causal and streaming** (the caches are the
only state, no lookahead), so it fits Fono's live path and adds one frame of
latency, not a buffer; and it is cheap because it never sees the raw bin count.

**And it does not lift whisper.** Re-run properly — Fono's own `local` whisper
small-q8_0 via `fono transcribe --no-polish`, language pinned **per fixture**
(the earlier ro+en pinning mis-forced the English clips and inflated one row),
ten en+ro fixtures:

| whisper small-q8_0 | raw | peak-normalized | denoised | denoised+norm |
|---|---|---|---|---|
| mean en+ro | 0.179 | 0.181 | **0.178** | 0.178 |
| mean en | 0.053 | 0.053 | 0.058 | 0.058 |
| mean ro | 0.304 | 0.308 | 0.298 | 0.298 |

Zero, in aggregate, and it moves individual fixtures both ways
(ro-bogdan-10s 0.500 → 0.417, ro-talcuirea 0.552 → 0.621, en-multi-sentence
0.075 → 0.100). Whisper trains on noisy audio and already handles this;
Parakeet is the model that is unusually sensitive to it. So denoising is not a
rising tide — it closes the gap rather than preserving it.

**Peak normalization is not a trick — it is measurement noise.** It looked like a
win (ro-bogdan-10s 0.417 → 0.333, fr 0.903 → 0.871) until the mechanism was
checked. Both front-ends are **scale-invariant by construction**: Parakeet does
per-feature CMVN (per-mel-bin mean/variance over the utterance) and whisper
normalizes its log-mel against the clip's own maximum, so multiplying the
waveform by a constant cannot change either input. Scaling `ro-man` and
`en-self-casual` by 0.25 / 0.5 / 0.95 / 1.0 returns **byte-identical**
transcripts. `ro-bogdan-10s` returns a *different* transcript at every scale —
that clip peaks at full scale and is decode-unstable, so its "improvement" was a
coin landing the right way up. Whisper agrees: 0.179 → 0.181 over ten fixtures,
with one better and one worse. **Do not implement it for accuracy.** (Input gain
for *recording* level is a separate, real concern; this is only about scaling the
samples before the model.)

Also worth knowing for its own sake: neither model can be helped by amplitude
tuning, so any future "the mic was too quiet" complaint needs fixing at capture
time, not in the STT front-end.

**Blank penalty** (`blank_penalty`, subtracted from the blank logit to suppress
deletions) helps exactly where deletions dominate and hurts elsewhere:
fr 0.903 → 0.774 at 1.0, ro-woman 0.138 → 0.103, ro-bogdan-10s 0.417 → 0.333 at
2.0 — but en-single-sentence collapses to empty at 2.0 and en-self-dictation
degrades from 1.0 up. en+ro mean is flat across 0.0–1.5 (0.190 / 0.185 / 0.189 /
0.191). Not worth a knob; the small wins are inside the noise.

**Beam search** (`modified_beam_search`, 4 paths) is neutral at best and
*catastrophic* twice — it returned an empty string for both en-single-sentence
and fr where greedy produced text. Do not use it, and note that hotword
biasing in sherpa requires it, so decoder-level personal-vocabulary biasing on
this model is not a free ride.

**Silence padding does not fix short clips.** Padding a 1.5 s slice out to 3 s
with silence still yields empty or truncated output — the model needs real
speech duration, not frames. `feature_dim` is likewise a non-issue: sherpa reads
`feat_dim = 128` from the encoder metadata, so the Python default of 80 is
ignored (identical output either way).

**What the denoiser would cost us.** Not large, but not free either:

- **Download:** 536 KB, plus a mirror asset + `manifest.json` entry with sha256
  and license, plus catalog/wizard wiring. Binary itself unchanged.
- **Minimal runtime:** the graph converts to a 0.7 MB `.ort`, but it is exported
  at opsets 5–11, so against our mostly-13-and-up `ops.config` it wants 23
  additional operator registrations — mostly older-version kernels of ops we
  already compile, but two genuinely new ones, `GRU(7)` and `ScatterND(11)`.
  That means a **minimal-runtime rebuild across every triple**, to be batched
  with the other pending rebuilds rather than done alone. Re-exporting the model
  at opset 17 would collapse most of the 23; do that before believing the number.
  Binary growth unmeasured; the `GRU` kernel is the one real addition.
- **CPU:** ~600 ms per 10 s of audio at 4 threads (RTF ~0.05) — about 5 % of a
  core if run continuously. Fine inside a dictation window; it must **not** run
  during always-on wake-word listening, where it would burn battery for nothing.
- **Latency:** one frame. Negligible.
- **Code:** STFT/iSTFT at 512/257 bins, three cache tensors threaded across
  frames, and — the awkward part — an SNR gate, because enhancement *hurts* clean
  audio (whisper en 0.053 → 0.058, and it silenced one fragile Parakeet fixture
  entirely). A noise estimator good enough to decide when to switch it on is its
  own small pile of code and its own new failure mode.

So the honest cost verdict: for **whisper**, which is what we actually ship,
the measured benefit is zero and none of the above is worth paying. The
denoiser is only interesting bundled with Parakeet — the model this memo
declines.

**Net effect on the decision.** With denoising, en+ro mean WER goes 0.202 →
0.157 (excluding the fragile en-single-sentence fixture, which the denoiser
silences), against whisper-small-q8_0's 0.188 on the same nine — i.e. ~17 %
better, just under the 20 % bar, and still behind large-v3-turbo's 0.120. So one
trick retires the **noise** objection almost entirely and dents the
language-misfire one, but nothing retires the two that matter most: no way to
pin the language, and nothing under ~2 s. The no-go stands, on a narrower margin
than the raw numbers suggested.

### Re-open triggers

Any one of these flips the calculation and justifies re-running this spike:

1. **A way to constrain the language.** An upstream export with a language
   input, or a decoder-side constraint that keeps output in the user's
   `general.languages`. Fixes the French-class failure, which is the hardest
   blocker.
2. **Noise robustness closes.** Mostly already achievable with a 0.5 MB GTCRN
   denoise stage (0.413 → 0.111 on the radio fixture, and it does *not* lift
   whisper the same way). A v3.x checkpoint that needs no front-end help would
   remove the extra model and the SNR gate.
3. **A smaller artifact.** A ≤ 300 MB export (distilled, or int4) at similar
   accuracy would make the download story competitive with whisper-small — and,
   more importantly, bring the ~1 GB working set down to where it fits a
   low-RAM machine.
4. **We need a speed tier.** If a future feature is bounded by STT latency
   (always-on transcription, long-form batch), RTF 0.04–0.05 single-threaded is
   worth revisiting on its own, restricted to clean English long-form — but as a
   *high*-resource tier that spends RAM to save cores, never as the light one.
5. **`NEMO-Speech.cpp` matures.** It removes the ONNX/featurizer port risk
   entirely, but it is a second inference stack next to whisper.cpp — only
   attractive if it could eventually replace whisper.cpp rather than sit beside
   it.

Follow-ups this spike does **not** do: no ADR change (ADR 0004's amended
"under re-evaluation" wording from Task C1 stays accurate), no ops.config or
runtime rebuild, no catalog entry.

## Phase B — Integration (gated on a Phase A "go")

**Not executing** — Phase A returned a no-go. Kept as the ready-made task list
for whenever a re-open trigger fires.

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
