# AGENTS.md — Fono Agent Orientation

## What is Fono?

Fono is a GPL-3.0 Rust single-binary voice dictation tool for the desktop. It replaces
[Tambourine](https://github.com/kstonekuan/tambourine-voice) (Tauri + Python) and
[OpenWhispr](https://github.com/OpenWhispr/openwhispr) (Electron) with a lighter native
stack — no WebKit, no Node, no Python — while unioning their feature sets (global hotkey
push-to-talk, local + cloud STT, optional polish, text injection, tray UI, history).
Target users: Linux desktop (i3 / sway / KDE / GNOME, X11 and Wayland), Windows, and macOS.

## Orientation: read in this order

1. `docs/plans/2026-04-24-fono-design-v1.md` — authoritative design and 10-phase
   implementation plan. This is the source of truth for *what to build and when*.
2. `docs/decisions/` — Architecture Decision Records (ADRs) explaining *why* key
   choices were made (language, name, license, default models).
3. `docs/status.md` — current phase, what's next, session log.
4. `CONTRIBUTING.md` — DCO sign-off requirement, formatting, and clippy rules.

## Current phase

**Phase 0 complete; Phase 1 next.** See `docs/status.md` for details.

## External references

- `/mnt/nvme0n1p5/Work/slackbuilds/earlyoom/` — NimbleX SlackBuild template to mirror
  when Phase 9 packaging lands.
- `/mnt/nvme0n1p5/Work/slackbuilds/tambourine-voice/` — earlier aborted attempt to
  package Tambourine on NimbleX; catalogued missing system deps (webkit2gtk-4.1,
  python3.13, uv, libxdo, libayatana-appindicator3). Useful reference for Phase 9
  dependency negotiation.
- <https://github.com/kstonekuan/tambourine-voice> and
  <https://github.com/OpenWhispr/openwhispr> — upstream projects whose feature union
  Fono is replicating.

## Voice model (ONNX) conversion & hosting — how it actually works

Everything needed to convert and host a new ONNX voice model exists **today**,
split across two repos (`fono` + `../fono-voice`). There is a
`.forge/skills/add-onnx-voice-model` skill with the full playbook, but this
section is the durable backup in case the skill is unavailable.

- **Two-repo split.** The conversion *scripts* live in **this** repo under
  `scripts/`; the *runtime build recipe, op-union config, and hosted assets*
  live in the `../fono-voice` mirror.
  - `scripts/gen-ort-models.sh` — `.onnx` → `.ort` (minimal-runtime flatbuffer)
    + emits a per-model `required_operators_and_types.config`. Drive it with
    `MODELS_DIR=… OUT_DIR=… PYTHON=… ALLOW_PARTIAL=1` for single-model probes.
  - `scripts/merge-ort-configs.py in1.config in2.config … out.config` — unions
    per-model configs (operators **and** per-op tensor types).
  - `scripts/build-onnxruntime-minimal.sh` — builds `libonnxruntime.a`/`.lib`
    from source, driven by the union config.
  - `scripts/fetch-onnxruntime.sh` — downloads prebuilt libs; holds the pinned
    SHA table (used by `ORT_LIB_LOCATION` / the size-budget gate).
  - `../fono-voice/onnxruntime/ops.config` — the **canonical union operator
    set** (must cover EVERY shipped model or the omitted model fails at load
    with `Could not find an implementation for <Op>(<opset>)`).
  - `../fono-voice/.github/workflows/build-onnxruntime.yml` — rebuilds the
    minimal runtime per triple; it checks out **this** repo for the build
    script. `../fono-voice/manifest.json` is the asset catalog (per asset:
    sha256, size, upstream URL + SHA, license). Assets live on ABI-tagged
    releases (`ort-<ver>` for models, `onnxruntime-<ver>` for libs).

- **The Python venv for conversion lives in THIS repo, not `fono-voice`.**
  `../fono-voice` has **no** venv. Use `tmp/venv/bin/python` (onnxruntime
  **1.24.2**, matching the `ort-sys` ABI pin) — `tmp/kokoro-venv` is
  equivalent; `.venv-wakeword` is onnxruntime 1.27.0 (wake-word *training*
  only, WRONG ABI for conversion). If `tmp/` gets pruned, recreate with
  `python3 -m venv tmp/venv && tmp/venv/bin/pip install onnxruntime==1.24.2
  flatbuffers numpy`. Conversion is **doable locally** — do not treat it as
  "blocked on tooling".

- **ABI lockstep (non-negotiable):** the python `onnxruntime` version MUST equal
  the version `ort-sys` links (currently **1.24.2**). The `.ort` flatbuffer
  schema and op config are version-coupled to the runtime.

- **Skip the runtime rebuild when ops don't change.** If a new model adds no
  net-new operators to `ops.config`, you only convert + host + index — no
  minimal-runtime rebuild. Confirm with a `merge-ort-configs.py` diff and
  `./tests/check.sh --size-budget`.

- **ReDimNet finding (2026-07-18, speaker verification Slice 1).** Probed
  `OpenVoiceOS/redimnet-b2-vox2-onnx` (B2 is a fair op-set proxy for the whole
  B0–B6 family — shared STFT front-end + GELU + L2-norm). Local conversion via
  `tmp/venv` succeeded and the union diff vs the current mirror `ops.config`
  shows **three net-new operators**, so shipping ReDimNet **DOES require a
  minimal-runtime rebuild** (not a convert-and-host-only case):
  - `ai.onnx;18;ReduceL2` (embedding L2-norm)
  - `ai.onnx;18;ReduceProd`
  - `com.microsoft;1;FastGelu` (fused GELU contrib op; distinct from the
    existing `Gelu`)
  Hosting layout for the pack (mirrors the openWakeWord family): size-tiered
  `redimnet-<tier>.ort` graphs + a `redimnet-<tier>.cohort.bin` impostor-cohort
  sidecar (~200 KB, for AS-Norm) + a `speaker_models[]` manifest section.

- **ReDimNet2 decision + export finding (2026-07-18).** We are targeting
  **ReDimNet2** (`PalabraAI/redimnet2`, Interspeech 2026, **MIT**), not v1 — it
  is a strictly better Pareto front (v2-B3 beats v1-B6 at ~1/4 the params).
  **Default pick: `b3`, dataset `vb2+vox2+cnc2_v0`, train_type `lm`** (the
  robust mixture-trained, large-margin weights); **B6 same recipe** as an
  optional max-accuracy tier. Pick the mixture + `lm` weights, never the
  `vox2`-only or `ptn` rows (those overfit / are un-polished). "Bad room" EER
  expectation: ~1–3 % realistic desktop, up to ~4 % (B3) / ~3 % (B6) in
  VOICES-like far-field.
  - **No ONNX ships upstream — you self-export** from the `.pt`. Do it in a
    **dedicated export venv** (NOT `tmp/venv`, which is the ABI-pinned .ort
    converter): `python3 -m venv tmp/redimnet2-export && pip install
    --index-url https://download.pytorch.org/whl/cpu torch==2.11.0
    torchaudio==2.11.0 && pip install onnx scipy numpy`. **torch/torchaudio
    versions must MATCH** — torchaudio caps at 2.11.0 on the cpu index (that
    mismatch is why `.venv-wakeword`'s torchaudio is broken). Load via the
    repo's `load_custom(...)`, run a forward, then `torch.onnx.export(...,
    opset_version=17, dynamo=False)` (waveform input `[N,T]`, dynamic time).
  - **Exported b3 op-diff vs mirror `ops.config` = three net-new ops → runtime
    rebuild required:** `ai.onnx;6;InstanceNormalization`, `ReduceProd`,
    `com.microsoft;1;FastGelu`. NOTE this differs from the v1 probe — v2's `tf`
    front-end uses conv/matmul framing (NO `STFT` op) and does **not** L2-norm
    in-graph (NO `ReduceL2`; Fono L2-normalises the embedding in Rust).
  - **Verified faithful:** torch vs onnxruntime-1.24.2 embedding cosine =
    1.000000, max abs diff 4e-6. **Embedding dim = 192.** fp32 ONNX ≈ 19 MB.
  - **Front-end parity (b3 `feat_type='tf'`, `TFMelBanks`):** 16 kHz, frame
    400 / hop 160 / n_fft 512, **Hann** window (symmetric, denom `N-1`), **72**
    mel bins, f 20–7600 Hz, HTK mel (`2595·log10(1+hz/700)`), **power**
    spectrum (`real²+imag²`, NO sqrt — the `fft_mode='abs'` label is misleading;
    the melbanks path never takes the root), natural log, per-utterance CMN.
    Upstream quirks to match only at Slice-5 oracle time: the DFT is a
    conv1d over `nfft/2` (256) truncated bins with `linspace(0, sr/2, 256)` mel
    spacing (not the standard `nfft/2+1` rfft grid), and a per-signal mean/std
    normalisation is applied before pre-emphasis. The Rust fbank in
    `crates/fono-audio/src/speaker.rs` is now reconciled to 72-mel / Hann /
    power / 20–7600 Hz; the two quirks above remain for Slice 5.

## Hard rules for agent sessions

- **Pre-commit gate (run, in order, before EVERY `git commit` and EVERY
  `git push`):**
  1. `cargo fmt --all -- --check` — must exit 0. If it fails, run
     `cargo fmt --all` and re-stage. Do **not** push fmt-dirty code; CI
     will reject it at the `cargo fmt --check` step (see
     `.github/workflows/ci.yml`). This caught us once on commit
     `33e3e51` — never again.
  2. `cargo clippy --workspace --all-targets -- -D warnings` — must
     exit 0. Same lint set as CI; passes locally ⇒ passes there. If CI
     stops at fmt it will *not* surface clippy errors, so always run
     clippy locally too.
  3. `cargo test --workspace --tests --lib` — must pass. (Skip doctests
     locally if your toolchain lacks `rustdoc`; CI runs them.)

  These three commands take under a minute on a warm target dir.
  Running them before pushing prevents the "push → wait 10 min → red CI
  → push fixup" loop. The agent is responsible for this gate; do not
  rely on the human to catch it.

- **Size-budget gate (run before EVERY `git push`, and before every tag
  / release):** `./tests/check.sh --size-budget`. This builds the
  canonical ship artefact (`release-slim`, glibc `cpu`, default
  features) and asserts the **exact** thing CI's `size-budget` job
  asserts — binary ≤ the `cpu` budget (currently 25 MiB / 26,214,400 B)
  and a four-entry `NEEDED` allowlist (libc, libm, libgcc_s, the dynamic
  linker). The numbers live in lockstep with the `ci.yml` `cpu` matrix
  rows; change them together. A green run here means the CI size gate
  will pass — so binary growth never surprises us at CI time. The flag
  runs **only** the size gate (it skips the fmt/build/clippy/test matrix
  and does its own dedicated build), so it composes with the three
  commands above. It pins `libonnxruntime.a` via `ORT_LIB_LOCATION`
  exactly as CI (auto-resolving through `scripts/fetch-onnxruntime.sh`
  when the env var is unset). If the binary is over budget, fix the
  growth or, with sign-off, bump the `cpu` row in both `ci.yml` and
  ADR 0022 (hard cap ≤ 28 MiB) — never silently exceed it.

  Style note: `rustfmt.toml` sets `use_small_heuristics = "Max"` so
  short fn calls / struct literals / if-else expressions stay on one
  line when they fit in `max_width = 100`. Compact code is preferred.
  For the rare case rustfmt insists on expanding a genuinely tasteful
  one-liner (e.g. `fn ok(s: &str) -> String { paint("32", s) }`),
  prefix the item with `#[rustfmt::skip]` rather than fighting the
  formatter codebase-wide.

- All commits **MUST** be signed off (`git commit -s`) — DCO enforced by CI.
- **NEVER** add a `Co-authored-by: Forge <forge@noreply.local>` trailer (or any
  agent / assistant co-author trailer) to commit messages — not on new commits,
  not when rewording, not when squashing. The agent is a tool, not an author.
  When squashing history, strip any pre-existing `Co-authored-by: Forge …`
  lines from the combined message. This rule is permanent.
- **Commit messages MUST be user-friendly.** Write the subject and body so a
  non-expert user reading the changelog / release notes / `git log` can easily
  understand *what changed for them and why* — describe the behaviour or
  benefit in plain language, not the internal mechanics. Prefer e.g.
  "Make the wake word trigger more reliably and stop false activations" over
  "Replace score smoother with sliding-window activation gate". Keep jargon,
  type/function names, and implementation detail out of the subject line; if
  such detail is useful, put it lower in the body. This rule is permanent.
- Every Rust source file **MUST** start with `// SPDX-License-Identifier: GPL-3.0-only`
  on line 1.
- Do **NOT** add dependencies without updating `deny.toml` and verifying the licenses
  are compatible with GPL-3.0.
- **Binary size is the top priority.** Do **NOT** introduce a dependency that is
  *new to the project* — i.e. a crate not already present in `Cargo.lock` /
  the binary's dependency graph — without flagging it first and getting explicit
  sign-off, because it grows the shipped single binary (the size budget is
  enforced by the CI size-budget gate). When you do flag one, state the expected
  size impact. Adding an *already-present* dependency as a new edge (e.g. a crate
  the binary already links transitively) is net-zero on binary size and does
  **not** need flagging — just proceed. Check with
  `cargo tree -p fono -i <crate>` if unsure whether a crate is already in the graph.
- Do **NOT** add Llama-family or non-OSI/custom-license Gemma models as defaults.
  Gemma models may be defaults only when the specific artifact and its upstream base
  model are published under an OSI-approved, GPL-3.0-compatible license such as
  Apache-2.0, with no extra use restrictions. Other Llama/Gemma variants remain
  opt-in only. (See `docs/decisions/0004-default-models.md`.)
- Do **NOT** run `git push` unless the user explicitly says to push. Commit and report
  what is staged; wait for the push instruction.
- Batch related doc/plan changes into a **single commit**. Do not make multiple
  incremental commits for what is one logical documentation change.
- Do **NOT** attempt to install system packages on NimbleX. Document required deps in
  `docs/providers.md` or the SlackBuild `REQUIRES=` and let the user install them.
- Work **one phase at a time**; tick checkboxes in the design plan as you go; update
  `docs/status.md` at the end of every session.
- For every release: add a `## [X.Y.Z] — YYYY-MM-DD` section to `CHANGELOG.md`
  **before** tagging. The release workflow extracts that section into the GitHub
  Release body via `body_path: release/RELEASE_NOTES.md`
  (`.github/workflows/release.yml`). A missing section yields a fallback one-liner
  body and a CI warning — don't ship without the changelog entry.
- For every release: also update `ROADMAP.md` **before** tagging. Move every
  roadmap item that ships in the release from the **In progress** / **Planned**
  sections into **Shipped** at the bottom, annotated with the release tag and
  date (`*vX.Y.Z, YYYY-MM-DD.*`). The roadmap is published at the repo root and
  linked from the README and the project site; keeping it in sync at tag time
  is non-negotiable.

## Next-step template

Typical user invocation for a new session:

> "Continue from Phase N per docs/plans/2026-04-24-fono-design-v1.md; update
> status.md when done."

<!-- fono-voice-preset -->
This preset is tuned for coding agents; other domains (e.g. Home
Assistant) will get their own preset when they land.

You are in VOICE MODE. The user is listening AND has the chat
window visible on screen. Treat the two channels differently.

Two channels, one turn:
- **Spoken channel (`fono.speak`)**: short, conversational, the way
  you'd actually talk. One to three sentences. No lists read aloud,
  no paths, no command names spelled out, no "firstly / secondly".
  Contractions are fine. If something is long or technical, say
  "details are on screen" and stop.
- **Written channel (the chat reply)**: the place for the full
  detail — file paths, command output summaries, next-step lists,
  diffs-by-reference. The user reads this when they want depth.

EVERY turn — including the very first reply of a session — MUST
call `fono.speak`. No exceptions: greetings, acknowledgements, and
"I'm here" responses all go through `fono.speak`. If you do not
call `fono.speak`, the user hears nothing. The spoken text and the
written text are NOT the same string; never paste the written
reply verbatim into `fono.speak`. Never speak code blocks, tables,
file paths, or long identifiers — refer to them as "the preset
file" or "the AGENTS doc" out loud and put the exact path in the
written reply.

Three turn-ending modes — pick one per turn. **Mode R is the
default; only switch to L or C when you genuinely need an answer
from the user to make progress on the current task.** Curiosity,
politeness, "leaving the door open", or wanting reaction to
information you just delivered are NOT reasons to open the mic —
that is what mode R already provides. Opening the mic without a
real pending question is a UX bug: it forces the user to either
ignore a hot mic or invent a reply they didn't owe you.

- **R. Read (default — no answer needed).** Use this ending
  whenever the current turn does not contain a genuine pending
  question the user must answer for you to continue. Concretely:
  (a) reporting completion / status / findings, (b) delivering
  information or analysis the user asked for, (c) the question
  is too complex for someone juggling other things, or (d) the
  action under discussion is destructive / irreversible / has
  real-world side effects the user couldn't undo by saying
  "never mind". `fono.speak` the big picture, end with a
  no-pressure handoff like "ready when you are" OR just a clean
  full stop, and STOP. No capture tool. Test before reaching for
  L or C: *"if the user says nothing for the next minute, am I
  blocked?"* If no, mode R.
- **L. Listen.** Only when this turn ends with a real question
  the user needs to answer for you to make progress — picking
  between options you've laid out, naming a thing, resolving an
  ambiguity, accepting or rejecting a concrete proposal. Call
  `fono.listen` with a `context` argument describing the kind of
  answer you expect, so the background-speech filter can ignore
  the radio / TV / side conversation. The model parses "A", a
  longer reasoned answer, or a counter-proposal equally well.
  Do not chain listens turn after turn just because the
  conversation is interesting; if you've delivered an answer and
  are merely curious what the user thinks, that is mode R.
- **C. Confirm (UX shortcut, NOT a safety gate).** Only when the
  answer is naturally one of a small fixed set (≤ ~4 options) and
  the user shouldn't have to think about phrasing. Call
  `fono.confirm` with the labels. Do not reach for confirm just
  to make a risky action feel safer — that's mode R's job.

Three hard rules:

1. **Refocus preamble.** Every `fono.speak` call opens with a
   1–2 second attention-grab — a one- or two-word cue, optionally
   naming the topic — that buys the user time to switch back in
   before the substance starts. Vary it; examples: "Right —",
   "Okay, on the preset —", "Back to you —", "Quick one —".
   Never start cold with the answer. Translate the *intent* (a
   short opener) into the user's spoken language; don't literally
   translate the English phrase.
2. **No bare spoken questions.** If a spoken turn ends in a
   question mark, the same turn MUST include a `fono.listen` or
   `fono.confirm` call. Either ask and capture, or narrate and
   stop.
3. **No voice authorisation for destructive or irreversible
   actions.** Never use `fono.listen` or `fono.confirm` to
   authorise things the user couldn't easily undo (delete,
   force-push, deploy, drop, overwrite, reset, and equivalents).
   Describe what would happen, point at the screen,
   let the user trigger it manually. Reversible side effects
   (picking a build mode, naming a flag) are fine via
   listen/confirm.

Language:
- Match the user's spoken language in `fono.speak` — if they
  speak Romanian, French, German, etc., speak back in that
  language so the conversation feels natural.
- Everything you **write** stays in English regardless of the
  spoken language: source code, identifiers, comments, commit
  messages, config keys and values, file and directory names,
  documentation files, log messages, and any text the chat
  reply contains. English is the project's lingua franca and
  the only language code reviewers and CI see.
- If the user dictates a string that is clearly meant to land
  verbatim in a file (a UI label, a translation, a test
  fixture), keep it in the language they gave — but the
  surrounding code, the variable names, and the commit message
  are still English.

Brevity > caveats. Be willing to be wrong fast.

When the user wants more input from you (asks a follow-up, says
"keep going"), call `fono.listen` to capture their next
instruction.

<!-- /fono-voice-preset -->
