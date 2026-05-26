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
- Every Rust source file **MUST** start with `// SPDX-License-Identifier: GPL-3.0-only`
  on line 1.
- Do **NOT** add dependencies without updating `deny.toml` and verifying the licenses
  are compatible with GPL-3.0.
- Do **NOT** add Llama-family or Gemma models as defaults — their licenses are not
  OSI-approved. Opt-in only. (See `docs/decisions/0004-default-models.md`.)
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
## Voice mode (Fono)

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

Rules:
- EVERY turn — including the very first reply of a session — MUST
  call `fono.speak`. No exceptions: greetings, acknowledgements,
  and "I'm here" responses all go through `fono.speak`. If you do
  not call `fono.speak`, the user hears nothing.
- The spoken text and the written text are NOT the same string.
  Speak the conversational summary; write the detailed version in
  the chat reply. Never paste the written reply verbatim into
  `fono.speak` — that produces stilted, read-aloud prose.
- Never speak code blocks, tables, file paths, or long identifiers.
  Refer to them as "the preset file" or "the AGENTS doc" out loud;
  put the exact path in the written reply.
- When you have multiple paths forward, offer them as A/B/C and
  call the `fono.confirm` tool with the choices array. Prefer
  `fono.confirm` over a free-form `fono.listen` whenever the
  decision is bounded — it's faster for the user, the spoken
  answer maps cleanly to one of the labels, and Fono flashes both
  the overlay and the tray so the user knows you're waiting on
  them. STOP after the call.
- When you DO need a free-form answer via `fono.listen`, ALWAYS
  pass a `context` argument describing the kind of answer you're
  expecting — e.g. the question text itself, or
  `"asking the user for their favourite colour"`. Fono uses this
  to filter out background speech (radio, TV, side conversation)
  so an unrelated voice in the room doesn't get fed back to you
  as the user's reply. Skipping `context` works but degrades the
  filter to the cheap heuristic-only path.
- End each spoken turn with a one-line cue that hands the turn
  back: a question, "your turn", or "ready when you are".

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
