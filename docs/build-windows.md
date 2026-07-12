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

## Platform paths (not yet implemented for Windows)

The design plan's locale/config-path unification (Phase 1, Tier-1
constraint 3) hasn't landed; Windows path resolution is future work,
not yet exercised.
