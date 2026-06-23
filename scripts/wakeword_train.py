#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
# Fono wake-word: train + export the per-phrase classifier.
#
# Phase B of plans/2026-06-23-wake-word-openwakeword-v2.md. Invoked by
# scripts/train-wakeword-model.sh (which synthesizes the POSITIVE clips with
# Fono itself — `fono speak` across the active TTS backend's voices — handles
# tool DETECTION, and does the .onnx -> .ort conversion). This module owns the
# model-specific steps that need numpy/torch/openwakeword:
#
#   1. Load the POSITIVE clips Fono already synthesized into --positives-dir
#      and AUGMENT them (speed + gain perturbation) up to --n-positive, so a
#      handful of distinct voices yields a usable positive set.
#   2. Compute features for positives + negatives through the FROZEN Apache
#      graphs (melspectrogram.onnx + the Apache Google speech_embedding
#      backbone) via openwakeword's feature extractor — the SAME graphs the
#      runtime detector loads (crates/fono-audio/src/wakeword.rs).
#   3. Train the small per-phrase classifier head on those features
#      (target bar: < ~0.5 false-accepts/hour, < ~5% false-reject).
#   4. Export the trained classifier to <model-id>.onnx in the runtime's exact
#      [1, 16, 96] -> [1, 1] score contract.
#
# This script DETECTS its dependencies and EXITS with the exact install
# command if any are missing — it never installs anything itself (AGENTS.md:
# no auto-install). It fabricates nothing: with no real data it cannot and must
# not emit a usable model.
#
# Run via the shell driver, not directly:
#   sh scripts/train-wakeword-model.sh
import argparse
import sys
import wave
from pathlib import Path


def fail(msg: str, *, install: str | None = None) -> "NoReturn":  # noqa: F821
    print(f"[wakeword_train] ERROR: {msg}", file=sys.stderr)
    if install:
        print("[wakeword_train] install it in your OWN venv (not installed", file=sys.stderr)
        print(f"[wakeword_train] here): {install}", file=sys.stderr)
    sys.exit(1)


def require(module: str, install: str) -> None:
    """Detect an importable dependency; instruct + exit if absent."""
    try:
        __import__(module)
    except ImportError:
        fail(f"python package '{module}' is not importable", install=install)


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Train the wake-word classifier from Fono-synthesized positives.")
    p.add_argument("--phrase", required=True, help='wake phrase, e.g. "hey fono" (for messages/provenance)')
    p.add_argument("--model-id", required=True, help="registry id / output basename")
    p.add_argument("--melspec", required=True, type=Path, help="frozen Apache melspectrogram.onnx")
    p.add_argument("--embedding", required=True, type=Path, help="frozen Apache embedding_model.onnx")
    p.add_argument(
        "--positives-dir",
        required=True,
        type=Path,
        help="dir of POSITIVE wake-phrase WAVs synthesized by `fono speak`",
    )
    p.add_argument("--negative-audio-dir", default="", help="dir of openly-licensed negatives")
    p.add_argument("--negative-features-dir", default="", help="dir of pre-computed negative .npy")
    p.add_argument(
        "--n-positive",
        type=int,
        default=2000,
        help="target number of positive clips after augmentation",
    )
    p.add_argument("--n-validation", type=int, default=200)
    p.add_argument("--work-dir", required=True, type=Path)
    p.add_argument("--out-dir", required=True, type=Path)
    return p.parse_args()


# --- Audio / feature contract ---------------------------------------------
# The runtime detector (crates/fono-audio/src/wakeword.rs) feeds the frozen
# melspectrogram + speech_embedding graphs and runs a classifier over a window
# of CLASSIFIER_WINDOW_EMB (=16) embeddings of EMBED_DIM (=96), reading the
# classifier's first output as the score. A 2.0 s clip yields exactly 16
# embedding windows, so every clip is padded/cropped to WINDOW_SAMPLES and the
# exported ONNX takes [1, 16, 96] and emits a single 0..1 score.
CLASSIFIER_WINDOW_EMB = 16
EMBED_DIM = 96
WINDOW_SAMPLES = 32000  # 2.0 s @ 16 kHz -> exactly 16 embedding windows


def _to_mono_16k(pcm16, rate: int, np):
    """Resample int16 PCM to 16 kHz (linear; adequate for training augmentation)."""
    if rate == 16000 or len(pcm16) <= 1:
        return pcm16.astype("<i2", copy=False)
    n_out = int(round(len(pcm16) * 16000 / rate))
    if n_out <= 1:
        return pcm16.astype("<i2", copy=False)
    x_old = np.linspace(0.0, 1.0, num=len(pcm16), endpoint=False)
    x_new = np.linspace(0.0, 1.0, num=n_out, endpoint=False)
    out = np.interp(x_new, x_old, pcm16.astype(np.float32))
    return np.clip(out, -32768, 32767).astype("<i2")


def _read_wav_16k_mono(path: Path, np):
    with wave.open(str(path), "rb") as w:
        ch, rate, width = w.getnchannels(), w.getframerate(), w.getsampwidth()
        frames = w.readframes(w.getnframes())
    if width != 2:
        raise RuntimeError(f"{path.name}: {width * 8}-bit (need 16-bit PCM wav)")
    data = np.frombuffer(frames, dtype="<i2")
    if ch > 1:
        data = data.reshape(-1, ch).mean(axis=1).astype("<i2")
    return _to_mono_16k(data, rate, np)


def _fixed_window(pcm16, np):
    """Centre-crop or zero-pad a clip to exactly WINDOW_SAMPLES."""
    if len(pcm16) >= WINDOW_SAMPLES:
        start = (len(pcm16) - WINDOW_SAMPLES) // 2
        return pcm16[start : start + WINDOW_SAMPLES]
    out = np.zeros(WINDOW_SAMPLES, dtype="<i2")
    off = (WINDOW_SAMPLES - len(pcm16)) // 2
    out[off : off + len(pcm16)] = pcm16
    return out


def _windows_from_clip(pcm16, np, max_windows: int):
    """Slice a long clip into up to max_windows non-overlapping 2 s windows."""
    n = len(pcm16) // WINDOW_SAMPLES
    if n == 0:
        return [_fixed_window(pcm16, np)]
    return [pcm16[i * WINDOW_SAMPLES : (i + 1) * WINDOW_SAMPLES] for i in range(min(n, max_windows))]


def _augment(pcm16, np, rng):
    """One speed + gain perturbed variant of a clip (int16, 16 kHz)."""
    speed = rng.uniform(0.90, 1.15)
    n_out = max(2, int(round(len(pcm16) / speed)))
    x_old = np.linspace(0.0, 1.0, num=len(pcm16), endpoint=False)
    x_new = np.linspace(0.0, 1.0, num=n_out, endpoint=False)
    out = np.interp(x_new, x_old, pcm16.astype(np.float32))
    out *= rng.uniform(0.85, 1.10)  # gain
    return np.clip(out, -32768, 32767).astype("<i2")


def _embed_clips(af, clips, np):
    """Return [N, 16, 96] embedding features for a list of int16 window arrays."""
    if not clips:
        return np.empty((0, CLASSIFIER_WINDOW_EMB, EMBED_DIM), "float32")
    batch = np.stack(clips).astype("<i2")
    # Embed in chunks so a large set reports progress instead of appearing to
    # hang inside one big embed_clips call.
    total = len(batch)
    chunk = 512
    parts = []
    for start in range(0, total, chunk):
        parts.append(np.asarray(af.embed_clips(batch[start : start + chunk], batch_size=64)))
        done = min(start + chunk, total)
        if total > chunk:
            pct = (100 * done) // total
            print(f"[wakeword_train]   embedding {done}/{total} windows ({pct}%)")
    emb = np.concatenate(parts, axis=0) if len(parts) > 1 else parts[0]
    # Normalise to exactly [N, 16, 96] (pad/trim the window axis if needed).
    if emb.ndim != 3 or emb.shape[1] != CLASSIFIER_WINDOW_EMB:
        fixed = np.zeros((emb.shape[0], CLASSIFIER_WINDOW_EMB, EMBED_DIM), "float32")
        if emb.ndim == 3:
            w = min(emb.shape[1], CLASSIFIER_WINDOW_EMB)
            fixed[:, :w, :] = emb[:, :w, :]
        emb = fixed
    return emb.astype("float32")


def _positive_windows(positives_dir: Path, n_target: int, np) -> list:
    """Load Fono-synthesized base positives and augment up to n_target windows."""
    pos_wavs = sorted(positives_dir.rglob("*.wav"))
    if not pos_wavs:
        fail(
            f"no positive .wav clips found under {positives_dir}. The driver "
            "synthesizes these with `fono speak`; check that step succeeded."
        )
    base = []
    for p in pos_wavs:
        try:
            base.append(_read_wav_16k_mono(p, np))
        except Exception as exc:  # noqa: BLE001 - skip a bad file, keep going
            print(f"[wakeword_train] skip {p}: {exc}", file=sys.stderr)
    if not base:
        fail(f"every positive clip under {positives_dir} failed to decode")

    print(f"[wakeword_train] {len(base)} base positive voice clip(s); augmenting -> {n_target}")
    clips = [_fixed_window(b, np) for b in base]  # the originals first
    rng = np.random.default_rng(0)
    i = 0
    while len(clips) < n_target:
        clips.append(_fixed_window(_augment(base[i % len(base)], np, rng), np))
        i += 1
        if len(clips) % 500 == 0:
            print(f"[wakeword_train]   augmented {len(clips)}/{n_target}")
    return clips


def _negative_windows(neg_audio: str, np) -> list:
    """Load negative WAVs into up to 8 windows each."""
    neg_wavs = sorted(Path(neg_audio).rglob("*.wav"))
    if not neg_wavs:
        fail(f"--negative-audio-dir has no .wav files: {neg_audio}")
    print(f"[wakeword_train] reading {len(neg_wavs)} negative file(s)")
    clips = []
    for idx, p in enumerate(neg_wavs, 1):
        try:
            pcm = _read_wav_16k_mono(p, np)
        except Exception as exc:  # noqa: BLE001 - skip a bad file, keep going
            print(f"[wakeword_train] skip {p}: {exc}", file=sys.stderr)
            continue
        clips.extend(_windows_from_clip(pcm, np, 8))
        if len(neg_wavs) >= 500 and (idx % 500 == 0 or idx == len(neg_wavs)):
            print(f"[wakeword_train]   read {idx}/{len(neg_wavs)} clip(s)")
    return clips


def _train_and_export_classifier(pos, neg, out_onnx: Path, np) -> None:
    """Train a small head on [16, 96] embeddings and export the [1,16,96]->score ONNX."""
    import torch
    from torch import nn

    torch.manual_seed(0)
    feats = np.concatenate([pos, neg]).astype("float32")
    labels = np.concatenate([np.ones(len(pos)), np.zeros(len(neg))]).astype("float32")
    x_all = torch.from_numpy(feats)
    y_all = torch.from_numpy(labels).unsqueeze(1)
    # Up-weight the (scarcer) positives to counter class imbalance.
    pos_w = max(1.0, len(neg) / max(1, len(pos)))
    sample_w = torch.where(y_all > 0.5, torch.tensor(pos_w), torch.tensor(1.0))

    model = nn.Sequential(
        nn.Flatten(),
        nn.Linear(CLASSIFIER_WINDOW_EMB * EMBED_DIM, 128),
        nn.ReLU(),
        nn.Linear(128, 64),
        nn.ReLU(),
        nn.Linear(64, 1),
        nn.Sigmoid(),
    )
    opt = torch.optim.Adam(model.parameters(), lr=1e-3)
    loss_fn = nn.BCELoss(reduction="none")
    model.train()
    for epoch in range(200):
        opt.zero_grad()
        out = model(x_all)
        loss = (loss_fn(out, y_all) * sample_w).mean()
        loss.backward()
        opt.step()
        if (epoch + 1) % 50 == 0:
            with torch.no_grad():
                acc = ((out > 0.5).float() == y_all).float().mean().item()
            print(f"[wakeword_train]   epoch {epoch + 1}: loss={loss.item():.4f} acc={acc:.3f}")

    model.eval()
    out_onnx.parent.mkdir(parents=True, exist_ok=True)
    dummy = torch.randn(1, CLASSIFIER_WINDOW_EMB, EMBED_DIM)
    # dynamo=False uses the stable TorchScript exporter, which needs no extra
    # onnxscript dependency and is sufficient for this small static head.
    torch.onnx.export(
        model,
        dummy,
        str(out_onnx),
        input_names=["x"],
        output_names=["score"],
        dynamic_axes={"x": {0: "batch"}, "score": {0: "batch"}},
        opset_version=17,
        dynamo=False,
    )


def main() -> int:
    args = parse_args()

    # numpy is needed by augmentation and feature extraction; the heavy training
    # deps (openwakeword/torch/onnx) are detected just before they are used.
    require("numpy", "pip install numpy")
    require("openwakeword", "pip install openwakeword")
    require("torch", "pip install torch")
    require("onnx", "pip install onnx")

    for graph in (args.melspec, args.embedding):
        if not graph.is_file():
            fail(f"frozen graph not found: {graph}")

    neg_audio = args.negative_audio_dir
    neg_feat = args.negative_features_dir
    if not neg_audio and not neg_feat:
        fail(
            "no negatives provided (--negative-audio-dir / --negative-features-dir). "
            "Supply an OPENLY-LICENSED negative corpus; see "
            "calibration/wakeword/README.md."
        )

    args.work_dir.mkdir(parents=True, exist_ok=True)
    args.out_dir.mkdir(parents=True, exist_ok=True)

    # --- Features through the frozen Apache graphs -----------------------
    # AudioFeatures wraps the SAME melspectrogram + embedding ONNX graphs the
    # runtime detector loads, so the embeddings we train on match what
    # OnnxWakeWord computes at inference (crates/fono-audio/src/wakeword.rs).
    import numpy as np
    from openwakeword.utils import AudioFeatures

    af = AudioFeatures(
        melspec_onnx_model_path=str(args.melspec),
        embedding_onnx_model_path=str(args.embedding),
        ncpu=1,
    )

    pos_clips = _positive_windows(args.positives_dir, max(1, args.n_positive), np)
    print(f"[wakeword_train] computing features for {len(pos_clips)} positive window(s)")
    pos_feats = _embed_clips(af, pos_clips, np)

    neg_feats_list = []
    if neg_audio:
        neg_feats_list.append(_embed_clips(af, _negative_windows(neg_audio, np), np))
    if neg_feat:
        for p in sorted(Path(neg_feat).rglob("*.npy")):
            arr = np.load(p).reshape(-1, CLASSIFIER_WINDOW_EMB, EMBED_DIM).astype("float32")
            neg_feats_list.append(arr)
    neg_feats = (
        np.concatenate(neg_feats_list)
        if neg_feats_list
        else np.empty((0, CLASSIFIER_WINDOW_EMB, EMBED_DIM), "float32")
    )
    if len(pos_feats) == 0 or len(neg_feats) == 0:
        fail(
            f"insufficient features (positive={len(pos_feats)}, negative={len(neg_feats)}); "
            "check the positive clips and the negative corpus."
        )
    print(
        f"[wakeword_train] features: {len(pos_feats)} positive, "
        f"{len(neg_feats)} negative windows"
    )

    # --- Train the per-phrase classifier head + export ONNX --------------
    out_onnx = args.out_dir / f"{args.model_id}.onnx"
    print(f"[wakeword_train] training classifier head -> {out_onnx}")
    _train_and_export_classifier(pos_feats, neg_feats, out_onnx, np)
    print(f"[wakeword_train] wrote classifier: {out_onnx}")
    print(
        "[wakeword_train] NOTE: model quality depends entirely on the number and "
        "diversity of voices Fono synthesized and the size/realism of the negative "
        "corpus. Validate against the < ~0.5 false-accepts/hour, < ~5% false-reject "
        "bar before relying on it; add more voices/negatives if it falls short."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
