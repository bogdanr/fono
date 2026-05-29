# Visual Context for Coding Agents and the Voice Assistant

## Objective

Let users say things like *"look at this error and fix it"* (to a coding agent over MCP) or
*"what am I seeing here?"* (to the F8 voice assistant) and have Fono attach a screenshot to
the next turn, without growing the binary by more than ~250 KB.

Two consumers, one capture pipeline:

1. **MCP path** — `fono.screen` tool in `fono-mcp-server` returns an MCP `image` content
   block, callable by Claude Code, Cursor, Forge, etc.
2. **Assistant path (F8)** — `fono_screen` is registered as an **LLM function-calling tool**
   in the assistant's outgoing request. The model decides when to call it based on user intent —
   no hardcoded phrase matching. When called, Fono executes the capture, returns the PNG as a
   `tool_result` image block, and the model continues with the image in context.

This is a natural first step toward the **Voice Actions** roadmap item: the same
function-calling plumbing that fires `fono_screen` will later fire `pomodoro_start`,
Home Assistant actions, etc.

## Capture Modes

| Mode | `capture.mode` | Behaviour |
|---|---|---|
| **Automatic** | `"automatic"` (default) | Grabs the focused window instantly, no UI. |
| **Interactive** | `"interactive"` | Opens the OS-native region picker; user drags a box. |

No full-screen mode. `automatic` covers single-window ("what am I looking at?");
`interactive` covers partial/multi-window ("let me show you this part").

## Config Block

```toml
[capture]
enabled = true
mode = "automatic"            # "automatic" | "interactive"
max_bytes_kb = 2048
private_window_classes = []   # additive to the built-in list
redact_window_titles = false  # strip title from the metadata text block
```

No `tool` or `select_tool` knobs. The probe ladder runs automatically; `fono doctor`
shows which rungs are available so the user knows what to install if needed.

## Grabber Tool Ladder

Probed once on daemon startup; first tool present per (session-type × mode) wins.
On runtime failure, Fono walks to the next rung silently.

### Wayland — Automatic (focused window)

| # | Tool | Command |
|---|---|---|
| 1 | portal (`org.freedesktop.portal.Screenshot`, non-interactive) | via `zbus` |
| 2 | `grimblast` | `grimblast active` |
| 3 | `spectacle` | `spectacle -b -a -n -o FILE` |
| 4 | `gnome-screenshot` | `gnome-screenshot -w --delay=0 -f FILE` |
| 5 | noop | `CaptureError::NoToolAvailable` |

### Wayland — Interactive (region picker)

| # | Tool | Command |
|---|---|---|
| 1 | portal (`org.freedesktop.portal.Screenshot`, interactive) | via `zbus` |
| 2 | `grim` + `slurp` | `grim -g "$(slurp -d)" FILE` |
| 3 | `grimblast` | `grimblast area` |
| 4 | `spectacle` | `spectacle -r -b -n -o FILE` |
| 5 | `gnome-screenshot` | `gnome-screenshot -a -f FILE` |
| 6 | noop | `CaptureError::NoToolAvailable` |

### X11 — Automatic (focused window)

| # | Tool | Command |
|---|---|---|
| 1 | `scrot` | `scrot -u -z FILE` |
| 2 | `maim` | `maim --window $(xdotool getactivewindow) FILE` |
| 3 | `gnome-screenshot` | `gnome-screenshot -w --delay=0 -f FILE` |
| 4 | `import` (ImageMagick) | `import -window $(xdotool getactivewindow) FILE` |
| 5 | noop | `CaptureError::NoToolAvailable` |

### X11 — Interactive (region picker)

| # | Tool | Command |
|---|---|---|
| 1 | `scrot` | `scrot -s FILE` |
| 2 | `maim` | `maim -s FILE` |
| 3 | `gnome-screenshot` | `gnome-screenshot -a -f FILE` |
| 4 | `import` (ImageMagick) | `import FILE` (built-in drag-select) |
| 5 | noop | `CaptureError::NoToolAvailable` |

**Tool rationale:** `scrot` ships in virtually every Linux distro's repo (Debian, Ubuntu,
Fedora, Arch, Slackware, Alpine, Void). `maim` is the modern Ubuntu/Debian alternative.
`gnome-screenshot` ships with any GNOME desktop. `grim`+`slurp` is the wlroots standard.
`spectacle` covers KDE. `import` (ImageMagick) is ubiquitous. The portal covers GNOME 45+
and KDE Plasma 5.27+ with zero extra deps.

## Design Constraints

- **Binary budget: +250 KB ceiling, hard.** CPU build ~21.24 MiB vs 22 MiB CI gate. No
  in-process PNG encoder. No `image` / `png` crates. `zbus` and `base64` are already present.
- **One pipeline.** `fono-core::screen_capture` module; reused by MCP and assistant paths.

## Implementation Plan

### Phase 0 — ADR and dep audit

- [ ] Task 0.1. Write ADR `0031-visual-context-capture.md`: two modes, no full-screen, probe-
  ladder rationale, LLM tool-calling for assistant path, 250 KB ceiling, no in-process encoder.
- [ ] Task 0.2. Confirm `zbus`, `base64`, `xdotool` already in workspace / documented. Add
  grabber tools (`grim`, `slurp`, `grimblast`, `spectacle`, `gnome-screenshot`, `scrot`, `maim`)
  to `docs/providers.md` and `packaging/slackbuild/fono/fono.info` as optional runtime deps.
- [ ] Task 0.3. CHANGELOG `[Unreleased]` stub + ROADMAP `Up next` block.

### Phase 1 — Core capture module (`fono-core::screen_capture`)

- [ ] Task 1.1. `screen_capture.rs`:
  - `CaptureMode::{Automatic, Interactive}`.
  - `GrabberProbe` — built at daemon startup; four ordered ladders
    (Wayland-auto, Wayland-interactive, X11-auto, X11-interactive). Each entry:
    `{ name: &str, probe_args: &[&str], run: fn(tmp: &Path) -> Command }`.
  - `capture(mode: CaptureMode) -> Result<CapturedImage, CaptureError>` — selects the ladder
    for (session-type, mode), tries each entry, returns on first success.
- [ ] Task 1.2. Implement `PortalCapture` via `zbus`. Automatic → `interactive: false`;
  Interactive → `interactive: true`. Result: `file://` URI, read + base64 + unlink.
- [ ] Task 1.3. Implement `ExternalToolCapture`: `Command::new`, output to a mkstemp file,
  5 s timeout (automatic) / 30 s timeout (interactive), read PNG bytes, unlink. Non-zero exit
  or timeout → next rung.
- [ ] Task 1.4. `CapturedImage { png_bytes: Vec<u8>, source: CaptureSource, width: u32,
  height: u32 }`. Width/height from PNG IHDR (~12 bytes, hand-rolled, no crate).
  `CaptureSource::{Window { wm_class, title }, Region { x, y, w, h } }`.
- [ ] Task 1.5. `CaptureError::{Cancelled, PrivateWindow, NoToolAvailable, Timeout}`.
  All first-class — callers never panic.
- [ ] Task 1.6. Optional downscale: `png_bytes.len() > 2 MiB` → shell out to
  `magick convert - -resize 1600x1600\> png:-`. Absent `magick` → ship original.
- [ ] Task 1.7. Unit tests: mock `GrabberProbe` with a stub that writes a 1×1 PNG; cover
  auto + interactive + cancel + fallback-on-failure. Integration test gated on
  `FONO_TEST_REAL_CAPTURE=1`.

### Phase 2 — Config and privacy

- [ ] Task 2.1. Parse `[capture]` block from `fono-core::config`. Apply `enabled`, `mode`,
  `max_bytes_kb`, `private_window_classes`, `redact_window_titles`.
- [ ] Task 2.2. Privacy gate for `Automatic`: probe `WindowContext` (`crates/fono/src/context.rs`);
  if focused window matches private-window list → `CaptureError::PrivateWindow`. `Interactive`
  mode: user frames the rectangle explicitly, no automatic gate.
- [ ] Task 2.3. `fono use capture on|off` CLI verb + tray toggle. Tray badge flashes on
  every capture.

### Phase 3 — `fono doctor` rows

- [ ] Task 3.1. Add capture section:
  ```
  Screen capture
    Session type  : Wayland (wlroots)
    Mode          : automatic
    Active tool   : grim (auto) / grim+slurp (interactive)
    Fallback chain: portal [missing] → grim ✓ → grimblast [missing] → spectacle [missing]
  ```
  Marks each rung present ✓ / missing. Prints install hint for the top missing rung.

### Phase 4 — MCP tool `fono.screen`

- [ ] Task 4.1. `crates/fono-mcp-server/src/tools/screen.rs`:
  - Input: `{ "mode": "automatic" | "interactive" }` (optional; defaults to `capture.mode`).
  - Output: MCP `image` content block (base64 PNG) + text block:
    `{ "source": "window|region", "wm_class": "...", "dimensions": "WxH", "tool": "scrot", "mode": "automatic" }`.
  - `"interactive"` blocks until picker completes, 30 s wall-clock timeout → `Cancelled`.
- [ ] Task 4.2. Wire through `ToolRegistry` + `McpContext`. `McpActivityGuard` so tray
  flashes amber during capture.
- [ ] Task 4.3. Update `assets/agent-presets/voice.md`: call `fono.screen` (automatic) for
  "look at this error"; call `fono.screen { "mode": "interactive" }` for "let me show you
  a piece of this". Rate-limit: one call per user turn.
- [ ] Task 4.4. Document in `docs/coding-agents.md`; per-agent verification matrix
  (Forge / Claude Code / Cursor).

### Phase 5 — Assistant LLM tool-calling path (F8)

This replaces hardcoded trigger-phrase matching with the LLM's own function-calling capability.

- [ ] Task 5.1. Define `fono_screen` as an assistant tool in
  `crates/fono-assistant/src/tools.rs`:
  ```json
  {
    "name": "fono_screen",
    "description": "Capture a screenshot of the user's focused window (mode=automatic) or open an interactive region picker so the user can frame a specific area (mode=interactive). Call this whenever the user refers to what is currently on their screen, asks you to look at something, or wants to show you something visually.",
    "parameters": {
      "type": "object",
      "properties": {
        "mode": { "type": "string", "enum": ["automatic", "interactive"], "default": "automatic" }
      }
    }
  }
  ```
- [ ] Task 5.2. Include `fono_screen` in the `tools` array of the assistant chat request
  when `[capture].enabled = true` AND the configured assistant model supports vision (per
  `provider_catalog.rs` capability flag) AND `[assistant].prefer_vision = true`.
- [ ] Task 5.3. Handle the model's `tool_call` / `function_call` response for `fono_screen`:
  1. Speak acknowledgement: "Looking at this window…" / "Pick the area…".
  2. Execute `screen_capture::capture(mode)`.
  3. Return the PNG as a `tool_result` with `type: "image_url"` (OpenAI) or
     `type: "tool_result"` with an `image` block (Anthropic). Each provider's assistant
     client formats the result per its own wire protocol.
  4. Continue the chat turn; the model now has the image in context and answers.
- [ ] Task 5.4. When `fono_screen` is unavailable (capture disabled, no tool in ladder,
  `PrivateWindow`): return a `tool_result` with `{ "error": "capture_unavailable",
  "reason": "..." }` so the model can explain gracefully rather than crashing.
- [ ] Task 5.5. `Ctrl+F8` hotkey override → assistant turn with `fono_screen` pre-called
  in `"interactive"` mode, result pre-attached before the user speaks. Avoids waiting for
  the model to decide to call the tool when the user explicitly wants to frame something.
- [ ] Task 5.6. Unit tests: mock LLM response containing a `fono_screen` tool_call; verify
  the capture dispatcher fires and the tool_result is correctly formatted for both OpenAI
  and Anthropic wire protocols.

### Phase 6 — Terminal text fast path (optional; cut first if budget is tight)

- [ ] Task 6.1. When `mode == Automatic` and focused wm_class is a known terminal, probe
  `tmux capture-pane -p -S -100` → `screen -X hardcopy` → AT-SPI. Attach text + image
  when both available.
- [ ] Task 6.2. Gate on `[capture].terminal_text_extraction = true`, default off.

### Phase 7 — Documentation and release

- [ ] Task 7.1. `docs/providers.md`: per-distro grabber dep matrix, both modes.
- [ ] Task 7.2. `docs/coding-agents.md`: `fono.screen` tool docs, mode table.
- [ ] Task 7.3. Binary size delta check ≤ 250 KB CPU + GPU.
- [ ] Task 7.4. CHANGELOG `[Unreleased]` graduation, ROADMAP update, ADR 0031 cross-link.

## Verification Criteria

- `fono.screen` (automatic) round-trips against Claude Code and Forge: agent names a UI element.
- `fono.screen { "mode": "interactive" }` opens the OS-native picker on wlroots, GNOME-Wayland,
  KDE-Wayland, X11 (scrot / maim).
- Removing `scrot` from PATH → Fono falls through to `maim` without error.
- Saying "what am I looking at?" to the F8 assistant on a vision-capable provider causes the
  LLM to call `fono_screen`, the window is captured, and the model describes what it sees —
  with no Rust-side trigger-phrase matching involved.
- Saying the same with a non-vision assistant model → model does not call `fono_screen` (tool
  not offered), Fono does not attempt a capture.
- KeePassXC focused + `Shift+F8` → `PrivateWindow` returned to the model as a tool error.
- `Ctrl+F8` → interactive picker opens before recording; model receives the framed image.
- Esc during picker → `Cancelled` → model receives tool error, answers text-only.
- `fono doctor` shows full ladder per compositor, present ✓ / missing, install hint.
- Binary delta ≤ 250 KB CPU and GPU.
- Pre-commit gate green: fmt, clippy -D warnings, tests.

## Risks and Mitigations

1. **Binary size.** No in-process encoder; shell out for PNG; existing deps. Cut Phase 6 first.
2. **GNOME portal non-interactive unavailable.** Fall through to `gnome-screenshot -w`.
3. **scrot on Wayland.** X11-only by design; Wayland ladder does not include it.
4. **Picker dismissed.** `Cancelled` first-class; model receives error, continues text-only.
5. **Privacy.** `Automatic` gated on private-window list; `Interactive` trusts user framing;
   tray flash; `fono use capture off` kill switch.
6. **Model calls `fono_screen` in interactive mode unprompted.** Rate-limit in tool dispatcher:
   one `fono_screen` call per assistant turn; preset discourages speculative calls.
7. **Provider tool-calling format differences.** OpenAI uses `function_call` / `tool_call` +
   `image_url`; Anthropic uses `tool_use` + `tool_result` with `image` blocks. Both branches
   needed in the assistant client adapters (small, self-contained).
