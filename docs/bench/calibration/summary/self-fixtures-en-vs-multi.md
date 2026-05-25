# `.en` vs multilingual Whisper on real dictation

This note pairs the per-host self-dictation calibration runs (see
`docs/status.md:30-60`) with the public Open-ASR-Leaderboard means, and
explains why a handful of cells in `matrix.md` invert the published ranking.

## Open-ASR-Leaderboard means (English)

The numbers below are mean WER% across the leaderboard's ~8 English ASR
datasets (LibriSpeech clean+other, TED-LIUM, GigaSpeech, SPGISpeech,
CommonVoice, AMI, VoxPopuli). They are the published prior the registry
now anchors to (`crates/fono-stt/src/registry.rs` `wer_by_lang` entries).

| Model            | OASR mean WER (en) | Registry `wer_by_lang["en"]` |
|------------------|--------------------|------------------------------|
| `tiny`           | ~15.6              | 16.0                         |
| `tiny.en`        | ~12.8              | 13.0                         |
| `small`          | ~9.5               | 10.0                         |
| `small.en`       | ~8.6               | 9.0                          |
| `large-v3-turbo` | ~7.6               | 8.0                          |

Registry numbers are rounded **up** to whole percent so the affordability
gates err on the conservative side.

The published prior reproduces the expected `.en ≤ multilingual` invariant
at every family (locked in by the
`english_only_variants_beat_multilingual_on_english` registry test). It
also pushes `tiny` multilingual across the 15% Inaccurate-bucket boundary
for English, which matches the matrix evidence that `tiny` multilingual is
unsuitable for serious English dictation.

## Why the calibration matrix sometimes shows the opposite

A small number of cells in `matrix.md` show `.en` worse than its
multilingual sibling on the self-dictation English fixture set. The two
mechanisms are well-understood:

1. **Worst-fixture gating.** The accuracy summary surfaced in the matrix
   is the *worst* per-fixture accuracy, not the mean (see
   `auto-select.html:367-372`). The English fixture set is small (≈4
   clips), and a single outlier dominates the displayed value.

2. **Short-clip flakiness.** `en-single-sentence` is short enough that
   the decoder's repetition-suppression and condition-on-prev-text
   heuristics are sensitive to model + quantization noise — particularly
   on fp16. See the `manifest.toml` note that pairs this fixture with the
   q5_1 default rationale.

Concrete examples of cells that look "wrong" against the published prior:

- `matrix.md:103-104` — small.en/q5_1 vs small/q5_1 on one host.
- `matrix.md:129`    — tiny.en/q5_1 on a single host.
- `matrix.md:257`    — large-v3-turbo cell with a single-fixture outlier.

These are calibration noise, not real model ranking. The registry trusts
the Open-ASR-Leaderboard mean because it averages across ~8 datasets and
hundreds of hours of audio; the self-fixture set is correspondingly tiny
and the worst-fixture gate amplifies its tail.

## Wizard implication

The wizard uses the registry's `wer_by_lang["en"]` (anchored to the OASR
mean) to bucket models into `Accurate / Adequate / Inaccurate`, and only
falls back to per-host calibration measurements when comparing two
otherwise-equivalent picks. This is the inversion-noise mitigation: the
public prior wins ties; the calibration matrix breaks them.
