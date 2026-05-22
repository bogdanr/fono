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
`Finalize` update, in segment order. The cleaned-up version (polish) runs once at end on the assembled text and is what gets
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
- **polish is not streamed.** It runs once on the full
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
   # Quality floor under budget pressure: "max" | "balanced" | "aggressive".
   quality_floor = "max"
   ```

   The live-dictation overlay is shown unconditionally when
   `enabled = true` — it's the only feedback surface for live
   previews, so there is no separate toggle. The `[overlay].enabled`
   flag controls only the passive recording indicator used in batch
   mode (when `[interactive].enabled = false`).

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

- **Overlay rendering depends on the display server.** On Wayland
  compositors that implement `zwlr_layer_shell_v1` (sway, hyprland,
  KDE Plasma 5.27+, COSMIC, Wayfire, niri, labwc) the overlay
  anchors bottom-centre with transparency and no focus theft. On
  GNOME / Mutter (no layer-shell) Fono falls through to its X11
  backend via Xwayland — same bottom-centre / always-on-top
  behaviour. On pure X11 the native override-redirect path is
  used. Headless or otherwise unusable display servers fall back
  to a `noop` backend (no window, daemon still runs); `fono
  doctor` prints which backend was selected and `FONO_OVERLAY_BACKEND`
  can force a specific one. Full table in [`docs/wayland.md`](wayland.md).
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
segment boundaries and flag end-of-utterance dangling words. The
heuristics ship with built-in defaults; the matching user-facing config
keys (`commit_*`, `eou_*`, `resume_grace_ms`, the budget / session-cap
knobs) were removed in the 2026-05-22 schema simplification because
they were never plumbed from `[interactive]` into the live session.
See `docs/decisions/0015-boundary-heuristics.md` for the design
rationale and `crates/fono/src/live.rs` (`HeuristicConfig::default`)
for the current values.

The surviving user-tunable knobs under `[interactive]`:

| Key                              | Default                        | What it does                                                                                                          |
| -------------------------------- | ------------------------------ | --------------------------------------------------------------------------------------------------------------------- |
| `mode`                           | `"hybrid"`                     | Pipeline mode. `"hybrid"` is the only Slice A value — reserved for Slice B variants.                                  |
| `chunk_ms_initial`               | `600`                          | Window the streaming decoder waits before the first preview pass. Smaller = lower TTFF, noisier early text.            |
| `chunk_ms_steady`                | `1500`                         | Steady-state window between preview passes after the first.                                                           |
| `cleanup_on_finalize`            | `true`                         | Run the polish pass once on the assembled transcript after the hotkey releases. Off = raw STT output is injected. |
| `quality_floor`                  | `"max"`                        | Quality floor under budget pressure: `"max"` / `"balanced"` / `"aggressive"`.                                          |
| `streaming_interval`             | `1.0`                          | Cloud streaming preview cadence in seconds. Clamped to `[0.5, 3.0]`; `> 3.0` disables the preview lane.                |
| `hold_release_grace_ms`          | `150`                          | Drain window between hotkey release and cpal capture stop so trailing audio reaches the streaming STT.                 |
