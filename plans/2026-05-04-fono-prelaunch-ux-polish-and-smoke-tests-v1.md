# Fono — Pre-Launch UX Polish + Smoke Test Suite

## Objective

Bring Fono to "Apple-style just works" quality before the v0.7.0 Public Beta tag:
- Defaults that match what 90% of users want, so the happy path requires zero
  config edits.
- A first-run wizard that asks 0–1 questions instead of 8–13.
- No silent failures: every error path either self-recovers or tells the user
  exactly what to do, in plain language.
- A reproducible smoke-test matrix exercising the new-user journey end-to-end,
  so we catch UX regressions before ship.

This plan is the prerequisite to Phase 1 of
`plans/2026-05-04-fono-public-launch-strategy-v1.md`. Land this, then tag
v0.7.0.

## Background — current state

The `sage` audit (cited inline below) confirmed Fono is technically mature
but UX-noisy: the first-run wizard asks 8–13 questions when 0–1 would
suffice; 16 of 24 `[interactive]` fields are exposed to first-time users
who never enabled live mode; 14 different "switch failed" toasts compete
for the same notification slot; and at least four user-facing flows fail
silently (degraded daemon, hotkey grab failure, slim+interactive
mismatch, sub-300 ms tap). All cite-line references in this plan are
from the audit; do not re-read those files unless the implementation
agent needs full context.

## High-level UX model — how users actually use Fono

### The three personas

1. **Casual desktop dictator (90% of users)** — installs Fono, presses
   F9, speaks, sees text. Touches a config file zero times in their
   lifetime. Will never read `docs/`. Uses the tray menu for everything.
2. **Power user (~9%)** — switches providers, edits hotkeys, enables
   live dictation, wires up an LLM cleanup model. Comfortable with
   `fono use`, `fono keys`, `~/.config/fono/config.toml`. Reads
   `docs/providers.md` once.
3. **Integrator (~1%)** — Home Assistant operator, MCP user, scripts
   over IPC, runs the systemd unit on a NAS. Lives in `docs/` and the
   GitHub Discussions tab.

The UX we ship must be optimised for persona #1 and **not** punish
them for personas #2/#3. Today every persona-#1 user is exposed to
persona-#2 prompts during first run.

### The end-to-end journey we promise

| Phase | What happens | What the user sees |
|---|---|---|
| **Discover** | User reads README / fono.page | Screencast + install one-liner + comparison table |
| **Install** | One command (distro pkg / curl-pipe / `sudo fono install`) | Installer prints "Run `fono` to start, press F9 to dictate" |
| **First run** | Daemon starts; tray icon appears; model downloads silently with a progress notification on the first key press if needed | "Fono is ready. Press F9 to dictate." (notification once) |
| **First dictation** | F9 → speak → release | Tray icon flashes red while recording, amber while processing; transcript appears at cursor; that's it |
| **First failure** | Anything that goes wrong (hotkey conflict, no audio, empty transcript, missing key) | One critical notification with one actionable next step (e.g. "Run `fono doctor` in a terminal") |
| **Customisation** | User wants something different | Right-click tray → submenu reveals every reasonable toggle |
| **Power use** | User edits config, switches providers, enables live | `fono use` / `fono keys` / `fono setup --advanced` |

The persona-#1 user never enters phases 6–7. Today they're forced into
phase 7 during first run.

## Implementation Plan

### Phase A — first-run wizard collapse (Apple-style "ask only what's necessary")

Source of truth for prompt-by-prompt audit: `crates/fono/src/wizard.rs`.

- [ ] **Task A1 — Auto-decide language from OS locale.** When
  `detect_os_languages()` returns exactly one BCP-47 code that is also
  in the English-only or non-English path, persist
  `general.languages = [<code>]` silently. Drop prompts Q2 and Q3
  entirely on the happy path. Preserve the multi-checkbox picker for
  `fono setup --advanced` and for the tray Languages submenu.
- [ ] **Task A2 — Auto-pick the recommended STT model.** The shortlist's
  position 0 is already the best fit. Replace the `Select` with a
  one-line `info!` "Picked Small (balanced) — 466 MB". Power users can
  pick a different model post-install via tray or
  `fono setup --advanced`. Keep the auto-pick fast path that already
  exists for single-option shortlists.
- [ ] **Task A3 — Honour the live-dictation `recommend` value.** Today
  the wizard computes `recommend`/`reason` then defaults to **No**
  regardless. Either trust `recommend` (set as `Select` default) or
  drop the prompt and persist
  `[interactive].enabled = recommend`. Recommendation: persist silently
  + surface a tray "Live dictation" toggle.
- [ ] **Task A4 — Drop the LLM cleanup 3-way prompt from first run.**
  Default Skip; expose tray "Clean up with AI" toggle. New user gets a
  working dictation experience without ever seeing the term "LLM".
- [ ] **Task A5 — Drop the assistant trio (Q8 + Q8a + Q8b) from first
  run.** Voice assistant is a separate product surface; expose via tray
  "Try the voice assistant" entry which then launches a focused mini-
  wizard for just the assistant fields.
- [ ] **Task A6 — Replace Q1a "show local anyway" with silent Cloud
  fallback.** On Unsuitable hardware, persist a sensible cloud default
  (Groq STT, Skip LLM) and fire one informational notification: "Your
  hardware is below the local-Whisper floor — using cloud Groq.
  Set GROQ_API_KEY via `fono keys add`."
- [ ] **Task A7 — Add `fono setup --advanced`** that exposes every
  prompt the simplified wizard hides, for the persona-#2 user who
  *wants* the full picker. Document in `docs/providers.md`.
- [ ] **Task A8 — End-state: simplified wizard asks at most 1 question.**
  *"Welcome to Fono. Press F9 to dictate, F8 to push-to-talk. Press
  Enter to continue."* On a Comfortable+ host with a recognised locale
  this is the only prompt. Cloud-only users (Unsuitable tier or
  explicit `--cloud`) still get the API-key paste prompt — that one
  is essential.

### Phase B — defaults flipped to "what 90% of users want"

Source: `crates/fono-core/src/config.rs`. Each change requires a
`CHANGELOG.md` entry under `Changed` and a config-migration test.

- [ ] **Task B1 — `overlay.waveform: true → false`.** First batch
  dictation pops a window without consent. Tray "Show waveform overlay"
  toggle exposes it back.
- [ ] **Task B2 — `audio.auto_stop_silence_ms: 0 → 1500`.** Toggle-mode
  user almost always wants this; the rare user who wants F9 to keep
  recording forever can set 0 explicitly.
- [ ] **Task B3 — Hotkey defaults audit.** F8/F9 collide with browser /
  OBS / IDE bindings (whole `troubleshooting.md` section). Investigate
  switching `hold`/`toggle` to `Pause`/`ScrollLock` or
  `Right Ctrl` / `Right Alt`-doubletap. Decision driven by a 1-day
  research spike: which keys are unbound on KDE/GNOME/sway/i3 default
  configs across the last 3 LTS versions of Ubuntu/Fedora/Debian.
  Document the choice in a new ADR.
- [ ] **Task B4 — `stt.local.model` first-run default.**
  Investigate making `base.en` the no-wizard fallback (~140 MB) for the
  non-TTY path; the TTY path already auto-picks via tier matching.
- [ ] **Task B5 — `[interactive]` schema cleanup.** Add
  `#[serde(skip_serializing_if = "Interactive::is_default")]` so a
  user with `enabled = false` does not see 24 streaming heuristics in
  their config. Remove the `eou_adaptive` and `resume_grace_ms` fields
  entirely until Slice D ships them; reserved-future fields in user-
  facing schemas always cause confusion.
- [ ] **Task B6 — `[server.wyoming]` schema cleanup.** Same treatment
  as `[interactive]`: skip-when-default. `[network]` already does this
  correctly.
- [ ] **Task B7 — Resolve `interactive.hold_release_grace_ms`
  doc-vs-default mismatch.** Doc-comment says 300 ms, struct default is
  150 ms. Pick one and update the other.
- [ ] **Task B8 — Filler / dangling word lists language-aware.** Today
  these are English-only. When `general.languages` excludes English,
  these heuristics fire on the wrong language. Either gate them on
  `languages.contains("en")` or load per-language word lists; minimum
  bar for the launch is gating, not full localisation.
- [ ] **Task B9 — Remove dead `general.startup_autostart` flag.** The
  desktop installer always writes the autostart `.desktop`; the flag
  is read nowhere effective. Confirm and delete to avoid user
  confusion.

### Phase C — silent-failure elimination

Every path the audit flagged. Each fix is a one-shot `notify::send`
call plus a tray-state change.

- [ ] **Task C1 — Degraded daemon notification.** When the orchestrator
  starts in degraded mode (`crates/fono/src/daemon.rs:122-130`), set
  the tray icon to a red error badge and queue a Critical notification
  on the next hotkey press: *"Speech-to-text failed to load. Run `fono
  doctor` for details."*
- [ ] **Task C2 — Hotkey grab failure notification.** At
  `crates/fono/src/daemon.rs:183-187`, on `BadAccess`-style failures,
  fire a Critical notification: *"Couldn't grab F9 — another app is
  using it. Edit `[hotkeys].toggle` in your Fono config or stop the
  conflicting app."* Use the existing X11 BadAccess handler (already
  shipped) as the trigger.
- [ ] **Task C3 — Sub-300 ms tap feedback.** At
  `crates/fono/src/session.rs:194-195` (`MIN_RECORDING`), fire a
  Low-urgency notification on the *first* short-tap of a session:
  *"Hold F9 a bit longer — Fono didn't catch any audio."* Throttled
  to once per 60 s to avoid spam.
- [ ] **Task C4 — Slim-build + interactive mismatch.** At
  `crates/fono/src/daemon.rs:1071`, instead of warning-then-silently-
  batching, fail loudly at startup: print a clear error and refuse to
  start until the user flips `[interactive].enabled = false` *or*
  installs the full build. Refusing to start beats silent fallback.
- [ ] **Task C5 — First-press model download progress.** At
  `crates/fono/src/daemon.rs:105`, reuse the existing
  download-begin/download-end notification surface
  (`daemon.rs:2071/2081`) for the initial preflight, not just tray-
  driven swaps. User pressing F9 with no model yet sees: *"Downloading
  Whisper Small (466 MB)… first dictation will be ready in a moment."*
- [ ] **Task C6 — Headless mode discoverability.** When
  `is_graphical_session()` is false, log at `info!` (not `debug!`) on
  startup: *"Fono running in headless mode — global hotkeys and tray
  disabled. Use `fono toggle` over IPC to dictate."* Power users who
  ssh in to a NAS need this signal.
- [ ] **Task C7 — Consolidate the 14 switch-failed toasts.** Replace
  the 14 inline `notify::send` calls in `daemon.rs` with one helper
  `notify_switch_failure(kind: SwitchKind, err: &dyn Error)` that
  produces uniform titles and bodies. Reduces maintenance burden and
  ensures consistent voice.
- [ ] **Task C8 — Refresh tray STT/LLM submenus on `Reload`.** The
  audit notes the menu snapshots at startup
  (`daemon.rs:322-323`); add a re-snapshot on `Request::Reload` so
  `fono keys add ANTHROPIC_API_KEY` immediately makes Anthropic
  appear in the LLM submenu.
- [ ] **Task C9 — Single-instance guard friendly message.** At
  `daemon.rs:88-92`, replace the technical "socket bind failed:
  Address in use" with: *"Fono is already running. Right-click the
  tray icon to use it, or run `fono toggle` to dictate."*

### Phase D — smoke test suite (the launch quality gate)

These tests will be the green-light gate for v0.7.0. Each test lives
in `crates/fono/tests/` or `tests/` and runs in CI.

- [ ] **Task D1 — `crates/fono/tests/first_run.rs`.** Drive the
  non-TTY default-config write path (`crates/fono/src/cli.rs:417`),
  start the daemon against a tempdir `XDG_CONFIG_HOME`, assert the
  written `config.toml` matches the documented default schema, assert
  the daemon stays up for ≥ 2 s (proxy for "didn't crash on missing
  model").
- [ ] **Task D2 — `tests/wizard_smoke.rs`.** Drive `wizard::run` with
  scripted `dialoguer` `Term` input (the all-Enter happy path); assert
  the resulting config matches the post-Phase-A simplified wizard
  output. Pin prompt order so refactors can't silently re-order the
  flow.
- [ ] **Task D3 — `tests/install_roundtrip.rs`.** Run `run_install`
  against a tempdir-rooted filesystem layout, then `run_uninstall`,
  assert zero leftover files (no marker, no autostart entry, no
  completions). Cover both desktop and `--server` modes.
- [ ] **Task D4 — `tests/install_partial_failure.rs`.** Inject a write
  failure mid-install (e.g. ENOSPC on icon write), assert
  `run_uninstall` still cleans up via the marker file's recorded
  prefix.
- [ ] **Task D5 — `tests/hotkey_ipc_fallback.rs`.** Simulate the KDE
  Wayland scenario (no global-hotkey backend). Assert that
  `fono toggle` over IPC successfully drives a recording session even
  when the in-process listener is disabled.
- [ ] **Task D6 — `tests/gpu_variant_filter.rs`.** Feed a captured
  `releases.json` payload (committed to `tests/fixtures/`) into
  `fono_update::check`; assert the variant-aware filter picks the
  correct asset for {CPU build / no GPU host}, {CPU build / GPU host},
  {GPU build / no GPU host}, {GPU build / GPU host}.
- [ ] **Task D7 — `tests/doctor_smoke.rs`.** Run `fono doctor` against a
  scripted environment fixture; pin the report shape (sections,
  ordering, presence of "Install" / "Compute backends" / "Audio
  inputs" / "Providers"). Catch reformatting regressions before they
  ship.
- [ ] **Task D8 — `tests/model_preflight.rs`.** Boot the daemon with
  a missing model file. Assert the download-begin notification fires
  on first hotkey press (Phase C5), and the download-end notification
  fires on completion. Use a tiny fixture model to keep test runtime
  bounded.
- [ ] **Task D9 — `tests/single_instance_guard.rs`.** Spawn two
  daemons against the same socket; assert the second exits with the
  Phase C9 friendly message and the first remains healthy.
- [ ] **Task D10 — `tests/empty_secrets_tray.rs`.** With an empty
  `secrets.toml`, assert the tray STT submenu still offers Local +
  active backend, and the LLM submenu still offers Skip + Local. No
  cloud entries appear without keys.
- [ ] **Task D11 — `tests/stale_update_cache.rs`.** Plant a cached
  `update.json` whose `current` field doesn't match
  `CARGO_PKG_VERSION`; assert `daemon.rs:215-223` evicts it and the
  tray doesn't briefly flash a stale "update available".
- [ ] **Task D12 — `tests/headless_daemon_boot.rs`.** Spawn the daemon
  with `DISPLAY` and `WAYLAND_DISPLAY` unset; assert it boots, logs
  the Phase C6 banner, refuses to register hotkeys / spawn tray, and
  the IPC socket still works for `fono toggle`.
- [ ] **Task D13 — `tests/inject_fallback_chain.rs`.** Mock a
  display-server scenario where `wtype` succeeds, where it fails (then
  `xtest-paste` succeeds), and where everything fails (clipboard-only
  fallback). Today every integration test uses a mock injector and
  bypasses the real chain.
- [ ] **Task D14 — `tests/check.sh --smoke` aggregator.** Add a new
  `--smoke` mode that runs only the D1-D13 tests plus the existing
  fmt/clippy/build matrix; this becomes the "green-before-tag" command.
  CI gates the v0.7.0 release on this passing.

### Phase E — manual QA checklist (the human smoke test)

Some failure modes can only be caught by a human pressing real keys on
real hardware. Document these in `docs/dev/release-qa.md` (already
exists for self-update; expand). Each row is checked manually before
tagging v0.7.0; subsequent releases re-run the subset that touches
modified surfaces.

- [ ] **Task E1 — Fresh user, fresh machine** (clean VM, no `~/.config/fono`):
  - Install via `.deb` / `.pkg.tar.zst` / `.txz` / `curl | sh`
  - Run `fono setup` → reach a working state in < 60 s including model
    download progress
  - Press F9 once; speak; verify text appears at cursor in: terminal,
    Firefox, VS Code, Slack/Element, GNOME Text Editor / Kate
  - Re-run with no `setup` (auto-default config) and verify F9 still
    works (after model preflight)
- [ ] **Task E2 — Fresh `sudo fono install`** (desktop mode):
  - Reboot
  - Verify autostart fired (tray icon present)
  - Press F9 — works
  - Run `sudo fono uninstall` — verify zero residue (`find /
    -name '*fono*' 2>/dev/null` shows only ~/.config/fono and the
    history DB)
- [ ] **Task E3 — `sudo fono install --server`** (headless box):
  - Verify `systemctl is-active fono` returns active
  - Verify install reports the Phase 0.6.1 journal-line dump on a
    forced failure (set `ExecStart` wrong, retry)
  - From another machine on the LAN, run `fono discover` and verify
    the server appears
- [ ] **Task E4 — Hotkey conflict path**:
  - Bind F9 to "Take screenshot" in KDE; start Fono; verify the Phase
    C2 notification fires; verify `[hotkeys].toggle` change in config
    plus daemon reload picks up the new key
- [ ] **Task E5 — Cloud-only happy path**:
  - Empty `secrets.toml`, run `fono setup --cloud`, paste a Groq key
  - Verify no model download
  - Press F9; verify cloud STT result
  - Run `fono use cloud cerebras` (without adding the key) — verify
    Phase C7-style consolidated error with a clear next step
- [ ] **Task E6 — Live dictation flow**:
  - Run `fono setup --advanced`, enable interactive
  - Hold F8; speak in 3 sentences with pauses; verify partials appear
    smoothly in the overlay; verify final transcript is committed
  - Verify the overlay does not steal focus on KDE X11, Plasma
    Wayland, sway, i3, GNOME
- [ ] **Task E7 — Tray menu coverage**:
  - Switch STT via tray: Local → Groq → back; verify hot-reload, no
    daemon restart, immediate F9 dictation works after each switch
  - Toggle each `Preferences` checkbox; verify the change lands in
    `config.toml` atomically
  - Languages submenu: pick a non-default language; verify next
    dictation respects it
- [ ] **Task E8 — Failure recovery**:
  - Yank the network cable mid-cloud-dictation; verify Phase C7
    notification, no stuck tray state
  - Plug a USB headset that advertises a passive endpoint; verify the
    silent-dock recovery notification (already shipped) fires
  - Kill `-9` the daemon during recording; verify socket cleanup +
    `fono` restart works without manual intervention
- [ ] **Task E9 — GPU variant auto-switch**:
  - Install CPU build on a Vulkan-capable host
  - Run `fono update`; verify auto-switch to GPU build
  - On a CPU-only host, verify no switch
- [ ] **Task E10 — Update path**:
  - Tag a v0.7.0-test release; install previous version; run
    `fono update`; verify download → SHA256 verify → atomic rename →
    daemon restart → tray icon back

### Phase F — documentation reset for the new defaults

Once Phases A–C land, the docs that reference the old defaults are
stale.

- [ ] **Task F1 — `docs/troubleshooting.md` rewrite.** Symptom-first
  ordering preserved; each symptom links to the new self-diagnostic
  notification message verbatim so users searching the toast text
  land on the right page.
- [ ] **Task F2 — `README.md` flow update.** Three-step happy path:
  install → `fono` → press F9. Drop the `fono setup` step from the
  primary flow now that it's almost-zero-question.
- [ ] **Task F3 — `docs/providers.md` provider quick-start.** Each
  provider gets a one-paragraph "what to type, in order" recipe.
- [ ] **Task F4 — `docs/dev/release-qa.md` expansion.** Phase E
  checklist becomes the canonical pre-tag manual gate.

## Verification Criteria

- The all-Enter first-run wizard produces a working dictation experience
  with zero further user intervention on a Comfortable+ host with a
  recognised locale.
- A power user opening `~/.config/fono/config.toml` after a default
  install sees ≤ 30 lines of TOML (today: 80+).
- Every notification fired by the daemon names the exact next step the
  user should take ("Run X" / "Edit Y" / "Set Z").
- `tests/check.sh --smoke` passes locally and in CI.
- The Phase E manual checklist passes on a clean Ubuntu 24.04 KDE VM
  and a clean Fedora 41 GNOME VM before tagging.
- No `notify::send` call site uses opaque language ("backend error",
  "switch failed"); each names the operation and the recovery step.
- `fono setup --advanced` exists and surfaces every prompt the
  simplified wizard hides.
- `[interactive]` and `[server.wyoming]` blocks no longer round-trip
  to disk when default-valued.

## Potential Risks and Mitigations

1. **Default flips break existing users on upgrade.** Mitigation:
   `Config::migrate` already handles schema changes; for value flips
   (B1, B2) only apply the new default when the field is absent /
   unchanged from its previous default. Never overwrite a user's
   explicit choice.

2. **Hotkey default change (B3) collides with a different niche of
   apps.** Mitigation: the 1-day research spike + ADR is the gate;
   keep the old F8/F9 as a documented fallback in
   `docs/troubleshooting.md`. Don't rush this one — wrong choice
   ships back out.

3. **Phase D smoke tests become flaky in CI** (audio mocks, IPC
   races). Mitigation: each new test must demonstrate 100/100
   green runs locally before merge; flaky tests get an issue +
   `#[ignore]` rather than a `retries=3` band-aid.

4. **`fono setup --advanced` becomes the dumping ground for every
   new prompt.** Mitigation: hard rule — any new prompt requires an
   ADR justifying why it can't be auto-decided or surfaced via tray.

5. **The simplified wizard surprises power users who expected a
   prompt.** Mitigation: the first banner the simplified wizard
   prints names `fono setup --advanced` as the alternative;
   `docs/providers.md` and the README link to it explicitly.

6. **The `eou_adaptive` / `resume_grace_ms` schema removal (B5) breaks
   somebody's hand-edited config.** Mitigation: Fono's TOML parser
   already ignores unknown fields silently (serde `#[deny_unknown_
   fields]` is *not* set); removing the fields is a no-op for the
   parser. Note this explicitly in CHANGELOG under `Removed`.

7. **Phase E manual QA is skipped under release pressure.** Mitigation:
   the v0.7.0 GitHub release workflow includes a `release-qa-checklist`
   issue template auto-created at tag time; the workflow blocks asset
   publication until the checklist issue is closed. Belt-and-braces
   for the human-in-the-loop gate.

## Alternative Approaches

1. **Skip Phase A wizard collapse; ship the long wizard.** Pro:
   no behaviour change, less risk. Con: the long wizard is the
   single biggest UX problem in the product per the audit; not
   fixing it locks in the persona-#2 bias and makes every
   landing-page screencast longer than it needs to be.
   **Rejected.**

2. **Ship Phase D smoke tests but skip Phase A/B/C.** Pro: green
   tests on a quiet schedule. Con: the tests would just pin today's
   suboptimal defaults in place; the smoke tests are valuable
   precisely because they hold the *new* simplified UX. Land A/B/C
   first, smoke-test the result. **Rejected.**

3. **Phase E manual QA only, no Phase D automation.** Pro: cheaper.
   Con: regressions are inevitable between releases; without
   automation we'll re-discover the same bugs every two months.
   **Rejected.**

4. **Phase D automation only, no Phase E.** Pro: cheaper, repeatable.
   Con: the audit shows several silent-failure paths (notifications
   ordering, focus-theft on Wayland compositors, KWin tray quirks)
   that no realistic test harness can cover. Manual QA is irreducible
   for a desktop UX product. **Rejected — both are required.**
