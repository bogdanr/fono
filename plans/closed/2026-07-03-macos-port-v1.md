# macOS Port — Remote Dev Setup and Build Plan

## Status: Completed

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

- [x] Task 4.1. **Default `cpal-backend` on macOS** for `fono-audio`
      (same mechanism the Windows plan Phase 5.1 sketches; Linux stays
      parec/paplay). Note pre-existing cpal-feature clippy debt
      (status.md 2026-06-17) — fix what the gate needs.
      *Done via a `[target.'cfg(target_os = "macos")'.dependencies]`
      table in `crates/fono/Cargo.toml` (target tables don't unify
      off-target ⇒ Linux byte-identical, `Cargo.lock` unchanged). The
      cpal capture stream is `!Send` on macOS — it now lives on a
      dedicated keeper thread. All 7 clippy-debt lints in the cpal
      playback worker fixed. Bonus: `AudioStack::CoreAudio` variant
      with osascript output-mute (verified headless), so auto-mute
      works on macOS too.*
- [x] Task 4.2. **Mic permission (TCC).** First capture prompts for
      microphone access — only in a GUI session, and per-binary. Over
      headless SSH there is no prompt and no way to grant it: verify the
      *failure* path instead — capture must degrade gracefully (clear
      error, no hang, no crash) and `fono doctor` must say exactly what to
      grant and where. Deferred-GUI: the actual grant + first capture.
      *Headless tier done: this Mac Studio has no mic hardware at all
      (`system_profiler SPAudioDataType` lists only speakers), which
      exercises the same failure path — clean error, no hang. Capture
      errors and the doctor "no inputs" hint now name System Settings →
      Privacy & Security → Microphone on macOS.*
- [x] Task 4.3. **Capture round-trip — headless tier**: `fono record`
      over SSH exercises device open → TCC denial → error path; the true
      mic → STT → transcript round-trip is deferred-GUI. The STT half is
      already proven via `fono transcribe` (Phase 3).
      *Headless tier done (no-device error path; see 4.2). Deferred-GUI:
      live mic round-trip on a Mac that has a microphone.*
- [x] Task 4.4. **Playback round-trip**: `fono speak` through CoreAudio.
      Audio *output* needs no TCC grant, so this should be provable over
      SSH (exit code + no error; nobody is there to hear the speaker).
      *Done: `fono speak stream` synthesised and played to "Mac Studio
      Speakers" via the cpal worker, rc=0, ring drained to completion.*
- [x] Task 4.5. **Mic enumeration** for the tray/doctor via cpal
      `input_devices()` (replaces `pactl list short sources`) — device
      *listing* is expected to work headless; confirm.
      *Done: `AudioStack::CoreAudio` routes `list_input_devices()` to
      the cpal enumerator; doctor prints "Audio stack : CoreAudio" and
      an honest empty-inputs line on this micless machine.*

**Phase 4 gate (headless)**: cpal backend compiles and enumerates; playback
path returns success; capture fails gracefully with actionable doctor
guidance. **Deferred-GUI**: mic grant + live voice round-trip.
Linux audio path untouched.

### Phase 5 — Global hotkeys

- [x] Task 5.1. **Backend decision: `global-hotkey`, and it's free.** The
      crate was *not* new — the Linux X11 listener is already built on
      `global-hotkey` 0.6 (`crates/fono-hotkey/src/listener.rs`), so it is
      in the shipped Linux binary's graph today; its Carbon backend
      (`RegisterEventHotKey`) compiles only on macOS. Zero Linux size
      cost, tens of KB on darwin, no `Cargo.lock` change, and — decisive
      for UX — Carbon hotkeys need **no TCC permission at all**, unlike a
      CGEventTap shim (Input Monitoring prompt, untestable headless).
      Trade-off recorded: Carbon swallows the registered key (fine for
      F7/F8/Esc-as-cancel).
- [x] Task 5.2. **Implemented behind the existing listener seam.** The
      generic `listener.rs` (GlobalHotKeyManager + FSM) runs unmodified on
      macOS (the `x11-dl`/`ashpd` Linux target table + portal gating had
      already landed in Phase 1). `detect.rs` now short-circuits to the
      `global-hotkey` listener on darwin (display env vars carry no
      signal), and `is_graphical_session()` gained a real macOS probe —
      `CGSessionCopyCurrentDictionary()` via raw framework FFI, zero new
      crates — so the daemon's headless gating is truthful on darwin.
- [x] Task 5.3. **Esc-to-cancel** needs no port: the transient
      `EnableCancel`/`DisableCancel` register/unregister calls go through
      the same `GlobalHotKeyManager` seam and were exercised by the probe
      example on the Mac (register + unregister Esc succeeded).
- [x] Task 5.4. **Linux regression check**: portal + X11 code untouched
      (only cfg-gated); fmt/clippy/36 test suites green, `Cargo.lock`
      unchanged.

**Phase 5 gate (headless)**: PASSED. macOS hotkey module compiles, unit
tests pass (darwin clippy clean, full workspace suites green), and —
answer recorded — Carbon `RegisterEventHotKey` **succeeds even over
headless SSH as root** (probe example registered F7/F8/Esc and
unregistered cleanly; no WindowServer session required for
registration). The daemon still gates the listener on
`is_graphical_session()`, which correctly reports headless over SSH.
**Deferred-GUI**: F7/F8/Esc actually *firing* (event delivery does need
a console session).

### Phase 6 — Text injection + focus detection

- [x] Task 6.1. **enigo on macOS** (CGEvent). The existing `enigo-backend`
      feature is now default on macOS via a target table in
      `crates/fono/Cargo.toml`; enigo's darwin deps (core-graphics,
      icrate, objc2 0.5) were already in `Cargo.lock` — no new-to-project
      crates, zero Linux cost. `detect_auto` short-circuits to enigo on
      darwin (clipboard-only when the feature is off); the Linux
      display-server cascade moved to a `not(macos)` fn. Bench finding
      recorded: `Enigo::new()` + `text()` **return Ok even over headless
      SSH as root** — CGEventPost accepts the events; whether keystrokes
      *land* needs the deferred-GUI pass, and the un-grantable-headless
      Accessibility denial path could therefore not be triggered from
      SSH. Error strings for enigo failures and the no-backend case name
      System Settings → Privacy & Security → Accessibility on macOS.
- [x] Task 6.2. **Clipboard fallback.** No target-table move needed:
      arboard's `wayland-data-control` feature only activates
      target-gated deps inside arboard itself — on darwin it compiles to
      the NSPasteboard backend (verified: darwin tree pulls only objc2
      crates). Added `pbcopy` as the macOS subprocess fallback (ships
      with the OS; needs no display env) and `pbpaste` to test-inject's
      readback. Bench finding: **both NSPasteboard and pbcopy need a
      logged-in user session** — over headless SSH as root they fail
      cleanly (pboard daemon is per-login); the macOS clipboard error
      message now says exactly that.
- [x] Task 6.3. **Focus detection**: `NSWorkspace.frontmostApplication`
      via objc2-app-kit (already in `Cargo.lock` at the exact version
      through arboard's darwin backend — net-zero, no flag needed).
      Populates `window_class` with the localized app name (bundle-id
      fallback) and `window_pid`; window *titles* need Screen Recording
      TCC and are deliberately left `None`. Classifier gained
      mac-flavoured classes (Terminal, iTerm2, ghostty, Warp, Safari,
      Google Chrome, Brave Browser, Microsoft Edge, Mail, Messages,
      Telegram); "Code"/"Slack"/"Discord" already matched
      case-insensitively. Headless: returns an empty `FocusInfo` (no
      frontmost app without WindowServer), never an error.
- [x] Task 6.4. **Headless tier**: `fono test-inject` over SSH exercised
      the cascade both ways — default (enigo accepted, pbcopy denied
      with per-tool diagnostics) and `FONO_INJECT_BACKEND=none` (full
      documented-order degradation: no backend → arboard fails → pbcopy
      fails → combined error with macOS guidance). **Deferred-GUI**:
      verification against three apps (TextEdit, Safari address bar,
      VS Code).

**Phase 6 gate (headless)**: PASSED — inject cascade compiles, degrades
in the documented order, and the error strings explain the Accessibility
grant. **Deferred-GUI**: dictation landing at the cursor in the reference
apps; per-app rules firing; the actual Accessibility-denial UX (headless
CGEventPost accepts events, so denial can only be observed in a GUI
session). Linux inject cascade unchanged (fmt/clippy in both feature
configs/36 suites green; `Cargo.lock` gained only edges to
already-present packages).

### Phase 7 — Menu-bar (tray) icon

- [x] Task 7.1. **Backend decision (flag first).** ~~`tray-icon` crate
      (muda-based, shared with the Windows plan) vs objc2 NSStatusItem
      direct.~~ **Decided 2026-07-04 (Option C): shared menu-model
      refactor + hand-rolled objc2 `NSStatusItem` renderer.** The real
      duplication risk was never the backend crate — it's that the
      ~600-line menu builder in `fono-tray` was written directly
      against ksni's types, so every backend would re-encode the menu.
      Decision:
      - **One platform-neutral menu model** (declarative tree of
        label/checked/disabled/action nodes) built in exactly one
        shared function for all OSes; per-OS backends become dumb
        one-time interpreters that never change when menu content
        evolves. Linux ksni becomes the first interpreter
        (byte-identical behaviour); this also discharges Windows plan
        Task 1.1 early.
      - **macOS renderer: objc2 `NSStatusItem` shim.** Zero new crates
        (`objc2 0.6` / `objc2-app-kit 0.3` / `objc2-foundation 0.3`
        are already darwin edges via the Phase 6 focus prober) vs
        `tray-icon 0.24` = three new-to-project crates (`tray-icon`,
        `muda`, `png`), est. ~0.5 MiB (verified against its crates.io
        dep list). NSStatusItem needs the AppKit main thread + run
        loop either way (tray-icon assumes a tao/winit pump we don't
        have); the in-house main-thread host serves both the tray and
        Phase 8's NSPanel overlay.
      - **Windows keeps `tray-icon`** per its plan (independent,
        unaffected; muda's win backend is thin over windows-sys) —
        with the model split its renderer is trivial too, and a
        hand-rolled `Shell_NotifyIcon` renderer stays open as a
        size-driven revisit at Windows-port time. Linux stays ksni per
        the standing workspace rationale.
      - **Web settings UI stays a separate front end** (discussed
        2026-07-04): the tray and the web config page already converge
        on the same daemon control plane (validate → persist TOML
        atomically → hot-reload), which is where consistency is
        guaranteed. The tray is a curated quick-action subset plus
        runtime-only items (recent dictations, live mDNS/mic lists,
        update entries) that don't exist in the config schema; forcing
        one UI description across a 2 s-polled native menu and a
        schema-driven HTML form would couple them for zero code
        savings.
- [x] Task 7.2. **Menu-model refactor** (`fono-tray`): extract the
      platform-neutral model + single shared builder; re-implement the
      ksni backend as a model interpreter. Gate: Linux behaviour
      byte-identical (fmt/clippy/tests green; menu structure snapshot
      test pins the tree). *Done 2026-07-04: `fono-tray::menu` holds
      the `MenuNode` tree + the one shared `build()` (a faithful
      transcription of the old ksni builder, all ~10 submenus); the
      ksni backend shrank to a ~40-line recursive interpreter.
      Snapshot tests pin the top-level structure and the load-bearing
      details (active markers, checkmarks, disabled sentinels,
      empty-state rows) and compile on every OS, so cross-platform
      menu parity is CI-tested. Windows plan Task 1.1 discharged
      early.*
- [x] Task 7.3. **macOS `NSStatusItem` renderer** behind the same model:
      ~~template-image icon (menu-bar-native dark/light),~~ full menu tree,
      2 s poll repaint parity. ~~Embed the icon via `include_bytes!`.~~
      Caveat: NSStatusItem requires the main thread + a running event
      loop; reconcile with the daemon's thread layout (same main-thread
      event pump the overlay backend needs — design them together with
      Phase 8). *Done 2026-07-04: `fono-tray::backend_macos` interprets
      the shared `MenuNode` tree into `NSMenu` (~40-line recursive
      renderer, mirror of the ksni one) with a target/action bridge
      (`NSMenuItem` tag → `TrayAction` registry, swapped atomically per
      render). Main-thread pump: `fono::main()` on darwin — daemon
      invocation in a graphical session only — moves the daemon to a
      worker thread and parks the real main thread in
      `NSApplication::run()` with the `Accessory` activation policy
      (no Dock icon); jobs ship via libdispatch's main queue
      (`dispatch_async_f`, part of libSystem — zero crates), which the
      run loop drains as they arrive, so delivery is event-driven
      rather than timer-polled; the same pump is the Phase 8
      overlay's host. Poll/diff loop stays on tokio, identical cadence
      to ksni; unchanged ticks ship nothing. Icon: deliberately NOT a
      template image — the tint carries FSM state (same
      `menu::state_color` palette as Linux), rendered at runtime into
      an `NSBitmapImageRep` (no embedded asset needed). Headless SSH /
      non-daemon invocations: no pump installed → `spawn` warns once,
      returns `false`, daemon runs tray-less (verified on the bench).
      Zero new crates; `objc2`/`objc2-app-kit`/`objc2-foundation`
      became direct darwin deps of `fono-tray` (already in the graph
      via Phase 6).*

**Phase 7 gate (headless)**: tray backend compiles and, with no
WindowServer access over SSH, fails gracefully into the existing no-tray
mode (daemon keeps running — same posture as Linux without ksni).
**Deferred-GUI**: icon + menu visible and working. Linux ksni tray
unchanged.

### Phase 8 — Overlay backend (NSPanel)

- [x] Task 8.1. **New `crates/fono-overlay/src/backends/macos.rs`**:
      non-activating, click-through, always-on-top borderless panel
      (`NSPanel` + `nonactivatingPanel`, `ignoresMouseEvents`, status-bar
      window level), software-rendered from the existing `renderer`
      framebuffer (CGImage/CALayer blit). Same `try_spawn` contract.
      *Done 2026-07-04: a `fono-overlay-mac` worker thread owns the
      `RendererState` + `OverlayCmd` channel (same command handling as
      the winit backend) and renders each frame into an owned ARGB
      `Vec<u32>`; frames go to the AppKit main thread through a
      newest-wins single-slot mailbox so the pump can never back up.
      The main-thread blit wraps the buffer via `NSBitmapImageRep` →
      `NSImage` → `NSImageView` (image sized in points so retina maps
      1 buffer px : 1 device px); the panel is Borderless +
      NonactivatingPanel, level 25 (`NSStatusWindowLevel`),
      `ignoresMouseEvents`, clear background, no shadow, CanJoinAllSpaces
      + Stationary + IgnoresCycle, bottom-centred on its screen (Cocoa's
      bottom-left origin maps `BOTTOM_OFFSET` directly). The backing
      scale is probed from `NSScreen` at spawn and kept in sync from
      panel truth on every blit. `fono-overlay` does not depend on
      `fono-tray`: the binary wires the pump's `dispatch_main` into
      `backends::macos::set_main_thread_dispatcher` at daemon startup.
      Blit jobs ride the GCD main queue, so the panel repaints at the
      producers' cadence (≈20–30 fps level/FFT ticks) — the run loop
      drains jobs as they arrive; the mailbox only coalesces when the
      main thread is genuinely busy (e.g. menu tracking).*
- [x] Task 8.2. **Extend `BackendId` + `candidate_list`** with a macOS row
      (macOS ⇒ `[MacPanel, Noop]`); `FONO_OVERLAY_BACKEND=mac|noop`
      aliases. Linux table unchanged (selection tests updated).
      *Done 2026-07-04 (landed with the Phase 8 prep commit): `HostOs`
      discriminator keeps the per-OS tables unit-testable from every
      platform; `mac|macos|mac-panel|nspanel` parse aliases; doctor's
      probe + capability rows cover `mac-panel`.*
- [x] Task 8.3. **Headless tier**: selector chooses `MacPanel` → spawn
      fails without WindowServer → falls through to `Noop` (mirrors the
      Linux no-display path); selection unit tests cover the macOS table.
      **Deferred-GUI**: click-through, no focus steal, no Dock/Cmd-Tab
      presence, correct positioning on the primary display.
      *Headless answer 2026-07-04: over SSH the daemon takes the plain
      (no-pump) path, so `try_spawn` returns `NotAvailable("AppKit
      main-thread pump not installed…")` and the selector logs the fall
      to `noop` — no WindowServer probe needed, the pump-installed check
      subsumes it.*

**Phase 8 gate (headless)**: overlay compiles, candidate table tested,
WindowServer-less fallback to noop verified over SSH. **Deferred-GUI**:
overlay actually painting during recording. Linux wlr/X11/noop backends
unchanged. *Gate met 2026-07-04: darwin clippy `-D warnings` clean, 36
test suites 0 failed, daemon smoke shows `mac-panel` skipped → `noop`;
Linux fmt/clippy/36 suites green, `Cargo.lock` gains darwin-scoped
edges only (objc2* already in the graph).*

### Phase 9 — Install, autostart, permissions onboarding

- [x] Task 9.1. **`Installer` trait split** (shared with Windows plan Task
      1.6). Done as cfg-dispatched platform modules with one shared public
      surface (`crates/fono/src/install/{mod,linux,macos}.rs`) — a literal
      trait would add ceremony with exactly one impl per compiled binary.
      macOS impl (per-user, no sudo): **`~/Applications/Fono.app`** bundle
      assembled around the running binary (fixed `org.fono.app` bundle id,
      `LSUIElement`, mandatory `NSMicrophoneUsageDescription`), LaunchAgent
      at `~/Library/LaunchAgents/org.fono.daemon.plist` (`RunAtLoad`,
      `KeepAlive.SuccessfulExit=false`, `LimitLoadToSessionType Aqua` so
      SSH logins never spawn it), `launchctl bootstrap gui/$UID` when a
      GUI session exists (headless: starts at next login), best-effort
      `/usr/local/bin/fono` symlink. The Task 11.4 install side landed
      here too: a `fono-local-signing` self-signed cert in a dedicated
      always-unlocked keychain signs the bundle; bench-proven that the
      designated requirement (`identifier "org.fono.app" and certificate
      leaf = H"…"`) is **byte-identical across re-installs**, which is
      the grant-once property. Bench facts: `security add-trusted-cert`
      is denied headless (GUI authorization) and `find-identity -v`
      filters the untrusted cert out — but `codesign` signs with it
      regardless and TCC never walks the trust chain, so the installer
      skips trust settings and probes identities without `-v`.
- [x] Task 9.2. **`fono uninstall`** boots the agent out (`gui/` + `user/`
      domains, best-effort), removes plist + bundle + our symlink +
      `~/.cache/fono`, keeps `~/.config/fono`, `~/.local/share/fono` and
      the signing keychain (re-install ⇒ same identity ⇒ old grants still
      match). Round-trip verified on the bench. Homebrew-managed binaries
      are refused (`is_package_managed` now knows `/opt/homebrew` +
      `/Cellar/`).
- [x] Task 9.3. **Permissions onboarding**: new zero-crate
      `fono_inject::permissions` module — `accessibility_trusted()`
      (silent `AXIsProcessTrusted`) and `accessibility_prompt()`
      (`AXIsProcessTrustedWithOptions(prompt)`, raw
      ApplicationServices/CoreFoundation FFI). `fono doctor` gained an
      Accessibility row with the Settings deep link; the daemon probes at
      startup — in a GUI session it raises the native dialog once (macOS
      dedupes by app identity), headless it logs the `open
      "x-apple.systempreferences:…"` command instead. Mic prompt needs no
      code: the OS raises it on first capture, and the bundle's usage
      string satisfies the bundled-app requirement.

      **Permission-UX facts (researched 2026-07-04; this is the whole
      game on macOS):**
      - Injection (CGEventPost, what enigo uses) is gated by the
        **Accessibility** TCC service. It is a **one-time grant**, not a
        per-use confirmation: the user flips one toggle in System
        Settings → Privacy & Security → Accessibility and is never asked
        again. Apple provides no per-keystroke dialog and no way to skip
        the grant — every dictation app that types for you (Wispr Flow,
        superwhisper, MacWhisper…) requires exactly this same toggle;
        Wispr Flow's own onboarding is a Permissions page requesting
        Accessibility + Microphone, and their MDM docs pre-grant
        Accessibility via a PPPC profile so users see only the mic
        prompt.
      - Microphone is a separate **native one-click prompt**
        (Allow/Don't Allow) on first capture. Accessibility cannot be
        granted from a dialog — the user must be sent to the Settings
        toggle. So the theoretical UX floor on macOS is: one click (mic)
        + one toggle (Accessibility), once ever. Match it, don't fight
        it.
      - Without the grant, CGEventPost **silently drops** events (no
        error — confirmed on the bench: `text()` returns Ok headless).
        First-run must therefore probe explicitly with
        `AXIsProcessTrustedWithOptions(kAXTrustedCheckOptionPrompt)`,
        which both answers truthfully and raises the system dialog that
        deep-links to the right pane; poll until granted and show a
        green tick (`AXIsProcessTrusted` — ApplicationServices is
        already linked). Never "inject and hope".
      - **TCC grants are keyed to the code-signing identity.** An
        ad-hoc/linker-signed binary gets a new identity every build, so
        the grant would break on **every update** — the single most
        common "it stopped working" complaint against mac dictation
        apps. A **stable** signing identity (+ consistent bundle id,
        shipped as a `fono.app` bundle so the grant attributes to fono
        rather than to Terminal) is a **hard prerequisite** for the
        polished UX, not packaging polish. Achieved without the Apple
        Developer Program via the local self-signed-cert scheme in Task
        11.4. Feeds Tasks 10.2 and 11.4.
      - Zero-permission fallback stays: clipboard write needs no TCC at
        all (synthetic Cmd+V would need Accessibility again, so don't) —
        dictate → pasteboard → notify "paste with Cmd+V". Fono already
        degrades in this order.

**Phase 9 gate (headless)**: ✅ `fono install` assembles + signs the bundle
and writes the LaunchAgent (plutil-lint clean); bootstrap correctly skips
headless (gui domain absent — recorded; the positive `launchctl print`
check moves to deferred-GUI with the login-session item); `fono uninstall`
reverses everything; doctor names the missing Accessibility grant with the
deep link. **Deferred-GUI**: daemon + menu-bar icon after a real login,
agent visible in `launchctl print gui/$UID`, the two TCC prompts
end-to-end. Linux install layer byte-identical (module split only).

### Phase 10 — `fono update` on macOS

- [x] Task 10.1. **Asset naming** via the `current_asset_name()` seam
      (Windows plan Task 1.7): darwin selects
      `fono-vX.Y.Z-aarch64-apple-darwin` (single Metal variant — no
      cpu/gpu split, matching the Phase 3 artefact decision, so the
      GPU-upgrade suggestion machinery is Linux-only by construction).
      Unit tests pin the darwin asset name and the absence of an
      upgrade suggestion.
- [x] Task 10.2. **Self-replacement**: unix rename semantics reused
      unchanged; the darwin-only step is the post-swap **re-sign hook**
      — after the updater swaps the binary, both apply sites (CLI
      `fono update` and the tray-triggered daemon path) call
      `install::resign_after_update()`, which re-signs the enclosing
      `Fono.app` with the persistent `fono-local-signing` identity so
      the designated requirement — and therefore the one-time
      Accessibility grant — survives the update. Bench-proven over
      SSH: swap breaks the bundle seal (`codesign --verify` fails),
      the re-sign hook restores it, and `codesign -d -r-` is
      byte-identical before/after. Bare-binary installs (no bundle)
      skip the hook silently; a failed re-sign warns with the
      re-grant + `fono install` recovery path instead of failing the
      update.
- [x] Task 10.3. **Linux update flow regression check** — the hook is
      a no-op shim off darwin; fmt/clippy/36 suites green, updater
      asset-selection tests unchanged.

**Phase 10 gate**: met at the headless tier — `fono update --check`
resolves the darwin asset name and reports truthfully (no darwin asset
published until Phase 11); the swap + re-sign + verify sequence proven
on the bench. The end-to-end swap-and-relaunch against a real
published release lands with Phase 11's artefact and is noted on the
deferred checklist in `docs/build-macos.md`.

### Phase 11 — Release workflow artefact

- [x] Task 11.1. **`release.yml` row**: `macos-15` runner, the single
      **Metal** variant (`--features accel-metal`, per the artefact-shape
      decision), `aarch64-apple-darwin`, fetch-onnxruntime,
      `ORT_CXX_STDLIB=c++`, release-slim build, asset + `.sha256`,
      `SHA256SUMS` inclusion, ELF-only steps skipped (Linux deps and
      NEEDED gate now `if: runner.os == 'Linux'`; a Mach-O analogue
      verifies every `LC_LOAD_DYLIB` is a system framework or
      `/usr/lib` system library). The asset keeps the full triple
      (`fono-vX.Y.Z-aarch64-apple-darwin`; the darwin arm is matched
      before the bare `aarch64-*` one to avoid colliding with the
      Linux arm asset). Dry-run on the bench 2026-07-04: release-slim
      + accel-metal + `ORT_CXX_STDLIB=c++` builds (15.40 MiB), the
      exact dylib gate passes (17 imports, all allowlisted incl.
      Metal/MetalKit/AppKit), and the artefact transcribes the
      fixture correctly on Metal.
- [x] Task 11.2. **Docs**: README "other ways to install" row (download +
      `fono install`, honest headless-tested caveat); CHANGELOG
      `[Unreleased]` section for the port; `docs/build-macos.md` carries
      the per-phase smoke log + deferred-GUI checklist.
- [x] Task 11.3. **Universal binary deferred** (decision recorded): the
      decided ship shape is a
      single universal (lipo) binary of the one Metal variant — x86_64 +
      aarch64, ~2× asset size (~32 MiB), normal for mac distribution.
      Blocked on the `x86_64-apple-darwin` onnxruntime pin (still unbuilt
      on the mirror); arm64-only until then. ggml's runtime CPU-backend
      fallback covers Intel Macs where the Metal backend can't initialize.
- [x] Task 11.4. **Grant-once signing pipeline, zero-cost posture** (the
      mechanism behind Task 9.3's "grant survives updates"). **All
      three pieces are now live**: install side (Phase 9 —
      cert/keychain/bundle/sign in `crates/fono/src/install/macos.rs`,
      designated requirement bench-stable across re-installs), the
      `fono update` re-sign hook (Phase 10 — requirement bench-stable
      across the swap), and the release-asset/docs plumbing (11.1–11.3
      above — CI ships the plain unsigned asset + `.sha256`, no
      secrets). TCC stores
      the app's *designated requirement* at grant time and re-checks it
      on every launch — identical signature ⇒ grant persists. Decision
      2026-07-04: **no Apple Developer Program** (USD 99/yr rejected —
      no users yet). The free alternative that still achieves
      grant-once:
      1. **Local stable identity at install time.** For an ad-hoc
         signature the designated requirement degenerates to the
         per-build CDHash — grants break on every update. But TCC keys
         on any *certificate-based* requirement equally well, so
         `fono install` creates a **self-signed code-signing
         certificate once** in the user's login keychain
         (`fono-local-signing`) and re-signs the app with it;
         `fono update` re-signs the swapped binary with the *same* local
         cert after `codesign --verify`ing the download hash. Same cert
         every time ⇒ stable designated requirement ⇒ Accessibility
         toggled once, survives all updates. (Signing happens on the
         user's machine with the user's own cert — nothing secret ships
         in the repo or CI.)
      2. **Gatekeeper without notarization**: quarantine only attaches
         to browser downloads. Primary install channel is **Homebrew**
         (`brew install`ed binaries carry no quarantine → no Gatekeeper
         wall) or `curl | fono install`-style bootstrap; for manual
         downloads, document the one-time right-click-Open (or
         `xattr -d com.apple.quarantine`). CI ships the plain ad-hoc
         arm64 asset + `.sha256` — no certs, no secrets.
      3. **`fono.app` bundle** with fixed bundle id (`org.fono.app`)
         and `Info.plist` usage strings (`NSMicrophoneUsageDescription`
         is mandatory — a bundled app hard-crashes on first mic access
         without it) — still required so the grant attributes to fono
         rather than to Terminal; the bundle is assembled locally by
         `fono install`, not in CI.
      4. **Developer ID + notarization stays a future opt-in** (revisit
         when the project has users/funding): drop-in replacement — CI
         signs/notarizes, the local re-sign step simply becomes a
         no-op. Nothing in the free posture paints us into a corner.

**Phase 11 gate**: tagged release publishes the first macOS asset —
fires automatically at the next `v*` tag; every workflow-side piece is
in place and was dry-run on the bench.

### Phase 12 — Promote macOS CI to gating + size budget

- [x] Task 12.1. **Drop `continue-on-error`.** Done 2026-07-04: the
      `macos` job in `ci.yml` is a blocking gate (name de-suffixed too).
- [x] Task 12.2. **Mach-O dylib allowlist** via `otool -L`. Done
      2026-07-04 as part of the new `size-budget-macos` job: the
      `LC_LOAD_DYLIB` set must equal the bench-verified 17-entry
      allowlist exactly (13 system frameworks + 4 `/usr/lib` system
      libs) — an exact match, not a prefix match, so even a benign new
      system framework is a visible, reviewed event. The gate script
      was executed verbatim on the bench artefact before landing
      (GATE-PASS, 17 imports).
- [x] Task 12.3. **Size budget.** Done 2026-07-04: `size-budget-macos`
      builds the exact release artefact (`release-slim`,
      `aarch64-apple-darwin`, `accel-metal`, pinned onnxruntime,
      `ORT_CXX_STDLIB=c++`) and enforces **≤ 18 MiB (18 874 368 B)**;
      measured 16 143 328 B (15.40 MiB), ~2.6 MiB headroom. Hard cap
      ≤ 20 MiB recorded in the ADR 0022 amendment (2026-07-04); the CI
      row and the ADR live in lockstep.
- [x] Task 12.4. **ROADMAP.md.** Done 2026-07-04 (adapted): the port
      isn't tagged yet, so instead of moving to Shipped the
      "macOS and Windows" horizon entry now states macOS is
      code-complete on `main` and ships with the next release,
      describing what actually shipped (self-signed `Fono.app`, not
      the originally sketched signed `.dmg`). Move to **Shipped** with
      the release tag per the AGENTS.md release rule.

**Phase 12 gate**: macOS CI gating ✅; budget enforced ✅; first official
macOS release goes out with the next tag (release.yml row landed in
Phase 11).

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
