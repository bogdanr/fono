# Fono Roadmap

> One binary. Any desktop. Your voice, at the cursor.

Fono is an open-source (GPL-3.0) voice dictation tool for Linux — native, lightweight,
and privacy-first. No Electron. No Python. No WebKit. Press a hotkey, speak, and your
words land at the cursor in any app, on any desktop, X11 or Wayland.

For exact per-release details see [`CHANGELOG.md`](CHANGELOG.md).
The home page is [fono.page](https://fono.page).

---

<table width="100%">
<tr>
<td valign="top" width="50%"><img src="https://img.shields.io/badge/Up_next-2ea44f?style=for-the-badge" alt="Up next"><br><br><strong><a href="#automatic-translation">Automatic translation</a></strong><br>Speak in any language, type in another — any pair, per-app rules, batch and live parity.<br><br><strong><a href="#wake-word-activation">Wake-word activation</a></strong><br>Say the magic word — Fono wakes and starts dictating. No hotkey, no hands.<br><br><strong><a href="#local-text-to-speech--home-assistant-voice-server">Local text-to-speech + Home Assistant voice server</a></strong><br>Speak any of your languages locally — Kokoro where it shines, Piper everywhere else (including Romanian). The same engine answers Home Assistant over Wyoming, no Python sidecar.<br><br><strong><a href="#talk-over-the-assistant">Talk over the assistant</a></strong><br>Just start speaking — Fono hears you over its own voice and hands the turn back. No hotkey, no escape, no awkward "stop, stop, stop".</td>
<td valign="top" width="50%"><img src="https://img.shields.io/badge/On_the_horizon-0075ca?style=for-the-badge" alt="On the horizon"><br><br><strong><a href="#hover-context-injection">Hover-context injection</a></strong> <em>(experimental)</em><br>Terminal hovered → shell prompts. Code editor hovered → identifier casing.<br><br><strong><a href="#voice-loop-for-coding-agents">Voice loop for coding agents</a></strong><br>Talk to Forge, Claude Code, Cursor and friends entirely by voice. Short spoken answers, A/B/C choices, no keyboard between turns.<br><br><strong><a href="#voice-actions">Voice actions</a></strong><br>"Turn on the kitchen lights." Fono speaks to Home Assistant, GitHub, and your own MCP servers — the assistant doesn't just answer, it does.<br><br><strong><a href="#realtime-voice-assistant">Realtime voice assistant</a></strong><br>OpenAI Realtime and Gemini Live: F8 speaks straight to the model, single WebSocket, sub-second time-to-first-audio.<br><br><strong><a href="#better-wayland-hotkeys">Better Wayland hotkeys</a></strong><br>Auto-register via the <code>GlobalShortcuts</code> portal when available.<br><br><strong><a href="#macos-and-windows">macOS + Windows</a></strong><br>Native platform integrations.</td>
</tr>
</table>

![Recently shipped](https://img.shields.io/badge/Recently_shipped-6e7681?style=for-the-badge)

**[v0.8.2 — Esc-to-cancel + smarter first-run model picks](#shipped)**  
Wayland Esc cancel, sharper wizard recommendations on older iGPUs,
assistant memory that survives a dictation pivot, PipeWire capture fix,
native aarch64 binary. *(2026-05-25)*

**[v0.8.1 — Two more cloud providers](#shipped)**  
Deepgram + Cartesia STT, headless install, pause UI polish. *(2026-05-23)*

**[v0.8.0 — One-key cloud setup](#shipped)**  
Live preview as a waveform style; four new TTS backends. *(2026-05-17)*

[Full changelog ↓](#shipped)

---

## Up next

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

### Wake-word activation

> Just say the word.

Always-on hands-free mode: Fono idles with a tiny wake-word detector (powered by
[openWakeWord](https://github.com/dscripka/openWakeWord)) using a fraction of one CPU
core. Say the magic word and Fono wakes up and starts dictating — no hotkey, no
reaching for the keyboard. When you stop speaking it goes back to sleep. The wake-word
model runs locally; your audio never leaves the machine while idle.

### Local text-to-speech + Home Assistant voice server

> Hear your assistant in your own language, locally. Then let Home Assistant
> hear it too.

Fono will speak back without a cloud call and without a separate Python sidecar.
Two engines, one automatic router: **Kokoro** for the nine locales it speaks
natively (American / British English, Spanish, French, Hindi, Italian, Japanese,
Brazilian Portuguese, Mandarin) where its prosody is best in class, and **Piper**
for everything else — Romanian, Polish, German, Dutch, Russian, Turkish, and the
long tail of European, Slavic, and Asian languages. Voices and language data
download on first use, like the Whisper models do today; the binary stays
self-contained.

The same local engine becomes a **Wyoming-protocol TTS server**, autodiscovered
by Home Assistant over mDNS. One Fono daemon on the LAN replaces
`wyoming-piper` (and, for the languages Kokoro covers, dramatically improves on
it) as your house's voice. ASR and TTS can run side-by-side on the same
listener; a headless `sudo fono install --server` box becomes a complete
HA-voice endpoint with one config flag.

Local TTS ships as a third release variant (`fono-tts-vX.Y.Z-x86_64`, target
≤ 32 MiB) alongside the existing CPU and GPU builds. `fono update` picks the
right one automatically. Plan:
`plans/2026-05-25-local-tts-piper-kokoro-and-wyoming-server-v1.md`.

### Talk over the assistant

> Just start speaking. Fono will hear you over its own voice and hand the turn
> back to you.

The voice assistant already accepts a tap of F8 or Escape to barge in mid-reply,
but you have to reach for the keyboard. After this lands, the assistant listens
while it speaks: the moment you start a sentence aloud, it stops talking, drops
the rest of its planned reply, keeps the conversation history, and starts a new
turn on your follow-up — exactly as if you had pressed F8 at that instant.

The trick is acoustic echo cancellation, otherwise the assistant interrupts
itself the instant its own voice reaches the mic. Rather than grow the binary
with a built-in AEC, Fono asks PipeWire to do the work: a private echo-cancel
sink and source bound to the assistant's playback, never touching your default
audio devices, so Zoom calls and music in other apps are unaffected. On the
modern Linux distros where PipeWire is the default audio stack
(Fedora 34+, Arch / Manjaro, openSUSE Tumbleweed, Debian 12+, Ubuntu 22.10+),
this is enabled automatically with no setup. Where PipeWire's echo-cancel
module isn't installed, Fono silently keeps the manual F8 / Escape behaviour
and tells you in `fono doctor` how to enable it. macOS and Windows ship in a
later slice. Plan:
`plans/2026-05-25-double-talk-barge-in-pipewire-aec-v1.md`.

---

## On the horizon

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

The MCP half lands first as a focused voice-loop integration — see
[Voice loop for coding agents](#voice-loop-for-coding-agents) below. The REST half
follows as a thin shim over the same IPC surface.

### Voice actions

> Stop asking. Start doing.

The voice assistant today answers questions. The next step is letting it **do
things** — turn on the kitchen lights, start a Pomodoro, open a GitHub issue,
anything an MCP server can expose. You hold F8, say what you want, and Fono
either explains (as today) or acts. The assistant decides per turn, no special
keyword, no separate hotkey.

The connector is the [Model Context Protocol](https://modelcontextprotocol.io)
— Fono is the **client**, speaking to whichever MCP servers you configure. A
typical setup points at Home Assistant on your LAN for smart-home control;
power users add GitHub, calendar, file-search, or any of the growing MCP
ecosystem. Tools are advertised to the assistant LLM via its native
function-calling API (works on OpenAI, Anthropic, Groq, Cerebras, and Gemini —
local LLM tool-calling lands later).

Two in-process built-ins ship by default — `pomodoro_start` and
`pomodoro_cancel` — so the feature works out of the box without any external
server, and the tray shows the active timer. A confirmation-policy hook is
wired in from day one so dangerous actions ("delete every file in Downloads")
can later require a spoken "yes" or a hotkey tap before they fire. v1 ships
with confirmation off by default; the UX layers on later without schema churn.

Concrete plan: `plans/2026-05-22-voice-actions-via-mcp-v1.md`. Strict
prerequisite for the [Realtime voice assistant](#realtime-voice-assistant) so
realtime and staged paths gain voice actions in lockstep.

### Realtime voice assistant

> Skip the relay race. Talk straight to the model.

Today the F8 assistant runs a relay: speech-to-text, then a chat LLM, then
text-to-speech, then the speaker — four stages, each waiting on the last. For
users on OpenAI or Google Gemini, the model itself can do the whole exchange
in one WebSocket: you speak, it hears you, it speaks back.

When the configured assistant model is a **realtime model** (OpenAI Realtime,
Gemini Live), pressing F8 opens a single bidirectional connection and bypasses
the staged pipeline entirely for that turn — F7 dictation is untouched.
Expected time-to-first-audio drops from ~1.5–3 s to **~500–900 ms** depending
on geography. Function calling works identically to the staged path: the same
`[assistant.tools]` config, the same Home Assistant integration, the same
dispatcher — so [voice actions](#voice-actions) work just as well over
realtime as over the regular pipeline.

Realtime audio is expensive (often 5–25× the cost of equivalent text tokens),
so Fono defaults to the **mini** tier on each provider and labels each option
with its cost multiplier in the wizard. The full preview tier is opt-in for
users who explicitly want it.

Concrete plan: `plans/2026-05-25-realtime-end-to-end-assistant-v4.md`. Blocked
on [Voice actions](#voice-actions) landing first so that voice actions are
available from day one on both paths.

### Voice loop for coding agents

> Talk to your coding agent. Hear short, voice-friendly answers back. Pick A, B, or C
> with your voice. Don't touch the keyboard between turns.

**The end goal is agent-agnostic.** Fono will speak the **server** side of the
[Model Context Protocol](https://modelcontextprotocol.io), exposing three voice tools
(`fono.speak`, `fono.listen`, `fono.confirm`) over stdio. Any MCP-capable coding agent —
present or future — becomes voice-driven by adding one `fono` MCP server entry to its
config and pointing at one shared voice-mode system prompt biased toward short
responses and A/B/C choices instead of page-long markdown. Adding a new agent is a
config snippet and a documentation section, never new Fono code.

**Forge is the first dogfood target** because it's the maintainer's daily driver, but
v1 ships verified end-to-end against at least three different agents (Forge + Claude
Code + Cursor) precisely to prove the integration is genuinely agent-agnostic before
tag. Codex CLI, Gemini CLI, Cline, Continue, Windsurf, and Goose ship as best-effort
documentation in the same release, plus an "Adding your own agent" recipe so the story
is genuinely open-ended.

Concrete plan: `plans/2026-05-25-fono-voice-loop-for-coding-agents-v1.md`.
Complementary to (but independent of) the [Voice actions](#voice-actions) work
where Fono is the MCP *client* asking Home Assistant et al. to do things on the
user's behalf.

### Better Wayland hotkeys

Today on Wayland (KDE, GNOME, wlroots) you bind the hotkey through your compositor's
own settings. Once the `org.freedesktop.portal.GlobalShortcuts` portal becomes
universally available, Fono will register its hotkeys through it automatically — zero
setup.

### macOS and Windows

Native integrations for both platforms: menu-bar app and signed `.dmg` on macOS;
system-tray app and native installer on Windows.

---

## Shipped

Newest first.

- ![v0.8.2](https://img.shields.io/badge/v0.8.2-2026--05--25-blue?style=flat-square)
  **Esc to cancel, smarter first-run model picks, and assistant memory
  that survives a dictation pivot.** On Wayland, pressing **Esc**
  during an active recording or assistant reply cancels the turn —
  the portal hotkey backend opens a transient `GlobalShortcuts`
  session (KDE / sway / Hyprland) and the GNOME-Wayland shim writes
  a temporary custom-keybinding, so Esc is only grabbed while Fono
  actually needs it. The same job is exposed as a new `fono cancel`
  CLI verb (idempotent, safe to bind anywhere); `fono assistant stop`
  and the "Stop assistant" tray entry are gone in its favour.

  The first-run wizard is sharper on tricky hardware. CPU-only builds
  no longer get credited with a GPU multiplier they can't deliver,
  and Vulkan-capable integrated GPUs are now split into two classes
  (`Integrated` 1.3× for fp16-only parts like UHD 620;
  `IntegratedTensor` 2.0× for fp16 + cooperative-matrix parts like
  Lunar Lake Xe2 and Apple Silicon). Net effect: older laptops are
  recommended `small` / `small.en` instead of a turbo model that
  can't keep up, while modern tensor-iGPU laptops correctly land on
  `large-v3-turbo`. `fono doctor` now walks the same affordability
  ladder as the wizard so the two never disagree.

  Assistant chat history is no longer wiped when you tap the
  dictation hotkey (F7) mid-conversation. The pivot still stops any
  in-flight assistant playback so it doesn't talk over your
  dictation, but the rolling history is preserved and you can resume
  the conversation on the next F8.

  Fixes: dictation on PipeWire-only Linux hosts (stock Ubuntu 24.04
  without `pulseaudio-utils`) was silently capturing noise because
  the `pw-cat` capture helper was missing `--raw`; clean audio is
  back. Native aarch64 release binary is now built and gated on a
  hosted `ubuntu-22.04-arm` runner (same glibc 2.35 floor, same
  size-budget check). *v0.8.2, 2026-05-25.*

- ![v0.8.1](https://img.shields.io/badge/v0.8.1-2026--05--23-blue?style=flat-square)
  **Two more cloud providers, friendlier installs, and a polished
  pause UI.** Deepgram and Cartesia speech-to-text are wired
  end-to-end (both were advertised in v0.8.0 but failed at startup
  if picked); Deepgram defaults to the newer Nova-3 model and streams
  over a real WebSocket for live dictation. Cartesia text-to-speech
  now picks a native voice per language — Romanian text reads in a
  Romanian voice, English in an English one, automatically.

  `sudo fono install` auto-detects headless servers (no graphical
  session, multi-user systemd target) and picks the systemd lane
  without `--server`; server installs also turn on the Wyoming STT
  listener on port 10300 out of the box so other machines on the LAN
  can use the box for dictation immediately. A new `--desktop` flag
  forces the desktop lane on hosts that just *look* headless.

  The PONDERING pause indicator is now consistent everywhere: it
  shows up on the assistant flow (F8) in the assistant palette, it
  works in live (streaming) dictation, it stays off if you've
  disabled auto-stop, and it no longer flickers on a single breath
  or mouse click. Auto-stop on silence now actually commits when the
  timer expires (previously it only painted the label). The tray
  presets moved from chat-app numbers (0.8 / 1.5 / 3 s) to
  prose-dictation ones (3 / 5 s).

  Smaller fixes: Wayland overlay no longer steals focus or paints
  opaque on GNOME (now backed by a pluggable layer with native
  `wlr-layer-shell` on KDE / wlroots / COSMIC / Hyprland);
  PipeWire audio playback works on every assistant reply;
  LAN dictation against IPv6-advertising Wyoming peers no longer
  fails with `EINVAL`; the history database rebuilds itself if it
  carries an older schema; `fono hwprobe` recommends the same model
  the setup wizard would actually pick. Local Whisper picks better
  defaults out of the box (quality-tested quantization ladder per
  ADR 0027; CPU threads default to physical core count, doubling
  throughput on Zen 3/4 SMT systems). 14 inert config keys were
  removed. One small breaking change: `[overlay].volume_bar` is now
  `"off" | "simple" | "advanced"` instead of a boolean.
  *v0.8.1, 2026-05-23.*

- ![v0.8.0](https://img.shields.io/badge/v0.8.0-2026--05--17-blue?style=flat-square)
  **One-key cloud setup, live preview as a waveform style, and full
  observability across cloud pipelines.** Picking a primary provider
  (OpenAI, Groq, Anthropic, Cerebras, OpenRouter) wires STT, polish,
  assistant, and TTS from a single API-key prompt — the wizard only
  asks for extra keys when the primary doesn't cover something. Four
  new TTS backends (Groq Orpheus, OpenRouter Mini TTS, Cartesia,
  Deepgram) join OpenAI and Wyoming, so users on a non-OpenAI primary
  can run the whole record → STT → polish → TTS loop with one key.
  Opt-in assistant extras in the wizard: vision-capable chat models
  and native web-search where the provider supports it.

  Live transcription becomes the fifth entry in the tray's
  Visualization picker (`Bars | Oscilloscope | Fft | Heatmap |
  Transcript`); picking it both swaps the overlay to streaming text
  *and* routes the dictation hotkey through the live pipeline,
  fixing a long-running bug where live preview only worked for the
  assistant. Hotkeys auto-detect toggle vs push-to-talk per press —
  short tap toggles, hold for 1 s is push-to-talk.

  Every cloud request now flows through a new HTTP layer with a
  per-stage stopwatch and stall watchdog, so a hung TTS upload
  surfaces in 15–30 s instead of waiting for a 60 s timeout. `fono
  doctor` is colorized with a tail mode; `sudo fono install` (and
  the `curl … | sh` one-liner) now walks the user through `fono
  setup` in the same terminal. Desktop notifications cover the
  important failure modes (missing key, daemon crash) with a
  per-session cap so one root cause doesn't spam you. Closes
  issues #8, #9, #11. *v0.8.0, 2026-05-17.*

- ![v0.7.1](https://img.shields.io/badge/v0.7.1-2026--05--05-blue?style=flat-square)
  **Default hotkeys overhauled.** Dictation collapses from F8/F9
  into a single key on `F7`; the voice assistant moves from F10 to
  `F8`. Both default to **toggle** (press once to start, press
  again to stop) via a single new `[hotkeys].mode = "toggle" |
  "hold"` setting that applies globally — no more juggling separate
  hold/toggle keys, and no more holding a key down through the
  multi-second STT → polish → TTS round-trip on the assistant. The old
  F9/F10 defaults collided with htop's kill/quit bindings and, for
  F10, the GTK menubar shortcut. `[hotkeys].toggle` was renamed to
  `[hotkeys].dictation` (old configs continue to parse via a serde
  alias); `[hotkeys].hold` is gone (express push-to-talk as `mode =
  "hold"`).

- ![v0.7.0](https://img.shields.io/badge/v0.7.0-2026--05--04-blue?style=flat-square)
  **Voice assistant.** A second hotkey turns Fono into a voice
  assistant: speak a question, hear the answer through your speakers.
  Fono streams the reply sentence-by-sentence into the text-to-speech
  backend, so the first sentence starts speaking before the model
  finishes generating — you don't wait for the full reply.

  Multi-turn rolling history is preserved across questions (default
  5-minute window). Pressing the dictation key clears the assistant's
  memory; pressing the assistant key again mid-reply barges in with
  history retained; Escape stops playback without forgetting. The
  assistant runs on its own configuration block with independent
  model selection from cleanup — mix a fast local 3B for cleanup with
  a bigger cloud model for the assistant, or any other combination.

  Text-to-speech supports the Wyoming protocol (any
  `wyoming-piper` server on the LAN) and the OpenAI
  `/v1/audio/speech` API. Chat supports Anthropic and the full
  OpenAI-compatible family (OpenAI, Cerebras, Groq, OpenRouter,
  Ollama). Audio playback on Linux uses `paplay`. `fono doctor`
  exercises both factories at startup so a missing API key or
  unreachable Wyoming server surfaces in one place.

- ![v0.6.1](https://img.shields.io/badge/v0.6.1-2026--05--03-blue?style=flat-square)
  **Headless and systemd robustness.** Fono now starts cleanly on a
  headless inference box with no display and no terminal: the GPU
  probe runs in a subprocess so a broken graphics driver can't take
  the daemon down, the hotkey listener is skipped when there's no
  graphical session, and the first-run wizard falls back to safe
  defaults instead of crash-looping under systemd. `sudo fono
  install` verifies the unit actually came up and prints recent logs
  on failure, so a misconfigured install no longer fails silently.
  CLI clients prefer the system-wide socket first, so the daemon
  installed under `fono.service` is reachable from any user on the
  box. LAN discovery on the hardened systemd unit is fixed (the
  service was previously blocking mDNS at the syscall layer). The
  audio-visualisation overlay is on by default; the old start/stop
  chime is gone in favour of the visual feedback shipped in v0.6.0.

- ![v0.6.0](https://img.shields.io/badge/v0.6.0-2026--05--03-blue?style=flat-square)
  **Audio-visualisation overlay + live-dictation VU bar.** A new
  `waveform` cargo feature (default-on, GUI-only) renders a 640-wide
  bottom-centre panel during batch (push-to-talk) recording with a
  selectable style: `bars` (scrolling RMS amplitude),
  `oscilloscope` (connected-line waveform from raw PCM), `fft`
  (real-input spectrum bars, 0–3 kHz), or `heatmap` (rolling
  spectrogram). Configured via `[overlay].waveform = true` and
  `[overlay].style`. The same audio-level pipeline feeds a thin
  right-side VU meter on the live-dictation panel
  (`[overlay].volume_bar = true` by default), so users can monitor
  mic level at a glance without breaking flow. Internally:
  `parec` is now invoked with `--latency-msec=20` for smooth
  chunked capture; `fill_round_rect` fast-paths its rectilinear
  interior; and the heatmap maintains a pre-blended pixel cache
  that scrolls leftward by one frame-width per FFT push, blitting
  straight to the framebuffer. End-to-end CPU per recording lands
  at ~13–15 % across all four styles. Server / headless builds
  (without `real-window`) keep working unchanged via the existing
  no-op `Overlay` stubs.

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
[v0.4.0]: https://github.com/bogdanr/fono/releases/tag/v0.4.0
[v0.5.0]: https://github.com/bogdanr/fono/releases/tag/v0.5.0
[v0.6.0]: https://github.com/bogdanr/fono/releases/tag/v0.6.0
[v0.6.1]: https://github.com/bogdanr/fono/releases/tag/v0.6.1
[v0.7.0]: https://github.com/bogdanr/fono/releases/tag/v0.7.0
[v0.7.1]: https://github.com/bogdanr/fono/releases/tag/v0.7.1
[v0.8.1]: https://github.com/bogdanr/fono/releases/tag/v0.8.1
[v0.8.0]: https://github.com/bogdanr/fono/releases/tag/v0.8.0
