# Changelog

All notable changes to Fono are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] — 2026-05-02

### Added

- **GPU-accelerated release variant.** Releases now ship two
  binaries side-by-side: the default `fono-vX.Y.Z-x86_64` (compact
  ~18 MB CPU-only build, NEEDED set of 4 universal glibc libs) and
  `fono-gpu-vX.Y.Z-x86_64` (Vulkan-enabled ~60 MB build, additionally
  links `libvulkan.so.1`). Both built from the same source; only
  the `accel-vulkan` cargo feature differs. Distro packages
  (`.deb` / `.pkg.tar.zst` / `.txz` / `.lzm`) are CPU-only at this
  release; raw GPU binary + `.sha256` ship as release assets.
  Per `plans/2026-05-02-fono-cpu-gpu-variants-v1.md` slice 1.
  CUDA / ROCm remain build-from-source-only; Vulkan covers ~80 % of
  NVIDIA / ~90 % of AMD perf at zero vendor lock-in.
- **Build variant identification.** `fono doctor` and the daemon
  startup log now report which variant is running (`cpu` /
  `gpu`). New `fono::variant::Variant` enum + `VARIANT` constant
  in `crates/fono/src/variant.rs` for runtime introspection (and
  for the upcoming GPU upgrade UX).
- **Runtime Vulkan probe.** `fono doctor` gains a "Compute backends"
  section that reports the host's Vulkan loader + physical device
  state (e.g. *"Vulkan: detected (Intel(R) Iris(R) Xe Graphics,
  llvmpipe (LLVM 22.1.3, 256 bits))"*). On a CPU-variant binary
  with a Vulkan-capable GPU detected, an upgrade hint points at
  the `fono-gpu` release asset. Implemented via `ash` runtime-loaded
  bindings (`Entry::load()` → `dlopen("libvulkan.so.1")`) so the
  CPU variant still has the strict 4-NEEDED-entry allowlist —
  libvulkan never appears in NEEDED. Module lives at
  `crates/fono-core/src/vulkan_probe.rs` behind the `vulkan-probe`
  feature; both `fono` and `fono-update` opt in. Slice 2 of
  `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`.
- **Auto-variant `fono update`.** Every `fono update` invocation now
  probes Vulkan on the host and fetches the matching release asset:
  `fono-vX.Y.Z-x86_64` when no usable GPU is present, or
  `fono-gpu-vX.Y.Z-x86_64` when libvulkan + a physical device are
  available. CPU users on GPU-equipped hardware are switched to
  the GPU build on their next update; if they later move to a
  GPU-less machine, the next update switches them back. No CLI
  flag, no wizard prompt, no config knob — one decision in one
  place. `fono_update::check` now takes the running binary's
  current asset prefix and treats a prefix mismatch as "update
  available" even at the same version. Slice 3 of
  `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`.
- **Tray "Update for GPU acceleration" entry.** On a CPU-variant
  build with a usable Vulkan host, the tray menu surfaces an
  explicit "Update for GPU acceleration" item that triggers the
  same auto-variant `apply_update` path. Hidden on GPU builds and
  on hosts without Vulkan. New `fono_tray::TrayAction::UpdateForGpuAcceleration`
  + `GpuUpgradeProvider` callback type.
- **CI gate split.** The `Binary size & deps audit` job now runs as
  a matrix `(cpu, gpu)`, asserting both variants stay within their
  respective budgets and NEEDED allowlists. CPU: ≤ 20 MiB + 4-entry
  allowlist (unchanged). GPU: ≤ 64 MiB + 4-entry allowlist
  + `libvulkan.so.1`.

- **`fono install` / `fono uninstall` self-installer.** Run
  `sudo fono install` (or `sudo ./fono-vX.Y.Z-x86_64 install` from a
  fresh release-asset download) to install fono system-wide on a
  desktop: places the binary at `/usr/local/bin/fono`, drops a menu
  desktop entry, an `/etc/xdg/autostart/fono.desktop` entry so the
  daemon launches automatically on next graphical login, the icon,
  and shell completions. Add `--server` for a headless install
  instead: writes a hardened systemd unit at
  `/lib/systemd/system/fono.service` running as a dedicated `fono`
  system user, and enables-and-starts it immediately. `--dry-run`
  prints the planned actions without touching the filesystem on
  either mode. `sudo fono uninstall` reads the install marker
  written at install time and removes exactly the files it recorded;
  per-user config and history are never touched. `fono doctor` now
  reports the install state (self-installed desktop / server,
  package-managed, or ad-hoc on PATH).

## [0.4.0] — 2026-05-02

### Added

- **Wyoming Home Assistant wire compliance.** Frames now use canonical
  Wyoming framing (header `version` + `data_length` with a separate
  JSON data block; `WYOMING_VERSION = "1.8.0"`). `info.asr` is now a
  `Vec<AsrProgram>` per Home Assistant's all-services-as-arrays
  expectation, with placeholder arrays for tts/handle/intent/wake/mic/
  snd/satellite. Server queues `transcribe` arriving before
  `audio-stop` to match Home Assistant client behavior. New
  `decode_pcm_le` handles variable bit-width and multi-channel
  `audio-chunk` headers. New round-trip test
  `server_accepts_home_assistant_transcribe_before_audio`.
- **Discovered-server tray UX.** Tray gains a "Discovered Wyoming
  servers" submenu under STT backend; clicking a peer hot-reloads the
  daemon's STT config to point at the chosen remote. Daemon filters
  its own local instance out of the discovered list. mDNS advertiser
  uses `enable_addr_auto()` so A/AAAA records track network topology
  changes.
- **Glibc symbol-version compat.** Both the size-budget CI gate and
  the release build matrix now pin `runs-on: ubuntu-22.04` (glibc
  2.35), so the shipped binary runs on Ubuntu 22.04+, Debian 12+,
  Fedora 36+, and any host with glibc ≥ 2.35.

### Changed

- **Canonical ship target is glibc-dynamic, not static-musl.**
  `release.yml` builds `x86_64-unknown-linux-gnu` `release-slim` (it
  always did); the new `Binary size & deps audit` CI gate mirrors
  that target and asserts (a) size ≤ 20 MiB (measured at release:
  18.08 MB, ~2 MB headroom) and (b) NEEDED set is exactly `libc.so.6
  libm.so.6 libgcc_s.so.1 ld-linux-x86-64.so.2`. Modern glibc (≥ 2.34)
  merges libpthread/librt/libdl into libc.so.6. Anything else (libgtk,
  libstdc++, libgomp, libayatana, libxdo, libasound, libxkbcommon,
  libwayland-*) fails the gate. ADR 0022 amended 2026-05-02; the
  original "no shared libraries" wording is superseded.
- **CI job names** rewritten for clarity: `test (ubuntu-latest)` →
  `Build & test (ubuntu-latest)`; `size-budget (release-slim)` →
  `Binary size & deps audit`; `cargo-deny` → `License & advisory
  audit`; `build ($target)` → `Release binary ($target)`.
- **Server name** `"fono"` → `"Fono"` for UI consistency in Home
  Assistant and elsewhere.

### Deferred

- **Static-musl single binary (Phase 2.4 of the binary-size plan).**
  `messense/rust-musl-cross:x86_64-musl` ships a `libgomp.a` that is
  non-PIC (breaks `-static-pie`) and references glibc-only symbols
  (`memalign`, `secure_getenv`) plus a chain of POSIX symbols whose
  resolution depends on rust's link order. Eleven CI commits chased
  the chain (preserved in `git log` as `901e41d..29cc577`, superseded
  in spirit by `d2b54cb`). Resurrection path: switch `llama-cpp-2`
  fork to llvm-openmp (libomp is PIC-friendly) **or** pin a PIC-built
  libgomp.a from GCC sources in our own minimal cross image. Not
  blocking the desktop ship target.

### Fixed

- **CI cache cross-glibc contamination.** Suffix the
  Swatinem/rust-cache key with the runner image
  (`size-budget-ubuntu-22.04`, `${{ matrix.target }}-${{ matrix.os }}`)
  so cached build-script binaries don't migrate between runner-glibc
  generations and fail at execute-time with `version 'GLIBC_2.X' not
  found`.

## [0.3.7] — 2026-04-30

### Changed

- **Binary size & shape — single 20 MiB static-musl ELF** (in progress
  per `plans/2026-04-30-fono-single-binary-size-v1.md`, ADR 0022).
  Fono ships as **one** binary that runs as desktop client, headless
  server, or LAN client of a remote peer; no `--features
  server`/`gui`/`headless` flavours. Graphical surfaces (tray,
  overlay, text injection) are runtime-detected from `DISPLAY` /
  `WAYLAND_DISPLAY` and silently no-op when the host is headless.
  This release lands the prep work: dead-code link flags
  (`-Wl,--gc-sections,--as-needed`), C/C++ size flags
  (`-Os -ffunction-sections -fdata-sections`), static llama.cpp C++ +
  OpenMP runtime linkage via fork features (`static-stdcxx`,
  `static-openmp`), daemon tray runtime gate on `DISPLAY` /
  `WAYLAND_DISPLAY`, and a new `tests/check.sh --size-budget` gate that
  asserts ≤ 20 MiB + `ldd`-empty + single ggml on the canonical
  `release-slim x86_64-unknown-linux-musl` artefact. Subsequent slices
  land source-level shared ggml and the remaining musl toolchain fixes
  that close the budget.
- **`llama-cpp-2` / `llama-cpp-sys-2` pinned to fork** at
  `github.com/bogdanr/llama-cpp-rs` branch `feature/static-runtime-linkage`
  via `[patch.crates-io]`. The branch includes the upstream-submitted
  default-on `common` cargo feature gating `llama.cpp`'s `common/`
  static library and the `wrapper_common` / `wrapper_oai` C++ shims
  (~24 MB of static archives), plus follow-up `static-openmp` and
  `static-stdcxx` features. Fono builds with `default-features = false,
  features = ["openmp", "static-openmp", "static-stdcxx"]`, so it opts
  out of `common` and links llama.cpp's `libgomp` / `libstdc++`
  statically. `cargo build --release -p fono` no longer has
  `libgomp.so.1` or `libstdc++.so.6` in `NEEDED`; the remaining GNU
  shared libraries are `libasound`, `libgcc_s`, `libm`, `libc`, and the
  dynamic loader until the musl ship build is fully operational.
  `common` patch submitted upstream as
  [utilityai/llama-cpp-rs#1015](https://github.com/utilityai/llama-cpp-rs/pull/1015);
  fork stays in place until merge.
- **Tray backend swapped from `tray-icon` (libappindicator + GTK3) to
  pure-Rust `ksni`** (Unlicense, public-domain), Phase 2 Task 2.1 of
  the binary-size plan. Drops `tray-icon`, `gtk`, `gdk`, `cairo-rs`,
  `pango`, `gdk-pixbuf`, `glib`, plus their `*-sys` shims and the
  libappindicator runtime — every transitive dep that pulled libgtk-3,
  libgdk-3, libcairo, libpango, libgio-2.0, libglib-2.0, and
  libgdk_pixbuf into the binary's `NEEDED` list. `ksni` speaks SNI +
  `com.canonical.dbusmenu` over `zbus` directly; KDE Plasma, GNOME
  (with the SNI shell extension), sway+waybar, hyprland+waybar,
  i3+i3status, xfce4-panel, and lxqt-panel all host SNI natively.
  Public API of `fono-tray` (`Tray::set_state`, `spawn`, providers,
  actions) unchanged; the daemon's tray spawn site at
  `crates/fono/src/daemon.rs:328` needed no edit. Architectural
  keystone of the "no shared libraries" promise on the static-musl
  ship build.

### Removed

- Unused `[workspace.dependencies]` declarations: `ort`, `rodio`,
  `swayipc`, `hyprland`. Confirmed zero `use` sites in the codebase;
  cosmetic cleanup, no binary impact.

### Added

- LAN **autodiscovery** via mDNS / DNS-SD (Slice 4 of the network
  plan). New `fono-net::discovery` module hosts an always-on passive
  `Browser` that maintains an ephemeral `Registry` of
  `_wyoming._tcp.local.` and `_fono._tcp.local.` peers, plus an
  automatic `Advertiser` that publishes the local Wyoming server when
  `[server.wyoming].enabled` is set. `[network].instance_name` remains
  available as an optional friendly-name override; there are no user-facing
  discovery enable/disable booleans. Discovered peers carry a typed
  `DiscoveredPeer { kind, hostname, port, proto, version, caps,
  model, auth_required, path, … }` with `host_port()` /
  `tray_label()` accessors so the tray and CLI render identical
  labels. Discovery state is **never** persisted — restart Fono and
  the LAN is rediscovered fresh, eliminating a whole class of
  stale-config bugs. Single new dependency: `mdns-sd 0.13`
  (pure-Rust, dual MIT/Apache-2.0, no Avahi/Bonjour FFI).
- IPC `Request::ListDiscovered` / `Response::Discovered(Vec<…>)`
  exposing the live registry to clients of the daemon. Snapshot
  conversion strips `Instant` / `IpAddr` for cross-process safety
  and reports peer age as `age_secs: u64`.
- New CLI `fono discover [--json]` prints the daemon's current
  registry as a fixed-width table or pretty JSON for scripting.
- Daemon goodbye-on-exit: graceful shutdown unregisters the mDNS
  publication so peers evict immediately rather than waiting for
  TTL.
- Integration test `crates/fono-net/tests/discovery_round_trip.rs`
  drives a real advertiser and a real browser on two independent
  `ServiceDaemon` instances over loopback multicast, asserting the
  TXT round-trip (`proto`, `model`, `caps`, `auth`) lands in the
  registry within 5 s. Skips cleanly on sandboxes without multicast.
- Wyoming-protocol STT **server** (`fono-net::wyoming::server`,
  `[server.wyoming]` config block). When enabled, the daemon hosts a
  Wyoming-compatible STT listener on the LAN backed by whatever
  `Arc<dyn SpeechToText>` the active config selects (local whisper-rs,
  Groq, OpenAI, Wyoming relay, …) — Home Assistant satellites and
  other Wyoming peers can route inference through this instance. Off
  by default; opt in via `[server.wyoming].enabled = true`. Loopback-
  only by default; set `[server.wyoming].bind` to `0.0.0.0`, `::`, or a
  specific interface address to expose it beyond the local machine.
  Provider-closure design tracks `Reload`-driven backend swaps without
  restarting the listener. Streaming-response
  (`transcript-start`/`-chunk`/`-stop`) lane will plug in once
  `Arc<dyn StreamingStt>` is plumbed; the one-shot `transcript`
  envelope is fully wired today and advertised via
  `info.asr.supports_transcript_streaming = false`. Two integration
  tests drive the real `WyomingStt` client (Slice 2) against the real
  server with a recording mock STT underneath, verifying the int16 LE
  PCM round-trip survives the wire end-to-end. Slice 3 of
  `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`.
- New internal `fono-net` crate hosting the LAN server + future mDNS
  browser/advertiser (Slice 4) + Fono-native WebSocket protocol
  (Slices 5–6). Wyoming-server feature is default-on; slim builds can
  opt out via `default-features = false`.

- Wyoming-protocol STT client backend (`SttBackend::Wyoming`,
  `[stt.wyoming]` config block). Fono can now use any
  Wyoming-compatible STT server on the LAN — `wyoming-faster-whisper`,
  `wyoming-whisper-cpp`, Rhasspy, Home Assistant satellites, and
  future `fono serve wyoming` daemons — as a drop-in cloud STT
  replacement that runs over TCP on the local network. Default port
  10300, optional model + auth-token hints, IPv6-literal URIs
  supported, fresh connection per `transcribe()` call, `prewarm()`
  pre-pays TCP handshake by issuing `describe`/`info`. Both the
  one-shot `transcript` flow and the streaming
  `transcript-start`/`-chunk`/`-stop` flow are handled by the same
  client. Two integration tests stand up an in-process Wyoming
  server stub and round-trip canned transcripts over a real loopback
  socket. Slice 2 of
  `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`.
- Internal `fono-net-codec` crate carrying the wire-format primitives
  for the upcoming network-inference work: a transport-agnostic
  `Frame { kind, data, payload }` codec covering Wyoming's JSONL
  header + optional UTF-8 data block + optional binary payload, typed
  event structs for the Wyoming STT subset (audio / describe / info /
  transcribe / transcript + streaming variants) and the Fono-native
  protocol (hello / cleanup / history / context / error / ping /
  pong), and a connection-arm allow-list that rejects cross-protocol
  events at parse time. Foundation only — no network I/O yet; full
  client + server slices, mDNS autodiscovery, and tray integration
  follow per
  `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`.

## [0.3.6] — 2026-04-29

### Added

- Empty-transcript microphone recovery. When a recording lasts at
  least 3 seconds but produces no transcribed text — the typical
  symptom of an external dock advertising a passive capture endpoint
  the OS elected as the default source — Fono now pops a critical
  desktop notification naming the silent device, the recording
  duration, and the recourse: switch via the tray "Microphone"
  submenu, `pavucontrol`, or your OS sound settings. Auto-suggested
  alternatives are filtered to exclude HDMI / monitor / loopback /
  S/PDIF decoys.
- Tray "Microphone" submenu (Linux desktops with PulseAudio /
  PipeWire). One row per source the audio server reports, marked
  with the system default. Clicking a row runs
  `pactl set-default-source` so the change applies system-wide and
  is reflected in `pavucontrol` / GNOME / KDE settings, then
  hot-reloads the daemon so the next capture opens the new
  endpoint. Hidden on hosts where `AudioStack::detect()` returns
  `Unknown` (macOS, Windows, pure-ALSA Linux) — the OS owns
  microphone selection there.

### Changed

- Microphone enumeration is now PulseAudio-first on Linux. When the
  audio stack is `PulseAudio` or `PipeWire` (Pulse compat layer),
  Fono lists sources via `pactl list sources` instead of cpal's
  ALSA host. Submenu rows show the source's friendly description
  ("Built-in Audio Analog Stereo", "Logitech BRIO") instead of
  cpal's raw `plughw:CARD=…` PCM names; the chronic
  `snd_pcm_dsnoop_open: unable to open slave` errors and the
  ALSA plugin pseudo-device clutter (`pulse`, `oss`, `speex`,
  `default`, `surround51`, …) that previously appeared in the
  submenu are gone. macOS, Windows, and pure-ALSA Linux fall back
  to cpal enumeration — unchanged.
- Microphone selection is fully delegated to the OS layer. Fono
  follows the PulseAudio / PipeWire default-source on Linux, the
  macOS Sound input device, and the Windows recording default.
  `pavucontrol`, GNOME / KDE settings, System Preferences and the
  Sound control panel are the canonical places to choose a
  microphone.
- `fono doctor` "Audio inputs:" section is now informational only.
  Lists every device the active stack reports with one row marked
  as the OS default; advice points at the tray submenu, pavucontrol,
  or OS sound settings.

### Removed

- Tray "Languages" submenu removed. The Languages submenu that
  previously listed the configured BCP-47 peer set and offered
  a "Clear language memory" action has been removed from the tray.
  The language cache is cleared automatically and language preference
  is managed via `config.toml` or `fono use language`.
- `[audio].input_device` config field. Fono no longer keeps a
  capture-device override; the OS default is always used.
- `fono use input <name>` CLI subcommand. Use the tray "Microphone"
  submenu, `pavucontrol`, or your OS audio settings instead.
- First-run wizard's microphone picker. New users get the OS
  default; switching afterwards is a tray-submenu click on Linux
  desktops or an OS-settings change elsewhere.
- `[general].language` (deprecated scalar — use `[general].languages`).
- `[stt.local].language` (deprecated scalar — use
  `[stt.local].languages` or `[general].languages`).
- `[general].cloud_force_primary_language` (superseded by the
  in-memory language cache shipped in v0.3.x).
- `cloud_force_primary` builders, struct fields, and dead first-pass
  branches on `GroqStt`, `GroqStreaming`, and `OpenAiStt`.
- `TrayAction::ClearInputDevice` variant (no override to clear).

## [0.3.5] — 2026-04-29

### Fixed

- Whisper trailing-closer hallucinations ("Thank you", "Bye", "Thanks
  for watching") on silent tails. Three layers, root-cause-first:
  - **Layer A** — local `whisper-rs` now opts in to the four
    hallucination guards that `FullParams::new()` leaves disabled by
    default: `set_no_speech_thold(0.6)`, `set_logprob_thold(-1.0)`,
    `set_compress_thold(2.4)`, `set_temperature_inc(0.2)`. Matches
    the canonical whisper.cpp CLI defaults.
  - **Layer B** — new `[stt.prompts]` config: a per-language
    `HashMap<bcp47, String>` whose entry for the request's resolved
    language is sent as the Whisper `initial_prompt` (local) or
    `prompt` (Groq + OpenAI form-data field). When no entry matches
    the resolved language, no prompt is sent — preserving today's
    unbiased behaviour for languages the user hasn't configured.
    English-only Whisper variants (e.g. `tiny.en`, `small.en`,
    `*-en-q5_1`) auto-seed `prompts.en` with a neutral professional-
    dictation default unless the user already set one.
  - **Layer C** — `interactive.hold_release_grace_ms` default
    lowered from 300 ms to 150 ms. Halves the silent tail Whisper
    sees on F8 release. Smoke-test: if trailing words get truncated,
    raise back to 300.
- LLM cleanup observability: new INFO line `llm: cleanup added=N
  removed=M chars` after each successful cleanup so users can see
  whether the LLM is doing real work or operating as a near-no-op
  pass-through.

### Removed

- `[stt.cloud].streaming` config field. Streaming for cloud Groq is
  now derived from `[interactive].enabled` — the master live-
  dictation switch — so there is no separate per-backend opt-in. A
  user who picks Groq and turns on live mode gets the pseudo-stream
  client automatically; cost can be bounded via
  `interactive.streaming_interval > 3.0` (finalize-only mode) or
  `interactive.budget_ceiling_per_minute_umicros`. Existing configs
  with `streaming = true` parse without warning (serde silently
  ignores unknown fields); the value is no longer consulted. Plan:
  `plans/2026-04-29-streaming-config-collapse-v1.md`.
- `[interactive].overlay` config field. The live-dictation overlay
  is now always shown when `[interactive].enabled = true` — it is
  the only feedback surface for live previews, so a per-section
  toggle was incoherent. The previous warn-and-ignore code path
  (added in v0.3.3) is gone. `[overlay].enabled` continues to
  control the passive recording indicator in batch mode.
- Wizard's third question on the cloud-STT path ("Enable Groq
  streaming dictation?"). Live-mode users on Groq now go straight
  through; users who want batch-only Groq just leave
  `[interactive].enabled = false`.

- `general.notify_on_dictation` config field. Redundant with the
  existing clipboard-fallback notification: when injection works the
  cleaned text is already at the cursor (the actual feedback); when
  it falls back to clipboard the dedicated `"Fono — copied to
  clipboard"` toast at `session.rs:171` fires with a Ctrl+V hint.
  The per-dictation toast just duplicated case 1.
- "Fono — live dictation active" toast on first F9 toggle-on.
  The on-screen overlay is the user-visible indicator.
- "Fono — STT switched" / "Fono — LLM switched" tray success toasts.
  The user just clicked the tray menu and the tray label updates to
  reflect the change. Switch *failures* still fire critical-urgency
  notifications.

### Changed

- Linux desktop notifications now route through `notify-send` (libnotify
  CLI) instead of `notify-rust`'s pure-Rust zbus path. Fixes a class of
  "no notification appeared" bugs in non-canonical environments (root
  sessions without `XDG_RUNTIME_DIR`/`DBUS_SESSION_BUS_ADDRESS`,
  systemd `--user` units without `PassEnvironment=`, container
  desktops, Flatpak/Snap launchers, etc.) where libnotify's autolaunch
  succeeds but zbus fails with "No such file or directory". `notify-rust`
  is retained behind `cfg(any(target_os = "macos", target_os =
  "windows"))` for the future cross-platform ports. New
  `fono_core::notify::send()` helper funnels every notification through
  one code path; ~40 inline `notify_rust::Notification::new()` call
  sites in `daemon.rs`/`session.rs` removed.

### Added

- `interactive.hold_release_grace_ms` config (default `300`). On F8
  release (and F9 toggle-off), the orchestrator now waits this many
  milliseconds before signalling the capture thread to stop. Closes a
  truncation bug where the last 100–300 ms of audio buffered in the
  cpal host callback were abandoned when the user released the hotkey
  early on a short utterance.
- Desktop notification on cloud STT rate-limit (HTTP 429), deduped to
  at most once per dictation session (per F8/F9 press). Surfaces via
  `notify-rust` in the default build; slim builds without the `notify`
  feature still emit a `tracing::warn!` line. A defensive 120 s
  auto-reset re-arms the flag if the orchestrator's reset path is
  skipped (e.g. by panic).
- 60-second preview-lane throttle after any cloud STT 429. The
  streaming pseudo-stream loop checks
  `rate_limit_notify::is_throttled()` before each preview tick and
  skips it; only VAD-boundary finalize requests fire during the
  throttle window. Self-clears after 60 s.
- Single-instance guard via the IPC socket. The daemon now probes the
  Unix socket on startup with `UnixStream::connect`; if a previous
  daemon answers, we bail before duplicating hotkey grabs and model
  loads. Stale sockets from crashed prior runs yield
  `ConnectionRefused` and proceed normally. No PID file parsing, no
  process probing — the socket itself is the source of truth.

### Changed

- Hotkey dispatch and live-dictation start/stop now log at DEBUG —
  the existing `pipeline ok: capture=… stt=… llm=… inject=…`
  summary at INFO is enough at default verbosity. Bump
  `RUST_LOG=fono=debug` to see the per-event detail. 429 sites
  upgraded from `tracing::info!` to `tracing::warn!` so they
  appear at default log level, with the verbose JSON body now
  compacted to a single human-readable line (model + RPM ceiling
  + retry-in seconds) instead of being dumped raw. Streaming
  finalize and preview lanes detect 429 in the closure-error
  string and trip the same warn + notification + throttle path
  the batch backend uses.

### Fixed

- Hotkey-grab conflicts on X11 no longer print the bare
  `X Error of failed request: BadAccess … X_GrabKey` to stderr.
  A custom `XSetErrorHandler` is installed at daemon startup that
  converts BadAccess-on-XGrabKey into an actionable
  `tracing::error!` message naming the conflict and pointing at
  `[hotkeys].hold` / `[hotkeys].toggle` in the config. Other X11
  errors are surfaced at WARN with their numeric codes instead of
  being printed by libxlib's default handler.

## [0.3.3] — 2026-04-28

### Added

- `interactive.streaming_interval` config (seconds, f32). Default `1.0`.
  Controls the cloud streaming preview cadence formerly hardcoded at
  700 ms. Valid range `[0.5, 3.0]`; values above `3.0` disable the
  preview lane entirely (only VAD-boundary finalize requests are sent —
  recommended for free-tier cloud users with strict per-minute caps).
  Values below `0.5` are clamped up; NaN/negative collapses to `1.0`.
- HTTP 429 detection in Groq cloud requests. When the cloud responds
  with `429 Too Many Requests`, an INFO log line now suggests bumping
  `interactive.streaming_interval` to `2.0` or higher.

### Changed

- The overlay is now always shown when streaming/interactive mode is
  enabled. `[interactive].overlay = false` is ignored (with a warning)
  while `[interactive].enabled = true`, because the overlay is the
  only feedback surface for live previews — without it there is no
  user-visible signal that streaming is doing anything. To run without
  the overlay, set `[interactive].enabled = false` and use batch mode.

## [0.3.2] — 2026-04-28

Hotfix: cloud STT post-validation gate did not actually run because the
default `json` response format does not include the detected language.
v0.3.1's confidence-aware rerun was correct but unreachable.

### Fixed

- Cloud STT post-validation gate now actually fires. The first-pass
  Groq / OpenAI request was using `response_format=json` (the implicit
  default), which does **not** include the detected `language` field —
  only `verbose_json` does. The post-validation block at
  `groq.rs:271`/`openai.rs:217`/`groq_streaming.rs:399` therefore
  silently skipped on every call, even when Groq returned Bulgarian
  for English audio with `languages = ["ro", "en"]`. Both batch and
  streaming first-pass requests now send `response_format=verbose_json`
  (zero latency cost — same endpoint, different output shape).
- Detected language is now normalised from Whisper's full English name
  (`"english"`, `"bulgarian"`) to alpha-2 (`"en"`, `"bg"`) before the
  allow-list check, via a new `crate::lang::whisper_lang_to_code`
  helper covering all 99 Whisper-supported languages. Without
  normalisation, `"bulgarian" != "bg"` would have prevented the gate
  from firing even with `verbose_json`.

## [0.3.1] — 2026-04-28

Hotfix for a cold-start banned-language injection bug in cloud STT.

### Fixed

- Cloud STT cold-start banned-language injection. When Groq's first
  response on a fresh session was a banned language (e.g. English audio
  misdetected as Russian) and the in-memory language cache was still
  empty, the unforced response was injected verbatim — producing
  Russian text on screen for an English speaker with `languages =
  ["ro", "en"]`. The rerun branch now runs a confidence-aware loop
  across every allow-list peer, requesting `verbose_json` to obtain
  per-segment `avg_logprob`, and injects the transcript with the
  highest mean log-probability (the language Whisper was most sure
  about). The previous warm-cache rerun path used a single forced
  retry; it now uses the same all-peers-by-confidence selection,
  closing the symmetric failure mode where the cache happened to hold
  a stale peer. Applied identically to the batch (`groq.rs`),
  streaming finalize (`groq_streaming.rs`), and OpenAI (`openai.rs`)
  backends. Streaming preview lane now suppresses banned-language
  partials so users do not briefly see Russian / Bulgarian / etc. on
  the overlay before the corrected finalize result arrives.
- Banned-language detections now log at INFO level with the detected
  code, banned-vs-allowed list, and chosen rerun action, so users can
  diagnose misdetections from the daemon log without enabling DEBUG.

## [0.3.0] — 2026-04-28

Cloud STT now self-heals from one-off language misdetections, the LLM
cleanup stage stops occasionally replying with a question instead of
the cleaned text, and every release tag is gated on a real Groq
equivalence check across five languages.

### Added

- Cloud equivalence gate at release time: a new `cloud-equivalence`
  job in `.github/workflows/release.yml` calls Groq's
  `whisper-large-v3-turbo` against the existing multilingual fixture
  set (en × 4, ro × 3, es × 1, fr × 1, zh × 1; ~110 audio-seconds
  total) and diffs the per-fixture verdicts against a committed
  baseline at `docs/bench/baseline-cloud-groq.json`. Blocks artefact
  production on failure. Auto-skipped when `GROQ_API_KEY` is unset
  (forks, bootstrap tags) or the tag carries the `-no-cloud-gate`
  suffix (operator escape hatch). Cost per release: < 0.5 % of
  Groq's free-tier daily cap. See ADR
  [`0021-cloud-equivalence-via-real-api.md`](docs/decisions/0021-cloud-equivalence-via-real-api.md)
  and `docs/dev/release-checklist.md`.
- `fono-bench equivalence --stt groq` accepts cloud Groq as an STT
  backend. Reads `GROQ_API_KEY` from env; default model
  `whisper-large-v3-turbo`, overridable via `--model`. New
  `--rate-limit-ms <ms>` flag (default 250 ms for `--stt groq`, 0
  otherwise) paces requests under Groq's 30-req/min ceiling. HTTP
  429 is a hard fail with code 3 and an explanatory message; never
  retried.
- New `docs/dev/release-checklist.md` documenting the bootstrap
  command for the cloud-equivalence baseline, the regenerate
  conditions, and the `-no-cloud-gate` override.

### Fixed

- LLM cleanup occasionally returned a clarification reply
  (“It seems like you're describing a situation, but the details are
  incomplete. Could you provide the full text you're referring to…”)
  instead of the cleaned transcript. Reproducible across **every**
  cleanup backend — Cerebras, Groq, OpenAI, OpenRouter, Ollama,
  Anthropic, and the local llama.cpp path — because the failure mode
  is a property of how chat-trained LLMs interpret a bare short
  utterance, not of any single provider. The fix is correspondingly
  universal: the default cleanup prompt was rewritten with hard
  “never ask for clarification” rules; every backend now wraps the
  user message in unambiguous `<<<` / `>>>` delimiters so the
  transcript cannot be mistaken for a chat message; and a refusal
  detector rejects clarification-shaped replies and falls back to the
  raw STT text. Applied identically to `OpenAiCompat`, `AnthropicLlm`,
  and `LlamaLocal`. See
  `plans/2026-04-28-llm-cleanup-clarification-refusal-fix-v1.md`.

### Changed

- `[llm].skip_if_words_lt` default raised from `0` to `3`. One- and
  two-word captures (“yes”, “okay”, “send it”) now bypass the LLM
  cleanup roundtrip entirely — regardless of whether the configured
  backend is cloud or local — saving 150–800 ms and avoiding the
  short-utterance clarification failure mode at the source. Override
  in `config.toml` if you want every utterance cleaned.

- `[stt.cloud].cloud_rerun_on_language_mismatch` default flipped from
  `false` to `true`. Combined with the new in-memory language cache,
  cloud STT now self-heals from one-off language misdetections (e.g.
  Groq Turbo flagging accented English as Russian) at the cost of one
  extra round-trip per misfire. Set `false` to opt out.

### Added

- In-memory per-backend language cache
  (`crates/fono-stt/src/lang_cache.rs`). Records the most recently
  correctly-detected language code per cloud STT backend; consulted
  **only as a rerun target** when post-validation fires. No file I/O,
  no persistence — daemon restarts rebuild within one or two
  utterances. OS locale (`LANG` / `LC_ALL`) seeds the cache at start
  if and only if its alpha-2 code is in `general.languages`.
- New `crates/fono-core/src/locale.rs` — POSIX-locale → BCP-47 alpha-2
  parser; used by both the cache bootstrap and the wizard.
- Tray **Languages** submenu (Linux): read-only checkbox display of
  the configured peer set plus a "Clear language memory" item that
  drops every entry from the in-memory cache.
- New ADR
  [`docs/decisions/0017-cloud-stt-language-stickiness.md`](docs/decisions/0017-cloud-stt-language-stickiness.md)
  documenting why the cache is rerun-only, in-memory only, and
  peer-symmetric (no primary/secondary).

### Deprecated

- `[stt.cloud].cloud_force_primary_language` — superseded by the
  in-memory language cache. Field still parses for one release; will
  be removed in v0.5.
- `LanguageSelection::primary()` — renamed to `fallback_hint()`. The
  alias is retained as `#[deprecated]` for one release; usage is
  scope-restricted in its doc-comment to single-language transports.

See `plans/2026-04-28-multi-language-stt-no-primary-v3.md`.

## [0.2.2] — 2026-04-28

First release in which the streaming live-dictation pipeline is
actually reachable from the shipped binary, plus supply-chain
hardening for `fono update`, a typed accuracy-gate API for
`fono-bench`, and the doc-reconciliation pass that closed out the
half-shipped plans inherited from v0.2.1.

### Changed — `interactive` is now a default release feature

- `crates/fono/Cargo.toml` flips `interactive` into the default
  feature set. **Before v0.2.2 the released binary contained none of
  the Slice A streaming code** — `record --live`, the live overlay,
  `test-overlay`, and the `[interactive].enabled` config knob were
  all `#[cfg(feature = "interactive")]`-gated and the release
  workflow built without that feature. Existing v0.2.1 users will
  see the live mode work for the first time after upgrading.
- Slim cloud-only builds remain available via
  `cargo build --no-default-features --features tray,cloud-all`.

### Added — self-update supply-chain hardening

- `apply_update` now verifies each downloaded asset against a
  per-asset `<asset>.sha256` sidecar published alongside the
  aggregate `SHA256SUMS` file. Mismatches fail closed (no rename,
  original binary untouched). Legacy releases without sidecars fall
  back to TLS-only trust with a `warn!` log.
- `parse_sha256_sidecar` accepts bare-digest, text-mode
  (`<hex>  <name>`), binary-mode (`<hex> *<name>`), and multi-entry
  sidecars; rejects too-short or non-hex inputs.
- New `--bin-dir <path>` flag on `fono update` overrides the install
  directory (matches the install-script `BIN_DIR` semantics). Useful
  when running with elevated privileges or when `current_exe()`
  resolves to a non-writable path. Still refuses to overwrite
  package-managed paths (`/usr/bin`, `/bin`, `/usr/sbin`).
- `.github/workflows/release.yml` now emits a `<asset>.sha256` file
  per artefact alongside the aggregate `SHA256SUMS`.

### Added — `fono-bench` typed capability surface

- New `crates/fono-bench/src/capabilities.rs` with
  `ModelCapabilities::for_local_whisper(model_stem)` and
  `for_cloud(provider, model)` resolvers. Replaces the inline
  `english_only` boolean previously sprinkled through `fono-bench`'s
  CLI.
- `ManifestFixture` schema split into `equivalence_threshold` and
  `accuracy_threshold` (with a `serde(alias = "levenshtein_threshold")`
  for back-compat). The two gates can now be tightened
  independently. `requires_multilingual: Option<bool>` lets fixtures
  override the derived `language != "en"` default.
- `EquivalenceReport` carries a populated `model_capabilities` block
  on every run; skipped rows now carry a typed `SkipReason`
  (`Capability` / `Quick` / `NoStreaming` / `RuntimeError`) instead
  of stringly-typed note fingerprints.
- New mock-STT capability-skip integration test asserts
  `transcribe` is never invoked on English-only models against
  non-English fixtures.

### Added — real-fixture CI bench gate

- `.github/workflows/ci.yml` replaces the prior `cargo bench --no-run`
  compile-only sanity step with a real-fixture equivalence run on
  every PR. The workflow fetches the whisper `tiny.en` GGML weights
  (cached via `actions/cache@v4` keyed on the model SHA), runs
  `fono-bench equivalence --stt local --model tiny.en --baseline
  --no-legend`, and diffs per-fixture verdicts against
  `docs/bench/baseline-comfortable-tiny-en.json`. Verdict divergence
  fails the build.
- New `--baseline` flag on `fono-bench equivalence` strips the
  non-deterministic timing fields (`elapsed_ms`, `ttff_ms`,
  `duration_s`) so the committed JSON is stable across runners.
- `tests/check.sh` mirrors the CI build/clippy/test matrix locally
  (full / `--quick` / `--slim` / `--no-test`) so contributors can
  run the same gate before pushing.

### Documentation

- Three obsolete plans superseded by the
  `--allow-multiple-definition` link trick (already live in
  `.cargo/config.toml`) moved to `plans/closed/` with `Status:
  Superseded` headers: `2026-04-27-candle-backend-benchmark-v1`,
  `2026-04-27-llama-dynamic-link-sota-v1`,
  `2026-04-27-shared-ggml-static-binary-v1`.
- `docs/decisions/` backfilled to numbers `0001`–`0019`. Recovered
  ADRs for `0005`–`0008` and `0010`–`0014` carry explicit
  `Status: Reconstructed` headers; new `0017` (auto-translation
  forward-reference), `0018` (`--allow-multiple-definition` link
  trick), `0019` (Linux-multi-package platform scope).
- `docs/dev/update-qa.md` lists the ten manual verification scenarios
  for self-update changes (bare binary, `/usr/local/bin`,
  distro-packaged, offline, rate-limited, mismatched sidecar,
  prerelease, `--bin-dir`, rollback).
- `docs/bench/README.md` documents how to regenerate the committed
  baseline anchor and how the CI gate interprets it.
- `docs/plans/2026-04-25-fono-roadmap-v2.md` Tier-1 R5.1 + R5.2
  ticked as fully shipped.

### Fixed — clippy violations exposed by `interactive` default

- `crates/fono-stt/src/whisper_local.rs:336` redundant clone removed
  on `effective_selection`'s already-owned return.
- `crates/fono-stt/src/whisper_local.rs:464-471` two `match` blocks
  rewritten as `let...else` per the `manual_let_else` lint.
- `crates/fono-audio/src/stream.rs:209-230` three `vec!` calls in
  test code replaced with array literals.

## [0.2.1] — 2026-04-28

Streaming/interactive dictation lands as a first-class mode, the
overlay stops stealing focus, and Whisper finally listens to a
language allow-list instead of free-styling into the wrong tongue.

### Added — interactive (streaming) dictation

- Slice A foundation: streaming STT, latency budget, overlay live
  text, and the equivalence harness (`fono-bench`) that gates
  stream↔batch consistency per fixture.
- v7 boundary heuristics — prosody, punctuation, filler-word and
  dangling-word handling — so partial commits feel natural rather
  than mid-phrase.
- `[interactive].enabled` is now wired end-to-end through the
  `StreamingStt` factory; flipping it on actually engages the
  streaming path.
- Equivalence harness gains a real accuracy gate (batch transcript vs
  manifest reference) on top of the stream↔batch gate, plus ten
  multilingual fixtures (EN/ES/FR/ZH/RO) and a `tests/bench.sh`
  runner.

### Added — STT language allow-list

- New `[general].languages: Vec<String>` (and `[stt.local].languages`
  override) replaces the single-language `language` scalar with a
  proper allow-list. Empty = unconstrained Whisper auto-detect; one
  entry = forced; two-or-more = constrained auto-detect (Whisper picks
  from the allow-list and **bans** every other language). The legacy
  `language` scalar still parses and is migrated automatically.
- `crates/fono-stt/src/lang.rs` exposes a `LanguageSelection` enum
  threaded through `SpeechToText` / `StreamingStt` so backends never
  compare sentinel strings.
- Local Whisper backend (`crates/fono-stt/src/whisper_local.rs`)
  runs `WhisperState::lang_detect` on the prefix mel, masks
  probabilities to allow-list members, then runs `full()` with the
  picked code locked. Forced and Auto paths keep the previous one-pass
  cost.
- Cloud STT (`groq.rs`, `openai.rs`) honours the allow-list
  best-effort via two opt-in `[general]` knobs:
  `cloud_force_primary_language` and
  `cloud_rerun_on_language_mismatch`.
- Wizard now persists the language prompt into `general.languages`
  (previously discarded).

### Fixed — overlay

- Real text rendering, lifecycle and visual overhaul; live-mode UX
  fixes (`1f23194`).
- Eliminated focus theft on X11 by setting override-redirect on the
  overlay window — tooltips/dmenu/rofi-style. The overlay no longer
  intercepts the synthesized `Shift+Insert` paste on its second map
  (`f94250e`).

## [0.2.0] — 2026-04-27

Single-binary local stack: STT (`whisper.cpp`) and LLM cleanup
(`llama.cpp`) now ship together in one statically-linked `fono` binary,
out of the box, with hardware-accelerated CPU SIMD selected at runtime.

### Added — single-binary local STT + LLM

- `llama-local` is now part of the `default` features set. The previous
  `compile_error!` guard in `crates/fono/src/lib.rs` is gone — both
  `whisper-rs` and `llama-cpp-2` link into the same ELF.
- `.cargo/config.toml` adds `-Wl,--allow-multiple-definition` to
  deduplicate the otherwise-colliding `ggml` symbols vendored by both sys
  crates. Both copies originate from the same `ggerganov` upstream and
  are ABI-compatible; the linker keeps one set, no UB at runtime.
- New `accel-cuda` / `accel-metal` / `accel-vulkan` / `accel-rocm` /
  `accel-coreml` / `accel-openblas` features on `crates/fono` that
  forward to matching `whisper-rs` / `llama-cpp-2` features for opt-in
  GPU acceleration.
- Startup banner prints a new `hw accel : <accelerators> + CPU <SIMD>`
  line (runtime SIMD probe: AVX512 / AVX2 / AVX / SSE4.2 + FMA + F16C on
  x86; NEON + DotProd + FP16 on aarch64).
- `LlamaLocal::run_inference` redirects llama.cpp / ggml's internal
  `printf`-style logging through `tracing` (matches the existing
  `whisper_rs::install_logging_hooks` pattern). Default verbosity now
  emits a single `LLM ready: <model> (<MB>, <threads> threads, ctx=<n>)
  in <ms>` line; cosmetic load-time warnings (control-token type,
  `n_ctx_seq < n_ctx_train`) are silenced. Re-enable on demand with
  `FONO_LOG=llama-cpp-2=info`.
- New smoke test `crates/fono/tests/local_backends_coexist.rs` boots
  `WhisperLocal` and `LlamaLocal` in the same process to lock in the
  no-collision contract.

### Added — wizard local LLM path

- First-run wizard now offers `Local LLM cleanup (qwen2.5, private,
  offline)` as a top-level option in both the Local and Mixed paths, in
  addition to `Skip` and `Cloud`. New `configure_local_llm` helper picks
  a tier-aware model: `qwen2.5-3b-instruct` (HighEnd),
  `qwen2.5-1.5b-instruct` (Recommended/Comfortable),
  `qwen2.5-0.5b-instruct` (Minimum/Unsuitable). All Apache-2.0 per
  ADR 0004.
- The wizard's auto-download now fires for either local STT *or* local
  LLM (was STT-only).

### Added — tray UX

- Tray STT and LLM submenus now show a `●` marker beside the active
  backend (was missing — `active_backends()` returned the trait `name()`
  while the comparison logic expected the canonical config-string
  identifier).
- Switching to the local STT or LLM backend from the tray now ensures
  the corresponding model file is on disk first, with a "downloading…"
  notification, a "ready" notification on completion, and a clear error
  notification on failure (with the orchestrator reload skipped to keep
  the user on a working backend).

### Changed — hotkey defaults

- `toggle = "F9"` (was `Ctrl+Alt+Space`). Single key, no default
  binding on any major desktop, easy to fire blind.
- `hold = "F8"` (was `Ctrl+Alt+Grave`). Adjacent to F9 for natural
  push-to-talk muscle memory.
- `cancel = "Escape"` unchanged (only grabbed while recording).
- `paste_last` hotkey **removed**. The tray's "Recent transcriptions"
  submenu and the `fono paste-last` CLI cover the same need with a
  better UX (re-paste any of the last 10, not just the newest).
  `Request::PasteLast` IPC and `Cmd::PasteLast` CLI are preserved and
  now route directly to `orch.on_paste_last()`.

### Changed — release profile size

- `[profile.release]` now sets `strip = "symbols"` and `lto = "thin"`,
  trimming the dev `cargo build --release` artifact from ~23 MB → ~19 MB
  (no code removal — only `.symtab` / `.strtab` deduplication).
  `release-slim` (used by packaging CI) is unchanged at ~15 MB.

### Documented

- `docs/status.md` — new entries for hotkey ergonomics and the
  single-binary local-stack resolution.
- `docs/troubleshooting.md`, `docs/wayland.md`, `README.md` updated for
  the new default hotkeys.
- New plans: `plans/closed/2026-04-27-shared-ggml-static-binary-v1.md` (the
  shared-ggml strategy that informed the linker-dedupe shortcut; later
  superseded by `--allow-multiple-definition`),
  `plans/closed/2026-04-27-llama-dynamic-link-sota-v1.md`,
  `plans/closed/2026-04-27-candle-backend-benchmark-v1.md`,
  `plans/2026-04-27-local-stt-llm-resolution-v1.md`.

## [0.1.0] — 2026-04-25

First public release. Pipeline (audio → STT → LLM → inject) is fully wired
end-to-end; default release ships local whisper.cpp out of the box.

### Added — pipeline

- `SessionOrchestrator` (`crates/fono/src/session.rs`) glues hotkey FSM →
  cpal capture → silence trim → STT → optional LLM cleanup → text injection
  → SQLite history. Hot-swappable backends behind `RwLock<Arc<dyn …>>`.
- `fono record` — one-shot CLI dictation (microphone → stdout / inject).
- `fono transcribe <wav>` — runs a WAV file through the same pipeline; useful
  for verifying API keys without a microphone.

### Added — providers

- **STT**: local whisper.cpp (small / base / medium models), Groq cloud
  (`whisper-large-v3-turbo`), OpenAI cloud, optional Deepgram / AssemblyAI /
  Cartesia stubs.
- **LLM cleanup**: optional, off-by-default. OpenAI-compatible endpoints
  (Cerebras, Groq, OpenAI, OpenRouter, Ollama) and Anthropic.
- `STT` and `TextFormatter` traits with `prewarm()` so the first dictation
  after daemon start is not cold (latency plan L2/L3).
- `fono use {stt,llm,cloud,local,show}` — one-command provider switching;
  rewrites config atomically and hot-reloads the orchestrator (no restart).
- `fono keys {list,add,remove,check}` — multi-provider API-key vault with
  reachability probes.
- Per-call overrides: `fono record --stt openai --llm anthropic`.

### Added — hardware-adaptive setup

- `crates/fono-core/src/hwcheck.rs` — pure-Rust probe of physical/logical
  cores, RAM, free disk, and CPU features (AVX2/NEON/FMA). Maps to a
  five-level `LocalTier` (`Unsuitable`, `Minimum`, `Comfortable`,
  `Recommended`, `High-end`).
- Wizard prints the live tier and steers the user toward local vs cloud
  based on what the machine can sustain.
- `fono hwprobe [--json]` exposes the snapshot for scripts.
- `fono doctor` shows the active hardware tier alongside provider
  reachability and the chosen injector.

### Added — input / output

- Default key-injection backend `Injector::XtestPaste` — pure-Rust X11 XTEST
  paste via `x11rb` + `xsel`/`wl-copy`/`xclip` clipboard write. No system
  dependencies beyond a clipboard tool. **Shift+Insert** is the default paste
  shortcut (universal X11 binding).
- Override paste shortcut via `[inject].paste_shortcut = "ctrl-v"` in config
  or `FONO_PASTE_SHORTCUT=ctrl-shift-v` env var.
- Always-clipboard safety net: every successful dictation also writes to both
  CLIPBOARD and PRIMARY selections (`general.also_copy_to_clipboard = true`).
- Always-notify: `notify-rust` toast on every dictation
  (`general.notify_on_dictation = true`).
- `fono test-inject "<text>" [--shortcut <variant>]` — smoke-tests injection
  and clipboard delivery without speaking.

### Added — tray

- `Recent transcriptions ▸` submenu with the last 10 dictations; click to
  re-paste.
- `STT: <active> ▸` and `LLM: <active> ▸` submenus for live provider
  switching from the tray (same code path as `fono use`).
- Open history folder (was misrouted to Dolphin in pre-release; now opens
  the directory itself via `xdg-open`).

### Added — safety + observability

- Per-stage tracing breadcrumbs at `info`: `capture=…ms trim=…ms stt=…ms
  llm=…ms inject=…ms (raw_chars → cleaned_chars)`.
- Pipeline in-flight guard refuses concurrent recordings with a toast.
- Skip-LLM-when-short heuristic (configurable `llm.skip_if_words_lt`) saves
  150–800 ms per short dictation.
- Trim leading/trailing silence pre-STT (`audio.trim_silence`); ~30 % faster
  STT on 5 s utterances with 1.5 s of tail silence.

### Added — benchmark harness

- New `crates/fono-bench/` crate: 6-language LibriVox fixture set (en, es,
  fr, de, it, ro), Word Error Rate + per-stage latency report, criterion
  benchmark, regression gate. CI-fast (network-free) and full-stack modes.

### Documented

- `docs/plans/2026-04-25-fono-pipeline-wiring-v1.md` (W1–W22, all landed).
- `docs/plans/2026-04-25-fono-latency-v1.md` (L1–L30, 17 landed, 13
  deferred-to-v0.2 with rationale).
- `docs/plans/2026-04-25-fono-local-default-v1.md` (H1–H25).
- `docs/plans/2026-04-25-fono-provider-switching-v1.md` (S1–S27).
- `docs/plans/2026-04-25-fono-roadmap-v2.md` (post-v0.1 roadmap).
- ADR `docs/decisions/0007-local-models-build.md` — glibc-linked default
  release vs musl-slim cloud-only artifact.

### Models locked in v0.1.0

| Provider | Model | License | First-run download |
|---|---|---|---|
| Whisper local | `ggml-small.bin` (multilingual) | MIT | ~466 MB |
| Whisper local (light) | `ggml-base.bin` | MIT | ~142 MB |
| Groq cloud STT | `whisper-large-v3-turbo` | (cloud, no license) | n/a |
| OpenAI cloud STT | `whisper-1` | (cloud) | n/a |
| Cerebras cloud LLM | `llama-3.3-70b` | (cloud) | n/a |
| Groq cloud LLM | `llama-3.3-70b-versatile` | (cloud) | n/a |

Local LLM (Qwen2.5 / SmolLM2) is opt-in behind the `llama-local` Cargo
feature and ships fully wired in v0.2.

### Verification

- 86 unit + integration tests; 2 latency-smoke `#[ignore]` tests.
- `cargo clippy --workspace --no-deps -- -D warnings` clean (pedantic +
  nursery).
- DCO sign-off enforced on every commit.

### Known limitations

- No streaming STT/LLM yet (latency plan L6/L7/L8 deferred to v0.2). Latency
  on cloud Groq+Cerebras is ~1 s end-to-end on a 5 s utterance.
- Wayland global hotkey requires compositor binding to `fono toggle`
  (`org.freedesktop.portal.GlobalShortcuts` not yet stable in upstream
  compositors).
- Local LLM cleanup (Qwen / SmolLM) is opt-in / preview.
- Real `winit + softbuffer` overlay window is a stub (event channel only).

[Unreleased]: https://github.com/bogdanr/fono/compare/v0.3.7...HEAD
[0.3.7]: https://github.com/bogdanr/fono/compare/v0.3.6...v0.3.7
[0.3.6]: https://github.com/bogdanr/fono/compare/v0.3.5...v0.3.6
[0.3.5]: https://github.com/bogdanr/fono/compare/v0.3.4...v0.3.5
[0.3.4]: https://github.com/bogdanr/fono/releases/tag/v0.3.4
[0.3.3]: https://github.com/bogdanr/fono/releases/tag/v0.3.3
[0.3.2]: https://github.com/bogdanr/fono/releases/tag/v0.3.2
[0.3.1]: https://github.com/bogdanr/fono/releases/tag/v0.3.1
[0.3.0]: https://github.com/bogdanr/fono/releases/tag/v0.3.0
[0.2.2]: https://github.com/bogdanr/fono/releases/tag/v0.2.2
[0.2.1]: https://github.com/bogdanr/fono/releases/tag/v0.2.1
[0.2.0]: https://github.com/bogdanr/fono/releases/tag/v0.2.0
[0.1.0]: https://github.com/bogdanr/fono/releases/tag/v0.1.0
