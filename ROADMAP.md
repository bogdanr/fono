# Fono Roadmap

> One binary. Any desktop. Your voice, at the cursor.

Fono is an open-source (GPL-3.0) voice dictation tool for Linux — native, lightweight,
and privacy-first. No Electron. No Python. No WebKit. Press a hotkey, speak, and your
words land at the cursor in any app, on any desktop, X11 or Wayland.

For exact per-release details see [`CHANGELOG.md`](CHANGELOG.md).
The home page is [fono.page](https://fono.page).

---

| ![Up next](https://img.shields.io/badge/Up_next-2ea44f?style=for-the-badge) | ![On the horizon](https://img.shields.io/badge/On_the_horizon-0075ca?style=for-the-badge) | ![Recently shipped](https://img.shields.io/badge/Recently_shipped-6e7681?style=for-the-badge) |
|:---|:---|:---|
| **[Network inference + Wyoming + autodiscovery](#network-inference)**<br>Speak Wyoming to interop with Home Assistant / faster-whisper. WebSocket-based Fono protocol so a browser can be a client too. mDNS auto-populates LAN servers in the tray menu — zero config. | **[Wake-word activation](#wake-word-activation)**<br>Say the magic word — Fono wakes and starts dictating. No hotkey, no hands. | **Wyoming + mDNS foundations**<br>Wyoming STT client/server, LAN discovery, pure-Rust SNI tray, and size-budget prep. ![v0.3.7](https://img.shields.io/badge/v0.3.7-blue?style=flat-square) |
| **[Automatic translation](#automatic-translation)**<br>Speak in any language, type in another — any pair, per-app rules, batch and live parity. | **[Hover-context injection](#hover-context-injection)** *(experimental)*<br>Terminal hovered → shell prompts. Code editor hovered → identifier casing. | **Silent-dock recovery + PulseAudio mic**<br>3-second empty-transcript toast; tray Microphone submenu via pactl; config purge. ![v0.3.6](https://img.shields.io/badge/v0.3.6-blue?style=flat-square) |
| **[Auto-update polish](#polish-the-auto-update)**<br>Finishing touches on `fono update`. | **[REST API + MCP server](#local-rest-api--mcp-server)**<br>Scripts and AI coding assistants drive Fono over HTTP. | **Streaming cadence controls**<br>Fine-tune live preview rate; 429-aware backoff. ![v0.3.3](https://img.shields.io/badge/v0.3.3-blue?style=flat-square) |
| | **[Better Wayland hotkeys](#better-wayland-hotkeys)**<br>Auto-register via the `GlobalShortcuts` portal when available. | |
| | **[macOS + Windows](#macos-and-windows)**<br>Native platform integrations. | |
| | **[Audio visualisation overlay](#audio-visualisation-overlay)**<br>Waveform bars, oscilloscope, or breathing pulse while you dictate. Signal-level VU bar during live dictation. GUI builds only. | |

---

## Up next

### Network inference

> Your old laptop, your tablet, your phone, your browser tab — all get first-class
> dictation because your powerful machine does the thinking for all of them.

Run the Fono server on your desktop; every other machine on your LAN automatically
sees it in the tray menu and uses it with one click. No host:port to type, no config
file to edit. The thin client streams audio over the LAN; the server runs Whisper and
the LLM cleanup. The result lands at the cursor on the client using near-zero CPU and
RAM — even on a ten-year-old laptop. Every byte stays on your private network; nothing
touches the cloud unless you explicitly configure a cloud provider on the server.

Three protocols, one experience:

- **[Wyoming](https://github.com/OHF-Voice/wyoming)** — the open standard for voice
  services. Fono speaks it as both client and server, so any existing
  faster-whisper / whisper.cpp / Piper / openWakeWord container drops in as a Fono
  backend, and conversely any Home Assistant satellite, Rhasspy, or Wyoming-compatible
  consumer can drive Fono's local Whisper.
- **Fono-native over WebSocket** — covers the parts Wyoming has no event types for
  (LLM cleanup, history mirror, app-context routing for hover-context rules). Built on
  WebSocket so a future browser-based Fono client is a small JavaScript bundle, not a
  protocol redesign.
- **mDNS / DNS-SD autodiscovery** — both protocols announce themselves on the LAN.
  Discovered servers appear automatically in the tray STT and LLM submenus alongside
  Local and Cloud. Click a row → Fono switches to that server. Restart the daemon →
  it rediscovers everything fresh; no discovery toggle required.

Full design: [`plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`](plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md).

### Automatic translation

> Speak in Romanian, type in English. Or any other pair. Without leaving your editor.

Fono will translate as it transcribes — the pipeline becomes
**STT → translate → cleanup → inject**, entirely in the background:

- **Any source/target language pair**, not English-only. When the target is English,
  Whisper's native translation mode and the Groq/OpenAI `/audio/translations` endpoint
  provide a zero-latency fast path.
- **Per-app rules.** A `[[context_rules]]` override lets you target a different language
  per application — translate to English in your code editor, keep the original in your
  chat app.
- **Batch and live parity.** Works the same whether you use push-to-talk or streaming
  live-dictation mode.
- **One-shot CLI.** `fono translate <text> --to <code>` pipes any text through the
  configured translator without touching audio capture.

### Polish the auto-update

`fono update` is already there. A few finishing touches remain to handle edge cases
gracefully.

---

## On the horizon

### Wake-word activation

> Just say the word.

Always-on hands-free mode: Fono idles with a tiny wake-word detector (powered by
[openWakeWord](https://github.com/dscripka/openWakeWord)) using a fraction of one CPU
core. Say the magic word and Fono wakes up and starts dictating — no hotkey, no
reaching for the keyboard. When you stop speaking it goes back to sleep. The wake-word
model runs locally; your audio never leaves the machine while idle.

### Hover-context injection

*(experimental)* Fono will peek at what the cursor is hovering over and silently adjust
the cleanup prompt before injecting. Hovering over a terminal? The LLM is told to
format output as shell commands. Hovering over a code editor? It prefers identifier-style
casing and avoids prose punctuation. This is exploratory — an experiment to see how
much smarter dictation can get just from a window-class and cursor-position hint, with
no extra effort from the user.

### Local REST API + MCP server

Fono already runs as a daemon with a Unix-socket IPC layer — every CLI subcommand
(`fono toggle`, `fono history`, `fono use …`) is a client talking to it. The next step
is exposing that same interface over HTTP and the
[Model Context Protocol](https://modelcontextprotocol.io), so scripts, editor plugins,
and AI coding assistants can drive Fono without any special tooling.

### Better Wayland hotkeys

Today on Wayland (KDE, GNOME, wlroots) you bind the hotkey through your compositor's
own settings. Once the `org.freedesktop.portal.GlobalShortcuts` portal becomes
universally available, Fono will register its hotkeys through it automatically — zero
setup.

### macOS and Windows

Native integrations for both platforms: menu-bar app and signed `.dmg` on macOS;
system-tray app and native installer on Windows.

### Audio visualisation overlay

> See your voice, not just the tray icon.

*GUI builds only — not available in server or headless deployments.*

Two companion features for the overlay panel, both opt-in:

- **Standalone waveform overlay** (when live-dictation mode is off). The bottom-centre
  panel appears as soon as you press the hotkey and shows a real-time audio visualisation
  until the transcription finishes. Three styles, user-selectable via `[overlay].style`:
  - **`bars`** — a scrolling bar chart; bars glow brighter at higher amplitude.
  - **`oscilloscope`** — a connected-line waveform drawn from raw PCM samples at ~60 fps.
  - **`pulse`** — a single breathing circle whose radius and glow track your voice level.
- **Interactive signal bar** (when live-dictation mode is on). A narrow vertical VU bar on
  the right edge of the live-dictation panel shows real-time microphone signal level at a
  glance — so you can see whether your voice is too quiet without interrupting the
  transcript. Enabled by default (`[overlay].volume_bar = true`), opt-out.

Full implementation plan: `plans/2026-04-29-waveform-overlay-v2.md`.

---

## Shipped

Newest first.

- ![v0.5.0](https://img.shields.io/badge/v0.5.0-2026--05--02-blue?style=flat-square)
  **Hardware acceleration on tap + self-installer + auto-variant
  update.** Releases now ship two binaries: the default
  `fono-vX.Y.Z-x86_64` (compact ~18 MB CPU-only build) and
  `fono-gpu-vX.Y.Z-x86_64` (Vulkan-enabled ~60 MB build with
  cross-vendor GPU acceleration on NVIDIA / AMD / Intel). `fono
  update` probes Vulkan on the host and auto-picks the matching
  asset every time — no flag, no prompt: a CPU build on a
  Vulkan-capable machine is switched to the GPU build on its next
  update; if that machine later loses its GPU it switches back. The
  tray surfaces a single discoverable "Update for GPU acceleration"
  entry on a CPU build with a usable Vulkan host. `fono doctor`
  reports the running variant and the live Vulkan device list.
  Separately: `sudo fono install` self-installs the running binary
  system-wide on a desktop (or `--server` for a hardened systemd
  unit); `sudo fono uninstall` reverses it cleanly. CUDA / ROCm
  remain available via build-from-source for the last 10–20 % of
  vendor-specific perf.

- ![v0.4.0](https://img.shields.io/badge/v0.4.0-2026--05--02-blue?style=flat-square)
  **Wyoming Home Assistant interop + tray-side LAN server picker.** Fono's
  Wyoming framing now matches the upstream Python library exactly (separate
  data block, version header, `info.asr` array shape with placeholder arrays
  for tts/handle/intent/wake/mic/snd/satellite), so Home Assistant treats Fono
  as a complete Wyoming endpoint. The server queues `transcribe` arriving
  before `audio-stop` (HA client behavior) and decodes variable bit-width /
  multi-channel `audio-chunk` headers. The tray gains a "Discovered Wyoming
  servers" submenu — clicking a peer hot-reloads the daemon's STT config to
  point at that remote. mDNS A/AAAA records now follow network topology
  changes via `enable_addr_auto`. The CI size-budget gate moved from the
  blocked static-musl target to a glibc-dynamic + NEEDED-allowlist check
  against the actual ship binary (~18 MB measured); artefact-producing
  runners pin to ubuntu-22.04 (glibc 2.35) so the binary runs on Ubuntu
  22.04+, Debian 12+, Fedora 36+. The Phase 2.4 static-musl ship is
  formally deferred (see ADR 0022 amendment).

- ![v0.3.7](https://img.shields.io/badge/v0.3.7-2026--04--30-blue?style=flat-square)
  **Wyoming + mDNS network foundations and binary-size prep.** Fono can now use
  Wyoming-compatible STT servers on the LAN and host its own Wyoming listener when
  enabled. mDNS/DNS-SD discovery tracks Wyoming and Fono peers in-memory and exposes
  them through IPC and `fono discover`. The tray backend moved to pure-Rust SNI via
  `ksni`, default Linux audio no longer pulls ALSA into the main build, and the
  release checks now include size/dependency guardrails for the canonical
  glibc-dynamic ship binary (gated by a NEEDED allowlist; see ADR 0022).

- ![v0.3.6](https://img.shields.io/badge/v0.3.6-2026--04--29-blue?style=flat-square)
  **Silent-dock auto-recovery + PulseAudio-first microphone.** When a 3+ second
  recording produces no transcribed text (e.g. an external dock's passive capture
  endpoint is the OS default), Fono pops a critical notification naming the silent
  device and alternative candidates. Microphone enumeration is now PulseAudio-first
  on Linux: the tray "Microphone" submenu lists `pactl` sources with friendly names
  and clicking a row runs `pactl set-default-source` system-wide. Removed:
  `[audio].input_device` config field, `fono use input`, Languages tray submenu,
  and all deprecated config scalars.

- ![v0.3.5](https://img.shields.io/badge/v0.3.5-2026--04--29-blue?style=flat-square)
  **Smarter first-run setup.** The setup wizard now asks whether you dictate only in
  English or multiple languages, then recommends the best on-device speech model your
  hardware can comfortably run. Plain-language hardware summary; model shortlist capped
  at three choices; `large-v3-turbo` added, `medium` retired. Live-mode recommendation
  calibrated against CPU-only vs. hardware-accelerated thresholds.

- ![v0.3.3](https://img.shields.io/badge/v0.3.3-2026--04--28-blue?style=flat-square)
  **Configurable streaming cadence + 429 awareness.** Live preview cadence controlled by
  `interactive.streaming_interval` (default 1.0 s, range 0.5–3.0). Values above 3.0
  disable the preview lane — recommended for free-tier cloud users with strict
  per-minute caps. HTTP 429 now surfaces a log suggestion to raise the interval.

- ![v0.3.2](https://img.shields.io/badge/v0.3.2-2026--04--28-blue?style=flat-square)
  **Banned-language gate actually fires.** v0.3.1's wrong-language self-correction was
  correct in design but unreachable in practice: the cloud transcribe call wasn't
  requesting the language field, so the gate never noticed a mismatch. Fixed.

- ![v0.3.1](https://img.shields.io/badge/v0.3.1-2026--04--28-blue?style=flat-square)
  **Cold-start language self-correction.** When the cloud transcriber's first response of
  a session is in the wrong language (e.g. English audio flagged as Russian for an
  accented speaker), Fono retries against every language you've configured and picks the
  one Whisper was most confident about.

- ![v0.3.0](https://img.shields.io/badge/v0.3.0-2026--04--28-blue?style=flat-square)
  **Release-time cloud quality gate.** Every tag now runs the full multilingual fixture
  set (English, Romanian, Spanish, French, Chinese) through Groq's cloud Whisper and
  refuses to publish if any fixture diverges from the committed baseline. Catches our
  regressions and upstream provider changes within minutes of tagging.

- ![v0.3.0](https://img.shields.io/badge/v0.3.0-2026--04--28-blue?style=flat-square)
  **Cloud transcription that learns your language.** If your cloud provider occasionally
  mishears your accent, Fono self-corrects after the first mistake. Bilingual users can
  switch languages freely without any toggle.

- ![v0.3.0](https://img.shields.io/badge/v0.3.0-2026--04--28-blue?style=flat-square)
  **Reliable AI cleanup.** Fixed a long-standing bug where the cleanup step would
  occasionally reply with a clarifying question instead of cleaning your dictation.
  Affects every cloud and local AI provider. Very short utterances now skip cleanup
  entirely, saving about half a second.

- ![v0.2.2](https://img.shields.io/badge/v0.2.2-2026--04--28-blue?style=flat-square)
  **Live dictation actually ships.** The streaming overlay was built but accidentally
  left out of the packaged binary. v0.2.2 turns it on by default.

- ![v0.2.2](https://img.shields.io/badge/v0.2.2-2026--04--28-blue?style=flat-square)
  **Tamper-proof self-update.** `fono update` now verifies every downloaded file against
  a published checksum, refuses to overwrite files installed by your system package
  manager, and accepts a custom install directory.

- ![v0.2.2](https://img.shields.io/badge/v0.2.2-2026--04--28-blue?style=flat-square)
  **Automated quality gate.** Every pull request runs a real speech-recognition test
  against committed audio samples, catching accuracy regressions before they ship.

- ![v0.2.1](https://img.shields.io/badge/v0.2.1-2026--04--28-blue?style=flat-square)
  **Streaming dictation mode.** First version of the live overlay — see your words
  appear as you speak, not only after you stop.

- ![v0.2.1](https://img.shields.io/badge/v0.2.1-2026--04--28-blue?style=flat-square)
  **Pick your dictation languages.** Replace the single-language setting with a list.
  Whisper constrains itself to the languages you actually speak.

- ![v0.2.1](https://img.shields.io/badge/v0.2.1-2026--04--28-blue?style=flat-square)
  **Overlay no longer steals keyboard focus** on X11 desktops.

- ![v0.2.0](https://img.shields.io/badge/v0.2.0-2026--04--27-blue?style=flat-square)
  **One binary, full local stack.** Whisper and a local LLM in the same executable, with
  optional GPU acceleration. No Python, no Node, no Electron.

- ![v0.2.0](https://img.shields.io/badge/v0.2.0-2026--04--27-blue?style=flat-square)
  **Local cleanup AI in the setup wizard.** First-run setup now offers an offline LLM
  sized automatically to your hardware.

- ![v0.2.0](https://img.shields.io/badge/v0.2.0-2026--04--27-blue?style=flat-square)
  **Friendlier hotkeys.** F9 to toggle, F8 for push-to-talk — single keys, no awkward
  chords, no clashes with desktop shortcuts.

- ![v0.1.0](https://img.shields.io/badge/v0.1.0-2026--04--25-blue?style=flat-square)
  **First public release.** Press a hotkey, speak, see your words at the cursor. Works
  with on-device Whisper out of the box, or with Groq / OpenAI / Anthropic / Cerebras /
  Deepgram. Tray icon, history, hot-swappable providers.

[v0.1.0]: https://github.com/bogdanr/fono/releases/tag/v0.1.0
[v0.2.0]: https://github.com/bogdanr/fono/releases/tag/v0.2.0
[v0.2.1]: https://github.com/bogdanr/fono/releases/tag/v0.2.1
[v0.2.2]: https://github.com/bogdanr/fono/releases/tag/v0.2.2
[v0.3.0]: https://github.com/bogdanr/fono/releases/tag/v0.3.0
[v0.3.1]: https://github.com/bogdanr/fono/releases/tag/v0.3.1
[v0.3.2]: https://github.com/bogdanr/fono/releases/tag/v0.3.2
[v0.3.3]: https://github.com/bogdanr/fono/releases/tag/v0.3.3
[v0.3.5]: https://github.com/bogdanr/fono/releases/tag/v0.3.5
[v0.3.6]: https://github.com/bogdanr/fono/releases/tag/v0.3.6
[v0.3.7]: https://github.com/bogdanr/fono/releases/tag/v0.3.7
