# Building Fono for Windows

Status: Phase 0 of `plans/2026-05-26-windows-port-v1.md` (remote dev
environment) is complete and verified end-to-end. Phase 1 (the
Linux-only trait-split refactor that makes IPC/inject/etc. compile on
Windows) has not started — see `docs/status.md` for session history.
This document describes the remote-Windows development loop and the
toolchain gotchas the design plan didn't originally call out.

## Dev host

- Windows 10 (build 19045/22H2 confirmed working; the design plan's
  1809+ baseline holds), 64-bit, reachable over LAN via OpenSSH Server
  with key-only auth (`PasswordAuthentication no`).
- Everything lives under `C:\fono-dev\fono` (rsync target mirroring the
  Linux working tree) so there is one place to look, though — unlike
  the macOS sandbox — the toolchain itself (VS Build Tools, LLVM, Rust,
  MSYS2) installs into normal system locations, not a self-contained
  prefix; Windows doesn't make relocatable dev toolchains easy the way
  `rustup`/standalone-CMake do on Unix.

## One-time Windows-box setup

Beyond what the design plan's Phase 0 already lists (OpenSSH, key auth,
VS Build Tools "Desktop development with C++", Rust via rustup,
rsync), three gotchas surfaced only by actually running a full build:

1. **libclang is missing.** VS Build Tools does **not** bundle
   `libclang.dll`, but `bindgen` (used by `llama-cpp-sys-2` and
   `whisper-rs-sys`) needs it. Install standalone LLVM
   (`https://github.com/llvm/llvm-project/releases`, the
   `LLVM-*-win64.exe` NSIS installer, `/S /D=C:\LLVM` for a silent
   install) and set `LIBCLANG_PATH=C:\LLVM\bin` **system-wide**
   (`setx LIBCLANG_PATH "C:\LLVM\bin" /M`), plus add `C:\LLVM\bin` to
   the system `Path`.
2. **The VS-bundled CMake isn't on PATH outside a Native Tools
   prompt**, which a plain SSH session never is. Add
   `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`
   to the system `Path` explicitly.
3. **`MAX_PATH` (260 chars).** The vendored `llama.cpp` git submodule
   checkout has paths that exceed the legacy Windows path limit and
   the clone fails with `path too long`. Fix both sides of the stack:
   `git config --global core.longpaths true`, **and**
   `LongPathsEnabled=1` under
   `HKLM\SYSTEM\CurrentControlSet\Control\FileSystem` (`reg add` from
   an elevated prompt) — git's own setting alone is not sufficient,
   the Win32 loader-level opt-in is also required. No reboot needed;
   it's read at process-creation time.
4. **The Vulkan SDK is required.** `windows-defaults` includes
   `accel-vulkan`, so ggml-vulkan's CMake needs the Vulkan headers,
   the `vulkan-1.lib` import library, and the `glslc` SPIR-V compiler
   at build time — and `whisper-rs-sys`' `build.rs` panics unless
   `VULKAN_SDK` is set. Install LunarG's SDK
   (`https://sdk.lunarg.com/sdk/download/<ver>/windows/vulkansdk-windows-X64-<ver>.exe`)
   silently — `installer.exe --root C:\VulkanSDK\<ver>
   --accept-licenses --default-answer --confirm-command install` —
   and set `VULKAN_SDK=C:\VulkanSDK\<ver>` plus add its `Bin` to
   `Path`. Unlike the VS Build Tools installer, the LunarG installer
   *does* run headless over SSH. The version is pinned in
   `.github/workflows/ci.yml` and `release.yml`; keep the box in
   lockstep. Note the shipped `vulkan-1.dll` loader itself is **not**
   a build dependency — it is installed by the GPU vendor driver and
   loaded lazily at runtime (see "Vulkan single build" below).

None of the above needs a GUI session — all three were done, and
verified, entirely over SSH (`reg add` to `HKLM` does not trigger a
UAC consent prompt the way running an installer does; see the
`vs_buildtools.exe` note below).

One thing that **does** need a human at the keyboard: the VS Build
Tools installer itself refuses to run under an SSH-invoked or
`SYSTEM`-context scheduled-task process (exit code 87 either way —
OpenSSH sessions carry a UAC-filtered token even for admin accounts).
Download it to the box, then double-click it locally and pick
"Desktop development with C++".

## Remote development loop

Development happens on the Linux workstation; the Windows box is the
build/test bench, driven by `scripts/win-remote.sh`:

```sh
export FONO_WIN_HOST=<user@host>     # never stored in the repo
scripts/win-remote.sh check          # rsync tree + cargo check --workspace
scripts/win-remote.sh test           # rsync tree + workspace tests
scripts/win-remote.sh build -p fono
scripts/win-remote.sh sh 'cargo --version & cl.exe /?'
```

The Windows box's address and credentials are deliberately kept out of
the repository (mirrors the macOS plan's guiding constraint):
`FONO_WIN_HOST` comes from your shell environment or an untracked
local file. The script errors out when it is unset.

`push` mirrors the working tree into `C:\fono-dev\fono` with the same
`.gitignore`-aware `--delete` policy as `scripts/mac-remote.sh`,
explicitly protecting `target/` from deletion. `check`/`build`/`test`/
`cargo` additionally run `scripts/fetch-onnxruntime.sh` remotely (via
MSYS bash, which has `curl`/`xz`/`sha256sum` — a bare `cmd.exe` session
does not) to resolve `ORT_LIB_LOCATION`; it's a pinned-and-cached
download, so this is a fast no-op after the first run.

### cmd.exe quoting gotcha

`ssh win 'set VAR=value && next-command'` silently breaks: `cmd.exe`'s
`set` has no implicit trim, so the space before `&&` becomes part of
the value (`VAR` ends up as `"value "`, with a trailing space, which
then fails to match anything downstream). Always quote:
`set "VAR=value" && next-command`. `scripts/win-remote.sh` does this
already; keep it in mind for any ad-hoc `sh` invocations.

### Cross-compiling from Linux (fast local iteration)

`cargo-xwin` lets most syntax/type-level iteration happen without
touching the Windows box at all:

```sh
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin
cargo xwin build --target x86_64-pc-windows-msvc -p fono-core
```

This only catches compile errors — no linking against the real MSVC
runtime libraries beyond what `xwin` vendors, and obviously no
execution. Confirm anything cross-compiled against `cargo-xwin` with a
real `scripts/win-remote.sh build` before trusting it.

## What's proven end-to-end (2026-07-06)

All of this ran over SSH with the setup above, no GUI session:

- `fono-core` — including the `llama-local` feature (embedded
  llama.cpp via `llama-cpp-sys-2`, MSBuild + cmake generator) —
  **builds cleanly**, natively, on `x86_64-pc-windows-msvc`.
- The pinned static `onnxruntime.lib` for `x86_64-pc-windows-msvc` is
  already hosted (`scripts/fetch-onnxruntime.sh` has a row for the
  triple) and downloads/verifies/links correctly — `ort-sys` links
  against it with no further configuration needed.
- `scripts/win-remote.sh push/check/build` round-trip correctly:
  rsync-over-SSH, remote MSYS bash for the onnxruntime fetch, and
  `cmd.exe`-invoked `cargo` all work together.

### A real cross-platform bug found and fixed

Building `fono-core` with `llama-local` first failed with a type
mismatch in `crates/fono-core/src/brain_tap.rs` comparing
`(*tensor).type_` (a bindgen-generated alias for `enum ggml_type`'s C
underlying integer type) against locally-declared `u32` constants.
This isn't a bindgen bug: the Itanium C++ ABI (Linux/macOS) lets the
compiler pick `unsigned int` for an all-non-negative enum, while the
Microsoft ABI (Windows/MSVC) always uses `int` — so the same C header
produces a `u32` alias on Linux and an `i32` alias on Windows. Fixed
by comparing through `i64` (`ggml_type_is()`, widening both sides via
`i64::from`) instead of direct equality against a fixed-signedness
constant — portable regardless of which ABI's bindgen output is in
play. Verified: all 7 `brain_tap` tests pass on Linux;
`fono-core --features llama-local` now builds clean on both platforms.

### Phase 1 boundary confirmed (expected, not a setup problem)

`cargo build/check -p fono` (the full binary) fails past `fono-core`
at exactly the crates the design plan's Phase 1 trait-split targets:

- `fono-ipc`: `crates/fono-ipc/src/lib.rs:10` unconditionally imports
  `tokio::net::{UnixListener, UnixStream}` — no Windows named-pipe path
  exists yet.
- `fono-inject`: `crates/fono-inject/src/focus.rs:189` unconditionally
  imports `std::os::unix::net::UnixStream` (the sway/i3 IPC probe) —
  needs a `#[cfg(unix)]` (or `target_os = "linux"`) gate plus a
  Windows no-op/win32 stub, following the pattern already used
  elsewhere in `fono-inject` (`wayland_probe.rs`, `terminal.rs`) for
  Linux-only functionality.

Both are exactly the shape of change Phase 1 describes (move
Linux-specific code behind a platform gate, add a Windows sibling
later) — this is the toolchain doing its job, not a gap in the
environment setup.

## CI: the non-blocking `windows` job (Phase 2)

`.github/workflows/ci.yml` has a `windows` job (`windows-2022` runner,
`continue-on-error: true`) added by Windows port plan Phase 2. It
builds and tests the shippable **`windows-defaults`** feature set
(`cargo build -p fono --no-default-features --features
windows-defaults`, then the matching `cargo test`), which links
cleanly as of Phase 5 (plan Tasks 3.3/3.5/5.1). It stays
`continue-on-error` — progress-surfacing only, never blocking the
Linux/macOS pipeline — and will be promoted to a blocking gate when the
Windows release artefact ships (plan Phase 13/14), the same way the
macOS job was promoted in its Phase 12.

The job encodes the Phase 0 environment findings for the hosted
runner: git-side long paths are enabled before checkout (the vendored
llama.cpp git dependency exceeds legacy `MAX_PATH`) and `LIBCLANG_PATH`
points at the image's preinstalled standalone LLVM (VS Build Tools
ships no `libclang.dll`). Since 0.16 `windows-defaults` includes
`tts-local` and `wakeword-onnx`, so the job pins `ORT_LIB_LOCATION` to
the SHA-verified merged `onnxruntime.lib` from the fono-voice mirror
(same fetch script as every other row) and blanks `ORT_CXX_STDLIB`.
There is no Windows size / import-table gate yet — the PE/dumpbin
analogue of the Linux ELF `NEEDED` check and macOS dylib allowlist is
deferred to plan Phase 14.

## Link-stage findings (Phase 3)

As of 2026-07-11 the `fono` binary **links and runs** on
`x86_64-pc-windows-msvc`: `target\debug\fono.exe --version` prints
`fono 0.15.0`, and `fono.exe doctor` enumerates the WASAPI default
input device. Getting there cleared three distinct `link.exe`-stage
failures, in the order they surfaced:

1. **`LNK1181: cannot open input file 'stdc++.lib'` — fixed.**
   `.cargo/config.toml` sets `ORT_CXX_STDLIB=static:-bundle=stdc++` so
   the Linux-gnu ship binary keeps its four-entry `NEEDED` allowlist.
   Cargo's `[env]` table is **not** target-scoped, so that value also
   reaches MSVC, where `ort-sys` turns it into a `-lstdc++` — but there
   is no `libstdc++` on MSVC (the CRT is Microsoft's). An **empty**
   `ORT_CXX_STDLIB` makes `ort-sys` fall back to its correct MSVC
   default: no explicit C++ stdlib link (the MSVC CRT is pulled in
   automatically). Because `cmd.exe` cannot hold an empty-valued
   variable, the two Windows entry points neutralise it differently
   but equivalently:
   - CI `windows` job: `echo "ORT_CXX_STDLIB=" >> "$GITHUB_ENV"`.
   - `scripts/win-remote.sh`: passes `--config env.ORT_CXX_STDLIB=''`
     (a TOML literal empty string; single quotes survive `cmd.exe`).

   Note `llama-cpp-sys-2` is already MSVC-aware here — it gates its
   `gomp`/OpenMP link on `gnu` targets and links the MSVC CRT (not
   `stdc++`) on Windows — so the anticipated OpenMP-on-MSVC problem
   (plan Task 3.3) never materialised; `ort-sys` was the sole offender.

2. **`LNK1120: 157 unresolved externals` from `libort_sys` —
   sidestepped at first, properly fixed in 0.16.** With the `stdc++`
   link fixed, the link proceeded to onnxruntime and failed on
   protobuf / abseil / onnx / cpuinfo symbols: the then-hosted Windows
   `onnxruntime.lib` was not self-contained the way the Linux
   `libonnxruntime.a` is. Windows initially shipped the ort-free
   `windows-defaults` set (default minus `tts-local`/`wakeword-onnx`).
   As of 0.16 fono-voice publishes a merged MinSizeRel archive (all
   FetchContent deps included, MSVC debug info stripped), and
   `crates/fono/build.rs` places `onnxruntime.lib` on the link line a
   second time so MSVC's single-pass `link.exe` can resolve the
   archive's intra-archive cycles — `windows-defaults` now includes
   `tts-local` and `wakeword-onnx`, and both CI and the release build
   pin `ORT_LIB_LOCATION` to the verified lib.

3. **`LNK2005 ... already defined` → `LNK1169: multiply defined
   symbols` (duplicate ggml) — fixed.** `whisper-rs-sys` and
   `llama-cpp-sys-2` each statically build their own copy of ggml,
   whose quantise/dequantise helpers (`quantize_row_q*_ref`,
   `quantize_tq*`, …) are plain external C symbols. `link.exe` cannot
   fold those the way it folds duplicate C++ COMDATs. This is the exact
   MSVC analogue of the GNU-ld case that `.cargo/config.toml` already
   handles with `-Wl,--allow-multiple-definition`; the MSVC fix is
   `/FORCE:MULTIPLE`, added in a new `[target.x86_64-pc-windows-msvc]`
   rustflags block. Both ggml copies are pinned to the same upstream
   family so the surviving definition is ABI-compatible (ADR 0018).
   With this in place the final `fono.exe` link succeeds.

4. **`error C1083: ... '': Invalid argument` — the compiler probe —
   fixed.** The same `[env]`-is-not-target-scoped gap that bit
   `ORT_CXX_STDLIB` (item 1) also applies to the GNU/Clang size flags
   `.cargo/config.toml` sets for the Linux ship binary:
   `CFLAGS`/`CXXFLAGS = "-Os -ffunction-sections -fdata-sections
   -fno-asynchronous-unwind-tables -fno-unwind-tables"`. These reach
   MSVC `cl`, which does not understand the `-f*` flags. On a **cached**
   build (CI's `continue-on-error` `windows` job) they are only harmless
   `D9002 ignoring unknown option` warnings — but on a **clean** checkout
   (the release build, and any fresh runner) they break the CMake
   compiler-identification probe for ggml-vulkan's `vulkan-shaders-gen`
   host tool (`error C1083: Cannot open compiler generated file: '':
   Invalid argument`), which leaves the sub-project configure incomplete.
   This was the **first** error in `v0.16.0`'s failed release run. Fix
   mirrors item 1 — blank both on Windows so `cl` uses its own defaults:
   - CI + release `windows` jobs:
     `echo "CFLAGS=" >> "$GITHUB_ENV"` and `echo "CXXFLAGS=" >> …`.
   - `scripts/win-remote.sh`: passes
     `--config env.CFLAGS='' --config env.CXXFLAGS=''`.

   With this in place the `vulkan-shaders-gen` sub-project configures and
   its `.exe` builds — confirmed on the CI `windows` job — so the C1083
   probe failure is gone.

5. **`VCEnd` / `MSB8066` on the `vulkan-shaders-gen` **install** step —
   the Visual Studio generator + nested ExternalProject bug.** After the
   C1083 flag leak was fixed, a **clean** build still failed here: the
   `vulkan-shaders-gen` ExternalProject install runs `cmake -P
   …/vulkan-shaders-gen-build/cmake_install.cmake`, reported `Not a
   file`, aborting MSBuild (`... batch label specified - VCEnd` →
   `MSB8066`), with a nested `TryCompile` C1083 whose flags were by then
   already empty (`CFLAGS = Some()` in the build log) — i.e. a
   generator-level failure, not the flag leak. This is the well-known
   ggml-vulkan behaviour under the default `Visual Studio 17 2022`
   generator, where the shader-gen host tool is built as a nested
   ExternalProject whose custom install rule misbehaves. The dev Windows
   host (`scripts/win-remote.sh`) masks it with a warm `/target` (the
   sub-project was configured once and never re-runs); the clean release
   runner reconfigures every time and hits it. It was originally
   mis-attributed to a poisoned `rust-cache` — the `prefix-key` bump
   (still in place, harmless) did not fix it because the release job
   never cached. **Fix: build the vendored C/C++ with the single-config
   Ninja generator on Windows** (`CMAKE_GENERATOR=Ninja`), which sidesteps
   the VS-generator ExternalProject path entirely. Ninja is preinstalled
   on `windows-2022`; `-G Ninja` needs `cl`/`INCLUDE`/`LIB` on PATH (the
   VS generator provided these implicitly), so the CI and release Windows
   jobs run `ilammy/msvc-dev-cmd@v1` first. `scripts/win-remote.sh` is
   left on the VS generator — its warm cache already works.

6. **`could not compile proc-macro2 (build script)` — Git's `link.exe`
   shadows MSVC's under bash.** Once the Ninja fix let the release build
   run, it failed linking host build scripts with
   `"C:\Program Files\Git\usr\bin\link.exe" … exit code: 1`. The release
   `Build` step uses `shell: bash` (shared arg-assembly across OSes), and
   Git-for-Windows' `/usr/bin/link.exe` (a coreutils hardlink tool) sits
   ahead of MSVC's `link.exe` on the bash PATH, so rustc invokes the wrong
   linker. CI's cargo steps use the default pwsh shell and never hit this.
   Fix: a Windows-only release step removes
   `/c/Program Files/Git/usr/bin/link.exe` before the build so PATH
   resolves to MSVC's linker (put there by msvc-dev-cmd).

7. **`fatal error C1041: cannot open program database` — the build path
   exceeds MAX_PATH.** With the linker fixed, the release build failed in
   ggml-vulkan's `vulkan-shaders-gen` compiler probe: cl could not create
   its `.pdb` because the deeply-nested ExternalProject path
   (`target\x86_64-pc-windows-msvc\release-slim\build\llama-cpp-sys-2-…\
   out\build\ggml\src\ggml-vulkan\vulkan-shaders-gen-prefix\src\
   vulkan-shaders-gen-build\CMakeFiles\CMakeScratch\TryCompile-…\…\vc143.pdb`)
   runs past Windows' 260-char limit, and mspdbcore has no long-path
   support. CI never hit it because it builds into the much shorter
   `target\debug\…`, while the release adds `--target … --profile
   release-slim`. Fix: point the Windows release build at a short
   `CARGO_TARGET_DIR` (`D:\t`), which keeps the nested PDB path under the
   limit; the staging step reads `${CARGO_TARGET_DIR:-target}` so only the
   Windows job is affected.

8. **`RC2136 : missing '=' in EXSTYLE=<flags>` on `manifest.rc` — rc.exe
   vs the same deep ExternalProject path.** With the PDB fixed, the same
   `vulkan-shaders-gen` try-compile then failed one tool later: `cmake -E
   vs_link_exe` writes a `manifest.rc` referencing the absolute
   `intermediate.manifest` path, and under `D:\t\x86_64-pc-windows-msvc\
   release-slim\…` that path is ~254 chars — past the resource
   compiler's internal filename limit, which it reports as a bogus parse
   error on a nonexistent line 3 (the identical signature is documented
   across llama-cpp-python issues #582/#426: paths whose manifest lands
   ≤ ~246 chars pass, ~254+ fail). The dev Windows host never sees it
   because it builds without `--target` (shorter paths) and with
   `LongPathsEnabled=1`. Fix: drop `--target x86_64-pc-windows-msvc`
   from the Windows release build — the runner's host triple already IS
   the release triple, so the artefact is identical while every nested
   path loses the 23-char triple segment (manifest path ≈ 231 chars,
   comfortably inside the known-good range). The staging step picks the
   binary up from `D:\t\release-slim\` accordingly.

## Hotkeys and the daemon on Windows (Phase 8)

As of 2026-07-12 the daemon runs on Windows and its push-to-talk
hotkeys resolve to the Win32 `RegisterHotKey` backend. Two
Windows-specific runtime issues were fixed on the way, and one
headless-SSH limitation is worth knowing about:

1. **Main-thread stack overflow — fixed.** The very first time the
   daemon (not just `--version` / `doctor`) ran on Windows it died
   with `thread 'main' has overflowed its stack`. The MSVC main
   thread defaults to a 1 MiB stack, versus 8 MiB on Linux/macOS, and
   daemon init overflows it. The entry point now runs on a dedicated
   worker thread with a generous stack on Windows, mirroring what the
   macOS path already did. Linux/macOS behaviour is unchanged.

2. **`detect_backend` / graphical-session probe — fixed.** Backend
   detection and the "is there a graphical session?" gate both keyed
   off the Linux-only `DISPLAY` / `WAYLAND_DISPLAY` environment
   variables, so on Windows they resolved to `Disabled` / "headless"
   and the daemon skipped the hotkey listener entirely. Both now treat
   all non-Linux desktop targets (macOS and Windows) as having a
   graphical session, so Windows resolves to the `global-hotkey`
   listener. Confirmed live: the daemon logs
   `hotkey backend resolved: X11` on Windows.

3. **Interactive window station required (headless SSH limitation).**
   Running `fono.exe` over an SSH session (e.g. via `Start-Process
   -NoNewWindow`) lands the process in a **non-interactive window
   station**, where `RegisterHotKey` fails with `os error 1459`
   ("This operation requires an interactive window station") and the
   tray icon cannot be created either. This is **not** a Fono bug —
   it is how Windows isolates non-interactive sessions. The hotkey /
   tray / typing smoke tests therefore have to be run by a human
   logged in at the actual Windows desktop, not over SSH. Everything
   up to and including the `RegisterHotKey` call is verified over SSH;
   the final key-press round-trip is a manual desktop check.

**Rebinding hotkeys.** The default push-to-talk keys are F7
(dictation) and F8 (the second action); Esc cancels an in-progress
recording. F7/F8 can clash with an app's own shortcuts on Windows.
Users can rebind them with `fono use hotkey` — same command and
behaviour as on Linux; there is no Windows-specific hotkey config for
v1.

## The recording overlay on Windows (Phase 10)

As of 2026-07-12 Fono's recording overlay paints on Windows via a
dedicated Win32 backend (`crates/fono-overlay/src/backends/windows.rs`).

- **Why not winit + softbuffer.** The plan called for winit with a
  softbuffer surface, but softbuffer presents through GDI `BitBlt`,
  which discards per-pixel alpha. The overlay renderer produces a
  premultiplied-ARGB framebuffer with rounded-corner transparency, so
  the backend instead drives a layered tool-window and pushes each
  frame with `UpdateLayeredWindow` + a `BLENDFUNCTION` carrying
  `AC_SRC_ALPHA` — the only path that honours per-pixel alpha. A
  dedicated worker thread owns the `HWND` and message pump and
  receives frame snapshots over a channel, mirroring the macOS
  backend's structure.
- **Window styles.** `WS_EX_LAYERED` (per-pixel alpha),
  `WS_EX_TRANSPARENT` (click-through), `WS_EX_NOACTIVATE`
  (focus-passthrough), `WS_EX_TOPMOST` (always-on-top), and
  `WS_EX_TOOLWINDOW` (excluded from Alt+Tab). Anchored to the primary
  monitor's bottom-centre via `GetSystemMetrics`.
- **Selecting / overriding.** `fono doctor` shows the chosen backend;
  the default on Windows is `win32-layered-toolwindow`.
  `FONO_OVERLAY_BACKEND` accepts `win32` (aliases `windows` / `win` /
  `layered`) and `noop`. Note the cmd.exe trailing-space trap
  described above — `set FONO_OVERLAY_BACKEND=win32 && …` captures the
  space into the value; `parse` trims it, but prefer
  `set "FONO_OVERLAY_BACKEND=win32"` to be safe.
- **Manual gate.** The overlay actually appearing during a recording —
  correct bottom-centre anchoring, no focus stealing, no Alt+Tab
  entry, clicks passing through to the window beneath — needs a human
  at the interactive desktop (the same window-station limitation as
  the tray/hotkeys). Everything up to backend selection is verified
  over SSH.

## Install and autostart on Windows (Phase 11)

As of 2026-07-12 `fono install` works on Windows, per-user and with no
elevation (`crates/fono/src/install/windows.rs`).

- **What it does.** Copies the running binary to
  `%LOCALAPPDATA%\fono\fono.exe`, writes the autostart value
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\fono` (the path
  is stored quoted so a profile directory with spaces still launches).
  The daemon then starts at the next login. `fono doctor` infers the
  install state from the two artefacts an install actually creates —
  the binary under `%LOCALAPPDATA%\fono\` plus the Run value — and
  reports "self-installed via `fono install`" (no marker file is
  written; this mirrors the marker-free macOS installer).
- **`fono uninstall`.** Removes the Run value and the
  `%LOCALAPPDATA%\fono\` directory, but keeps `%APPDATA%\fono\` (config
  + history) so a reinstall picks up where you left off.
- **`--server` is Linux-only.** `fono install --server` is refused on
  Windows (no Windows service install in v1), matching macOS. Run
  `fono` manually with `[server.wyoming].enabled = true` if you need
  the Wyoming server.
- **Why `reg.exe`, not `winreg`.** The installer shells out to the
  built-in `reg.exe` rather than adding the `winreg` crate — it mirrors
  the macOS installer's `launchctl`/`security` subprocess style, needs
  no `unsafe`, and keeps the binary dependency-free. Registry writes
  (unlike `RegisterHotKey`) work fine over headless SSH, so the whole
  install/uninstall roundtrip is verifiable remotely — no interactive
  desktop needed. The only manual check is that the Run entry actually
  launches the daemon at a real login.

## Self-update on Windows (Phase 12)

As of 2026-07-12 `fono update` works on Windows.

- **Rename, don't overwrite.** Windows refuses to overwrite or delete a
  running `.exe`, but it *does* allow renaming it. The existing
  cross-platform swap in `fono-update::apply_update` already relies on
  exactly that: download to a temp file in the target directory, verify
  SHA-256, `rename(old → old.bak)`, then `rename(tmp → old)`. So no
  Windows-specific swap code was needed — only the relaunch differs.
- **Relaunch instead of `execv`.** Windows has no `execv`, so
  `restart_in_place` spawns the freshly-installed binary as an
  independent child (inheriting this process's console + argv) and then
  exits, releasing the renamed old image (the sibling `.bak`). The PID
  changes — unavoidable on Windows — but the command continues in the
  new binary. The leftover `.bak` is cleaned up on the next
  `fono update`.
- **Program Files is treated as managed.** `is_package_managed` returns
  `true` for an install path containing `\Program Files`, so
  `fono update` refuses to self-replace there (it would need elevation
  and fail mid-swap) and instead tells you to reinstall under your user
  profile with `fono install`. A per-user install under
  `%LOCALAPPDATA%\fono\` stays self-updatable.
- **No Windows release asset yet.** `fono update --check` resolves the
  asset name (`fono-vX.Y.Z-x86_64.exe`) and queries GitHub, but returns
  "no matching release asset" until the release workflow starts
  publishing a Windows artefact (Phase 13). The download→swap→relaunch
  round-trip becomes exercisable then.

## Vulkan single build (soft-load, 2026-07-13)

Windows ships **one** `fono.exe` that is Vulkan-accelerated *and* runs
everywhere — it uses the GPU when a usable Vulkan driver is present and
falls back to the CPU when it isn't. There is no separate CPU-only vs
GPU Windows download (unlike Linux, which keeps the two-variant split
for the ~42 MB SPIR-V shader payload). This was a deliberate
simplicity-over-size trade for a target the maintainer rarely tests;
see `plans/2026-07-12-vulkan-soft-load-single-build-v1.md` and
`docs/status.md`.

The enabler is the in-tree Vulkan loader shim
(`crates/fono-core/src/vk_loader_shim.rs`). ggml references three bare
Vulkan symbols at link time; the shim defines them itself as lazy
forwarders that `LoadLibraryA("vulkan-1.dll")` on first use. That keeps
`vulkan-1.dll` **out of the PE import table**, so the `.exe` launches
even on a machine with no GPU driver (no "vulkan-1.dll not found"
dialog). When the loader is absent the shim hands ggml an error stub
that makes it throw → catch → register zero devices → CPU, rather than
faulting on a null function pointer.

Verified end-to-end on the Windows 10 bench (2026-07-13):

- `dumpbin /DEPENDENTS target\debug\fono.exe` lists **no**
  `vulkan-1.dll` import (the shim satisfies ggml's references; MSVC
  uses our definitions instead of pulling from `vulkan-1.lib`).
- Loader present: `fono.exe doctor` reports
  `vulkan : Vulkan: detected (Intel(R) HD Graphics 620)`, and
  `fono-bench equivalence --model tiny --quick` transcribes on the GPU
  (PASS, acc 0.0882).
- Loader absent (simulated with a bogus `vulkan-1.dll` in the exe dir,
  which Windows searches before System32 → `LoadLibraryA` returns
  NULL): the same transcription **exits 0** — no crash — and falls
  back to CPU with identical accuracy. `doctor` reports
  `Vulkan: not available (vulkan-1.dll not loadable: …)`.

## Release artefact on Windows (Phase 13)

As of 2026-07-13 the release workflow (`.github/workflows/release.yml`)
builds and uploads a Windows binary on every tag.

- **What ships.** A single `x86_64` binary named
  `fono-vX.Y.Z-x86_64.exe`, plus its `.sha256` sidecar and a line in
  `SHA256SUMS`. No MSI, no code signing, no distro-style package — a
  bare `.exe` is the whole Windows v1 deliverable. It is
  Vulkan-accelerated with CPU fallback (see "Vulkan single build"
  above), not CPU-only.
- **How it's built.** A `windows-2022` matrix row runs
  `cargo build --profile release-slim --target x86_64-pc-windows-msvc
  -p fono --no-default-features --features windows-defaults`. The row
  reuses the ci.yml windows job's environment prep: git long paths
  before checkout, `LIBCLANG_PATH` at the runner's LLVM, and the
  pinned LunarG Vulkan SDK install (headers + `vulkan-1.lib` +
  `glslc`). The onnxruntime fetch is skipped (no `ort` in the v1
  feature set), and `/FORCE:MULTIPLE` comes from
  `.cargo/config.toml`'s MSVC block.
- **Size.** Adding Vulkan brings the `release-slim` `fono.exe` up to
  ~60 MiB (from the earlier CPU-only ~15.7 MiB) — the SPIR-V shader
  payload — and enabling local text-to-speech and wake-word (the
  statically-embedded ONNX Runtime, matching Linux/macOS) adds a
  further ~3 MiB, to ~72 MiB. This is the accepted cost of the
  single-build decision; the Windows size budget in ADR 0022 is set
  accordingly (enforced ≤ 75 MiB, hard cap ≤ 80 MiB, per the 2026-07-14
  amendment).
- **Not yet gated.** The PE import-table allowlist + size budget (the
  Windows analogue of the Linux ELF `NEEDED` gate) is deferred to
  Phase 14, along with promoting the non-blocking CI windows job to a
  required check. When it lands it must assert `vulkan-1.dll` is
  *absent* from the import table (the soft-load guarantee).

## Platform paths (not yet implemented for Windows)

The design plan's locale/config-path unification (Phase 1, Tier-1
constraint 3) hasn't landed; Windows path resolution is future work,
not yet exercised.
