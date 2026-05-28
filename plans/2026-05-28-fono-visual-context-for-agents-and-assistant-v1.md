# Visual Context for Coding Agents and the Voice Assistant

## Objective

Let users say things like *"look at this error on my screen and fix it"* (to a coding agent over MCP) or *"what am I seeing here?"* (to the F8 voice assistant) and have Fono attach a screenshot of the focused window — or the whole screen — to the next agent / assistant turn, without growing the binary by more than ~250 KB.

Two consumers, one capture pipeline:

1. **MCP path** — a new `fono.screen` tool exposed by `fono-mcp-server` that returns an MCP `image` content block, callable by Claude Code, Cursor, Forge, etc.
2. **Assistant path (F8)** — the existing voice assistant detects a small set of trigger phrases ("look at my screen", "what am I seeing", "ce vezi pe ecran", etc.) and attaches the captured image to the outgoing multimodal chat request, gated on `[assistant].prefer_vision = true`.

## Design constraints

- **Binary budget: +250 KB ceiling, hard.** The CPU build is at ~21.24 MiB against a 22 MiB CI gate (`.github/workflows/ci.yml:184`). No new heavy deps. No `image` / `png` / `gstreamer` crates in-process. PNG encoding is done by the OS-side grabber tool; Fono just reads bytes and base64s them.
- **Portal-first on Wayland, native-tool on X11.** Both rely on dependencies the user (or our SlackBuild `REQUIRES=`) already needs for the rest of Fono.
- **Privacy parity with v0.8.2 window-context.** Reuse the existing private-window allow-list (KeePassXC, Bitwarden, etc., `crates/fono/src/context.rs`). If the focused window is on that list, the tool returns an error and the assistant says "I can't see that one" out loud.
- **Vision-capable provider gate.** If the configured assistant / agent doesn't accept image input, the trigger phrase is acknowledged with a spoken hint instead of a wasted API call.
- **One capture pipeline.** `fono-capture` is *not* a new crate — it's a small module under `fono-core` (or `fono-inject`, alongside window-focus probing). Reused by MCP and assistant code paths.

## Implementation Plan

### Phase 0 — Decision, ADR, and dep audit

- [ ] Task 0.1. Write ADR `0031-visual-context-capture.md` recording the portal-first / shell-out strategy, the 250 KB binary ceiling, the privacy-list reuse, and the explicit decision **not** to embed an in-process image encoder.
- [ ] Task 0.2. Audit current dependencies for transitive reuse: confirm `zbus` is already present (portal hotkey backend uses it), confirm `base64` is present (used by cloud STT clients), confirm `xdotool` is already a documented runtime dep. Document any genuinely-new system dependency (`grim` on wlroots, `gnome-screenshot` on GNOME-Wayland, `spectacle` on KDE, `maim` or `scrot` or `import` on X11) in `docs/providers.md` and `packaging/slackbuild/fono/fono.info`.
- [ ] Task 0.3. Add a CHANGELOG `[Unreleased]` stub entry and a ROADMAP `Up next` block titled **Visual context for agents and assistant**.

### Phase 1 — Core capture module (`fono-core::screen_capture`)

- [ ] Task 1.1. Add `screen_capture.rs` to `fono-core` with a single trait `ScreenCapture` and one struct per backend: `PortalCapture` (Wayland), `X11Capture`, and `NoopCapture`. Selection table mirrors the overlay backend selection (`WAYLAND_DISPLAY` / `DISPLAY` presence). Forced override env var `FONO_CAPTURE_BACKEND=portal|x11|noop`.
- [ ] Task 1.2. Implement `PortalCapture` against `org.freedesktop.portal.Screenshot` via existing `zbus`. Request modal flags: `interactive: false`, `handle_token: <random>`, `modal: false`. Result is a `file://` URI Fono reads, base64s, and discards (unlinking the temp file). On portals that lack the non-interactive mode (older GNOME), set `interactive: true` and let the user confirm — slower but legal everywhere.
- [ ] Task 1.3. Implement `X11Capture` by shelling out to a probe ladder: `maim --window $(xdotool getactivewindow)` → `scrot -u` → `import -window <id>` → full-screen fallback `maim` / `scrot` / `import root`. First binary in `PATH` wins; ladder is built once on daemon startup and cached. **No in-process X11 protocol code** — keeps the binary lean and avoids pulling in `x11rb` features we don't already have.
- [ ] Task 1.4. Result type: `CapturedImage { png_bytes: Vec<u8>, source: CaptureSource, width: u32, height: u32, mime: &'static str }`. `CaptureSource::{Window { wm_class, title }, Region { x, y, w, h }, FullScreen }`. Width/height parsed cheaply from the PNG IHDR chunk (~12 bytes, hand-rolled, no `image` crate).
- [ ] Task 1.5. Optional downscaling shell-out: when `png_bytes.len() > 2 MiB`, attempt `magick convert - -resize 1600x1600\> png:-` via stdin/stdout. If `magick` / `convert` is absent, ship the original — vision providers accept large images, this is purely a cost/latency optimisation.
- [ ] Task 1.6. Unit tests with a mock `ScreenCapture` trait object; one integration test gated on `FONO_TEST_REAL_CAPTURE=1` so CI doesn't try to grab a desktop.

### Phase 2 — Privacy and policy layer

- [ ] Task 2.1. Reuse `WindowContext` from `crates/fono/src/context.rs` to determine the focused window's `wm_class` at capture time. If it matches the private-window allow-list (KeePassXC, Bitwarden, Veracrypt, KeePass, password managers in general), `screen_capture` returns `CaptureError::PrivateWindow` instead of bytes.
- [ ] Task 2.2. Add `[capture]` config block: `enabled = true`, `max_bytes_kb = 2048`, `prefer = "window" | "screen"`, `private_window_classes = [...]` (override list), `redact_window_titles = false` (when true, the title field in the source metadata is blanked).
- [ ] Task 2.3. Add `fono use capture on|off` CLI verb and a tray submenu toggle so the user can disable screen capture session-wide with one click. Tray badge briefly flashes when a capture fires so it never happens invisibly.

### Phase 3 — MCP tool `fono.screen`

- [ ] Task 3.1. Add `crates/fono-mcp-server/src/tools/screen.rs` exposing `fono.screen` with input schema `{ "region": "window"|"screen", "annotate"?: string }` and output as an MCP `image` content block (base64 PNG) plus a text block with the source metadata (`wm_class`, dimensions, capture-mode used).
- [ ] Task 3.2. Wire the tool through `ToolRegistry` and the existing `McpContext`. Re-use the `McpActivityGuard` pattern so the tray flashes amber for the duration of the capture (matches `fono.listen` / `fono.speak`).
- [ ] Task 3.3. Add the tool description in `assets/agent-presets/voice.md` so coding agents know to call `fono.screen` after the user says "look at my screen" or anything semantically close, before producing a fix.
- [ ] Task 3.4. Document the tool in `docs/coding-agents.md` and add a per-agent verification matrix entry (Forge / Claude Code / Cursor).

### Phase 4 — Voice assistant trigger (F8 path)

- [ ] Task 4.1. Add a small trigger-phrase matcher in `crates/fono-assistant/src/screen_trigger.rs`. A static list of multilingual phrases keyed by `[language].assistant_screen_triggers` in the config (defaults shipped for `en`, `ro`, `fr`, `de`, `es`, `pt`, `it`, `ja` — the languages the wizard already advertises). Match is whole-utterance substring + a few regex variants ("what am I (?:seeing|looking at)", "look at (?:my|this) (?:screen|terminal|window)", etc.). Cheap; no LLM call.
- [ ] Task 4.2. When the matcher fires **and** the configured assistant model is vision-capable (per `provider_catalog.rs` capability flag and `[assistant].prefer_vision = true`), the assistant pipeline:
  1. Calls `screen_capture::capture(prefer="window")`.
  2. Inserts the resulting image as a multimodal content block before the user's text in the outgoing chat request.
  3. Speaks a one-line acknowledgement *before* the model responds ("Looking at your screen — one second.") so the user knows the picture was taken.
- [ ] Task 4.3. When the matcher fires but vision is unavailable, speak a short fallback ("I can't see screens with the current assistant model — switch to OpenAI or Anthropic in the tray to enable that.") and continue the turn as text-only.
- [ ] Task 4.4. Manual hotkey override: pressing `Shift+F8` (push-to-talk) attaches a screenshot regardless of trigger phrase. Implemented as a new `HotkeyAction::AssistantWithScreenPressed` variant; the listener decides on the modifier at press time. No new state in the FSM beyond the variant — the rest of the assistant flow is unchanged.

### Phase 5 — Terminal text fast path (optional, cheap quality bump)

- [ ] Task 5.1. When the focused window's `wm_class` matches a known terminal emulator (alacritty, kitty, foot, gnome-terminal, konsole, xterm, wezterm), prefer text-extraction over screenshot. Three probes in order: (a) `tmux capture-pane -p -S -100` if `TMUX` env var is on the focused process, (b) `screen -X hardcopy` likewise, (c) AT-SPI accessibility tree read via `dbus-send` to `org.a11y.atspi` if the bus is up.
- [ ] Task 5.2. If text extraction succeeds, attach the text *and* the screenshot when room allows; otherwise the screenshot alone. Text wins on cost (text tokens are ~25× cheaper than image tokens at the vision tier) and on accuracy (no OCR error).
- [ ] Task 5.3. This phase is genuinely optional — defer to Phase 6 if Phase 1–4 already hit the binary budget or if AT-SPI bindings prove non-trivial.

### Phase 6 — Documentation, telemetry, release

- [ ] Task 6.1. Update `docs/coding-agents.md` and `docs/providers.md` with the new tool, the per-distro grabber dep, and the trigger-phrase list.
- [ ] Task 6.2. Add `fono doctor` rows: "Screen capture: portal (GNOME 46)" / "Screen capture: maim (X11)" / "Screen capture: unavailable — install one of grim/maim/scrot". Include the cached binary path so packaging issues are visible.
- [ ] Task 6.3. Measure the release binary size before and after; refuse to merge if the delta exceeds 250 KB. If it does, drop Phase 5 first, then drop the downscale ladder, then revisit.
- [ ] Task 6.4. CHANGELOG `[Unreleased]` graduation, ROADMAP move from "Up next" to "Shipped" with the release tag, ADR 0031 cross-link.

## Verification Criteria

- `fono.screen` round-trips end-to-end against Claude Code and Forge: agent calls the tool, receives a PNG, names a UI element from the image in its next turn.
- Saying "what am I looking at?" with the F8 assistant on a vision-capable provider (OpenAI / Anthropic / Gemini / Groq) results in the model accurately describing the focused window within ~3 s of release.
- Saying the same on a non-vision provider produces the spoken fallback, never a crash, never a wasted API call.
- Focusing KeePassXC and pressing Shift+F8 returns `CaptureError::PrivateWindow` and the assistant says it can't see that window.
- `fono doctor` correctly reports the active capture backend on Wayland-wlroots (`grim`), GNOME-Wayland (portal), KDE-Wayland (portal or `spectacle`), and X11 (`maim` / `scrot` / `import`).
- Release-binary size delta vs the pre-feature baseline is ≤ 250 KB on the CPU build and ≤ 250 KB on the GPU build.
- Pre-commit gate green: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --tests --lib`.

## Potential Risks and Mitigations

1. **Binary size creep from new dependencies.**
   Mitigation: Do not add an in-process image encoder. Shell out for PNG and for resize. `zbus` and `base64` are already in the workspace. If the budget is exceeded, drop Phase 5 (terminal text), then drop in-process downscale, then drop multilingual trigger-phrase tables (English-only initial ship).

2. **GNOME portal forces an interactive crosshair prompt every capture.**
   Mitigation: Document the GNOME 47 / 48 non-interactive flag in `docs/providers.md`; on older GNOME, offer the X11 / Xwayland path via the existing override-redirect backend (which Mutter accepts for Fono's overlay today) and shell out to `maim` against the Xwayland root window. Worst case the user picks `FONO_CAPTURE_BACKEND=x11`.

3. **Sensitive content leaking to a cloud LLM by accident.**
   Mitigation: Tray badge flashes on every capture; private-window allow-list blocks password managers; `fono use capture off` is one click away; `[capture].redact_window_titles` strips the title from the metadata text block; the trigger-phrase list is conservative and explicit.

4. **Trigger-phrase false positives in normal conversation.**
   Mitigation: Phrases require the literal word "screen", "terminal", "window", "display" (or locale equivalents) — bare "look at this" doesn't fire. `Shift+F8` exists as the deterministic path for users who want to skip the phrase matcher entirely.

5. **MCP agents calling `fono.screen` unprompted on every turn.**
   Mitigation: Document in the voice preset that the tool is *only* to be called when the user explicitly references their screen. Add a one-call-per-user-turn rate-limit in the tool wrapper to bound the cost of a misbehaving agent.

6. **AT-SPI / accessibility bus requirement on Phase 5.**
   Mitigation: Phase 5 is optional and gated behind a `[capture].terminal_text_extraction = true` opt-in. Default off until we have a measured improvement on the coding-agent workflow.

## Alternative Approaches

1. **Always-on screen-sharing stream (Wayland ScreenCast portal).** Continuous frames into a ring buffer; assistant pulls the most recent frame at trigger time. Trade-off: substantially heavier (PipeWire client in-process, ~600 KB of new deps), constant CPU/GPU draw, and a persistent screen-share indicator on GNOME / KDE. Rejected for the binary budget and the UX cost; revisit only if discrete captures prove too slow.

2. **In-process screenshot via `xcap` or `screenshots` crate.** Cleaner code, no external tool dependency, works without portals. Trade-off: pulls in ~1 MB of platform code (Wayland + X11 + macOS scaffolding), which blows the binary budget on its own. Rejected.

3. **OCR-the-screenshot client-side and send text only.** Privacy- and cost-friendly. Trade-off: requires a Tesseract or PaddleOCR sidecar (binary or system dep), and modern vision LLMs read screenshots better than Tesseract does. Rejected for v1; revisit if vision-API costs become a user complaint.

4. **Only ship the MCP tool, defer the F8 assistant path.** Smaller change. Trade-off: leaves the headline "what am I seeing here?" use case on the floor and doubles the work later when Phase 4 lands. Rejected — the matcher and the multimodal request plumbing are small additions next to the capture pipeline.
