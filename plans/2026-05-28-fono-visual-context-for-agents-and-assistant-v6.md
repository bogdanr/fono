# Visual Context for Coding Agents and the Voice Assistant

## Objective

Let users say things like *"look at this error and fix it"* (to a coding agent over MCP) or
*"what am I seeing here?"* (to the F8 voice assistant) and have Fono attach a screenshot to
the next turn, without growing the binary by more than ~250 KB.

Two consumers, one capture pipeline. No new config block.

## When the Feature Is Active

- **MCP path**: always available when `fono use mcp-server on`; the coding agent decides
  when to call `fono.screen`.
- **Assistant path (F8)**: active when `[assistant].prefer_vision = true` (already exists)
  **and** the configured assistant model supports vision (already in `provider_catalog.rs`).
  Both gates already exist; no new knob needed.

## Capture Modes

The **mode is never configured**. It is chosen per call by the caller:

| Mode | When used |
|---|---|
| `automatic` | Grabs the focused window instantly, no UI. LLM calls this when the user refers to what is on screen. MCP default. |
| `interactive` | Opens the OS-native region picker; user draws a rectangle. LLM calls this when the user says "let me show you a part of this". |

## Grabber Tool Ladder

Probed once at daemon startup per (session-type × mode). First tool present wins.
Runtime failure → next rung silently. No user configuration.

### Wayland — Automatic

`portal (non-interactive)` → `spectacle -b -a -n` → `gnome-screenshot -w --delay=0`

### Wayland — Interactive

`portal (interactive)` → `grim -g "$(slurp -d)"` → `spectacle -r -b -n` → `gnome-screenshot -a`

### X11 — Automatic

`scrot -u -z` → `maim --window $(xdotool getactivewindow)` → `gnome-screenshot -w --delay=0` → `import -window $(xdotool getactivewindow)`

### X11 — Interactive

`scrot -s` → `maim -s` → `gnome-screenshot -a` → `import`

**Tool rationale:** `scrot` is in virtually every distro's base repo. `maim` is the modern
Debian/Ubuntu alternative. `gnome-screenshot` ships with any GNOME desktop. `grim`+`slurp`
is the wlroots standard. `spectacle` covers KDE. `import` (ImageMagick) is ubiquitous.
The portal covers GNOME 45+ and KDE Plasma 5.27+ with zero extra deps.

## Design Constraints

- **Binary budget: +250 KB ceiling, hard.** No in-process PNG encoder. No `image`/`png` crates.
  `zbus` and `base64` are already in the workspace. PNG comes from the grabber tool.
- **No new crate.** A module under `fono-core`, reused by both paths.

## Implementation Plan

### Phase 0 — ADR and dep audit

- [ ] Task 0.1. Write ADR `0031-visual-context-capture.md`: two-mode design, no config block,
  LLM tool-calling for assistant path, probe-ladder rationale, 250 KB ceiling.
- [ ] Task 0.2. Confirm `zbus`, `base64`, `xdotool` already present. Add optional runtime deps
  (`grim`, `slurp`, `spectacle`, `gnome-screenshot`, `scrot`, `maim`) to `docs/providers.md`
  and `packaging/slackbuild/fono/fono.info`.
- [ ] Task 0.3. CHANGELOG `[Unreleased]` stub + ROADMAP `Up next` block.

### Phase 1 — Core capture module (`fono-core::screen_capture`)

- [ ] Task 1.1. `screen_capture.rs`:
  - `CaptureMode::{Automatic, Interactive}`.
  - `GrabberProbe` — four ordered ladders built once at startup.
  - `capture(mode: CaptureMode) -> Result<CapturedImage, CaptureError>` — walks the ladder for
    (session-type, mode), returns on first success.
- [ ] Task 1.2. `PortalCapture` via existing `zbus`. `Automatic` → `interactive: false`;
  `Interactive` → `interactive: true`. Result: `file://` URI → read + base64 + unlink.
- [ ] Task 1.3. `ExternalToolCapture`: `Command::new`, output to mkstemp file,
  5 s timeout (automatic) / 30 s (interactive), read PNG bytes, unlink. Failure → next rung.
- [ ] Task 1.4. `CapturedImage { png_bytes: Vec<u8>, source: CaptureSource, width: u32, height: u32 }`.
  `CaptureSource::{Window { wm_class, title }, Region }`.
  Width/height from PNG IHDR chunk (~12 bytes, hand-rolled, no crate).
- [ ] Task 1.5. `CaptureError::{Cancelled, PrivateWindow, NoToolAvailable, Timeout}`.
- [ ] Task 1.6. Downscale: `png_bytes.len() > 2 MiB` → `magick convert - -resize 1600x1600\> png:-`.
  Absent `magick` → ship original. Hardcoded threshold, not configurable.
- [ ] Task 1.7. Unit tests: mock `GrabberProbe`; cover both modes + cancel + fallback.
  Integration test gated on `FONO_TEST_REAL_CAPTURE=1`.

### Phase 2 — Privacy

- [ ] Task 2.1. `Automatic` only: probe `WindowContext` (`crates/fono/src/context.rs`); if the
  focused window matches the existing private-window list → `CaptureError::PrivateWindow`.
  `Interactive` mode trusts user framing — no gate.
- [ ] Task 2.2. Tray badge flashes briefly on every capture so it never happens invisibly.
  No new toggle, no new CLI verb.

### Phase 3 — `fono doctor` rows

- [ ] Task 3.1. Add capture section to `fono doctor`:
  ```
  Screen capture
    Session type  : Wayland (wlroots)
    Auto tool     : portal [missing] → grim+slurp ✓
    Select tool   : portal [missing] → grim+slurp ✓
  ```
  Marks each rung present ✓ / missing. Prints one install hint for the top missing rung.

### Phase 4 — MCP tool `fono.screen`

- [ ] Task 4.1. `crates/fono-mcp-server/src/tools/screen.rs`:
  - Input: `{ "mode": "automatic" | "interactive" }` (required).
  - Output: MCP `image` content block (base64 PNG) + text block with source metadata
    (`wm_class`, dimensions, tool used, mode).
  - `interactive` blocks until picker completes; 30 s timeout → `Cancelled`.
- [ ] Task 4.2. Wire through `ToolRegistry` + `McpContext`. `McpActivityGuard` → tray flashes amber.
- [ ] Task 4.3. Update `assets/agent-presets/voice.md` and `docs/coding-agents.md`.

### Phase 5 — Assistant LLM tool-calling path (F8)

- [ ] Task 5.1. Define `fono_screen` as an assistant tool in `crates/fono-assistant/src/tools.rs`:
  ```json
  {
    "name": "fono_screen",
    "description": "Capture a screenshot. Use mode=automatic to grab the user's focused window instantly (for questions like 'what am I looking at?' or 'look at this error'). Use mode=interactive to open a region picker so the user can frame a specific area (for 'let me show you this part'). Only call this when the user explicitly references something on their screen.",
    "parameters": {
      "type": "object",
      "required": ["mode"],
      "properties": {
        "mode": { "type": "string", "enum": ["automatic", "interactive"] }
      }
    }
  }
  ```
- [ ] Task 5.2. Include `fono_screen` in the assistant chat request when
  `[assistant].prefer_vision = true` AND the provider is vision-capable.
- [ ] Task 5.3. Handle the `tool_call` response:
  1. Speak acknowledgement: "Looking at this window…" / "Pick the area…".
  2. Execute `screen_capture::capture(mode)`.
  3. Return PNG as `tool_result` with image block (OpenAI: `image_url`; Anthropic: `image`).
  4. Continue the turn; model answers with image in context.
- [ ] Task 5.4. On `PrivateWindow`, `NoToolAvailable`, `Cancelled`: return a `tool_result`
  with `{ "error": "...", "reason": "..." }` so the model explains gracefully.
- [ ] Task 5.5. Unit tests: mock LLM response with `fono_screen` tool_call; verify PNG
  is correctly formatted for both OpenAI and Anthropic wire protocols.

### Phase 6 — Terminal text fast path (optional; cut first if budget is tight)

- [ ] Task 6.1. When `mode == Automatic` and focused wm_class is a known terminal, probe
  `tmux capture-pane -p -S -100` → `screen -X hardcopy`. Attach text + image when both
  available. Text tokens are ~25× cheaper than image tokens.
- [ ] Task 6.2. No config knob. Fono attempts it silently and falls back to image-only if
  the probe returns nothing.

### Phase 7 — Documentation and release

- [ ] Task 7.1. `docs/providers.md`: per-distro grabber dep matrix.
- [ ] Task 7.2. `docs/coding-agents.md`: `fono.screen` tool docs.
- [ ] Task 7.3. Binary size delta ≤ 250 KB CPU + GPU.
- [ ] Task 7.4. CHANGELOG `[Unreleased]` graduation, ROADMAP update, ADR 0031 cross-link.

## Verification Criteria

- `fono.screen { "mode": "automatic" }` round-trips against Claude Code and Forge.
- `fono.screen { "mode": "interactive" }` opens the OS-native picker on wlroots, GNOME-Wayland,
  KDE-Wayland, X11.
- Removing `scrot` → Fono falls through to `maim` without error.
- Saying "what am I looking at?" to the F8 assistant causes the LLM to call `fono_screen`,
  the window is captured, the model describes it — no Rust-side phrase matching.
- Non-vision assistant model → `fono_screen` not offered → no capture attempted.
- KeePassXC focused + Shift+F8 → `PrivateWindow` returned to model; model explains gracefully.
- Esc during interactive picker → `Cancelled` → model answers text-only.
- `fono doctor` shows full ladder per compositor, present ✓ / missing.
- Binary delta ≤ 250 KB.
- Pre-commit gate green.

## What Was Deliberately Removed (and Why)

| Removed | Reason |
|---|---|
| `[capture]` config section | No knobs survive scrutiny (see below) |
| `capture.enabled` | Gated by `prefer_vision` + provider capability — already exists |
| `capture.mode` | Caller (LLM / MCP agent) picks mode per call; no global default needed |
| `capture.max_bytes_kb` | Internal threshold — hardcoded constant, not user-facing |
| `capture.private_window_classes` | Built-in list covers all known cases; add back only if requested |
| `capture.redact_window_titles` | Niche edge case; defer |
| `capture.tool` / `capture.select_tool` | Probe ladder handles this automatically |
| `fono use capture on/off` | Redundant without an `enabled` flag |
| Tray capture toggle | Same |
| `Ctrl+F8` hotkey | LLM picks interactive mode from context; extra hotkey unnecessary |
| `grimblast` ladder rung | Hyprland is covered by `grim`+`slurp`; wrapper not needed |
| Multilingual trigger-phrase tables | Replaced by LLM tool-calling |
