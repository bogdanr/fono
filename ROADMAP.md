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
<td valign="top" width="50%"><img src="https://img.shields.io/badge/Up_next-2ea44f?style=for-the-badge" alt="Up next"><br><br><strong><a href="#personal-vocabulary--voice-correction">Personal vocabulary &amp; voice correction</a></strong><br>Teach Fono once that "Phono" means "Fono" — it sticks forever, deterministically, before the text ever hits the cursor.<br><br><strong><a href="#automatic-translation">Automatic translation</a></strong><br>Speak in any language, type in another — any pair, per-app rules, batch and live parity.<br><br><strong><a href="#talk-over-the-assistant">Talk over the assistant</a></strong><br>Just start speaking — Fono hears you over its own voice and hands the turn back. No hotkey, no escape, no awkward "stop, stop, stop".</td>
<td valign="top" width="50%"><img src="https://img.shields.io/badge/On_the_horizon-0075ca?style=for-the-badge" alt="On the horizon"><br><br><strong><a href="#self-hosted-modelship-backend">Self-hosted Modelship backend</a></strong><br>One box on your LAN runs the LLM, speech-to-text, text-to-speech, and embeddings — every Fono desktop points at it, fully local.<br><br><strong><a href="#hover-context-injection">Hover-context injection</a></strong> <em>(experimental)</em><br>Terminal hovered → shell prompts. Code editor hovered → identifier casing.<br><br><strong><a href="#voice-actions">Voice actions</a></strong><br>"Turn on the kitchen lights." Fono speaks to Home Assistant, GitHub, and your own MCP servers — the assistant doesn't just answer, it does.<br><br><strong><a href="#better-wayland-hotkeys">Better Wayland hotkeys</a></strong><br>Auto-register via the <code>GlobalShortcuts</code> portal when available.<br><br><strong><a href="#macos-and-windows">macOS + Windows</a></strong><br>Native platform integrations.<br><br><strong><a href="#shared-ggml-size-reclaim-spike">Shared ggml size-reclaim spike</a></strong><br>Investigated whether one source-level ggml runtime could replace the linker workaround — measured outcome: the duplicate is already pruned at link time, so the reclaim is about zero. Deferred.</td>
</tr>
</table>

![Recently shipped](https://img.shields.io/badge/Recently_shipped-6e7681?style=for-the-badge)

**[v0.12.0 — Hands-free wake-word activation](#shipped)**  
Idle, listen for a spoken wake phrase, and start dictating or talking to the
assistant with no key and no hands — detection runs locally on the ONNX runtime
already in the binary, so it adds no new dependency and no measurable size. When
the LAN Wyoming server is on, Fono auto-serves wake detection over it, so Home
Assistant discovers it as a drop-in wake-word provider with audio staying on the
machine. *(2026-06-24)*

**[v0.11.1 — Hands-free realtime conversation](#shipped)**  
Tap the assistant hotkey to open one persistent realtime session and hold a
natural, back-and-forth spoken conversation — the model hears when you stop and
replies in its own voice, with no key between turns. The overlay shows whose turn
it is and animates to the live audio, and the session closes itself on a short
silence, a goodbye, or a hard cap so it never quietly burns cloud credits. The
realtime path now connects strictly on demand. *(2026-06-22)*

**[v0.11.0 — Realtime voice assistant + one-key Gemini](#shipped)**  
Hold a spoken conversation straight over the Gemini Live WebSocket — with memory
of earlier turns and an optional look at your screen — drive the whole pipeline
with a single Google Gemini key, and hear cloud voices stream back gaplessly.
Plus universal cloud-voice autodiscovery, per-program voices, and ElevenLabs and
Speechmatics backends. *(2026-06-18)*

**[v0.10.0 — Faster local AI, local voice out of the box](#shipped)**  
The embedded engine reuses prompt checkpoints so warm dictations and assistant
turns stay quick, offline Piper/Kokoro text-to-speech ships in the default
binary, and local AI cleanup now types into the cursor word-by-word instead of
making you wait for the whole pass. The speed work is written up in
[Making local LLM fast](https://bogdan.nimblex.net/programming/2026/06/10/making-local-llm-fast.html).
*(2026-06-12)*

**[v0.9.1 — See your screen, dictate in any language](#shipped)**  
The voice assistant and your coding agents can now look at your screen when you
point at something. AI cleanup no longer drops text or accents on non-English
dictation. Three new overlay looks. *(2026-05-29)*

**[v0.9.0 — Voice loop for coding agents](#shipped)**  
Talk to Forge, Claude Code, Cursor, Codex CLI, Gemini CLI and other
MCP-capable agents entirely by voice. Short spoken answers, A/B/C choices,
no keyboard between turns. Plus a Debian/Ubuntu install fix so the overlay
shows up on first run. *(2026-05-26)*

**[v0.8.2 — Context-aware dictation + Esc-to-cancel](#shipped)**  
Window-aware Whisper and LLM prompting (terminal, code editor, private windows),
Wayland Esc cancel, sharper wizard recommendations on older iGPUs,
assistant memory that survives a dictation pivot, PipeWire capture fix,
native aarch64 binary. *(2026-05-26)*

**[v0.8.1 — Two more cloud providers](#shipped)**  
Deepgram + Cartesia STT, headless install, pause UI polish. *(2026-05-23)*

**[v0.8.0 — One-key cloud setup](#shipped)**  
Live preview as a waveform style; four new TTS backends. *(2026-05-17)*

[Full changelog ↓](#shipped)

---

## Up next

### Personal vocabulary & voice correction

> Say "Fono". Get "Fono". Every time. Without touching the keyboard.

Whisper — cloud and local alike — reliably mishears proper nouns, project names,
and jargon. Fono will let you teach it your vocabulary once, and have every future
dictation corrected before the text ever reaches the cursor.

**How it works:** a `vocabulary.toml` in your config directory maps mishearings to
canonical spellings (`phono → Fono`, `bug done → Bogdan`, `cube ernetes → Kubernetes`).
After every STT result — regardless of whether LLM cleanup is on or off — a
word-boundary-aware substitution pass rewrites the final text before injection. It is
deterministic and idempotent: no probability, no model call, no network round-trip.

The vocabulary grows via `fono vocabulary add/remove/list`. `fono vocabulary suggest`
mines your dictation history for swaps you already accepted via LLM cleanup and offers
them for one-keystroke confirmation — no auto-pollution of your vocabulary file.

Later in the same slice: a **voice "fix that" correction hotkey**. Press it after a
mishearing, speak the intended word, and Fono re-injects the corrected text and
auto-records the (heard → meant) pair into your vocabulary so the same error never
recurs. Plan: `plans/2026-06-03-correction-with-memory-v2.md`.

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

### OpenAI Realtime backend

> The same hands-free conversation, on OpenAI's voice models.

Live conversation mode is built provider-agnostic at the trait and catalogue
layer, so adding a second realtime backend is a self-contained client, not a
rearchitecture. OpenAI's Realtime API is the next provider to land: it speaks a
different wire protocol (`session.update` + `response.create` instead of Gemini's
`setupComplete` / `audioStreamEnd`) and runs at 24 kHz in and out, so it needs
its own client module and an input resampler, both already scoped. Once it lands,
tap-to-converse, the cost guardrails, the floor-ownership overlay, and the
mute-while-speaking baseline all apply unchanged — you simply pick OpenAI as your
assistant provider.

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

Voice UX polish follow-up: an optional server-side wake chime
before TTS after long idle gaps, as a deferred refinement to the
spoken refocus preamble.

---

## On the horizon

### Local REST API

Fono already runs as a daemon with a Unix-socket IPC layer — every CLI subcommand
(`fono toggle`, `fono history`, `fono use …`) is a client talking to it. The next step
is exposing that same interface over HTTP, so scripts, editor plugins, and tools that
are not MCP-capable can drive Fono without any special tooling. This is a thin shim
over the existing IPC surface, independent of the MCP work.

### Self-hosted Modelship backend

> One box on your LAN runs the whole stack. Fono just points at it.

Fono already speaks the OpenAI-compatible API for STT, cleanup, the assistant, and
TTS. [Modelship](https://github.com/alez007/modelship) (Apache-2.0) is a self-hosted,
multi-model inference server that runs an LLM, speech-to-text, text-to-speech, and
embeddings simultaneously behind a single OpenAI-compatible endpoint, with per-model
GPU/CPU allocation. The next step is making Modelship a first-class target: point every
Fono stage at one Modelship server on your LAN from a single base URL, so a household
or office runs all the heavy models on one machine while every desktop dictates against
it — fully local, no cloud, no per-desktop GPU. The wizard learns to detect a Modelship
server (it already advertises its models via `GET /v1/models`) and offers a one-key
setup that wires STT, polish, assistant, and TTS at once.

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

Concrete plan: `plans/2026-05-22-voice-actions-via-mcp-v1.md`. Once it lands,
voice actions apply in lockstep to both the staged pipeline and the realtime
assistant.

### Better Wayland hotkeys

Today on Wayland (KDE, GNOME, wlroots) you bind the hotkey through your compositor's
own settings. Once the `org.freedesktop.portal.GlobalShortcuts` portal becomes
universally available, Fono will register its hotkeys through it automatically — zero
setup.

### macOS and Windows

Native integrations for both platforms: menu-bar app and signed `.dmg` on macOS;
system-tray app and native installer on Windows.

### Shared ggml size-reclaim spike

Fono currently links `ggml` through both the local STT and local LLM stacks. The
existing linker workaround keeps the binary buildable, but the long-term cleanup
is to investigate a source-level shared `ggml` runtime between `whisper-rs-sys`
and `llama-cpp-sys-2`.

This is a standalone spike, not part of the local TTS critical path: confirm the
ABI/version reconciliation work, decide whether a forked or upstreamed sys-crate
path is viable, and re-measure the expected binary-size win.

**Outcome (2026-06-24): deferred — the reclaim is ≈ 0 MiB.** A direct
measurement of the shipped `cpu` artefact found `ggml` is already present as a
*single* copy: `-ffunction-sections`/`-fdata-sections` + `--gc-sections` collect
the duplicate copy's sections at link time, so the `--allow-multiple-definition`
trick (ADR 0018) ships only one `ggml`. The long-standing **~7 MiB** estimate was
an archive-size inheritance that does not survive the link. The linker workaround
is now the documented steady state; a source-level shared `ggml` would buy no
binary size (only build time). See `docs/binary-size.md` §4 and
`plans/2026-06-23-shared-ggml-size-reclaim-spike-v1.md`.

---

## Shipped

Newest first.

- ![v0.12.0](https://img.shields.io/badge/v0.12.0-2026--06--24-blue?style=flat-square)
  **Hands-free wake-word activation.** Fono can now idle and listen for a
  spoken wake phrase, then start dictation or the assistant on the same path
  the hotkey uses — no key, no hands. Detection runs locally on the ONNX
  runtime already in the binary via
  [openWakeWord](https://github.com/dscripka/openWakeWord), so it adds no new
  dependency and no measurable size, and your audio never leaves the machine
  while idle on the default path. The listener suspends during any active
  recording or assistant turn and resumes when Fono goes idle. It ships with a
  clean Apache-2.0 default phrase as the only enabled model, plus an opt-in
  community phrase catalog that is downloaded on demand, never bundled, and
  shows its NonCommercial license as a notice when you pick one. When the LAN
  Wyoming server is enabled, Fono automatically serves wake detection over it —
  exactly like it serves STT and TTS, with no extra switch — so Home Assistant
  discovers Fono as a drop-in wake-word provider and detection runs on the Fono
  box with audio staying on the machine. Behind an explicit "idle mic audio
  leaves the machine over the LAN" warning, Fono can instead forward audio to an
  external `wyoming-openwakeword` service, and `fono doctor` reports the
  wake-word configuration and that privacy warning.

  The clean-license `hey_fono` default model is not yet hosted, so the local
  always-on listener stays off until you enable it; the auto-served Wyoming path
  uses the community `hey_jarvis` model as a temporary fetchable default in the
  meantime. An offline training pipeline for custom models ships alongside.
  Engine and licensing rationale is in
  [ADR 0012](docs/decisions/0012-wake-word-activation.md). *v0.12.0, 2026-06-24.*

- ![v0.11.1](https://img.shields.io/badge/v0.11.1-2026--06--22-blue?style=flat-square)
  **Hands-free realtime conversation mode.** Tapping the assistant hotkey now
  opens a first-class, back-and-forth spoken conversation with a realtime model
  (Gemini Live today): talk, listen to the reply, and just keep talking — no key
  press between turns, all over one persistent session. The on-screen overlay
  shows whose turn it is and animates to the live audio — green while you speak,
  sky-blue while the assistant does. The conversation ends on its own after a
  short silence or when you say you're done, and instantly on a second tap or
  Escape, so it never sits there running up cost. The session connects only when
  you start it, never at startup. Holding the hotkey is unchanged: hold to talk,
  release to hear the full reply. To stay clean on every desktop without echo
  cancellation, the mic is muted while the model speaks, so you take turns rather
  than talk over it — acoustic barge-in is the next upgrade.

  Also removes the realtime startup prewarm shipped in 0.11.0: it only warmed
  transient network caches that went stale within minutes, so realtime sessions
  now connect strictly on demand at first use. *(2026-06-22)*

- ![v0.11.0](https://img.shields.io/badge/v0.11.0-2026--06--18-blue?style=flat-square)
  **Realtime voice assistant, one-key Google Gemini, and gapless cloud
  speech.** The assistant hotkey can now talk straight to the model over
  the Gemini Live WebSocket instead of running the staged speech-to-text →
  chat → text-to-speech relay: audio streams up as you speak and the spoken
  reply streams back over one session, with memory of earlier turns and an
  optional one-shot look at the focused window when vision is enabled. A
  single Google Gemini API key now drives the whole pipeline — speech-to-text,
  cleanup, the assistant, and native Gemini text-to-speech — and cloud
  voices stream back gaplessly instead of arriving a sentence at a time.

  Plus: universal, fail-safe cloud-voice autodiscovery (probe a provider's
  live catalogue on demand, never on the speech path); per-program voices so
  different apps speak in different, stable voices chosen from friendly
  gendered labels; ElevenLabs (Scribe + Eleven v3) and Speechmatics as
  first-class cloud backends; two male English Kokoro voices; an automatic
  local fallback when an English-only cloud voice is handed non-English
  text; and turn traces you can actually read.

  Fixes: barge-in now works while the assistant is still thinking, not just
  while speaking; the first realtime turn no longer pays the full WebSocket
  handshake latency; Kokoro local voices no longer fail to load with a
  "Greater(13) node" error; payment-required cloud failures surface as a
  notification instead of failing silently. *(2026-06-18)*

- ![v0.10.0](https://img.shields.io/badge/v0.10.0-2026--06--12-blue?style=flat-square)
  **Faster local AI, local voice out of the box, and cleanup that types
  as it thinks.** The embedded llama.cpp engine now keeps reusable
  checkpoints of the prompts it has already processed — the cleanup
  instructions, the assistant's system prompt, your running conversation —
  so warm dictations and follow-up assistant turns skip re-crunching all of
  it and process only what's new. Time-to-first-token stays flat as a
  conversation grows instead of climbing every turn. Local text-to-speech
  now ships built into the `cpu` and `gpu` binaries by default: offline
  Piper across 42 voices / 38 languages, with **Kokoro** providing a
  higher-quality voice for English (four voices sharing one model). And
  local AI cleanup streams into the cursor word-by-word as the model
  decodes — first words in about one to three seconds on a long dictation
  instead of after the whole pass — while still running every safety check
  on the first sentence before a character is typed. The prompt-cache speed
  work is written up in
  [Making local LLM fast](https://bogdan.nimblex.net/programming/2026/06/10/making-local-llm-fast.html).

  Fixes: local cleanup with a Gemma model no longer loops or runs away;
  `[polish].backend = "local"` runs the embedded engine instead of silently
  routing Gemma models to an Ollama HTTP server; the transcription history
  database and `/var/log/fono.log` are clamped to owner-only permissions on
  shared machines; and several local-TTS language/voice-selection bugs are
  closed. Hands-free recording now auto-stops after 3 s of silence (was 5 s).
  *(2026-06-12)*

- ![v0.9.1](https://img.shields.io/badge/v0.9.1-2026--05--29-blue?style=flat-square)
  **See your screen, dictate in any language.** The F8 voice
  assistant and any connected coding agent can now look at your
  screen when you reference something on it — automatic mode grabs
  the focused window instantly, interactive mode opens your
  desktop's region picker so you frame exactly what to share.
  Private windows (KeePassXC, Bitwarden, 1Password) are never
  captured, and it works with whatever screenshot tool you already
  have — no new required dependencies. Coding agents reach it via
  the `fono.screen` MCP tool; the assistant calls `fono_screen` via
  LLM function-calling.

  AI cleanup of non-English dictation is fixed: it no longer
  silently returns empty (injecting the raw transcript) and no
  longer drops diacritics on the way to the cursor — Romanian,
  French, Spanish and the rest now come out polished and correctly
  accented. The voice assistant pipeline is on by default, and the
  recording overlay gains three new looks (Aurora Beziers,
  System/360, Terrain 3D). *(2026-05-29)*

- ![v0.9.0](https://img.shields.io/badge/v0.9.0-2026--05--26-blue?style=flat-square)
  **Voice loop for coding agents (early preview).** Fono ships an MCP
  server with three voice tools — `fono.speak`, `fono.listen`, and
  `fono.confirm` — that let any MCP-capable coding agent drive a
  voice loop: the agent speaks short replies, asks free-form
  questions, or offers A/B/C choices, and you answer with your
  voice. Verified end-to-end against Forge and Claude Code;
  best-effort against Cursor, Codex CLI, Gemini CLI, Cline,
  Continue, Windsurf, and Goose. `fono agent-setup <name>` wires
  everything in one shot — enables the MCP server, merges the
  right `mcpServers.fono` entry into your agent's MCP config, and
  appends the shared voice-mode preset to your project's
  `AGENTS.md` / `CLAUDE.md`. The same dictation overlay pops up
  while the agent is listening so you always know whether Fono is
  hearing you, and the tray icon turns amber while a voice turn is
  in flight. A built-in background-speech filter ignores radio /
  TV / side-conversation chatter when the agent is waiting for an
  answer (`[mcp].relevance_filter`, default `"heuristic"`, with an
  optional `"llm"` mode that uses the configured polish backend as
  a one-shot classifier with a 1.5 s timeout). New companion CLI
  verbs: `fono speak --stream` (sentence-segments stdin and speaks
  through the configured TTS backend) and
  `fono use mcp-server on|off`. Disabled by default — opt in with
  `fono use mcp-server on`. ADR 0030 captures the design.
  Protocol, defaults, and tool surface may still shift before the
  feature graduates.

  Fixes: installing via `curl https://fono.page/install | sh` on
  Debian/Ubuntu desktops no longer skips the prompt that offers to
  install `libxkbcommon-x11` and `xdotool`, so the on-screen
  recording overlay shows up on first run instead of after a manual
  daemon restart. The background daemon spawn also reconstructs
  `DISPLAY` and `XAUTHORITY` when sudo strips them. Server installs
  are unaffected. *v0.9.0, 2026-05-26.*

- ![v0.8.2](https://img.shields.io/badge/v0.8.2-2026--05--26-blue?style=flat-square)
  **Context-aware dictation, Esc-to-cancel, and smarter first-run model
  picks.** Fono now reads the focused window at hotkey-press time and
  silently adjusts both the Whisper `initial_prompt` and the LLM cleanup
  suffix — no user configuration required. Terminal emulators get a
  shell-vocabulary Whisper hint (`ls -la`, `grep -r`, `chmod 755`,
  `git commit`, etc.) and a shell-syntax LLM cleanup suffix. Code
  editors (Cursor, Zed, Kate) get a language-specific hint derived
  from the file extension in the window title. Private windows
  (KeePassXC, Bitwarden) suppress history writes. On Linux, `/proc`
  enrichment detects the active project type (Rust, Python, Node, Go,
  Docker, K8s) and coding agents (Forge, Claude Code, Codex, Aider,
  Goose, and others) when a terminal is focused. Detection covers X11,
  sway, Hyprland, and GNOME Wayland (XWayland fallback for GNOME 46+).

  On Wayland, pressing **Esc** during an active recording or assistant
  reply cancels the turn — the portal hotkey backend opens a transient
  `GlobalShortcuts` session (KDE / sway / Hyprland) and the
  GNOME-Wayland shim writes a temporary custom-keybinding, so Esc is
  only grabbed while Fono actually needs it. The same job is exposed
  as a new `fono cancel` CLI verb.

  The first-run wizard is sharper on tricky hardware: CPU-only builds
  no longer get credited with a GPU multiplier, and Vulkan-capable
  integrated GPUs are split into `Integrated` (1.3×, fp16-only) and
  `IntegratedTensor` (2.0×, fp16 + cooperative-matrix) classes.

  Fixes: PipeWire-only Linux hosts (stock Ubuntu 24.04) were silently
  capturing noise because `pw-cat` was missing `--raw`; clean audio is
  back. Native aarch64 release binary built on `ubuntu-22.04-arm`.
  *v0.8.2, 2026-05-26.*

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
[v0.10.0]: https://github.com/bogdanr/fono/releases/tag/v0.10.0
[v0.9.1]: https://github.com/bogdanr/fono/releases/tag/v0.9.1
[v0.9.0]: https://github.com/bogdanr/fono/releases/tag/v0.9.0
[v0.8.2]: https://github.com/bogdanr/fono/releases/tag/v0.8.2
[v0.8.1]: https://github.com/bogdanr/fono/releases/tag/v0.8.1
[v0.8.0]: https://github.com/bogdanr/fono/releases/tag/v0.8.0
