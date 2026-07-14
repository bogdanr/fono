# Changelog

All notable changes to Fono are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.16.0] — 2026-07-14

### Added

- **Windows support (experimental).** Fono now runs on Windows: a single
  `fono.exe` alongside the Linux and macOS builds, with the notification-area
  tray icon and its menu, push-to-talk dictation and the voice assistant
  (F7 and F8 by default, Escape to cancel), text typed straight into your
  apps with a clipboard fallback, the floating recording overlay, focused-app
  awareness, local speech-to-text, local text polishing, local text-to-speech
  and wake word, and every cloud provider. Like the macOS build there is one
  download, not a CPU/GPU choice: it uses your graphics card to speed up
  transcription when a driver is present and quietly falls back to the
  processor when it isn't — and it starts fine on a fresh machine or virtual
  machine that has no graphics driver yet. `fono install` copies the app into
  your user folder and starts it at login with no administrator prompt,
  `fono uninstall` reverses that while keeping your settings and history, and
  `fono update` downloads, verifies, and swaps in the new version in place.
  This is an early port: most of it was built and exercised on a remote
  Windows machine rather than daily-driven, so expect rough edges and please
  file an issue with whatever you hit. Nothing changes for Linux or macOS
  users — their binary is byte-for-byte identical, with no new dependencies.

- **Glass Cortex overlay (opt-in, experimental).** A new recording-overlay
  style that shows a live view of the on-device AI while it works — the model
  thinking through a reply, then speaking it back. Pick it as your
  visualisation style to try it; it is off by default and only has anything
  to show when you are running a local (on-device) model. It is still rough
  on real replies and is due for a rework, so treat it as a preview.

### Fixed

- **An interrupted model download no longer leaves a broken model behind
  (all platforms).** If a model download was cut short — a dropped
  connection, the machine sleeping, or the app closing mid-download — Fono
  could keep the half-finished file and then fail on every later start with a
  "model corrupted or incomplete" error that never cleared on its own.
  Downloads now finish into a temporary file and are only moved into place
  once the whole file is present and its checksum verifies; a bad download is
  re-fetched automatically.

## [0.15.0] — 2026-07-04

### Added

- **macOS support (Apple Silicon, experimental).** Fono now runs on
  macOS: dictation, the voice assistant, local and cloud providers,
  a native menu-bar icon with the full menu, the on-screen recording
  indicator, global hotkeys (no special permission needed), text
  injection at the cursor, and local text-to-speech. Releases attach a
  single Metal-accelerated `aarch64-apple-darwin` binary — GPU
  transcription is ~4× faster than CPU at almost no size cost, so
  there is no separate CPU download. `fono install` sets everything up
  per-user (no sudo): an app bundle, start-at-login, and a guided
  one-time permissions flow — the microphone and Accessibility grants
  are keyed to a stable local signature, so they survive every
  `fono update` instead of breaking after each one. Caveat: the port
  was developed and verified on a headless remote Mac; on-screen
  behaviour (menu bar, overlay, permission prompts) has not yet been
  eyeballed on a physical display. If you have an Apple Silicon Mac,
  giving it a try and filing an issue with what you find — good or
  bad — would genuinely help; see `docs/build-macos.md` for what's
  been checked so far.

## [0.14.0] — 2026-07-02

All of Fono's settings are now editable from your browser, and live
transcript mode degrades gracefully when your STT backend can't stream.

### Added

- **Browser-based settings UI (`[server.web]`, default off).** Pick
  *Settings…* in the tray — or run `fono config web` — and Fono opens a
  searchable settings page in your browser: a nine-section accordion covering
  every option, with live value summaries on each section, an
  unsaved-changes bar that shows exactly what you edited before you save,
  press-to-set hotkey capture, provider card grids for the STT / polish /
  assistant / TTS backends, and dark/light themes. Saving applies
  immediately — the daemon hot-reloads, no restart. API keys are write-only
  through the page (you can set them, never read them back). The server
  binds to loopback only (`127.0.0.1:10808`) and takes an optional bearer
  token; it's plain embedded HTML/CSS/JS on the HTTP plumbing that shipped
  in 0.13.0, so it adds no new dependencies and no measurable binary size.

### Fixed

- **Live transcript mode with a non-streaming STT backend no longer breaks
  the overlay.** Live transcript preview needs a streaming-capable backend
  (local Whisper, Groq, or Deepgram). With any other backend (e.g. Gemini),
  the dictation hotkey falls back to the normal batch pipeline — but the
  overlay could get stuck on screen permanently, and nothing told you why
  there was no live text. Now the fallback session shows the standard audio
  visualisation for the whole recording, a one-time notification explains
  that the configured backend can't stream (and that your text is typed
  when you stop), your Transcript preference is restored automatically
  afterwards, and the overlay always clears when the session ends.

### Removed

- Three inert config keys: `[audio].sample_rate`, `[interactive].mode`, and
  `[interactive].quality_floor` — each was a reserved knob with only one
  implemented value. Existing config files keep working; the keys are
  simply ignored and dropped on the next save.

## [0.13.1] — 2026-07-02

A maintenance release on top of 0.13.0: smaller downloads and a security
update to a core dependency. No behaviour changes.

### Changed

- **Smaller binaries.** The shipped executables are trimmed further by hiding
  redundant static-archive exports and stripping debug info from the bundled
  GPU shaders, so downloads and on-disk size shrink with no change in
  functionality.

### Security

- Updated `anyhow` to 1.0.103, which fixes an upstream soundness issue in
  `Error::downcast_mut()`
  ([RUSTSEC-2026-0190](https://rustsec.org/advisories/RUSTSEC-2026-0190)).

## [0.13.0] — 2026-07-01

Fono can now share the AI model you already have configured with everything
else on your machine and LAN: turn on one switch and Fono answers requests
on a local, OpenAI- and Ollama-compatible HTTP API — so your editor, Open
WebUI, `llm`, LangChain, and Home Assistant's Ollama conversation agent can
all talk to Fono's model with no extra setup.

### Added

- **Serve your local model over a standard API (`[server.llm]`, default
  off).** Flip it on — from the config file or the tray (*Servers → Local
  LLM server*) — and Fono exposes whatever assistant you have configured on
  a local HTTP endpoint that speaks **both** the OpenAI
  (`/v1/chat/completions`, `/v1/models`) and Ollama-native (`/api/chat`,
  `/api/tags`) formats, on Ollama's usual port `11434` so existing clients
  connect unchanged. Editors, Open WebUI, `llm`, LangChain, and Home
  Assistant's Ollama conversation agent can use Fono as their model backend
  out of the box. It binds to loopback by default, takes an optional bearer
  token before you expose it on the LAN, hot-reloads from the tray without a
  restart, and follows a backend swap (`fono use assistant …`) on the next
  request. `fono doctor` reports whether it's running and which model it
  serves, and Fono advertises it over mDNS so other machines discover it.
  Design and rationale in
  [ADR 0036](docs/decisions/0036-local-llm-server-openai-ollama.md).
- **Full cloud fidelity via pass-through.** When the model you serve is an
  OpenAI-compatible cloud provider (OpenAI, Gemini, Groq, Cerebras,
  OpenRouter), Fono forwards the request straight to the provider — injecting
  your stored key on the way out and keeping it on the machine — and streams
  the reply back unchanged. Every model the provider offers, plus
  tool/function-calling, vision, and JSON mode, work exactly as if you called
  the provider directly, with nothing to configure. This is what lets Home
  Assistant drive smart-home devices through a cloud model behind Fono.
- **A one-line request log.** Run the daemon with `--debug` (or
  `FONO_LOG=fono::llm::server=debug`) and Fono prints one tidy line per
  request: the endpoint and status, whether it was served locally or passed
  through to a cloud provider, the model, timing (time-to-first-token and
  total), an output-token count and throughput where available, and which app
  made the call (from its `User-Agent`) so you can tell clients apart on a
  shared port. Prompt and reply content are never logged.

### Changed

- **Realtime assistants keep working over the API automatically.** If your
  assistant is a realtime speech-to-speech model (e.g. Gemini Live) that a
  text chat API can't expose, Fono serves the **same provider's fast text
  model** instead (for Gemini, `gemini-flash-lite-latest`), reusing the same
  key — so you keep Gemini Live for voice *and* get a cheap, fast, smart text
  model on the API at the same time, with zero extra configuration. An
  optional `[server.llm].model` pins any specific model.

## [0.12.0] — 2026-06-24

Hands-free wake-word activation: idle, listen for a spoken phrase, and
start dictation or the assistant with no key — locally, on the ONNX
runtime already in the binary, and auto-served over Wyoming for Home
Assistant.

### Added

- **Optional always-on wake-word activation (`[wakeword]`, default
  off).** Fono can idle and listen for a spoken wake phrase, then start
  dictation or the assistant on the same path the hotkey uses — no key,
  no hands. Detection runs locally on the ONNX runtime already in the
  binary via [openWakeWord](https://github.com/dscripka/openWakeWord),
  so it adds no new dependency and no measurable size, and your audio
  never leaves the machine while idle on the default path. The listener
  suspends during any active recording or assistant turn and resumes
  when Fono goes idle. Ships with a clean Apache-2.0 default phrase as
  the only enabled model, plus an opt-in community phrase catalog that
  is downloaded on demand, never bundled, and shows its NonCommercial
  license as a notice when you pick one. When the LAN Wyoming server is
  enabled, Fono automatically serves wake detection over it — exactly
  like it serves STT and TTS, with no extra switch — so Home Assistant
  discovers Fono as a drop-in wake-word provider and detection runs on the
  Fono box with audio staying on the machine. Opt-in and off by default,
  behind an explicit "idle mic audio leaves the machine over the LAN"
  warning, Fono can instead forward audio to an external
  `wyoming-openwakeword` service. `fono doctor` reports the wake-word
  configuration and that privacy warning. The clean-license `hey_fono`
  default model is not yet hosted, so the local always-on listener stays
  off until you enable it; the auto-served Wyoming path uses the community
  `hey_jarvis` model as a temporary fetchable default in the meantime.
  Engine and licensing rationale is in ADR 0012.

## [0.11.1] — 2026-06-22

Hands-free realtime conversation mode, and a leaner on-demand realtime
connection path.

- **Live conversation mode for the realtime assistant.** Tapping the
  assistant hotkey now opens a hands-free, back-and-forth spoken
  conversation: talk, listen to the reply, and just keep talking — no
  key press between turns, all over one session. The on-screen overlay
  shows whose turn it is and animates to the live audio — green while
  you speak, blue while the assistant does. The conversation ends on its
  own after a short silence or when you say you're done (or instantly on
  a second tap / Escape), so it never sits there running up cost. Holding
  the hotkey still works exactly as before: hold to talk, release to hear
  the full reply. (Talking over the assistant to interrupt it needs
  system echo cancellation and is coming next — see the roadmap.)
- **Removed the realtime startup prewarm.** The Gemini Live startup
  pre-connect shipped in 0.11.0 only warmed transient DNS/TCP/TLS/
  WebSocket caches that go stale within minutes, so it delivered no
  reliable latency gain once the daemon had been idle for a while — the
  common case — while leaving dead code on the realtime path. It has
  been removed; realtime sessions connect strictly on demand at first
  use. Push-to-talk behaviour is unchanged.

## [0.11.0] — 2026-06-18

A realtime voice assistant, one-key Google Gemini, and gapless cloud speech.
Fono can now hold a spoken conversation over the Gemini Live WebSocket — with
memory of earlier turns and an optional look at your screen — a single Gemini
API key drives speech-to-text, cleanup, the assistant, and text-to-speech end
to end, and cloud voices stream back gaplessly instead of arriving a sentence
at a time. Plus universal cloud-voice autodiscovery, per-program voices,
ElevenLabs and Speechmatics backends, two new male English Kokoro voices, and
turn traces you can actually read.

### Added

- **Realtime voice assistant over the Gemini Live WebSocket.** Alongside
  the existing staged pipeline (record → STT → chat → TTS), the assistant
  hotkey can now drive an end-to-end realtime path backed by the Gemini
  Live API: audio streams up as you speak and the spoken reply streams
  back over one WebSocket session. The session seeds the rolling
  conversation history so it remembers earlier turns instead of opening
  amnesiac each press, and — when vision is enabled and the backend is
  vision-capable — it sends a one-shot screenshot of the focused window
  before the mic audio so the model can see what you're looking at. The
  realtime turn is fed a live mic-frame stream rather than a finished
  buffer, the groundwork for upload overlapping the hold. Barge-in
  interrupts an in-flight reply. Selected via a realtime profile in the
  capability catalogue (ADR 0035); discoverable from the setup wizard and
  `fono doctor`.
- **Single-key Google Gemini provider.** One Gemini API key now drives the
  whole pipeline — speech-to-text, LLM cleanup, the staged assistant, and
  native Gemini text-to-speech. Default cloud models use the 3.x flash
  family via the `gemini-flash-latest` alias (and `flash-lite-latest` for
  low-latency turns) rather than pinned ids, with `reasoning_effort=low`
  to trim thinking latency. Surfaced automatically by the wizard, doctor,
  and CLI.
- **Gapless cloud text-to-speech.** Cloud voices now stream back over SSE
  and play continuously instead of arriving one sentence at a time, with a
  small fixed (~300 ms) prebuffer to smooth start-up. Time-to-first-audio
  is reported at the first PCM frame rather than at sentence end.
- **Universal, fail-safe cloud voice autodiscovery.** The short curated
  cloud voice palettes can now be expanded by probing a provider's live
  voice catalogue on demand, driven by a single declarative descriptor so
  providers are onboarded with data rather than code. The probe never runs
  on the speech path: the daemon fires one non-blocking background refresh
  of the active backend at start (~10 s timeout), `fono voices list` does a
  short lazy refresh when the cache is missing or older than 24 h, and a
  new `fono voices discover` refreshes on demand. Any error falls back to
  the curated/cached palette, capped to a deterministic, gender-balanced
  subset. ElevenLabs and Cartesia ship with discovery descriptors; toggle
  with `[tts].voice_discovery` (default on).
- **Per-program text-to-speech voices.** Different calling programs can
  speak in different, stable voices chosen from friendly positional
  gendered labels ("Female 1", "Male 2") instead of cryptic backend ids. A
  single shared resolver feeds every speech path (`speak`, `listen`,
  `confirm`, `summarize`, and the CLI); precedence runs explicit per-call
  voice → manual `[mcp.voices]` pin → stable automatic assignment → backend
  default, with a stale pin degrading to auto rather than erroring.
  Automatic assignment hashes the normalised program name onto the
  gender-filtered palette so a program keeps its voice across restarts.
  Managed with `fono voices` (list / set / unset / gender / preview) and
  the `[mcp]` keys `voices`, `voice_gender`, and `auto_assign_voices`.
- **ElevenLabs speech-to-text and text-to-speech.** ElevenLabs Scribe
  (STT) and Eleven v3 (TTS) are wired end to end — catalogue, config,
  setup wizard, doctor, and factories. The free tier works with a premade
  voice; the default is "Sarah".
- **Speechmatics speech-to-text and text-to-speech.** Speechmatics is
  now a first-class cloud backend for both directions. STT runs over
  the realtime WebSocket (`wss://eu.rt.speechmatics.com/v2`) as a
  one-shot round-trip — `StartRecognition`, buffered `AddAudio`
  frames, `EndOfStream`, then collect the `AddTranscript` finals —
  reusing the same `tokio-tungstenite` dependency the Deepgram
  streaming path already pulls in, so no new crates and no `deny.toml`
  churn. TTS uses the preview REST endpoint
  (`https://preview.tts.speechmatics.com/generate/<voice>`) returning
  16 kHz signed-16-bit PCM. Both surfaces authenticate with
  `Authorization: Bearer <SPEECHMATICS_API_KEY>` (pinned by a unit
  test so it can't regress to Deepgram's `Token` form). The TTS
  preview is English-only with four voices (`sarah`, `theo`, `megan`,
  `jack`, default `sarah`). Enable via `backend = "speechmatics"` in
  `[stt]` / `[tts]`; the setup wizard, `fono doctor`, and the CLI
  surface it automatically.
- **Automatic local fallback for English-only cloud TTS voices.** Some
  cloud voices only render intelligible English (Groq's Orpheus
  `…-english`, the Speechmatics TTS preview, Deepgram's `aura-2-…-en`
  voices). Feeding them non-English text produced an English
  phonemization of foreign words — gibberish, not speech in that
  language. The capability catalogue now carries a single
  `english_only` boolean per TTS provider (default `false`, so a new
  provider fails safe as multilingual). When the active backend is
  flagged English-only and an utterance is reliably non-English, Fono
  transparently routes that one utterance to the local multilingual
  Piper voice for its language (downloaded + cached on first use)
  instead of the cloud backend; English or inconclusive text still
  goes to the cloud voice unchanged, so the common path is untouched.
  Language is taken from the known signal where it exists and otherwise
  detected with the already-bundled `whatlang` trigram classifier
  (no model files, sub-millisecond, run only for English-only
  backends). When the local engine is unavailable (the `tts-local`
  feature is off, or no catalogue voice exists for that language), the
  utterance is skipped with a single warning rather than spoken as
  gibberish. No new configuration keys.
- **Two male English Kokoro voices.** `am_michael` (en-US) and
  `bm_lewis` (en-GB) join the local catalogue, closing the all-female
  English gap. Both reuse the existing `kokoro-v1.0-q8f16.ort` model
  with byte-identical upstream style packs.
- **Readable turn traces.** The Perfetto turn-trace output is now a
  legible top-to-bottom waterfall: capture and playback lanes are
  emitted, lanes are named and ordered via process/thread metadata (no
  more hashed `stt 46340` ids in arbitrary order), events are written
  chronologically, and the post-TTS drain span is always recorded so
  the tail is never unexplained whitespace. All trace work is one-shot
  at turn end — no hot-path cost.
- **Richer MCP tool logs.** Each MCP tool call now emits a single
  completion-time line carrying backend, voice, client, outcome, and a
  per-step latency breakdown (synth / playback / capture / STT /
  relevance, as applicable for `speak` / `listen` / `confirm` /
  `summarize`), replacing the previous pre-call debug breadcrumb. No
  transcript or summary bodies are logged — only lengths.

### Changed

- **Setup wizard provider lists and key validation are now catalogue-
  driven.** The cloud speech-to-text picker, the cloud LLM/cleanup
  picker, and the API-key reachability check are generated directly
  from the capability catalogue (`CLOUD_PROVIDERS`) instead of
  hand-maintained menus and per-provider probe arms. A new provider
  now surfaces in every wizard list — and validates its key — with no
  edits to the wizard. The primary "one key fills everything" matrix
  also widened: it now lists every cloud provider with at least one
  wired capability (including speech-only ones like Speechmatics,
  Deepgram, AssemblyAI, and Cartesia), and any capability the chosen
  provider doesn't cover transparently leans on the local backend
  (local Whisper, embedded GGUF cleanup, on-device TTS).
- **Default cloud models moved to the Gemini 3.x flash family.** New
  Gemini configs use the `gemini-flash-latest` alias for STT/LLM and
  `flash-lite-latest` for low-latency turns, rather than pinned model
  ids.

### Fixed

- **0.11.0 release artefacts fit the size gate again without dropping
  features.** The `release-slim` build now disables unused Rust/native
  unwind-table emission while keeping C++ exceptions and OpenMP, and the
  strict CPU budget moves to 27 MiB — still below the accepted 32 MiB cap —
  to cover the realtime/provider growth measured for this release.
- **Barge-in now works while the assistant is still thinking.**
  Pressing the assistant hotkey during an in-flight reply — whether it
  is thinking or already speaking — stops the current reply and starts
  a fresh recording in one atomic step, with conversation history
  preserved. Previously a re-press while merely thinking was dropped,
  and the speaking-state restart could race itself back to idle,
  stranding the capture with no overlay.
- **First realtime turn no longer pays the full handshake latency.**
  The Gemini Live client now warms DNS, TCP, TLS, and the WebSocket
  upgrade off the hot path, so the first assistant press does not wait
  on the whole connection setup. The prewarm opens and immediately
  closes the upgrade connection without sending a setup message, so no
  model turn starts and no quota is consumed.
- **Kokoro local voices failed to load with a "Greater(13) node"
  error** on Linux, macOS, and Windows. The bundled ONNX Runtime is
  rebuilt from the complete operator set covering every shipped voice
  (Piper + Kokoro), with a guard so the operator set can never silently
  regress to a partial build again. Logs now show which engine actually
  spoke (Piper vs Kokoro) and which provider handled each synthesis.
- **Payment-required (HTTP 402) cloud failures are now visible.** They
  surface as a desktop notification with actionable advice instead of
  failing silently.
- **Three-letter language codes from cloud transcripts** (e.g. `ron`,
  `eng`) are normalised to two-letter codes, fixing spurious
  "banned language" warnings and wrong downstream voice hints.

## [0.10.0] — 2026-06-12

Faster local AI, local text-to-speech out of the box, and cleanup that types
as it thinks. The embedded engine now reuses prompt checkpoints so warm
dictations and follow-up assistant turns stay quick, offline Piper/Kokoro TTS
ships in the default binary, and local AI cleanup streams into the cursor
word-by-word instead of making you wait for the whole pass. Plus the usual
round of fixes — Gemma cleanup no longer loops, and history/log files are
locked down on shared machines.

### Added

- **Local AI cleanup now types as it thinks.** When cleanup runs on the
  embedded local model, the cleaned text streams into the cursor word by
  word as the model decodes it, instead of making you wait for the whole
  pass before anything appears. On a long dictation the first words land in
  about one to three seconds rather than after the full seven-to-twenty-second
  decode, then keep flowing continuously. All of the existing safety checks
  still run on the first sentence before a single character is typed — a
  clarifying-question, degenerate, or wrongly-translated cleanup still falls
  back to your raw transcript with nothing typed. It applies only to the
  local backend (cloud cleanup is already sub-second and one-shot) and turns
  itself off automatically for short utterances and clipboard-fallback
  sessions. On by default; set `[polish].stream_injection = false` to keep
  the old wait-for-the-whole-thing behaviour.
- **Local AI got noticeably snappier on repeat use.** The embedded
  llama.cpp engine now keeps reusable checkpoints of the prompts it has
  already processed (the cleanup instructions, the assistant's system
  prompt, your running conversation), so warm dictations and follow-up
  assistant turns skip re-crunching all of that and only process what's
  new. Time-to-first-token stays flat as a conversation grows instead of
  climbing with every turn; restoring a saved checkpoint takes tens of
  milliseconds where a cold re-read of a long conversation took seconds.
  Cleanup also gets a per-app checkpoint, so dictating into your editor,
  browser, or terminal each reuses its own warmed-up state. The how and the
  measured numbers are written up in
  [Making local LLM fast](https://bogdan.nimblex.net/programming/2026/06/10/making-local-llm-fast.html).
- English dictation read-back now uses **Kokoro**, a higher-quality local
  voice, while every other language keeps using Piper. Four English voices
  ship — `af_heart` (the default), `af_bella`, `af_nicole` (American) and
  `bf_emma` (British) — all sharing one model, so picking a different one is
  a tiny download. The model and voices fetch on demand like every other
  voice, and the binary's runtime dependency set is unchanged (the
  statically linked runtime grows ~0.8 MiB, well under the size budget).
- Local text-to-speech is now built into the shipped binary by default. The
  `cpu` and `gpu` downloads do offline Piper TTS out of the box (42 voices
  across 38 languages); pick it with `fono use tts local`. The statically
  linked ONNX Runtime adds ~2 MiB and keeps the binary's runtime dependency
  set at the same four-entry glibc allowlist (measured 24.57 MiB, `cpu`).

### Fixed

- **Local cleanup with a Gemma model no longer loops or runs away.** Three
  compounding bugs made the embedded engine repeat a (correctly cleaned)
  sentence until a token cap — turning a 1-second cleanup into a 25-second
  one that injected garbage. The engine now renders Gemma's own prompt
  format instead of assuming ChatML, stops generation on any
  control/end-of-turn token (model-agnostic, so models with non-standard
  vocabularies stop correctly too), and applies a repetition penalty to its
  own output so near-echo cleanups can't lock into a verbatim loop.
- **Privacy hardening on shared machines.** The transcription history
  database (`history.sqlite`) is now clamped to owner-only permissions
  (0600) every time it is opened — it holds everything you have ever
  dictated and was previously created with the default umask (typically
  world-readable). And `/var/log/fono.log` is no longer created
  world-writable (0666): `fono install` now creates it owned by the
  installing user with mode 0600, so other local users can neither read
  usage details (focused-window classes/titles) nor poison/truncate the
  log. Fono processes run by other users fall back to `/dev/null` for
  logging, as before.
- Local LLM cleanup (`[polish].backend = "local"`) now runs the **embedded**
  `llama-cpp-2` engine on a local GGUF, as intended — it no longer silently
  routes any Gemma-named model to an Ollama HTTP server. The old behaviour
  meant the default local setup (model `gemma-4-e2b`) POSTed to
  `http://localhost:11434`, 404'd on a model Ollama didn't have, and fell back
  to injecting the raw transcript with no notification — so cleanup appeared to
  do nothing. The factory's `is_gemma_model` special-case has been removed; a
  missing local GGUF now fails loudly with a one-shot notification pointing at
  `fono models install <model>`, and the setup wizard's "local polish" choice
  no longer writes a stale Ollama `[polish.cloud]` block. An Ollama /
  OpenAI-compatible server is reachable only via the explicit
  `backend = "ollama"` config. As a consequence of routing polish through the
  embedded engine, both the cleanup and voice-assistant paths now share a
  single process-wide `LlamaBackend` (in `fono-core`); previously each crate
  initialised llama.cpp independently, and once polish also went embedded the
  second initialiser panicked — surfacing as `llama-local mutex poisoned` on
  whichever path ran second (typically the assistant stream after a dictation
  turn).
- Local TTS now speaks each reply in the matching voice. Previously a bilingual
  user heard every reply in their primary language's voice — a Romanian reply
  read aloud by the English voice (wrong phonemes, wrong accent). The local
  backend now picks the voice per utterance from the language of the text it is
  about to speak, identified against the configured `general.languages`, and
  loads each language's voice on demand. Identifying the language from the text
  itself (rather than from the speech recogniser's detected *input* language)
  is what makes a Romanian answer to an English question come out in the
  Romanian voice. The speech-recogniser language is used only as a fallback for
  text too short to fingerprint. An explicit `[tts.local].voice` still pins one
  voice for everything.
- Local TTS for the US English voice (`en_US-amy-medium`) no longer fails
  phonemization. Its `.onnx.json` declares espeak voice `en-us`, which had no
  dictionary in the catalog; it now folds onto the shared `en` base/`en_dict`
  (same as the British `en-gb-x-rp` voice), so the on-demand dictionary
  download resolves instead of warning.
- Local TTS no longer mangles Romanian words containing the comma-below letters
  `ș`/`ț`. The vendored pure-Rust espeak-ng port truncated a word at the first
  such letter (`Ploiești` came out as "Ploie") or dropped it entirely (`țara`
  was silent); these comma-below codepoints are now folded onto their cedilla
  equivalents (`ş`/`ţ`) before phonemization, as the upstream C library does
  internally.

### Changed

- **Hands-free recording stops a little sooner.** The default auto-stop
  silence window dropped from 5 s to 3 s, so dictation commits faster
  after you stop talking. Set `[audio].auto_stop_silence_ms` to keep a
  longer pause budget.

## [0.9.1] — 2026-05-29

Show your screen, dictate in any language. This release teaches the voice
assistant and your coding agents to *look* at what you're pointing at, fixes
AI cleanup so it stops dropping text and accents on non-English dictation, and
adds a few new looks for the recording overlay.

### Added

- **Point at your screen and ask.** The F8 voice assistant and any
  connected coding agent can now see your screen when you reference
  something on it — "what does this error mean?", "read this dialog to
  me". Fono grabs the focused window automatically, or opens your
  desktop's region picker so you can frame exactly what to share, then
  hands the picture to the model. Private windows (KeePassXC, Bitwarden,
  1Password) are never captured. Works out of the box with whatever
  screenshot tool you already have (scrot, grim, maim, spectacle,
  gnome-screenshot, …) — no new required dependencies. `fono doctor`
  shows whether capture is ready.
- **New looks for the recording overlay.** Three fresh visualisation
  styles join the picker: **Aurora Beziers** (Siri-style glowing
  ribbons), **System/360** (a retro mainframe console-lamp spectrum),
  and **Terrain 3D** (your voice as a flowing 3D landscape). Pick one
  from the tray's Visualization menu.

### Changed

- **The voice assistant is on by default.** The pipeline that powers F8
  and the coding-agent voice loop now works without extra setup. If you
  had explicitly turned it off, that choice is respected.
- **Voice mode talks more naturally.** The built-in voice preset for
  coding agents was rewritten: agents now listen by default, only ask
  bounded A/B/C questions when it actually helps, never ask you to
  approve risky actions by voice, and open each spoken turn with a short
  cue so you have a moment to refocus before the answer.

### Fixed

- **Dictation cleanup no longer drops your words — or your accents.**
  On non-English dictation, the AI cleanup step could silently come back
  empty and inject the raw, unpolished transcript instead; diacritics
  (ă, î, ș, ț, é, ñ, …) could also get lost on the way to the cursor.
  Both are fixed: cleanup now reliably tidies up non-English text and
  restores the correct accented characters. When a coding agent is in
  focus in a terminal, dictation is framed as prose (capitalisation and
  punctuation) rather than shell commands.
- **The assistant now actually answers about your screen.** Previously
  it captured the screen but spoke a placeholder instead of describing
  what it saw. It now sends the image to the model and reads back the
  real answer.
- **Escape reliably cancels while the agent is listening,** and Ctrl-C
  restores the tray icon cleanly when you stop a voice session.

## [0.9.0] — 2026-05-26

Talk to your coding agent. The headline feature is an early-preview voice
loop that lets any MCP-capable coding agent — Forge, Claude Code, Cursor,
Codex CLI, Gemini CLI, and others — speak and listen through Fono. Plus a
Debian/Ubuntu install fix so the on-screen overlay shows up on first run
instead of after a manual restart.

### Added

- **Talk to your coding agent (early preview).** Fono now ships an
  MCP server that lets any MCP-capable coding agent — Forge, Claude
  Code, Cursor, Codex CLI, Gemini CLI, and others — drive a voice
  loop through three tools: `fono.speak` (the agent speaks a reply),
  `fono.listen` (the agent asks a free-form question and gets your
  spoken answer back as text), and `fono.confirm` (the agent offers
  A/B/C choices and matches your spoken pick). Verified end-to-end
  against Forge and Claude Code; best-effort for the rest. Disabled
  by default, opt in with `fono use mcp-server on`. This is an
  **early preview** — the protocol, defaults, and tool surface may
  still shift before the feature graduates.
- **One-command setup for your coding agent.**
  `fono agent-setup <name>` wires everything in one shot: enables
  the MCP server, merges the right `mcpServers.fono` entry into
  your agent's MCP config, and appends the shared voice-mode preset
  to your project's `AGENTS.md` / `CLAUDE.md`. Idempotent, supports
  `--dry-run`, and `--list` shows every registered agent. After
  setup, launch your agent the normal way and it can speak and
  listen.
- **Voice-friendly overlay while the agent is talking with you.**
  When the agent calls `fono.listen` the same overlay you see for
  F7 dictation pops up — waveform/transcript while you speak, a
  PONDERING animation while it waits — so you always know whether
  Fono is listening. The overlay is scoped strictly to the
  microphone-open phase (not while the agent is speaking its
  prompt) and is skipped when a regular Fono daemon is already
  running so a daemon-paired environment never double-paints.
- **Background-speech filter.** When the agent asks a question,
  Fono now filters out chatter that doesn't look like an answer —
  radio, TV, a side conversation in the room, or the agent's own
  prompt echoing back through the speakers. Tunable via
  `[mcp].relevance_filter` (`"off" | "heuristic" | "llm"`, default
  `"heuristic"`) and `[mcp].relevance_max_rejections` (default
  `2`). The optional `"llm"` mode uses the configured polish
  backend as a one-shot classifier with a 1.5 s timeout; on
  timeout or parse failure it fails open and accepts the
  utterance. Each rejection flashes a dim grey `Ignoring` badge in
  the overlay so you can see that Fono heard you but is still
  waiting for a real answer.
- **Tray icon turns amber while the agent is in a voice turn.**
  Same colour Fono uses while STT or polish is running, so you can
  tell at a glance that a `fono.listen` / `fono.speak` /
  `fono.confirm` call is in flight. The previous tray state is
  restored when the call ends; nested spans (a prompt that speaks
  before listening) keep the icon steady. No configuration needed.
- **`fono speak --stream`** — reads stdin line by line, segments
  into sentences, strips markdown, and speaks each sentence
  through the configured TTS backend. Backpressure prevents a
  fast producer from outrunning playback; SIGINT flushes cleanly.
  Pipe-friendly: `echo "Hello. World." | fono speak --stream`.
- **`fono use mcp-server on|off`** — toggle `[mcp.server].enabled`
  without editing config by hand.
- **`fono doctor` "Coding agents" section** — reports whether the
  MCP server is enabled, the last MCP handshake timestamp, the
  advertised tools, and the last tool-call result.
- **Tray "MCP server" submenu** (visible only when enabled) —
  enable/disable toggle, last-connected timestamp, per-tool
  enable/disable rows.
- **Docs.** New `docs/coding-agents.md` integration guide covering
  Forge, Claude Code and Cursor (verified) plus Codex CLI, Gemini
  CLI, Cline, Continue, Windsurf and Goose (best-effort), with an
  "Adding your own agent" section. Shared voice-mode preset at
  `assets/agent-presets/voice.md`.
- **ADR 0030** — records the Fono-as-MCP-server decision, the
  three-tool surface, the agent-agnostic design principle, and the
  `agents.toml` registry.

### Changed

- **MCP listen default silence is now 10 s** (was 2 s) so an
  agent turn can pause for thought without being cut off
  mid-sentence.
- **MCP listen default `max_seconds` lowered from 60 s to 45 s** —
  combined with the multi-utterance relevance loop this gives a
  responsive turn-taking budget without stranding you.

### Fixed

- **Overlay now appears on first run on Debian/Ubuntu desktops.**
  Installing via `curl https://fono.page/install | sh` silently
  skipped the post-install prompt that offers to add
  `libxkbcommon-x11` and `xdotool`, so the on-screen recording
  overlay fell back to `noop` and only appeared after a manual
  restart. The prompt now reads from `/dev/tty` directly so it
  survives `curl|sh` + `sudo` PTY allocation, and the background
  daemon spawn also reconstructs `DISPLAY` and `XAUTHORITY` when
  sudo strips them. Server installs are unaffected — they never
  pull X11 libraries.

## [0.8.2] — 2026-05-26

Context-aware dictation, Esc-to-cancel on Wayland, and smarter
first-run model picks. Fono now reads which window is focused when you
press the hotkey and silently adjusts how it transcribes and cleans up
your speech — terminal windows get a shell-vocabulary bias so `ls`,
`git commit`, and `chmod 755` come out right; code editors get a
language-specific vocabulary hint. No configuration required.

### Added

- **Window-aware context injection.** At hotkey-press time Fono reads
  the focused window class and title and silently passes a tailored
  `initial_prompt` to Whisper (local and cloud) and a matching cleanup
  suffix to the LLM. Built-in profiles cover terminal emulators (shell
  vocabulary: `ls -la`, `grep -r`, `chmod 755`, `git commit`, etc.),
  per-language code editors (Cursor, Zed, Kate; hints derived from
  the file extension in the window title), and private windows
  (KeePassXC, Bitwarden — history writes suppressed). Existing
  `[[context_rules]]` entries take precedence as usual. Detection
  covers X11, sway, Hyprland, and GNOME Wayland (with XWayland
  fallback for GNOME 46+ where the Shell introspect API is restricted).
  Visible at any time with `FONO_LOG=fono::context=debug`.
- **Terminal project and agent detection.** When a terminal emulator is
  focused, Fono walks `/proc` to find the CWD and detects the active
  project type (Rust, Python, Node, Go, Docker, K8s) and whether a
  coding agent (Forge, Claude Code, Codex, Aider, Goose, and others)
  is running. Project type refines the Whisper vocabulary hint; agent
  detection is stored for future prompt biasing and is currently no-op.
- **Escape cancels recordings on Wayland.** The portal hotkey
  backend (KDE / sway / Hyprland) opens a transient second
  `GlobalShortcuts` session while a recording is active; the
  GNOME-Wayland gsettings shim writes a temporary `fono-cancel`
  custom-keybinding for the same duration. Either way Esc is only
  grabbed while Fono needs it.
- **`fono cancel` CLI verb.** Aborts an active recording or
  assistant turn. Idempotent. Backs `Request::Cancel` and the
  Esc grab above.
- **Native aarch64 release binary.** `fono-vX.Y.Z-aarch64` is now
  built on a hosted `ubuntu-22.04-arm` runner alongside the x86_64
  builds and is gated by the same size-budget check (same glibc 2.35
  floor; verified end-to-end on a Debian 13 aarch64 host).

### Fixed

- **Dictation on PipeWire-only Linux hosts.** On stock Ubuntu 24.04
  (and similar systems without `pulseaudio-utils` installed) the
  `pw-cat` capture helper was missing `--raw` and emitted a
  containerized stream that Fono interpreted as PCM — recordings
  came out as noise. The capture path now passes `--raw` and
  produces clean audio on every PipeWire setup.
- **Wizard recommendation accuracy on older iGPUs.** The picker no
  longer credits CPU-only builds with a GPU multiplier they can't
  deliver, and Vulkan-capable integrated GPUs are split into two
  classes (`Integrated` at 1.3× for fp16-only parts like UHD 620,
  `IntegratedTensor` at 2.0× for fp16 + `VK_KHR_cooperative_matrix`
  parts like Lunar Lake Xe2 and Apple Silicon). The `small.en`
  registry anchor was also off by 2× — fixed against the matrix.
  Net effect: older laptops are recommended `small` or `small.en`
  instead of a turbo model that can't keep up; modern tensor-iGPU
  laptops correctly get `large-v3-turbo`. `fono doctor` now uses
  the same affordability walk as the wizard so the two never
  disagree.

### Changed

- **Wizard model selection is now data-driven.** A new three-class
  `HostGpu` classifier (`None` / `Integrated` / `Discrete`), derived
  from the Vulkan probe's `deviceType` + `shaderFloat16` bit, replaces
  the previous static `accelerated()` 4× heuristic with multipliers
  `1× / 2× / 4×` (no PCI tables, no runtime calibration, no maintained
  device lists; see ADR 0028). The live-mode `Borderline` affordability
  middle state and the two `LIVE_REALTIME_MIN_*` constants are gone —
  the wizard now applies a single `BATCH_REALTIME_MIN = 2.0` gate and
  every shortlist entry is comfortable by construction. Quantization
  defaults are unified on `q8_0` across the registry (per the ADR 0027
  2026-05-25 amendment): `tiny`, `tiny.en`, `small`, `small.en`, and
  `large-v3-turbo` all default to `q8_0`; the previous `q5_1` defaults
  remain reachable via `[stt.local].quantization`. `wer_by_lang`
  English numbers are refreshed to Open-ASR-Leaderboard means (rounded
  up: `tiny` 12→16, `tiny.en` 9→13, `small` 6→10, `small.en` 5→9,
  `large-v3-turbo` 4→8) so the accuracy buckets the wizard surfaces
  match the public prior users will find elsewhere.

- **Assistant history now survives dictation pivots.** Previously a
  press of the dictation hotkey (F7) while an assistant conversation
  was in flight would wipe the rolling chat history; the next F8
  turn would start fresh and the assistant would tell you it had no
  memory of what you'd just said. Dictation and the assistant have
  always had fully separate histories — the dictation transcript log
  lives in SQLite, the assistant's chat turns live in memory — so
  the cross-wired auto-clear was surprising rather than protective.
  The pivot still stops any in-flight assistant playback so it
  doesn't talk over your dictation, but the chat history is
  preserved and you can resume the conversation on the next F8.

### Removed

- **`fono assistant stop` CLI verb** — use `fono cancel` instead.
- **"Stop assistant" tray entry** — redundant with `fono cancel`.
- **`[assistant].auto_clear_on_dictation` config key.** No longer
  read; remove it from your `config.toml` if present (unknown keys
  are silently ignored, so existing configs keep working). The
  remaining knobs (`history_window_minutes`, `history_max_turns`)
  and the tray "Forget conversation" entry / `fono assistant
  forget` CLI still cover every legitimate need to drop history.

## [0.8.1] — 2026-05-23

A quality-of-life release: two more cloud providers, polish on the
"Pondering" pause UI, headless servers install themselves, and a handful
of papercuts gone.

### Added

- **Deepgram speech-to-text now actually works.** Picking Deepgram in
  `fono setup` (or running `fono use stt deepgram`) had been broken
  since v0.8.0 — it offered the option but failed at startup. The full
  pipeline is now wired: both the batch endpoint and a real WebSocket
  for live dictation, with the newer **Nova-3** model as the default
  (Nova-2 is still selectable for languages Nova-3 doesn't cover yet).
- **Cartesia speech-to-text.** Same story — was advertised, now
  delivered. Batch transcription via the `ink-whisper` family;
  realtime `ink-2` will follow in a future release.
- **Cartesia text-to-speech now picks a native voice per language.**
  Speak Romanian, hear a Romanian voice; switch to English in the
  same session, hear an English voice. No more one-voice-fits-all.
- **Auto-stop on silence is now wired end-to-end.** If you enable
  "Auto-stop after pause" in the tray, dictation actually stops once
  you've been quiet for the configured time — previously the
  PONDERING label appeared but nothing committed.
- **`sudo fono install` is friendlier on servers.** Headless boxes
  (no graphical session, multi-user systemd target) are now detected
  and the systemd lane runs by default — no `--server` flag needed.
  A new `--desktop` flag forces the desktop lane on hosts that just
  *look* headless.
- **Server installs auto-enable LAN sharing.** `fono install --server`
  now turns on the Wyoming STT listener on port 10300 out of the box,
  probes that it actually bound, and prints the address so other
  machines on the LAN can discover it immediately. `fono uninstall`
  on a server also cleans up `/var/cache/fono` (multi-GB model
  blobs).
- **A diagnostic VU bar.** `[overlay].volume_bar = "advanced"` paints
  a dBFS-axis meter with reference ticks for your speaking level and
  the silence threshold — useful for tuning auto-stop without
  guesswork. The default simple bar is unchanged.

### Changed

- **The "PONDERING" pause indicator is consistent everywhere.**
  - It now shows up on the assistant flow (F8) too, in the green
    assistant palette, with the same auto-stop behaviour as
    dictation.
  - It only appears when you've actually enabled auto-stop — no
    more PONDERING under your finger if you've opted out.
  - It works in live (streaming) dictation, not just batch.
  - It doesn't flicker on a single breath, chair creak, or mouse
    click during a real pause.
- **Tray "Auto-stop after pause" presets** reworked from
  `Off / 0.8 s / 1.5 s / 3 s` (chat-app numbers) to `Off / 3 s / 5 s`
  (prose-dictation numbers). Default stays Off.
- **Tray "Visualization" picker** now turns the VU bar on automatically
  for the Transcript style and off for the others — sensible default,
  still overridable from `config.toml`.
- **`fono hwprobe` matches what the setup wizard actually picks.**
  The recommendation table used to promise `large-v3-turbo` on
  CPU-only boxes that the wizard would then quietly downgrade. Now
  the report and the wizard agree.
- **Hotkey reliability on Wayland.** Switching the overlay style from
  the tray now takes effect on the very next hotkey press (no
  restart). GNOME 47's portal hotkey rejection is detected upfront so
  Fono falls back to gsettings/X11 instead of silently dropping
  presses.
- **Local Whisper picks better defaults out of the box.** Model names
  now resolve through a quality-tested quantization ladder
  (`tiny → q5_1`, `small → q5_1`, `small.en → q8_0`,
  `large-v3-turbo → q8_0`); CPU threads default to the physical core
  count, which doubles throughput on Zen 3 / Zen 4 SMT systems where
  the previous default over-subscribed logical threads.

### Fixed

- **Wayland overlay** no longer steals keyboard focus, paints as an
  opaque rectangle, or lands top-left on GNOME / Mutter. The overlay
  now runs through a pluggable backend layer: native
  `wlr-layer-shell` on KDE / wlroots / COSMIC / Hyprland; X11 via
  Xwayland on GNOME (which doesn't implement layer-shell). Set
  `FONO_OVERLAY_BACKEND=…` to force a specific backend.
- **PipeWire audio playback** (`pw-play`) no longer fails on every
  assistant reply — the `--raw` flag was missing.
- **LAN dictation against a Wyoming peer that advertises IPv6** no
  longer fails with `EINVAL` when the peer's first-listed address is
  a link-local. Discovery now prefers routable IPv4 / IPv6.
- **History database** rebuilds itself when it carries an older
  schema, instead of warning on every dictation.
- **The dictation key held down** while pausing no longer flips the
  overlay into PONDERING and (with auto-stop on) no longer ends the
  session out from under you.
- **Shipped binaries no longer SIGILL on pre-VNNI / pre-AVX-512 CPUs.**
  The release build inherited ggml's `GGML_NATIVE=ON` default, which
  appends `-march=native` to the C/C++ compile line. On the GitHub
  Actions Linux runner (AMD EPYC 7763, Zen 3) the C compiler's
  auto-vectoriser baked VPDPBUSD (AVX-VNNI) into the binary, causing
  immediate SIGILLs on users' Kaby Lake, 8th-gen Intel, and earlier
  laptops. The shipped binary now pins an explicit
  AVX2 / FMA / F16C / BMI2 baseline (Intel Haswell ≥ 2013, AMD
  Excavator ≥ 2015) via `.cargo/config.toml`, so what CI builds is
  what users download — regardless of which CPU GitHub puts in its
  runner pool. A/B verified on Lunar Lake: zero throughput loss
  (±7% noise) because ggml's hand-written VNNI kernels are separately
  gated by `GGML_AVX_VNNI` (also off by default), so `-march=native`
  was costing portability without delivering any actual VNNI speedup.
- **Hotkeys work immediately after `sudo fono install` on
  GNOME-Wayland.** The post-install autostart spawned the daemon via
  `runuser -u $SUDO_USER`, which inherited the sudo-scrubbed
  environment: `DISPLAY=:0` was preserved but `WAYLAND_DISPLAY`,
  `XDG_RUNTIME_DIR`, and `DBUS_SESSION_BUS_ADDRESS` were not. With
  only `DISPLAY` set, the daemon's hotkey-backend detector picked
  the X11 listener, the GNOME-gsettings shim never ran, and F7 / F8
  fell through in every native-Wayland app — users only saw working
  hotkeys after logging out and back in (when the XDG autostart entry
  fired with a real session env). The installer now reconstructs the
  graphical-session env from `/run/user/$(id -u)` inside the spawn
  command — after the user-switch — so the first daemon launched by
  `sudo fono install` is identical to what the next-login autostart
  would have produced. Drive-by: `shutdown_existing_daemon` no
  longer panics with "Cannot start a runtime from within a runtime"
  when re-running install while a previous daemon is still alive.

### Removed

- 14 inert config keys (the always-warm-mic flag, eight commit-tuning
  knobs, three session-budget knobs, and two more) — all of them
  were silently ignored at runtime. Defaults are unchanged.

### Breaking

- **`[overlay].volume_bar` is now `"off" | "simple" | "advanced"`**
  instead of a boolean, and defaults to `"off"`. Existing configs
  need a one-line edit: `volume_bar = true` → `"simple"`,
  `volume_bar = false` → `"off"`. The tray picker handles new
  installs automatically.

## [0.8.0] — 2026-05-17

### Changed

- **Live preview is now a waveform style, not a separate toggle.** The
  tray "Waveform style" submenu gains a fifth entry — `Transcript
  (live preview — more CPU / tokens)` — that replaces the old
  config-file-only `[interactive].enabled` flag. Picking Transcript
  both swaps the overlay to streaming text **and** routes the
  dictation hotkey through the live pipeline (this is the fix for
  "live transcription only worked for the assistant, not for
  dictation"). `Fft` remains the first-run default; live preview stays
  opt-in because it costs more CPU on local STT and more tokens on
  any cloud backend that bills per-second of streamed audio.
  Internally `Config::live_preview()` is the single source of truth,
  defined as `overlay.style == Transcript`. See
  [ADR 0026](docs/decisions/0026-live-preview-as-overlay-style.md).

### Removed

- `[interactive].enabled` config field (Fono has no users yet, so no
  migration is provided — the field is just gone). The rest of the
  `[interactive]` block — boundary heuristics, drain grace,
  `cleanup_on_finalize`, prosody/filler vocab, chunk timing — stays
  put as streaming-pipeline tuning that applies whenever Transcript is
  active.

### Added

- **`scripts/capture-overlay.sh`** — reproducible overlay screencast
  helper for the README. Three modes: `overlay` (tight 640×≤240 crop),
  `paste` (overlay + target-app window for "lands in a real app"
  demos), and `gallery` (records each waveform style — bars,
  oscilloscope, FFT, heatmap — labels them, and stitches the clips
  via `ffmpeg -f concat` or a 2×2 `xstack` grid). Detects
  X11 vs Wayland, resolves monitor geometry via xrandr / wlr-randr /
  swaymsg, encodes MP4 + GIF (palette pipeline with 5 MB soft / 9.5 MB
  hard budget auto-tiering) + animated WebP, and probes deps with
  per-distro install hints. Dev-only; not part of the shipped binary.
  See `docs/troubleshooting.md` → "Capturing screencasts".

- **Onboarding auto-start and contextual tray left-click.** Three
  small UX changes that turn the first-launch path into a one-command
  experience:
  1. `sudo fono install` (and therefore `curl -fsSL
     https://fono.page/install | sh`) now starts `fono` in the
     background as the invoking user — picked up from `$SUDO_USER`
     and launched via `runuser`/`sudo` with `setsid` detachment — and
     then runs the `fono setup` wizard interactively in the same
     terminal (also as `$SUDO_USER`, with stdio inherited so the
     prompts reach the user). Running the installer as bare root (no
     `sudo` wrapper) is a fully supported path: fono spawns and the
     wizard runs as root, writing under `/root/.config/fono/` — fono
     is allowed to run as root if that's what you want.
     `packaging/install.sh` re-attaches `</dev/tty` to the install
     invocation under the `curl | sh` transport so the wizard's
     stdin still has a real terminal when curl is piping the script
     in. The backgrounded daemon's stdout/stderr now append to
     `$XDG_STATE_HOME/fono/fono.log` (typically
     `~/.local/state/fono/fono.log`, or `/root/.local/state/fono/fono.log`
     for the bare-root install path) — matching `Paths::log_file()`
     so `tail -f` and what fono itself considers its log path are the
     same file. Previously the spawn redirected to `/dev/null`, which
     made post-install troubleshooting needlessly hard. Each step now
     reports a precise outcome (started / setup completed / skipped
     because headless / spawn failed) so users always know exactly
     what happened. Skipped on headless boxes (no
     `DISPLAY`/`WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR`) and bypassable
     with `FONO_INSTALL_NO_START=1` for packagers and CI. The XDG
     autostart entry still handles next-login start. The server-mode
     install path is unchanged — systemd's `systemctl enable --now`
     was already starting the unit (logs via `journalctl -u
     fono.service`).
  2. The daemon now fires a single low-urgency desktop notification
     on startup when no TTS backend is configured, prompting the user
     to run `fono setup`. Once per process; suppressed once setup
     completes (the daemon's IPC `Reload` hook refreshes the
     onboarding snapshot atomically so no restart is required).
  3. The tray icon's SNI left-click is now contextual: when TTS is
     not yet configured it nudges toward `fono setup`; once configured
     it shows the current hotkey cheat sheet (dictation / assistant /
     cancel). The "Show last transcription" menu entry continues to
     work for users who want it; the left-click no longer fires that
     action.

  Implemented without adding any config field — the question "is setup
  finished?" is answered by the new `Config::tts_configured(&Secrets)`
  helper, which folds the existing `configured_tts_backends` logic.
  `packaging/install.sh` is now the canonical source for the
  `https://fono.page/install` one-liner and lives next to the binary
  it ships.

- **Unified log file at `/var/log/fono.log`.** Single-user-box
  convention: every fono process writes there (world-writable 0666,
  pre-created by `fono install`). `Paths::log_file()` now points at
  that path. The daemon's `tracing` formatter forces ANSI on, so the
  file preserves colors. `fono doctor` appends the last 10 log lines
  to its report; `fono doctor -f` (or `--follow`) streams the file in
  real time via `tail -F`, ANSI escapes intact. The background spawn
  in `fono install` falls back to `/dev/null` if `/var/log/fono.log`
  is not writable, so a permissions hiccup never blocks startup.

- **Colorized `fono doctor` output.** Section headers in bold cyan,
  `ready` / `present` / `exists` in green, `FAIL` / `MISSING` /
  `FAILED TO LOAD` / `NONE` in bold red, `disabled` / `(unset)` /
  `(fallback)` dimmed, active-provider `*` highlighted. Auto-disabled
  when stdout is not a TTY (pipes, redirects, CI) and when `NO_COLOR`
  is set, so scripts parsing the output remain unaffected.

- **Animated "POLISHING" overlay for local STT/LLM.** The
  standalone-waveform overlay's post-release phase used to show a
  static "POLISHING" panel while STT (and optional LLM cleanup) ran;
  with a local whisper.cpp backend that's a 1–3 s dead patch where
  the user has no signal the dictation is actually progressing. The
  overlay now reuses the assistant's per-style thinking animation
  (FFT bell sweep, neural-strand heatmap, oscilloscope standing
  wave, centre-out bars) during that phase whenever the active STT
  backend reports `is_local()` — or whenever LLM cleanup is enabled
  and the LLM is local. Cloud STT+LLM (sub-second) keep the static
  panel so it doesn't just flash. Implemented via a new
  `OverlayState::Polishing` variant that shares the amber accent +
  "POLISHING" label with the existing `Processing` state but is
  consumed by the same synthetic-frame renderer path as
  `AssistantThinking`. New default `is_local()` method on both the
  `SpeechToText` and `StreamingStt` traits (also `TextFormatter`),
  overridden to `true` only in the `whisper-local` and `llama-local`
  backends.

### Fixed

- **OpenRouter TTS default swapped from `openai/gpt-4o-mini-tts-…` to
  `openai/tts-1`** (default voice `alloy`). The LLM-based
  `gpt-4o-mini-tts` model produced higher-quality voices but its
  streaming output was not reliably forwarded by OpenRouter's
  `/audio/speech` proxy: the proxy flushed an ~9.6 KB preamble and
  then buffered the rest of the synthesised body until upstream
  finished (~30+ s for a typical 200-character reply), exceeding
  every reasonable client timeout. Verified via the `fono.http`
  instrumentation's one-shot stall hex dump — bytes were valid PCM,
  just never delivered. Classical `tts-1` produces audio in
  ~0.5-2 s regardless of length and the whole body is forwarded in
  one go, sidestepping the proxy-buffering problem entirely. Users
  who want the LLM-based voice can pin
  `[tts.cloud] model = "openai/gpt-4o-mini-tts-2025-12-15"` in
  `config.toml` and accept the failure mode on long replies, or
  switch to OpenAI direct (where streaming works correctly).

- **OpenRouter TTS second-sentence stalls eliminated** by disabling
  HTTP/2 connection-pool reuse on the TTS client. Previously, the
  first sentence of an assistant turn synthesised correctly but every
  subsequent sentence stalled identically (~9.6 KB chunk arrived,
  then 15 s of silence, then watchdog fired) — symptomatic of
  OpenRouter's `/audio/speech` proxy mishandling multiplexed HTTP/2
  streams. The TTS reqwest client now runs with
  `pool_max_idle_per_host(0)` and `http1_only()`, forcing a fresh
  TCP+TLS handshake per request (~200-400 ms overhead, negligible
  against multi-second LLM-based synthesis). Other backends (LLM,
  STT, assistant chat) keep their HTTP/2 pooling because no
  equivalent stall pattern was observed there.

- **TTS inter-chunk watchdog set to 20 s.** Empirically OpenRouter's
  `/audio/speech` proxy delivers a small preamble (~9.6 KB across ~8
  chunks) and then pauses for several seconds before resuming the
  audio stream proper. The previous 5 s watchdog tripped during that
  pause and produced false-stall failures on otherwise-healthy
  synthesis; 20 s keeps headroom for that pause while still catching
  genuinely wedged connections far faster than the overall 30 s
  request timeout. A one-shot `warn!`-level hex dump of the partial
  body fires on the first TTS stall per process lifetime, surfacing
  whether the preamble bytes are SSE framing, JSON metadata, or
  genuine PCM — diagnostic data for the next round of investigation.

- **Structured-log `chunks` field now reports the truth** on stalled
  / transport-error outcomes. Previously hardcoded to `0` in the TTS,
  LLM, and STT consumers, which made it impossible to distinguish
  "proxy sent one chunk then hung" from "nothing ever arrived" in
  `fono.http=debug` logs. New `BodyError::chunks()` and
  `BodyError::after_ms()` accessors expose the underlying watchdog
  state to all consumers uniformly.

- **OpenRouter TTS time-to-first-audio collapsed from ~30 s to ~2-4 s**
  by sending `stream_format: "audio"` on `/audio/speech` requests for
  models that benefit from it (OpenRouter's `gpt-4o-mini-tts` and
  OpenAI direct). Without this field, OpenAI's LLM-based TTS models
  buffer the entire synthesis server-side before opening the response
  body — visible in the `fono.http` instrumentation as a ~30 s
  `headers_ms` followed by a ~200 ms `body_ms`. With it, the upstream
  streams raw audio bytes as they are generated and `headers_ms` drops
  to sub-second. The catalogue gates the new field per provider:
  enabled for OpenAI and OpenRouter, intentionally omitted for Groq's
  Orpheus deployment (which is conservative about unknown request
  fields). Classical models like `tts-1` are unaffected — they already
  stream by default and accept the field as a no-op.

### Added

- **Structured HTTP instrumentation across every cloud-backed
  pipeline** (STT transcribe, LLM cleanup chat, voice-assistant
  streaming chat, TTS `/audio/speech`, wizard key validation). A new
  `fono-http` crate provides a single per-stage stopwatch
  (`RequestTimings`), an inter-chunk body watchdog
  (`read_body_with_watchdog`), and one chokepoint
  (`emit_http_debug`) that funnels every consumer through the same
  schema (`stage`, `provider`, `endpoint`, `status`, `headers_ms`,
  `ttfb_ms`, `body_ms`, `decode_ms`, `total_ms`, `body_bytes`,
  `content_length`, `chunks`, `request_id`, `attempt`, `outcome`).
  Silent by default; opt in per session with
  `RUST_LOG=info,fono.http=debug fono daemon`. Detects stalled
  bodies in 15-30 s (per stage) rather than waiting for the global
  60 s reqwest timeout, surfaces the upstream `x-request-id` /
  `request-id` on every response (success and failure), and on TTS
  retries once automatically when the upstream stalls mid-body
  (typical OpenRouter proxy hiccup). The improved error surface for
  stalled TTS now reads e.g. `openrouter TTS body read failed
  (request_id=or-…, attempt=2)` instead of the previous bare
  `reading openrouter TTS response body`. Per-stage chunk watchdogs:
  TTS 15 s (overall cap reduced from 60 s to 30 s), STT 30 s, LLM
  cleanup 30 s, assistant SSE 20 s inter-event.

- **OpenRouter app attribution** is now sent on every outbound
  request to `openrouter.ai` (STT transcribe + prewarm, LLM chat +
  prewarm, voice-assistant chat stream + prewarm, TTS
  `/audio/speech`, and the wizard's `validate_cloud_key` probe),
  not just from the STT backend as before. The three static headers
  are `HTTP-Referer: https://fono.page`,
  `X-OpenRouter-Title: Fono`, and
  `X-OpenRouter-Categories: personal-agent,writing-assistant` —
  identical across every install, no per-user or per-machine
  identifier embedded, no request body changes. Fono now appears on
  https://openrouter.ai/rankings, in the "Apps" tab of each model
  it routes through, and gets a public dashboard at
  https://openrouter.ai/apps?url=https://fono.page. The previous
  STT-only attribution used the GitHub repo URL as the Referer; the
  switch to `fono.page` is a deliberate one-time reset onto the
  canonical project homepage. See
  <https://openrouter.ai/docs/app-attribution> and the new
  `fono_core::openrouter_attribution` module.

- **`fono setup` now hot-reloads the daemon when it finishes.**
  Previously, running the wizard while `fono` was already running
  saved the new config but the daemon kept using the old one until
  manually restarted. The wizard now sends `Request::Reload` over
  IPC after `config.toml` / `secrets.toml` are written, and prints
  `Daemon reloaded — new settings are live.` (or a friendly
  fallback hint when no daemon is running).

- **Desktop notification when a configured backend's API key is
  missing at startup or after a config reload.** Previously, a
  rotated key or a wizard pick whose secret hadn't been added yet
  surfaced only as a single `tracing::WARN` line (e.g. `TTS
  unavailable; assistant replies will be silent: Cartesia TTS API
  key "CARTESIA_API_KEY" not found in secrets.toml or environment`).
  A new `ErrorClass::MissingKey` variant is now classified from
  reload errors and fired as a Critical-urgency popup with copy
  that names the env var and the `fono keys add <KEY>` command.
  Wired through the LLM / TTS / Assistant reload paths; subject to
  the existing session cascade cap.

### Changed

- **OpenRouter TTS default swapped from `hexgrad/kokoro-82m` to
  `openai/gpt-4o-mini-tts-2025-12-15`** for native multilingual output
  (default voice `coral`, $0.60 / 1 M characters). Kokoro voices are
  monolingual and prefixed by language code, so every non-English
  synthesis was routed through an American-English voice; OpenAI Mini
  TTS speaks French, German, Spanish, Romanian, Mandarin, etc.
  natively with no per-call language argument or per-language voice
  map needed. Existing users who prefer Kokoro can pin
  `[tts.cloud] model = "hexgrad/kokoro-82m"` and
  `voice = "af_heart"` in `config.toml`; full Kokoro support is
  deferred to a future local+cloud-symmetric backend (see
  `plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md`).

- **Voice assistant wizard step now renders as an aligned three-
  column table** (Provider · Model · Key). Model names are
  human-readable (`GPT-5.4 mini`, `Claude Haiku 4.5`,
  `GPT-OSS 120B`, `Qwen 3 235B`, …) rather than raw catalogue ids,
  and the key-status column reads `set` / `missing` instead of the
  earlier `(key already set)` / `(will ask for key)` parenthetical.

- **Assistant TTS auto-picked from the same key.** When the chosen
  assistant chat provider also offers TTS (e.g. OpenAI for both),
  the wizard reuses the same provider + key for the spoken reply
  and prints `TTS: <provider> (same key as the assistant — no
  extra prompt).` instead of running the explicit TTS picker. The
  picker still runs when the chat provider has no TTS capability.

- **Comfortable-tier first-run latency budget bumped from 1500 ms
  to 2000 ms.** The earlier 1.5 s ceiling tripped first-dictation
  warnings on perfectly usable mid-range hardware (laptops on
  battery, slower SSDs). 2.0 s reflects measured p50 latency on
  the lower end of the Comfortable tier; tiers above it (HighEnd
  600 ms / Recommended 1000 ms) are unchanged.

- **Tray TTS submenu drops the redundant `cloud,` prefix and greys
  out unavailable backends.** Every cloud backend was annotated
  `(cloud, will ask for key)` or `(cloud, key already set)` — but
  clicking the entry never asked for a key, so the message was
  misleading. The submenu now shows backends whose key is missing
  as non-clickable (greyed-out) rows with a plain `(no key)`
  suffix; backends with a configured key remain clickable. A new
  `DISABLED_SENTINEL` prefix in `fono-tray` lets daemon submenus
  opt rows out of activation without per-row plumbing.

### Fixed

- **Groq TTS rejected `response_format: pcm` with HTTP 400
  (`response_format must be one of [wav]`).** Groq's Orpheus
  deployment only emits WAV-wrapped audio. The OpenAI-compat TTS
  client now reads its `response_format` from the catalogue
  (`OpenAiCompat { base_url, response_format }`) and strips the
  RIFF/WAVE header transparently when the provider returns WAV,
  yielding the same raw 24 kHz int16 LE PCM the playback path
  expects. OpenAI and OpenRouter keep `pcm` (lowest latency).

- **Groq TTS rejected the default voice (`tara`) with HTTP 400
  (`voice must be one of the following voices: [autumn diana hannah
  austin daniel troy]`).** Fono's catalogue defaulted to `tara`,
  which is part of Canopy Labs' open-source Orpheus voice set but
  not part of Groq's hosted six-voice subset for
  `canopylabs/orpheus-v1-english`. The Groq TTS default voice is
  now `hannah` (neutral female, in Groq's curated set). Users with
  an explicit `[tts.cloud.groq].voice` override pinned to a Canopy-
  only voice (`tara`/`leah`/`jess`/`leo`/`dan`/`mia`/`zac`/`zoe`)
  must edit to one of `autumn`/`diana`/`hannah`/`austin`/`daniel`/
  `troy` to get audio out of Groq.

### Added

- **Desktop notification when a TTS/STT/LLM/assistant model requires
  terms acceptance.** Providers like Groq return HTTP 400 with
  `model_terms_required` when an org admin hasn't accepted a model's
  terms (e.g. Orpheus, PlayAI). The critical-notify classifier now
  recognises that shape as a new `TermsRequired` class, and the
  notification body embeds the acceptance URL extracted from the
  provider response so the user can click straight through to the
  console. Subject to the existing session cascade cap.

### Fixed

- **Anthropic LLM cleanup 400 `stop_sequences: each stop sequence must
  contain non-whitespace`.** The client was sending
  `stop_sequences = ["\n\n"]` which Anthropic now rejects. The
  blank-line heuristic is dropped; cleanup output length is bounded by
  `max_tokens = 512` alone.

- **Groq assistant returned 404 (`model_not_found`)** because the
  catalogue advertised `llama-4-maverick-17b-128e-instruct` as Groq's
  multimodal model and the new default of `prefer_vision = true`
  caused the runtime to swap to it. That model isn't available on
  Groq today. Groq's `multimodal_model` is now `None`; the assistant
  uses `openai/gpt-oss-120b` (the existing `text_model`) for every
  Groq request.

- **Groq TTS model decommissioned.** The previously catalogued
  `playai-tts` model (voice `Fritz-PlayAI`) was retired by Groq and
  now returns `model_not_found`. Groq's catalogue entry now points
  at `canopylabs/orpheus-v1-english` (Canopy Labs' Orpheus, OpenAI-
  compatible audio/speech on Groq) with default voice `tara`. The
  endpoint URL and auth header are unchanged.

- **OpenAI assistant requests rejected by chat/completions when
  `prefer_web_search` was on (`Invalid value: 'web_search_preview'`).**
  The `web_search_preview` tool descriptor is Responses-API-only;
  chat/completions rejects unknown tool types with a 400. OpenAI's
  catalogue entry now advertises `web_search = None`; the default of
  `[assistant].prefer_web_search` has been flipped to `false`.
  Anthropic's `web_search_20250305` (Messages API) is unaffected. A
  future commit will re-enable OpenAI web search via the Responses
  API migration. As a defensive belt-and-braces, the OpenAI
  chat/completions client now drops any web-search tool descriptor
  at request build time and emits a one-shot `tracing::warn!` so a
  hand-edited `prefer_web_search = true` no longer surfaces a 400 to
  the user.

- **Cloud STT clients (OpenAI, Deepgram) were missing from the default
  build.** `crates/fono/Cargo.toml` listed `fono-stt` and `fono-llm`
  with no feature selection, so the default release shipped only the
  per-crate `default` features (Groq + Wyoming STT, OpenAI-compat +
  Groq LLM). A user picking OpenAI as primary in the wizard hit a
  `STT not compiled in` warning at daemon startup. `fono-stt` is now
  built with `groq + openai + deepgram + wyoming`; `fono-llm` is
  built with `cerebras + openai-compat + anthropic`. The `cloud-all`
  meta-feature is widened to match. (Cartesia / AssemblyAI STT
  clients are not yet wired as `fono-stt` features — tracked
  separately.)

### Added

- **Cloud provider capability catalogue.** A single
  `fono_core::provider_catalog::CLOUD_PROVIDERS` table is the source of
  truth for which cloud providers offer STT / LLM cleanup / assistant
  chat / vision / web search / TTS. The wizard, tray, `fono use cloud`,
  and `fono doctor` all consume the catalogue, eliminating the five
  duplicated `match` blocks the wizard used to carry. (Phase A, #9; see
  [ADR 0025](docs/decisions/0025-cloud-provider-catalogue.md).)
- **Multi-provider TTS for the voice assistant (#11).** The assistant
  audio path now supports Groq (PlayAI `playai-tts`), OpenRouter
  (Kokoro `hexgrad/kokoro-82m`), Cartesia (`sonic-2`), and Deepgram
  (`aura-2-thalia-en`) in addition to OpenAI and Wyoming. Users on a
  non-OpenAI primary can run the full record → STT → LLM → TTS loop
  without obtaining a second key. `CARTESIA_API_KEY` and
  `DEEPGRAM_API_KEY` already present in `secrets.toml` from STT usage
  are reused automatically; the wizard's TTS picker orders providers
  with stored keys first.
- **Optional assistant extras.** Two new `[assistant]` toggles surface
  in the wizard's *Optional extras* MultiSelect when the chosen primary
  supports them: `prefer_vision` swaps the assistant chat model for the
  provider's multimodal variant (OpenAI / Anthropic / Groq / Gemini),
  and `prefer_web_search` attaches the provider's native web-search
  tool to every assistant request (OpenAI's `web_search_preview`,
  Anthropic's `web_search_20250305`; Gemini's `google_search` is
  catalogued for forward compatibility). Both default to `false`.
- **Desktop notifications for critical pipeline failures.** Total STT
  pipeline failures (auth errors, network errors, 5xx) and LLM-cleanup
  auth-class failures now fire a Critical-urgency desktop notification
  in addition to the existing `error!`/`warn!` log line, so an
  expired API key is no longer silently buried in journalctl. Dedup
  is per-session and per `(stage, provider, error class)`, so a stuck
  key pops exactly once per F8/F9 press and an STT-auth + LLM-auth
  failure in the same session each get their own surface. LLM
  transient errors (network blips, 5xx) keep the existing silent
  fallback to the raw STT transcript — only configuration-class
  failures pop a notification.
- **Critical-failure notification coverage extended (issue #8).** TTS
  (assistant-mode reply playback), Assistant chat (both stream-open
  and mid-stream errors), and text-injection failures now route
  through the same `critical_notify` surface as STT/LLM, so a
  rotated API key in any stage produces a Critical-urgency popup
  instead of journal-only output. The LLM cleanup path also now
  notifies on `Network`-class failures (previously `Auth`-only), so
  an offline endpoint is visibly surfaced.
- **Daemon-startup-failure notification.** When `fono daemon` exits
  with an error (bad config, locked single-instance socket, hotkey
  backend init failure), a one-shot Critical-urgency notification
  fires before the process exits, pointing the user at
  `journalctl --user -u fono` and `fono doctor`. This addresses the
  systemd `--user` autostart case where stderr is invisible.

### Changed

- **Assistant extras default policy.** `prefer_vision` stays
  default-on (no API impact — the multimodal model is the same model
  on OpenAI/Anthropic, just with image input capability advertised).
  `prefer_web_search` now defaults off: the only provider whose
  chat/completions API supports it natively today is Anthropic, and
  OpenAI's chat/completions endpoint hard-rejects the
  `web_search_preview` descriptor. The default flips back to `true`
  once the OpenAI client migrates to the Responses API.

- **Wizard first-run UX corrections (pre-release polish).**
  - The step-1 path picker is now a fixed-order two-column table
    (`Local` / `Cloud` / `Customize`) instead of a tier-dependent
    paragraph-shaped list. Column padding is computed from the
    longest option name + 2 spaces so future variants stay aligned.
  - The language picker is skipped entirely when the OS reports at
    least one detected language; the picker only renders for the
    zero-detection fallback. A one-line info trace records the
    detected codes and points the user at the tray's Languages
    submenu for editing.
  - The "Enable live dictation?" question is dropped from every
    branch — the tray's existing toggle is the editing surface, and
    `config.interactive.enabled` already defaults to `false`.
  - The cloud-assistant fast-path is now automatic: when the chosen
    primary covers chat, the assistant is enabled without a Confirm.
    Two info lines state the configuration; `pick_tts_for_assistant`
    still runs when no TTS was set, and `prompt_assistant_extras`
    keeps vision / web-search as explicit opt-ins. The legacy
    `Confirm("Enable the voice assistant?")` survives only for the
    local-LLM branch where no catalogue primary matches.

- **Wizard cloud branch collapsed onto a single primary-provider
  picker (#9).** Picking OpenAI or Groq now configures STT, LLM
  cleanup, the voice assistant, and TTS from one API-key prompt;
  picking Anthropic / Cerebras / OpenRouter configures LLM + Assistant
  and asks an opt-in follow-up only for the capabilities the primary
  doesn't cover. The wizard label list shows runtime-derived capability
  badges (`STT · LLM · Assistant · TTS · Vision · Search`), capped at
  six per row.
- **`PathChoice::Mixed` renamed to `PathChoice::Customize`.** The
  advanced wizard branch now appears in the top-level menu as
  *"Customize each capability (advanced)"*. Legacy configs that still
  carry `mixed` semantics continue to load — there is no on-disk
  enum to migrate.
- **Re-running the wizard reuses stored keys silently.** Every
  cloud-key prompt now routes through `prompt_or_reuse_key`, which
  prints a single `reusing <KEY> from secrets.toml` line instead of
  re-asking. A returning user with a populated `secrets.toml` sees
  zero key prompts on a wizard re-run.
- **Cascade cap on critical notifications (issue #8).** When a single
  root cause (e.g. a rotated cloud API key) cascade-fails through
  STT → LLM → Assistant → TTS in the same dictation session, the
  user now sees **exactly one** notification — the first stage to
  fail — instead of one per stage. Downstream failures still go to
  the journal at `warn!`. The cap auto-resets at the start of each
  new F8/F9/F10 press and after 120 s of dictation inactivity.
  `Stage` is now `#[non_exhaustive]` so future stages can be added
  without breaking matches.

- **Hotkeys auto-detect toggle vs push-to-talk per press.** A short tap
  (under one second) on the dictation or assistant hotkey toggles
  recording on; pressing-and-holding for at least a second flips the
  same key into push-to-talk and recording stops on release. The
  global `[hotkeys].mode = "toggle" | "hold"` setting is removed —
  there is now one consistent behaviour across both keys with no
  configuration required.

### Removed

- **`[hotkeys].mode` configuration field.** Old configs that still set
  `mode = "toggle"` or `mode = "hold"` continue to load (serde
  silently ignores the unknown field); the value has no effect. The
  `HotkeyMode` enum is dropped from `fono_core::config`.

## [0.7.1] — 2026-05-05

### Changed

- **Default hotkeys: `F7` for dictation, `F8` for the voice assistant,
  toggle mode by default.** Previous defaults — F8 (push-to-talk hold),
  F9 (toggle), F10 (assistant push-to-talk) — collided with htop's
  exit / kill / nice bindings and, in F10's case, the GTK menubar
  shortcut. The two dictation keys collapse into one (`F7`) and the
  assistant key moves down by two (to `F8`). Both keys now share a
  single `[hotkeys].mode = "toggle" | "hold"` setting that applies
  globally; `mode = "toggle"` (default) means press once to start,
  press again to stop. The assistant moves to toggle by default too
  — no more holding a key down through the multi-second STT → LLM →
  TTS round trip.
- **`[hotkeys].toggle` renamed to `[hotkeys].dictation`.** Old configs
  continue to parse via a serde alias; existing user overrides keep
  working.
- **`[hotkeys].hold` field removed.** Push-to-talk is now expressed
  as `[hotkeys].mode = "hold"`, which applies to both the dictation
  and the assistant key. Old configs with `hold = "F8"` are silently
  ignored on load. To keep push-to-talk, set `mode = "hold"` in your
  config (the dictation and assistant keys remain whatever you have
  bound).

## [0.7.0] — 2026-05-04

### Added

- **Voice assistant — F10 hold-to-talk, streaming chat, TTS playback.**
  A second push-to-talk key (F10 by default) turns Fono into an
  offline-capable voice assistant. The pipeline diverges after STT:
  instead of cleaning the transcript and injecting it, Fono asks a
  chat-capable LLM, streams the reply sentence-by-sentence into a TTS
  backend, and plays the audio. First sentence starts speaking before
  the model finishes generating, so time-to-first-audio is bounded by
  one sentence's synth latency rather than the full reply.
- **`[assistant]` and `[tts]` config blocks.** Independent backend
  selection from the `[llm]` cleanup pipeline — pick a fast local 3B
  for cleanup and a bigger cloud model for the assistant, or any
  mix-and-match. Multi-turn rolling history with a configurable time
  window (default 5 minutes) and max-turn cap (default 12). Pressing
  the dictation key clears assistant context (configurable);
  pressing F10 again mid-reply barges in with history retained;
  Escape stops playback ("shut up") without forgetting.
- **Cloud assistant backends.** Anthropic (Claude Haiku 4.5) and the
  full OpenAI-compatible family — OpenAI (gpt-5.4-mini), Cerebras
  (qwen-3-235b-a22b-instruct-2507), Groq (openai/gpt-oss-120b),
  OpenRouter, Ollama. Each
  ships in the default binary; one feature flag per family lets slim
  builds drop unused providers.
- **Cloud cleanup model defaults refreshed** to match retired and
  newly-released models: Cerebras `llama3.1-8b`, Groq
  `openai/gpt-oss-20b`, OpenAI `gpt-5.4-nano`, Anthropic
  `claude-haiku-4-5-20251001`. The OpenAI-compat client now sends
  `max_completion_tokens` (the new field name newer OpenAI models
  require; older models still accept it).
- **TTS backends.** Wyoming protocol client (any
  `wyoming-piper`-style server on the LAN), the OpenAI
  `/v1/audio/speech` API (24 kHz PCM stream), and an in-process
  Piper stub that points users at Wyoming-piper for now (the
  static-musl ship build can't yet pull in onnxruntime). Audio
  playback uses `paplay` on the Linux release variant (no libasound
  link, matches the existing parec capture path) or cpal behind the
  `cpal-backend` feature.
- **CLI surface.** `fono use assistant <backend>`,
  `fono use tts <backend> [--uri tcp://host:port]`,
  `fono assistant {press,release,stop}` for scripted end-to-end
  testing.
- **Tray.** New *Stop assistant* and *Forget conversation* entries;
  *Assistant backend* and *TTS backend* submenus mirror the existing
  STT/LLM submenus and switch backends live via Reload. Tray icon
  flips amber while the assistant is active.
- **Wizard.** `fono setup` ends with an opt-in assistant + TTS step;
  reuses any cloud key already entered earlier in the flow so a
  single OPENAI_API_KEY powers both chat and TTS without a second
  prompt.
- **Doctor.** `fono doctor` exercises both factories at startup so a
  missing API key or unreachable Wyoming server surfaces in one
  place; new `Providers (assistant)` and `Providers (TTS)` tables
  show key/URI status per backend with an active marker.
- **Overlay feedback for the assistant flow.** Recording paints
  green ("ASSISTANT") with the chosen waveform style; the post-
  release thinking + speaking phase paints amber ("THINKING") with
  per-style synthetic animations distinct from the real-audio
  recording shape:
  - **FFT** — Gaussian "scanner" (σ ≈ 8 bins out of 100) sweeps
    across the panel; per-bin breathing baseline blends in via
    a screen composite so the bell emerges smoothly.
  - **Bars** — symmetric centre-out, peak at midline rippling
    outward.
  - **Oscilloscope** — two interfering sine waves with edge taper
    pinning x = 0 / x = 1 to the centerline; central antinode
    reaches ±1.0 without clipping.
  - **Heatmap** — two anti-phased Gaussian "neural strands"
    crossing over the rolling 6 s window; transitions seamlessly
    from recording-FFT data without clearing the cache.
  Default `[overlay].style` flipped from Bars → FFT — most active
  visualisation across both phases.
- **Runtime overlay style swap.** Changing `[overlay].style` via
  `fono use`, the tray *Waveform style* submenu, or `fono config
  edit` now applies on the next frame instead of waiting for a
  daemon restart.
- **Smoke-test binary** (`cargo run --release --example
  smoke_assistant -p fono`) exercises each cloud assistant + the
  OpenAI TTS path end-to-end. The release CI's new
  `cloud-assistant` job runs the `--ci` subset (Groq + Cerebras,
  the providers whose API keys are stored as GitHub Secrets).

### Fixed

- **FSM stuck on a sub-300 ms F10 tap.** Brief F10 taps released
  before `MIN_RECORDING` left the orchestrator's
  `on_assistant_hold_release` early-returning without firing
  `ProcessingDone`; the FSM sat in `AssistantThinking` forever and
  silently rejected subsequent F8/F9/F10 presses. Every early-return
  path now emits `ProcessingDone`; `AssistantRecording` also accepts
  `ProcessingDone` as a safety net.
- **Audio playback worker dying after every cancel.** `pb.stop()`
  used to send `Cmd::Stop` which made the worker `break` out of its
  loop; the next turn's enqueue then failed with "audio playback
  worker stopped". `Cmd::Drain` now drains queued items + clears the
  abort flag without exiting the worker, so multi-turn conversations
  keep working across barge-ins, Forget, and dictation pivots.
- **Frozen overlay during the post-release phase.** The level task
  was aborted in `stop_and_drain` the moment capture ended, leaving
  the waveform on its last pre-release frame for 4–5 s while STT +
  LLM ran. The overlay now switches into the synthetic thinking
  animation as soon as F10 is released, and the FFT thinking
  visualisation gets even-spaced inter-bar gaps via integer-aligned
  slot widths.

## [0.6.1] — 2026-05-03

### Fixed

- **Robust headless / systemd startup.** Five regressions surfaced
  when running `fono` under `fono.service` on a headless inference box (Debian 13,
  no `DISPLAY`, systemd) all collapsed into one pass:
  - **Vulkan probe crash on shutdown.** `ash::Entry::load()` at
    daemon start enumerates every Vulkan ICD on the host (incl.
    Mesa `lvp` / LLVMpipe), which spawns CPU worker threads still
    parked in futexes when glibc `dl_fini` unmaps `libvulkan` on
    exit — segfault. The probe now runs in a disposable subprocess
    (re-exec self with `FONO_INTERNAL_VULKAN_PROBE=1`) and the
    parent reads a single tab-delimited result line off stdout
    cached in a `OnceLock`. Any spawn / timeout / parse failure
    collapses to `Outcome::NotAvailable` so the daemon never crashes
    on a broken Vulkan stack.
  - **`global-hotkey` null-display crash.** `global-hotkey` 0.6.4's
    X11 `events_processor` calls `XOpenDisplay(NULL)` and then
    dereferences the result via `XDefaultRootWindow` without a
    NULL check, segfaulting on hosts without `DISPLAY` /
    `WAYLAND_DISPLAY`. `fono_hotkey::spawn_listener` is now gated
    on `is_graphical_session()`, the same runtime check the tray
    already uses.
  - **Systemd crash-loop on first run.** The implicit first-run
    wizard ran whenever `~/.config/fono/config.toml` was missing;
    under systemd `dialoguer` aborts with `IO error: not a terminal`
    and the unit restart-loops. The implicit wizard is now gated on
    `stdin().is_terminal()`; with no TTY, Fono writes
    `Config::default()` and continues. Explicit `fono setup` is
    unchanged.
  - **Redundant `daemon --no-tray` in the systemd unit.** `daemon`
    is the implicit default and the tray is already runtime-gated
    on `is_graphical_session()`, so the flag was dead weight.
    `ExecStart` is now plain `/usr/local/bin/fono`.
  - **Silent install failures.** `systemctl enable --now` returns
    success the moment `ExecStart` is spawned, so a unit that
    crashes a second later (`Restart=on-failure` loop) was invisible
    at install time. `sudo fono install --server` now waits 2 s, runs
    `systemctl is-active`, and on failure dumps the last 20 journal
    lines plus the recommended follow-up command.
  - **mDNS discovery on hardened systemd installs.** The system
    unit's `RestrictAddressFamilies=` allow-list blocked
    `AF_NETLINK`, which `mdns-sd` needs (via `getifaddrs(3)`) to
    enumerate network interfaces. Without it the advertiser
    registered the service in its in-process table and logged
    `mDNS advertising _wyoming._tcp …`, but never bound UDP/5353
    or joined the `224.0.0.251` multicast group — so LAN clients
    running `fono discover` saw nothing while TCP/10300 was
    perfectly reachable. `AF_NETLINK` is now in the allow-list,
    with a comment in `packaging/assets/fono.service` explaining
    why removing it silently breaks discovery.

### Changed

- **`--no-tray` flag removed; system IPC socket tried first.** The
  tray is already runtime-gated on `is_graphical_session()`, making
  `--no-tray` redundant. CLI clients (`fono toggle`, `fono history`,
  `fono use …`, …) now try the system-wide IPC socket
  (`/var/lib/fono/fono.sock`) before the per-user one, so a `fono`
  process running under the system `fono.service` unit can be
  driven from any user account on the box without per-user setup.
  Documentation in `docs/wayland.md` and `docs/troubleshooting.md`
  updated to match.
- **`general.sound_feedback` config field, tray "Start/stop chimes"
  toggle, and the chime playback action removed.** The chime path
  was a vestige from before the audio-visualisation overlay landed
  in v0.6.0; the overlay's bottom-centre panel + right-side VU bar
  now serves the same "did the recording start?" feedback role
  without spawning a separate audio process. Existing configs that
  set the field are silently ignored — no migration needed.

- **`[overlay].waveform` now defaults to `true`.** The standalone
  batch-mode overlay was off by default; new users had to discover
  the setting and edit `~/.config/fono/config.toml` to turn it on.
  The push-to-talk feedback panel (volume bars / oscilloscope /
  FFT / heatmap) is the kind of UX that's better-on-than-off.
  Existing configs are unaffected: the field is `#[serde(default)]`
  on the `Overlay` struct, so a config that omits the line picks
  up the new default; configs with an explicit `waveform = false`
  stay opted-out as the user wrote them.

## [0.6.0] — 2026-05-03

### Added

- **Tray `Preferences ▸` submenu.** Right-click the tray icon to
  toggle the most-touched settings without editing the TOML:
  - Six native-checkbox booleans — start/stop chimes, mute system
    audio while recording, keep mic always-on, also-copy to clipboard,
    autostart, voice-activity detection.
  - Auto-stop after silence (Off / 0.8 s / 1.5 s / 3 s).
  - Visualisation overlay style (Bars / Oscilloscope / FFT / Heatmap).
  - Multi-language allow-list — pick any combination of the curated
    17 languages (English, Spanish, French, German, Italian,
    Portuguese, Dutch, Romanian, Polish, Russian, Ukrainian, Turkish,
    Chinese, Japanese, Korean, Hindi, Arabic) plus an Auto-detect
    entry that clears the list. Each click writes
    `general.languages` atomically and triggers an in-process
    orchestrator reload — no daemon restart.

  All toggles share a single canonical curated language shortlist
  (`fono_core::languages::CURATED_LANGUAGES`) with the wizard, so
  picking "English" in either surface writes the same value.

- **Tray poll throttle.** The 2-second tray-state poll only fires
  `handle.update` when at least one provider's result actually
  changed since the last tick. Steady-state daemons emit zero
  `LayoutUpdated` D-Bus signals; cuts wake-ups and gives flaky tray
  hosts (notably snixembed) fewer events to mishandle.

- **Tray-task exit logging.** The poll loop's `Ok(())` exit path is
  now logged at `warn`, so a user noticing the icon disappear has a
  breadcrumb in the daemon log.

### Changed

- **Wizard consistency rework.** Yes/No prompts (`pick_english_only`,
  `pick_interactive_mode`) switched from `Confirm` to arrow-key
  `Select` defaulting to **No** — first-time users can press Enter
  on the safer choice and reach the full multi-language picker /
  batch-mode default in one keystroke. `pick_local_stt_model`
  auto-picks (and announces) when only one model fits the hardware
  + language selection. LLM cleanup choice reordered to **Skip /
  Cloud / Local** with a hardware-aware "— recommended" suffix:
  Cloud is recommended on hosts without LLM acceleration; Local is
  recommended only when Apple Silicon or a Vulkan-capable GPU is
  detected. Default cursor on Skip — local LLM on a CPU-only host
  is a frustrating first-run experience.

- **Single canonical curated language list.** Wizard and tray now
  draw from `fono_core::languages::CURATED_LANGUAGES`. Adding a
  language to the list adds it to both surfaces.

### Fixed

- **Update prompt no longer offers downgrades on variant mismatch.**
  When the GitHub releases API serves an older release as `latest`
  (e.g. v0.6.0 was tagged but only published as a Draft, so the API
  still returns v0.5.0), a CPU-variant binary on a Vulkan-capable
  host no longer trips the variant-switch path into surfacing
  "Update to v0.5.0". `fono_update::check` requires the remote
  release to be `>= current_version` before the variant-switch
  branch can fire. Two regression tests pin the behaviour.

- **Stale `update.json` cache invalidation.** On daemon startup the
  cached `UpdateStatus` is discarded if its `current` field doesn't
  match the running binary's `CARGO_PKG_VERSION`. Prevents a stale
  "Available" entry from briefly flashing in the tray after a
  version bump until the 10-second background re-check overwrites it.

### Performance

- **Whisper-local prewarm now materialises GPU compute pipelines.** On
  GPU-accelerated builds (`accel-vulkan` / `accel-cuda` / `accel-metal` /
  `accel-hipblas` / `accel-coreml`), `WhisperLocal::prewarm()` runs a
  one-shot silent decode (1 s of zeros at 16 kHz) right after loading
  the model, so `whisper.cpp`'s backend builds every `VkPipeline` /
  CUDA kernel / Metal pipeline-state and allocates its KV cache on the
  device during the background warmup at session start, not on the
  user's first dictation. Measured on RTX 4090 + Vulkan with
  `large-v3-turbo`: first-fixture batch latency drops from 7.8 s to
  1.0 s, and the total Vulkan bench batch time drops from 9.11 s to
  2.27 s (4.0×). CPU-only builds keep the original cheap mmap
  behaviour. The silent decode is best-effort — if it fails (e.g.
  driver bug) it is logged at `debug` and prewarm still returns
  success so real dictation can still proceed. See
  `plans/2026-05-03-whisper-vulkan-prewarm-v1.md`.

### Added (audio + visualisation)

- **Audio-visualisation overlay + live-dictation VU bar.** A new
  `waveform` cargo feature (default-on, GUI-only) renders a 640-wide
  bottom-centre overlay panel during batch (push-to-talk) recording
  with a selectable style:
  - `bars` — scrolling RMS amplitude bars; bars glow brighter at
    higher amplitude.
  - `oscilloscope` — connected-line waveform from raw PCM samples,
    pre-scaled by `1.0 / WAVEFORM_AMPLITUDE_CEILING` so a typical
    speaking voice fills a comfortable chunk of the panel; the
    overlay's 5000-sample (~300 ms) ring buffer scrolls slowly
    enough for individual cycles to be visible.
  - `fft` — real-input spectrum bars from a 4096-pt Hann-windowed
    FFT, aggregated into 300 display bins covering 0–3 kHz with a
    −20 … +30 dB normalisation. Bars are pixel-tiled (no AA gap)
    so the spectrum reads as a continuous gradient.
  - `heatmap` — rolling spectrogram (frequency on Y, time on X,
    magnitude as colour intensity), backed by a pre-blended pixel
    cache that scrolls leftward by one frame-width per FFT push so
    `redraw` is a straight blit.

  Configured via `[overlay].waveform = true` and
  `[overlay].style = "bars" | "oscilloscope" | "fft" | "heatmap"`.
  The standalone overlay is visible during `Recording`, transitions
  to amber `POLISHING` while STT runs, and hides on completion or
  cancel.

- **Live-dictation VU bar.** When `[interactive].enabled = true`
  the live-dictation panel now grows a thin right-side vertical
  meter that tracks microphone level in real time
  (`[overlay].volume_bar = true` by default). Drives off the same
  `OverlayCmd::AudioLevel` pipeline as the `bars` waveform style,
  so users can see whether their voice is too quiet without
  interrupting the transcript.

- **Smoother audio capture for visualisation + streaming.** The
  Linux PulseAudio backend now invokes `parec --latency-msec=20`
  so PCM lands in small frequent chunks (~20 ms). Without this PA
  picked a default fragment of several hundred ms, which made the
  waveform overlay's RMS tail look frozen between chunks and added
  end-of-utterance latency to the streaming pipeline.

### Changed

- The pre-existing `[overlay]` config block (which had unused
  `enabled`/`position`/`opacity` fields) is replaced in place with
  the new `waveform` / `style` / `volume_bar` shape. No other
  consumers existed in the workspace.



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

[0.10.0]: https://github.com/bogdanr/fono/compare/v0.9.1...v0.10.0
[0.9.1]: https://github.com/bogdanr/fono/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/bogdanr/fono/compare/v0.8.2...v0.9.0
[0.8.2]: https://github.com/bogdanr/fono/compare/v0.8.1...v0.8.2
[0.8.1]: https://github.com/bogdanr/fono/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/bogdanr/fono/compare/v0.7.1...v0.8.0
[0.7.1]: https://github.com/bogdanr/fono/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/bogdanr/fono/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/bogdanr/fono/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/bogdanr/fono/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/bogdanr/fono/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/bogdanr/fono/compare/v0.3.7...v0.4.0
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
