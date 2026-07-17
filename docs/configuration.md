# Configuration

Fono is configured through two TOML files in `~/.config/fono/`:

| File | Mode | Purpose |
|---|---|---|
| `config.toml` | `0644` | All non-secret settings |
| `secrets.toml` | `0600` | API keys (refuses to load if world-readable) |

Both files are written atomically, can be edited with any text editor,
and are reloaded on every `fono use` command (no daemon restart). Run
`fono setup` to recreate them from the wizard if you ever want a clean
slate.

## Hot-reload

The daemon listens on its IPC socket for a `Reload` message. Anything
that mutates the file (`fono use`, the tray menu, `fono setup`, or a
manual edit followed by `fono toggle` from any terminal) re-reads the
config and applies the change atomically. The orchestrator's
single-in-flight cap means an active pipeline finishes first, then the
new config takes effect on the next press.

## Section overview

The full schema lives in `crates/fono-core/src/config.rs` with
field-level rustdoc comments. The user-facing sections are:

| Section | Purpose | See |
|---|---|---|
| `[general]` | Languages, autostart, system-mute, clipboard safety net | this file |
| `[hotkeys]` | Dictation, cancel, assistant key bindings | below |
| `[audio]` | VAD, silence trimming, auto-stop | this file |
| `[stt]` | Speech-to-text backend selection + per-backend config | [providers.md](providers.md) |
| `[polish]` | Cleanup-pass backend selection + behaviour | [providers.md](providers.md) |
| `[assistant]` | Voice-assistant chat backend + capability flags | [providers.md](providers.md) |
| `[tts]` | Text-to-speech for the assistant | [providers.md](providers.md) |
| `[interactive]` | Streaming-pipeline tuning (live mode) | [interactive.md](interactive.md) |
| `[inject]` | Injection backend override and clipboard safety net | [inject.md](inject.md) |
| `[overlay]` | Waveform style; picking `transcript` enables live mode | [interactive.md](interactive.md) |
| `[history]` | History DB retention, FTS5 settings | this file |
| `[update]` | Auto-check toggle, release channel | [install.md](install.md) |
| `[server]` | Wyoming STT/TTS host + local LLM API (OpenAI/Ollama) + web settings UI | [install.md](install.md), below |
| `[network]` | mDNS metadata overrides | — |
| `[mcp]` | MCP server limits + voice-tool relevance filter | this file |
| `[[context_rules]]` | Per-app prompt/behaviour overrides | this file |
| `[wakeword]` | Always-on "hey fono" wake-word activation | below |

## Common knobs by example

### Languages — bilingual setup (English + Romanian)

```toml
[general]
languages = ["en", "ro"]            # empty = unconstrained auto-detect
```

Codes are BCP-47 (alpha-2 forms most-commonly used). Empty list = full
Whisper auto-detect; one entry = constrained auto-detect (not a hard
force — see ADR 0016); two or more entries ban every language outside
the set. Order doesn't matter; the in-memory cache reflects what was
actually heard. See the *Multilingual STT and language stickiness*
section of [providers.md](providers.md).

### Auto-stop after N seconds of silence

```toml
[audio]
auto_stop_silence_ms = 3000         # 0 disables; tray presets: 0 / 3000 / 5000
```

Only fires in toggle mode. Hold-to-talk and assistant-hold always honour
the explicit release.

### Force a specific input device

```toml
[audio]
input_device = "alsa_input.pci-0000_00_1f.3.analog-stereo"
```

`fono doctor` lists the detected default and any candidates; `arecord
-l` (ALSA) or `pw-cli list-objects | grep node.name` (PipeWire) gives
the system view.

### Switch STT or polish

Edit by hand if you want, but `fono use` is shorter:

```sh
fono use stt groq           # writes [stt].backend = "groq"
fono use polish anthropic   # writes [polish].backend = "anthropic"
fono use polish none        # disable polish entirely
fono use show               # show the active selection
```

The minimal viable cloud block is two lines plus one key:

```toml
[stt]
backend = "groq"
[polish]
backend = "cerebras"
enabled = true
```

…with `GROQ_API_KEY` and `CEREBRAS_API_KEY` either in `secrets.toml` or
exported in the environment. The factories fall through to the canonical
env-var name when the optional `[stt.cloud]` / `[polish.cloud]`
sub-blocks are absent.

### Skip polish for short utterances

```toml
[polish]
skip_if_words_lt = 3        # default; one- and two-word captures bypass cleanup
```

Useful when the polish step is slower than the dictation it cleans
(typical for one-word commands and chat-bar dictation).

### Live mode (streaming preview)

Live mode is gated by the overlay style, not by a separate flag. Pick
`transcript` in the tray (*Preferences → Waveform style*) or by hand:

```toml
[overlay]
style = "transcript"        # bars | oscilloscope | fft | heatmap | transcript
```

`[interactive]` tunes the streaming pipeline once it's on; most users
never touch it. See [interactive.md](interactive.md).

### Serve local inference over HTTP (OpenAI + Ollama API)

Fono can expose whatever assistant you already have configured as a local
HTTP endpoint that speaks **both** the OpenAI (`/v1/chat/completions`,
`/v1/models`) and Ollama-native (`/api/chat`, `/api/tags`) wire formats.
Editors, Open WebUI, `llm`, LangChain, and Home Assistant's Ollama
conversation agent can then use Fono as a local model backend (ADR 0036).

```toml
[server.llm]
enabled = true              # off by default
bind    = "127.0.0.1"       # loopback only; "0.0.0.0" exposes it on the LAN
port    = 11434             # Ollama's port, so existing clients connect unchanged
auth    = true              # require an API key for non-loopback callers (default on)
# model = ""                          # optional served-model override (see below)
```

**Authentication is a simple on/off switch, on by default.** When `auth`
is on, callers reaching the server from another machine must present a
valid inbound API key as `Authorization: Bearer <key>` (loopback callers
— the local owner — are always trusted, so a local client is never locked
out). There is no token string in this file: keys are managed separately
(see [Inbound API keys](#inbound-api-keys-for-the-llmsttts-api-and-web-ui)
below) and stored hashed in `api_keys.sqlite`, never in `config.toml`.
Set `auth = false` only if you deliberately want an unauthenticated LAN
endpoint.

Toggle it from the tray too (*Servers → Local LLM server*) — the listener
starts/stops in place, no restart. The served model tracks whatever
`[assistant]` backend is active, so a swap via `fono use assistant …`
takes effect on the next request without restarting the listener.

**Realtime assistants fall back automatically.** If your `[assistant]` is
a *realtime* speech-to-speech model (e.g. Gemini Live) that the text chat
API can't expose, Fono automatically serves the **same provider's default
text model** instead — for Gemini that's `gemini-flash-lite-latest`,
reusing the same API key. So you can keep Gemini Live driving your F8
voice conversations *and* get a fast, cheap, smart text model on the API
at the same time, with zero extra configuration. `fono doctor` prints the
model that is actually being served.

**Pin a specific model** with the optional `model` override. It wins over
both the primary assistant and the realtime fallback — handy to keep
Gemini Live for voice while serving a different Gemini text model over the
API:

```toml
[server.llm]
enabled = true
model   = "gemini-2.5-flash"   # serve this regardless of the [assistant] model
```

The override uses the same provider and API key as `[assistant]`. Leave it
empty (the default) to serve the active assistant with the automatic
realtime fallback described above. See
[home-assistant.md](home-assistant.md) for the Home Assistant wiring.

**Cloud backends get full fidelity via pass-through.** When the served
backend is an OpenAI-compatible **cloud** provider (OpenAI, Gemini, Groq,
Cerebras, OpenRouter), Fono forwards your client's `/v1/chat/completions`
request **straight to the provider** — injecting your stored API key on
the way out — and streams the response back unchanged. This means every
model the provider offers, plus tool/function-calling, vision, JSON mode,
and every request parameter, work exactly as if you called the provider
directly; there is nothing to configure and no per-feature gaps. `GET
/v1/models` lists the provider's full catalogue, and the `model` your
client requests is honoured verbatim (the `model` override above is only
the *default* used when a client omits it). Backends that are not
OpenAI-shaped — the local llama.cpp engine, Anthropic, and the
Ollama-native `/api/*` surface — are served through Fono's built-in
adapter instead. `fono doctor` shows which path is in effect.

> **Security note:** because your client's requested model is honoured and
> your provider key is injected outbound, exposing `[server.llm]` on
> `0.0.0.0` **with `auth = false`** turns the box into an open relay to
> your paid cloud account. Keep the loopback default, or leave `auth` on
> (the default) and create at least one API key before binding to the
> LAN. `fono doctor` warns loudly when a server is LAN-exposed with auth
> off, or on with no keys (which rejects every remote call).

**See who's calling the server.** At `debug` level the server prints one
line per request on the `fono::llm::server` target — run the daemon with
`--debug` (or `FONO_LOG=fono::llm::server=debug`) to see them:

```text
openai/chat 200  proxy→gemini  gemini-flash-lite-latest  stream  ttft=310ms total=1.84s  214tok @116/s  via ollama/0.3.3
ollama/chat 200  adapt  qwen2.5-3b-instruct  stream  ttft=180ms total=3.90s  301tok @77/s  via Home Assistant/2024.12
```

Each line shows the endpoint + status, whether the request was proxied to
a cloud provider (`proxy→…`) or served by the local adapter (`adapt`), the
model, timing (time-to-first-token + total), an output-token count and
throughput where available, and the client's `User-Agent` (`via …`) so you
can tell callers apart on a shared port. The peer IP is appended for
non-loopback callers. **Prompt and reply content are never logged** — the
line is metadata only.

### Settings in the browser

Every user-facing option in this file can also be edited from a local
web page — a searchable accordion with one section per config area,
live summaries, and write-only API-key fields (stored values are never
sent to the browser). Open it with:

```console
$ fono config web        # enables [server.web] if needed, opens the browser
```

or via the tray's **Settings…** entry, which starts the listener on
demand. The underlying block:

```toml
[server.web]
enabled = false          # off by default; `fono config web` / tray flip it on
bind    = "127.0.0.1"    # loopback only; non-loopback binds should keep auth on
port    = 10808
auth    = true           # require an API key for non-loopback callers (default on)
```

Saves go through the same atomic-write + hot-reload path as `fono use`,
so changes apply immediately — no daemon restart. API keys entered on
the page land in `secrets.toml`, never in `config.toml` or any HTTP
response.

> **Security note:** the page can rewrite your whole config and store
> API keys. It refuses non-loopback peers while `bind` is loopback; if
> you widen `bind`, keep `auth = true` (the default) and create a key —
> the daemon warns loudly otherwise. Loopback browsers are always
> trusted, so you can create the first key locally without a lockout.
> Turning the listener off again is a config edit (`enabled = false`)
> plus a daemon restart.

### Inbound API keys (for the LLM/STT/TTS API and web UI)

The `auth` toggles above are guarded by a set of **inbound API keys** —
distinct from the outbound provider keys in `secrets.toml`. One key set
covers everything Fono *serves*: the OpenAI/Ollama chat API, the
speech-to-text (`/v1/audio/transcriptions`) and text-to-speech
(`/v1/audio/speech`) routes, and the web settings page.

Manage them from the web settings **API Keys** section (a table of name,
masked secret, created / last-used dates, expiry, and per-month usage,
with create / rename / revoke / delete) or from the CLI:

```console
$ fono server keys create laptop            # prints the secret ONCE — copy it now
$ fono server keys create ci --expires-in-days 30
$ fono server keys list                      # masked; shows last-used + monthly usage
$ fono server keys rename 3 home-assistant
$ fono server keys expire 3 --in-days 90     # or --never to clear an expiry
$ fono server keys revoke 3                   # disables it, keeps usage history
$ fono server keys delete 3                   # removes it and its counters
```

Keys live in `api_keys.sqlite` (mode `0600`) as a SHA-256 hash — the
plaintext secret is shown **exactly once**, at creation, and can never be
shown again by the CLI or the web page. Use it as a bearer token:
`Authorization: Bearer fono_sk_…`.

**Usage tracking without an access log.** Each key records a coarse
"last used" timestamp (debounced) and per-day / per-month request
*counts* — not a per-request log. The counters are pre-aggregated and
old buckets are pruned, so `api_keys.sqlite` stays small no matter how
much the API is called; it never grows into an access log. The web UI
shows the current month's count per key; `fono server keys list` prints
the same.

**Upgrading from an older token.** Earlier versions used a single
`auth_token_ref` string per server. On first run after upgrading, any
non-empty `auth_token_ref` is migrated into a named inbound key
(`migrated-llm` / `migrated-web`), `auth` is left on, and the old field
is cleared from `config.toml`. The migration logs the new secret once so
you can update existing clients.

### Per-app context rules

```toml
[[context_rules]]
match_class = "Slack"               # WM_CLASS / wl_compositor app-id
prompt_append = "Format as a chat message; keep it casual."

[[context_rules]]
match_class = "code"                # VSCode
polish = "anthropic"                # override polish backend just for this app
```

Rules are evaluated in order; the first match wins. The `match_class`
field matches the focused window's class id (`xprop WM_CLASS` on X11,
`hyprctl activewindow` on Hyprland, etc.).

### History retention

```toml
[history]
max_entries = 10_000        # rolling cap; older entries are pruned
```

`fono history clear` truncates the table without touching the file;
deleting `~/.local/share/fono/history.sqlite` wipes everything.

### MCP voice-tool relevance filter

When a coding agent calls `fono.listen` (directly or via
`fono.confirm`), the captured utterance is scored to filter out
background speech (radio, TV, side conversation, prompt-TTS echo):

```toml
[mcp]
# "off"       — disable the filter, every transcript is returned.
# "heuristic" — length / filler / echo rules only (cheap, default).
# "llm"       — heuristic first, then the configured polish backend
#               as a one-shot classifier (1.5 s hardcoded timeout;
#               fails open on timeout / parse failure).
relevance_filter = "heuristic"

# Maximum number of background utterances the loop will drop before
# returning the most recent one regardless. Prevents an infinite
# wait in pathological environments.
relevance_max_rejections = 2

# System-prompt override for `fono.summarize` (MCP tool) and
# `fono summarize` (CLI). Empty/omitted — use the built-in
# prompt: 1-2 spoken sentences saying who wants what; never read raw
# logs or long content aloud; mention attachments briefly by kind.
# Requires a configured `[assistant]` backend.
# A failed summarize request is retried once on the configured
# backend, then tried once on the first other backend with a usable
# API key (canonical env vars: CEREBRAS_API_KEY, GROQ_API_KEY, …) or
# local model. Cloud requests time out fast (10 s to first byte);
# the local backend keeps a long budget (60 s) for model load.
# summarize_prompt = ""
```

The 1.5 s LLM-classifier timeout is hardcoded in
`crates/fono-mcp-server/src/relevance.rs` and not user-configurable;
it's a per-iteration ceiling, not a budget. On timeout the filter
**fails open** (accepts the utterance) so a sluggish polish backend
can never strand a real answer.

Tray feedback during MCP voice interactions is **automatic** — no
config knob. The daemon's tray icon turns amber (the same colour as
the existing `Processing` state used for STT / polish) for the
duration of any `fono.listen`, `fono.speak`, or `fono.confirm` call,
then restores whatever it was showing before. See
[coding-agents.md](coding-agents.md#what-you-see-and-hear-during-an-mcp-voice-turn).

### Per-program voices

Fono can speak with a **different voice per program**, so you can tell
at a glance whether it was the coding agent, the chat notifier, or a
coach talking. Every voice path — `fono.speak`, `fono.listen` and
`fono.confirm` (the spoken prompt), `fono.summarize`, and the
`fono summarize` CLI — resolves a voice for the calling program.

Voices are addressed by **positional gendered labels** — `Female 1`,
`Male 2` — never the cryptic, backend-specific ids (`alloy`, an
ElevenLabs/Cartesia UUID, `af_heart`). Each TTS backend exposes a short
curated palette; the label is just a stable position *within a gender*,
so an existing voice (e.g. Kokoro's `af_heart`, or the new male English
voices `am_michael` / `bm_lewis`) keeps its intrinsic name and is merely
*addressed* by label. Run `fono voices list` to see the active backend's
palette with each label, its intrinsic id, and gender.

```toml
[mcp]
# Optional global gender preference that filters automatic assignment.
# "male", "female", or "" / "any" (no preference). Manual pins and an
# explicit per-call voice always bypass this filter.
voice_gender = ""

# Give every unpinned program a stable, automatically-assigned voice
# (the program name is hashed onto the gender-filtered palette, so a
# program keeps the same voice across restarts). Default true.
auto_assign_voices = true

# Manual per-program pins. The key is the program identity — the MCP
# clientInfo.name, or the notification source_app for fono.summarize.
# The value is a positional label, the literal "auto", or a raw
# backend voice id. Manage these with `fono voices set/unset` rather
# than editing by hand.
# [mcp.voices]
# "chat-cli" = "female 1"
# "coding-agent" = "male 2"
```

Resolution precedence, highest first:

1. An explicit per-call `voice` argument (the tool arg / `--voice`).
2. A manual `[mcp.voices]` pin for the program.
3. Automatic stable assignment (when `auto_assign_voices = true`).
4. The backend default voice.

A pin that names a slot the active backend doesn't have (e.g. after you
switch backends) degrades gracefully to automatic assignment rather than
erroring. Manage everything through the guided CLI:

```console
$ fono voices list                      # palette + pins + preferences
$ fono voices set chat-cli "female 1"   # pin a program to a voice
$ fono voices preview "male 2"          # audition a voice
$ fono voices gender female             # global gender preference
$ fono voices unset chat-cli            # revert a program to automatic
```

Per-call voice override is honoured by all cloud backends and by the
local on-device backend. Note the local English palette ships female
voices plus two males (`am_michael` US, `bm_lewis` UK); other languages
expose whatever the on-device catalog provides.

#### Discovering more voices

A cloud backend's curated palette is short by design. When a provider
exposes an enumerable voice catalogue, `fono voices discover` probes it
and caches a refreshed, gender-labelled palette (capped to a short
list) for the active backend:

```console
$ fono voices discover            # refresh the active backend's palette
$ fono voices discover --json     # machine-readable output
$ fono voices list                # the discovered palette is now active
```

This is **fail-safe**: discovery runs only when you ask for it, the
result is cached under `~/.cache/fono/voices/discovered/<backend>.json`,
and *any* failure (no network, rejected key, a provider with no voice
list, malformed response) leaves the current palette untouched — it is
never on the speech path. A backend with no discoverable catalogue (e.g.
OpenAI, whose voice set is fixed) simply reports there is nothing to do.

Discovery also refreshes **automatically**, still fail-safe and never on
the speech path:

- **At daemon start** — a single non-blocking probe of the active cloud
  backend runs in the background (default ~10s timeout). Startup is never
  delayed; on failure the cache is left as-is.
- **On `fono voices list`** — if the cache is missing or older than 24h,
  a short (~4s) refresh runs before listing, then falls back to the
  curated / cached palette on any error. A fresh cache (<24h) is used
  as-is so listing stays instant.

Set `voice_discovery = false` to disable both the cache lookup and the
automatic refreshes.

Discovery is declarative: each provider entry in the catalogue carries
an optional descriptor (list URL, key auth, and how to read each voice's
id and gender from the JSON), modelled on the existing API-key
validation metadata — so onboarding a new provider is data, not code.
ElevenLabs (`/v1/voices`) and Cartesia (`/voices`) ship descriptors
today.

```toml
[tts]
# Consult the cached discovered palette for the active cloud backend and
# refresh it automatically (background probe at daemon start; lazy >24h
# refresh on `fono voices list`). Default true. Set false to use only the
# curated catalogue palette and disable all automatic discovery. Reads are
# best-effort; a missing or unreadable cache silently falls back to the
# curated list.
voice_discovery = true
```

## Hotkeys

```toml
[hotkeys]
dictation = "F7"                    # short tap = toggle; long hold = PTT
cancel    = "Escape"
assistant = "F8"                    # empty disables the assistant hotkey
```

Accelerator syntax: `Mod+Key`. Modifiers are `Ctrl` / `Alt` / `Shift` /
`Super` (or `Meta`); keys are letter / digit / function-key / named
key names (`Space`, `Tab`, `Return`, `Pause`, `ScrollLock`, `Insert`,
`Delete`). Examples: `Ctrl+Alt+Space`, `Super+grave`, `Mod4+space`,
`F11`, `Pause`. The dictation key is a soft modal: a short tap (under
~1 s) toggles capture, a longer hold runs push-to-talk. The cancel key
stops a recording in flight or shuts up an in-progress assistant reply.
Leave `assistant` empty to disable the F8 hotkey.

## `[wakeword]` — always-on wake word

An optional always-on wake word: while Fono is idle it listens for a
fixed phrase and, on a confirmed match, starts dictation or the assistant
through the **same** path as the physical hotkey. It is **disabled by
default** — with `enabled = false` no idle capture stream is ever opened
and behaviour is identical to today.

```toml
[wakeword]
enabled       = true        # master switch; false (default) opens no mic
refractory_ms = 800         # ignore further fires this long after one fires

# One block per active phrase. Multiple phrases share one backbone, so
# extra phrases are nearly free.
[[wakeword.phrases]]
model       = "hey_fono"    # registry model id (see providers.md)
sensitivity = 0.5           # 0..=1; higher = fewer false accepts
target      = "dictation"   # "dictation" or "assistant"

# Optional Wyoming CLIENT integration (opt-in; see below and
# home-assistant.md). The privacy-preserving SERVER direction is
# automatic — just enable `[server.wyoming]` — and needs nothing here.
# [wakeword.wyoming]
# enabled = true            # only meaningful together with a `uri`
# uri     = "tcp://..."     # opt-in client direction (see warning)
```

**Phrases and the English-first limit.** Each phrase loads a fixed
classifier keyed by `model`; matching is tied to that phrase and is
**English-first** — it is wake-phrase detection, not free-form speech
recognition. The built-in default is the clean-licence **`hey_fono`**
("Hey Fono") model. The opt-in community phrases (`hey_jarvis`, `alexa`,
`hey_mycroft`) are **NonCommercial**; Fono shows their licence notice when
you pick one, then downloads — see
[providers.md → Wake-word models](providers.md#wake-word-models).

**Custom phrases.** A bespoke phrase is trained from Piper-synthetic
positive samples plus openly-licensed negatives; the training pipeline
that produces a clean-licence classifier is provided separately. A
custom phrase id that is not in the registry resolves to `<id>.ort` in the
wake-word model cache.

**No AEC while idle.** The idle listener reads the system **default**
microphone source with **no acoustic echo cancellation** — AEC only
helps reject Fono's *own* TTS, which is silent while idle, so it cannot
filter out ambient TV/music anyway.

**Wyoming serving is automatic.** Fono's wake word is served over the
LAN exactly like its STT and TTS: whenever `[server.wyoming]` is enabled
and this build can do wake detection, Fono advertises and serves its
**local** detector as a Wyoming wake `Detection` service — no extra
switch. Audio never leaves the machine; the server *is* the detector. On
a fresh install with no `[[wakeword.phrases]]` it serves the runtime
default model (currently **`hey_jarvis`** until the clean-licence
`hey_fono` artifact is published).

`[wakeword.wyoming]` therefore exists **only** for the opt-in **client**
direction: `enabled = true` **plus** a `uri` to an external
`wyoming-openwakeword` service, which delegates Fono's own activation to
that box.

> ⚠️ The client direction **streams idle microphone audio over the
> LAN**, breaking the "audio never leaves the machine while idle"
> guarantee. It is never a default and `fono doctor` prints a prominent
> warning when it is active.

Run `fono doctor` to see, at a glance, whether the wake word is enabled,
which detector backend would run (the openWakeWord ONNX detector if the
`wakeword-onnx` build feature is compiled in and the model files are
cached, otherwise the energy stub), each phrase's target and licence, the
default model's cache state, whether any configured phrase is a
NonCommercial community model, whether the wake service is served over
Wyoming, and whether the opt-in Wyoming client direction is active.

## Inject and clipboard

Text injection has no per-key knobs in `config.toml` — the backend is
auto-detected at startup and can be overridden per-session with the
`FONO_INJECT_BACKEND` environment variable (`enigo`, `wtype`, `ydotool`,
`xdotool`, `xtest`, `none`). See [inject.md](inject.md) for the
priority table.

The `[general].also_copy_to_clipboard` flag (default `true`) is a
belt-and-suspenders that keeps the clipboard populated even on
compositors where key injection silently fails (KDE Wayland with
`wtype`). Disable only if you have a specific reason.

## Secrets

`~/.config/fono/secrets.toml` is a flat key-value file:

```toml
GROQ_API_KEY      = "gsk_..."
OPENAI_API_KEY    = "sk-..."
ANTHROPIC_API_KEY = "sk-ant-..."
```

Manage it via the CLI instead of editing by hand:

```sh
fono keys add GROQ_API_KEY          # paste at the prompt
fono keys list                      # masked listing
fono keys check                     # reachability probe per key
fono keys remove OPENAI_API_KEY
```

The file refuses to load if it's world- or group-readable. Fono never
logs key values; the `fono.http` tracing target records masked
request-id metadata only.

You can also reference an environment variable instead of pasting the
key, by setting `api_key_ref` in `[stt.cloud]` / `[polish.cloud]` /
`[assistant.cloud]` / `[tts.cloud]` to an env-var name (e.g.
`"GROQ_API_KEY"`); the daemon reads `$GROQ_API_KEY` at request time and
nothing touches disk. Useful for systemd `EnvironmentFile=` setups.

## Personal vocabulary (`vocabulary.toml`)

Deterministic transcript correction: teach Fono once that a mishearing
should always come out as the canonical spelling, and every future
dictation is fixed before the text reaches the cursor — with any STT
engine, polish on or off, batch or live, including the word-by-word
streaming inject. No model call, no network, no config keys.

`~/.config/fono/vocabulary.toml` is a plain, hand-editable file:

```toml
[[vocabulary]]
from = ["phono", "phone oh"]   # mishearings (case-insensitive)
to   = "Fono"                  # canonical spelling always emitted
```

Or manage it via the CLI (or the browser settings page):

```sh
fono vocabulary add phono Fono        # add a correction
fono vocabulary add "phone oh" Fono   # multi-word mishearing
fono vocabulary list
fono vocabulary remove phono
```

Semantics (see ADR 0037 for the full contract):

- whole words / whole phrases only — “phonograph” is never touched;
- matching is case-insensitive, output is your exact `to` casing;
- multi-word phrases match across spaces or hyphens, never across
  sentence punctuation;
- longest match wins; substitutions run in a single pass and are
  idempotent (validated at load — an invalid file disables corrections
  with a warning in the log and in `fono doctor`, it never crashes).

The file is re-read at the start of each dictation, so edits take
effect immediately — no daemon restart. A missing or empty file is
simply a no-op. The file is never auto-deleted or auto-modified;
learning corrections stays opt-in.

## On-disk paths (XDG)

| Kind | Path |
|---|---|
| Config | `$XDG_CONFIG_HOME/fono/config.toml` (default `~/.config/fono/config.toml`) |
| Secrets | `$XDG_CONFIG_HOME/fono/secrets.toml` |
| Vocabulary | `$XDG_CONFIG_HOME/fono/vocabulary.toml` |
| Whisper models | `$XDG_CACHE_HOME/fono/models/whisper/` |
| Polish models | `$XDG_CACHE_HOME/fono/models/polish/` |
| History DB | `$XDG_DATA_HOME/fono/history.sqlite` |
| IPC socket + PID | `$XDG_STATE_HOME/fono/` |

Server mode uses `/etc/fono/`, `/var/lib/fono/`, `/var/cache/fono/`,
and `/run/fono/` instead. See [install.md](install.md).

## Versioning and migration

The top-level `version = N` field is bumped whenever the schema gains a
breaking change. The daemon refuses to load a config from a future
version; it loads older configs and migrates fields on the fly. Removed
fields (the 2026-05-22 simplification, for example) are silently
ignored — your old config keeps working, the dropped keys just no
longer do anything.
