# Building Fono for macOS

Status: the macOS port is in progress — see
`plans/2026-07-03-macos-port-v1.md` for the phased plan and
`docs/status.md` for session history. This document describes how the
darwin build works, the remote-Mac development loop, and the
deferred-GUI checklist (the manual test pass that headless development
cannot cover).

## Build requirements

- macOS 15+ with Xcode (clang, ld64, `metal` toolchain for
  `accel-metal`).
- Rust per `rust-toolchain.toml` (currently 1.88) via rustup.
- CMake ≥ 3.28 (llama-cpp-sys-2 / whisper-rs-sys build scripts). A
  standalone tarball from cmake.org works; Homebrew is not required.
- `xz` **only** for a cold `scripts/fetch-onnxruntime.sh` download
  (stock macOS lacks it). Do **not** substitute bsdtar's raw-xz mode —
  it silently truncates the multi-stream `.xz` asset (see the script
  header). CI runners and most dev machines have `xz`.

## Building

```sh
export ORT_LIB_LOCATION="$(./scripts/fetch-onnxruntime.sh)"
export ORT_CXX_STDLIB=c++
cargo build --profile release-slim -p fono --features accel-metal
```

`ORT_CXX_STDLIB=c++` is required: the workspace `.cargo/config.toml`
sets `[env] ORT_CXX_STDLIB="static:-bundle=stdc++"` for the Linux-GNU
NEEDED-allowlist (ADR 0022), cargo `[env]` cannot be target-scoped, and
ld64 has no `libstdc++` — the inherited environment wins over `[env]`,
so exporting `c++` restores `ort-sys`'s own Apple default.

### Artefact shape

macOS ships **one variant only: Metal-accelerated** (no cpu/gpu split —
measured +0.65 MiB / +4.3 % over CPU-only for 4.3× faster
large-v3-turbo transcription and ~170× less CPU time; ggml falls back
to its CPU backend at runtime when Metal init fails). Eventually a
single universal (lipo) binary; arm64-only until the
`x86_64-apple-darwin` onnxruntime pin exists. The arm64 linker applies
an ad-hoc code signature automatically (`codesign -dv` →
`adhoc,linker-signed`); no notarization in v1.

## Remote development loop (headless Mac)

Development happens on the Linux workstation; a headless Mac reachable
over SSH is the build/test bench, driven by `scripts/mac-remote.sh`:

```sh
export FONO_MAC_HOST=<user@host>     # never stored in the repo
scripts/mac-remote.sh check          # rsync tree + cargo check --workspace
scripts/mac-remote.sh test           # rsync tree + workspace tests
scripts/mac-remote.sh build --profile release-slim -p fono --features accel-metal
scripts/mac-remote.sh sh './target/release-slim/fono doctor'
```

The Mac's address, credentials, and any machine-specific details are
deliberately kept out of the repository (plan guiding constraint 7):
`FONO_MAC_HOST` comes from your shell environment or an untracked local
file. The script errors out when it is unset.

### Sandbox layout on the Mac

Everything lives under one directory so `rm -rf ~/fono-dev` removes
every trace (no Homebrew formulae, no system-wide installs):

| Path | Purpose |
|---|---|
| `~/fono-dev/rustup`, `~/fono-dev/cargo` | `RUSTUP_HOME` / `CARGO_HOME` |
| `~/fono-dev/fono` | repo mirror (rsync target) |
| `~/fono-dev/tools/` | standalone CMake |
| `~/fono-dev/env.sh` | exports `RUSTUP_HOME`, `CARGO_HOME`, `PATH`, `ORT_LIB_LOCATION`, `ORT_CXX_STDLIB=c++` |

Every `mac-remote.sh` remote command sources `~/fono-dev/env.sh` first.
The `push` subcommand mirrors the working tree with `--delete` but
never touches the remote `target/` (explicit exclude) — the pinned
onnxruntime lib and the build cache survive every sync.

## Platform paths (pinned 2026-07-03)

Fono resolves the same XDG-style dotfile paths on macOS as on Linux —
there is **no** `~/Library/Application Support` drift (plan risk 5
closed; any future remap would be a deliberate migration):

| Purpose | Path |
|---|---|
| config | `~/.config/fono/config.toml` (+ `secrets.toml`, `vocabulary.toml`) |
| data | `~/.local/share/fono` |
| cache (models) | `~/.cache/fono` |
| state | `~/.local/state/fono` |

## What already works headless (smoked 2026-07-03)

All of this ran over SSH on the dev Mac with no GUI session, default
features, debug build:

- `fono doctor` / `hwprobe` — real values via Mach sysctls
  (10 physical cores, 64 GB RAM, tier `recommended`).
- The **full daemon** starts and idles: locale auto-detection,
  headless tray, noop overlay, mDNS browser, update check (graceful
  "no published binary for macos/aarch64").
- Local TTS: kokoro + piper voices auto-download (sha256-verified) at
  daemon startup; `fono speak stream --out x.wav` synthesises via the
  statically-linked onnxruntime.
- Round-trip: the synthesized WAV transcribed back correctly with
  `fono transcribe --stt local`.
- Wyoming server: listens on `127.0.0.1:10300`, advertises TTS voices
  and wake-word detection.
- `fono history`, `config show|path`, `models install`, `use`,
  `voices list` — all functional.
- Graceful degradation where it must: `test-inject` reports
  `Detected key-injector: None` (Phase 6).

### Phase 4 smoke (cpal / CoreAudio) — 2026-07-03

- Playback: `fono speak stream` played synthesized speech to "Mac
  Studio Speakers" through the cpal worker (rc=0, ring drained).
- Capture: this Mac Studio has **no microphone hardware** at all
  (`system_profiler SPAudioDataType` lists only speakers), so `fono
  record` exercises the no-device failure path — clean error naming
  System Settings → Privacy & Security → Microphone, no hang.
- `fono doctor`: "Audio stack : CoreAudio", cpal-backed input
  enumeration, macOS-specific empty-inputs hint.
- Auto-mute: `AudioStack::CoreAudio` toggles the system output mute
  via `osascript` — round-trip verified headless.

### Phase 5 smoke (global hotkeys / Carbon) — 2026-07-03

- Backend: the same `global-hotkey` crate the Linux X11 listener uses;
  its Carbon `RegisterEventHotKey` backend needs **no TCC permission**.
- Registration works even over headless SSH as root: the
  `fono-hotkey` probe example registered F7, F8 and Esc and
  unregistered them cleanly (rc=0) — no WindowServer session required
  for registration, only for event *delivery*.
- The daemon correctly detects the SSH session as non-graphical and
  skips the listener with a clear log line; on a console session it
  would select the `macos` (Carbon) backend.

## Deferred-GUI checklist

The dev Mac is headless-only, so anything that needs a seated user —
TCC permission grants and on-screen behaviour — cannot be verified
during development (plan guiding constraint 8). This checklist is the
release-blocking manual pass on a physically accessible Mac before a
macOS artefact is advertised as tested. Items accumulate as phases
land:

- [ ] Grant Microphone (TCC) on first capture; live `fono record` →
  STT → transcript round-trip (plan Task 4.2/4.3). Needs a Mac with a
  microphone — the dev Mac Studio has none, so only the failure path
  is verified.
- [ ] Global hotkeys F7 / F8 / Esc fire in a GUI session (Phase 5;
  registration already proven headless — only event delivery is
  untested).
- [ ] Grant Accessibility (TCC); dictation lands at the cursor in
  TextEdit, Safari address bar, VS Code; per-app rules fire (Phase 6).
- [ ] Menu-bar icon visible with working menu (Phase 7).
- [ ] Overlay paints during recording: click-through, no focus steal,
  no Dock/Cmd-Tab presence, correct positioning (Phase 8).
- [ ] `fono install` → logout/login → daemon + menu-bar icon running
  (Phase 9).
