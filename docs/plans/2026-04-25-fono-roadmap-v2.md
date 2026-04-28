# Fono — Post-Working-Pipeline Roadmap (v0.1.0 → v0.2.0)

Date: 2026-04-25
Status: tracking — see `docs/status.md` for what landed

The end-to-end dictation pipeline is working on the user's host (KDE
Wayland + xtest-paste + Shift+Insert + Groq STT + Groq LLM). This plan
sequences the remaining work into three tiers: blockers for tagging
v0.1.0, high-value features for v0.1.x, and longer-term v0.2 items.

---

## Tier 1 — block v0.1.0 release tag

### R1. Real-machine validation (manual)

* [ ] **R1.1** Smoke-test on a clean NimbleX VM: install, wizard, dictate,
  history viewer. Capture rough edges; file follow-up issues.
* [ ] **R1.2** Repeat on KDE Wayland with XWayland (user's setup).
  Confirm Shift+Insert path holds across terminal, browser, VS Code.
* [ ] **R1.3** Repeat on a `Minimum`-tier machine (4 cores, 4 GB RAM).
  Confirm hwcheck recommendation matches measured latency.

### R2. Notification & tray polish

* [x] **R2.1** Tray submenus for STT/LLM switching (deferred S15–S17).
  Right-click → `STT: ▾` / `LLM: ▾` shows every backend with active
  ticked; click to switch. Single source of truth (`set_active_stt`)
  shared with CLI.
* [ ] **R2.2** Notification action button "Edit last" — re-runs the prior
  raw transcript through the LLM with a correction prompt; useful for
  "the cleanup made it worse".
* [ ] **R2.3** Tray status badge for in-flight pipeline — already
  implemented, verify icon flips on KDE/GNOME/sway in real use.

### R3. Wizard path-of-least-surprise

* [x] **R3.1** In-wizard latency probe (commit `7bea0a9`; `crates/fono/src/wizard.rs:72,720,725`).
  After model
  download in the local branch, run a canned 3-second WAV through the
  just-installed whisper. If `stt_ms > 1500`, downgrade tier and
  re-prompt.
* [x] **R3.2** Cloud branch: paste-to-validate keys before persisting.
  Reuses `fono keys check` reachability probe.
* [x] **R3.3** Wizard offers mixed pipeline: "Cloud STT + Local LLM" and
  "Local STT + Cloud LLM". Currently it's all-cloud or all-local.

### R4. Docs + release plumbing

* [x] **R4.1** README first-run snippet matches current defaults
  (Shift+Insert, `fono use`, `fono keys`, `fono hwprobe`).
* [x] **R4.2** New `docs/inject.md` covering paste shortcut precedence,
  override examples, troubleshooting recipes (clipit, KDE Wayland,
  Shift+Insert in tmux copy mode).
* [x] **R4.3** New `docs/troubleshooting.md` consolidating common
  symptoms → fix recipes.
* [x] **R4.4** Release notes draft + `git tag v0.1.0`. CHANGELOG already
  exists — enumerate shipped models + SHA256s + the four landed plans
  (W/L/H/S). Tags `v0.1.0`, `v0.1.1`, `v0.2.0`, `v0.2.1` exist; current
  tip is `v0.2.1`.

### R5. Real-audio benchmark coverage

* [x] **R5.1** Real-fixture equivalence gate live in
  `.github/workflows/ci.yml` (Wave 2 Thread C). Runs
  `fono-bench equivalence --stt local --model tiny.en --baseline`
  on every PR and diffs per-fixture verdicts against
  `docs/bench/baseline-comfortable-tiny-en.json`. Whisper `tiny.en`
  weights cached via `actions/cache@v4` keyed on the model SHA.
* [x] **R5.2** Baseline JSON anchor seeded at
  `docs/bench/baseline-comfortable-tiny-en.json` (Wave 2 Thread C).
  Captures per-fixture verdicts + `model_capabilities` +
  `pinned_params`; absolute timings stripped via the `--baseline`
  flag so the file is deterministic across CI runners. Regen
  procedure documented in `docs/bench/README.md`. (`tiny.en` shape
  today; multilingual `small` baseline + p95 latency budget remain a
  follow-up — see Wave 5 / nightly job in the strategic plan.)

---

## Tier 2 — v0.1.x point releases

### F1. Streaming pipeline (latency plan L6/L7/L8/L10)

* [ ] **F1.1** Streaming LLM token output via SSE on `OpenAiCompat`.
  `mpsc::Sender<String>` into `TextFormatter::format`.
* [ ] **F1.2** Progressive injection: change `Injector::inject` to accept
  optional `mpsc::Receiver<String>` and inject tokens as they arrive
  (buffered to word boundaries).
* [ ] **F1.3** Streaming STT via Deepgram / AssemblyAI WebSocket.
* [ ] **F1.4** Speculative LLM connection prewarm at `StartRecording`.

### F2. Local LLM (H8)

* [ ] **F2.1** Implement `LlamaLocal` against `llama-cpp-2`. Today's stub
  returns `Err("not yet wired")`.
* [ ] **F2.2** Pin Qwen2.5-0.5B + 1.5B GGUF URLs and SHAs in the model
  registry per tier.
* [ ] **F2.3** Wizard recommends "STT local + LLM local" for any tier ≥
  Comfortable.

### F3. Profiles + cycle hotkey (S9/S10)

* [ ] **F3.1** `[profiles.<name>]` config tables; `fono profile
  save/list/use/delete`.
* [ ] **F3.2** `hotkeys.cycle_profile = "Ctrl+Alt+P"` cycles profiles.
* [ ] **F3.3** Tray submenu: `Profile: ▾` with checkmark on active.

### F4. Real overlay window

* [ ] **F4.1** `winit + softbuffer` overlay (today a stub). Pulsing dot +
  dB meter while recording.
* [ ] **F4.2** Position from config; X11 notification window-type;
  Wayland layer-shell.
* [ ] **F4.3** Click-to-cancel, Esc-to-cancel.

---

## Tier 3 — v0.2+, no urgency

* [ ] Per-app paste rules (focus-aware shortcut table; revisit only on
  real demand).
* [ ] Dictionary / vocabulary boost surfaced in wizard.
* [ ] Anthropic streaming (different protocol from F1.1).
* [ ] History viewer GUI (winit-based table; replaces CLI listing).
* [ ] Wayland-native global shortcut via
  `org.freedesktop.portal.GlobalShortcuts` once `global-hotkey`
  supports it.
* [ ] Auto-update channel.

---

## Verification gates still to clear

From `docs/plans/2026-04-24-fono-design-v1.md:512-537`:

* [ ] Static-musl build ≤ 25 MB stripped, `ldd` reports not dynamic.
  (Cloud-only slim variant; local-models flavour is glibc-linked per
  ADR 0007.)
* [ ] End-to-end dictation ≤ 2 s on 4-core x86_64 with **local** STT+LLM
  (blocked on F2).
* [ ] Six-artifact GitHub Release with SHA256SUMS + minisign (R4.4).
* [ ] NimbleX `.txz` installs and removes cleanly (R1.1).
* [ ] `fono doctor` green on NimbleX + i3 + PipeWire (R1.1 / R1.3).

---

## Recommended sequencing

1. **R1.1, R1.2, R1.3** — three real-machine smoke runs.
2. **R2.1** (tray STT/LLM submenus) — biggest non-technical-user UX win.
3. **R3.2** (key validation in wizard) — eliminates silent-fail mode.
4. **R3.3** (mixed cloud/local pipeline).
5. **R4.1, R4.2, R4.3, R4.4** — docs + release.
6. After v0.1.0 ships, open v0.1.1 milestone for **R2.2, R2.3, R3.1, R5**.
7. **v0.1.2**: F2 (local LLM real).
8. **v0.2.0**: F1 (streaming pipeline).
9. **v0.2.x**: F3, F4.

## Risks

1. **Tray submenu rebuild on `tray-icon` 0.19** — verify in-place
   `set_text`/`set_checked` works for STT/LLM submenus before committing.
2. **Streaming-injection × clipboard fallback** — XtestPaste has no clean
   primitive for "type tokens as they arrive"; F1.2 needs a story for
   paste-via-clipboard backends (likely: stream applies only to true
   keystroke backends like `wtype`/`xdotool`/`enigo`).
3. **LlamaLocal API churn** — `llama-cpp-2` is pre-1.0 and re-shapes its
   API across minor releases. Pin to a specific tag and treat F2 as
   breakage-prone.
