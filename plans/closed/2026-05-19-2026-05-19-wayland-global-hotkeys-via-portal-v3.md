# Native Wayland global hotkeys — v3

## Status: Completed

Supersedes v2. Two simplifications from user feedback (2026-05-19):

1. **Drop the overlay/FSM decoupling.** The user is happy to see the overlay briefly through the audio drain; the only real bug is that `ProcessingDone` fires before audio drains, releasing the cancel grab too early. The drain-poll alone fixes this; no separate overlay-hide-early path is needed.
2. **Add a sudo-install-time fallback for the trigger keys** (F7/F8), via DE-native compositor bindings written by the packager at install time. This does *not* replace the portal — it complements it for older desktops without portal v1 and reduces the first-run dialog from "two shortcuts" to "one shortcut" (cancel only) on GNOME/KDE when present.

The portal `CancelSession` design from v2 stays intact; bare-Esc must never be system-wide grabbed, so it cannot be pre-bound at install time.

## Objective

F7 / F8 / Esc work as true global hotkeys on every Wayland desktop Fono targets (GNOME-Wayland 46+, KDE Plasma 5.27+/6, Hyprland, sway / wlroots) and on older Wayland desktops without the GlobalShortcuts portal, while:

- Showing **zero or one** portal dialog at first run on stock Ubuntu/Fedora/openSUSE GNOME-Wayland (one if portal is present; zero on older desktops where compositor-native bindings already cover F7/F8 and Esc-cancel is the only thing needing the portal).
- Showing **zero** portal dialogs on daemon restart or per-session.
- Never grabbing bare Esc system-wide while Fono is idle.
- Cancelling TTS playback for as long as audio is audible (drain-poll fix), even after the FSM-visible state suggests the assistant is "done".

X11 / Xwayland / headless paths stay on the existing `global-hotkey` listener unchanged.

## Initial Assessment

### Why sudo install cannot bypass the portal directly

The `xdg-desktop-portal.GlobalShortcuts` interface stores approvals in **per-user** stores (`~/.config/dconf/user` for GNOME, `~/.config/kglobalshortcutsrc` + `~/.local/share/xdg-desktop-portal/permissions.db` for KDE, similar for Hyprland and wlr). These do not exist at `apt install` / SlackBuild time — the user may not even be logged in. There is no D-Bus method, polkit action, or system-wide override that pre-seeds portal permissions; that's by design (the portal exists *because* the compositor refused to give clients silent global-key access). Verified against `xdg-desktop-portal-gnome` (Mutter backend, GNOME 46) and `xdg-desktop-portal-kde` (Plasma 6) upstream sources.

### Where sudo *can* help: DE-native compositor bindings for F7/F8 only

| DE | Mechanism | File written by `postinst` / SlackBuild | Stable? |
|---|---|---|---|
| GNOME / Cinnamon / Budgie / Unity | GSettings schema override | `/usr/share/glib-2.0/schemas/90_fono.gschema.override`, recompiled via `glib-compile-schemas` | Stable since GNOME 3.0 |
| KDE Plasma 5.27+ / 6 | System defaults merge | `/etc/xdg/kglobalshortcutsrc` (kdedefaults layer) | Stable since KDE Frameworks 5.20 |
| Hyprland / sway / wlroots | First-run helper | `/etc/xdg/autostart/fono-firstrun.desktop` → `fono firstrun --apply-compositor-bindings` runs once in the user's session, writes `~/.config/hypr/fono.conf` etc. | DE-specific; per-user runtime, not install time |
| Older GNOME (≤ 45) / KDE (≤ 5.26) | Same as their modern equivalents | Same | Yes |

**The Esc cancel binding cannot be installed this way.** A compositor-level binding on bare Esc would be permanent and system-wide; the only acceptable mechanism for dynamic Esc grab is the portal's GlobalShortcuts (v2) plus the `wayland_cancel_strategy` fallback for portal-less desktops (where the user accepts no Esc cancel, or opts into Super+Escape).

### Project structure summary (cited)

(Unchanged from v2; restating only the touched paths for handoff clarity.)

- `crates/fono-hotkey/src/listener.rs:36, :289-329` — `LONG_PRESS_THRESHOLD` + `map_event` translator. Replicated in the portal backend.
- `crates/fono-hotkey/src/listener.rs:54-75` — `HotkeyControl` channel. Same shape on both backends.
- `crates/fono/src/daemon.rs:562-581` — FSM-event-driven Esc-grab arm/disarm. Unchanged.
- `crates/fono/src/daemon.rs:777-781` — belt-and-braces disarm on transition-to-Idle. Unchanged; with the drain-poll, this now fires at the correct moment.
- `crates/fono/src/session.rs:1610-1620` — assistant cleanup closure. **Unchanged in v3** (was modified in v2; reverted).
- `crates/fono/src/assistant.rs:327-349` — the deferred drain-wait TODO. v3 implements it.
- `crates/fono-core/src/config.rs:189-218` — `[hotkeys]` config table; adds `backend = "auto"` and `wayland_cancel_strategy`.
- `packaging/` (Debian + SlackBuild) — new schema override + KDE fragment installed at `postinst`.

### Identified risks (ranked)

1. **Permission cache reliability across portal backends.** Same as v2; mitigated by `wayland_cancel_strategy = "auto"` heuristic and the install-time compositor-native fallback (which makes the user's first-run dialog cost drop from "two shortcuts" to "one shortcut" on portal-having DEs, and to zero on portal-less DEs that accept Esc-less cancel).
2. **Drain-poll deadlock.** Same as v2; mitigated by 60 s timeout warning + `notify` arm.
3. **Schema override clashes with user's pre-existing custom keybinding.** Mitigation: write the override only when the override file doesn't already exist and the schema path is empty in user dconf (preserve user intent). Document opt-out via `fono firstrun --remove-compositor-bindings`.
4. **KDE `/etc/xdg/kglobalshortcutsrc` format drift between Plasma 5 and 6.** Mitigation: ship two fragment files; postinst picks by detecting installed `kglobalaccel5` / `kglobalaccel6` binary; degrade to "no fragment" if neither is present.
5. **Hyprland / sway compositor-config write at first-run helper time may conflict with the user's hand-written config.** Mitigation: append to a Fono-owned include file (`~/.config/hypr/fono.conf`) and emit a one-line `source = ` directive in the main config only if not already present; print a notice on first run and link to the troubleshooting page. Reversible via `fono firstrun --remove-compositor-bindings`.
6. **User installs Fono from source (no postinst).** The portal path still works without the install-time bindings; degradation is graceful — one extra dialog at first run on portal-having desktops, no F7/F8 on portal-less desktops until the user runs `fono firstrun` manually.
7. **`ashpd` dep audit / churn.** Unchanged from v2; MIT, pin `0.9.x`.

## Implementation Plan

### Phase 0 — Drain-poll fix *(land first, independently shippable)*

This is now a tiny phase (one task + tests). Overlay-decoupling work from v2 is dropped per user feedback.

- [ ] **Task 0.1.** In `crates/fono/src/assistant.rs:327-349`, replace the early `Ok(any_audio)` return with the cooperative drain-poll the existing TODO describes:
  - Acquire `state.lock().await.playback.clone()`.
  - `loop { tokio::select! { biased; () = notify.notified() => break; () = tokio::time::sleep(Duration::from_millis(100)) => { if playback.as_ref().is_none_or(|p| p.is_idle()) { break; } } } }`.
  - 60 s belt-and-braces timeout that breaks with a `tracing::warn!("drain-poll exceeded 60 s; forcing ProcessingDone")` so a wedged playback handle can't soft-lock the FSM.
- [ ] **Task 0.2.** Unit tests in `crates/fono/src/assistant.rs`:
  - `drain_poll_exits_when_playback_idle` — stub playback returning `is_idle() = true` on first poll; assert immediate return.
  - `drain_poll_exits_on_notify` — `notify.notify_one()` mid-loop; assert prompt return.
  - `drain_poll_loops_until_audio_done` — stub that returns false for N polls then true; assert correct timing.
  - `drain_poll_timeout` — stub that returns false forever; assert the 60 s timeout fires with the warn log.
- [ ] **Task 0.3.** Integration test in `crates/fono/tests/pipeline.rs`:
  - Assistant turn with synthetic LLM + TTS stubs that produce 3 s of audio.
  - Assert `ProcessingDone` is emitted **after** `playback.is_idle()` first returns true.
  - Esc during the drain window → `on_assistant_stop` called → playback aborted → `ProcessingDone` emitted promptly.
- [ ] **Task 0.4.** No ADR. The drain-poll is a pure bug fix; the TODO comment at `assistant.rs:327-349` already documents the design.

### Phase 1 — Backend detection scaffold *(unchanged from v2)*

- [ ] **Task 1.1.** `HotkeyBackendChoice` enum + `[hotkeys].backend` config field (default `"auto"`).
- [ ] **Task 1.2.** `detect_backend(env: &impl EnvProvider) -> HotkeyBackend` in new `crates/fono-hotkey/src/detect.rs`.
- [ ] **Task 1.3.** Move `is_graphical_session()` into `fono_hotkey::detect`; re-export from `crates/fono/src/lib.rs`.

### Phase 2 — Portal client with two-session architecture *(carried from v2)*

- [ ] **Task 2.1.** Add `ashpd = { version = "0.9", default-features = false, features = ["tokio"] }` Linux-only. Update `deny.toml`.
- [ ] **Task 2.2.** `crates/fono-hotkey/src/portal.rs` — `PrimarySession` (persistent, dictation + assistant) + `CancelSession` (transient, cancel only). Session token persisted to `~/.local/state/fono/portal-session.toml`.
- [ ] **Task 2.3.** `async fn spawn_portal_listener` — introspect portal version; resume primary session or `CreateSession` + `BindShortcuts`; on first run only, also open and immediately close a throw-away cancel session inside the same approval window so the backend caches the cancel binding's approval. **Skip the throw-away open if compositor-native bindings for F7/F8 are already in place** (Phase 3 sets a sentinel file at `~/.local/state/fono/compositor-bindings.toml`); in that case the portal session binds *only* cancel, dropping the first-run dialog count to one shortcut.
- [ ] **Task 2.4.** `HotkeyControl` channel — `EnableCancel` opens a fresh `CancelSession` on the fly (cache-approved, no dialog); `DisableCancel` drops it (Drop impl calls `Session.Close()`).
- [ ] **Task 2.5.** `[hotkeys].wayland_cancel_strategy = "auto" | "dynamic" | "persistent" | "non-bare"` with 800 ms auto-fallback heuristic.

### Phase 3 — DE-native compositor bindings via the installer *(new in v3)*

#### 3a. Package-time install (sudo-context)

- [ ] **Task 3.1.** Author `packaging/share/gsettings/90_fono.gschema.override` containing a custom-keybinding entry that binds `F7` → `/usr/bin/fono toggle` and `F8` → `/usr/bin/fono assistant`. Document that it sets *defaults*, not enforced values — users can rebind via gnome-control-center as normal.
- [ ] **Task 3.2.** Author `packaging/share/kde/kglobalshortcutsrc.fragment` (Plasma 6 variant) and `packaging/share/kde/kglobalshortcutsrc5.fragment` (Plasma 5 variant). Each binds the same two key codes to a `D-Bus org.fono.Fono.Toggle/Assistant` invocation routed through `dbus-send` (no portal). Install both; postinst chooses based on which `kglobalaccel{5,6}` binary is present.
- [ ] **Task 3.3.** Debian packaging — `debian/fono.postinst` step: install `/usr/share/glib-2.0/schemas/90_fono.gschema.override` from the staged source, run `glib-compile-schemas /usr/share/glib-2.0/schemas/`. Append the appropriate KDE fragment to `/etc/xdg/kglobalshortcutsrc` (with a Fono-owned begin/end marker so `prerm` can excise it cleanly). Install `/etc/xdg/autostart/fono-firstrun.desktop` (see 3.5).
- [ ] **Task 3.4.** SlackBuild parity — `packaging/SlackBuild/fono.SlackBuild` installs the same three files; `doinst.sh` runs `glib-compile-schemas`.

#### 3b. Per-user first-run helper (Hyprland / sway / wlroots, no system-wide pre-bind path)

- [ ] **Task 3.5.** New subcommand `fono firstrun --apply-compositor-bindings` / `--remove-compositor-bindings`. Logic:
  1. Detect compositor via `XDG_CURRENT_DESKTOP`, fall back to env hints (`HYPRLAND_INSTANCE_SIGNATURE`, `SWAYSOCK`).
  2. If GNOME / KDE — no-op (system-wide schema override already covers it).
  3. If Hyprland — append `bind = ,F7, exec, /usr/bin/fono toggle` + same for F8 to `~/.config/hypr/fono.conf`; ensure `~/.config/hypr/hyprland.conf` has `source = ~/.config/hypr/fono.conf` (insert if missing, preserving user content).
  4. If sway — append the equivalent `bindsym F7 exec /usr/bin/fono toggle` to `~/.config/sway/config.d/fono.conf`; ensure the main config includes `config.d/*`.
  5. Else — print "no known compositor-native binding path; relying on portal" and exit success.
  6. Write a sentinel marker `~/.local/state/fono/compositor-bindings.toml` recording which bindings are now in place + their target keys. Phase 2 Task 2.3 reads this to decide whether to bind trigger keys via the portal.
- [ ] **Task 3.6.** Autostart desktop file `/etc/xdg/autostart/fono-firstrun.desktop` — `Exec=/usr/bin/fono firstrun --apply-compositor-bindings --idempotent`. The `--idempotent` flag skips work if the sentinel already exists or if it can't safely modify the user's config.
- [ ] **Task 3.7.** `fono firstrun --remove-compositor-bindings` deletes the Fono-owned fragments + the sentinel. Debian `prerm` invokes the system-wide equivalent via `glib-compile-schemas` and KDE fragment excision.
- [ ] **Task 3.8.** Doctor reports: which of (gsettings schema override, KDE fragment, Hyprland include, sway include) is active for the current session, and the resolved triggers as observed by each layer.

### Phase 4 — Backend selection at daemon spawn *(unchanged from v2)*

- [ ] **Task 4.1.** `fono_hotkey::spawn(backend_choice, bindings, action_tx)`.
- [ ] **Task 4.2.** Replace call site at `crates/fono/src/daemon.rs:210-227`.
- [ ] **Task 4.3.** Daemon FSM-event/Idle-transition code at `crates/fono/src/daemon.rs:562-581` and `:777-781` unchanged.

### Phase 5 — UX wiring *(refined from v2)*

- [ ] **Task 5.1.** First-run wizard text: explain the *single* expected dialog ("Approve Fono's cancel shortcut once and you're done"); show different copy when the install-time bindings are detected vs. when only the portal is in play.
- [ ] **Task 5.2.** Background the portal binding so daemon startup is non-blocking.
- [ ] **Task 5.3.** `fono doctor` Hotkeys section:
  - Backend: `portal` / `x11` / `disabled`.
  - Portal version (if portal).
  - Compositor-native bindings: list each layer (`gsettings`, `kglobalshortcutsrc`, `hyprland`, `sway`) and which is active.
  - Resolved triggers from each layer.
  - `wayland_cancel_strategy` resolved value + last-bind RTT.
- [ ] **Task 5.4.** Tray "Hotkeys" submenu — read-only triggers + "Reconfigure hotkeys…" (calls portal `ConfigureShortcuts` if available, else re-creates session, else opens a tray notification linking to `fono firstrun --reconfigure`).
- [ ] **Task 5.5.** `Stage::HotkeyBinding` variant added to `fono_core::critical_notify::Stage`; user-visible notification only on failure.

### Phase 6 — Documentation and ADRs

- [ ] **Task 6.1.** ADR `docs/decisions/0028-wayland-global-hotkeys.md`. Sections: portal as primary primitive; rejected alternatives (in-process suppression, libei InputCapture, per-compositor IPC); two-session architecture; `wayland_cancel_strategy` knob; install-time compositor-native bindings for F7/F8 *only*; explicit non-goal of bypassing portal consent.
- [ ] **Task 6.2.** Rewrite `docs/troubleshooting.md:82-100` Wayland section.
- [ ] **Task 6.3.** `README.md` updates: "Wayland hotkeys work natively; one approval dialog at first launch (or zero if installed from a Fono package on GNOME/KDE)."
- [ ] **Task 6.4.** `docs/providers.md` — add a "Wayland compositor bindings" subsection covering the install-time files and the `fono firstrun` helper.
- [ ] **Task 6.5.** `ROADMAP.md` — move items to **Shipped** at tag time.
- [ ] **Task 6.6.** `CHANGELOG.md` `[Unreleased]` Added/Changed/Fixed entries.

### Phase 7 — Tests

- [ ] **Task 7.1.** `detect.rs` unit tests — 5-cell env matrix + override.
- [ ] **Task 7.2.** Portal translator unit tests — match `listener.rs:289-329` byte-for-byte.
- [ ] **Task 7.3.** Mock `GlobalShortcutsProxy` — assert single-shortcut bind path when compositor-native sentinel exists; two-shortcut path when not.
- [ ] **Task 7.4.** Drain-poll tests (Tasks 0.2, 0.3) included.
- [ ] **Task 7.5.** `fono firstrun --apply-compositor-bindings` golden-output tests for Hyprland and sway (synthetic HOME); idempotency tests.
- [ ] **Task 7.6.** `wayland_cancel_strategy` migration test — slow synthetic backend → auto-flip to `persistent` + config write-back.
- [ ] **Task 7.7.** Pre-commit gate: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --tests --lib`.

### Phase 8 — Smoke testing on real desktops

- [ ] **Task 8.1.** Target ThinkPad `192.168.0.112` (Ubuntu 24.04 LTS, GNOME-Wayland). Install Fono `.deb`. Acceptance gates:
  - Schema override active → F7/F8 work zero-dialog from very first launch.
  - Portal dialog at first launch asks for **one** shortcut (Esc cancel) only.
  - Esc cancels recording / thinking / speaking — including during audio drain after the visible overlay/processing UI has finished.
  - Other-app bare-Esc check (nano insert mode) while Fono idle — Esc is free.
  - Daemon restart → zero dialogs.
- [ ] **Task 8.2.** KDE Plasma 6 Wayland: same matrix.
- [ ] **Task 8.3.** Hyprland: `fono firstrun` writes `~/.config/hypr/fono.conf`, F7/F8 fire via Hyprland binding (not portal); cancel via portal.
- [ ] **Task 8.4.** sway with `xdg-desktop-portal-wlr`: same as 8.3 + verify the `wayland_cancel_strategy = auto` heuristic correctly auto-falls-back to `persistent` if wlr re-prompts.
- [ ] **Task 8.5.** GNOME-X11 regression check.
- [ ] **Task 8.6.** Headless / SSH-only — graceful disable; `fono toggle` IPC still works.

## Verification Criteria

- **Zero dialogs at first launch on packaged Ubuntu/Fedora/openSUSE GNOME-Wayland** when the system-wide schema override is in place and the user has not pre-bound F7/F8; or **one dialog** (cancel only) when GNOME re-resolves the schema and the user accepts the override defaults.
- **One dialog at first launch on KDE Plasma 6 Wayland** when the KDE fragment is in place — for cancel only.
- **Zero dialogs on daemon restart** regardless of DE.
- **Zero dialogs on each EnableCancel** thanks to portal permission cache + 800 ms heuristic auto-fallback.
- **Esc cancels audio playback for as long as it's audible**, even after the overlay/tray have flipped to Idle (Task 0.1 drain-poll).
- **Bare Esc works in other apps while Fono is in Idle** — verified by nano-Esc check.
- **X11 path unchanged** — regression test 8.5.
- AGENTS.md pre-commit gate green.

## Potential Risks and Mitigations

1. **Schema override conflicts with the user's pre-existing custom-keybinding entries.**
   Mitigation: Task 3.3 installs the override at a high-precedence path but only sets *defaults*; users overriding via gnome-control-center retain precedence. `fono doctor` shows the resolved binding so a conflict is one command away.
2. **KDE fragment-merge surprises in the user's `~/.config/kglobalshortcutsrc`.**
   Mitigation: write to `/etc/xdg/kglobalshortcutsrc` (system layer), not the user file. KDE's kdedefaults merge correctly subordinates this to user choices.
3. **Hyprland / sway helper appends bindings the user doesn't want.**
   Mitigation: writes to a Fono-owned include file; user can `rm ~/.config/hypr/fono.conf` and remove the `source =` line, or invoke `fono firstrun --remove-compositor-bindings`. The autostart entry runs `--idempotent` so it never re-adds after removal (sentinel + explicit-removal marker file).
4. **Drain-poll wedge** — same as v2; 60 s timeout with warn log.
5. **`ashpd` dep churn / license.** Same as v2.
6. **Source builds get a downgraded UX** (no install-time bindings).
   Mitigation: `fono firstrun` is also callable manually; `make install` target in `packaging/Makefile` runs it post-install for source-build users. Document in README.

## Alternative Approaches

1. **Portal-only (v2 plan).** Workable but costs one extra first-run dialog on portal-having DEs and doesn't help portal-less DEs at all. v3's install-time path strictly improves on this.
2. **Compositor-only — bind everything at the compositor level including Esc.** Rejected: Esc system-wide grab is unacceptable per user requirement.
3. **Single-binary, no install-time files — write per-user compositor configs on first daemon launch.** Possible alternative to Task 3.5's autostart entry; declined because the portal already gives us a zero-config path on every modern desktop and the autostart approach is only the niche fallback for Hyprland/sway.
4. **Pre-seed portal permission stores by reverse-engineering their on-disk format.** Rejected: violates portal consent model; formats are not stable across backend versions; not maintainable.
5. **Use libei + InputCapture portal** for keystroke capture. Rejected: requires broad input-capture grant; wrong primitive for this use case.
