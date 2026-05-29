# Visual Context for Coding Agents and the Voice Assistant

## Objective

Let users say things like *"look at this error on my screen and fix it"* (to a coding agent over MCP) or *"what am I seeing here?"* (to the F8 voice assistant) and have Fono attach a screenshot — focused window or user-drawn rectangle — to the next agent / assistant turn, without growing the binary by more than ~250 KB.

Two consumers, one capture pipeline:

1. **MCP path** — a new `fono.screen` tool exposed by `fono-mcp-server` that returns an MCP `image` content block, callable by Claude Code, Cursor, Forge, etc.
2. **Assistant path (F8)** — the existing voice assistant detects a small set of trigger phrases ("look at my screen", "what am I seeing", "ce vezi pe ecran", etc.) and attaches the captured image to the outgoing multimodal chat request, gated on `[assistant].prefer_vision = true`.

Two capture regions:

- **`window`** (default) — the currently focused window, instant, no UI. Covers the single-app case.
- **`select`** — the OS-native rectangle picker; the user drags a box and Fono captures only that area. Covers everything else: two side-by-side terminals, part of a dashboard, a chart next to a log. Dismissable — `CaptureError::Cancelled` if the user presses Esc.

`screen` (full-screen capture) is **intentionally omitted**: it is strictly worse than `select` (which lets you frame exactly what you want) and adds unnecessary privacy risk (notifications, other open windows) plus larger token costs for the vision API.

## Design Constraints

- **Binary budget: +250 KB ceiling, hard.** The CPU build is at ~21.24 MiB against a 22 MiB CI gate (`.github/workflows/ci.yml:184`). No new heavy deps. No `image` / `png` / `gstreamer` crates in-process. PNG encoding is done by the OS-side grabber tool; Fono just reads bytes and base64s them.
- **Portal-first on Wayland, native-tool on X11.** Both rely on dependencies the user (or our SlackBuild `REQUIRES=`) already needs for the rest of Fono.
- **Privacy parity with v0.8.2 window-context.** Reuse the existing private-window allow-list (KeePassXC, Bitwarden, etc., `crates/fono/src/context.rs`). For `window` mode, if the focused window is on that list, the call returns `CaptureError::PrivateWindow`. For `select`, the user is drawing the rectangle interactively so intent is explicit — capture proceeds, but the source metadata notes any private-window bounding box overlap.
- **Vision-capable provider gate.** If the configured assistant / agent doesn't accept image input, the trigger phrase is acknowledged with a spoken hint instead of a wasted API call.
- **One capture pipeline.** Not a new crate — a small module under `fono-core`, reused by MCP and assistant code paths.

## Implementation Plan

### Phase 0 — Decision, ADR, and dep audit

- [ ] Task 0.1. Write ADR `0031-visual-context-capture.md` recording the portal-first / shell-out strategy, the 250 KB binary ceiling, the two-region model (`window` / `select` only — full-screen deliberately excluded), the privacy-list reuse, and the explicit decision **not** to embed an in-process image encoder.
- [ ] Task 0.2. Audit current dependencies for transitive reuse: confirm `zbus` is already present (portal hotkey backend uses it), confirm `base64` is present (used by cloud STT clients), confirm `xdotool` is already a documented runtime dep. Document any genuinely-new system dependency in `docs/providers.md` and `packaging/slackbuild/fono/fono.info`:
  - **wlroots Wayland**: `grim` (window) + `slurp` (region select via `grim -g "$(slurp -d)"`).
  - **GNOME / KDE Wayland**: portal handles both regions; no extra dep.
  - **X11**: `maim` preferred (`-i` for window, `-s` for select) or fallback ladder `scrot -s` / `import`.
- [ ] Task 0.3. Add a CHANGELOG `[Unreleased]` stub entry and a ROADMAP `Up next` block titled **Visual context for agents and assistant**.

### Phase 1 — Core capture module (`fono-core::screen_capture`)

- [ ] Task 1.1. Add `screen_capture.rs` to `fono-core` with a single trait `ScreenCapture` and one struct per backend: `PortalCapture` (Wayland), `X11Capture`, and `NoopCapture`. Selection mirrors the overlay backend selection (`WAYLAND_DISPLAY` / `DISPLAY` presence). Forced override env var `FONO_CAPTURE_BACKEND=portal|x11|noop`. Public API: `capture(region: CaptureRegion) -> Result<CapturedImage, CaptureError>` where `CaptureRegion::{Window, Select}`.
- [ ] Task 1.2. Implement `PortalCapture` against `org.freedesktop.portal.Screenshot` via existing `zbus`.
  - `Window` → portal `Screenshot` with `interactive: false`; on portals that refuse non-interactive (older GNOME), fall through to `interactive: true` with a one-line banner.
  - `Select` → portal `Screenshot` with `interactive: true` (every modern portal exposes a rectangle tool). On wlroots compositors without portal Screenshot, fall through to `grim -g "$(slurp -d)"`.
  - Result is a `file://` URI Fono reads, base64s, and discards (unlinking the temp file).
- [ ] Task 1.3. Implement `X11Capture` by shelling out to a probe ladder per region:
  - `Window`: `maim --window $(xdotool getactivewindow)` → `scrot -u` → `import -window <id>`.
  - `Select`: `maim -s` → `scrot -s` → `import` (drag select built-in).
  - First binary present in `PATH` wins; probe cached once on daemon startup. **No in-process X11 protocol code.**
- [ ] Task 1.4. Result type: `CapturedImage { png_bytes: Vec<u8>, source: CaptureSource, width: u32, height: u32, mime: &'static str }`. `CaptureSource::{Window { wm_class, title }, Region { x, y, w, h, source_window: Option<String> } }`. Width/height parsed cheaply from PNG IHDR chunk (~12 bytes, hand-rolled, no `image` crate).
- [ ] Task 1.5. `CaptureError::Cancelled` returned when the user dismisses the region picker. Callers surface this distinctly — assistant says "OK, nothing to look at then" and continues text-only; MCP tool returns `{ "cancelled": true }`.
- [ ] Task 1.6. Optional downscaling shell-out: when `png_bytes.len() > 2 MiB`, attempt `magick convert - -resize 1600x1600\> png:-` via stdin/stdout. Absent `magick` → ship original.
- [ ] Task 1.7. Unit tests with a mock `ScreenCapture` trait object covering both regions plus cancel; one integration test gated on `FONO_TEST_REAL_CAPTURE=1`.

### Phase 2 — Privacy and policy layer

- [ ] Task 2.1. Reuse `WindowContext` from `crates/fono/src/context.rs` at capture time. For `Window` mode, if the focused window matches the private-window allow-list, return `CaptureError::PrivateWindow`. `Select` mode proceeds — user intent is explicit.
- [ ] Task 2.2. Add `[capture]` config block: `enabled = true`, `max_bytes_kb = 2048`, `default_region = "window" | "select"` (default `"window"`), `private_window_classes = [...]`, `redact_window_titles = false`, `select_picker = "auto" | "portal" | "slurp" | "maim"`.
- [ ] Task 2.3. Add `fono use capture on|off` CLI verb and tray toggle. Tray badge flashes briefly on every capture.

### Phase 3 — MCP tool `fono.screen`

- [ ] Task 3.1. Add `crates/fono-mcp-server/src/tools/screen.rs` exposing `fono.screen` with input schema `{ "region": "window" | "select", "annotate"?: string }` (default `"window"`) and output as MCP `image` content block (base64 PNG) plus a text block with source metadata (region, `wm_class`, dimensions, capture-mode used).
- [ ] Task 3.2. Wire through `ToolRegistry` and `McpContext`. Use `McpActivityGuard` so tray flashes amber during capture; `select` shows a dotted-rectangle badge so the user knows what to do.
- [ ] Task 3.3. Update `assets/agent-presets/voice.md`: call `fono.screen { "region": "window" }` when the user says "look at this"; call `fono.screen { "region": "select" }` when they say "let me show you a piece of this" or "this part".
- [ ] Task 3.4. Document in `docs/coding-agents.md`, per-agent verification matrix (Forge / Claude Code / Cursor).

### Phase 4 — Voice assistant trigger (F8 path)

- [ ] Task 4.1. Add trigger-phrase matcher in `crates/fono-assistant/src/screen_trigger.rs`. Multilingual defaults for `en`, `ro`, `fr`, `de`, `es`, `pt`, `it`, `ja`. Returns `Option<CaptureRegion>`:
  - "what am I looking at" / "look at this window" / "what's here" → `Window`.
  - "let me show you" / "look at this part" / "circle this" / "what's this" → `Select`.
- [ ] Task 4.2. When triggered and vision-capable: speak acknowledgement first ("Looking at this window…" / "Pick the area…"), capture, attach image as multimodal content block.
- [ ] Task 4.3. When triggered but vision unavailable: speak fallback, continue text-only.
- [ ] Task 4.4. Keyboard overrides: `Shift+F8` → assistant + `Window`; `Ctrl+F8` → assistant + `Select`. Implemented as `HotkeyAction::AssistantWithScreenPressed { region }`.

### Phase 5 — Terminal text fast path (optional)

- [ ] Task 5.1. When `region == Window` and the focused `wm_class` is a known terminal: probe `tmux capture-pane -p -S -100` → `screen -X hardcopy` → AT-SPI read. Attach text *and* image when both available.
- [ ] Task 5.2. Skip for `Select` — user framed pixels, respect that.
- [ ] Task 5.3. Gate on `[capture].terminal_text_extraction = true`, default off. Cut this phase first if binary budget is tight.

### Phase 6 — Documentation, doctor, release

- [ ] Task 6.1. Update `docs/coding-agents.md` and `docs/providers.md`: tool docs, per-distro grabber dep matrix, trigger-phrase list.
- [ ] Task 6.2. Add `fono doctor` rows: "Screen capture (window): portal" / "Screen capture (select): slurp" / "Screen capture: unavailable — install one of grim+slurp / maim / scrot". Include cached binary paths.
- [ ] Task 6.3. Measure binary delta; refuse merge if > 250 KB. Cut Phase 5 first, then downscale ladder, if over budget.
- [ ] Task 6.4. CHANGELOG graduation, ROADMAP move from "Up next" to "Shipped", ADR 0031 cross-link.

## Verification Criteria

- `fono.screen { "region": "window" }` round-trips against Claude Code and Forge: agent names a UI element from the image in its next turn.
- `fono.screen { "region": "select" }` opens the OS-native picker on wlroots, GNOME-Wayland, KDE-Wayland, X11.
- "Let me show you something" with F8 on a vision-capable provider opens the picker; model describes only the framed area.
- Esc during the picker returns cleanly; assistant continues text-only.
- KeePassXC focused + Shift+F8 → `PrivateWindow` error spoken; Ctrl+F8 opens picker and captures only the user-framed rectangle.
- `fono doctor` reports capture backend correctly on all four compositor families.
- Binary delta ≤ 250 KB on both CPU and GPU builds.
- Pre-commit gate green: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --tests --lib`.

## Risks and Mitigations

1. **Binary size.** No in-process encoder; shell out for PNG. `zbus` + `base64` already in workspace. Cut Phase 5 if budget is tight.
2. **GNOME portal interactive prompt on `Window` mode.** Accept the one-click confirm on older GNOME; document in `docs/providers.md`. `Select` mode always wants interaction anyway.
3. **wlroots without portal.** Probe ladder falls through to `grim` + `slurp` — tools every Sway/Hyprland/niri user already has.
4. **Privacy.** Private-window list blocks `window`; `select` trusts user intent; tray flash on every capture; `fono use capture off` kill switch.
5. **Trigger-phrase false positives.** Require explicit screen/window/terminal noun or explicit framing verb. Deterministic hotkeys bypass the matcher entirely.
6. **Cancelled select.** `CaptureError::Cancelled` is first-class; graceful text-only fallback.
7. **Agents calling `fono.screen` unprompted in `select` mode.** Rate-limit one call per user turn; preset explicitly discourages unprompted captures.
