# Live (interactive) dictation

> **Status:** Shipped in the default build. Realtime push, the
> two-lane preview/finalize pipeline, the live overlay, and the
> equivalence harness are all in place; the preview pane paints
> *while* you speak. A handful of internal quality knobs
> (punctuation-extend wiring, adaptive end-of-utterance drain) remain
> informational-only today — they are detected and reported on the
> tracing span but don't yet feed back into capture timing. See
> `docs/decisions/0009-interactive-live-dictation.md` for the full
> design rationale.

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
`Finalize` update, in segment order. After the hotkey releases, the
polish pass runs once on the assembled text and the cleaned result is
what gets typed into the focused window.

## Known limitations

- **Polish is not streamed.** It runs once on the full transcript
  after the hotkey releases — a deliberate design decision (ADR 0009
  §4). The cleaned text is what gets injected; previews shown during
  speech are raw STT.
- **Punctuation-extend and adaptive-EOU drain are informational.**
  The heuristics are computed and reported on the run's tracing span
  (`live.commit_extended_by_punct_ms`, `live.drain_extended_by_filler`,
  `live.drain_extended_by_dangling`) but do not currently feed back
  into capture timing. `crates/fono/src/live.rs` carries the relevant
  TODO markers; the prosody-extend hint is fully wired.

## Enabling live mode

Live mode ships in the default build (`interactive` is in the default
cargo features). It is toggled by picking the **Transcript** overlay
style — either through the tray (*Preferences → Waveform style →
Transcript*) or by editing `~/.config/fono/config.toml`:

```toml
[overlay]
style = "transcript"     # bars | oscilloscope | fft | heatmap | transcript
```

`fono_core::Config::live_preview()` is `true` iff `overlay.style ==
"transcript"`; that single flag flips the daemon between the batch and
streaming code paths. The four passive visualisations
(`bars` / `oscilloscope` / `fft` / `heatmap`) keep the daemon on the
batch path; only `transcript` triggers the streaming STT pipeline.

Once live mode is on, the `[interactive]` block tunes the streaming
behaviour. Most users never need to touch it; see the
*Tuning boundary behaviour* table below for the surviving knobs.

```toml
[interactive]
quality_floor = "max"    # max | balanced | aggressive — budget-pressure floor
```

The overlay style is re-read on every `Reload` IPC, so editing the file
or running any `fono use` command applies immediately — no daemon
restart. Custom builds that compile without `--features interactive`
ignore the streaming code path even with `style = "transcript"` (the
overlay falls back to a static "live preview unavailable" indicator).

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
the streaming pipeline live, and prints the final cleaned text. Press
Ctrl+C to stop early. The default cap is 30 s; `--max-seconds 0`
disables the cap entirely.

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
- **Equivalence-harness fixtures.** Check `tests/fixtures/equivalence/`
  for the current corpus and the harness's `verdict` thresholds — the
  Tier-1 Levenshtein bar moves as real speech fixtures replace earlier
  synthetic tones. ADR 0009 §7 documents the path to the strict bar.

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
per-fixture `verdict`). See ADR 0009 §7 for the current Tier-1
threshold and the path to the strict bar.

If the local whisper model isn't installed, the harness exits with
status 2 and prints `run \`fono models install <name>\`` rather than
panicking. CI rows that don't have a whisper cache should treat
exit 2 as "skip this row, not failure".

## Tuning boundary behaviour

A small set of additive heuristics delay segment boundaries and flag
end-of-utterance dangling words. The heuristics ship with built-in
defaults baked into `HeuristicConfig::default` in
`crates/fono/src/live.rs`; the matching `commit_*` / `eou_*` /
`resume_grace_ms` config keys (and the budget / session-cap knobs)
were removed in the 2026-05-22 schema simplification because they were
never plumbed from `[interactive]` into the live session. See
`docs/decisions/0015-boundary-heuristics.md` for the rationale.

The surviving user-tunable knobs under `[interactive]`:

| Key                              | Default                        | What it does                                                                                                          |
| -------------------------------- | ------------------------------ | --------------------------------------------------------------------------------------------------------------------- |
| `mode`                           | `"hybrid"`                     | Pipeline mode. `"hybrid"` is the only value wired today; the field is kept so future variants can land without a config break. |
| `chunk_ms_initial`               | `600`                          | Window the streaming decoder waits before the first preview pass. Smaller = lower TTFF, noisier early text.            |
| `chunk_ms_steady`                | `1500`                         | Steady-state window between preview passes after the first.                                                           |
| `cleanup_on_finalize`            | `true`                         | Run the polish pass once on the assembled transcript after the hotkey releases. Off = raw STT output is injected. |
| `quality_floor`                  | `"max"`                        | Quality floor under budget pressure: `"max"` / `"balanced"` / `"aggressive"`.                                          |
| `streaming_interval`             | `1.0`                          | Cloud streaming preview cadence in seconds. Clamped to `[0.5, 3.0]`; `> 3.0` disables the preview lane.                |
| `hold_release_grace_ms`          | `150`                          | Drain window between hotkey release and cpal capture stop so trailing audio reaches the streaming STT.                 |
