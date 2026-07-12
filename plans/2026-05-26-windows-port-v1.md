# Windows Port — Remote Dev Setup and Build Plan

## Objective

Ship a working Windows build of Fono as a second-class target alongside the
first-class Linux build, developed primarily from the Linux workstation with
build and runtime testing executed remotely on a Windows machine over
SSH/rsync. The Linux build must stay byte-equivalent to today through every
phase except where an explicit, called-out cross-platform improvement
(IPC/locale unification) is adopted with documented Linux cost.

**Target Windows surface for v1**: native `x86_64-pc-windows-msvc` `.exe`,
WASAPI audio capture and playback, Win32 tray icon, global hotkeys via
`global-hotkey`, click-through overlay panel, autostart via `HKCU\…\Run`
registry key, `fono update` self-replacement via rename-and-relaunch, full CLI
parity (`fono doctor`, `fono use`, `fono history`, etc.). No code signing in
v1, no MSI installer; ship the bare `.exe` plus a `.sha256` sidecar.

**Out of scope for v1**: macOS port (separate plan), MSI / NSIS installer,
Authenticode code signing, Windows-specific tray-only "kiosk" mode, ARM64
Windows, Windows-flavored Vulkan release variant.

## Guiding Constraints (read these first)

1. **Linux first-class** — every change must leave the existing Linux build
   green, no NEEDED-set growth, no `release-slim` size budget regression, no
   workspace-deps mutation that affects the Linux dep graph. CI's Linux size
   gate at `.github/workflows/ci.yml:268-296` is the mechanical enforcement.
2. **Windows code is additive** — every new Windows file lives behind
   `#[cfg(target_os = "windows")]` or in a `[target.'cfg(windows)'.dependencies]`
   table. Existing Linux modules are not rewritten; they are moved verbatim
   into `linux.rs` sibling files when a trait split is introduced.
3. **Explicit Linux trade-offs are flagged** — three Tier-1 unifications
   (IPC, locale, notifications) cost Linux <50 KB combined and zero behaviour
   change. They are called out in their own phase. Any other proposed Linux
   sacrifice must be raised explicitly before adoption.
4. **CI Linux gate cannot block on Windows** — Windows CI row starts as
   `continue-on-error: true` and the release-asset matrix produces Linux
   artefacts independently. A red Windows build never holds a Linux release.
5. **Pre-commit gate stays Linux-only** — the AGENTS.md three-step gate
   (`cargo fmt`, `cargo clippy`, `cargo test --workspace --tests --lib`) runs
   on the Linux dev host before every commit. Windows verification is an
   optional pre-push step over SSH.

## Implementation Plan

### Phase 0 — Remote Windows dev environment (setup, no Fono code yet)

- [x] Task 0.1. **Confirm Windows host is reachable on LAN.** Document IP,
      hostname, and Windows edition (Win 10 1809+ or Win 11) in `docs/build-windows.md`.
      Required: 64-bit, x86_64. Rationale: every later phase assumes the
      host exists and is addressable.
      Done 2026-07-06: Windows 10 build 19045 (22H2), 64-bit, confirmed
      reachable over SSH.
- [x] Task 0.2. **Enable OpenSSH Server on Windows.** Via
      `Settings → Apps → Optional Features → Add → OpenSSH Server`, or
      PowerShell:
      `Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0`. Then
      `Set-Service sshd -StartupType Automatic; Start-Service sshd`. Open
      firewall: `New-NetFirewallRule -Name sshd -DisplayName 'OpenSSH SSH Server' -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22`.
      Rationale: native built-in service; no third-party install, no WSL.
      Done 2026-07-06.
- [x] Task 0.3. **Configure SSH key auth from Linux dev host.** Copy
      `~/.ssh/id_ed25519.pub` to `C:\Users\<user>\.ssh\authorized_keys` on
      Windows (case-sensitive filename, no extension). Permissions matter on
      Windows OpenSSH — run `icacls authorized_keys /inheritance:r /grant <user>:F`
      from an admin PowerShell. Disable password auth in `C:\ProgramData\ssh\sshd_config`
      (`PasswordAuthentication no`); restart sshd. Test `ssh win 'whoami'`
      from Linux returns the user name with no prompt.
      Done 2026-07-06 (by the user, ahead of the agent session).
- [x] Task 0.4. **Install Visual Studio Build Tools 2022 on Windows.**
      Download the standalone Build Tools installer from Microsoft; select
      the "Desktop development with C++" workload, which pulls in MSVC v143
      compiler, Windows 11 SDK (latest), CMake, and Ninja. Verify with
      `where cl.exe` and `cmake --version` in a `x64 Native Tools Command Prompt`.
      Rationale: `whisper-rs-sys` and `llama-cpp-sys-2` vendor C++ that needs
      MSVC; this is non-optional.
      Done 2026-07-06 (installer needed a human at the keyboard — SSH
      sessions carry a UAC-filtered token even for admin accounts, so the
      elevation prompt cannot be scripted). MSVC v14.44 (v143), Windows 11
      SDK 10.0.26100.0, VS Build Tools 17.14 confirmed via `vswhere`.
      **Gotcha found, not in the original task**: the VS-bundled CMake is
      not on `PATH` outside a Native Tools prompt; added its bin dir to the
      system `Path` explicitly. See `docs/build-windows.md`.
- [x] Task 0.5. **Install Rust MSVC toolchain on Windows.** `rustup-init.exe`,
      accept defaults, host triple `x86_64-pc-windows-msvc`. Then
      `rustup component add clippy rustfmt`. Verify
      `rustc --version --verbose` reports `host: x86_64-pc-windows-msvc`.
      Done 2026-07-06; `rustc 1.88` (per `rust-toolchain.toml`) confirmed
      `host: x86_64-pc-windows-msvc`.
      **Gotcha found, not in the original task**: bindgen (used by
      `llama-cpp-sys-2`/`whisper-rs-sys`) needs `libclang.dll`, which VS
      Build Tools does not bundle. Installed standalone LLVM and set
      `LIBCLANG_PATH` system-wide. Also needed:
      `LongPathsEnabled=1` (`HKLM\SYSTEM\CurrentControlSet\Control\FileSystem`)
      plus `git config --global core.longpaths true` — the vendored
      llama.cpp submodule checkout exceeds the legacy 260-char `MAX_PATH`.
      See `docs/build-windows.md` for the full writeup.
- [x] Task 0.6. **Install rsync on Windows.** Easiest path: install Git for
      Windows (includes `rsync.exe` in recent versions). Alternative: MSYS2
      with `pacman -S rsync`. Confirm `ssh win 'rsync --version'` from Linux.
      Done 2026-07-06 via MSYS2 (`pacman -S rsync openssh`) — current Git
      for Windows no longer bundles `rsync.exe`, contrary to this task's
      assumption.
- [x] Task 0.7. **Install `cargo-xwin` on Linux dev host** for cross-compile
      iteration. `cargo install cargo-xwin`. Add the target on Linux:
      `rustup target add x86_64-pc-windows-msvc`. Rationale: ~80 % of porting
      work doesn't need a Windows runtime to validate; cross-compile catches
      compile errors in seconds instead of seconds-plus-rsync.
      Done 2026-07-06; smoke-tested with `cargo xwin build --target
      x86_64-pc-windows-msvc -p fono-core` (clean compile).
- [x] Task 0.8. **Create the remote helper script.** Add
      `scripts/win-remote.sh` (Linux-side, marked executable) with three
      subcommands: `push` (rsync to win), `build` (push + ssh cargo build),
      `test` (push + ssh cargo test). Excludes: `target/`, `.git/`,
      `models/`, `*.wav`, large bench output. Document in
      `docs/build-windows.md`.
      Done 2026-07-06, modeled on `scripts/mac-remote.sh`; also resolves
      `ORT_LIB_LOCATION` on each push-based command via
      `scripts/fetch-onnxruntime.sh` run remotely through MSYS bash.
- [x] Task 0.9. **First end-to-end smoke through the remote pipeline.** From
      Linux: `./scripts/win-remote.sh push && ssh win 'cd fono && cargo --version'`.
      Confirms rsync works, ssh works, cargo is on Windows PATH, target
      directory mounts cleanly. No actual Fono build yet — just the plumbing.
      Done 2026-07-06, and taken further: `fono-core` (including the
      `llama-local` feature — full MSBuild/cmake C++ compile) builds
      cleanly natively on Windows after fixing a genuine cross-platform
      bindgen ABI bug (`crates/fono-core/src/brain_tap.rs`, see
      `docs/status.md`). `cargo build/check -p fono` (the full binary)
      fails exactly at the Phase 1 boundary (`fono-ipc`'s Unix sockets,
      `fono-inject::focus`'s Unix socket import) — expected, not a setup
      gap. See `docs/build-windows.md` for full detail.

**Phase 0 gate**: `ssh win 'cd fono && cargo --version && cl.exe /?'` runs
without error from the Linux dev host. `docs/build-windows.md` exists.

**Phase 0 status: COMPLETE (2026-07-06).**

### Phase 1 — Linux-only trait refactor (zero behaviour change)

This phase touches only Linux files. The goal is to introduce trait+impl
splits in every subsystem that will later need a Windows sibling, so each
subsequent phase becomes a single isolated `windows.rs` file addition. No
Windows code lands in this phase. Linux pre-commit gate must stay green
throughout.

- [x] Task 1.1. **`fono-tray` trait split.** Define `pub trait TrayBackend`
      in `crates/fono-tray/src/lib.rs` with the same surface as the current
      free functions. Move existing ksni-based code into
      `crates/fono-tray/src/linux.rs` behind `#[cfg(target_os = "linux")]`.
      The `lib.rs` re-export switches on `cfg(target_os)`. Verify by
      `cargo build -p fono-tray` and confirming no clippy regressions.
- [x] Task 1.2. **`fono-overlay` already has the backend split** — confirm
      no refactor needed. The `BackendId` enum and `candidate_list_with`
      table at `crates/fono-overlay/src/backend.rs` are already the right
      extension point. Add a TODO comment marking where the Windows
      `Win32LayeredToolWindow` row will slot in.
- [x] Task 1.3. **`fono-inject` `Injector` enum extension point.** The
      `Injector` enum at `crates/fono-inject/src/inject.rs:10-30` is already
      the unified surface. Move the auto-detection branch in `detect_auto`
      that early-returns `Self::Enigo` on X11 into a `#[cfg(target_os = "linux")]`
      block; later add a `#[cfg(target_os = "windows")]` branch returning
      `Self::Enigo` unconditionally.
- [x] Task 1.4. **`fono-hotkey` already split correctly** — the `portal` and
      `gnome_gsettings` modules are already `#[cfg(target_os = "linux")]` at
      `crates/fono-hotkey/src/lib.rs:8-13`. The listener and FSM are
      OS-agnostic. Document this in a comment block at the crate root.
- [x] Task 1.5. **`fono-audio` already split correctly** — capture.rs and
      playback.rs already gate Linux subprocess paths with
      `#[cfg(all(target_os = "linux", not(feature = "cpal-backend")))]`.
      Document in crate-root comment that non-Linux targets compile the
      cpal path by default.
- [x] Task 1.6. **`fono/src/install.rs` trait split.** Define
      `pub trait Installer` with `install(mode) -> Result<()>` and
      `uninstall() -> Result<()>` methods. Move existing module into
      `install/linux.rs`; introduce `install/mod.rs` that dispatches on
      `cfg(target_os)`. Linux behaviour byte-identical.
- [x] Task 1.7. **`fono-update` asset-naming abstraction.** Extract the
      asset-name format string into a small `fn current_asset_name() -> String`
      that switches on `cfg(target_os)` and `cfg(target_arch)`. Linux returns
      today's value (`fono-vX.Y.Z-x86_64`); Windows branch is a stub returning
      `fono-vX.Y.Z-x86_64.exe` for now, gated `#[cfg(target_os = "windows")]`.
- [x] Task 1.8. **Pre-commit gate green.** Run `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --tests --lib`. All must pass with no new
      warnings and no test changes. Binary size delta must be within ±5 KB
      of pre-refactor `cargo build --profile release-slim -p fono`.

**Phase 1 gate**: zero behaviour change on Linux. Refactor commit can be
reverted cleanly; binary size unchanged ±5 KB; all 728+ tests still pass.

**Phase 1 status: COMPLETE (2026-07-10).** Notes: several tasks were
already discharged by earlier work — the overlay backend table (1.2),
hotkey/audio cfg splits (1.4/1.5), and the installer module dispatch
(1.6, realised during the macOS port as cfg-dispatched `install/{mod,
linux,macos}.rs` with an `unsupported` fallback rather than a literal
trait; the same applies to the tray, where the existing
`ActiveBackends`/provider surface already is the platform-neutral seam,
so 1.1 became a verbatim move of the ksni code into
`backend_linux.rs`). `fono-inject::focus` gained
`cfg(target_os = "linux")` gates around the Unix-socket focus cascade
(1.3), and `fono-update` grew the Windows `.exe` asset-name stub plus
the CPU-only prefix short-circuit (1.7). Gate: fmt/clippy/tests green
(1423 tests passed), release-slim binary size delta exactly 0 bytes.

### Phase 2 — CI Windows row, non-blocking

- [x] Task 2.1. **Add Windows row to `.github/workflows/ci.yml`** build
      matrix. Runner: `windows-2022`. Variant: cpu only initially. Set
      `continue-on-error: true` on the row and `fail-fast: false` on the
      matrix. Steps: checkout, install Rust (default host on Windows is
      MSVC), `cargo build -p fono`, `cargo test --workspace --tests --lib`.
- [x] Task 2.2. **Windows-specific size + NEEDED check skipped.** The
      existing ELF NEEDED check at `ci.yml:268-296` runs only on
      `runner.os == 'Linux'`. Confirm this condition is honored; add an
      explicit comment marking Windows size gate as deferred to Phase 14.
- [x] Task 2.3. **First Windows CI run is expected to fail.** Document
      this in the Phase 2 commit message and in `docs/build-windows.md`.
      The job exists to surface progress; `continue-on-error` prevents it
      from blocking the Linux pipeline.

**Phase 2 gate**: Linux CI rows green; Windows row present and visible in
the GitHub Actions UI; PR / push workflow unaffected by Windows failures.

**Phase 2 status: COMPLETE (2026-07-10).** Notes: implemented as a
dedicated `windows` job (mirroring how the macOS port added its
`macos` job) rather than a matrix row — same visibility, cleaner
separation. The job bakes in the Phase 0 findings for the hosted
runner: `git config --system core.longpaths true` before checkout,
`LIBCLANG_PATH` pointed at the image's preinstalled LLVM, and the
pinned `onnxruntime.lib` fetched via Git Bash. On 2.2: the ELF check
no longer lives behind a `runner.os == 'Linux'` condition — it is the
structurally Linux-only `size-budget` job (with a `size-budget-macos`
Mach-O sibling); the deferred-to-Phase-14 PE gate is documented in the
`windows` job header and `docs/build-windows.md`.

### Phase 3 — First successful Windows cross-compile (`cargo-xwin`)

This phase iterates on `cargo xwin build --target x86_64-pc-windows-msvc -p fono`
from the Linux host until the binary links. Expect snags in vendored C++
(whisper-rs-sys, llama-cpp-sys-2). No runtime testing yet.

- [x] Task 3.1. **First xwin invocation.** *(Done 2026-07-10 — via the
      native SSH loop `scripts/win-remote.sh build -p fono`, which is the
      Phase 0 reference toolchain; xwin cross-compile deferred as the fast
      path.)* All vendored C++ (`whisper-rs-sys`, `llama-cpp-sys-2`) and
      every Rust crate compile cleanly on `x86_64-pc-windows-msvc`. Two
      link-stage failures surfaced, in order: (1) `LNK1181: cannot open
      input file 'stdc++.lib'` (fixed — Task 3.3); (2) `LNK1120: 157
      unresolved externals` from `libort_sys` (protobuf / abseil / onnx /
      cpuinfo) — the pinned Windows `onnxruntime.lib` needs its companion
      static libs on the link line. That ONNX-Runtime-on-MSVC provisioning
      gap is the current Phase 3 blocker (tracked into Phase 5's audio/ORT
      work; a CPU-only build without `tts-local` may sidestep it for v1).
- [x] Task 3.2. **Fix CMake toolchain detection for whisper-rs-sys.**
      *(Done 2026-07-10 — no fix needed.)* With the Phase 0 native MSVC
      toolchain (VS Build Tools + `LIBCLANG_PATH` + long paths),
      `whisper-rs-sys` and `llama-cpp-sys-2` CMake/Ninja builds succeed
      out of the box; no `[target.x86_64-pc-windows-msvc]` env overrides
      were required.
- [x] Task 3.3. **Resolve OpenMP / C++-runtime-on-MSVC linkage.**
      *(Done 2026-07-10.)* The anticipated OpenMP problem did **not**
      materialise: `llama-cpp-sys-2`'s build script already gates its
      `gomp` link on `target_triple.contains("gnu")` and links the MSVC
      CRT (not `stdc++`) on MSVC, so `openmp + static-openmp +
      static-stdcxx` are no-ops on Windows. The real blocker was
      `ort-sys`: `.cargo/config.toml` sets `ORT_CXX_STDLIB=static:-bundle
      =stdc++` for the Linux-gnu NEEDED allowlist, but cargo's `[env]`
      table is **not** target-scoped, so it leaked to MSVC and ort-sys
      emitted a bogus `-lstdc++` → `LNK1181`. Fix: neutralise the env to
      empty on Windows (ort-sys then uses its correct MSVC default of no
      explicit C++ stdlib link) — the CI `windows` job exports
      `ORT_CXX_STDLIB=` and `scripts/win-remote.sh` passes
      `--config env.ORT_CXX_STDLIB=''`. Documented in `.cargo/config.toml`
      and `docs/build-windows.md`.
- [x] Task 3.4. **Decide on Vulkan for Windows v1.** *(Done — CPU-only
      Windows v1; GPU/Vulkan variant deferred. Already reflected in the
      Phase 1 `fono-update` asset-name work.)*
- [ ] Task 3.5. **Linker success.** `cargo xwin build --target x86_64-pc-windows-msvc -p fono`
      produces `target/x86_64-pc-windows-msvc/release-slim/fono.exe`.
      Binary size first measurement (no budget yet). Run-time correctness
      not verified in this phase.
- [ ] Task 3.6. **Verify native build over SSH matches.**
      `rsync` and `ssh win 'cargo build --profile release-slim -p fono'`
      from Linux. Confirms the Windows host toolchain produces an
      equivalent (not necessarily byte-identical) binary. From here on,
      native is the reference; xwin is the fast-iteration shortcut.

**Phase 3 gate**: `fono.exe` builds via both cross-compile (xwin) and
native (ssh-driven). No runtime correctness yet. Phase 2 CI Windows row
turns green at compile-and-link level.

### Phase 4 — Tier-1 unifications (IPC, locale, notifications)

This phase makes three deliberate Linux-affecting cleanups documented in
the prior unification analysis. Each is a small, called-out Linux trade-off.

- [x] Task 4.1. **IPC: switch from `tokio::net::UnixListener` to
      `interprocess` crate.** *(Done 2026-07-10.)* Added
      `interprocess = { version = "2", features = ["tokio"] }` to workspace
      deps (licences 0BSD OR Apache-2.0, already in the deny.toml
      allowlist). `crates/fono-ipc` now builds its listener/stream via
      `interprocess::local_socket::tokio` — Unix-domain socket at the same
      filesystem path on Linux/macOS, named pipe on Windows — and exposes
      `Stream` / `Listener` / `RecvHalf` / `SendHalf` plus `accept()` /
      `split_stream()` helpers so `fono` and `fono-mcp-server` don't need
      `interprocess` as a direct dep. **Verified**: fmt/clippy/tests green;
      release-slim size **21.34 MiB** (well under budget, NEEDED allowlist
      clean — interprocess added ~0); and the full `fono` binary now
      compiles on `x86_64-pc-windows-msvc` (the old `fono-ipc` `UnixListener`
      breakpoint is gone).
- [ ] Task 4.2. **Locale: switch to `sys-locale` crate.** Replace ad-hoc
      env-var probes in `crates/fono-core/src/locale.rs`. **Linux trade-off**:
      ~30 KB binary growth, behaviour identical (sys-locale falls through
      to the same envs on Linux). Verify locale tests pass.
- [ ] Task 4.3. **Confirm notifications already cross-platform.**
      `notify-rust` already works on all three OSes; no change needed.
      Document in `docs/build-windows.md`.
- [ ] Task 4.4. **Pre-commit gate green + size budget check.** Linux
      `release-slim` binary may grow by up to ~30 KB. Confirm under 20 MiB
      budget with margin.

**Phase 4 gate**: Linux binary still under 20 MiB; all tests green; IPC
works against running daemon; Windows compile picks up the new crates
cleanly.

### Phase 5 — Audio capture and playback on Windows (cpal default)

- [ ] Task 5.1. **Make `cpal-backend` feature default on Windows.** In
      `crates/fono-audio/Cargo.toml`, add a
      `[target.'cfg(target_os = "windows")'.features]`-equivalent
      construct: enable `cpal-backend` via a `windows-defaults` cargo
      feature wired into the top-level `fono` binary's
      `[target.'cfg(windows)'.dependencies]` block. Linux stays on parec
      by default.
- [ ] Task 5.2. **Verify WASAPI capture round-trip.** `cargo run -p fono -- doctor`
      via ssh on Windows reports a working default input device. Manual
      smoke: `fono setup`, configure cloud STT (Groq), press hotkey, speak,
      confirm transcript lands at cursor.
- [ ] Task 5.3. **Verify WASAPI playback round-trip.** Manual smoke:
      configure assistant, press F8, ask question, hear reply through
      default output device.
- [ ] Task 5.4. **Microphone enumeration on Windows.** The Linux tray
      microphone submenu uses `pactl list short sources`. On Windows,
      enumerate via cpal's `HostTrait::input_devices()`. Wire into the
      same tray-action layer once tray lands in Phase 6.

**Phase 5 gate**: end-to-end voice → cloud STT → injected text works on
Windows. Linux audio path unchanged and verified by Linux smoke test.

### Phase 6 — Tray icon on Windows (`tray-icon` crate)

- [ ] Task 6.1. **Add `tray-icon` to Windows-only deps.** In
      `crates/fono-tray/Cargo.toml`:
      `[target.'cfg(target_os = "windows")'.dependencies] tray-icon = "0.20"`.
      Linux stays on ksni; tray-icon never touches the Linux dep graph.
- [ ] Task 6.2. **Implement `tray::windows` impl behind the trait from
      Phase 1.1.** Mirror the menu structure of the Linux impl: icon,
      title, primary action (toggle), submenu for backend selection,
      microphone selection, etc.
- [ ] Task 6.3. **Embed PNG icon for Windows.** Linux uses an SVG (good for
      hicolor scalable); Windows expects PNG or ICO. Embed at compile time
      via `include_bytes!` from `assets/fono.png` (already in the repo).
- [ ] Task 6.4. **Verify Linux tray unchanged.** `cargo build -p fono`
      on Linux produces a binary with identical `nm`-visible ksni symbols
      and zero new NEEDED entries.

**Phase 6 gate**: tray icon appears in Windows notification area with
correct menu structure. Linux ksni tray unchanged.

### Phase 7 — Text injection on Windows (enigo)

- [ ] Task 7.1. **Enable enigo on Windows builds.** Add
      `[target.'cfg(target_os = "windows")'.dependencies] enigo = "0.2"`
      with default features (no libxdo on Windows; enigo uses Win32
      `SendInput` directly).
- [ ] Task 7.2. **Adjust `Injector::detect_auto` for Windows.** Add a
      `#[cfg(target_os = "windows")]` branch that returns `Self::Enigo`
      unconditionally; no Wayland / X11 probes apply.
- [ ] Task 7.3. **Verify against three target Windows apps**: Notepad,
      Chrome address bar, Discord/Slack chat input. Each should receive
      the dictated text via SendInput without focus stealing.
- [ ] Task 7.4. **Clipboard fallback on Windows.** The existing
      `copy_to_clipboard` path uses platform-agnostic crates already; verify
      it works on Windows. If not, add `arboard` crate behind Windows-only
      cfg.

**Phase 7 gate**: text injection works in three reference apps; Linux
inject cascade unchanged.

### Phase 8 — Hotkeys on Windows (`global-hotkey`)

- [ ] Task 8.1. **`global-hotkey` already cross-platform.** Confirm
      Windows MSVC build picks up the Win32 `RegisterHotKey` backend.
      Smoke test: register F7, press F7, verify the listener emits
      `TogglePressed`.
- [ ] Task 8.2. **Default hotkeys reasonable on Windows.** F7/F8 work but
      conflict with some apps' built-in shortcuts. Document in
      `docs/build-windows.md` that users can rebind via `fono use hotkey`.
      No behavioural change vs Linux for v1.
- [ ] Task 8.3. **Esc-to-cancel on Windows.** Use the same dynamic
      `EnableCancel` / `DisableCancel` machinery; register Esc transiently
      via `global-hotkey` only during active recording. Mirrors the v0.8.2
      Linux behaviour via a different backend.
- [ ] Task 8.4. **Verify no Linux portal regression.** The
      `#[cfg(target_os = "linux")] mod portal` import stays intact; Linux
      portal-based Esc cancel still works on KDE-Wayland / sway / Hyprland.

**Phase 8 gate**: hotkey press starts/stops recording on Windows;
Esc-to-cancel works; Linux Wayland Esc-portal flow unchanged.

### Phase 9 — Focus detection on Windows (Win32 foreground window)

- [ ] Task 9.1. **Define `FocusBackend` trait.** Extract today's `detect_focus`
      free function in `crates/fono-inject/src/focus.rs` behind a trait.
      Linux impl uses `x11rb`; Windows impl uses `windows-sys` crate's
      `GetForegroundWindow` + `GetWindowThreadProcessId` + executable name
      lookup via `QueryFullProcessImageNameW`.
- [ ] Task 9.2. **Windows-only dep.** Add
      `[target.'cfg(target_os = "windows")'.dependencies] windows-sys = { version = "0.59", features = ["Win32_UI_WindowsAndMessaging", "Win32_System_Threading", "Win32_System_ProcessStatus"] }`.
      Confirms zero Linux impact.
- [ ] Task 9.3. **Per-app context rules on Windows.** The classifier in
      `fono-inject/src/classifier.rs` matches on app names / process names;
      ensure the Windows focus probe returns names in a form the classifier
      can match. Add Windows-flavoured rules for known apps (e.g.
      `chrome.exe`, `code.exe`, `WindowsTerminal.exe`).

**Phase 9 gate**: `fono doctor` on Windows reports the current focused
window's app name. Per-app rules fire correctly for at least three test
apps. Linux x11rb path unchanged.

### Phase 10 — Overlay backend on Windows (Win32 layered toolwindow)

- [ ] Task 10.1. **New backend file `crates/fono-overlay/src/backends/windows.rs`.**
      Uses winit with raw Win32 escape hatch to set
      `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOPMOST`
      and exclude from Alt+Tab via `WS_EX_TOOLWINDOW`. softbuffer handles
      ARGB premultiplied draws same as the existing renderer.
- [ ] Task 10.2. **Extend `BackendId` enum and selection table.** Add
      `BackendId::Win32LayeredToolWindow`. Update
      `candidate_list_with` to return this single candidate on Windows;
      Linux table unchanged.
- [ ] Task 10.3. **Click-through and focus passthrough verified.** With
      overlay visible during recording, click through onto a window beneath
      and confirm the click reaches it. Type into a text field while
      overlay is shown — keystrokes must land in the field, not the overlay.
- [ ] Task 10.4. **Multi-monitor positioning.** Anchor to primary monitor's
      bottom-centre, mirroring the Linux behaviour. Use Win32
      `GetSystemMetrics` / `EnumDisplayMonitors`.
- [ ] Task 10.5. **`FONO_OVERLAY_BACKEND` env override works on Windows.**
      Aliases `win32` / `noop`. Selection still falls through to noop on
      failure.

**Phase 10 gate**: overlay paints during recording on Windows with
correct anchoring; doesn't steal focus; doesn't appear in Alt-Tab. Linux
wlr-layer-shell / X11 / noop backends unchanged.

### Phase 11 — Install and autostart on Windows

- [ ] Task 11.1. **`Installer::windows` impl behind the trait from Phase 1.6.**
      Default install location: `%LOCALAPPDATA%\fono\fono.exe`. Autostart:
      write `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\fono`
      pointing to the install path. Use the `winreg` crate behind
      Windows-only cfg.
- [ ] Task 11.2. **`sudo fono install` becomes `fono install` on Windows.**
      No elevation needed for `HKCU` writes. Document that `--server` mode
      is Linux-only in v1 (no Windows service install in v1).
- [ ] Task 11.3. **`fono uninstall` on Windows.** Remove registry key,
      delete `%LOCALAPPDATA%\fono\` directory, leave user config under
      `%APPDATA%\fono\` intact (mirrors Linux behaviour of preserving
      user config).
- [ ] Task 11.4. **Install marker on Windows.** Write
      `%LOCALAPPDATA%\fono\install_marker.toml` analogous to the Linux
      `/usr/local/share/fono/install_marker.toml`. Same TOML schema.
- [ ] Task 11.5. **Verify Linux install path unchanged.** Run
      `sudo fono install --server` on a Linux test host; confirms
      systemd unit installation, `fono` system user creation, and
      hardened service activation still work.

**Phase 11 gate**: `fono install` on Windows copies the binary, writes
the registry autostart entry, and the daemon starts on next login.
`fono uninstall` reverses cleanly. Linux install layer behaviour
byte-identical.

### Phase 12 — `fono update` on Windows (rename-and-relaunch)

- [ ] Task 12.1. **Asset name lookup uses `current_asset_name()` from
      Phase 1.7.** Confirm Windows returns `fono-vX.Y.Z-x86_64.exe`.
- [ ] Task 12.2. **Self-replacement via rename trick.** Windows can't
      overwrite a running `.exe`. Add a `#[cfg(target_os = "windows")]`
      branch in `fono-update`: (a) download to a temp file in same
      directory, (b) verify SHA-256, (c) rename current `fono.exe` to
      `fono.exe.old`, (d) rename temp to `fono.exe`, (e) launch the new
      binary, (f) exit current process. Cleanup of `.exe.old` happens
      on next start.
- [ ] Task 12.3. **Package-managed detection.** The Linux
      `is_package_managed` check looks for `/usr/bin/fono`; on Windows
      there is no equivalent — skip the check on Windows or treat
      install under `Program Files` as managed (don't self-replace).
- [ ] Task 12.4. **Verify Linux update path unchanged.** Run
      `fono update --dry-run` on Linux to confirm asset selection
      (CPU vs GPU) and rename(2)-based replace still work.

**Phase 12 gate**: `fono update` on Windows downloads, verifies, and
replaces the running `.exe` atomically. Linux update flow unchanged
including the v0.5.0 CPU↔GPU auto-switching.

### Phase 13 — Release workflow: Windows artefact

- [ ] Task 13.1. **Add Windows row to `release.yml` build matrix.**
      Runner: `windows-2022`. Variant: cpu. Asset name:
      `fono-${version}-x86_64.exe`. Build with
      `cargo build --profile release-slim --target x86_64-pc-windows-msvc -p fono`.
- [ ] Task 13.2. **Skip the ELF NEEDED verification step on Windows.**
      The existing step at `release.yml:268` already gates on
      `runner.os == 'Linux'`. Confirm.
- [ ] Task 13.3. **Stage the `.exe` for upload.** Existing staging step
      at `release.yml:298-337` already handles `.exe` suffix via
      `if [[ "${target}" == *windows* ]]`. Verify.
- [ ] Task 13.4. **No Windows packaging job** — ship bare `.exe` plus
      `.sha256` sidecar only in v1. MSI / signing are explicit non-goals.
- [ ] Task 13.5. **`SHA256SUMS` includes the Windows asset.** The
      existing `find … -name "fono-v*-x86_64.exe"` at `release.yml:595,605`
      already covers this. Verify.
- [ ] Task 13.6. **Update `fono-update`'s known-asset-set test fixtures**
      to include the `.exe` row.

**Phase 13 gate**: tagging a release on the main branch produces three
release assets (Linux CPU, Linux GPU, Linux aarch64, Windows CPU) plus
their `.sha256` sidecars and the `SHA256SUMS` manifest.

### Phase 14 — Promote Windows CI to gating + size budget

- [ ] Task 14.1. **Drop `continue-on-error: true` from Phase 2 row.**
      Windows CI now blocks PRs the same way Linux does. Pre-commit gate
      remains Linux-only (per AGENTS.md); CI is the safety net.
- [ ] Task 14.2. **Add Windows-side size budget.** PE-COFF doesn't have
      a NEEDED set in the ELF sense, but `dumpbin /dependents fono.exe`
      gives the DLL import table. Establish an allowlist:
      `KERNEL32.dll`, `USER32.dll`, `GDI32.dll`, `SHELL32.dll`,
      `ADVAPI32.dll`, `WS2_32.dll`, `BCRYPT.dll`, `ole32.dll`,
      `OLEAUT32.dll`, plus whatever WASAPI and tray-icon pull in.
      Document the allowlist in `ci.yml` with rationale, mirroring the
      Linux allowlist's comment block.
- [ ] Task 14.3. **Size budget on Windows: ~30 MiB ceiling.** Windows
      `.exe` will be larger than Linux due to PE-COFF overhead, embedded
      manifest, and MSVC CRT linkage. Pin a starting budget at 30 MiB;
      measure actual size and tighten in a follow-up if there's
      headroom.
- [ ] Task 14.4. **Update `CHANGELOG.md` and `ROADMAP.md`.** Move the
      "macOS + Windows" roadmap item into "Recently shipped" (Windows
      half). macOS stays in the on-the-horizon section.
- [ ] Task 14.5. **Update `README.md` install table** with a Windows
      row pointing at the `.exe` asset and a brief setup blurb.
- [ ] Task 14.6. **Tag a `vX.Y.Z` release** containing the first Windows
      asset.

**Phase 14 gate**: first official Windows release published; Windows CI
is gating; size budget enforced.

## Verification Criteria

- Linux `release-slim` binary size at end of Phase 14 is within +50 KB
  of pre-port baseline (today: ~21.17 MiB), accounting for IPC and
  locale dep additions.
- Linux `NEEDED` set at end of Phase 14 unchanged from today: exactly
  `libc.so.6 libm.so.6 libgcc_s.so.1 ld-linux-x86-64.so.2`.
- Linux test suite passes all 728+ tests at every phase boundary.
- Windows `.exe` size at end of Phase 14 under 30 MiB.
- Windows DLL imports under the Phase 14.2 allowlist.
- `fono setup → cloud-STT-key → hotkey-press → speak → text-injected`
  end-to-end works on Windows.
- `fono assistant → ask → hear reply` end-to-end works on Windows.
- `fono install / uninstall / update / doctor / use` CLI subcommands
  all work on Windows.
- Tray icon, overlay, and hotkey UX visually match the Linux experience
  to within reasonable platform-idiomatic differences.
- A Linux-only release (with the Windows CI row red) can still be
  tagged and published at any phase boundary before Phase 14.

## Potential Risks and Mitigations

1. **MSVC + vendored C++ build snags (whisper.cpp, llama.cpp, ggml).**
   Most likely Phase 3 surprises: CMake toolchain detection, OpenMP
   linkage, C++ ABI mismatches under clang-cl vs cl.
   Mitigation: Phase 3 budgets 2–3 days for snag resolution; lean on
   the existing `bogdanr/llama-cpp-rs` fork as the place to add
   Windows-specific build.rs branches if needed. Tag the fork commits
   so the pin in `Cargo.toml:191-192` stays explicit.

2. **OpenMP feature on Windows pulls `vcomp140.dll` into runtime deps.**
   Linux currently links libgomp statically (workspace
   `static-openmp` feature). On MSVC there is no equivalent static
   path.
   Mitigation: Phase 3.3 explicitly evaluates three options; the
   chosen one is documented as a Linux-vs-Windows asymmetry, not a
   regression.

3. **`tray-icon` crate pulls unexpected transitive deps on Windows that
   shadow into Linux via the workspace lockfile.**
   Mitigation: Phase 6.1 strictly uses `[target.'cfg(target_os = "windows")'.dependencies]`,
   never workspace-level deps. Verify with `cargo tree --target x86_64-unknown-linux-gnu`
   after each phase showing zero new entries on Linux.

4. **Windows OpenSSH server is flakey on consumer Windows builds.**
   Documented occasional drops; sshd refuses connections after sleep.
   Mitigation: document workarounds in `docs/build-windows.md`
   (disable Windows fast-startup, `Set-Service sshd -StartupType Automatic`).
   Fall back to `cargo-xwin` for inner-loop iteration if SSH is
   misbehaving on a given day.

5. **macOS path closes off** if all the Linux/Windows abstractions
   start drifting toward "the two OSes that exist".
   Mitigation: Phase 1's trait splits use generic OS-naming
   (`linux.rs`, `windows.rs`), never binary `unix.rs` / `windows.rs`
   bool-splits. macOS gets its own `macos.rs` file when its phase
   arrives. The selection tables already accommodate N>2.

6. **CI minute budget on Windows runners** is higher per minute than
   Linux; matrix expansion costs real money for private repos.
   Mitigation: public repo today; if it goes private later, consider
   self-hosted Windows runner on the same physical machine used for
   remote-dev SSH. Documented as a future tuning item.

7. **`global-hotkey` Windows backend has known minor issues** with
   modifier-only chords and some keyboard layouts.
   Mitigation: ship F7/F8 defaults that don't involve modifier-only
   chords; document layout-specific issues in Phase 8 docs.

## Alternative Approaches

1. **All-cross-compile via `cargo-xwin`, no SSH.** Faster inner loop, no
   Windows toolchain install needed. Trade-off: no way to actually run
   the resulting `.exe` for runtime smoke without manual transfer; can't
   exercise WASAPI, tray, overlay, or focus detection. Suitable as a
   *complement* to SSH (Phase 0.7) but not a replacement.

2. **GitHub Actions Windows runners as the sole Windows environment**,
   skipping the dedicated Windows host entirely. Cleaner setup
   (no SSH, no rsync, no Windows toolchain on the LAN), but iteration
   loop is 5–15 min per change. Viable for final polish phases (13–14);
   painful for Phases 3–10.

3. **WSL2-based "Windows" build.** Produces ELF binaries, not Windows
   `.exe`s. Doesn't address any of the actual cross-platform shims
   (WASAPI, tray, overlay). Rejected as non-viable.

4. **Self-hosted GitHub Actions Windows runner on the LAN Windows box.**
   Adds CI automation to the same Windows host used for SSH dev. Worth
   adding as a Phase 15 enhancement once the port is stable; not
   recommended as a v1 dependency because runner config drift becomes
   a third surface to maintain.

5. **Single unified audio crate (cpal everywhere) instead of dual
   parec/cpal split.** Explicitly rejected: workspace Cargo.toml and
   status.md document the cost (libasound NEEDED, latency regression
   on PipeWire-shim, distro packaging changes). Linux trade-off too
   high for the gain.

6. **Single unified tray crate (`tray-icon` everywhere).** Explicitly
   rejected: would re-introduce libgtk-3 et al. into the Linux NEEDED
   set and break the 20 MiB size budget. The workspace Cargo.toml
   comment block at lines 96-106 is the standing rationale.
