# Native Wayland onboarding + hotkeys + auto-paste — v5

## Status: Completed

Supersedes v4. One scope addition + two corrections from user feedback (2026-05-19):

1. **Auto-paste on native Wayland via `org.freedesktop.portal.RemoteDesktop`.** New `Injector::PortalRemoteDesktop` synthesises **Shift+Insert** at the cursor after a native clipboard set. The wizard asks for the portal permission *at the same time* as the GlobalShortcuts permission (Option B — both dialogs at first run, no deferred opt-in).
2. **No new config options** for this feature. Behaviour is hard-coded: ask for both permissions at first run; persist both session tokens; auto-paste with Shift+Insert when RemoteDesktop is granted; fall back to clipboard-only when denied or unavailable.
3. **Fix `Injector::detect()` Wtype false-positive** on GNOME-Wayland. `wtype` is currently selected at `crates/fono-inject/src/inject.rs:63-68` whenever the binary is on `$PATH`, but Mutter does not implement `zwp_virtual_keyboard_v1`; events are silently dropped. v5 probes the compositor for protocol support before selecting `Wtype`.

v4's Phase 0 (drain-poll), Phase 1 (native clipboard + `ClipboardPaste`), Phases 2–4 (portal GlobalShortcuts), Phase 5 (wizard), Phase 6 (directive doctor) all carry through with small surface edits noted below.

## Objective

A user on stock Ubuntu 24.04 GNOME-Wayland (or KDE Plasma 6, Hyprland, sway) installs Fono, launches it, approves **two consent dialogs in one wizard step** (GlobalShortcuts + RemoteDesktop), and from then on:

- Presses F7 → speaks → text auto-pastes at the cursor via synthetic Shift+Insert.
- Zero extra system packages installed.
- Zero recurring dialogs.

If the user declines the RemoteDesktop dialog: Fono still works via clipboard + user-driven Shift+Insert / Ctrl+V (v4 Phase 1 path). No degraded mode requires installing anything.

## Initial Assessment

### Cited project facts (verified or re-verified for v5)

- `crates/fono-inject/src/inject.rs:11-27` — `Injector` enum already has `Wtype`, `Ydotool`, `Xdotool`, `XtestPaste`, `None` variants. v5 adds `PortalRemoteDesktop` and `ClipboardPaste`.
- `crates/fono-inject/src/inject.rs:63-68` — current `Wtype` selection has no compositor-protocol probe. Bug fixed in v5.
- `crates/fono-inject/src/inject.rs:241-245` — current subprocess clipboard path. v4 Phase 1 replaces with native; v5 unchanged from v4.
- `crates/fono-hotkey/src/listener.rs:36, :289-329` — long-press translator. Replicated in portal hotkey backend (Phase 3, carried).
- `crates/fono/src/daemon.rs:562-581, :777-781` — FSM-driven cancel grab. Unchanged.
- `crates/fono-stt/src/registry.rs:576` — confirms `base` is not in registry; v4 Phase 6 directive doctor message stands.
- `ashpd 0.9` exposes `ashpd::desktop::remote_desktop::RemoteDesktop`, `Session`, `DeviceType::Keyboard`, and `notify_keyboard_keycode` per upstream docs. Already added in v4 Phase 3 for GlobalShortcuts; no new crate dep.

### Identified risks (ranked)

1. **RemoteDesktop session token portability across daemon restarts.** Persisted token + `Restore` call must succeed without re-prompting on every launch. If the backend drops sessions on D-Bus client disconnect (some implementations do), the daemon must `Restore` from the persisted token on startup; if `Restore` fails, fall back to `ClipboardPaste` *silently* and surface a doctor row suggesting `fono setup` to re-grant.
2. **Synthesising Shift+Insert into Xwayland windows.** Some compositors route portal-synthesised keys to native Wayland clients only; Xwayland-resident apps (older Electron, some IDEs) may not receive them. Mitigation: doctor `--verify-paste` round-trip + fallback to `ClipboardPaste` if the test phrase isn't echoed by the focused window.
3. **Two portal dialogs in immediate succession.** The wizard text must prepare the user; sequencing matters (GlobalShortcuts first, then RemoteDesktop after the first dialog returns) to avoid one dialog overlaying the other.
4. **`Injector::detect()` probing the virtual-keyboard protocol** requires a transient Wayland registry connection. Cheap (≤ 10 ms) but blocks startup if the connection hangs; wrap in a 250 ms timeout.
5. **Drain-poll wedge** — unchanged, 60 s timeout warn.
6. **`ashpd` license/version churn** — unchanged from v4.
7. **GNOME `Restore` semantics quirks.** GNOME 46's RemoteDesktop sessions may require user re-confirmation on `Restore` for security; verify against the live target box in Phase 10.
8. **Permission cache reliability for GlobalShortcuts** — unchanged, `wayland_cancel_strategy = "auto"` heuristic (one of the few config knobs that survives — diagnostic, not user-facing).

## Implementation Plan

### Phase 0 — Drain-poll fix *(unchanged from v4; ships first, independently)*

- [ ] **Task 0.1.** Implement cooperative drain-poll in `crates/fono/src/assistant.rs:327-349`.
- [ ] **Task 0.2.** Unit tests: idle-on-first-poll, notify-mid-loop, eventual-idle, timeout-warn.
- [ ] **Task 0.3.** Integration test in `crates/fono/tests/pipeline.rs`.

### Phase 1 — Native clipboard + `ClipboardPaste` injector *(unchanged from v4; the no-permissions baseline)*

- [ ] **Task 1.1.** Add `arboard = { version = "3", default-features = false, features = ["wayland-data-control"] }` to `crates/fono-inject/Cargo.toml`. Update `deny.toml`.
- [ ] **Task 1.2.** Replace subprocess clipboard at `crates/fono-inject/src/inject.rs:241-245` with native `Clipboard::new()?.set_text(text)`. Subprocess kept as defensive secondary fallback.
- [ ] **Task 1.3.** New `Injector::ClipboardPaste` variant — native clipboard set + transient overlay/tray toast (*"Text on clipboard — press Ctrl+V or Shift+Insert"*).
- [ ] **Task 1.4.** Tests: clipboard round-trip smoke; detect-returns-ClipboardPaste-on-Wayland-without-tools; toast-emitted-on-injection.

### Phase 1.5 — `PortalRemoteDesktop` injector *(new in v5)*

- [ ] **Task 1.5.1.** New variant `Injector::PortalRemoteDesktop` in `crates/fono-inject/src/inject.rs:9-27`. Holds (via the `Inject` dispatcher state, not the enum itself which stays `Copy`) a handle to a long-lived `RemoteDesktopSession` owned by a tokio task.
- [ ] **Task 1.5.2.** New module `crates/fono-inject/src/portal_remote_desktop.rs`:
  - `pub async fn open_session(restore_token: Option<&str>) -> Result<RemoteDesktopHandle>` — calls `RemoteDesktop::create_session`, `select_devices(DeviceType::Keyboard)`, `start()`. On success returns a handle that exposes:
    - `restore_token() -> Option<String>` — persisted to `~/.local/state/fono/remote-desktop-session.toml`.
    - `paste(&self) -> Result<()>` — synthesises **Shift+Insert** via `notify_keyboard_keycode(KEY_LEFTSHIFT, true)` → `notify_keyboard_keycode(KEY_INSERT, true)` → release both. (Linux keycode constants from `input-event-codes.h`; vendored as inline `pub const KEY_LEFTSHIFT: u32 = 42;` etc. to avoid a `libc`-only crate dep.)
  - `pub async fn restore_session(token: &str) -> Result<RemoteDesktopHandle>` — calls `Restore`; on backend rejection returns an error tagged `RestoreRefused` so the caller knows to re-prompt or fall back.
- [ ] **Task 1.5.3.** `Inject::inject(text)` for `PortalRemoteDesktop`:
  1. Native clipboard set (Phase 1 path).
  2. Brief async wait (≤ 50 ms) for clipboard manager to capture (heuristic, not strictly necessary but reduces edge cases).
  3. `RemoteDesktopHandle::paste()` synthesises Shift+Insert.
  4. No user-facing toast — the keystroke is invisible by design.
- [ ] **Task 1.5.4.** Update `Injector::detect()` at `crates/fono-inject/src/inject.rs:31-91` with the new Wayland precedence:
  - **X11 path unchanged** — `Enigo` → `Xdotool` → `XtestPaste`.
  - **Wayland path (new precedence):**
    1. `PortalRemoteDesktop` — if a persisted token resumes successfully OR the wizard just granted a fresh session. This is the steady-state for users who approved the dialog.
    2. `Wtype` — **only if** the virtual-keyboard protocol probe (Task 1.5.5) returns true (KWin, wlroots).
    3. `Ydotool` — only if `ydotoold` socket is reachable (probe `/run/user/$UID/.ydotool_socket` or equivalent).
    4. `Xdotool` — XWayland fallback.
    5. `ClipboardPaste` — always-available baseline.
  - **Detection is a layered fall-through, not a static pick.** Each layer can fail at runtime (e.g. portal session expired) and the dispatcher walks to the next.
- [ ] **Task 1.5.5.** Virtual-keyboard-protocol probe — new helper `compositor_supports_virtual_keyboard() -> bool` in `crates/fono-inject/src/wayland_probe.rs`:
  - Opens a transient `wayland-client` connection.
  - Walks the registry; returns true iff `zwp_virtual_keyboard_manager_v1` is advertised.
  - 250 ms timeout. Result cached process-lifetime in an `OnceLock<bool>`.
  - Crate dep: `wayland-client = "0.31"`, behind a `linux-wayland` cfg gate. Tiny — only the protocol headers, no toolkit.
- [ ] **Task 1.5.6.** Token persistence — extend the state directory schema (already used for `portal-session.toml` in Phase 3) with `remote-desktop-session.toml`. Both files are mode-`0600`; ownership and write-via-tempfile-rename to avoid corruption on crash mid-write. No new state directory; both files live in `~/.local/state/fono/`.
- [ ] **Task 1.5.7.** Tests:
  - Mock `RemoteDesktopProxy` — assert the `paste()` keystroke sequence is exactly `[Shift down, Insert down, Insert up, Shift up]`.
  - `injector_detect_prefers_remote_desktop_when_token_present` — synthetic state with `remote-desktop-session.toml` present + mock backend accepts `Restore`.
  - `injector_detect_falls_back_to_clipboard_when_restore_refused` — same as above, mock backend refuses `Restore`.
  - `wtype_only_selected_when_virtual_keyboard_protocol_present` — env stub with WAYLAND_DISPLAY, mock probe returning false → `Wtype` is skipped; mock probe returning true → `Wtype` chosen.

### Phase 2 — Hotkey backend detection scaffold *(unchanged from v4 Phase 2)*

- [ ] **Task 2.1.** `HotkeyBackendChoice` enum + `[hotkeys].backend = "auto" | "portal" | "x11" | "disabled"` config. (Diagnostic, defaults to auto — kept because it's an existing field shape, not a new knob added for v5.)
- [ ] **Task 2.2.** `fono_hotkey::detect_backend(env)`.
- [ ] **Task 2.3.** `is_graphical_session()` relocation.

### Phase 3 — Portal GlobalShortcuts client *(unchanged from v4 Phase 3)*

- [ ] **Task 3.1.** `ashpd 0.9` (already added in Phase 1.5 effectively — same crate).
- [ ] **Task 3.2.** `crates/fono-hotkey/src/portal.rs` — `PrimarySession` + `CancelSession`.
- [ ] **Task 3.3.** `spawn_portal_listener` — first-run binds all three shortcuts in one approval window; subsequent `EnableCancel` reuses the cached approval.
- [ ] **Task 3.4.** `HotkeyControl` channel — same.
- [ ] **Task 3.5.** `wayland_cancel_strategy` knob (existing-shape diagnostic config; not new in v5).

### Phase 4 — Backend selection at daemon spawn *(unchanged from v4 Phase 4)*

- [ ] **Task 4.1.** `fono_hotkey::spawn` orchestrator.
- [ ] **Task 4.2.** Replace `crates/fono/src/daemon.rs:210-227` call site.
- [ ] **Task 4.3.** Daemon FSM-event handling unchanged.

### Phase 5 — First-run wizard with both portal dialogs *(refined from v4 — Option B)*

#### State detection (no sentinel files — unchanged from v4)

- [ ] **Task 5.1.** `is_first_run()` derives from: Wayland → `~/.local/state/fono/portal-session.toml` absent (regardless of `remote-desktop-session.toml`); X11 → `~/.config/fono/config.toml` absent + no model present.

#### Wizard flow (Option B — both permissions in one step)

- [ ] **Task 5.2.** Wizard steps:
  1. **Welcome** — one paragraph naming the **two** permissions the user is about to approve: *"Fono will ask your desktop for two permissions: (1) the F7 / F8 / Esc shortcuts, (2) the ability to paste at your cursor automatically. You'll see two consent dialogs in quick succession. Both are one-time."*
  2. **Bind hotkeys + grant auto-paste** — single wizard step that fires the two portal calls sequentially:
     - `BindShortcuts` (GlobalShortcuts) — await `Response`.
     - On success, immediately `CreateSession` + `SelectDevices(Keyboard)` + `Start()` (RemoteDesktop) — await `Response`.
     - Both dialogs visible to the user in close succession; second only opens after first resolves so they don't overlap.
     - **On RemoteDesktop denial: silently downgrade to `ClipboardPaste`** for this user. No error UI; the next wizard step adapts its messaging.
  3. **Verify hotkey** — "Press F7 now to test." Live indicator turns green.
  4. **Pick STT** — default `tiny`; download; spinner.
  5. **Verify mic** — 1 s record + level meter; directive on no devices.
  6. **Verify injection** — speaks a test phrase; injects via the resolved injector. Result line shows exactly one of:
     - *"Pasted at cursor (auto-paste enabled)"* — `PortalRemoteDesktop` succeeded.
     - *"On clipboard — press Ctrl+V to paste"* — `ClipboardPaste` fallback (RemoteDesktop denied or unavailable). The wizard prints a one-line note: *"To enable auto-paste later, re-run `fono setup`."*
     - *"Typed via wtype"* / *"Typed via xdotool"* / etc. — non-portal paths on appropriate compositors.
  7. **Done.**
- [ ] **Task 5.3.** `fono setup` subcommand — runs the wizard. Idempotent (no-op if already configured; explicit `fono setup --reconfigure` forces re-prompt of both portal sessions).
- [ ] **Task 5.4.** Daemon auto-launches wizard when `is_first_run()` is true and a graphical session is detected. Suppressed on headless/SSH (offer CLI `fono setup` instead via stderr hint).

### Phase 6 — Directive doctor *(unchanged from v4 Phase 6, with two added rows)*

- [ ] **Task 6.1.** Rewrite `fono doctor` output as `OK` / `WARN` / `FAIL` rows with copy-pasteable commands.
- [ ] **Task 6.2.** Distro-aware install hints from `/etc/os-release`.
- [ ] **Task 6.3.** Specific message rewrites:
  - Injection — see updated 6.3 below.
  - Audio inputs none — directive linking to troubleshooting.
  - Stale `model = "base"` — directive to run `fono use stt local tiny`.
- [ ] **Task 6.3.5 (v5 additions).** Two new doctor rows:
  - **Auto-paste** — one of:
    - `OK Auto-paste: enabled via RemoteDesktop portal`
    - `WARN Auto-paste: disabled. Run 'fono setup --reconfigure' and approve the keyboard-emulation prompt to enable.`
    - `OK Auto-paste: native (XTEST)` — X11 path.
    - `OK Auto-paste: wtype (virtual keyboard)` — wlroots / KWin path.
  - **Wtype health** — only printed when `wtype` is on PATH:
    - `OK Wtype: compositor supports zwp_virtual_keyboard_v1`
    - `WARN Wtype: installed but compositor does not implement zwp_virtual_keyboard_v1 (e.g. GNOME-Wayland). Fono will not use wtype here — auto-paste via RemoteDesktop portal is recommended.`
- [ ] **Task 6.4.** Optional `fono doctor --fix` (deferred follow-up, unchanged from v4).
- [ ] **Task 6.5.** Default-config validity test (unchanged from v4).

### Phase 7 — UX wiring

- [ ] **Task 7.1.** Background the portal binding + RemoteDesktop session open so daemon startup is non-blocking. Bindings happen on a detached task; the daemon's main FSM is live throughout.
- [ ] **Task 7.2.** Tray menu — "Set up Fono…", "Run diagnostics…", "Reconfigure hotkeys & auto-paste…" (last one runs `fono setup --reconfigure`).
- [ ] **Task 7.3.** `Stage::HotkeyBinding` and `Stage::RemoteDesktopGrant` variants on `fono_core::critical_notify::Stage`. Notify only on failure of the **hotkey** binding; auto-paste denial is silent (clipboard mode is the graceful fallback).
- [ ] **Task 7.4.** When `Injector::ClipboardPaste` runs, transient on-screen toast (~2 s) — same as v4. When `PortalRemoteDesktop` runs, no toast.

### Phase 8 — Documentation and ADRs

- [ ] **Task 8.1.** ADR `docs/decisions/0028-wayland-onboarding.md`. Sections: portal as primary primitive; rejected alternatives (in-process suppression, install-time pre-bindings, sentinel files, ydotool default, `wtype` on GNOME); native clipboard as no-permissions baseline; RemoteDesktop portal as the auto-paste mechanism; **Option B (both portal dialogs at first run)** and the rationale (one wizard moment + one mental model + no deferred opt-in surface); decision to not add user-visible config knobs for these defaults.
- [ ] **Task 8.2.** Rewrite `docs/troubleshooting.md:82-100`. Two consent dialogs at first run; both persistent; clipboard fallback if auto-paste denied.
- [ ] **Task 8.3.** README — "Fono works on Wayland out of the box. Approve two prompts at first launch (hotkeys + auto-paste). No extra packages required."
- [ ] **Task 8.4.** `docs/providers.md` — restructure "system tools" section. `wl-clipboard` / `xclip` / `xsel` move from required to "diagnostic only, not used by Fono itself." `wtype` moves from "required on Wayland" to "alternative auto-paste backend, only useful on wlroots/KWin where the virtual-keyboard protocol is implemented."
- [ ] **Task 8.5.** `ROADMAP.md` — move items to Shipped at tag time.
- [ ] **Task 8.6.** `CHANGELOG.md` `[Unreleased]` Added / Changed / Fixed entries.

### Phase 9 — Tests

- [ ] **Task 9.1.** `detect.rs` unit tests (Phase 2).
- [ ] **Task 9.2.** Portal GlobalShortcuts translator tests (Phase 3).
- [ ] **Task 9.3.** Mock GlobalShortcuts proxy — single-dialog three-shortcut bind.
- [ ] **Task 9.4.** Drain-poll tests (Phase 0).
- [ ] **Task 9.5.** Native clipboard tests (Phase 1).
- [ ] **Task 9.6.** RemoteDesktop session tests (Phase 1.5) — see 1.5.7.
- [ ] **Task 9.7.** Wizard state-derivation tests (Phase 5).
- [ ] **Task 9.8.** Wizard sequencing test — synthetic portal mocks for both GlobalShortcuts and RemoteDesktop; assert the second dialog is opened only after the first `Response` arrives.
- [ ] **Task 9.9.** Doctor snapshot tests (Phase 6) — including the new auto-paste and wtype-health rows.
- [ ] **Task 9.10.** Default-config validity test.
- [ ] **Task 9.11.** Pre-commit gate per AGENTS.md.

### Phase 10 — Smoke testing on real desktops

- [ ] **Task 10.1.** ThinkPad `192.168.0.112` (Ubuntu 24.04 GNOME-Wayland). Acceptance gates:
  - `apt install fono` then launch → wizard opens.
  - **Two portal dialogs**, in sequence: GlobalShortcuts (F7/F8/Esc), then RemoteDesktop (keyboard emulation).
  - Press F7 → wizard verify step turns green.
  - `tiny` model downloads; mic verifies (or directive on failure).
  - Test phrase → wizard says *"Pasted at cursor (auto-paste enabled)"* and the test phrase appears in the wizard's text field via synthetic Shift+Insert.
  - Open `gedit`, press F7, speak, **text appears at cursor without pressing any key**.
  - Daemon restart → zero dialogs; `Restore` resumes both portal sessions.
  - Deny-path: re-run `fono setup --reconfigure`, deny RemoteDesktop → wizard verify step says *"On clipboard — press Ctrl+V to paste"*, gedit test confirms manual paste works.
- [ ] **Task 10.2.** KDE Plasma 6 Wayland: same matrix. Verify RemoteDesktop dialog wording is acceptable to the user. Verify `wtype` health row reports OK (KWin implements the protocol).
- [ ] **Task 10.3.** Hyprland: same matrix. Verify RemoteDesktop and `wtype` health both OK.
- [ ] **Task 10.4.** sway with `xdg-desktop-portal-wlr`: confirm `wayland_cancel_strategy = "auto"` heuristic on the GlobalShortcuts cache; confirm RemoteDesktop works (or surface a directive if portal-wlr doesn't implement it).
- [ ] **Task 10.5.** GNOME-X11 regression — X11 injector path (Enigo / Xdotool / XtestPaste) unchanged.
- [ ] **Task 10.6.** Stale-config retest — set `model = "base"` manually, restart, verify doctor wording.
- [ ] **Task 10.7.** `Injector::detect()` Wtype-on-GNOME false-positive — install `wtype` on the GNOME box, verify Fono **does not** select it (probe correctly returns false), doctor surfaces the `WARN Wtype: installed but compositor does not implement…` row.

## Verification Criteria

- **Fresh-install Ubuntu 24.04 GNOME-Wayland, no extra packages, accept both dialogs**: install → launch → wizard → two dialogs → press F7 → speak → text appears at cursor automatically. End-to-end auto-paste.
- **Same flow, deny the RemoteDesktop dialog**: wizard adapts; clipboard + Ctrl+V flow still works end-to-end. Doctor surfaces an actionable directive to enable auto-paste later.
- **Daemon restart**: both portal sessions restore from persisted tokens; zero dialogs.
- **Esc cancels TTS during audio drain** (Phase 0 + portal CancelSession).
- **Bare-Esc free for other apps while Fono is idle.**
- **`wtype` on GNOME-Wayland is no longer falsely selected** — virtual-keyboard-protocol probe gates the choice; doctor surfaces a WARN row instead of failing silently.
- **No new user-visible config keys** added by Phase 1.5 or Phase 5. (Diagnostic-tier `[hotkeys].backend` and `[hotkeys].wayland_cancel_strategy` already existed from v3/v4; v5 adds none.)
- **No sentinel files** beyond real artifacts (`portal-session.toml`, `remote-desktop-session.toml`, `config.toml`, downloaded models).
- AGENTS.md pre-commit gate green.

## Potential Risks and Mitigations

1. **RemoteDesktop `Restore` rejected on every launch** on a given backend. Mitigation: fall back to `ClipboardPaste` silently; doctor row directs the user to `fono setup --reconfigure`. Telemetry-free: never auto-re-prompt without user-initiated `--reconfigure`.
2. **Two consent dialogs in immediate succession feel heavy** to the user. Mitigation: wizard welcome paragraph names both upfront so the second dialog is expected; sequenced (not concurrent) so they don't overlap.
3. **Shift+Insert ignored by some Xwayland apps.** Mitigation: wizard verify step echoes back the test phrase; if it fails, surface a directive ("auto-paste worked at the portal level but the focused app didn't receive it — try Ctrl+V mode via `fono setup --reconfigure` and decline auto-paste").
4. **`wayland-client` direct probe is fragile on some compositors.** Mitigation: 250 ms timeout + cached result; on timeout assume `false` (skip `Wtype`) — safer to over-select `ClipboardPaste` / `PortalRemoteDesktop` than to under-deliver via dead `wtype`.
5. **Drain-poll wedge** — unchanged.
6. **Tokens at `~/.local/state/fono/*.toml` leaked via misconfigured backups.** Mitigation: mode-0600 on write; document.
7. **`ashpd` 0.9 API churn for RemoteDesktop.** Mitigation: behind a thin internal trait; can swap to a hand-rolled `zbus::proxy` block locally.

## Alternative Approaches

1. **Option A (lazy opt-in for auto-paste)** — rejected per user feedback in favour of Option B's single-moment ask.
2. **Ship a `fono-paste-helper` setuid binary** that uses `/dev/uinput` directly. Rejected — setuid expansion is a packaging and security risk for marginal benefit over the portal.
3. **`ydotool` with a Fono-managed user-level daemon** that activates `/dev/uinput`. Rejected — requires kernel-level perms or a one-time root setup the user must do; not "just apt install".
4. **Make `[hotkeys].paste_keystroke = "shift+insert" | "ctrl+v"` configurable.** Rejected per user feedback ("don't add new config options"). Hard-coded to Shift+Insert — Linux convention, works in every text input.
5. **Single combined portal dialog** showing both GlobalShortcuts and RemoteDesktop in one prompt. Spec doesn't allow it; the two interfaces are independent and the desktop shells render their own dialogs.
