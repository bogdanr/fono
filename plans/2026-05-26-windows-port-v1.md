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

> **SUPERSEDED (2026-07-12):** the "CPU-only Windows v1, GPU variant
> deferred" decision below (Task 3.4, Phase 5.1, Phase 14.3) is
> superseded by
> `plans/2026-07-12-vulkan-soft-load-single-build-v1.md`. Windows now
> ships a **single Vulkan-accelerated `.exe` that soft-loads the Vulkan
> loader and falls back to CPU** when `vulkan-1.dll` (or a usable
> device) is absent — chosen for simplicity (one artefact, no variant
> plumbing). The same plan removes the hard `libvulkan.so.1` link from
> the Linux `fono-gpu` variant. Read that plan for the current
> direction; the Windows tasks below are historical up to that date.

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
      **REVERSED 2026-07-12:** Windows v1 will instead ship a single
      Vulkan-with-CPU-fallback `.exe`. See
      `plans/2026-07-12-vulkan-soft-load-single-build-v1.md`.
- [x] Task 3.5. **Linker success.** *(Done 2026-07-11 — via the native
      SSH loop, the Phase 0 reference toolchain; xwin cross-compile
      remains the deferred fast path.)* `fono.exe` LINKS and RUNS on
      `x86_64-pc-windows-msvc`: `target\debug\fono.exe --version` prints
      `fono 0.15.0`. Two link blockers were cleared to get here:
      (1) the ort `stdc++.lib` leak (Task 3.3), and (2) the duplicate
      ggml symbols across `whisper-rs-sys` + `llama-cpp-sys-2`, fixed
      with `/FORCE:MULTIPLE` (the MSVC analogue of GNU
      `--allow-multiple-definition`) in a new
      `[target.x86_64-pc-windows-msvc]` rustflags block in
      `.cargo/config.toml`. The 157-unresolved-`ort` externals blocker
      was sidestepped by building the ort-free `windows-defaults`
      feature set (see Task 5.1). Binary size not yet measured against a
      budget (debug build is 58 MB; release-slim + budget is Phase 14).
- [x] Task 3.6. **Verify native build over SSH matches.** *(Done
      2026-07-11.)* `scripts/win-remote.sh build -p fono
      --no-default-features --features windows-defaults` (rsync +
      remote `cargo build`) produces a working `fono.exe`. Native is now
      the reference toolchain; xwin stays the fast-iteration shortcut.
      Note: the dev Windows box sleeps aggressively — long builds may
      need a resume or two, but the remote `target/` cache persists so
      each resume continues from where it stopped.

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

- [x] Task 5.1. **Make `cpal-backend` feature default on Windows.**
      *(Done 2026-07-11.)* Added a
      `[target.'cfg(target_os = "windows")'.dependencies]` block to
      `crates/fono/Cargo.toml` enabling `fono-audio`'s `cpal-backend`
      (mirrors the existing macOS block), so the WASAPI capture/playback
      path compiles and links on Windows. Linux stays on parec
      (byte-identical — target tables don't unify off-target). Also added
      a `windows-defaults` feature to the `fono` crate: the shippable
      Windows v1 set, identical to the Linux default MINUS `tts-local`
      and `wakeword-onnx` (the only two features that pull the `ort`
      static lib, which does not yet link on MSVC — its
      protobuf/abseil/onnx/cpuinfo deps are unresolved). Windows builds
      pass `--no-default-features --features windows-defaults`; local
      whisper STT and local llama polish (no `ort`) are kept, so v1 is
      cloud + local STT + local polish, cloud TTS — just no local TTS or
      wake-word until a merged static `onnxruntime.lib` is provisioned.
- [x] Task 5.2. **Verify WASAPI capture round-trip (device detection).**
      *(Partial 2026-07-11.)* `fono.exe doctor` over SSH enumerates the
      Windows default input device ("CABLE Output (VB-Audio Virtual
      Cable)") via cpal/WASAPI — the capture backend initialises and lists
      devices. **Still manual/pending** (needs a human at the box + a
      cloud STT key + Windows text injection from Phase 7): `fono setup`,
      configure Groq, press hotkey, speak, confirm transcript lands at
      the cursor.
- [ ] Task 5.3. **Verify WASAPI playback round-trip.** Manual smoke:
      configure assistant, press F8, ask question, hear reply through
      default output device. (Pending human-at-box.)
- [x] Task 5.4. **Microphone enumeration on Windows.** *(Done
      2026-07-11 — backend level.)* `fono.exe doctor` lists Windows audio
      inputs through cpal's `HostTrait::input_devices()`; wiring the list
      into the tray Microphone submenu lands with the Windows tray in
      Phase 6.

**Phase 5 gate**: end-to-end voice → cloud STT → injected text works on
Windows. Linux audio path unchanged and verified by Linux smoke test.

### Phase 6 — Tray icon on Windows (`tray-icon` crate)

- [x] Task 6.1. **Add `tray-icon` to Windows-only deps.** In
      `crates/fono-tray/Cargo.toml`:
      `[target.'cfg(target_os = "windows")'.dependencies] tray-icon = "0.20"`.
      Linux stays on ksni; tray-icon never touches the Linux dep graph.
      Also added a Windows-only `windows-sys` edge (already in the lock
      via cpal, net-zero) for the Win32 message pump. `muda` is reached
      transitively through `tray_icon::menu`, so `tray-icon` is the only
      new-to-project crate — MIT/Apache-2.0, already allowlisted, and
      `deny.toml` already carried the gtk/libappindicator advisory
      ignores from tray-icon's earlier stint as the Linux backend.
- [x] Task 6.2. **Implement the Windows tray backend behind the Phase 1.1
      seam.** `crates/fono-tray/src/backend_windows.rs` renders the shared
      `menu::build` node tree via `tray-icon` + `muda`, wired into the
      `spawn` dispatch in `lib.rs` exactly like `backend_linux`/
      `backend_macos`. A dedicated `fono-tray` OS thread owns the
      (`!Send`) `TrayIcon` and runs a `PeekMessageW` pump (Windows allows
      any thread, not just main — so no `fono::main` change, unlike
      macOS); the tokio poll task keeps the same 2 s snapshot-diff cadence
      and ships `MenuNode` trees over a channel. Full menu parity (backend
      submenus, mic, preferences, servers) is automatic — it's the same
      model every backend consumes.
- [x] Task 6.3. **Tinted icon for Windows.** Deviation from the original
      "embed `assets/fono.png`" plan: that asset never existed, and the
      Linux/macOS backends already generate the icon **in code** from
      `menu::state_color`. The Windows backend does the same — a 32×32
      RGBA state-tinted circle via `Icon::from_rgba` — so no PNG/`image`
      crate is pulled in and the icon colour language stays identical
      across all three platforms.
- [x] Task 6.4. **Verify Linux tray unchanged.** tray-icon is Windows-only,
      so the Linux `fono` binary is byte-for-byte identical (target tables
      don't unify off-target). Confirmed via green `cargo clippy`/`cargo
      test` on Linux and the earlier size gate; ksni symbols and the
      NEEDED list are untouched. `fono-tray` + full `fono.exe` both
      compile and link on `x86_64-pc-windows-msvc`, and `fono.exe
      --version` runs with the tray backend linked in.

**Phase 6 gate**: tray icon appears in Windows notification area with
correct menu structure. Linux ksni tray unchanged. *Compile/link/run
side verified over SSH; the visual "icon appears in the notification
area" check needs an interactive Windows desktop session (not headless
SSH) and is handed to the user.*

### Phase 7 — Text injection on Windows (enigo)

- [x] Task 7.1. **Enable enigo on Windows builds.** Done via the same
      pattern as macOS (Task 6.1): the `fono` crate enables
      `fono-inject/enigo-backend` in its
      `[target.'cfg(target_os = "windows")'.dependencies]` block rather
      than depending on `enigo` directly (keeps the backend behind the
      crate's feature seam). No libxdo on Windows — enigo calls Win32
      `SendInput`, and its Windows deps (`windows 0.56`) were already in
      `Cargo.lock`, so **zero** new-to-project crates and byte-identical
      Linux/macOS binaries.
- [x] Task 7.2. **`Injector::detect_auto` already returns `Enigo` on
      Windows.** Discharged in Phase 1's cfg refactor (Task 1.3): the
      `#[cfg(not(target_os = "linux"))]` arm of `detect_auto` returns
      `Self::Enigo` when `enigo-backend` is on (else `None`), with no
      Wayland/X11 probes. Confirmed live: `fono.exe doctor` reports
      `Injector : Enigo`.
- [ ] Task 7.3. **Verify against three target Windows apps**: Notepad,
      Chrome address bar, Discord/Slack chat input. Each should receive
      the dictated text via SendInput without focus stealing. *(Manual —
      needs a human at the interactive desktop; over headless SSH there
      is no focused window to type into. `Injector : Enigo` is selected;
      handed to the user for the visual smoke.)*
- [x] Task 7.4. **Clipboard fallback works on Windows.** No new dep
      needed: `fono-inject`'s non-optional `arboard` dep speaks the
      Win32 clipboard natively. Confirmed live: `fono.exe doctor`
      reports `Clipboard : native (arboard)`. Bonus: gated the doctor's
      Linux-only clipboard-manager probe (ICCCM/`XTEST`/clipit&c.) under
      `cfg(target_os = "linux")` so its X11-specific guidance no longer
      leaks into Windows/macOS `doctor` output.

**Phase 7 gate**: text injection works in three reference apps; Linux
inject cascade unchanged.

### Phase 8 — Hotkeys on Windows (`global-hotkey`)

- [x] Task 8.1. **`global-hotkey` already cross-platform.** Confirm
      Windows MSVC build picks up the Win32 `RegisterHotKey` backend.
      Smoke test: register F7, press F7, verify the listener emits
      `TogglePressed`.
      *Done 2026-07-12. `detect_backend` was resolving to `Disabled` on
      Windows (it only knew the Linux `DISPLAY`/`WAYLAND_DISPLAY`
      signals); generalised the macOS special-case to all non-Linux
      desktop targets so Windows resolves to the `global-hotkey`
      listener. Also fixed `is_graphical_session` (same Linux-env-var
      blind spot) so the daemon no longer skips the listener as
      "headless" on Windows, and fixed a Windows-only main-thread stack
      overflow (1 MiB MSVC default) by running the entry point on a
      big-stack worker thread. Confirmed live over SSH: the daemon logs
      `hotkey backend resolved: X11` and reaches the `RegisterHotKey`
      call. The registration itself returns `os error 1459` over
      headless SSH (non-interactive window station); the actual
      key-press round-trip is the manual desktop gate.*
- [x] Task 8.2. **Default hotkeys reasonable on Windows.** F7/F8 work but
      conflict with some apps' built-in shortcuts. Document in
      `docs/build-windows.md` that users can rebind via `fono use hotkey`.
      No behavioural change vs Linux for v1.
      *Done 2026-07-12. Documented in the new "Hotkeys and the daemon on
      Windows" section of `docs/build-windows.md`.*
- [x] Task 8.3. **Esc-to-cancel on Windows.** Use the same dynamic
      `EnableCancel` / `DisableCancel` machinery; register Esc transiently
      via `global-hotkey` only during active recording. Mirrors the v0.8.2
      Linux behaviour via a different backend.
      *Done 2026-07-12. No code change needed — `listener.rs` drives the
      Esc-cancel machinery entirely through `global_hotkey`'s
      cross-platform `manager.register`/`unregister`, which resolve to
      the Win32 backend on Windows.*
- [x] Task 8.4. **Verify no Linux portal regression.** The
      `#[cfg(target_os = "linux")] mod portal` import stays intact; Linux
      portal-based Esc cancel still works on KDE-Wayland / sway / Hyprland.
      *Done 2026-07-12. The non-Linux branch is additive; Linux
      `detect_backend` tests and the full suite stay green.*

**Phase 8 gate**: hotkey press starts/stops recording on Windows;
Esc-to-cancel works; Linux Wayland Esc-portal flow unchanged.

### Phase 9 — Focus detection on Windows (Win32 foreground window)

- [x] Task 9.1. **Define `FocusBackend` trait.** Extract today's `detect_focus`
      free function in `crates/fono-inject/src/focus.rs` behind a trait.
      Linux impl uses `x11rb`; Windows impl uses `windows-sys` crate's
      `GetForegroundWindow` + `GetWindowThreadProcessId` + executable name
      lookup via `QueryFullProcessImageNameW`.
      *Done 2026-07-12. Kept the established `#[cfg]`-gated function
      dispatch inside `detect_focus()` rather than inventing a
      `FocusBackend` trait — the free-function-per-OS seam is already how
      macOS/Linux are split, and it is the same call the tray backend
      decision (Phase 1) made. Added `windows_focus()` +
      `windows_process_exe_name()` using `GetForegroundWindow` /
      `GetWindowTextW` / `GetWindowThreadProcessId` /
      `QueryFullProcessImageNameW`. Returns the bare exe name (e.g.
      `chrome.exe`) as `window_class`; degrades to empty `FocusInfo` when
      there is no foreground window.*
- [x] Task 9.2. **Windows-only dep.** Add
      `[target.'cfg(target_os = "windows")'.dependencies] windows-sys = { version = "0.59", features = ["Win32_UI_WindowsAndMessaging", "Win32_System_Threading", "Win32_System_ProcessStatus"] }`.
      Confirms zero Linux impact.
      *Done 2026-07-12. Used features `Win32_Foundation`,
      `Win32_UI_WindowsAndMessaging`, `Win32_System_Threading`
      (`QueryFullProcessImageNameW` lives in Threading, so
      `Win32_System_ProcessStatus` was not needed). `windows-sys 0.59`
      was already in `Cargo.lock` via cpal / fono-tray, so this is a
      new edge only — no new-to-project crate, zero Linux/macOS binary
      cost (the lockfile diff is a single dependency line).*
- [x] Task 9.3. **Per-app context rules on Windows.** The classifier in
      `fono-inject/src/classifier.rs` matches on app names / process names;
      ensure the Windows focus probe returns names in a form the classifier
      can match. Add Windows-flavoured rules for known apps (e.g.
      `chrome.exe`, `code.exe`, `WindowsTerminal.exe`).
      *Done 2026-07-12. Added Windows `.exe` entries to every built-in
      rule (terminals, editors, browsers, email, chat, spreadsheets,
      documents, private apps), each gated `#[cfg(target_os = "windows")]`
      on the individual array element so the Linux/macOS binary is
      byte-for-byte unchanged. A Windows-gated unit test
      (`windows_exe_names_classify`) confirms chrome.exe → Browser,
      Code.exe → CodeEditor, WindowsTerminal.exe → Terminal, KeePassXC.exe
      → history-suppressed; it passes on the Windows box.*

**Phase 9 gate**: `fono doctor` on Windows reports the current focused
window's app name. Per-app rules fire correctly for at least three test
apps. Linux x11rb path unchanged.
*Gate 2026-07-12: `doctor` gained a cross-platform `Focus` line (shows
the focused app's exe/class and the matched profile). Over headless SSH
it reads "none detected" — live population is a manual desktop check,
like the tray/hotkey/typing smokes. The three-app rule firing is proven
by the passing Windows unit test. Linux gate green; x11rb path
untouched.*

### Phase 10 — Overlay backend on Windows (Win32 layered toolwindow)

- [x] Task 10.1. **New backend file `crates/fono-overlay/src/backends/windows.rs`.**
      Sets `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOPMOST |
      WS_EX_LAYERED` and excludes from Alt+Tab via `WS_EX_TOOLWINDOW`.
      **Deviation from plan:** dropped winit+softbuffer in favour of a
      pure-Win32 layered window updated via `UpdateLayeredWindow` from the
      shared renderer's premultiplied-ARGB framebuffer. Rationale: softbuffer
      blits through GDI `BitBlt`, which ignores per-pixel alpha, so the
      rounded-corner transparency the renderer produces would be lost;
      `UpdateLayeredWindow` with `AC_SRC_ALPHA` is the only path that honours
      it. Structurally mirrors the macOS worker-thread backend (dedicated
      thread owns the HWND + message pump; snapshot channel from the app).
- [x] Task 10.2. **Extend `BackendId` enum and selection table.** Added
      `BackendId::Win32LayeredToolWindow` + `HostOs::Windows`.
      `candidate_list_with` returns `[Win32LayeredToolWindow, Noop]` on
      Windows; Linux/macOS tables unchanged (verified by
      `pick_backend_with` unit tests). `doctor` reports the selected
      backend and its capabilities.
- [~] Task 10.3. **Click-through and focus passthrough (code-complete;
      visual check pending).** `WS_EX_TRANSPARENT` (click-through) +
      `WS_EX_NOACTIVATE` (focus-passthrough) are set and reported by
      `doctor` (`focus-passthrough=yes click-passthrough=yes`). The live
      click-through / keystroke-lands-in-field check needs an interactive
      desktop — handed to the manual desktop gate.
- [x] Task 10.4. **Multi-monitor positioning.** Anchors to the primary
      monitor's bottom-centre via `GetSystemMetrics(SM_CXSCREEN/SM_CYSCREEN)`,
      mirroring the Linux bottom-centre placement, with the same bottom
      offset as the other backends.
- [x] Task 10.5. **`FONO_OVERLAY_BACKEND` env override works on Windows.**
      Aliases `win32` / `windows` / `win` / `layered` and `noop`. Verified
      live over SSH (`win32` → layered tool-window, `noop` → noop).
      `parse` now trims surrounding whitespace so a stray trailing space
      from cmd.exe `set VAR=win32 ` doesn't defeat the override.

**Phase 10 gate**: overlay paints during recording on Windows with
correct anchoring; doesn't steal focus; doesn't appear in Alt-Tab. Linux
wlr-layer-shell / X11 / noop backends unchanged.

### Phase 11 — Install and autostart on Windows

- [x] Task 11.1. **`Installer::windows` impl behind the trait from Phase 1.6.**
      Default install location: `%LOCALAPPDATA%\fono\fono.exe`. Autostart:
      write `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\fono`
      pointing to the install path. **Deviation:** used the built-in
      `reg.exe` via subprocess rather than the `winreg` crate — mirrors
      the macOS installer's `launchctl`/`security` subprocess style, needs
      no `unsafe` FFI, and keeps the binary dependency-free (binary size
      is the top priority, and the `winreg` crate would have been new to
      the graph). Realised in `crates/fono/src/install/windows.rs`.
- [x] Task 11.2. **`sudo fono install` becomes `fono install` on Windows.**
      No elevation needed for `HKCU`/`%LOCALAPPDATA%` writes. `--server` is
      refused with a Linux-only message (no Windows service install in v1).
- [x] Task 11.3. **`fono uninstall` on Windows.** Removes the registry
      Run value and the `%LOCALAPPDATA%\fono\` directory, leaves user
      config under `%APPDATA%\fono\` intact. Verified over SSH: reg value
      + install dir gone, config path untouched.
- [x] Task 11.4. **Install marker on Windows.** Writes
      `%LOCALAPPDATA%\fono\install_marker.toml` (version + install path +
      unix timestamp), surfaced by `fono doctor`. Verified: valid TOML with
      a backslash-escaped path.
- [x] Task 11.5. **Verify Linux install path unchanged.** Linux installer
      is a separate cfg-gated module (`install/linux.rs`) untouched by this
      phase; the only shared edit is `install/mod.rs` module wiring. Full
      Linux gate (fmt/clippy/tests) green. (Live `sudo fono install
      --server` on a Linux host remains a manual check.)

**Phase 11 gate**: `fono install` on Windows copies the binary, writes
the registry autostart entry, and the daemon starts on next login.
`fono uninstall` reverses cleanly. Linux install layer behaviour
byte-identical.

### Phase 12 — `fono update` on Windows (rename-and-relaunch)

- [x] Task 12.1. **Asset name lookup uses `current_asset_name()` from
      Phase 1.7.** Confirmed: `current_asset_name()` returns
      `fono-vX.Y.Z-x86_64.exe` on Windows. `fono update --check` on the
      box exercises the path and correctly reports "no matching release
      asset" (no Windows release published yet — expected until Phase 13).
- [x] Task 12.2. **Self-replacement via rename trick.** The existing
      cross-platform swap in `apply_update` already does the
      rename-into-place dance (download to temp in the same dir → verify
      SHA-256 → `rename(old → old.bak)` → `rename(tmp → old)`), which
      works on Windows because a running `.exe` *can* be renamed even
      though it can't be overwritten. Added the `#[cfg(windows)]`
      `restart_in_place`: since Windows has no `execv`, it spawns the
      freshly-installed binary as an independent child (inheriting stdio
      + argv) and exits to release the renamed old image. The running
      image ends up at the sibling `.bak`, cleaned up on next
      `fono update`. (Uses `.bak`, the existing codebase convention,
      not the illustrative `.exe.old` from this note.)
- [x] Task 12.3. **Package-managed detection.** Added a `#[cfg(windows)]`
      branch to `is_package_managed`: no system package manager on
      Windows, so a per-user install under `%LOCALAPPDATA%\fono\` stays
      self-updatable, while an install under `Program Files` /
      `Program Files (x86)` is treated as managed (refuse up front with a
      clear message rather than fail mid-swap on access-denied).
      Case-insensitive match. `elevation_hint()` gives a Windows-
      appropriate message (reinstall with `fono install`) instead of
      `sudo`.
- [x] Task 12.4. **Verify Linux update path unchanged.** Linux
      `fono-update` tests all green (15 passed), CPU↔GPU asset selection
      and rename(2)-based replace untouched (non-Windows branches
      unchanged; the Unix-specific `pkg_managed_paths` test still runs
      and passes on Linux).

**Phase 12 gate**: `fono update` on Windows downloads, verifies, and
replaces the running `.exe` atomically. Linux update flow unchanged
including the v0.5.0 CPU↔GPU auto-switching.

### Phase 13 — Release workflow: Windows artefact

- [x] Task 13.1. **Add Windows row to `release.yml` build matrix.**
      Added `x86_64-pc-windows-msvc` on `windows-2022`, variant cpu,
      asset `fono-vX.Y.Z-x86_64.exe`. Build uses `--profile release-slim
      --target x86_64-pc-windows-msvc -p fono --no-default-features
      --features windows-defaults` (a new `no_default_features` matrix
      key drives the flag; the Build step is now `shell: bash` so the
      arg-assembly runs under Git Bash on the runner). Added the two
      Windows-only prep steps mirrored from the ci.yml windows job:
      `git config --system core.longpaths true` before checkout, and
      `LIBCLANG_PATH=C:\Program Files\LLVM\bin`. Verified over SSH: the
      exact `release-slim` build command links a working `fono.exe`
      (16,443,392 B ≈ 15.7 MiB, `--version` prints `fono 0.15.0`).
- [x] Task 13.2. **Skip the ELF NEEDED verification step on Windows.**
      Confirmed: the "Verify Linux binary NEEDED set" step gates on
      `runner.os == 'Linux'`, and the macOS dylib check on
      `runner.os == 'macOS'` — both skip on Windows.
- [x] Task 13.3. **Stage the `.exe` for upload.** The staging step
      already appends `.exe` (`bin_src` + asset name) via
      `if [[ "${target}" == *windows* ]]`; the bare-binary artifact
      (`fono-bin-cpu-x86_64-pc-windows-msvc`) uploads `out-bin/*`.
      Additionally excluded Windows from the internal distro-staging
      tarball (build guard + upload `if`), since it ships bare `.exe`
      only and its `x86_64` arch label would otherwise clash with the
      Linux staging stem.
- [x] Task 13.4. **No Windows packaging job** — ships bare `.exe` plus
      `.sha256` sidecar only. No MSI / signing (explicit non-goals);
      no distro-style package job added.
- [x] Task 13.5. **`SHA256SUMS` includes the Windows asset.** Confirmed:
      the checksum `find` includes `-name "fono-v*-x86_64.exe"` and the
      per-asset sidecar loop lists `fono-v*-x86_64.exe`, so the `.exe`
      gets both a `SHA256SUMS` line and its own `.sha256` sidecar.
- [x] Task 13.6. **Update `fono-update`'s known-asset-set test fixtures**
      to include the `.exe` row. Already satisfied in Phase 1.7:
      `asset_name_has_exe_suffix_on_windows` asserts the `.exe` suffix +
      CPU-only prefix; `asset_name_for` returns
      `fono-vX.Y.Z-x86_64.exe` on Windows. No other asset-list fixture
      exists.

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
      **REVISED 2026-07-12:** the single Vulkan-with-fallback `.exe`
      (Task 3.4 reversal) carries the ~42 MB SPIR-V shader payload, so
      the Windows budget rises to ~60 MiB. See
      `plans/2026-07-12-vulkan-soft-load-single-build-v1.md`.
      **REVISED 2026-07-14:** enabling local TTS + wake-word on Windows
      (embedded ONNX Runtime) added ~3 MiB, to ~72 MiB, so the budget
      was raised to enforced ≤ 75 MiB / hard cap ≤ 80 MiB (ADR 0022,
      2026-07-14 amendment). The Phase 14 gate asserts ≤ 75 MiB.
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
