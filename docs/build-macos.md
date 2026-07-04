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

### Phase 6 smoke (text injection / focus) — 2026-07-03

- Injector: `enigo` (CGEvent) is the macOS default; `fono test-inject`
  detects it and `Enigo::new()`/`text()` **return Ok even over
  headless SSH as root** — CGEventPost accepts the events, so the
  Accessibility-denial path cannot be triggered from SSH (deferred to
  the GUI pass, along with whether keystrokes actually land).
- Clipboard: NSPasteboard (arboard) and `pbcopy` both **fail cleanly
  over headless SSH** — the pasteboard daemon is per-login — with
  per-tool diagnostics and a macOS-specific error message.
  `FONO_INJECT_BACKEND=none` exercised the full documented-order
  degradation. `pbpaste` readback is wired into test-inject.
- Focus: `NSWorkspace.frontmostApplication` returns no app headless →
  empty `FocusInfo`, no error, matching the graceful-degradation
  contract.

### Phase 7 smoke (menu-bar tray / NSStatusItem) — 2026-07-04

- The tray menu is defined once, platform-neutrally, in
  `fono-tray::menu::build`; macOS renders it via an `NSStatusItem` +
  `NSMenu` interpreter (`fono-tray::backend_macos`). Zero new crates.
- Main-thread pump: on darwin, a daemon invocation in a graphical
  session parks the real main thread in `NSApplication::run()`
  (Accessory activation policy — no Dock icon, no Cmd+Tab entry);
  tray/overlay render jobs arrive event-driven via libdispatch's main
  queue; the daemon runs on a worker thread.
- Headless degradation verified over SSH: `is_graphical_session()` is
  false → no pump installed → daemon logs
  `tray icon    : skipped (headless: no graphical session)` and keeps
  running (STT warmup, mDNS, servers all normal). Non-daemon
  subcommands never install the pump.

### Phase 8 smoke (NSPanel overlay) — 2026-07-04

- Overlay backend `mac-panel`: a borderless, non-activating,
  click-through, always-on-top `NSPanel` (level 25, all Spaces,
  excluded from the window cycler), software-blitted from the same
  renderer as Linux. A worker thread renders ARGB frames; the AppKit
  main thread blits them via `NSBitmapImageRep` → `NSImageView`
  through a newest-wins mailbox, event-driven on the GCD main queue
  (repaints at the producers' ≈20–30 fps cadence). Zero new crates
  (objc2* already in the graph).
- `fono doctor` probe: `Overlay : mac-panel (transparency=yes
  positioning=client focus-passthrough=yes click-passthrough=yes)`.
- Headless degradation verified over SSH: no pump installed →
  `mac-panel` skipped (`AppKit main-thread pump not installed`) →
  selector falls to `noop`; daemon unaffected.
- `FONO_OVERLAY_BACKEND=mac|macos|mac-panel|nspanel` forces the
  backend; `noop|none|off` disables, same as Linux.

### Phase 9 smoke (install / autostart / permissions) — 2026-07-04

- `fono install` (per-user, no sudo): assembles `~/Applications/Fono.app`
  around the running binary (`org.fono.app`, `LSUIElement`,
  `NSMicrophoneUsageDescription`), creates the `fono-local-signing`
  self-signed cert once in a dedicated always-unlocked keychain
  (`~/Library/Keychains/fono-signing.keychain-db`), signs the bundle
  with it, writes the LaunchAgent
  (`~/Library/LaunchAgents/org.fono.daemon.plist` — `RunAtLoad`,
  crash-only `KeepAlive`, `LimitLoadToSessionType Aqua`), and symlinks
  `/usr/local/bin/fono`. Both plists `plutil -lint` clean.
- **Grant-once property bench-proven**: `codesign -d -r-` shows the
  designated requirement `identifier "org.fono.app" and certificate
  leaf = H"…"`, byte-identical across re-installs — the TCC
  Accessibility grant therefore survives updates.
- Bench facts: `security add-trusted-cert` is denied headless (GUI
  authorization) and `find-identity -v` hides the untrusted cert, but
  `codesign` signs with it regardless — the installer probes without
  `-v` and skips trust settings entirely.
- `launchctl bootstrap gui/$UID` correctly degrades headless ("domain
  does not exist") — install reports "starts at next login".
- `fono doctor`: `Install:` row shows the bundle+agent state;
  `Accessibility:` row probes `AXIsProcessTrusted` (answers headless)
  and prints the `x-apple.systempreferences:…Privacy_Accessibility`
  deep link when not granted.
- `fono uninstall` round-trip: agent booted out, plist + bundle +
  symlink + `~/.cache/fono` removed; config, history, and the signing
  keychain kept (re-install reuses the same identity).

### Phase 10 smoke (`fono update`) — 2026-07-04

- `fono update --check` resolves the darwin asset name
  (`fono-vX.Y.Z-aarch64-apple-darwin`, single Metal variant) and
  reports truthfully that the latest release carries no matching
  asset — correct until Phase 11 publishes one. The GPU-upgrade
  suggestion machinery is Linux-only by construction (macOS ships one
  variant).
- **Update swap + re-sign sequence bench-proven** (the exact steps
  `install::resign_after_update()` performs): swapping the binary
  inside `Fono.app` breaks the bundle seal (`codesign --verify`
  complains), `codesign --force --sign fono-local-signing` restores
  it, `codesign --verify --deep` passes, and `codesign -d -r-` shows
  the designated requirement **byte-identical** before/after — the
  Accessibility grant therefore survives `fono update`.
- Bare-binary installs (no `Fono.app` on the executable path) skip
  the hook silently; a failed re-sign warns and points at the
  re-grant + `fono install` recovery instead of failing the update.

### Phase 11 dry-run (release artefact) — 2026-07-04

- The exact `release.yml` step sequence was replicated on the bench:
  `scripts/fetch-onnxruntime.sh` (pinned static lib), `ORT_CXX_STDLIB=c++`,
  `cargo build --profile release-slim -p fono --features accel-metal`
  for `aarch64-apple-darwin`.
- Artefact: **15.40 MiB** (16,143,328 B), `fono --version` runs, and it
  transcribes the English fixture correctly on Metal with
  `large-v3-turbo`.
- The workflow's Mach-O dylib gate passed verbatim: 17 `LC_LOAD_DYLIB`
  imports, every one under `/System/Library/Frameworks/` or `/usr/lib/`
  (incl. Metal, MetalKit, AppKit, CoreAudio) — no third-party dylibs.
- The asset (`fono-vX.Y.Z-aarch64-apple-darwin` + `.sha256`, listed in
  `SHA256SUMS`) publishes automatically at the next `v*` tag; the
  end-to-end `fono update` onto it is on the checklist below.

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
  Note: headless CGEventPost *accepts* events without the grant, so
  also verify the denial UX (deny the grant, confirm the clipboard
  fallback + notification) — it is unobservable over SSH.
- [ ] Clipboard round-trip in a logged-in session: dictation →
  NSPasteboard via arboard → Cmd+V paste; `fono test-inject` readback
  via pbpaste MATCHES (Phase 6; headless SSH has no pboard daemon).
- [ ] Menu-bar icon visible with working menu (Phase 7): state tint
  changes while recording, all submenus present and firing actions,
  no Dock icon / no Cmd+Tab entry (Accessory policy), tooltip shows
  the FSM state line.
- [ ] Overlay paints during recording: click-through, no focus steal,
  no Dock/Cmd-Tab presence, bottom-centred on the primary display,
  correct retina sharpness (scale sync), and smooth animation —
  frames arrive event-driven via the GCD main queue at the producers'
  ≈20–30 fps cadence (Phase 8).
- [ ] `fono install` → logout/login → daemon + menu-bar icon running,
  agent listed by `launchctl print gui/$UID/org.fono.daemon`; first
  daemon start raises the native Accessibility dialog
  (`AXIsProcessTrustedWithOptions`) exactly once, deep-linking to the
  right Settings pane; after granting, `fono doctor` shows
  `Accessibility: granted` and injection types at the cursor. Then
  `fono update` (or a re-install simulating one) — the grant must
  survive without re-toggling (stable designated requirement,
  Phase 9 / Task 11.4).
- [ ] End-to-end `fono update` against a real published darwin release
  asset (Phase 10; needs Phase 11's artefact): download, sha256
  verify, in-place swap, automatic re-sign, relaunch — and the
  Accessibility grant survives without re-toggling.
