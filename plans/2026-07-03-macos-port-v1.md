# macOS Port — Remote Dev Setup and Build Plan

## Objective

Ship a working macOS build of Fono as a second-class target alongside the
first-class Linux build, developed from the Linux workstation with build and
runtime testing executed remotely on a Mac over SSH. **The Mac is
headless-only** — it is in another country, no console session, no screen
sharing, no human at the machine — so every phase must be verified as far
as headless SSH allows, and the residue that genuinely needs a GUI session
(TCC prompts, on-screen behaviour) is tracked explicitly and deferred to
pre-release manual testing on a physically accessible Mac. The Linux build
must stay byte-equivalent through every phase except explicitly called-out
cross-platform improvements adopted with documented Linux cost.

**Target macOS surface for v1**: native `aarch64-apple-darwin` binary,
CoreAudio capture/playback via cpal, menu-bar (tray) icon, global hotkeys,
click-through overlay panel, autostart via LaunchAgent, `fono update`
self-replacement, full CLI parity (`fono doctor`, `fono use`, `fono history`,
etc.). Ad-hoc code signature only (the arm64 linker applies one
automatically); no notarization, no `.dmg` in v1 — ship the bare binary plus
`.sha256` sidecar.

**Out of scope for v1**: `x86_64-apple-darwin` + universal (lipo) binary
(design doc lists it as the eventual release shape; arm64-only first),
signed/notarized `.dmg` (roadmap "menu-bar app and signed .dmg" is the v2
finish line), Mac App Store.

**Artefact-shape decision (2026-07-03)**: macOS ships **one variant only,
Metal-accelerated** — no cpu/gpu split like Linux. Measured on the Mac
Studio (see Phase 3): `accel-metal` costs +0.65 MiB (+4.3 %) over the
CPU-only build while cutting large-v3-turbo transcription wall time 4.3×
and CPU time ~170× (battery/thermals). Every supported Mac has Metal, and
ggml falls back to the CPU backend at runtime if Metal init fails, so a
single Metal artefact serves all. Once the `x86_64-apple-darwin`
onnxruntime pin exists, the ship shape becomes a single **universal (lipo)
binary** of that one variant (Task 11.3); until then, arm64-only.

## Guiding Constraints

1. **Linux first-class** — every change leaves the Linux build green: no
   NEEDED-set growth, no `release-slim` size regression beyond flagged deps,
   no Linux dep-graph mutation. `./tests/check.sh --size-budget` remains the
   mechanical enforcement.
2. **macOS code is additive** — new macOS code lives behind
   `#[cfg(target_os = "macos")]` or `[target.'cfg(target_os = "macos")'.dependencies]`
   tables. Existing Linux modules move verbatim into `linux.rs` siblings when
   a trait split is introduced (the Windows plan's Phase 1 splits are shared
   groundwork; whichever port lands first does the split, the other reuses it).
3. **New-to-project dependencies need sign-off** (AGENTS.md size rule) even
   when macOS-only — target-gated deps don't grow the Linux binary, but the
   flag-first discipline stands; each is called out in its phase.
4. **CI Linux gate cannot block on macOS** — the macOS CI row starts
   `continue-on-error: true`. A red macOS build never holds a Linux release.
5. **Pre-commit gate stays Linux-only** — the AGENTS.md three-step gate runs
   on the Linux dev host. macOS verification is an SSH step before push.
6. **Everything on the Mac lives in one directory** (`~/fono-dev`, i.e.
   `/var/root/fono-dev`) — toolchain, repo, tools, caches — so
   `rm -rf ~/fono-dev` cleans the machine completely. No system-wide installs
   (no brew formulae, no /usr/local writes).
7. **No machine identifiers in the repo.** The remote Mac's IP/hostname,
   credentials, and any machine-specific connection details never appear in
   tracked files, scripts, or commit messages. Tooling reads the host from
   `FONO_MAC_HOST` (environment or an untracked/git-ignored local file) and
   fails with a clear message when unset.
8. **Headless-first verification.** Each GUI-adjacent phase (4–9) defines
   two tiers: (a) what SSH can prove — compile, unit tests, graceful
   degradation, doctor/CLI output, API return codes — which gates the
   phase; and (b) a named **deferred-GUI checklist** item (collected in
   Phase 11.2's `docs/build-macos.md`) for what only a seated user can
   confirm. Deferred items don't block landing the code; they block
   calling the macOS artefact "tested" in release notes.

## Implementation Plan

### Phase 0 — Remote macOS dev environment ✅ (done 2026-07-03)

- [x] Task 0.1. **Host confirmed reachable.** A remote Mac Studio (arm64,
      10 cores, 64 GiB), macOS 15.6, Xcode 26.1.1, SSH key auth as `root`.
      The machine's address and any connection details stay **out of the
      repo** — tooling reads them from the `FONO_MAC_HOST` environment
      variable (or an untracked local file); no IPs/hostnames in tracked
      files, scripts, or commit messages.
- [x] Task 0.2. **Rust 1.88 installed inside the sandbox.**
      `RUSTUP_HOME`/`CARGO_HOME` under `~/fono-dev`; `rust-toolchain.toml`
      picked up automatically (1.88 + musl cross targets).
- [x] Task 0.3. **Repo cloned** to `~/fono-dev/fono` (shallow, GitHub main).
- [x] Task 0.4. **Standalone CMake 3.31.6** unpacked under `~/fono-dev/tools`
      (needed by llama-cpp-sys-2 / whisper-rs-sys; no brew).
- [x] Task 0.5. **Session env file** `~/fono-dev/env.sh` exports
      `RUSTUP_HOME`, `CARGO_HOME`, `PATH` (cargo + cmake), and
      `ORT_LIB_LOCATION`.
- [x] Task 0.6. **Pinned onnxruntime for `aarch64-apple-darwin`.** The Mac
      lacks `xz` and `sha256sum`; the verified lib was provisioned into the
      script's cache dir from the Linux host, and
      `scripts/fetch-onnxruntime.sh` gained a `shasum -a 256` fallback so its
      fast path verifies on stock macOS. Note: bsdtar's raw-xz mode
      silently truncates the multi-stream `.xz` asset — never use it as an
      xz substitute (documented in the script header). A fresh Mac still
      needs `xz` (or a pre-provisioned lib) for a cold download.
- [x] Task 0.7. **Remote helper script.** `scripts/mac-remote.sh`
      (Linux-side): `push` (rsync working tree per `.gitignore`, with an
      explicit `/target` exclude — the per-dir merge filter alone did not
      protect the remote build cache from `--delete` and one push wiped
      it), `check` / `build` / `test` / `cargo` (push + ssh cargo …,
      sourcing `~/fono-dev/env.sh`), `sh` (raw remote shell, no push).
      The host comes exclusively from `FONO_MAC_HOST` (no default, no IP
      in the script — constraint 7); errors out with guidance when unset.
      Sandbox layout documented in `docs/build-macos.md`. Done 2026-07-03.

**Phase 0 gate**: `ssh "$FONO_MAC_HOST" 'source ~/fono-dev/env.sh && cd ~/fono-dev/fono && cargo --version && cmake --version'`
succeeds. ✅ (complete, including Task 0.7)

### Phase 1 — `cargo check --workspace` green on darwin ✅ (complete 2026-07-03)

The probe found only two front-line failures; both fixed:

- [x] Task 1.1. **`fono-core` notify fix.** `notify_rust::Notification::hint`
      only exists on `cfg(all(unix, not(macos)))`, so the mac/windows arm in
      `crates/fono-core/src/notify.rs` could never compile off-Linux (it
      would have failed on Windows too). Urgency is now accepted and ignored
      on those targets (their notification backends have no urgency concept).
- [x] Task 1.2. **`fono-overlay` Linux-backend gating.** The graphical
      backend deps (winit/softbuffer/smithay/wayland-*/rustix/libloading)
      moved to a `[target.'cfg(target_os = "linux")'.dependencies]` table;
      backend modules and the `try_spawn` dispatch are gated on
      `all(feature, target_os = "linux")`. On macOS `real-window` compiles to
      the noop-only selector (no `WAYLAND_DISPLAY`/`DISPLAY` ⇒ noop), so the
      daemon runs headless until Phase 8 adds a native backend.
- [x] Task 1.3. **Full-workspace `cargo check` green on
      `aarch64-apple-darwin`** — all 19 crates, default features (including
      `tts-local` against the pinned onnxruntime, llama.cpp and whisper.cpp
      compiled by Xcode clang).
- [x] Task 1.4. **Darwin warnings silenced — zero-warning workspace check**
      (done 2026-07-03). cfg-gates (not `allow(dead_code)`) on the
      cfg-shadowed Linux-only items: `fono-core` locale XKB helpers,
      `fono-audio` capture/playback imports + backend-only helpers
      (`is_stopping`, `Cmd` payload fields get a targeted `cfg_attr`
      allow since the enum itself is cross-platform), `fono-inject`
      terminal consts + `CodingAgentKind` import. Linux clippy
      byte-identical (fmt/clippy/test gate green).
- [x] Task 1.5. **`cargo test --workspace --tests --lib` green on darwin**
      (done 2026-07-03): 36 suites, 0 failures, no cfg-gating of tests
      needed. The one real failure it caught was a **latent FFI bug**:
      `fono-core::hwcheck`'s hand-rolled `struct statvfs` used the Linux
      all-u64 layout on every unix, but Darwin's block/file counts are
      u32 — the garbage product overflowed. Fixed with a per-OS layout +
      checked multiply. Same run exposed `read_meminfo`/`physical_cores`
      returning 0/None on macOS (doctor claimed "0 GB RAM, unsuitable"
      on a 64 GiB machine); both now use Mach (`sysctlbyname`
      `hw.memsize`/`hw.physicalcpu`, `host_statistics64` for available
      RAM) via a macOS-only `libc` dependency edge (crate already in
      every target's graph — net-zero size).

**Phase 1 gate**: check green (✅), tests green (✅), zero warnings on
darwin (✅); Linux pre-commit gate green throughout (✅). **Phase complete
2026-07-03.**

### Phase 2 — CI macOS row, non-blocking

- [x] Task 2.1. **`macos` job added to `.github/workflows/ci.yml`**
      (macos-15 arm64, `continue-on-error: true`, own job rather than a
      matrix row — the Linux job's apt/fixture steps don't apply):
      checkout, stable rustup, rust-cache, `scripts/fetch-onnxruntime.sh`
      (runners ship xz/shasum), `ORT_CXX_STDLIB=c++` at job level,
      `cargo check --workspace` with `-D warnings`, `cargo test
      --workspace --tests --lib`. Promote to blocking at Phase 11/12.
- [x] Task 2.2. **ELF NEEDED / size gates stay Linux-only** — confirmed:
      `size-budget` matrix rows are all Linux runners; the new macOS job
      has no size gate (Mach-O gate deferred to Phase 12).

**Phase 2 gate**: Linux rows unaffected; macOS row visible in Actions.
(Job lands with this commit; first Actions run will confirm — it
exercises the same commands proven green on the dev Mac.)

### Phase 3 — Full binary build + accel decision ✅ (complete 2026-07-03)

- [x] Task 3.1. **`cargo build --profile release-slim -p fono` links on
      darwin.** Two link fixes were needed:
      1. The workspace `[env] ORT_CXX_STDLIB = "static:-bundle=stdc++"`
         (`.cargo/config.toml`, a Linux-GNU allowlist fix) leaks into darwin
         builds and makes `ort-sys` emit `-lstdc++`, which ld64 cannot find
         (macOS has only libc++) — the `tts-local` link fails. Cargo `[env]`
         cannot be target-scoped and `ort-sys` has no target-suffixed
         variant of the var, so darwin builds **must export
         `ORT_CXX_STDLIB=c++` in the environment** (inherited env wins over
         `[env]`). Done in `~/fono-dev/env.sh`; the Phase 2 CI row and the
         Phase 11 release row must set it too. (Unset, `ort-sys` would pick
         `c++` by itself — the override merely cancels our Linux-ism.)
      2. Harmless residue: ld64 warns about llama's nonexistent `lib64`
         search path and duplicate `-lc++`; no action needed.
      Sizes (release-slim, default features, arm64): **CPU-only 15,871,968 B
      (15.14 MiB); `accel-metal` 16,554,400 B (15.79 MiB)** — Metal delta
      +682,432 B (+4.3 %). Both run (`fono 0.14.0`), dylib imports are
      system frameworks + libSystem/libc++/libiconv/libobjc only.
      `codesign -dv`: `Mach-O thin (arm64)`, `flags=0x20002
      (adhoc,linker-signed)` — ad-hoc signature confirmed.
- [x] Task 3.2. **Metal decided: ship Metal-only** (see the artefact-shape
      decision at the top). Benchmarks on the Mac Studio (M-series, 10
      cores), 30 s Romanian fixture (`ro-bogdan-30s.wav`), `--no-polish
      --stt local`, best of 2 (run-to-run spread < 3 %):
      | model (q8_0) | CPU-only wall / user | Metal wall / user |
      |---|---|---|
      | small | 1.51 s / 5.67 s | 1.10 s / 0.17 s |
      | large-v3-turbo | 9.25 s / 39.68 s | 2.12 s / 0.23 s |
      Metal is 1.4× (small) to 4.3× (large-v3-turbo) faster wall-clock and
      offloads essentially all compute off the CPU (user time ÷ ~25–170) —
      decisive for battery and thermals on laptops. Verified the CPU-only
      binary contains zero `ggml_metal` code (the Metal *framework* link is
      unconditional on darwin but dormant), and the Metal binary logs
      `ggml_metal_device_init` at runtime. Both binaries produce the same
      transcripts.
- [x] Task 3.3. **Headless surfaces smoked over SSH** (completed
      2026-07-03; results recorded in `docs/build-macos.md`). Highlights:
      the **full daemon** starts and idles headless (headless tray, noop
      overlay, mDNS, graceful update check); local TTS voices
      auto-download and `fono speak stream --out` synthesises through the
      static onnxruntime; the synthesized WAV round-trips through `fono
      transcribe --stt local`; the Wyoming server listens on
      `127.0.0.1:10300` advertising TTS + wake-word; `doctor`, `history`,
      `hwprobe`, `models install`, `use`, `voices list` all work.
      Graceful-degradation paths verified: `record` errors with the
      cpal-backend hint (Phase 4), `test-inject` reports no key-injector
      (Phase 6). **Paths pinned (risk 5 closed)**: fono resolves the same
      XDG-style dotfiles on macOS as on Linux (`~/.config/fono`,
      `~/.local/share/fono`, `~/.cache/fono`, `~/.local/state/fono`) — no
      `~/Library` drift. Risk-4 signal: the notify test logged
      "Connection to notification center invalid" (headless, unbundled) —
      the osascript fallback question stays open for a GUI Mac.
      Earlier partial (2026-07-03 morning): `models install` +
      `transcribe` round-trips on both release binaries.

**Phase 3 gate**: release-slim binary builds and headless CLI works on the
Mac. ✅ **Phase complete 2026-07-03.**

### Phase 4 — Audio capture and playback (cpal / CoreAudio)

- [ ] Task 4.1. **Default `cpal-backend` on macOS** for `fono-audio`
      (same mechanism the Windows plan Phase 5.1 sketches; Linux stays
      parec/paplay). Note pre-existing cpal-feature clippy debt
      (status.md 2026-06-17) — fix what the gate needs.
- [ ] Task 4.2. **Mic permission (TCC).** First capture prompts for
      microphone access — only in a GUI session, and per-binary. Over
      headless SSH there is no prompt and no way to grant it: verify the
      *failure* path instead — capture must degrade gracefully (clear
      error, no hang, no crash) and `fono doctor` must say exactly what to
      grant and where. Deferred-GUI: the actual grant + first capture.
- [ ] Task 4.3. **Capture round-trip — headless tier**: `fono record`
      over SSH exercises device open → TCC denial → error path; the true
      mic → STT → transcript round-trip is deferred-GUI. The STT half is
      already proven via `fono transcribe` (Phase 3).
- [ ] Task 4.4. **Playback round-trip**: `fono speak` through CoreAudio.
      Audio *output* needs no TCC grant, so this should be provable over
      SSH (exit code + no error; nobody is there to hear the speaker).
- [ ] Task 4.5. **Mic enumeration** for the tray/doctor via cpal
      `input_devices()` (replaces `pactl list short sources`) — device
      *listing* is expected to work headless; confirm.

**Phase 4 gate (headless)**: cpal backend compiles and enumerates; playback
path returns success; capture fails gracefully with actionable doctor
guidance. **Deferred-GUI**: mic grant + live voice round-trip.
Linux audio path untouched.

### Phase 5 — Global hotkeys

- [ ] Task 5.1. **Backend decision (flag before adding).** Candidates:
      `global-hotkey` crate (Carbon `RegisterEventHotKey`; same crate the
      Windows plan picks — one new dep serves two ports) vs a minimal
      objc2/CGEventTap shim. Evaluate F7/F8/Esc coverage and the
      Input-Monitoring/Accessibility permission story; state binary-size
      impact (macOS-only dep ⇒ zero Linux cost) and get sign-off.
- [ ] Task 5.2. **Implement behind the existing listener seam** in
      `fono-hotkey` (`x11-dl`/`ashpd` paths compile on macOS today but are
      dead there; gate them `target_os = "linux"` while adding the macOS
      module). FSM stays OS-agnostic.
- [ ] Task 5.3. **Esc-to-cancel** mirrors the transient
      `EnableCancel`/`DisableCancel` registration.
- [ ] Task 5.4. **Linux regression check**: portal + X11 hotkeys unchanged.

**Phase 5 gate (headless)**: macOS hotkey module compiles, unit tests for
the keymap/FSM wiring pass, and registration over SSH either succeeds or
fails gracefully with doctor guidance (Carbon/event-tap registration
likely needs a WindowServer session — record which). **Deferred-GUI**:
F7/F8/Esc actually firing.

### Phase 6 — Text injection + focus detection

- [ ] Task 6.1. **enigo on macOS** (CGEvent). The `enigo-backend` feature
      already exists in `fono-inject`; enable it for macOS builds and add a
      `#[cfg(target_os = "macos")]` branch in `detect_auto` (no
      Wayland/X11 probes). Requires the Accessibility permission (TCC).
- [ ] Task 6.2. **Clipboard fallback**: `arboard` supports macOS; drop its
      Linux-only `wayland-data-control` feature into a target table so the
      mac build gets the NSPasteboard backend.
- [ ] Task 6.3. **Focus detection**: NSWorkspace `frontmostApplication`
      (objc2 — flag; it's the standard Rust↔ObjC bridge) behind the same
      seam the Windows plan's Phase 9 trait split defines. Wire app names
      into the existing per-app classifier with mac-flavoured rules
      (`Terminal`, `iTerm2`, `Code`, `Safari`, …).
- [ ] Task 6.4. **Headless tier**: `fono test-inject` over SSH exercises
      the cascade → Accessibility denial → clipboard fallback → error
      reporting. **Deferred-GUI**: verification against three apps
      (TextEdit, Safari address bar, VS Code).

**Phase 6 gate (headless)**: inject cascade compiles, degrades in the
documented order, and doctor explains the Accessibility grant.
**Deferred-GUI**: dictation landing at the cursor in the reference apps;
per-app rules firing. Linux inject cascade unchanged.

### Phase 7 — Menu-bar (tray) icon

- [ ] Task 7.1. **Backend decision (flag first).** `tray-icon` crate
      (muda-based, shared with the Windows plan) vs objc2 NSStatusItem
      direct. macOS-only target dep either way; Linux stays ksni.
- [ ] Task 7.2. **Implement behind the `fono-tray` trait split** (Windows
      plan Task 1.1 — do the split now if the Windows port hasn't). Mirror
      the Linux menu structure; embed the PNG icon via `include_bytes!`.
- [ ] Task 7.3. **Caveat**: NSStatusItem requires the main thread + a running
      event loop; reconcile with the daemon's thread layout (likely the same
      main-thread event pump the overlay backend needs — design them
      together with Phase 8).

**Phase 7 gate (headless)**: tray backend compiles and, with no
WindowServer access over SSH, fails gracefully into the existing no-tray
mode (daemon keeps running — same posture as Linux without ksni).
**Deferred-GUI**: icon + menu visible and working. Linux ksni tray
unchanged.

### Phase 8 — Overlay backend (NSPanel)

- [ ] Task 8.1. **New `crates/fono-overlay/src/backends/macos.rs`**:
      non-activating, click-through, always-on-top borderless panel
      (`NSPanel` + `nonactivatingPanel`, `ignoresMouseEvents`, status-bar
      window level), software-rendered from the existing `renderer`
      framebuffer (CGImage/CALayer blit). Same `try_spawn` contract.
- [ ] Task 8.2. **Extend `BackendId` + `candidate_list`** with a macOS row
      (macOS ⇒ `[MacPanel, Noop]`); `FONO_OVERLAY_BACKEND=mac|noop`
      aliases. Linux table unchanged (selection tests updated).
- [ ] Task 8.3. **Headless tier**: selector chooses `MacPanel` → spawn
      fails without WindowServer → falls through to `Noop` (mirrors the
      Linux no-display path); selection unit tests cover the macOS table.
      **Deferred-GUI**: click-through, no focus steal, no Dock/Cmd-Tab
      presence, correct positioning on the primary display.

**Phase 8 gate (headless)**: overlay compiles, candidate table tested,
WindowServer-less fallback to noop verified over SSH. **Deferred-GUI**:
overlay actually painting during recording. Linux wlr/X11/noop backends
unchanged.

### Phase 9 — Install, autostart, permissions onboarding

- [ ] Task 9.1. **`Installer` trait split** (shared with Windows plan Task
      1.6). macOS impl: binary to `~/Applications/fono/` or
      `/usr/local/bin` (decide; no sudo for the user-local path), LaunchAgent
      plist at `~/Library/LaunchAgents/org.fono.daemon.plist`
      (`RunAtLoad`, `KeepAlive`).
- [ ] Task 9.2. **`fono uninstall`** removes the LaunchAgent + binary, keeps
      `~/.config/fono` (config dir stays XDG-shaped via the existing dirs
      handling — verify where `dirs` maps config/data on macOS and pin it
      in `docs/build-macos.md`).
- [ ] Task 9.3. **Permissions onboarding**: `fono doctor` (and first-run)
      must explain the two TCC grants (Microphone, Accessibility) with
      System Settings deep links (`x-apple.systempreferences:` URLs), since
      nothing works until the user grants them.

**Phase 9 gate (headless)**: `fono install` places the binary + LaunchAgent
plist correctly and `launchctl print`/`bootstrap` confirms registration for
the SSH user; `fono uninstall` reverses it; doctor lists the missing TCC
grants. **Deferred-GUI**: daemon + menu-bar icon after a real login. Linux
install layer byte-identical.

### Phase 10 — `fono update` on macOS

- [ ] Task 10.1. **Asset naming** via the `current_asset_name()` seam
      (Windows plan Task 1.7): `fono-vX.Y.Z-aarch64-apple-darwin`.
- [ ] Task 10.2. **Self-replacement**: plain rename works on macOS (unix
      semantics) — but replacing a signed running binary invalidates the
      ad-hoc signature cache; re-sign check + relaunch path verified.
- [ ] Task 10.3. **Linux update flow regression check.**

**Phase 10 gate**: `fono update` swaps the binary and relaunches on macOS.

### Phase 11 — Release workflow artefact

- [ ] Task 11.1. **`release.yml` row**: `macos-15` runner, the single
      **Metal** variant (`--features accel-metal`, per the artefact-shape
      decision), `aarch64-apple-darwin`, fetch-onnxruntime,
      `ORT_CXX_STDLIB=c++`, release-slim build, asset + `.sha256`,
      `SHA256SUMS` inclusion, ELF-only steps skipped.
- [ ] Task 11.2. **Docs**: README install table row; `docs/build-macos.md`
      complete; CHANGELOG entry.
- [ ] Task 11.3. **Universal binary deferred**: the decided ship shape is a
      single universal (lipo) binary of the one Metal variant — x86_64 +
      aarch64, ~2× asset size (~32 MiB), normal for mac distribution.
      Blocked on the `x86_64-apple-darwin` onnxruntime pin (still unbuilt
      on the mirror); arm64-only until then. ggml's runtime CPU-backend
      fallback covers Intel Macs where the Metal backend can't initialize.

**Phase 11 gate**: tagged release publishes the first macOS asset.

### Phase 12 — Promote macOS CI to gating + size budget

- [ ] Task 12.1. **Drop `continue-on-error`.**
- [ ] Task 12.2. **Mach-O dylib allowlist** via `otool -L` (system
      frameworks + libSystem only; the static-link posture carries over).
- [ ] Task 12.3. **Size budget**: measure, pin a ceiling analogous to the
      Linux 25 MiB gate (ADR 0022 amendment), enforce in CI.
- [ ] Task 12.4. **ROADMAP.md**: move the macOS half of "macOS + Windows"
      to Shipped with the release tag.

**Phase 12 gate**: macOS CI gating; budget enforced; first official
macOS release out.

## Verification Criteria

- Linux `release-slim` size and four-entry NEEDED set unchanged at every
  phase boundary (`./tests/check.sh --size-budget`).
- Linux workspace tests green at every phase boundary (pre-commit gate).
- Every phase's **headless gate** proven over SSH; the aggregated
  **deferred-GUI checklist** lives in `docs/build-macos.md` and is the
  pre-release manual test script for whoever first sits at a Mac.
- Deferred (GUI): `fono setup → cloud STT key → hotkey → speak → text
  injected` and `F8 → ask → hear reply` end-to-end.
- `fono install / uninstall / update / doctor / use` work on macOS (headless
  provable).
- macOS dylib imports within the Phase 12 allowlist; size within budget.
- A Linux-only release can be tagged at any phase boundary before Phase 12.

## Potential Risks and Mitigations

1. **TCC permissions can never be granted on this machine.** Microphone,
   Accessibility, and Input Monitoring prompts appear only in a console
   GUI session; our access is headless SSH as `root`, the Mac is in
   another country, and no screen sharing / human is available. Accepted
   consequence: phases 4–9 land with headless-tier verification only
   (constraint 8) — graceful-degradation paths become first-class test
   subjects, and the deferred-GUI checklist is the release-blocking manual
   pass on some other, physically accessible Mac before the artefact is
   advertised as tested. Do **not** hack the TCC database (SIP) to fake
   grants — it wouldn't reproduce real user posture.
2. **GUI subsystems need a main-thread event loop** (NSStatusItem, NSPanel).
   The daemon currently has no macOS main-thread pump. Mitigation: design
   Phases 7+8 together around one shared AppKit event loop; the overlay
   backend thread contract (`try_spawn` + waker) already tolerates
   backend-owned loops. Note: over headless SSH these subsystems also lack
   WindowServer access entirely — the code must treat "no WindowServer"
   as a normal runtime condition, which conveniently is also the honest
   headless test.
3. **Vendored C++ (whisper/llama/ggml) under Xcode clang.** Fully
   de-risked: both compiled during the Phase 1 probe, and the `accel-metal`
   build links, runs, and transcribes correctly (Phase 3 benchmarks).
4. **Unbundled-binary notifications.** `mac-notification-sys` (notify-rust's
   backend) needs a bundle identifier for reliable delivery on modern
   macOS. Mitigation: verify in Phase 3 smoke; if broken, fall back to
   `osascript display notification` (zero deps, mirrors the Linux
   notify-send subprocess pattern) and record the decision.
5. **`dirs`-crate path drift** (`~/Library/Application Support` vs
   `~/.config`) could scatter config/history. **Closed 2026-07-03**: the
   Phase 3 smoke pinned the actual paths — macOS resolves the same
   XDG-style dotfiles as Linux (`~/.config/fono` etc., table in
   `docs/build-macos.md`); any future remap is a deliberate migration.
6. **Root-owned sandbox** (`/var/root/fono-dev`) differs from real user
   posture (paths, permissions, launchd domain). Mitigation: keep CLI/path
   assertions `$HOME`-relative in tests; Phase 9 exercises `launchctl`
   against the SSH user's domain and documents the delta; the deferred-GUI
   pass covers the real-user posture.

## Alternative Approaches

1. **Cross-compile from Linux (osxcross / zig cc)**: rejected for the build
   of record — Apple SDK licensing and vendored-C++ toolchain friction;
   native Xcode clang already works over SSH.
2. **GitHub Actions macOS runners as the only mac environment**: viable for
   CI (Phases 2, 11, 12) but a 5–15 min loop for porting work; the remote
   Mac is the inner loop.
3. **Tauri/Swift wrapper app**: rejected — Fono's whole thesis is one native
   Rust binary; AppKit access via objc2/known crates suffices.
4. **cpal everywhere (drop parec on Linux)**: explicitly rejected before
   (workspace Cargo.toml rationale); macOS simply defaults to the
   already-existing cpal feature.
