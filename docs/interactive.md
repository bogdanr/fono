# Live (interactive) dictation

> **Status:** Slice A — record-then-replay. The streaming decoder, the
> two-lane preview/finalize architecture, the live overlay, and the
> equivalence harness all land in Slice A. The realtime cpal-callback
> push (so the overlay paints text **while** you speak) lands in
> Slice B. See `docs/decisions/0009-interactive-live-dictation.md` for
> the full design rationale.

## What live mode is

Live mode is fono's streaming dictation pipeline. Instead of recording
the whole utterance and running whisper end-to-end at the end, fono
feeds 30 ms PCM frames into a *streaming* whisper decoder that emits
two kinds of `TranscriptUpdate`:

- **Preview** — speculative low-latency text. Re-emitted as more audio
  arrives. Rendered into the live overlay in a dimmed colour.
- **Finalize** — authoritative text for one VAD-bounded segment.
  Committed to history, never overwritten.

The full transcript the user sees is the concatenation of every
`Finalize` update, in segment order. The cleaned-up version (LLM
cleanup) runs once at end on the assembled text and is what gets
typed into the focused window.

## Slice A limitations (read this before you file a bug)

- **Record-then-replay.** Live mode currently captures all PCM first,
  then replays it through the streaming pipeline. The preview pane
  paints *after* you release the hotkey, not while you speak. Slice
  B turns on the realtime push.
- **Slim builds need a rebuild.** Live code is gated behind the
  `interactive` cargo feature so the default slim build stays slim
  (no streaming code, no broadcast channels, no extra deps). To use
  live mode you currently need a build with the feature compiled in:

  ```bash
  cargo build --release --features tray,interactive
  ```

  A future release will ship `interactive` in the default feature set
  once the realtime push lands.
- **Local STT only.** Slice A wires whisper.cpp's streaming lane.
  Cloud streaming (Groq, OpenAI realtime, Deepgram, AssemblyAI) lands
  in Slice B alongside the equivalence harness's cloud rows.
- **LLM cleanup is not streamed.** It runs once on the full
  transcript after the hotkey releases. This is a deliberate design
  decision — see ADR 0009 §4.

## Enabling live mode

Two switches:

1. Build with the feature compiled in:
   ```bash
   cargo build --release --features tray,interactive
   ```
2. Toggle the runtime flag in `~/.config/fono/config.toml`:
   ```toml
   [interactive]
   enabled = true
   # Per-minute spending ceiling, in USD micro-cents (1¢ = 10_000 µ¢).
   # Local STT is free, so leave at 0 unless you flip to cloud streaming.
   budget_ceiling_per_minute_umicros = 0
   # Quality floor under budget pressure: "max" | "balanced" | "aggressive".
   quality_floor = "max"
   # Show the live-dictation overlay window. Independent of the
   # static [overlay].enabled toggle.
   overlay = true
   ```

The runtime toggle is read at daemon startup *and* on every `Reload`
IPC (so `fono use stt local` or any other config-rewrite triggers a
hot pick-up). When the cargo `interactive` feature is *not* compiled
in, the toggle is parsed from disk but ignored — the daemon has no
streaming code to turn on.

## CLI

### `fono record --live`

Run a one-shot live dictation from the command line, useful for
smoke-testing without the hotkey daemon path:

```bash
fono record --live
fono record --live --max-seconds 30
fono record --live --no-inject     # print the text, don't inject
```

Captures from the configured input device, paints the overlay, runs
the streaming pipeline once (in record-then-replay mode for Slice A),
and prints the final cleaned text. Press Ctrl+C to stop early. The
default cap is 30 s; `--max-seconds 0` disables the cap entirely.

### `fono test-overlay`

Spawns the live overlay in isolation so you can verify it paints on
your compositor before relying on it during a dictation:

```bash
fono test-overlay
```

Cycles through Recording → LiveDictating(text) → Hidden over a few
seconds. If nothing visible appears, see the *Known issues* section
below.

## Known issues

- **Overlay may not paint on hostile compositors.** Some Wayland
  compositors (sway with strict layer-shell policies, custom KWin
  setups with `Force black background` rules) refuse to honour the
  overlay's window-type hint. The overlay daemon falls back to
  *no overlay* when winit reports a window-creation error; you'll
  still see the dictated text on stdout / in the focused app. Slice
  B's sub-process overlay refactor will improve crash isolation here.
- **Wayland may steal focus on first overlay creation.** On a small
  number of compositors the overlay-show transient takes input focus
  for ~1 frame, dropping the first keystroke or two from a
  simultaneously-typed key. Mitigation: avoid typing during the first
  100 ms of `fono record --live`. This is on the Slice B fix list.
- **Synthetic-tone equivalence-harness fixtures.** The two committed
  fixtures under `tests/fixtures/equivalence/` are synthesized 440 Hz
  tones, not real speech, so the equivalence-harness Tier-1
  Levenshtein threshold is currently loosened to `≤ 0.05`. Real CC0
  speech fixtures land in Slice B; the threshold tightens back to the
  v6 plan's strict `≤ 0.01` at the same time.

## Equivalence harness (developer-facing)

The streaming↔batch equivalence harness lives at
`crates/fono-bench/src/equivalence.rs` and is exposed via the
`fono-bench` CLI:

```bash
cargo run -p fono-bench --features equivalence,whisper-local -- \
  equivalence --stt local --model tiny.en \
              --output /tmp/equivalence.json
```

For a fast smoke pass that skips fixtures longer than 5 s:

```bash
cargo run -p fono-bench --features equivalence,whisper-local -- \
  equivalence --stt local --model tiny.en --quick
```

The harness compares each fixture's *batch-lane* `transcribe()` text
against the *streaming-lane* concatenated finalize text and emits a
JSON report (`stt_levenshtein_norm`, `ttff_ratio`, `ttc_ratio`,
per-fixture `verdict`). Tier-1 PASS threshold is `≤ 0.05` for
Slice A; see ADR 0009 §7 for the rationale and the path to the strict
bar.

If the local whisper model isn't installed, the harness exits with
status 2 and prints `run \`fono models install <name>\`` rather than
panicking. CI rows that don't have a whisper cache should treat
exit 2 as "skip this row, not failure".

## Tuning boundary behaviour

Slice A v7 introduces a small set of additive heuristics that delay
segment boundaries and flag end-of-utterance dangling words. Every
knob is opt-out (or opt-in), additive only — turning a heuristic off
on a fixture that doesn't trigger it produces an identical
transcript. See `docs/decisions/0015-boundary-heuristics.md` for the
design rationale.

All keys live under `[interactive]` in `~/.config/fono/config.toml`.

| Key                              | Default                        | What it does                                                                                                          |
| -------------------------------- | ------------------------------ | --------------------------------------------------------------------------------------------------------------------- |
| `mode`                           | `"hybrid"`                     | Pipeline mode. `"hybrid"` is the only Slice A value — reserved for Slice B variants.                                  |
| `chunk_ms_initial`               | `600`                          | Window the streaming decoder waits before the first preview pass. Smaller = lower TTFF, noisier early text.            |
| `chunk_ms_steady`                | `1500`                         | Steady-state window between preview passes after the first.                                                           |
| `cleanup_on_finalize`            | `true`                         | Run the LLM cleanup pass once on the assembled transcript after the hotkey releases. Off = raw STT output is injected. |
| `max_session_seconds`            | `120`                          | Hard ceiling on a single live session, in seconds.                                                                    |
| `max_session_cost_usd`           | unset                          | Optional hard cost cap for cloud-streaming sessions.                                                                  |
| `commit_use_prosody`             | `false`                        | Engage the prosody-aware boundary delay. Flip to `true` if you find segments cut off mid-thought during slow speech.   |
| `commit_prosody_extend_ms`       | `250`                          | Extension granted when prosody fires. Capped by the session at `chunk_ms_steady * 1.5`.                                |
| `commit_use_punctuation_hint`    | `true`                         | Engage the punctuation hint (Slice A: function-tested stub; full wiring in Slice B).                                  |
| `commit_punct_extend_ms`         | `150`                          | Extension granted by the punctuation hint when it fires.                                                              |
| `commit_hold_on_filler`          | `true`                         | At end-of-input, flag the trailing word if it's a filler/dangling word and surface that signal to the orchestrator.   |
| `commit_filler_words`            | `["um","uh","er","ah","mm","like","you know"]` | Filler-word vocabulary checked by `commit_hold_on_filler`. **English by default** — see localisation note below.       |
| `commit_dangling_words`          | `["and","but","or","so","because","the","a","an","of","to","with","for","in","on","at","from"]` | Syntactically-dangling vocabulary. **English by default**.                                                            |
| `eou_drain_extended_ms`          | `1500`                         | End-of-utterance drain window flagged when a filler/dangling suffix is detected.                                       |
| `eou_adaptive`                   | `false` (reserved)             | **Reserved for Slice D.** Inert in Slice A.                                                                            |
| `resume_grace_ms`                | `0` (reserved)                 | **Reserved for Slice D.** Inert in Slice A.                                                                            |

### Localisation caveat

`commit_filler_words` and `commit_dangling_words` ship with English
vocabularies. Users dictating in other languages should override
both:

```toml
[interactive]
# Brazilian Portuguese
commit_filler_words = ["é", "tipo", "então", "sabe"]
commit_dangling_words = ["e", "mas", "ou", "porque", "o", "a", "os", "as", "de"]
```

Comparison is case-insensitive after stripping trailing `.,;:!?`. An
empty list disables the corresponding heuristic for that language
without needing to flip the parent boolean.

### Reserved Slice D keys

`eou_adaptive` and `resume_grace_ms` are accepted by the parser but
have no effect in Slice A. Slice D (plan tasks R15.x) replaces the
static `eou_drain_extended_ms` with a silence-distribution estimator
and adds a hotkey-resume grace window so a re-pressed hotkey within
`resume_grace_ms` continues the prior session instead of opening a
new one.
