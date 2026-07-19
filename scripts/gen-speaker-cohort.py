#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Build the AS-Norm impostor-cohort sidecar (<model>.cohort.bin) from
Mozilla Common Voice audio.

Two subcommands, mirroring the plan's "pinned selection + model-agnostic
generation" split (plans/2026-07-19-speaker-verification-calibration-ux-v2.md,
Step 1):

  select    Pick a stratified, deterministic speaker set from one or more
            extracted Common Voice language directories and write the
            selection manifest (TSV, committed to the repo for provenance
            and future-model regeneration).

  generate  Embed every clip in a selection manifest through a given .ort
            speaker-embedding graph and serialise the per-speaker mean
            embeddings as a .cohort.bin sidecar.

The .cohort.bin format matches the daemon loader (load_cohort in
crates/fono/src/daemon.rs): u32 LE row count, u32 LE embedding dim, then
rows*dim little-endian f32 raw embeddings (Cohort::from_raw normalises).

Run `generate` with the ABI-pinned converter venv (onnxruntime must equal the
version ort-sys links, currently 1.24.2):

  tmp/venv/bin/python scripts/gen-speaker-cohort.py select \
      --corpus ro=/data/cv/cv-corpus-21.0-ro --quota ro=130 \
      --corpus en=/data/cv/cv-corpus-21.0-en --quota en=150 \
      --release cv-corpus-21.0-2026-03-05 \
      --out calibration/speaker-cohort/selection.tsv

  tmp/venv/bin/python scripts/gen-speaker-cohort.py generate \
      --manifest calibration/speaker-cohort/selection.tsv \
      --corpus ro=/data/cv/cv-corpus-21.0-ro \
      --corpus en=/data/cv/cv-corpus-21.0-en \
      --model ~/.cache/fono/models/speaker/redimnet2-b3.ort \
      --out redimnet2-b3.cohort.bin

Requires ffmpeg on PATH (mp3 -> 16 kHz mono f32 PCM decode).
"""

import argparse
import csv
import hashlib
import random
import struct
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

SAMPLE_RATE = 16_000
MIN_CLIP_SECS = 2.0  # skip clips too short to embed meaningfully
MAX_CLIP_SECS = 12.0  # truncate very long clips; matches enrollment scale


def parse_kv(pairs, what):
    out = {}
    for p in pairs:
        if "=" not in p:
            sys.exit(f"--{what} expects LANG=VALUE, got: {p}")
        k, v = p.split("=", 1)
        out[k] = v
    return out


def cmd_select(args):
    corpora = parse_kv(args.corpus, "corpus")
    quotas = {k: int(v) for k, v in parse_kv(args.quota, "quota").items()}
    missing = set(quotas) - set(corpora)
    if missing:
        sys.exit(f"quota given for language(s) without --corpus: {sorted(missing)}")

    rng = random.Random(args.seed)
    rows = []  # (lang, client_id, clip filename)
    for lang, quota in sorted(quotas.items()):
        root = Path(corpora[lang])
        tsv = root / "validated.tsv"
        if not tsv.is_file():
            sys.exit(f"{tsv} not found (extract the Common Voice tarball first)")
        by_speaker = defaultdict(list)
        with open(tsv, newline="", encoding="utf-8") as f:
            for rec in csv.DictReader(f, delimiter="\t"):
                by_speaker[rec["client_id"]].append(rec["path"])
        eligible = {s: c for s, c in by_speaker.items() if len(c) >= args.min_clips}
        if len(eligible) < quota:
            sys.exit(
                f"{lang}: only {len(eligible)} speakers have >= {args.min_clips} "
                f"validated clips; quota is {quota}"
            )
        # Deterministic: sort speakers, seeded sample, seeded clip pick.
        speakers = rng.sample(sorted(eligible), quota)
        for spk in speakers:
            clips = sorted(eligible[spk])
            picked = rng.sample(clips, min(args.clips_per_speaker, len(clips)))
            # Short stable speaker tag: manifest stays greppable, and the
            # full 128-hex client_id is overkill for provenance.
            tag = hashlib.sha256(spk.encode()).hexdigest()[:16]
            rows.extend((lang, tag, clip) for clip in sorted(picked))

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with open(out, "w", encoding="utf-8") as f:
        f.write("# Speaker-cohort selection manifest (committed for provenance).\n")
        f.write(f"# source: Mozilla Common Voice (CC0), release {args.release}\n")
        f.write(f"# selection: seed={args.seed} min_clips={args.min_clips} ")
        f.write(f"clips_per_speaker={args.clips_per_speaker} ")
        f.write(f"quotas={','.join(f'{k}={v}' for k, v in sorted(quotas.items()))}\n")
        f.write("# columns: lang\tspeaker(sha256[:16] of client_id)\tclip\n")
        for lang, spk, clip in rows:
            f.write(f"{lang}\t{spk}\t{clip}\n")
    n_speakers = len({(l, s) for l, s, _ in rows})
    print(f"wrote {out}: {n_speakers} speakers, {len(rows)} clips")


def decode_clip(path):
    """mp3/anything -> f32 mono 16 kHz in [-1,1] via ffmpeg."""
    p = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(path), "-f", "f32le", "-ac", "1",
         "-ar", str(SAMPLE_RATE), "-"],
        capture_output=True,
    )
    if p.returncode != 0:
        raise RuntimeError(f"ffmpeg failed on {path}: {p.stderr.decode().strip()}")
    import numpy as np

    pcm = np.frombuffer(p.stdout, dtype=np.float32)
    return pcm[: int(MAX_CLIP_SECS * SAMPLE_RATE)]


def cmd_generate(args):
    import numpy as np
    import onnxruntime as ort

    corpora = parse_kv(args.corpus, "corpus")
    sess = ort.InferenceSession(args.model, providers=["CPUExecutionProvider"])
    input_name = sess.get_inputs()[0].name

    speakers = defaultdict(list)  # (lang, spk) -> [clip embedding]
    skipped = 0
    with open(args.manifest, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            lang, spk, clip = line.split("\t")
            if lang not in corpora:
                sys.exit(f"manifest language '{lang}' has no --corpus mapping")
            pcm = decode_clip(Path(corpora[lang]) / "clips" / clip)
            if len(pcm) < MIN_CLIP_SECS * SAMPLE_RATE:
                skipped += 1
                continue
            (emb,) = sess.run(None, {input_name: pcm[np.newaxis, :]})
            v = emb[0].astype(np.float32)
            v /= np.linalg.norm(v) or 1.0  # L2 per clip before averaging
            speakers[(lang, spk)].append(v)

    rows = [np.mean(np.stack(e), axis=0) for _, e in sorted(speakers.items()) if e]
    if not rows:
        sys.exit("no embeddings produced")
    dim = len(rows[0])
    with open(args.out, "wb") as f:
        f.write(struct.pack("<II", len(rows), dim))
        for r in rows:
            f.write(np.asarray(r, dtype="<f4").tobytes())
    print(f"wrote {args.out}: {len(rows)} speakers x dim {dim} ({skipped} clips skipped)")
    print(f"sha256: {hashlib.sha256(Path(args.out).read_bytes()).hexdigest()}")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("select", help="pick speakers, write the selection manifest")
    s.add_argument("--corpus", action="append", required=True, metavar="LANG=DIR",
                   help="extracted Common Voice language dir (repeatable)")
    s.add_argument("--quota", action="append", required=True, metavar="LANG=N",
                   help="speakers to select for a language (repeatable)")
    s.add_argument("--release", required=True, help="Common Voice release id (provenance)")
    s.add_argument("--min-clips", type=int, default=3)
    s.add_argument("--clips-per-speaker", type=int, default=5)
    s.add_argument("--seed", type=int, default=42)
    s.add_argument("--out", required=True)
    s.set_defaults(func=cmd_select)

    g = sub.add_parser("generate", help="embed manifest clips, write .cohort.bin")
    g.add_argument("--manifest", required=True)
    g.add_argument("--corpus", action="append", required=True, metavar="LANG=DIR")
    g.add_argument("--model", required=True, help="path to the .ort embedding graph")
    g.add_argument("--out", required=True)
    g.set_defaults(func=cmd_generate)

    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
