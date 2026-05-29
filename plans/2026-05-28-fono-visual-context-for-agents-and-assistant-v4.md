# Visual Context for Coding Agents and the Voice Assistant

## Objective

Let users say things like *"look at this error and fix it"* (to a coding agent over MCP) or
*"what am I seeing here?"* (to the F8 voice assistant) and have Fono attach a screenshot to
the next turn, without growing the binary by more than ~250 KB.

Two consumers, one capture pipeline:

1. **MCP path** — `fono.screen` tool in `fono-mcp-server` returns an MCP `image` content block.
2. **Assistant path (F8)** — trigger-phrase detection attaches the image to the outgoing
   multimodal chat request, gated on `[assistant].prefer_vision = true`.

## Capture Modes

| Mode | Config value | Behaviour |
|---|---|---|
| **Automatic** | `capture.mode = "automatic"` (default) | Grabs the focused window — no UI, no mouse interaction, hands-free. Ideal for "what am I looking at?" and coding-agent error grabs. |
| **Interactive** | `capture.mode = "interactive"` | Opens the OS-native region picker; user drags a rectangle. Ideal for "let me show you this part" and multi-window scenarios. |

No full-screen mode. `automatic` covers the single-window case; `interactive` covers everything
else. The user is never captured more than they explicitly frame.

## Grabber Tool Ladder

Fono does not depend on a single tool. On startup the daemon probes `PATH` once, builds a
priority-ordered list of available tools per (session-type, mode), and caches it. If a tool
fails at runtime, the next one in the ladder is tried transparently.

### Wayland — Automatic (focused window, no interaction)

| Priority | Tool | Command |
|---|---|---|
| 1 | **portal** (`org.freedesktop.portal.Screenshot`, non-interactive) | D-Bus call via existing `zbus`; `interactive: false` |
| 2 | `grimblast` | `grimblast active` |
| 3 | `spectacle` | `spectacle -b -a -n -o /tmp/fono-cap-XXXX.png` |
| 4 | `gnome-screenshot` | `gnome-screenshot -w --delay=0 -f /tmp/fono-cap-XXXX.png` |
| 5 | noop + hint | Speak "No screenshot tool found — install grim or spectacle." |

### Wayland — Interactive (region picker)

| Priority | Tool | Command |
|---|---|---|
| 1 | **portal** (interactive) | D-Bus call; `interactive: true` |
| 2 | `grim` + `slurp` | `grim -g "$(slurp -d)" /tmp/fono-cap-XXXX.png` |
| 3 | `grimblast` | `grimblast area` |
| 4 | `spectacle` | `spectacle -r -b -n -o /tmp/fono-cap-XXXX.png` |
| 5 | `gnome-screenshot` | `gnome-screenshot -a -f /tmp/fono-cap-XXXX.png` |
| 6 | noop + hint | Speak "No screenshot tool found — install slurp or spectacle." |

### X11 — Automatic (focused window, no interaction)

| Priority | Tool | Command |
|---|---|---|
| 1 | `scrot` | `scrot -u -z /tmp/fono-cap-XXXX.png` |
| 2 | `maim` | `maim --window $(xdotool getactivewindow) /tmp/fono-cap-XXXX.png` |
| 3 | `gnome-screenshot` | `gnome-screenshot -w --delay=0 -f /tmp/fono-cap-XXXX.png` |
| 4 | `import` (ImageMagick) | `import -window $(xdotool getactivewindow) /tmp/fono-cap-XXXX.png` |
| 5 | noop + hint | Speak "No screenshot tool found — install scrot or maim." |

### X11 — Interactive (region picker)

| Priority | Tool | Command |
|---|---|---|
| 1 | `scrot` | `scrot -s /tmp/fono-cap-XXXX.png` |
| 2 | `maim` | `maim -s /tmp/fono-cap-XXXX.png` |
| 3 | `gnome-screenshot` | `gnome-screenshot -a -f /tmp/fono-cap-XXXX.png` |
| 4 | `import` (ImageMagick) | `import /tmp/fono-cap-XXXX.png` (built-in drag-select) |
| 5 | noop + hint | Speak "No screenshot tool found — install scrot or maim." |

**Rationale for tool selection:** `scrot` ships in virtually every Linux distro's base repos
(Debian, Ubuntu, Fedora, Arch, Slackware, Alpine, Void). `maim` is the modern Ubuntu/Debian
alternative. `gnome-screenshot` ships with any GNOME desktop. `grim`+`slurp` is the wlroots
standard. `spectacle` covers KDE. `import` from ImageMagick is ubiquitous wherever ImageMagick
is installed. The portal covers GNOME 45+ and KDE Plasma 5.27+ without any extra dep.

### User override

```toml
[capture]
tool = "scrot"          # pin a specific tool; bypass the probe ladder
select_tool = "slurp"   # pin the region-picker used in interactive mode (Wayland only)
```

If the pinned tool is missing, Fono logs a warning and falls back to the ladder.

## Design Constraints

- **Binary budget: +250 KB ceiling, hard.** CPU build is at ~21.24 MiB against a 22 MiB CI
  gate. No in-process image encoder. No `image` / `png` crates. PNG bytes come from the grabber
  tool; Fono reads bytes and base64s them.
- **`zbus` and `base64` are already in the workspace** — the portal D-Bus call and the
  base64 encoding cost zero new binary weight.
- **One capture pipeline** — a module under `fono-core`, reused by MCP and assistant paths.
- **No new crate** — the grabber probe table is a small runtime struct, not a separate crate.

## Implementation Plan

### Phase 0 — ADR and dep audit

- [ ] Task 0.1. Write ADR `0031-visual-context-capture.md`: two-mode design (`automatic` /
  `interactive`), no full-screen, probe-ladder rationale, 250 KB ceiling, explicit no-in-process-
  encoder decision.
- [ ] Task 0.2. Audit workspace deps: confirm `zbus`, `base64`, `xdotool` (runtime dep). Document
  each genuinely new system dep in `docs/providers.md` and `packaging/slackbuild/fono/fono.info`:
  `grim`, `slurp`, `grimblast`, `spectacle`, `gnome-screenshot`, `scrot`, `maim`. None are
  required — all are optional rungs of a fallback ladder.
- [ ] Task 0.3. CHANGELOG `[Unreleased]` stub + ROADMAP `Up next` block.

### Phase 1 — Core capture module (`fono-core::screen_capture`)

- [ ] Task 1.1. `screen_capture.rs` with:
  - `CaptureMode::{Automatic, Interactive}` enum.
  - `CaptureBackend::{Portal, ExternalTool}` — portal tried first on Wayland, external-tool
    ladder used otherwise or as fallback.
  - `GrabberProbe` struct — built once at daemon startup, holds the four ordered ladders
    (Wayland-auto, Wayland-interactive, X11-auto, X11-interactive). Each ladder entry stores
    `{ name: &str, test_args: &[&str], capture_args_fn: fn(tmp: &Path, mode: CaptureMode) -> Vec<String> }`.
  - `capture(mode: CaptureMode) -> Result<CapturedImage, CaptureError>` — walks the probe for
    the active session type + mode; tries each tool in order; returns on first success.
- [ ] Task 1.2. Implement `PortalCapture` via `zbus` (`org.freedesktop.portal.Screenshot`).
  `Automatic` → `interactive: false`. `Interactive` → `interactive: true`. Result is a `file://`
  URI; read, base64, unlink.
- [ ] Task 1.3. Implement `ExternalToolCapture`: spawn tool with `Command::new`, stdout piped
  to a tmp file, wait with a 15 s timeout (interactive mode) / 5 s timeout (automatic), read
  PNG bytes, unlink. On non-zero exit or timeout → try next rung.
- [ ] Task 1.4. `CapturedImage { png_bytes: Vec<u8>, source: CaptureSource, width: u32,
  height: u32 }`. `CaptureSource::{Window { wm_class, title }, Region { x, y, w, h } }`.
  Width/height from PNG IHDR (~12 bytes, hand-rolled, no crate).
- [ ] Task 1.5. `CaptureError::{Cancelled, PrivateWindow, NoToolAvailable, Timeout}`. `Cancelled`
  when interactive picker is dismissed (tool exits non-zero with empty output). All are
  first-class — callers never panic on these.
- [ ] Task 1.6. Optional downscale: if `png_bytes.len() > 2 MiB`, attempt
  `magick convert - -resize 1600x1600\> png:-`. Absent → ship original.
- [ ] Task 1.7. Unit tests: mock `GrabberProbe` with a stub tool that writes a 1×1 PNG;
  cover automatic + interactive + cancel + fallback-on-failure. Integration test gated on
  `FONO_TEST_REAL_CAPTURE=1`.

### Phase 2 — Config and privacy

- [ ] Task 2.1. `[capture]` config block:
  ```toml
  enabled = true
  mode = "automatic"          # "automatic" | "interactive"
  tool = "auto"               # "auto" | "scrot" | "maim" | "grim" | "spectacle" | ...
  select_tool = "auto"        # Wayland interactive picker override: "auto" | "slurp" | "portal"
  max_bytes_kb = 2048
  private_window_classes = [] # additive to the built-in list
  redact_window_titles = false
  ```
- [ ] Task 2.2. Privacy gate: for `Automatic` mode, probe `WindowContext` (already in
  `crates/fono/src/context.rs`) — if focused window is on the private-window list, return
  `CaptureError::PrivateWindow`. For `Interactive` mode, user frames the rectangle explicitly;
  no automatic gate (user intent is explicit).
- [ ] Task 2.3. `fono use capture on|off` CLI verb + tray toggle. Tray badge flashes on
  every capture so it never happens invisibly.

### Phase 3 — `fono doctor` rows

- [ ] Task 3.1. Add capture section to `fono doctor`:
  ```
  Screen capture
    Session type  : Wayland (wlroots)
    Mode          : automatic
    Active tool   : grim (automatic) / slurp (interactive)
    Fallback chain: portal [n/a] → grim ✓ → grimblast [missing] → spectacle [missing]
  ```
  Shows every rung, marks present/missing, highlights the active one.

### Phase 4 — MCP tool `fono.screen`

- [ ] Task 4.1. `crates/fono-mcp-server/src/tools/screen.rs`:
  - Input: `{ "mode": "automatic" | "interactive" }` — if absent, uses config default.
  - Output: MCP `image` content block (base64 PNG) + text block with
    `{ "source": "window|region", "wm_class": "...", "dimensions": "WxH", "tool": "scrot", "mode": "automatic" }`.
  - `interactive` blocks until picker completes, with a 30 s wall-clock timeout.
- [ ] Task 4.2. Wire through `ToolRegistry` + `McpContext`. `McpActivityGuard` so tray flashes
  amber during capture.
- [ ] Task 4.3. Update `assets/agent-presets/voice.md`: call `fono.screen` (automatic) for
  "look at this error"; call `fono.screen { "mode": "interactive" }` for "let me show you
  a piece of this". Rate-limit: one call per user turn.
- [ ] Task 4.4. Document in `docs/coding-agents.md`; per-agent verification matrix.

### Phase 5 — Voice assistant trigger (F8)

- [ ] Task 5.1. Trigger-phrase matcher in `crates/fono-assistant/src/screen_trigger.rs`.
  Multilingual defaults (`en`, `ro`, `fr`, `de`, `es`, `pt`, `it`, `ja`). Returns
  `Option<CaptureMode>`:
  - "what am I looking at" / "look at this window" / "what's here" → `Automatic`.
  - "let me show you" / "look at this part" / "circle this" → `Interactive`.
- [ ] Task 5.2. When triggered + vision-capable: speak acknowledgement ("Looking at this
  window…" / "Pick the area…"), capture, attach image as multimodal content block.
- [ ] Task 5.3. When triggered but vision unavailable: speak fallback, continue text-only.
- [ ] Task 5.4. Keyboard overrides:
  - `Shift+F8` → assistant + `Automatic` (uses config `mode`).
  - `Ctrl+F8` → assistant + `Interactive` regardless of config.
  - Implemented as `HotkeyAction::AssistantWithScreenPressed { mode: CaptureMode }`.

### Phase 6 — Terminal text fast path (optional, cut first if budget is tight)

- [ ] Task 6.1. When `mode == Automatic` and the focused `wm_class` is a known terminal,
  probe `tmux capture-pane -p -S -100` → `screen -X hardcopy` → AT-SPI. Attach text + image
  when both available. Text tokens are ~25× cheaper than vision tokens.
- [ ] Task 6.2. Gate on `[capture].terminal_text_extraction = true`, default off.

### Phase 7 — Documentation and release

- [ ] Task 7.1. `docs/providers.md`: per-distro grabber dep matrix, both modes, fallback chain.
- [ ] Task 7.2. `docs/coding-agents.md`: `fono.screen` tool docs, mode table, trigger-phrase list.
- [ ] Task 7.3. Binary size delta check: ≤ 250 KB on CPU and GPU builds. Cut Phase 6 first,
  then downscale ladder, if over budget.
- [ ] Task 7.4. CHANGELOG `[Unreleased]` graduation, ROADMAP update, ADR 0031 cross-link.

## `fono doctor` per-distro expected output

| Distro / compositor | Active tool (automatic) | Active tool (interactive) |
|---|---|---|
| Ubuntu 24.04 GNOME + Wayland | portal (Screenshot, non-interactive) | portal (Screenshot, interactive) |
| Arch Linux + sway (wlroots) | grim | grim + slurp |
| Arch Linux + KDE Plasma 6 | portal or spectacle | portal or spectacle |
| Ubuntu 22.04 GNOME + X11 | scrot | scrot -s |
| Debian 12 + i3 (X11) | scrot | scrot -s |
| NimbleX + any X11 WM | scrot → maim → import | scrot -s → maim -s → import |
| Fedora 40 GNOME + Wayland | portal | portal |

## Verification Criteria

- `fono.screen` (automatic) round-trips against Claude Code and Forge: agent names a UI
  element from the image.
- `fono.screen { "mode": "interactive" }` opens the OS-native picker on wlroots, GNOME-Wayland,
  KDE-Wayland, X11 (with scrot, maim).
- Removing `scrot` from PATH causes Fono to fall through to `maim` without error.
- `fono doctor` shows the full ladder per compositor family, correctly marking present/missing
  rungs.
- KeePassXC focused + `Shift+F8` → `PrivateWindow` spoken; `Ctrl+F8` opens picker, captures
  only the user-framed rectangle.
- Esc during picker → `Cancelled` → assistant continues text-only.
- Binary delta ≤ 250 KB CPU and GPU builds.
- Pre-commit gate green: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace --tests --lib`.

## Risks and Mitigations

1. **Binary size.** Shell out for PNG; no in-process encoder; `zbus` + `base64` already
   present. Cut Phase 6 first if budget is tight.
2. **GNOME portal non-interactive mode unavailable on older GNOME.** Ladder falls through to
   `gnome-screenshot -w`; document in `docs/providers.md`.
3. **scrot on Wayland.** `scrot` is X11-only; on a pure Wayland session without Xwayland it
   will not be in the Wayland ladder — correct by design. `fono doctor` will surface this.
4. **Picker dismissed / timed out.** `CaptureError::Cancelled` is first-class; assistant
   continues text-only gracefully.
5. **Privacy.** `Automatic` gate on private-window list; `Interactive` trusts user framing;
   tray flash on every capture; `fono use capture off` kill switch.
6. **Agents calling `fono.screen` in interactive mode unprompted.** Rate-limit one call per
   user turn; preset explicitly flags `"interactive"` as user-initiated only.
