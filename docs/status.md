# Fono — Project Status

Last updated: 2026-05-02

## 2026-05-02 — Release v0.4.0

Tagged v0.4.0. Headline changes:

- **Wyoming Home Assistant wire compliance** + **discovered-server tray
  UX** (~600 LOC; PR #1). Frame format aligned with upstream Python
  Wyoming, `info.asr` array shape, queued-transcribe HA flow, multi-
  channel PCM decode, mDNS auto-addresses, tray submenu for picking a
  remote Wyoming server with hot-reload.
- **CI size-budget gate** pivoted from static-musl to glibc-dynamic +
  NEEDED allowlist (~20 MiB budget; measured at release: 18.08 MB).
- **Artefact-producing runners** pinned to ubuntu-22.04 (glibc 2.35)
  so the binary runs on Ubuntu 22.04+, Debian 12+, Fedora 36+.
- **CI cache key** suffixed with the runner image to prevent
  cross-glibc contamination of cached build-script binaries.
- **CI job names** rewritten for UI clarity (Build & test, Binary
  size & deps audit, License & advisory audit, Release binary).
- **Phase 2.4 (musl ship)** formally deferred. Resurrection path
  documented in ADR 0022 amendment + CHANGELOG.

Release notes: `CHANGELOG.md` `[0.4.0]`.

## 2026-05-02 — Pin build runners to ubuntu-22.04 for older-distro glibc compat

`size-budget` (`.github/workflows/ci.yml`) and the release build matrix
(`.github/workflows/release.yml`) now both pin `runs-on:` to
**`ubuntu-22.04`** (glibc 2.35) instead of `ubuntu-latest` (24.04 →
glibc 2.39). The shipped binary's `GLIBC_2.X` symbol versions are
stamped at link time by the build host's glibc; staying on the older
image keeps the binary compatible with Ubuntu 22.04+, Debian 12+,
Fedora 36+, and any host with glibc ≥ 2.35. The previous
`ubuntu-latest` floor would have silently excluded ~3 years of
supported distros.

The `test` job in `ci.yml` stays on `ubuntu-latest` so we still get
newer-environment regression coverage. Only artefact-producing jobs
need the older glibc pin.

ADR 0022's "Glibc symbol-version surface" note (formerly a
follow-up TODO) is updated to reflect the pinned state.

## 2026-05-02 — CI size-budget pivots from static-musl to glibc-dynamic + NEEDED allowlist

The `size-budget` CI job no longer tries to build a fully-static
`x86_64-unknown-linux-musl` artefact. Eleven post-v0.3.7 commits
(`901e41d..29cc577`, excluding `01e9411`'s unrelated Node 24 bump)
chased a chain of toolchain breakage in `messense/rust-musl-cross`'s
`libgomp.a` — non-PIC archive (vs `-static-pie`), glibc-only `memalign`
and `secure_getenv`, plus link-order-dependent POSIX symbols
(`gethostname`, `strcasecmp`, `getloadavg`) — and abandoned. Each shim
exposed the next layer; the libgomp.a in available musl-cross images
is unfit for purpose without a custom build.

The replacement gate builds `x86_64-unknown-linux-gnu` `release-slim`
on `ubuntu-latest` (mirroring `release.yml`) and asserts:

1. Size ≤ 20 MiB (20 971 520 bytes); measured today: **18 957 120 bytes
   (≈ 18.08 MB)**, ~2 MB headroom.
2. `NEEDED` set is exactly `libc.so.6 libm.so.6 libgcc_s.so.1
   ld-linux-x86-64.so.2`. Modern glibc (≥ 2.34) merges
   `libpthread/librt/libdl` into `libc.so.6` so they don't appear
   separately. Anything else (libgtk, libstdc++, libgomp, libayatana,
   libxdo, libasound, libxkbcommon, libwayland-*) fails the gate.

The dedup invariant (single ggml copy) stays enforced at link time by
`--allow-multiple-definition` in `.cargo/config.toml` (ADR 0018);
release-slim's `strip = "symbols"` removes runtime symbol info, so a
post-strip `nm` check is not possible. Breaking dedup yields
multiple-definition link errors, not silent passes.

Phase 2.4 of `plans/2026-04-30-fono-single-binary-size-v1.md` (musl
ship) is **deferred**. Resurrection path: switch the `llama-cpp-2`
fork to llvm-openmp (libomp is PIC-friendly) **or** pin a PIC-built
`libgomp.a` from GCC sources in our own minimal cross image.

Files: `.github/workflows/ci.yml` (size-budget job rewritten to
glibc/native, with positive NEEDED allowlist), `.cargo/config.toml`
(musl rustflags block deleted), `crates/fono/src/main.rs` (`memalign`
and `secure_getenv` shims deleted),
`plans/2026-04-30-fono-single-binary-size-v1.md` (Tasks 2.3/2.4,
verification criteria, outcome table updated),
`docs/decisions/0022-binary-size-budget.md` (status amended;
Decision/Verification/Trade-offs reframed for glibc-dynamic +
allowlist).

Verification: local `cargo build -p fono --profile release-slim
--target x86_64-unknown-linux-gnu` produced an 18 957 120-byte ELF
with the expected NEEDED set. The gate's bash logic was exercised
locally in both pass (full allowlist) and fail (deliberately tightened
allowlist) paths against that binary.

## 2026-05-01 — Alpine size-budget preserves Rust image PATH

The Alpine-backed size-budget command no longer starts a login shell that can
reset the Docker image PATH before invoking `rustc`. The job now passes the Rust
image toolchain path explicitly and uses a non-login shell, so `rustc`, `cargo`,
`cargo fmt`, and `cargo clippy` resolve before the size-budget script runs.

Verification: `.github/workflows/ci.yml` YAML parsing, extracted shell syntax
validation, and `git diff --check` pass on the current Linux host. A local Docker
smoke test could not run because the Docker daemon is unavailable here.

## 2026-05-01 — GitHub Actions now target Node 24

The CI and Release workflows no longer rely on JavaScript actions that run on the
Node 20 runtime. Cache, upload-artifact, download-artifact, and release-publishing
actions were advanced to their Node 24 majors while checkout was already on the
Node 24-compatible major.

Verification: workflow YAML parsing and `git diff --check` pass on the current
Linux host.

## 2026-05-01 — Alpine size-budget no longer assumes rustup

The first Alpine-backed size-budget run failed before the build because the
`rust:1.88-alpine` image provides the Rust toolchain directly, but not `rustup`.
The job no longer tries to add components with `rustup`; it prints `rustc`,
`cargo`, `cargo fmt`, and `cargo clippy` versions before running the size-budget
script so missing tools fail with a direct diagnostic.

Verification: `git diff --check`, `.github/workflows/ci.yml` YAML parsing, and
shell syntax validation for the Alpine size-budget step pass on the current Linux
host.

## 2026-05-01 — CI musl size-budget now runs in Alpine

The third `main` CI attempt failed in the install-step smoke test because the
Ubuntu host `libstdc++` headers are glibc-configured and are not safe to combine
with `musl-gcc.specs`; `<array>` pulled in glibc-only preprocessor checks before
the actual size-budget build could start. The size-budget job now runs the gate
inside `rust:1.88-alpine`, installing Alpine's native musl C/C++ build toolchain
so C, C++, libstdc++, and the Rust musl target all agree on musl from the start.

Verification: `git diff --check`, `.github/workflows/ci.yml` YAML parsing, and
shell syntax validation for the Docker-backed size-budget step pass on the
current Linux host. A local Docker smoke test could not run because the Docker
client is installed but the daemon is not running in this environment.

## 2026-05-01 — CI musl C++ wrapper now restores standard headers

The follow-up `main` CI run for the v0.3.7 release fix advanced past CMake's
missing `x86_64-linux-musl-g++` probe, then failed while compiling whisper.cpp
because the musl specs file removes the default C++ header search path and
`ggml.cpp` could not include `<array>`. The CI wrapper now keeps the musl specs
file and explicitly restores the host libstdc++ include directories, with an
install-step smoke compile for `<array>` so this failure is caught before the
full size-budget build.

Verification: `git diff --check`, `.github/workflows/ci.yml` YAML parsing, and
shell syntax validation for the patched musl install step pass on the current
Linux host. Full musl size-budget validation remains CI-only here because this
host lacks the musl Rust standard library and musl C/C++ toolchain.

## 2026-05-01 — Live fallback stop now completes batch transcription

When live dictation is enabled but the active STT backend is batch-only, Fono starts
the normal batch capture path as a fallback. The daemon still receives the matching
live-stop event, so the interactive stop handler now checks for and stops that batch
fallback capture instead of immediately marking processing done. This fixes the
"falling back to batch path" case where recording stopped but no transcript was
injected.

The Wyoming server now advertises its ASR program/attribution as `Fono`, matching the
product name, and logs each remote transcription request at INFO level when processing
starts and when the backend returns.

Verification: `cargo fmt --all -- --check`, `cargo test -p fono-net --test
wyoming_server_round_trip`, `cargo test -p fono-net
wyoming::server::tests::build_info_advertises_models`, `cargo check -p fono
--features interactive`, and `git diff --check` pass on the current Linux host.

## 2026-05-01 — Wyoming ASR flow now matches Home Assistant event ordering

Home Assistant's Wyoming ASR client sends `transcribe` first to select the
language/model, then streams `audio-start` / `audio-chunk` events, and expects the
`transcript` response when `audio-stop` arrives. Fono previously treated
`transcribe` as the terminal event, so it invoked Whisper immediately with zero
collected samples and closed the connection with `Input sample buffer was empty`.

The Wyoming server now queues an early `transcribe` request until `audio-stop`,
continues to support Fono's existing audio-first flow, accepts audio chunks even
when a client omits `audio-start`, and decodes int16 LE mono/stereo payloads using
the format fields from each `audio-chunk`. The probe's optional ASR flow now sends
the Home Assistant ordering so it catches this compatibility issue.

Verification: `cargo fmt --all -- --check`, `python3 -m py_compile
tests/wyoming_protocol_probe.py`, `cargo test -p fono-net --test
wyoming_server_round_trip`, `cargo test -p fono-net-codec -p fono-net -p fono-stt
wyoming`, and `cargo check -p fono-net-codec -p fono-net -p fono-stt` pass on the
current Linux host. The deployed server at `192.168.0.79:10300` still times out on
the updated Home Assistant-style probe until rebuilt/restarted with this patch.

## 2026-05-01 — Wyoming describe/info is now Home Assistant-compatible

Home Assistant's Wyoming loader sends `describe`, waits for an `info` event, and
parses `info.asr`, `info.tts`, `info.wake`, `info.handle`, `info.intent`,
`info.mic`, and `info.snd` as service arrays. Fono's Wyoming server previously
returned `asr` as a single object and omitted the empty service families, which
made Home Assistant's `Info.from_event` reject the response. The codec now writes
canonical Wyoming frames with `version` and `data_length` data blocks, and the
server now advertises ASR as an installed program with models under
`info.asr[]`, plus empty arrays for the unsupported service families.

A new `tests/wyoming_protocol_probe.py` script sends the same describe/info
handshake and validates the returned info shape against Home Assistant's schema.
The currently deployed server on `192.168.0.79:10300` still reports the old shape
until rebuilt/restarted, and the probe correctly flags that mismatch.

Verification: `cargo fmt --all -- --check`, `python3 -m py_compile
tests/wyoming_protocol_probe.py`, `cargo test -p fono-net-codec -p fono-net -p
fono-stt wyoming`, `cargo test -p fono-net --test wyoming_server_round_trip`,
`cargo test -p fono-stt --test wyoming_round_trip`, and `cargo check -p
fono-net-codec -p fono-net -p fono-stt` pass on the current Linux host.

## 2026-05-01 — Tray now exposes remote mDNS Wyoming servers

The tray backend now appends live mDNS-discovered Wyoming servers to the existing
"STT backend" submenu, using the same discovery registry as `fono discover`. The
daemon filters out its own local Wyoming advertisement before passing labels to
the tray, so the menu contains only remote, actionable servers. Selecting a
discovered server writes `[stt.wyoming].uri`, switches `[stt].backend` to
`wyoming`, and hot-reloads the orchestrator.

Verification: `cargo fmt --all -- --check`, `cargo check -p fono-tray --features
tray-backend`, `cargo check -p fono`, `cargo test -p fono
daemon::tests::tray_wyoming_peers_filter_local_fullname`, `cargo build -p fono`,
and `git diff --check` pass on the current Linux host.

## 2026-05-01 — mDNS Wyoming advertisements now publish host addresses

Manual Wyoming connections to the remote `ai` host worked, but automatic
mDNS discovery resolved the Fono advertisement with no A/AAAA records. The
advertiser now calls `mdns-sd` address auto-detection when no explicit publish
addresses are configured, so `_wyoming._tcp.local.` registrations include the
current non-loopback host addresses and stay updated as interfaces change.

Verification: `cargo test -p fono-net discovery::advertiser` and `cargo build
-p fono` pass. A patched debug binary copied to `ai` advertised
`fono-ai-mdns-fixed._wyoming._tcp.local.` on port 10309; local
`avahi-browse -rt _wyoming._tcp` resolved both IPv4 and IPv6 addresses, and
`./target/debug/fono discover --json` listed the remote Wyoming peer.

## 2026-04-30 — CI musl size-budget toolchain fix

The v0.3.7 Release workflow published successfully, but the `main` CI run failed
in the `size-budget (musl, release-slim)` job because Ubuntu's `musl-tools`
package provides `x86_64-linux-musl-gcc` but no matching
`x86_64-linux-musl-g++` executable. The CI musl dependency setup now installs a
small wrapper at `/usr/local/bin/x86_64-linux-musl-g++` so whisper.cpp's CMake
compiler probe can resolve the C++ compiler name it requests.

Verification: `git diff --check`, workflow YAML parsing via Python `yaml`, and
`cargo fmt --all -- --check` pass on the current Linux host. Full musl
size-budget validation remains CI-only on this host because the local NimbleX
environment still lacks the musl Rust standard library and musl C toolchain.

## 2026-04-30 — v0.3.7 release prep

Prepared the v0.3.7 release metadata: workspace and lockfile versions are now
0.3.7, `CHANGELOG.md` has a `## [0.3.7] — 2026-04-30` section, and
`ROADMAP.md` lists the Wyoming + mDNS network foundations and binary-size prep
as recently shipped.

Verification: `cargo fmt --all -- --check`, `cargo check -p fono`,
`./tests/check.sh`, and the Rust-source SPDX header audit pass on the current
Linux host. `./tests/check.sh --size-budget --no-test` passes the build,
dependency, format, and clippy portions, then stops at the size-budget
preflight because this host lacks the `x86_64-unknown-linux-musl` Rust standard
library under `/usr`; CI/release runners remain responsible for the canonical
musl artefact gate.

## 2026-04-30 — Tray left-click now shows status under snixembed

The SNI tray backend now handles `Activate` by dispatching the existing
`ShowStatus` tray action. This gives snixembed and other hosts that call
`org.kde.StatusNotifierItem.Activate` a useful left-click path, while the normal
right-click D-Bus menu path remains unchanged.

The libdbusmenu warning seen under snixembed was traced to the upstream `ksni`
D-Bus menu layout builder adding `children-display = "submenu"` to the root
layout item. The root is the menu container rather than a visible submenu item,
so libdbusmenu-gtk warns even though Fono's actual submenu items are populated.

Verification: `cargo fmt --check`, `cargo check -p fono-tray --features
tray-backend`, `cargo test -p fono-tray --lib`, and `cargo clippy -p fono-tray
--features tray-backend -- -D warnings` pass on the current Linux host.

## 2026-04-30 — Discovery and bind config cleanup

Removed the unreleased `[network].autodiscover`, `[network].advertise`, and
`[server.wyoming].allow_public` config fields entirely. Discovery browsing is
always on while the daemon is running, Wyoming advertising is automatic only
when `[server.wyoming].enabled = true`, and `[server.wyoming].bind` is now the
sole network exposure control. The network plan and unreleased changelog were
updated to match the simplified config surface.

Verification: `cargo fmt --check`, `cargo test -p fono-core config::tests`,
and `cargo check -p fono` pass on the current Linux host.

## 2026-04-30 — Missing tray watcher now raises a desktop notification

When the SNI tray backend fails because the session bus has no
`org.kde.StatusNotifierWatcher`, Fono now sends a critical desktop
notification titled "Fono tray unavailable" with a 20-second requested
expiry. The notification now uses a short body that fits typical notification
popups while telling the user to start a tray host such as Waybar tray, KDE
tray, xfce4-panel, or snixembed before restarting Fono. The existing warning
log keeps the longer explanation for terminal/service diagnostics.

Verification: `cargo fmt --check`, `cargo test -p fono-tray --lib`, `cargo
check -p fono-tray --features tray-backend`, `cargo clippy -p fono-tray
--features tray-backend -- -D warnings`, and `cargo check -p fono --features
tray,interactive` pass on the current Linux host.

## 2026-04-30 — mDNS discovery is always-on

Discovery browsing is not controlled by a config toggle, and server
advertising is not controlled by a config toggle. The daemon now always starts
the mDNS browser when it can create the mDNS service daemon, and advertises
Wyoming automatically whenever `[server.wyoming].enabled = true`.
`[network].instance_name` remains as the optional friendly-name override.

Verification: `cargo fmt --check`, `cargo test -p fono-core config::tests`,
and `cargo check -p fono` pass on the current Linux host.

## 2026-04-30 — Tray watcher absence now degrades cleanly

NimbleX/i3-style sessions without an SNI StatusNotifierWatcher now get an
actionable tray warning instead of the raw `ksni::Tray::spawn` error. Fono
continues hotkeys, dictation, and overlay operation without a tray icon, and
points the user at a tray host/watcher such as KDE Plasma's tray, waybar tray,
xfce4-panel, or snixembed.

Overlay startup now reports early winit event-loop failures back to the caller
instead of returning a handle whose wake proxy is missing. This makes overlay
startup failures visible at daemon startup rather than silently dropping later
`set_state` / `update_text` commands.

Verification: `cargo fmt --check`, `cargo test -p fono-tray --lib`, `cargo test
-p fono-overlay --lib`, `cargo check -p fono-tray --features tray-backend`,
`cargo check -p fono-overlay --features real-window`, `cargo clippy -p
fono-tray --features tray-backend -- -D warnings`, `cargo clippy -p
fono-overlay --features real-window -- -D warnings`, and `cargo check -p fono
--features tray,interactive` pass on the current Linux host. A broader `cargo
test -p fono-tray -p fono-overlay` was also attempted but this host cannot run
the overlay doctest because `rustdoc` is unavailable in `PATH`.

## 2026-04-30 — Default Linux audio no longer links ALSA/libasound

Moved Linux default microphone capture off `cpal` and onto a process-backed
PulseAudio/PipeWire path (`parec` raw mono s16le at the target sample rate),
so the default Fono binary no longer pulls `cpal`, `alsa`, or `alsa-sys` into
the dependency graph. `cpal` remains available behind `fono-audio`'s
`cpal-backend` feature for macOS, Windows, and explicit bare-ALSA Linux builds.

Release/CI guardrails now reject regressions: `tests/check.sh` fails if the
default Linux dependency tree includes `cpal`, `alsa`, or `alsa-sys`, the
musl size-budget gate already requires zero `NEEDED` entries, and the release
workflow rejects Linux artifacts with `libasound.so` or `libgomp.so` in
`NEEDED`. CI/release package installs no longer install `libasound2-dev`.

Verification: `cargo check -p fono`, `cargo check -p fono-audio`,
`cargo check -p fono-audio --features cpal-backend`, `cargo test -p
fono-audio --lib`, `cargo test -p fono-audio --lib --features
cpal-backend`, `cargo clippy -p fono-audio --all-targets -- -D warnings`,
`cargo fmt --all -- --check`, and `./tests/check.sh --quick --no-test` all
pass on the current Linux host. `./tests/check.sh --size-budget --no-test`
passes build/clippy/dependency checks, then stops at the preflight because this
host still lacks the `x86_64-unknown-linux-musl` Rust standard library under
`/usr`.

## 2026-04-30 — Release GNU no longer links libgomp/libstdc++ dynamically

User reported that `cargo build --release -p fono` still produced a GNU
binary with `libgomp.so.1` in `NEEDED`, and that the musl build does not
start locally. Root cause: late `.cargo/config.toml` `link-arg` flags do
not override `cargo:rustc-link-lib=gomp` / `dylib=stdc++` emitted by
`llama-cpp-sys-2`'s build script. Fixed on fork branch
`bogdanr/llama-cpp-rs:feature/static-runtime-linkage` (commit
`e9f5cc12`) by adding `static-openmp` and Linux-capable `static-stdcxx`
features that make the sys crate emit `static=gomp` / `static=stdc++` at
the right point in the link line, including compiler-discovered archive
search paths.

Fono now pins `[patch.crates-io]` to that branch and enables
`llama-cpp-2` features `openmp`, `static-openmp`, and `static-stdcxx`.
Verification: `cargo build --release -p fono` succeeds, and `ldd
target/release/fono` / `readelf -d` show no `libgomp.so.1` and no
`libstdc++.so.6`. Remaining GNU `NEEDED`: `libasound.so.2`,
`libgcc_s.so.1`, `libm.so.6`, `libc.so.6`, `ld-linux-x86-64.so.2`.
Those are expected until the canonical musl artefact builds.

Musl recheck still fails before any C/C++ linkage with Rust error E0463:
this NimbleX host has distro `rustc`/`cargo` but no `rustup`, no
`x86_64-unknown-linux-musl` Rust standard library, and no musl C/C++
cross compiler in `PATH`. `tests/check.sh --size-budget` now detects the
missing Rust std cleanly on non-rustup hosts instead of assuming `rustup`
exists. CI musl deps were also cleaned up to drop obsolete GTK packages.

## 2026-04-30 — Task 2.1 complete: GTK gone, pure-Rust SNI tray

Phase 2 Task 2.1 of `plans/2026-04-30-fono-single-binary-size-v1.md`.
Replaced `tray-icon`'s libappindicator + GTK3 backend with a
pure-Rust StatusNotifierItem (SNI) implementation via `ksni 0.3`
(Unlicense, public-domain) talking `zbus`. Confirmed via
`cargo tree -p fono --features tray`: `tray-icon`, `gtk`, `gdk`,
`cairo-rs`, `pango`, `gdk-pixbuf`, `glib`, and every `*-sys` shim
(`gtk-sys`, `gdk-sys`, `pango-sys`, `glib-sys`, `gobject-sys`,
`cairo-sys-rs`, `gdk-pixbuf-sys`) have left the dep tree. The new
`fono-tray` keeps the public API identical (`Tray::set_state`,
`spawn`, the four `*Provider` aliases, `TrayAction`); the daemon's
spawn site at `crates/fono/src/daemon.rs:328` was unchanged.

Internally the backend now spawns a tokio task instead of a
dedicated GTK thread, owns a `KsniTray` model implementing
`ksni::Tray`, and pushes provider snapshots into the model every
two seconds via `Handle::update`. Menu rebuild is declarative —
`menu()` returns the current `Vec<MenuItem<KsniTray>>` and ksni
diffs against the last snapshot, so we no longer maintain
pre-allocated slot arrays + ID maps. Icon is still the in-code
ARGB32 circle (byte order corrected for SNI: `[A, R, G, B]` not
`[R, G, B, A]`).

`cargo check -p fono --features tray` clean. `cargo clippy -p
fono-tray --features tray-backend` clean. The five
`graphical_session` unit tests still pass (no behaviour change at
the daemon's runtime gate).

`deny.toml` updated to allow the `bogdanr/llama-cpp-rs.git` git
source consumed via `[patch.crates-io]`.

Task 1.2 (source-level shared ggml on a second `bogdanr/llama-cpp-rs`
branch) remains the next blocker.

## 2026-04-30 — Task 1.1 wired into Fono via fork

Upstream PR submitted: [utilityai/llama-cpp-rs#1015](https://github.com/utilityai/llama-cpp-rs/pull/1015).
Fork branch `feature/optional-common-build` on
`github.com/bogdanr/llama-cpp-rs` is now consumed via
`[patch.crates-io]` in `Cargo.toml`. Fono's existing
`default-features = false, features = ["openmp"]` declaration on
`llama-cpp-2` means we automatically opt out of the new `common`
feature, so building Fono today drops `libcommon.a` (~14 MB) and the
`wrapper_common`/`wrapper_oai` shim archives (~10 MB) from the link
line — a ~24 MB raw archive saving, expected to land as ~6–10 MB of
`.text` after LTO + `--gc-sections`. `cargo check -p fono` clean. Task
1.1 closed; Task 1.2 (source-level shared ggml) is the next blocker.

## 2026-04-30 — Binary-size pass kickoff: single 20 MiB static-musl ELF

Plan: `plans/2026-04-30-fono-single-binary-size-v1.md`. ADR:
`docs/decisions/0022-binary-size-budget.md` (supersedes 0018 once Task
1.2 lands).

User feedback: the release artefact had drifted to ~25–30 MiB stripped
and was dynamically linked to GTK 3 + glib + cairo + libstdc++ + libgomp
+ glibc — both contradicting the v1 design plan's "single static-musl
ELF, `ldd` not a dynamic executable" promise. Target rolled back to
**≤ 20 MiB with all features**, **one binary** (no
desktop/server/cloud-only flavours; graphical surfaces runtime-gated on
`DISPLAY`/`WAYLAND_DISPLAY`), and **zero `NEEDED` shared libraries**.

What landed this session (prep work; the structural wins are next):

- `Cargo.toml` — removed unused workspace deps (`ort`, `rodio`,
  `swayipc`, `hyprland`). Confirmed zero `use` sites; cosmetic cleanup.
- `.cargo/config.toml` — added dead-code link flags
  (`-Wl,--gc-sections`, `-Wl,--as-needed`) and C/C++ size flags
  (`-Os -ffunction-sections -fdata-sections`) for every supported
  target. Added `-static-libstdc++`, `-static-libgcc`,
  `-l:libgomp.a` for the musl target so the final ELF has no C++/OMP
  `NEEDED`. The legacy `--allow-multiple-definition` flag stays until
  Task 1.2 lands the source-level shared ggml; both flags now coexist
  with documented retirement path in the file's header comment.
- `crates/fono/src/daemon.rs:232-247` — tray spawn now runtime-gated
  on `DISPLAY`/`WAYLAND_DISPLAY`. Headless hosts get a `debug!` log
  line and an empty tray channel; the rest of the daemon runs
  unmodified. This is the architectural keystone of the
  one-binary-many-roles contract.
- `tests/check.sh --size-budget` — new gate that builds
  `release-slim x86_64-unknown-linux-musl` and asserts (a) binary
  size ≤ 20 971 520 bytes, (b) `ldd` reports "not a dynamic
  executable", (c) `nm` shows exactly one `ggml_init` symbol. Skips
  cleanly when the musl target isn't installed.
- `plans/2026-04-30-llama-cpp-sys-2-strip-common.patch.md` — the
  upstream / fork patch ready to apply for Task 1.1 (kill 24 MB of
  unused llama.cpp `common/`). Two application paths documented
  (vendored fork at `vendor/llama-cpp-sys-2/` vs git fork on GitHub);
  blocked on operator choice.
- ADR 0022 published; ADR 0018 will be marked Superseded once Task
  1.2 lands.

Next-session blockers (operator decisions):

1. **Task 1.1 application path.** Vendor 22 MiB of patched
   llama-cpp-sys-2 into `vendor/` (option A), or push a fork to
   GitHub and reference it via `[patch.crates-io]` git URL (option
   B)? Patch contents are the same either way.
2. **Task 2.1 tray library swap.** Replace the libappindicator/GTK
   backend of `tray-icon` with a pure-Rust `ksni` SNI implementation.
   Drops every GTK / glib / cairo `NEEDED` from the ELF; adds the
   `ksni` + `zbus` deps. Worth confirming the SNI compatibility with
   the operator's panel before swinging the change.

Once both decisions land the path forward is mechanical: apply
patch → build → measure → repeat. Phase 4 Rust trims held in reserve
in case Phases 1 + 2 + 3 don't already hit budget.

## 2026-04-29 — Slice 4: mDNS LAN autodiscovery

Plan: `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`

Slice 4 lights up the *Discovered on LAN* surface that Slices 5–7 will
build on. Concrete deliverables:

- New crate-internal module `fono_net::discovery` with `Browser`,
  `Advertiser`, `Registry`, and `DiscoveredPeer`. One passive `tokio`
  task per service type (`_wyoming._tcp.local.`, `_fono._tcp.local.`)
  feeds an `Arc<RwLock<HashMap<fullname, DiscoveredPeer>>>`; peers
  stale after 120 s and are evicted on a 15 s sweep.
- New `[network]` config block: only `instance_name` remains as a
  cosmetic override (empty ⇒ `fono-<hostname>`). Discovery browsing is
  always on while the daemon is running; advertising happens
  automatically for enabled servers.
- Daemon hooks: spawn browser + (optional) advertiser at startup; hold
  handles for the daemon's lifetime so `unregister` fires goodbye
  packets on `Drop`.
- IPC `Request::ListDiscovered` / `Response::Discovered(Vec<DiscoveredPeer>)`
  surfaces the live registry to clients.
- New CLI `fono discover [--json]` prints the registry as a fixed-width
  table or pretty JSON.
- Integration test (`crates/fono-net/tests/discovery_round_trip.rs`)
  drives two independent `ServiceDaemon` instances over loopback
  multicast and asserts the TXT round-trip lands in the registry
  within 5 s. Skips cleanly on sandboxes without multicast.
- Single new dependency: `mdns-sd 0.13` (pure-Rust, dual MIT/Apache-2.0,
  no Avahi/Bonjour FFI).

Verification: `cargo build --workspace`, `cargo test --workspace --lib`,
`cargo test -p fono-net --tests --features discovery`,
`cargo test -p fono-stt --tests`, `cargo clippy --workspace --all-targets
-- -D warnings -A dead_code`, `cargo fmt --all -- --check` all green.

Tray *Discovered on LAN* submenu population is split off into Slice 7
(tray polish) per the v2 plan; the IPC contract is in place so the
tray can read from a single source when that lands.

Next up: **Slice 5 — Fono-native protocol design + `FonoLlm`/`FonoStt`
client over WebSocket.**

## 2026-04-29 — OS-delegated microphone selection (PulseAudio-first + config purge)

Plans (combined execution):
- `plans/2026-04-29-pulseaudio-first-microphone-enumeration-v1.md`
- `plans/2026-04-29-drop-input-device-config-knob-v1.md`

Pivot triggered by two follow-up issues against the v2 recovery work
shipped earlier today: (a) the tray "Microphone" submenu was full of
ALSA plugin pseudo-devices (`pulse`, `oss`, `speex`, `default`,
`surround51`, …) and the daemon spammed `snd_pcm_dsnoop_open: unable
to open slave` because cpal's ALSA host enumerates every PCM in
`asound.conf`; (b) the user — a sample size of one but a strong one —
correctly observed that `[audio].input_device` was the wrong place to
solve "which microphone?" because every modern OS already owns that
question.

End-state: Fono no longer keeps a microphone override. The OS layer
is the source of truth.

- **PulseAudio-first enumeration.** New `crates/fono-audio/src/pulse.rs`
  shells to `pactl list sources [short]` and `pactl get-default-source`
  / `pactl set-default-source`, mirroring the `mute.rs` shell-out
  pattern. `crates/fono-audio/src/devices.rs` dispatches on
  `AudioStack::detect()`: `PulseAudio` / `PipeWire` → `pulse`,
  `Unknown` → cpal. Sink monitors are dropped at the source on the
  Pulse branch; the `is_likely_microphone` heuristic only matters on
  the cpal fallback. `InputBackend::{Pulse{pa_name}, Cpal{cpal_name}}`
  carries the backend-specific identifier through to the daemon.
- **Tray "Microphone" submenu rewired** to `pactl set-default-source`.
  Clicking a row mutates Pulse's default-source system-wide (visible
  to `pavucontrol`, GNOME / KDE settings, every other app), then
  triggers `Request::Reload` so cpal re-opens its default-source
  stream on the new endpoint. Submenu hidden on `Unknown` hosts —
  the OS owns the UI there.
- **Config purge.** `[audio].input_device` removed (no migration —
  no released users yet). `[general].language`, `[stt.local].language`
  (deprecated language scalars superseded by `languages: Vec<String>`)
  and `[general].cloud_force_primary_language` (superseded by the
  in-memory language cache) all gone. `cloud_force_primary` builder /
  struct field / dead first-pass branch removed from `GroqStt`,
  `GroqStreaming`, `OpenAiStt`. Schema migration block in
  `Config::migrate` collapsed to the version check.
- **Recovery hook reworded** — body now points at "the tray Microphone
  submenu" + `pavucontrol` / OS sound settings; the deprecated
  `fono use input "<name>"` advice is gone (test pinned).
- **CLI / wizard / doctor cleanup.** `fono use input` removed.
  Wizard microphone picker removed. `fono doctor` "Audio inputs:"
  is informational — flat list with one row marked as the OS default,
  no override-aware highlight.
- **Tray surface trimmed.** `TrayAction::ClearInputDevice` removed
  (no override to clear); the "Auto (system default)" entry stays
  as informational only (disabled, no menu-event ID bound).

Status: implementation complete. `tests/check.sh` (full matrix —
fmt, build × default + interactive, clippy × default + interactive,
test × default + interactive) green. CHANGELOG `[Unreleased]`
section reorganised into Added / Changed / Removed reflecting the
new design.

## 2026-04-29 — Empty-transcript microphone recovery (plan v2)

Plan: `plans/2026-04-29-empty-transcript-microphone-recovery-v2.md`.
Triggered by a real-world dock complaint: external dock advertises a
passive capture endpoint with no microphone wired to it, the OS elects
it as `@DEFAULT_SOURCE@`, and Fono's recordings come out flat-line
silent — Whisper hallucinates or returns empty, and the user is left
without an actionable signal.

Three layers, all stacked behind the existing `STT returned empty
text` signal at `crates/fono/src/session.rs` (no new RMS/peak detector
needed):

- **Phase 1 — empty-transcript notification.** New
  `crates/fono/src/audio_recovery.rs` fires a critical desktop toast
  when capture ≥ 5 s and the transcript is empty. Body names the
  silent device, the recording duration in seconds, and the recourse:
  "switch to '<name>'" + `fono use input` CLI when exactly one
  non-loopback alternative is detected, or "open tray Microphone
  submenu" when 2+ alternatives exist. The user's
  `[audio].input_device` override is never silently rewritten. Five
  unit tests cover the body composer.
- **Phase 2 — tray "Microphone" submenu.** Mirrors the existing STT/
  LLM/Languages pattern at `crates/fono-tray/src/lib.rs`. `Auto` plus
  a row per cpal device, active-marked. Clicking writes
  `[audio].input_device` and triggers `Request::Reload` so the next
  capture opens the new endpoint without restarting. New
  `TrayAction::SetInputDevice(u8)` / `ClearInputDevice` + a
  `MicrophonesProvider` polled every ~2 s by the tray refresh loop.
- **Phase 3 — wizard probe + doctor row + `fono use input` CLI.**
  First-run wizard offers a microphone picker only when 2+ devices
  are visible (single-mic laptops skip the prompt). `fono doctor`
  gains an "Audio inputs:" matrix with the active marker and surfaces
  "configured device not currently visible" when the override is
  unplugged. `fono use input <name>` (and `auto` to clear) is
  symmetric with `fono use stt` / `fono use llm`, with
  case-insensitive name matching.

Status: implementation complete. `tests/check.sh` (full matrix —
fmt, build × default + interactive, clippy × default + interactive,
test × default + interactive) green on the work branch. CHANGELOG
[Unreleased] section updated with the four user-visible additions;
will graduate to a versioned section at next release.

## 2026-04-28 — v0.3.0 release

Tagged v0.3.0. Bundles three user-visible fixes plus the release-time
cloud quality gate:

- LLM cleanup clarification fix (universal across all backends).
- In-memory cloud-STT language stickiness, peer-symmetric.
- Live Groq equivalence gate at release time (~0.5 % of free-tier
  daily cap per release).

Baseline `docs/bench/baseline-cloud-groq.json` bootstrapped by the
maintainer; all 10 fixtures (en × 4, ro × 3, es, fr, zh) passing.
CHANGELOG promoted from `[Unreleased]` to `[0.3.0]`. ROADMAP entries
moved into Shipped with the v0.3.0 tag and date. Workspace version
bumped to 0.3.0 in `Cargo.toml`.

## 2026-04-28 — Wave 3 Slice B1 Thread C: live Groq equivalence gate

Plan: `plans/2026-04-28-wave-3-slice-b1-thread-c-live-groq-v2.md`
(supersedes the cloud-mock approach in v1 Tasks C1–C9). User pushed
back on mocks: they catch our regressions but not upstream Groq
schema/behaviour changes, and the maintenance cost of refreshing
recordings is recurring.

What landed:

- `fono-bench equivalence --stt groq` arm at
  `crates/fono-bench/src/bin/fono-bench.rs:327-364`. Reads
  `GROQ_API_KEY` from env (exits with code 2 + bootstrap-friendly
  message when missing). Default model `whisper-large-v3-turbo`,
  overridable via `--model`. `caps.english_only = false`
  (multilingual).
- `--rate-limit-ms <ms>` flag with provider-aware default (250 ms for
  Groq, 0 otherwise). 429 detection + hard-fail with code 3 and a
  named-fixture message; never retried.
- `.github/workflows/release.yml` gains a `cloud-equivalence` job
  that runs **before** the build matrix. Auto-skipped when
  `GROQ_API_KEY` is empty (forks; bootstrap tags) or the tag carries
  the `-no-cloud-gate` suffix (operator escape hatch). `build` job
  uses `if: always() && (success || skipped)` so skip propagates
  cleanly without blocking releases that pre-date the secret.
- `.github/scripts/diff-cloud-bench.py` — exit code 1 on verdict
  divergence, exit code 2 on missing baseline (with the exact
  bootstrap command printed to stderr), exit code 0 on match.
- ADR `docs/decisions/0021-cloud-equivalence-via-real-api.md`
  records the live-vs-mock decision and the cost-shape analysis (10
  fixtures, ~110 audio-seconds, < 0.5 % of free-tier daily cap).
- `docs/dev/release-checklist.md` — bootstrap command, regenerate
  conditions, override-tag instructions, manual-rerun-after-outage
  steps.
- `CHANGELOG.md` Unreleased Added entries; `ROADMAP.md` In progress
  flipped to "bootstrap the baseline" + new Shipped entry.

Operator owes (one-time): bootstrap the baseline locally. The diff
script prints the command on the first CI run if you'd rather see it
fail-soft once before running locally:

```sh
GROQ_API_KEY=gsk_... \
  cargo run --release -p fono-bench --features equivalence -- \
  equivalence --stt groq \
    --output docs/bench/baseline-cloud-groq.json \
    --baseline --no-legend
```

Sanity-check the resulting JSON, commit it, and `v0.3.0` is ready to
tag.

Build verified: `cargo build -p fono-bench --features equivalence`
compiles clean.

## 2026-04-28 — Multi-language STT, no primary, in-memory stickiness

Plan: `plans/2026-04-28-multi-language-stt-no-primary-v3.md`. User
report: Groq's `whisper-large-v3-turbo` frequently misclassifies the
user's accented English as Russian. Wanted a fix that (a) keeps Fono
lightweight on cloud-only builds, (b) handles bilingual switchers
without breaking them, (c) avoids a "primary / secondary" UX, (d) uses
OS hints rather than asking the user.

Three earlier plan iterations explored and rejected: a local-Whisper
"language bridge" (v1, contradicts cloud users' lightweight constraint),
a cache-as-first-call-force (v2, breaks switchers — once stickiness
pins the wrong language every following call is mangled), and a
file-persisted cache (v2, marginal cold-start benefit + active harm
when stale). v3 (executed here) is **rerun-target only, in-memory
only, peer-symmetric**.

What landed:

- **`crates/fono-stt/src/lang_cache.rs`** — `LanguageCache` with
  `record` / `get` / `seed_if_empty` / `clear`, keyed by backend
  `&'static str`. Process-wide singleton via `LanguageCache::global()`
  shared across batch + streaming variants. 8 unit tests.
- **`crates/fono-core/src/locale.rs`** — POSIX → BCP-47 alpha-2 parser
  (`LANG=ro_RO.UTF-8` → `Some("ro")`, `C` / `POSIX` / empty → `None`).
  Used by both the cache bootstrap and the wizard.
- **`LanguageSelection::primary()` renamed to `fallback_hint()`**
  with a doc-comment that scope-restricts callers to single-language
  transports. The old name is kept as `#[deprecated]` for one release.
- **`groq.rs`, `openai.rs`, `groq_streaming.rs`** — first call is
  unforced; the response's detected language is checked against the
  allow-list; in-list → `cache.record()`; banned + cache populated +
  rerun knob on → re-issue with `language=<cached>`; banned + cache
  empty → accept unforced response, debug-log the skip.
- **`cloud_rerun_on_language_mismatch` default flipped to `true`** in
  `crates/fono-core/src/config.rs`. Combined with the cache, cloud STT
  self-heals from one-off Turbo misfires after the first correctly
  detected utterance per session (or immediately on cold start when OS
  locale ∈ allow-list).
- **`cloud_force_primary_language` deprecated** with a `#[deprecated]`
  attribute on the field. Removed in v0.5.
- **Wizard rework** in `crates/fono/src/wizard.rs` — checkbox-style
  "Languages you dictate in" picker with English pre-checked but
  freely uncheckable. Detected OS locale gets pre-checked alongside.
  No "primary" anywhere in the copy.
- **Tray Languages submenu** in `crates/fono-tray/src/lib.rs` —
  read-only peer-list display + "Clear language memory" action that
  emits `TrayAction::ClearLanguageMemory`; the daemon dispatcher at
  `crates/fono/src/daemon.rs:524-530` calls
  `LanguageCache::global().clear()`.
- **ADR
  [`docs/decisions/0017-cloud-stt-language-stickiness.md`](decisions/0017-cloud-stt-language-stickiness.md)**
  records the rejection rationale for local-bridge / file-persisted /
  cache-as-first-call / primary-secondary alternatives, so future
  agents don't regress to one of them.
- **`docs/providers.md`** — new "Multilingual STT and language
  stickiness" section.
- **`docs/troubleshooting.md`** — new "Cloud STT keeps detecting the
  wrong language" section explaining cache, rerun, tray clear, config
  edit recourses.
- **`CHANGELOG.md`** — `Added` / `Changed` / `Deprecated` entries.

### Switcher safety guarantee

Two configs `general.languages = ["ro", "en"]` and `["en", "ro"]`
behave identically at runtime — config order is consulted nowhere in
the request path. The cache reflects what was last heard. Trace with
`ro → en → en → ro` produces three correct transcripts and zero
reruns; the switching cost is whatever the cloud provider's
auto-detect already absorbs.

### Owed verification (no Rust toolchain in this environment)

```sh
cargo test -p fono-stt -p fono-core -p fono
cargo test --no-default-features --features tray,cloud-all -p fono-stt
cargo clippy --workspace --all-targets -- -D warnings
```

The `--no-default-features --features tray,cloud-all` invocation
verifies the slim cloud-only build still compiles without
`whisper-rs`. Once green, commit with `git commit -s` per AGENTS.md
DCO rule.

### Deferred follow-ups (not blocking the user's bug fix)

- **HTTP-mock switcher integration test for `groq.rs` and
  `openai.rs`.** `groq_streaming.rs` already has `with_request_fn`
  closure injection (Wave 3 Thread B); adding the same hook to the
  batch backends is a small but separate refactor. Cache invariants
  are already covered by the 8 unit tests in `lang_cache.rs`.
- **Desktop toast on rerun.** Currently a `tracing::warn!` line ("groq
  returned banned language … re-issuing with cached
  language=<code>"). Promoting it to a `notify-rust` toast requires
  adding `notify-rust` to `fono-stt` (it currently lives only in
  `fono`); deferred to keep `fono-stt` notification-free.
- **One-shot tray "Force next dictation as: <language>" radio.** The
  Languages submenu currently exposes the read-only checkboxes and
  "Clear language memory"; the per-utterance force radio (plan task
  8 sub-bullet) is design-complete but unwired.

## 2026-04-28 — LLM cleanup clarification-refusal fix

Bug report: a short utterance dictated through the cloud cleanup
provider sometimes injected a chat-style clarification reply
(*"It seems like you're describing a situation, but the details are
incomplete. Could you provide the full text you're referring to, so I
can better understand and assist you?"*) rather than the cleaned
transcript. Investigation showed:

- The hotkey is irrelevant. F8 (`HoldPressed`) and F9 (`TogglePressed`)
  share the same cleanup pipeline at
  `crates/fono/src/session.rs:1213-1276`. F8 just correlates because
  push-to-talk produces shorter recordings.
- The provider is irrelevant. Reproducible on Cerebras, Groq, OpenAI,
  OpenRouter, Ollama, Anthropic, **and** the local llama.cpp backend;
  the failure mode is a property of how chat-trained LLMs interpret a
  bare short utterance.

The fix is therefore universal — applied identically to every
`TextFormatter` impl. Plan:
`plans/2026-04-28-llm-cleanup-clarification-refusal-fix-v1.md`. Three
layers of defence shipped:

1. **Hardened default prompt** in
   `crates/fono-core/src/config.rs:402-415` — explicit hard rules:
   never ask for clarification, never respond with a question or
   meta-comment, return the transcript verbatim if it's short / empty /
   already clean. Same prompt for every backend.
2. **User-message framing** via new `fono_llm::traits::user_prompt`
   helper that wraps the raw transcript in `<<<` / `>>>` fences,
   referenced by all three backend impls (`OpenAiCompat` — used by
   Cerebras / Groq / OpenAI / OpenRouter / Ollama, `AnthropicLlm`,
   `LlamaLocal`).
3. **Refusal detector** `fono_llm::traits::looks_like_clarification`
   matches case-insensitive opener phrases AND a corroborating
   clarification fragment (low-false-positive heuristic). On a hit,
   the backend returns `Err`; the existing pipeline fallback at
   `crates/fono/src/session.rs:1264-1273` then injects raw STT text.
   Identical wiring in every backend.

Plus `Llm::skip_if_words_lt` default raised from `0` to `3` so
one- and two-word captures bypass the LLM entirely on every backend
(saves 150–800 ms; eliminates the failure mode at the source).

Tests: 5 new unit tests in `crates/fono-llm/src/traits.rs` for the
detector and framing helper; 2 new integration tests in
`crates/fono/tests/pipeline.rs`
(`pipeline_falls_back_to_raw_when_llm_rejects_clarification`,
`pipeline_skips_llm_for_short_capture_under_default_threshold`). The
existing `pipeline_produces_history_row_and_injects_cleaned_text` was
updated to set `skip_if_words_lt = 0` because its 2-word fixture would
otherwise trip the new skip default.

Docs: `CHANGELOG.md` Unreleased gets a `Fixed` and `Changed` bullet
(both phrased universally, naming every backend); `docs/troubleshooting.md`
gets a new "LLM responds with a question" section that explicitly
flags the failure mode as not provider-specific; `docs/providers.md`
gets a "Short-utterance handling" subsection covering all backends.

`cargo test` / `cargo clippy` were not run in this session (no rust
toolchain available in the agent environment) — the operator should
run `cargo test -p fono-llm -p fono` and
`cargo clippy --workspace --all-targets` before tagging the next release.

## 2026-04-28 — Wave 3 (Slice B1) — Threads A + B shipped; Thread C deferred

Two DCO-signed commits delivered the user-visible half of Slice B1
(driven by `plans/2026-04-28-wave-3-slice-b1-v1.md`); Thread C
(equivalence harness cloud rows) is deferred to a follow-up.

| Thread | SHA | Subject |
|---|---|---|
| A | `1e5682f` | `feat(fono-audio): cpal-callback push for live capture (Thread A / R10.x)` |
| B | `eaf46a3` | `feat(fono-stt): Groq streaming pseudo-stream backend (R4.2)` |
| C | _deferred_ | cloud-mock equivalence rows + recorded-HTTP Groq fixtures (R18.12) |

**Thread A** replaces the 30 ms-poll `RecordingBuffer` drain at
the live-dictation hot path with a true cpal-callback push pipeline:
each cpal data callback resamples to mono f32 and `try_send`s its
slice into a bounded(64) crossbeam SPSC; a dedicated `fono-live-bridge`
std::thread forwards into a tokio mpsc; the drain task pulls
straight into the streaming `Pump`. No 30 ms tick, no
`Mutex<RecordingBuffer>` middleman for live sessions. The batch
path (`run_oneshot`) still uses `RecordingBuffer` unchanged. New
unit test `forwarder_receives_every_callback_in_order` drives a
synthetic cpal stand-in 100x without a real device. Phase A4
manual latency measurement
(`live.first_partial < 400 ms` on the reference machine) cannot be
produced from a headless agent and is left for the operator to
record post-merge.

**Thread B** adds an opt-in Groq streaming STT backend implemented
as a "pseudo-stream": every 700 ms the streaming task re-POSTs the
trailing 28 s of buffered audio to Groq's existing batch endpoint,
pipes each decode through `LocalAgreement` to extract a stable
token-prefix preview, and emits a single finalize decode on
`SegmentBoundary` / `Eof`. In-flight cap = 1 (drop on overlap;
counted in `preview_skipped_count`). New ADR
`docs/decisions/0020-groq-pseudo-stream.md` captures the design
trade-offs (no Groq WebSocket today, 700 ms cadence trade-off,
~25-40× cost overhead vs single batch POST). Selectable via
`fono use stt groq` + `[interactive].enabled = true` +
`[stt.cloud].streaming = true`; the wizard prompts for the third
knob when the first two are set. `docs/providers.md` updated. The
backend takes a `GroqRequestFn` closure for production HTTPS, tests,
and the future cloud-mock equivalence path — keeping the Thread C
hook free.

**Thread C** is deferred. Scope:
1. New `--stt cloud-mock --provider groq` mode in
   `fono-bench equivalence` that swaps the real Groq client for a
   recorded-HTTP closure injected via
   `GroqStreaming::with_request_fn`.
2. Recording format (one JSON file per fixture per provider with
   `(request_audio_sha256, response_body)` exchange list) and at
   least one committed recording.
3. Second per-PR CI gate that runs the cloud-mock lane against a
   sibling baseline anchor (`docs/bench/baseline-cloud-mock-groq.json`).

Why deferred: Thread C is test infrastructure that doesn't block
users. The plumbing alone (mock client + recording format + JSON
fixture + manifest threshold extension + CI workflow change) is a
focused session in its own right; landing it half-done would leave
the equivalence report shape inconsistent. The `GroqRequestFn`
closure injection in Thread B's `groq_streaming.rs` already
preserves the hook Thread C will use, so deferring costs nothing
architecturally. Tracked as the next-session focus.

### Verification gate

`tests/check.sh` (full matrix incl. slim cloud-only build):
- `cargo fmt --check` — clean
- `cargo build` (default + default+interactive + slim + slim+interactive) — clean
- `cargo clippy` (same matrix) — clean
- `cargo test` (same matrix) — green (incl. new
  `forwarder_receives_every_callback_in_order` and
  `groq_streaming::tests::*`)

### Recommended next session

**Wave 3 Thread C** — drop in the cloud-mock equivalence lane.
Plan: `plans/2026-04-28-wave-3-slice-b1-v1.md` Thread C (Tasks
C1-C9). The closure-injection hook is already in
`crates/fono-stt/src/groq_streaming.rs::GroqStreaming::with_request_fn`;
the manifest threshold types are already typed (Wave 2). The work
is scoped to:
1. `crates/fono-bench/src/cloud_mock.rs` — recording loader +
   `SpeechToText` / `StreamingStt` impls keyed by request-WAV SHA.
2. `tests/fixtures/cloud-recordings/groq/<fixture>.json` recording
   fixture format + 1-2 committed recordings (real-key capture
   preferred; placeholder via local-Whisper output is the
   documented fallback).
3. `--stt cloud-mock --provider groq` flag wiring at
   `crates/fono-bench/src/bin/fono-bench.rs:288-333` and
   `:659-684`.
4. Sibling baseline `docs/bench/baseline-cloud-mock-groq.json` and
   second CI job in `.github/workflows/ci.yml`.

Once Thread C lands, the `v0.3.0` release tag becomes appropriate
(Slice B1 fully delivered; CHANGELOG entry + `release.yml`
auto-extracts CHANGELOG sections per `4577dd7`).

## 2026-04-28 — Wave 2: half-shipped plans closed out + real-fixture CI gate

Three DCO-signed commits delivered the trust-restoration leg of the
revised strategic plan (driven by
`plans/2026-04-28-wave-2-close-out-v1.md`).

| Thread | SHA | Subject |
|---|---|---|
| A | `76b9b08` | `feat(fono-bench): typed ModelCapabilities + split equivalence/accuracy thresholds` |
| B | `87221a2` | `feat(fono-update): per-asset sha256 sidecar verification + --bin-dir` |
| C | _this commit_ | `ci(fono-bench): real-fixture equivalence gate with tiny.en + baseline JSON anchor` |

**Thread A** lifted the inline `english_only` boolean
(`crates/fono-bench/src/bin/fono-bench.rs:339` pre-wave) into a typed
`ModelCapabilities` value at `crates/fono-bench/src/capabilities.rs`
with `for_local_whisper` / `for_cloud` resolvers, split the conflated
single threshold into `equivalence_threshold` and `accuracy_threshold`
on `ManifestFixture`, and added a typed `SkipReason` (`Capability` /
`Quick` / `NoStreaming` / `RuntimeError`) so `overall_verdict` no
longer needs to substring-match notes. New mock-STT capability-skip
integration test asserts `transcribe` is never invoked.

**Thread B** closed the supply-chain gap in `apply_update`: per-asset
`.sha256` sidecars are now fetched and verified during
`fetch_latest` / `apply_update`, with a `parse_sha256_sidecar` helper
covering bare-digest, text-mode, binary-mode, and multi-entry
sidecars. `--bin-dir <path>` is exposed on `fono update` for
non-default install layouts. Release workflow emits a `<asset>.sha256`
file per artefact alongside the aggregate `SHA256SUMS`.
`docs/dev/update-qa.md` carries the ten-scenario manual verification
checklist (bare-binary, `/usr/local/bin`, distro-packaged, offline,
rate-limited, mismatched sidecar, prerelease, `--bin-dir`, rollback).

**Thread C** replaced the compile-only `cargo bench --no-run` step at
`.github/workflows/ci.yml:64-68` with a real-fixture equivalence gate:
the workflow fetches the whisper `tiny.en` GGML weights (cached via
`actions/cache@v4` keyed on the model SHA, integrity-checked against
`921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f`),
runs `fono-bench equivalence --stt local --model tiny.en --baseline
--no-legend`, and diffs per-fixture verdicts against
`docs/bench/baseline-comfortable-tiny-en.json`. The `--baseline` flag
strips absolute timings (`elapsed_ms`, `ttff_ms`, `duration_s`) from
the JSON so the committed anchor is deterministic across CI runners.
Regeneration procedure + flapping-fixture mitigation documented in
`docs/bench/README.md`. R5.1 and R5.2 in
`docs/plans/2026-04-25-fono-roadmap-v2.md` now ticked as fully shipped.

Bonus: `tests/check.sh` lands as a single command that mirrors the CI
build/clippy/test matrix locally (full / `--quick` / `--slim` /
`--no-test` modes) so contributors can run the same gate before
pushing.

Verification (this session):

| Command | Result |
|---|---|
| `cargo build --workspace --all-targets` | clean |
| `cargo test --workspace --lib --tests` | green (all suites incl. new `parse_sidecar_*` tests) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |

## 2026-04-28 — Doc reconciliation pass

Pure-doc pass driven by `plans/2026-04-28-doc-reconciliation-v1.md`. No
Rust source touched. Highlights:

- **`crates/fono/tests/pipeline.rs` is not broken on `main`.** The earlier
  status entry below (line ~50) calling out an `Injector` signature
  mismatch was stale: the signatures align in the current source
  (`crates/fono/src/session.rs:140-142` vs
  `crates/fono/tests/pipeline.rs:54-58`) and the workspace test gate runs
  green. Verified this session: `cargo build --workspace`,
  `cargo test --workspace --lib --tests`, and `cargo clippy --workspace
  --no-deps -- -D warnings` are all clean.
- **Self-update plan `plans/2026-04-27-fono-self-update-v1.md`** —
  ~85% landed in commit `3e2c742` (2026-04-22) without ever being
  reflected in the plan tree. This pass ticks Tasks 1–11, 13–15
  (partial), 17–19 and adds an explicit Status header + Open
  follow-ups list. Remaining work (Tasks 12, 16, 20–22) carried
  forward as Wave 2 Task 8.
- **Equivalence accuracy gate plan
  `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md`**
  — ~50% landed in commits `b6596c0` and `7db29b5` (2026-04-28) as
  inline behaviour (`english_only = args.stt == "local" &&
  args.model.ends_with(".en")` at
  `crates/fono-bench/src/bin/fono-bench.rs:339`,
  `Metrics.stt_accuracy_levenshtein` at
  `crates/fono-bench/src/equivalence.rs:113-114`), without the typed
  `ModelCapabilities` API the plan describes. This pass ticks Tasks 7,
  8, 12, 17, 18 with annotations and carries the typed-API refactor
  forward as Wave 2 Task 7.
- **R3.1 in-wizard latency probe** shipped in commit `7bea0a9`
  (`crates/fono/src/wizard.rs:72, 720, 725`). The same commit advertised
  a "R5.1 CI bench gate" but only added `cargo bench --no-run`
  compile-sanity at `.github/workflows/ci.yml:64-68`; the real-fixture
  equivalence-harness gate is carried forward as Wave 2 Task 9.
  `docs/plans/2026-04-25-fono-roadmap-v2.md` Tier-1 reconciled to
  reality (R2.1, R3.1, R3.2, R3.3, R4.1, R4.2, R4.3, R4.4 ticked; R5.1
  demoted to partial).
- **Three obsolete plans superseded** by the
  `--allow-multiple-definition` link trick already live in
  `.cargo/config.toml:21-28`:
  `plans/2026-04-27-candle-backend-benchmark-v1.md`,
  `plans/2026-04-27-llama-dynamic-link-sota-v1.md`, and
  `plans/2026-04-27-shared-ggml-static-binary-v1.md` were moved to
  `plans/closed/` with `Status: Superseded` headers. None of the three
  was ever executed; the linker workaround landed first.
- **ADR backfill.** `docs/decisions/` previously listed only
  `0001`–`0004`, `0009`, `0015`, `0016` while plan history and status
  entries referenced `0005`–`0008` and `0010`–`0014`. Reconstructed
  stubs for the missing numbers landed this pass with `Status:
  Reconstructed (original lost in filter-branch rewrite)` headers, plus
  three new ADRs: `0017-auto-translation.md` (forward-reference for the
  pending feature), `0018-ggml-link-trick.md` (active `--allow-multiple-definition`
  decision), and `0019-platform-scope.md` (v0.x Linux-multi-package
  scope).

Verification (this session, `4517133` + doc edits only):

| Command | Result |
|---|---|
| `cargo build --workspace` | clean |
| `cargo test --workspace --lib --tests` | green |
| `cargo clippy --workspace --no-deps -- -D warnings` | clean |

## 2026-04-28 — Language allow-list (constrained Whisper auto-detect)

User reported: *"A lot of the people will use fono in more than one
language. But whisper might autodetect some of the other languages.
We need to be able to specify a list of languages that should be
considered and the others should essentially be banned."*

Plan: `plans/2026-04-28-stt-language-allow-list-v1.md`.

**Schema** — `[general]` and `[stt.local]` gain a new `languages:
Vec<String>` field. Empty = unconstrained Whisper auto-detect (today's
default); one entry = forced single language (today's `language = "ro"`);
two-or-more = constrained auto-detect: Whisper picks from the allow-list,
every other language is **banned**. The legacy scalar `language: String`
is still accepted on read and migrated into `languages` on first save
(`skip_serializing_if = "String::is_empty"` drops it from disk).

**Local Whisper** (`crates/fono-stt/src/whisper_local.rs`) — when an
allow-list is in effect, run `WhisperState::lang_detect` on the prefix
mel, mask probabilities to allow-list members only, argmax → run
`full()` with the picked code locked. Forced and Auto paths preserve
the previous one-pass behaviour (no extra cost).

**Cloud STT** (`groq.rs`, `openai.rs`) — banning is impossible at the
provider API. Two opt-in knobs on `[general]`:
`cloud_force_primary_language` (sends `languages[0]` instead of `auto`)
and `cloud_rerun_on_language_mismatch` (one extra round-trip when the
returned `language` is outside the allow-list). Defaults preserve the
current cost profile.

**New module** `crates/fono-stt/src/lang.rs` carries the
`LanguageSelection` enum (`Auto` / `Forced(code)` / `AllowList(Vec)`)
and the parser, so backends never compare sentinel strings like
`"auto"` directly.

**Wizard** — both `configure_cloud` and `configure_mixed` now persist
their language prompt (previously discarded into `_lang`) into
`general.languages` via `LanguageSelection::parse_csv`.

**Verification** — `cargo build --workspace`, `cargo test --workspace
--lib`, and `cargo clippy -p fono-stt -p fono-core -p fono --lib --bins
-- -D warnings` all green. New tests in `lang.rs` cover the parser /
normaliser; `config.rs::languages_round_trip_drops_legacy_field` and
`explicit_languages_wins_over_legacy_scalar` lock the migration.

The pre-existing `crates/fono/tests/pipeline.rs` `Injector` signature
mismatch is unrelated to this change and was already broken on
`main`.

## 2026-04-28 — Overlay focus-theft eliminated (X11 override-redirect)

User reported: *"The overlay window still seems to be stealing focus
twice; when it appears in live mode and when it does cleanup."*

The previous mitigation (`.with_active(false)` +
`WindowType::Notification`, landed in `1f23194`) is correct in spirit,
but X11 window managers disagree about how aggressively to honour
those hints across multiple map cycles. The overlay is shown → hidden
→ shown again twice per dictation (live state, then
processing/finalize state), and many WMs default to "give focus on
map" on the second-and-subsequent map even for notification toplevels.
Net result was that every overlay state transition re-stole focus
from the user's editor / terminal / browser, and the synthesized
`Shift+Insert` paste then landed in the overlay itself rather than
the original target window.

**Fix landed in `d2823f1`** (`crates/fono-overlay/src/real.rs:488-494`):
add `.with_override_redirect(true)` to the X11 window attributes on
top of the existing `.with_active(false)` and
`WindowType::Notification` hints. Override-redirect windows are
completely outside WM management — the X server never asks the WM
about focus, mapping, or stacking for them. This is what tooltips,
dmenu, and rofi all do; it makes focus theft physically impossible
on X11 regardless of WM behaviour.

**Trade-offs**

- WM-managed always-on-top is lost. Mitigation: borderless
  override-redirect windows naturally stack above normal toplevels
  because the WM never moves them on focus changes; no observable
  regression vs the prior `WindowLevel::AlwaysOnTop` hint.
- Compositor-managed transparency varies slightly across compositors
  for OR windows. picom honours it; KWin and Mutter compose it
  correctly. The solid-charcoal fallback at `COLOR_BG = 0xEE17171B`
  still applies if the compositor refuses the alpha channel.

**Wayland deferred to Slice B.** On Wayland the compositor controls
focus completely; the proper solution is `xdg_activation_v1` /
`wlr-layer-shell` from a dedicated overlay subprocess, which is the
Slice B subprocess-overlay refactor (ADR 0009 §5). For Slice A this
X11-only fix matches the dominant target environment.

**Verification**

| Command | Result |
|---|---|
| `cargo build  -p fono-overlay --features real-window` | clean |
| `cargo clippy -p fono-overlay --features real-window -- -D warnings` | clean |
| `cargo test   -p fono-overlay --lib` | 2/0 |

(Workspace clippy currently reports unrelated in-flight bench errors
from the v7 equivalence-fixtures swap; tracked separately.)

## 2026-04-27 — Slice A v7 delta landed (boundary heuristics)

Plan v7 (`plans/2026-04-27-fono-interactive-v7.md`) extends Slice A with
boundary-quality heuristics. Four DCO-signed commits on top of v6 Slice A:

| SHA       | Title |
|-----------|-------|
| `ce6a21e` | fono-core(config): v7 `[interactive]` keys (boundary heuristics) |
| `d0e21a0` | fono(live): R2.5 prosody/punct chunk-boundary + R7.3a hold-on-filler drain |
| `beae861` | fono-bench(equivalence): pin v7 boundary knobs + A2 row variants |
| `6a6c6c1` | docs: ADR 0015 + interactive.md tuning section |

**What landed**

- R9.1 — `[interactive]` config grew from 4 keys to 18, covering the v6
  carryover (`mode`, `chunk_ms_initial/steady`, `cleanup_on_finalize`,
  `max_session_seconds/cost_usd`) and the v7 heuristic knobs
  (`commit_use_prosody`, `commit_use_punctuation_hint`,
  `commit_hold_on_filler`, `commit_filler_words`,
  `commit_dangling_words`, plus matching `*_ms` extensions). Reserved
  `eou_adaptive` / `resume_grace_ms` defined but inert until Slice D.
- R2.5 — prosody pitch-tail tracker (hand-rolled time-domain
  autocorrelation, no FFT dep) wired into the FrameEvent → StreamFrame
  translator; punctuation-hint pure function shipped, full wiring
  deferred to Slice B (translator can't yet see preview text).
- R7.3a — filler/dangling-word suffix detection; ships as informational
  signal on `LiveTranscript` rather than a true drain extension to
  avoid an >80 LoC pump refactor. Daemon can act on the flags now;
  Slice D's adaptive-EOU work will make the extension first-class.
- R10.5 / R10.6 — tracing fields on `live.first_stable` + 13 new
  heuristic-isolation unit tests + 2 new equivalence-harness tests.
- R18.10 / R18.23 — pinned heuristic knobs in equivalence reports;
  four A2 row variants (`A2-no-heur`, `A2-default`, `A2-prosody`,
  `A2-filler`); `A2-default` gates Tier-1 + Tier-2.
- ADR 0015 — boundary-heuristics architecture, additive-only invariant,
  forward-reference to adaptive EOU in Slice D.

Verification gate (slim + `interactive` feature): build clean, clippy
clean with `-D warnings`, all tests green (no regressions).

## 2026-04-27 — Slice A landed (interactive / live dictation)

Plan v6 (`plans/2026-04-27-fono-interactive-v6.md`) Slice A is in.
Five commits on `main`, each DCO-signed:

| SHA       | Title |
|-----------|-------|
| `7fbf974` | Slice A checkpoint: streaming primitives, overlay, budget, live session |
| `92d4cc3` | Slice A: live pipeline integration tests (plan v6 R10.2) |
| `074a6c7` | Slice A: equivalence harness foundation + 2 fixtures (plan v6 R18) |
| `c3f2b68` | Slice A: ADR 0009 + interactive.md user guide (plan v6 R11) |
| (this)    | Slice A: docs/status.md — Slice A complete, Slice B queued |

The four Forge follow-up commits to `7fbf974` cover deliverables R10.2,
R18 (foundation), R11.1, R11.2, and R17 (status update).

### What Slice A actually ships

- **R1 / R3** — `fono-stt::StreamingStt` trait + `LocalAgreement`
  helper + dual-pass finalize lane on top of `WhisperLocal`. Gated
  behind the `streaming` cargo feature on `fono-stt`.
- **R2** — `fono-audio::AudioFrameStream` + `FrameEvent` enum + VAD-
  driven segment-boundary heuristic. Gated behind `fono-audio/streaming`.
- **R5** — Live overlay (`fono-overlay::OverlayState::LiveDictating`
  + `RealOverlay` winit window) painting preview / finalize text.
  In-process; sub-process refactor deferred to Slice B (see ADR 0009 §5).
- **R7.4 / R10.2** — `fono::live::LiveSession` orchestrator that wires
  `Pump` → `AudioFrameStream` → `StreamingStt` → overlay. Two new
  integration tests (`crates/fono/tests/live_pipeline.rs`) drive it
  with a synthetic `StreamingStt` and assert (a) two-segment
  concatenation under preview→finalize lanes and (b) clean
  cancellation when no voiced frames arrive.
- **R10.4** — `fono record --live` CLI — record-then-replay-through-
  streaming. Realtime cpal-callback push lands in Slice B.
- **R11.1** — ADR `docs/decisions/0009-interactive-live-dictation.md`
  capturing the six locked architectural decisions for Slice A.
- **R11.2** — User-facing guide `docs/interactive.md` covering
  `[interactive].enabled`, the `interactive` cargo feature, the
  `fono record --live` and `fono test-overlay` flows, and the two
  known issues (hostile compositors, Wayland focus theft).
- **R12** — `fono-core::BudgetController` (price table + per-minute
  ceiling + `BudgetVerdict::{Continue, StopStreaming}`) wired into
  `LiveSession::run`. Gated behind `fono-core/budget`.
- **R17.1 / R18 (foundation)** — Streaming↔batch equivalence harness
  in `crates/fono-bench/src/equivalence.rs` + `fono-bench equivalence`
  subcommand + two synthetic-tone WAV fixtures
  (`tests/fixtures/equivalence/{short-clean,medium-pauses}.wav`,
  ~410 KB total). 7 new unit tests cover the levenshtein
  normalization, JSON round-trip, overall-verdict aggregation, and
  manifest parsing. End-to-end smoke (`--stt local --model tiny.en`)
  produced PASS on both fixtures.

### Bug fixed in passing

`LiveSession::run` previously called `pump.subscribe()` *after* the
caller had pushed PCM and called `pump.finish()` — which loses every
frame because `tokio::sync::broadcast` does not deliver pre-subscribe
messages to fresh subscribers. `Pump` now pre-subscribes a primary
receiver at construction and exposes it via
`Pump::take_receiver()`; `LiveSession::run` takes a
`broadcast::Receiver<FrameEvent>` directly, and `fono record --live`
spawns the run task before pushing so the broadcast buffer drains
between pushes. Caught while landing the live integration tests; not
in scope of `7fbf974` itself.

### Build matrix (verified this session)

| Command | Result |
|---|---|
| `cargo build --workspace` | ✅ |
| `cargo build --workspace --features fono/interactive` | ✅ |
| `cargo clippy --workspace --no-deps -- -D warnings` | ✅ |
| `cargo clippy --workspace --no-deps --features fono/interactive -- -D warnings` | ✅ |
| `cargo test --workspace --lib --tests` | ✅ 110 ok, 0 fail (was 103 at HEAD) |
| `cargo test --workspace --lib --tests --features fono/interactive` | ✅ 126 ok, 0 fail |
| `cargo run -p fono-bench --features equivalence,whisper-local -- equivalence --stt local --model tiny.en --output report.json` | ✅ both fixtures PASS |

### Deferred to Slice B (next session candidates)

- **R4 / R8 / R10.4 (realtime)** — Cloud streaming providers (Groq,
  OpenAI realtime, Deepgram, AssemblyAI) and the realtime cpal-
  callback audio push so the overlay paints text *while* you speak.
- **R5.6** — Overlay sub-process refactor for crash isolation.
- **R18 cloud rows** — Cloud-streaming equivalence rows of R18
  (`--stt groq` and friends). Requires the cloud-mock recordings
  pipeline that the v6 plan R18.12 sketches.
- **R18 Tier-2** — With-LLM equivalence comparison (`--llm local
  qwen-0.5b`). The Tier-1 (whisper-only) gate is in; Tier-2 needs
  the deterministic-LLM scaffolding (n_threads=1 + seed-pinning) to
  produce stable outputs.
- **R18.6 fixture set completion** — The remaining 10 fixtures of the
  curated 12-fixture set (long-monologue, noisy-cafe, accented-EN,
  numbers/commands, whispered, with-music, multi-speaker,
  code-dictation, long-with-pauses, short-noisy-quick). Needs real
  CC0 audio sources.
- **R16** — Tray icon-state palette refactor.

### Recommended next session

1. **Slice B kickoff** — wire the realtime cpal-callback push and the
   first cloud streaming provider (Groq's faster-whisper streaming
   endpoint is the obvious first target — same auth flow as the
   existing Groq batch backend).
2. **Or, if Slice B is too big a chunk to start cold:** drop the
   remaining 10 R18 fixtures into `tests/fixtures/equivalence/` from
   real CC0 LibriVox / Common Voice clips, recompute SHA-256s, set
   `synthetic_placeholder = false` in the manifest, and tighten
   `TIER1_LEVENSHTEIN_THRESHOLD` from `0.05` back to the v6 plan's
   strict `0.01` in the same commit. Self-contained, fast feedback.

## Hotkey ergonomics — single-key defaults

Default hotkeys switched from three-key chords to single function keys:

- `toggle = "F9"` (was `Ctrl+Alt+Space`)
- `hold = "F8"` (was `Ctrl+Alt+Grave`)
- `cancel = "Escape"` (unchanged — only grabbed while recording)
- `paste_last` hotkey **removed**. The tray's "Recent transcriptions"
  submenu and the `fono paste-last` CLI cover the same need with a
  better UX (re-paste any of the last 10, not just the newest).

Touched: `crates/fono-core/src/config.rs`, `crates/fono-hotkey/{fsm,listener,parse}.rs`,
`crates/fono-ipc/src/lib.rs` (kept `Request::PasteLast` for CLI), `crates/fono/src/{daemon,wizard}.rs`,
`crates/fono-tray/src/lib.rs`, `README.md`, `docs/troubleshooting.md`, `docs/wayland.md`.

`Request::PasteLast` now routes directly to `orch.on_paste_last()` instead of
through the FSM, since there is no longer a hotkey path for it.

## Single-binary local STT + local LLM (ggml symbol collision resolved)

Default builds now ship **both** local STT (`whisper-rs`) and local LLM
(`llama-cpp-2`) statically linked into one self-contained `fono` binary —
the previous `compile_error!` guard in `crates/fono/src/lib.rs` is gone, and
`crates/fono/Cargo.toml` re-enables `llama-local` in `default`.

The `ggml` duplicate-symbol collision (each sys crate vendors its own static
`ggml`) is resolved at link time via `-Wl,--allow-multiple-definition` in
the new `.cargo/config.toml`. Both crates' `ggml` copies originate from the
same `ggerganov` upstream and are ABI-compatible; the linker keeps one set
of symbols and discards the duplicate. Verified post-link with
`nm target/release/fono | grep ' [Tt] ggml_init$'` → exactly one entry.

A new smoke test `crates/fono/tests/local_backends_coexist.rs` constructs a
`WhisperLocal` and a `LlamaLocal` in the same process to guard against
runtime breakage from any future upgrade of either sys crate.

### Hardware acceleration banner

Every daemon start now logs an `info`-level summary of the actual
accelerator path the binary will use, e.g.:

```
hw accel     : CPU AVX2+FMA+F16C
```

Implemented in `crates/fono/src/daemon.rs::hardware_acceleration_summary`.
GPU backends are wired through opt-in cargo features
(`accel-cuda` / `accel-metal` / `accel-vulkan` / `accel-rocm` /
`accel-coreml` / `accel-openblas`) on `fono`, `fono-stt`, and `fono-llm`;
flipping any of them prepends the matching label (e.g. `CUDA + CPU AVX2`).
The default ship build stays CPU-only — single binary, runs everywhere,
auto-picks the best SIMD kernel ggml has compiled in.

## H8 landed — real local LLM cleanup via `llama-cpp-2`

`crates/fono-llm/src/llama_local.rs` is no longer a stub. The `llama-local`
feature now runs honest GGUF inference: process-wide `LlamaBackend` cached in
a `OnceLock`, lazy model load via `Arc<Mutex<Option<LlamaModel>>>` (mirrors
`WhisperLocal`), greedy sampling, ChatML prompt template that fits both
Qwen2.5 and SmolLM2, `MAX_NEW_TOKENS = 256`, EOS + `<|im_end|>` stop tokens,
and a `tokio::task::spawn_blocking` boundary so the async runtime keeps
moving while llama.cpp grinds. The factory grew an `llm_models_dir` parameter
that resolves `cfg.local.model` (a name) to `<dir>/<name>.gguf` — the
existing scaffold's "model NAME passed as a path" bug is gone.

A cleanup that takes > 5 s emits a `warn!` recommending the user pick a
cloud provider (`fono use llm groq` / `cerebras`) or a smaller model. CPU-only
Q4_K_M inference of a 1.5B-parameter model is on the order of 5–15 tok/s on
a laptop, so this matters: the wizard continues to default-skip the local
LLM for tiers ≤ `Recommended`. Local LLM model auto-download (H9 / H10) is
still open — follow-up.

**Build constraint.** `whisper-rs-sys` and `llama-cpp-sys-2` each statically
link their own copy of ggml; combining both in one binary collides on every
`ggml_*` symbol. We keep the static-binary stance (no sidecar `libllama.so`)
by guarding the combo with a `compile_error!` in `crates/fono/src/lib.rs`.
Default-features build (whisper-local + cloud LLM) works as before. Users
who want local LLM cleanup build cloud-STT instead:

```
cargo build --release --no-default-features --features tray,llama-local,cloud-all
```

Lifting this constraint requires moving llama.cpp to a shared library
(`llama-cpp-sys-2/dynamic-link`), which is **not** the path forward — fono
ships as a single self-contained binary.

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
| `plans/2026-04-27-fono-self-update-v1.md` | ~85% landed in `3e2c742`; finishing pass tracked as Wave 2 Task 8 |
| `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md` | ~50% landed in `b6596c0`/`7db29b5`; typed-API refactor tracked as Wave 2 Task 7 |
| `plans/2026-04-28-fono-auto-translation-v1.md` | Not started (Wave 4 of revised strategic plan) |
| `plans/closed/` (candle / dynamic-link / shared-ggml) | Superseded by `--allow-multiple-definition` link trick (ADR 0018) |

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

> Recommended next session: execute **Wave 3** of the revised strategic
> plan (Slice B1 — realtime cpal-callback push + first cloud streaming
> provider). Wave 2 landed in three DCO-signed commits:
> `76b9b08` (typed `ModelCapabilities` + split equivalence/accuracy
> thresholds), `87221a2` (per-asset `.sha256` sidecar verification +
> `--bin-dir` CLI flag), and the Thread-C CI gate commit (real-fixture
> `fono-bench equivalence` run against
> `docs/bench/baseline-comfortable-tiny-en.json` on every PR).
>
> Wave 3 concretely:
>
> 1. **Realtime cpal-callback push** (R4 / R10.4 of
>    `plans/2026-04-27-fono-interactive-v6.md`). Replace the
>    record-then-replay live path so the overlay paints text *as the
>    user speaks*. The `Pump` / `broadcast` plumbing landed in
>    Slice A; this is now scope-bounded.
> 2. **Groq streaming STT backend** (R8). Same auth path as the
>    existing Groq batch backend; the `StreamingStt` trait already
>    lives at `crates/fono-stt/src/streaming.rs`. Selectable via
>    `fono use stt groq` with `[interactive].enabled = true`.
> 3. **Equivalence harness cloud rows** (R18.12). Mocked-HTTP
>    recordings so the CI gate runs offline; extend
>    `docs/bench/baseline-comfortable-tiny-en.json` (or sibling) once
>    cloud rows produce stable verdicts.

### Earlier next-session notes (preserved for context)

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
