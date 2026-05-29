# Visual Context for Coding Agents and the Voice Assistant

## Objective

Let users say things like *"look at this error on my screen and fix it"* (to a coding agent over MCP) or *"what am I seeing here?"* (to the F8 voice assistant) and have Fono attach a screenshot — focused window, full screen, or a user-drawn rectangle — to the next agent / assistant turn, without growing the binary by more than ~250 KB.

Two consumers, one capture pipeline:

1. **MCP path** — a new `fono.screen` tool exposed by `fono-mcp-server` that returns an MCP `image` content block, callable by Claude Code, Cursor, Forge, etc.
2. **Assistant path (F8)** — the existing voice assistant detects a small set of trigger phrases ("look at my screen", "what am I seeing", "ce vezi pe ecran", etc.) and attaches the captured image to the outgoing multimodal chat request, gated on `[assistant].prefer_vision = true`.

Three capture regions, picked per call:

- **`window`** (default) — the currently focused window, no prompt, no UI.
- **`screen`** — every pixel on the primary output, no prompt, no UI.
- **`select`** — the OS-native rectangle picker (slurp / slop / portal interactive mode); the user drags a box and Fono captures only that area. Picker is dismissable; cancelled selection returns `CaptureError::Cancelled`.

The voice assistant chooses the region based on trigger-phrase shape ("look at this window" → window, "look at my screen" → screen, "look at **this**" with no noun, or "let me show you" → select). MCP callers pass `region` explicitly.

## Design constraints

- **Binary budget: +250 KB ceiling, hard.** The CPU build is at ~21.24 MiB against a 22 MiB CI gate (`.github/workflows/ci.yml:184`). No new heavy deps. No `image` / `png` / `gstreamer` crates in-process. PNG encoding is done by the OS-side grabber tool; Fono just reads bytes and base64s them.
- **Portal-first on Wayland, native-tool on X11.** Both rely on dependencies the user (or our SlackBuild `REQUIRES=`) already needs for the rest of Fono.
- **Privacy parity with v0.8.2 window-context.** Reuse the existing private-window allow-list (KeePassXC, Bitwarden, etc., `crates/fono/src/context.rs`). If the focused window is on that list, the `window` and `screen` modes return an error; `select` is allowed because the user is explicitly framing the rectangle and may want to capture a partial screen that happens to neighbour a private window.
- **Vision-capable provider gate.** If the configured assistant / agent doesn't accept image input, the trigger phrase is acknowledged with a spoken hint instead of a wasted API call.
- **One capture pipeline.** `fono-capture` is *not* a new crate — it's a small module under `fono-core` (or `fono-inject`, alongside window-focus probing). Reused by MCP and assistant code paths.

## Implementation Plan

### Phase 0 — Decision, ADR, and dep audit

- [ ] Task 0.1. Write ADR `0031-visual-context-capture.md` recording the portal-first / shell-out strategy, the 250 KB binary ceiling, the three-region model (`window` / `screen` / `select`), the privacy-list reuse, and the explicit decision **not** to embed an in-process image encoder.
- [ ] Task 0.2. Audit current dependencies for transitive reuse: confirm `zbus` is already present (portal hotkey backend uses it), confirm `base64` is present (used by cloud STT clients), confirm `xdotool` is already a documented runtime dep. Document any genuinely-new system dependency in `docs/providers.md` and `packaging/slackbuild/fono/fono.info`:
  - **wlroots Wayland**: `grim` (full / window) + `slurp` (region select).
  - **GNOME / KDE Wayland**: portal handles all three regions; no extra dep.
  - **X11**: `maim` (preferred — `-s` does region select natively, `-i` does window) or fallback ladder `scrot -s` / `import` (mouse drag built-in).
- [ ] Task 0.3. Add a CHANGELOG `[Unreleased]` stub entry and a ROADMAP `Up next` block titled **Visual context for agents and assistant**.

### Phase 1 — Core capture module (`fono-core::screen_capture`)

- [ ] Task 1.1. Add `screen_capture.rs` to `fono-core` with a single trait `ScreenCapture` and one struct per backend: `PortalCapture` (Wayland), `X11Capture`, and `NoopCapture`. Selection table mirrors the overlay backend selection (`WAYLAND_DISPLAY` / `DISPLAY` presence). Forced override env var `FONO_CAPTURE_BACKEND=portal|x11|noop`. Public API: `capture(region: CaptureRegion) -> Result<CapturedImage, CaptureError>` where `CaptureRegion::{Window, FullScreen, Select}`.
- [ ] Task 1.2. Implement `PortalCapture` against `org.freedesktop.portal.Screenshot` via existing `zbus`.
  - `Window` / `FullScreen` → portal `Screenshot` with `interactive: false`; on portals that refuse non-interactive (older GNOME), fall through to `interactive: true` once with a banner that says "GNOME requires confirming each capture".
  - `Select` → portal `Screenshot` with `interactive: true` (every modern portal exposes a rectangle tool in its interactive UI). On wlroots compositors that don't ship the portal Screenshot at all, fall through to `grim -g "$(slurp -d)"`.
  - Result is a `file://` URI Fono reads, base64s, and discards (unlinking the temp file).
- [ ] Task 1.3. Implement `X11Capture` by shelling out to a probe ladder per region:
  - `Window`: `maim --window $(xdotool getactivewindow)` → `scrot -u` → `import -window <id>`.
  - `FullScreen`: `maim` → `scrot` → `import root`.
  - `Select`: `maim -s` → `scrot -s` → `import` (drag select is built-in to ImageMagick's `import`).
  - First binary present in `PATH` wins per region; ladder is probed once on daemon startup and cached. **No in-process X11 protocol code** — keeps the binary lean and avoids pulling in `x11rb` features we don't already have.
- [ ] Task 1.4. Result type: `CapturedImage { png_bytes: Vec<u8>, source: CaptureSource, width: u32, height: u32, mime: &'static str }`. `CaptureSource::{Window { wm_class, title }, Region { x, y, w, h, source_window: Option<String> }, FullScreen }`. Width/height parsed cheaply from the PNG IHDR chunk (~12 bytes, hand-rolled, no `image` crate).
- [ ] Task 1.5. `CaptureError::Cancelled` variant returned when the user dismisses the region picker (Esc, right-click, portal close). Callers (MCP tool and assistant) surface this distinctly from a hard failure — the assistant says "OK, nothing to look at then" and continues text-only; the MCP tool returns a clean `{ "cancelled": true }` payload.
- [ ] Task 1.6. Optional downscaling shell-out: when `png_bytes.len() > 2 MiB`, attempt `magick convert - -resize 1600x1600\> png:-` via stdin/stdout. If `magick` / `convert` is absent, ship the original — vision providers accept large images, this is purely a cost/latency optimisation.
- [ ] Task 1.7. Unit tests with a mock `ScreenCapture` trait object covering all three regions plus the cancel path; one integration test gated on `FONO_TEST_REAL_CAPTURE=1` so CI doesn't try to grab a desktop.

### Phase 2 — Privacy and policy layer

- [ ] Task 2.1. Reuse `WindowContext` from `crates/fono/src/context.rs` to determine the focused window's `wm_class` at capture time. For `Window` and `FullScreen` regions, if the focused window matches the private-window allow-list (KeePassXC, Bitwarden, Veracrypt, KeePass, password managers in general), the call returns `CaptureError::PrivateWindow`. For `Select`, the user is drawing the rectangle interactively, so we trust the user's intent — but the source metadata still flags whether a private-window bounding box overlaps the captured rectangle, surfaced to the user via the tray flash.
- [ ] Task 2.2. Add `[capture]` config block: `enabled = true`, `max_bytes_kb = 2048`, `default_region = "window" | "screen" | "select"` (default `"window"`), `private_window_classes = [...]` (override list), `redact_window_titles = false`, `select_picker = "auto" | "portal" | "slurp" | "maim"`.
- [ ] Task 2.3. Add `fono use capture on|off` CLI verb and a tray submenu toggle so the user can disable screen capture session-wide with one click. Tray badge briefly flashes when a capture fires so it never happens invisibly.

### Phase 3 — MCP tool `fono.screen`

- [ ] Task 3.1. Add `crates/fono-mcp-server/src/tools/screen.rs` exposing `fono.screen` with input schema `{ "region": "window" | "screen" | "select", "annotate"?: string }` (default `"window"`) and output as an MCP `image` content block (base64 PNG) plus a text block with the source metadata (region, `wm_class`, dimensions, capture-mode used). `{"region":"select"}` blocks until the user finishes the rectangle picker, with a 30 s timeout that returns `Cancelled` if the user wanders off.
- [ ] Task 3.2. Wire the tool through `ToolRegistry` and the existing `McpContext`. Re-use the `McpActivityGuard` pattern so the tray flashes amber for the duration of the capture (matches `fono.listen` / `fono.speak`); during `select` the tray badge shows a small dotted-rectangle glyph so the user remembers what they're being asked to do.
- [ ] Task 3.3. Add the tool description in `assets/agent-presets/voice.md` so coding agents know to call `fono.screen` after the user says "look at my screen" / "let me show you a piece of this" / equivalent, before producing a fix. Document the three regions and recommend `"select"` when the user says "this part of" or "let me circle something".
- [ ] Task 3.4. Document the tool in `docs/coding-agents.md` and add a per-agent verification matrix entry (Forge / Claude Code / Cursor).

### Phase 4 — Voice assistant trigger (F8 path)

- [ ] Task 4.1. Add a small trigger-phrase matcher in `crates/fono-assistant/src/screen_trigger.rs`. A static list of multilingual phrases keyed by `[language].assistant_screen_triggers` in the config (defaults shipped for `en`, `ro`, `fr`, `de`, `es`, `pt`, `it`, `ja` — the languages the wizard already advertises). Match is whole-utterance substring + a few regex variants. Output is `(matched: bool, region: CaptureRegion)`:
  - "what's on **my screen**" / "look at **my screen**" → `FullScreen`.
  - "look at **this window**" / "what am I looking at" → `Window`.
  - "look at **this part**" / "what's **this**" / "let me show you something" / "circle this" → `Select`.
- [ ] Task 4.2. When the matcher fires **and** the configured assistant model is vision-capable (per `provider_catalog.rs` capability flag and `[assistant].prefer_vision = true`), the assistant pipeline:
  1. Speaks a one-line acknowledgement *before* the capture so the user knows what's happening: "Looking at your screen…" / "Pick the area…" / "Looking at this window…".
  2. Calls `screen_capture::capture(region)`.
  3. If `region == Select`, the dictation overlay flips to a `Selecting…` state with the same walking-letter animation `Pondering` uses, so the user has a visual cue they're in the picker. Cancellation returns gracefully.
  4. Inserts the resulting image as a multimodal content block before the user's text in the outgoing chat request.
- [ ] Task 4.3. When the matcher fires but vision is unavailable, speak a short fallback ("I can't see screens with the current assistant model — switch to OpenAI or Anthropic in the tray to enable that.") and continue the turn as text-only.
- [ ] Task 4.4. Manual hotkey overrides — no trigger-phrase matching needed:
  - `Shift+F8` — push-to-talk, attach `Window`.
  - `Ctrl+F8` — push-to-talk, open the `Select` picker first, then record.
  - Implemented as `HotkeyAction::AssistantWithScreenPressed { region }`. No new FSM states beyond the variant.

### Phase 5 — Terminal text fast path (optional, cheap quality bump)

- [ ] Task 5.1. When the captured region is a focused terminal emulator (alacritty, kitty, foot, gnome-terminal, konsole, xterm, wezterm) **and** the region is `Window`, prefer text-extraction over screenshot. Three probes in order: (a) `tmux capture-pane -p -S -100` if `TMUX` env var is on the focused process, (b) `screen -X hardcopy` likewise, (c) AT-SPI accessibility tree read via `dbus-send` to `org.a11y.atspi` if the bus is up.
- [ ] Task 5.2. If text extraction succeeds, attach the text *and* the screenshot when room allows; otherwise the screenshot alone. Text wins on cost (text tokens are ~25× cheaper than image tokens at the vision tier) and on accuracy (no OCR error).
- [ ] Task 5.3. Skip the fast path entirely for `Select` and `FullScreen` — the user explicitly framed pixels.
- [ ] Task 5.4. This phase is genuinely optional — defer to Phase 6 if Phase 1–4 already hit the binary budget or if AT-SPI bindings prove non-trivial.

### Phase 6 — Documentation, telemetry, release

- [ ] Task 6.1. Update `docs/coding-agents.md` and `docs/providers.md` with the new tool, the three regions, the per-distro grabber dep matrix, and the trigger-phrase list.
- [ ] Task 6.2. Add `fono doctor` rows: "Screen capture (window): portal" / "Screen capture (select): slurp" / "Screen capture: unavailable — install one of grim+slurp / maim / scrot". Include the cached binary paths so packaging issues are visible.
- [ ] Task 6.3. Measure the release binary size before and after; refuse to merge if the delta exceeds 250 KB. If it does, drop Phase 5 first, then drop the downscale ladder, then revisit.
- [ ] Task 6.4. CHANGELOG `[Unreleased]` graduation, ROADMAP move from "Up next" to "Shipped" with the release tag, ADR 0031 cross-link.

## Verification Criteria

- `fono.screen { "region": "window" }` round-trips end-to-end against Claude Code and Forge: agent calls the tool, receives a PNG, names a UI element from the image in its next turn.
- `fono.screen { "region": "select" }` opens the OS-native rectangle picker on each of: wlroots (slurp), GNOME-Wayland (portal interactive), KDE-Wayland (portal interactive), X11 (`maim -s` fallback ladder).
- Saying "let me show you something" with the F8 assistant on a vision-capable provider opens the region picker, captures the user's rectangle, and the model describes only the framed area.
- Saying "what's on my screen?" attaches the full screen; "look at this window" attaches the focused window.
- Saying the same on a non-vision provider produces the spoken fallback, never a crash, never a wasted API call.
- Focusing KeePassXC and pressing Shift+F8 returns `CaptureError::PrivateWindow`; pressing Ctrl+F8 opens the region picker and captures only what the user frames.
- Pressing Esc during the region picker returns cleanly with `Cancelled` and the assistant says "OK, nothing to look at then" and continues text-only.
- `fono doctor` correctly reports the active capture backend per region on Wayland-wlroots, GNOME-Wayland, KDE-Wayland, and X11.
- Release-binary size delta vs the pre-feature baseline is ≤ 250 KB on the CPU build and ≤ 250 KB on the GPU build.
- Pre-commit gate green: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --tests --lib`.

## Potential Risks and Mitigations

1. **Binary size creep from new dependencies.**
   Mitigation: Do not add an in-process image encoder. Shell out for PNG and for resize. `zbus` and `base64` are already in the workspace. If the budget is exceeded, drop Phase 5 (terminal text), then drop in-process downscale, then drop multilingual trigger-phrase tables (English-only initial ship).

2. **GNOME portal forces an interactive crosshair prompt on every `Window` / `FullScreen` call.**
   Mitigation: Document the GNOME 47 / 48 non-interactive flag in `docs/providers.md`; on older GNOME, accept the one-click prompt as the cost of doing business. `Select` mode is unaffected — it *wants* an interactive picker.

3. **wlroots compositors without portal Screenshot.**
   Mitigation: Probe ladder falls through to `grim` (window / full) and `grim -g "$(slurp -d)"` (select). Both are tiny C tools every Sway / Hyprland / niri user already has.

4. **Sensitive content leaking to a cloud LLM by accident.**
   Mitigation: Tray badge flashes on every capture; private-window allow-list blocks password managers on `window` / `screen`; `Select` mode trusts the user's framing; `fono use capture off` is one click away; `[capture].redact_window_titles` strips the title from the metadata text block; the trigger-phrase list is conservative and explicit.

5. **Trigger-phrase false positives in normal conversation.**
   Mitigation: Phrases require the literal word "screen", "terminal", "window", "display", "this", or locale equivalents — bare "look" doesn't fire. `Shift+F8` / `Ctrl+F8` exist as deterministic paths for users who want to skip the phrase matcher entirely.

6. **User cancels the `Select` picker mid-assistant-turn.**
   Mitigation: `CaptureError::Cancelled` is a first-class result; assistant degrades to text-only without complaint and remembers the user's spoken context so the rest of the question still gets answered.

7. **MCP agents calling `fono.screen` unprompted on every turn, especially in `select` mode (which is interruptive).**
   Mitigation: Document in the voice preset that `select` is for explicit user framing only; add a one-call-per-user-turn rate-limit in the tool wrapper to bound the cost of a misbehaving agent.

8. **AT-SPI / accessibility bus requirement on Phase 5.**
   Mitigation: Phase 5 is optional and gated behind a `[capture].terminal_text_extraction = true` opt-in. Default off until we have a measured improvement on the coding-agent workflow.

## Alternative Approaches

1. **Always-on screen-sharing stream (Wayland ScreenCast portal).** Continuous frames into a ring buffer; assistant pulls the most recent frame at trigger time. Trade-off: substantially heavier (PipeWire client in-process, ~600 KB of new deps), constant CPU/GPU draw, and a persistent screen-share indicator on GNOME / KDE. Rejected for the binary budget and the UX cost.

2. **In-process screenshot via `xcap` or `screenshots` crate.** Cleaner code, no external tool dependency. Trade-off: pulls in ~1 MB of platform code (Wayland + X11 + macOS scaffolding). Rejected.

3. **OCR-the-screenshot client-side and send text only.** Privacy- and cost-friendly. Trade-off: requires a Tesseract or PaddleOCR sidecar, and modern vision LLMs read screenshots better than Tesseract. Rejected for v1.

4. **Only ship the MCP tool, defer the F8 assistant path.** Smaller change. Trade-off: leaves the headline "what am I seeing here?" use case on the floor. Rejected.

5. **Built-in Fono region picker (we draw the overlay).** Visually consistent across compositors. Trade-off: input grabs on Wayland are painful, edge cases are endless, and reinventing what `slurp` and `maim -s` already do well is a binary-budget loss. Rejected — defer to the OS-native pickers.
