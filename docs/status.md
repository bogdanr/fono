# Fono — Project Status

Last updated: 2026-04-25

## Recent fix — silenced GTK/GDK startup warnings

User reported a `Gdk-CRITICAL: gdk_window_thaw_toplevel_updates: assertion ...
freeze_count > 0 failed` line at startup. This is a benign assertion fired by
libappindicator/GTK3 when the indicator first paints on KDE's StatusNotifier
host; the tray works correctly. The tray thread now installs `glib`
log handlers for the `Gdk`, `Gtk`, `GLib-GObject`, and `libappindicator-gtk3`
domains and demotes their warning/critical messages to `tracing::debug`, so
default startup is clean.

## Recent fix — cancel hotkey only grabbed while recording

User reported Fono was holding a global grab on `Escape`, blocking it in other
apps. The cancel hotkey is now registered with the OS only when entering the
Recording state and unregistered as soon as recording stops or is cancelled.
Implemented via a new `HotkeyControl` channel between the daemon's FSM event
loop and the `fono-hotkey` listener thread, plus an `unregister(...)` call in
the listener using the existing `global-hotkey` API.

## Recent fix — quieter whisper logging

User reported there were still too many startup messages coming from whisper.
The default CLI log filters now keep `whisper-rs` whisper.cpp/GGML `info`
chatter hidden behind explicit module-level `FONO_LOG` overrides while keeping
warnings and errors visible.

## Recent fix — quieter daemon startup logging

User reported too many `info` messages when starting Fono. Startup-only details
such as XDG paths, tray/hotkey internals, model-present checks, warmup timings,
inject backend discovery, and paste-shortcut setup now log at `debug`; default
`info` startup keeps only the concise daemon start/ready lines and warnings.

## Recent fix — setup wizard API key paste feedback

User reported that pasting a cloud LLM API key gave no immediate visual
indication that the paste landed. The wizard now reads API keys with a masked
prompt that prints one `*` per accepted character, then reports the received
character count before validation. The key contents remain hidden.

## Recent fix — setup wizard nested Tokio runtime panic

User reported a setup crash after adding a Groq key:
`Cannot start a runtime from within a runtime` at `crates/fono/src/wizard.rs:627`.
Root cause: the local-STT latency probe built a new Tokio runtime and called
`block_on()` while the setup wizard was already running inside Tokio. The probe
is now async and awaits `stt.transcribe(...)` on the existing wizard runtime.

## Recent fixes — tray menu hardening (env-var leak + stale binary)

User reported: "I can still see backends that aren't configured for STT and
LLM and switching through them doesn't seem to dynamically switch while the
software is running." Two distinct issues; both fixed.

1. **Env-var leak into the tray submenu.** The previous filter used
   `Secrets::resolve()` which falls through to the process environment.
   On a typical dev machine with `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`
   etc. exported in the shell, every one of those backends was wrongly
   marked "configured" and listed in the menu — clicking them then
   produced a 401 on the next dictation. New strict filter:
   `crates/fono-core/src/secrets.rs` exposes `has_in_file()` /
   `resolve_in_file()` and `crates/fono-core/src/providers.rs:178-218`
   (`configured_stt_backends` / `configured_llm_backends`) only consult
   `secrets.toml`. Two regression tests
   (`configured_filter_ignores_env`, `configured_filter_includes_explicit_keys`)
   pin the new contract.
2. **Stale release binary.** The binary at `target/release/fono` was
   older than the daemon's tray-filter source — the user was running
   the pre-fix version and the menu still listed every backend. Rebuilt
   so the live binary matches the source.

## Recent fixes — tray polish + whisper log noise + repo URL

- **Tray menu trimmed.** Removed the broken `Open history folder` entry
  (`xdg-open` on the data directory just opened the parent in Dolphin and
  was useless). The `Recent transcriptions` submenu is the supported way to
  revisit history.
- **Provider submenus restricted to configured backends.** STT/LLM submenus
  now only list backends whose API key is present in `secrets.toml` (plus
  `Local` and `None`). New helpers in `crates/fono-core/src/providers.rs`:
  `configured_stt_backends` / `configured_llm_backends`. Eliminates the
  "click OpenAI in tray, get a 401 on next dictation" trap.
- **Whisper.cpp log noise silenced.** `whisper-rs 0.16` ships a
  `whisper_rs::install_logging_hooks()` redirector that funnels GGML and
  whisper.cpp logs through `tracing`. Enabled via the new `log_backend`
  feature in workspace `Cargo.toml` and a `Once` guard in
  `crates/fono-stt/src/whisper_local.rs`. With the default `info` filter
  the formerly noisy timing dumps stay silent; `FONO_LOG=whisper_rs=debug`
  re-enables them when needed.
- **Repo URL → `bogdanr/fono`.** Replaced every reference in `Cargo.toml`,
  `README.md`, `CHANGELOG.md`, `packaging/**`, and systemd units with
  `github.com/bogdanr/fono`.

## Recent fixes (Tier-1 roadmap pass — wizard + docs polish)

- **Wizard rewrite** (`fono/src/wizard.rs`): now offers four explicit
  paths instead of a binary local/cloud choice — `Local`, `Cloud`,
  `Mixed (Cloud STT + Local LLM)`, `Mixed (Local STT + Cloud LLM)`. Path
  recommendation order is hardware-tier aware (Recommended/High-end →
  local first; Minimum → cloud first; Unsuitable → cloud only).
- **Cloud key validation** (R3.2): every API key entered in the wizard
  is hit against the provider's `/v1/models` endpoint with a 5 s
  timeout before persistence. 401/403 responses re-prompt for the key;
  network errors warn but allow override (offline-first install).
- **`docs/inject.md`** — full reference for the injection stack: priority
  table, paste-shortcut precedence, per-environment recipes (Wayland /
  KDE-Wayland / X11 / terminals / Vim / tmux), and troubleshooting.
- **`docs/troubleshooting.md`** — symptom-first guide covering hotkey,
  pipeline, STT, latency, tray, audio, provider switches, and bug
  reporting checklist.

## Recent fixes (Tier-1 roadmap pass — provider-switching tray + docs)

- **Tray STT/LLM submenus** (`fono-tray/src/lib.rs`, `fono/src/daemon.rs`).
  Right-click the tray icon → `STT: <active> ▸` or `LLM: <active> ▸` shows
  every backend with the active one ticked; click another item to hot-swap.
  Same code path as `fono use stt … / llm …` (atomic config rewrite +
  orchestrator `Reload`); tray notification confirms the switch.
- **README v0.1.0 pass** — added CLI cheatsheet entries for `fono use`,
  `fono keys`, `fono test-inject`, `fono hwprobe`, plus a tray-menu visual
  reference and a Text-Injection section explaining the Shift+Insert default
  + override layers.
- **CHANGELOG v0.1.0 entry** drafted (`CHANGELOG.md`) — pipeline, providers,
  hardware tiers, injection, tray, observability, bench harness, model
  matrix, known limitations.

## Recent fixes (delivery path — clipit/Wayland)

- **Default paste shortcut → Shift+Insert** (`fono-inject/src/xtest_paste.rs`).
  Was Ctrl+V — captured by shells/tmux/vim normal mode/terminal verbatim-
  insert bindings. Shift+Insert is the X11 legacy paste binding hard-coded
  into virtually every toolkit (xterm/urxvt/st PRIMARY, GTK/Qt CLIPBOARD,
  VTE-based PRIMARY, alacritty/kitty CLIPBOARD, Vim/Emacs in insert mode);
  fono populates **both** PRIMARY and CLIPBOARD on every dictation so the
  toolkit's selection choice is invisible. Net effect: text now lands in
  terminals as well as GUI apps.
- **`PasteShortcut` enum** with `ShiftInsert` (default), `CtrlV`,
  `CtrlShiftV`. Generalized XTEST sender: presses modifiers in order,
  presses key, releases in reverse, with `Insert` ↔ `KP_Insert` keysym
  fallback for exotic keymaps.
- **Two override layers** for the rare app that needs a different binding:
  - `[inject].paste_shortcut = "ctrl-v"` in `~/.config/fono/config.toml`
    (validated at startup; typos surface as a warn-level log line).
  - `FONO_PASTE_SHORTCUT=ctrl-v` env var (highest precedence; useful for
    one-shot testing without editing config).
  - `fono test-inject "..." --shortcut ctrl-v` flag for the smoke command.
- **Diagnostic surfaces**:
  - `fono doctor` now prints `Paste keys  : Shift+Insert (config="..."  env=...)`.
  - `fono test-inject` prints the active shortcut at the top.
  - Inject path logs `xtest-paste: synthesizing Shift+Insert (mod_keycodes=...)`
    so users can confirm what was actually sent.
- **Pure-Rust XTEST paste backend** (`fono-inject/src/xtest_paste.rs`,
  `x11-paste` feature, **on by default**). Synthesizes the configured
  shortcut against the focused X11 / XWayland window after writing to the
  clipboard. **No system tools required** — works on any X session even
  without `wtype`/`ydotool`/`xdotool`/`enigo`. Auto-selected by
  `Injector::detect()` on X11 when no other backend is available; verified
  live: `typed via xtest-paste in 15ms`.
- **`FONO_INJECT_BACKEND=xtest|paste|xtestpaste`** override for forcing
  the backend during testing.

- **Multi-target clipboard write** (`fono-inject/src/inject.rs`) — new
  `copy_to_clipboard_all()` writes to **every** detected backend
  (wl-copy + xclip clipboard + xsel + xclip primary) so X11-only managers
  like clipit catch the entry on Wayland sessions, and Wayland-native
  managers like Klipper catch it on hybrid setups.
- **Per-tool stderr capture** — silent failures (no `DISPLAY`, missing
  protocol support, non-zero exit) are now surfaced in logs and in
  `fono test-inject` output instead of being swallowed.
- **`Injector::Xdotool` subprocess backend** — independent of the
  `libxdo` C dep; XWayland fallback for KWin sessions where `wtype` is
  accepted but silently dropped.
- **`FONO_INJECT_BACKEND=…` override** — forces a specific injector for
  testing.
- **`fono test-inject "<text>"`** — bypasses STT/LLM, prints per-tool
  diagnostic + clipboard readback verification.
- **readback_clipboard `.ok()?` short-circuit fix** — verifier no longer
  aborts when the first read tool isn't installed.

## Current milestone

**v0.1.0-rc: provider switching without daemon restart.** Local-models
default + hardware-adaptive wizard (previous slice) plus a one-command
provider-switching UX: `fono use stt groq`, `fono use cloud cerebras`,
`fono use local`, plus `fono keys add/list/remove/check` and per-call
`fono record --stt … --llm …` overrides. All flips hot-reload through a
new `Request::Reload` IPC; the orchestrator hot-swaps STT/LLM behind a
`RwLock<Arc<dyn _>>` and re-prewarms on every reload.

## Active plans

| Plan | Status |
|---|---|
| `docs/plans/2026-04-24-fono-design-v1.md` (Phases 0–10) | ✅ Phases 0–10 landed |
| `docs/plans/2026-04-25-fono-pipeline-wiring-v1.md` (W1–W22) | ✅ 22/22 |
| `docs/plans/2026-04-25-fono-latency-v1.md` (L1–L30) | ✅ 17/30 landed, 13 deferred-to-v0.2 |
| `docs/plans/2026-04-25-fono-local-default-v1.md` (H1–H25) | ✅ 11/25 landed, 14 deferred-to-v0.2 |
| `docs/plans/2026-04-25-fono-provider-switching-v1.md` (S1–S27) | ✅ 16/27 landed, 11 deferred-to-v0.2 |

## Phase progress

| Phase | Description                                                        | Status |
|-------|--------------------------------------------------------------------|--------|
| 0     | Repo bootstrap + workspace + CI skeleton                           | ✅ Complete |
| 1     | fono-core: config, secrets, XDG paths, SQLite schema, hwcheck      | ✅ Complete |
| 2     | fono-audio: cpal capture + VAD stub + resampler + silence trim     | ✅ Complete |
| 3     | fono-hotkey: global-hotkey parser + hold/toggle FSM + listener     | ✅ Complete |
| 4     | fono-stt: trait + WhisperLocal + Groq/OpenAI + factory + prewarm   | ✅ Complete |
| 5     | fono-llm: trait + LlamaLocal stub + OpenAI-compat/Anthropic + factory + prewarm | ✅ Complete |
| 6     | fono-inject: enigo wrapper + focus detection + warm_backend        | ✅ Complete |
| 7     | fono-tray (real appindicator backend) + fono-overlay stub          | ✅ Complete |
| 8     | First-run wizard + CLI (+ tier-aware probe + `fono hwprobe`)       | ✅ Complete |
| 9     | Packaging: release.yml + NimbleX SlackBuild + AUR + Nix + Debian   | ✅ Complete |
| 10    | Docs: README, providers, wayland, privacy, architecture            | ✅ Complete |
| W     | Pipeline wiring (audio→STT→LLM→inject orchestrator)                | ✅ Complete |
| L     | Latency optimisation v0.1 wave (warm + trim + skip + defaults)     | ✅ Complete |
| H     | Local-models out of box + hardware-adaptive wizard (v0.1 slice)    | ✅ Complete |
| S     | Easy provider switching: `fono use`, `fono keys`, IPC Reload, hot-swap | ✅ Complete |

## What landed in this session (2026-04-25, provider switching)

* **S1/S2/S3** — `crates/fono-core/src/providers.rs` central registry of
  every backend's CLI string + canonical env-var name + paired-cloud
  preset. Factories in `fono-stt` / `fono-llm` now resolve a missing
  `cloud` sub-block by falling through to the canonical env var, so the
  smallest valid cloud config is just `stt.backend = "groq"` plus a key
  in `secrets.toml` or env.
* **S4/S5/S6** — `fono use stt|llm|cloud|local|show` subcommand tree in
  `crates/fono/src/cli.rs`; per-call `--stt` / `--llm` overrides on
  `fono record` and `fono transcribe` clone the in-memory config, never
  persist. `set_active_stt` / `set_active_llm` clear the stale `cloud`
  sub-block but preserve every unrelated user customisation.
* **S7** — `fono keys list|add|remove|check`. Atomic 0600 writes;
  `check` runs the same 2-second reachability probe as `fono doctor`.
* **S11/S12/S13** — new `Request::Reload` IPC variant; orchestrator
  holds STT + LLM + Config each behind a `RwLock<Arc<…>>`; `reload()`
  re-reads config + secrets, rebuilds via factories, swaps in place,
  and re-runs `prewarm()` so the first dictation after a switch is
  warm. `fono use` automatically calls Reload on the running daemon.
* **S18** — `fono doctor` Providers section: per-row marker for the
  active backend, key-presence flag, resolved model string, hint to
  switch via `fono use`.
* **S20/S21/S23** — new tests: `crates/fono-stt/src/factory.rs` covers
  cloud-optional resolution; `crates/fono/tests/provider_switching.rs`
  asserts `set_active_stt` / `set_active_llm` preserve unrelated fields,
  TOML round-trip survives swap, and provider-string parsers form a
  bijection with their printers.
* **S24/S25/S27** — `docs/providers.md` rewritten around the new flow;
  README has a "Switching providers" subsection; status.md updated.

## Hotfix this session (2026-04-25, tray Recent submenu + clipboard safety net)

User reported two issues after a real dictation on KDE:

1. *"I can't see any notification or anything in the clipboard after
   doing my last recording"* — root cause was a **subprocess-stdin
   deadlock**: `copy_to_clipboard` borrowed `child.stdin.as_mut()` but
   never closed the pipe, so `xsel`/`xclip`/`wl-copy` (all of which
   read stdin to EOF before daemonizing) hung forever waiting for EOF
   that never came. `child.wait()` then deadlocked, the pipeline
   returned without populating the clipboard, and any notification
   that depended on the outcome never fired. Compounding it: KDE
   Wayland's KWin doesn't implement the wlroots virtual-keyboard
   protocol that `wtype` uses, so even when the inject log read
   `inject: 27ms ok`, no keys actually reached the focused window.
2. *"OpenHistory tray action … should work in a similar fashion to
   clipit"* — clicking the tray entry only opened the parent dir;
   recent dictations weren't visible at all from the tray.

Fixes:

* **`crates/fono-tray/src/lib.rs`** — replaced single `OpenHistory`
  entry with a **"Recent transcriptions" submenu** holding 10
  pre-allocated slots refreshed every ~2 s by a `RecentProvider`
  closure (passed in by the daemon). Click any slot to re-paste that
  dictation. Clipit-style. Slots refresh in place via `set_text` to
  avoid KDE/GNOME indicator flicker. Added `OpenHistoryFolder` as a
  separate entry for power users. New `TrayAction::PasteHistory(usize)`
  carries the slot index.
* **`crates/fono/src/daemon.rs`** — provides the `RecentProvider` that
  reads `db.recent(10)` and returns the cleaned (or raw) labels.
  Handles `PasteHistory(idx)` by fetching the row and calling
  `fono_inject::type_text_with_outcome` on the blocking pool, with a
  notify-rust toast on `Clipboard` outcome.
* **`crates/fono-core/src/config.rs`** — two new `[general]` knobs,
  both default `true`:
  - `also_copy_to_clipboard` — every successful pipeline also copies
    the cleaned text to the system clipboard so the user can Ctrl+V
    even when key injection silently no-op'd.
  - `notify_on_dictation` — every successful pipeline pops a
    notify-rust toast with the dictated text (truncated to 240 chars).
* **`crates/fono-inject/`** — `copy_to_clipboard` made `pub` and
  re-exported so the orchestrator can call it directly.
* **`crates/fono/src/session.rs`** — pipeline now copies-to-clipboard
  + notifies after every successful inject; gives the user reliable
  feedback even on KDE Wayland.

User saw `WARN inject failed: no text-injection backend available` on a
host without `wtype`/`ydotool` and without the `enigo-backend` feature
compiled in. Cleaned text was lost.

* **`crates/fono-inject/src/inject.rs`** — added `Injector::Clipboard`
  fallback that shells out to `wl-copy` (Wayland) → `xclip` → `xsel`
  (X11) and a `wtype --version` page-cache warm step. New
  `InjectOutcome { Typed, Clipboard, NoBackend }` returned from
  `type_text_with_outcome()` so callers can tell the user which path
  ran. `wtype`/`ydotool` failures now fall through to the clipboard
  rather than swallowing the text.
* **`crates/fono/src/session.rs`** — pipeline calls
  `type_text_with_outcome`; on `Clipboard` shows a toast "Fono — text
  copied to clipboard, paste with Ctrl-V"; on `NoBackend` shows a toast
  with a one-line install hint (`pacman -S wtype` / `apt install xsel`).
  The toast prevents a "press hotkey, nothing happens" failure mode
  even when no injector + no clipboard tool exists.
* **`crates/fono/src/doctor.rs`** — Injector section now also lists the
  detected clipboard tool (or "none — text will be lost"); printed near
  the active injector to make the gap obvious.

### Deferred to v0.2 (documented in the plan)

* **S8** wizard multi-key (S7 already lets users add keys post-wizard).
* **S9/S10** named profiles + cycle hotkey (hold for real demand).
* **S14** auto-reload on file change (notify watcher).
* **S15/S16/S17** tray submenu for switching (depends on tray-icon API).
* **S19** dedicated `fono provider list` (covered by `fono use show` + doctor).
* **S22** full reload integration test (covered by S20 unit tests +
  manual; deferred until profiles arrive).
* **S26** ADR `0009-multi-provider-switching.md` (rationale captured in
  this plan + commit messages).

## Build matrix (verified this session, provider switching)

| Command | Result |
|---|---|
| `cargo build --workspace` | ✅ |
| `cargo test --workspace --lib --tests` | ✅ **79 tests pass** (66 unit + 13 hwcheck), 2 ignored (latency smoke) |
| `cargo clippy --workspace --no-deps -- -D warnings` | ✅ pedantic + nursery clean |
| `fono use show` | (manual) prints active stt + llm + key references |
| `fono keys list` | (manual) masked listing |

## What landed in this session (2026-04-25, local-default + hwcheck)

### Tasks fully landed (11 of 25 from the local-default plan)

* **H1** — `crates/fono/Cargo.toml:22-32`: default features now include
  `local-models` (transitively `fono-stt/whisper-local`) so the released
  binary runs whisper out of the box. Slim cloud-only build available
  via `--no-default-features --features tray`.
* **H5/H6/H21** — new `crates/fono-core/src/hwcheck.rs` (478 lines, 13
  unit tests). `HardwareSnapshot::probe()` reads `/proc/cpuinfo`,
  `/proc/meminfo`, `statvfs`, and `std::is_x86_feature_detected!` to
  produce a `LocalTier` ∈ { Unsuitable, Minimum, Comfortable,
  Recommended, HighEnd } with documented thresholds (`MIN_CORES = 4`,
  `MIN_RAM_GB = 4`, `MIN_DISK_GB = 2`, etc.) duplicated as `pub const`
  so docs and tests stay in sync.
* **H11/H12/H13** — wizard rewritten around the tier:
    * `crates/fono/src/wizard.rs` prints the hardware summary up-front.
    * `Recommended`/`HighEnd`/`Comfortable` → local first, default.
    * `Minimum` → cloud first ("faster on your machine"), local kept
      as the second option with a "~2 s" warning.
    * `Unsuitable` → local hidden behind a `Confirm` showing the
      specific failed gate (e.g. "only 2 physical cores; minimum is 4").
    * Local-model menu narrowed to the tier's recommended model + one
      safer fallback (no longer shows whisper-medium on a 4-core box).
* **H16** — `fono doctor` now prints the hardware snapshot and tier
  alongside the existing factory probes, so users see at a glance
  whether their config matches their hardware.
* **H17** — new `fono hwprobe [--json]` subcommand:

  ```
  cores : 10 physical / 12 logical  (AVX2)
  ram   : 15 GB total · disk free : 11 GB · linux/x86_64
  tier  : comfortable (recommends whisper-small)
  ```

  JSON output is consumable by packaging scripts and the bench crate.
* **H20** — `README.md` reflects v0.1.0-rc reality: default release
  bundles whisper.cpp, build-flavour matrix, `fono hwprobe` mention.
* **H24/H25** — plan persisted at
  `docs/plans/2026-04-25-fono-local-default-v1.md`; this status entry.

### Toolchain bumps

* `Cargo.toml:73` — `whisper-rs = "0.13" → "0.16"` (0.13.2 had an
  internal API/ABI mismatch with its sys crate; 0.16 is the current
  upstream and is what whisper.cpp tracks).
* `crates/fono-stt/src/whisper_local.rs:84-92` — adapt to the 0.16
  segment API (`get_segment(idx) -> Option<WhisperSegment>` +
  `to_str_lossy()`).

### Tasks intentionally deferred to v0.2 (all annotated in plan)

* **H8** — Real `LlamaLocal` implementation against `llama-cpp-2`.
  `llama-cpp-2 0.1.x` exposes a low-level API that needs several hundred
  lines of safe-wrapper code; the v0.1 slice ships local STT only with
  optional cloud LLM cleanup. New ADR
  `docs/decisions/0008-llama-local-deferred.md` captures the rationale.
* **H2/H3** — Release CI matrix (musl-slim + glibc-local-capable
  artifacts) — Phase 9 release work, separate from this slice.
* **H4** — OpenBLAS / Metal compile flags (would speed local inference
  another 2–3× on capable hosts) — opt-in v0.2 work.
* **H7/H14/H22** — In-wizard smoke bench + tier-profile bench in
  `fono-bench` — static rule + `fono doctor` are sufficient for v0.1.
* **H15/H18/H19** — Persisting tier in config + flipping
  `LlmBackend::default()` to Local + auto-migration — blocked on H8.
* **H23** — Wizard tier-decision unit test — covered by H21 tier tests
  + manual run; full `dialoguer` mock not worth the dependency.

## Build matrix (verified this session)

| Command | Result |
|---|---|
| `cargo build -p fono` (default features) | ✅ — bundles whisper.cpp |
| `cargo build -p fono --no-default-features --features tray` | (slim, cloud-only — covered by H1's feature graph) |
| `cargo test --workspace --lib --tests` | ✅ **67 tests pass** (54 unit + 13 hwcheck), 2 ignored (latency smoke) |
| `cargo clippy --workspace --no-deps -- -D warnings` | ✅ pedantic + nursery clean |
| `cargo run -p fono -- hwprobe` | ✅ classified host as `comfortable` (10c/16GB/AVX2) |
| `cargo run -p fono -- hwprobe --json` | ✅ structured snapshot + tier |

## Recommended next session

1. Implement **H8** (`LlamaLocal` against `llama-cpp-2`) so the local
   path also covers LLM cleanup. Keep behind `llama-local` feature flag
   until proven; flip the wizard's local LLM offer back on once H9's
   integration test passes.
2. Land **L7+L8** (streaming LLM + progressive injection) — the next
   biggest perceived-latency win.
3. Pin real fixture SHA-256s via
   `crates/fono-bench/scripts/fetch-fixtures.sh` and commit
   `docs/bench/baseline-*.json` for CI regression gating.
4. Tag `v0.1.0` once `fono-bench` passes on the reference machine.
