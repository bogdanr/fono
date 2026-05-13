# Fono provider matrix

Fono ships with one **speech-to-text (STT)** engine and one **LLM cleanup**
engine active at a time. Both are selected in `~/.config/fono/config.toml`
and can be swapped at any time with `fono use`, `fono setup`, or by editing
the file directly. API keys are stored in `~/.config/fono/secrets.toml`
(mode 0600, never logged) or read from `$ENV_VAR`.

## Capability matrix

The wizard, tray, `fono use cloud`, and `fono doctor` all consume the
single capability catalogue defined in
`fono_core::provider_catalog::CLOUD_PROVIDERS`. The matrix below mirrors
that catalogue; **new** marks TTS backends added in the v2 wizard
rework (issue #11). See
[ADR 0025](decisions/0025-cloud-provider-catalogue.md) for the design
rationale.

| Provider       | STT | LLM cleanup | Assistant chat | Vision                       | Web search                      | TTS                       |
|----------------|-----|-------------|----------------|------------------------------|----------------------------------|---------------------------|
| **OpenAI**     | ✓   | ✓           | ✓              | ✓ (`gpt-5.4-mini`)          | ✓ `web_search_preview`           | ✓ `tts-1`                 |
| **Groq**       | ✓   | ✓           | ✓              | ✓ (Llama-4 Maverick)         | —                                | ✓ PlayAI **new**          |
| **Anthropic**  | —   | ✓           | ✓              | ✓ (Claude Haiku 4.5)         | ✓ `web_search_20250305`          | —                         |
| **Cerebras**   | —   | ✓           | ✓              | —                            | —                                | —                         |
| **Gemini**     | —   | ✓ *(planned)* | ✓ *(planned)* | ✓ (Flash)                    | ✓ `google_search` *(planned)*    | —                         |
| **OpenRouter** | —   | ✓           | ✓              | *(route-dependent)*          | *(route-dependent)*              | ✓ Kokoro **new**          |
| **Cartesia**   | ✓   | —           | —              | —                            | —                                | ✓ Sonic-2 **new**         |
| **Deepgram**   | ✓   | —           | —              | —                            | —                                | ✓ Aura-2 **new**          |
| **AssemblyAI** | ✓   | —           | —              | —                            | —                                | —                         |

Picking OpenAI or Groq as the primary cloud provider configures every
capability in that row from a single key prompt; picking Anthropic or
Cerebras configures LLM + Assistant and asks an opt-in secondary
question for STT and/or TTS, defaulting to "key already set" providers
when their key is in `secrets.toml`. **Five** providers can now drive
the assistant end-to-end (OpenAI, Groq, Anthropic + any new cloud TTS,
Cerebras + any new cloud TTS, OpenRouter) — up from OpenAI-only before
this release.

## Switching providers (no daemon restart)

The smallest valid cloud config is two lines plus one key:

```toml
[stt]
backend = "groq"     # or openai, deepgram, …
[llm]
backend = "cerebras" # or none, openai, anthropic, groq, openrouter, ollama, local
enabled = true
```

…and `GROQ_API_KEY` + `CEREBRAS_API_KEY` either in `secrets.toml` or
exported in the environment. The factories fall through to the canonical
env-var name when the optional `[stt.cloud]` / `[llm.cloud]` sub-block is
absent — there is no need to repeat the provider name twice.

Once that is in place, switching providers is one command:

```sh
fono use stt groq         # flip STT only
fono use llm cerebras     # flip LLM only
fono use cloud cerebras   # paired preset (STT=Groq + LLM=Cerebras)
fono use local            # whisper-local + skip LLM
fono use show             # print active selection + key refs
```

Each `fono use` writes the change atomically and then issues a hot-reload
to any running daemon (no restart, no lost state). Per-call overrides
without persisting use the same backend names:

```sh
fono record --stt openai --llm anthropic
fono transcribe sample.wav --stt groq --llm none
```

API keys for as many providers as you like coexist in `secrets.toml`:

```sh
fono keys add GROQ_API_KEY
fono keys add CEREBRAS_API_KEY
fono keys list                   # masked listing
fono keys check                  # reachability probe per key
```

## Speech-to-text

| Backend       | Type       | Model(s)                               | API key env var       | Streaming |
|---------------|------------|----------------------------------------|-----------------------|-----------|
| Whisper local | local      | ggml tiny · tiny.en · base · base.en · small · small.en · medium · large-v3 | — | no |
| Groq          | cloud HTTP | `whisper-large-v3`, `whisper-large-v3-turbo` | `GROQ_API_KEY`        | yes (pseudo-stream, opt-in) |
| OpenAI        | cloud HTTP | `whisper-1`, `gpt-4o-transcribe`       | `OPENAI_API_KEY`      | no |
| Deepgram      | cloud WS   | `nova-2`, `nova-3`                     | `DEEPGRAM_API_KEY`    | yes |
| Cartesia      | cloud HTTP | `sonic-transcribe`                     | `CARTESIA_API_KEY`    | yes |
| AssemblyAI    | cloud HTTP | `best`, `nano`                         | `ASSEMBLYAI_API_KEY`  | yes |

Whisper model files land in `~/.cache/fono/models/whisper/ggml-<name>.bin`.
Override the download host with `FONO_MODEL_MIRROR=https://your.mirror`.

> **Note for CI / forks.** `GROQ_API_KEY` is also consumed by the
> release-time cloud equivalence gate (`.github/workflows/release.yml`'s
> `cloud-equivalence` job). The job is auto-skipped on tags pushed
> from forks (where the secret is not exposed) and on tags carrying
> the `-no-cloud-gate` suffix. End users do not need to set this for
> normal Fono usage; it's only consumed by your own CI when *you*
> tag a release. See `docs/dev/release-checklist.md`.

### Groq streaming dictation (pseudo-stream)

Groq has no native streaming endpoint today. Fono ships an opt-in
"pseudo-stream" backend that re-POSTs the trailing 28 s of buffered
audio to the standard batch endpoint every 700 ms while the user is
speaking, pipes each decode through a stable-prefix agreement
helper, and emits preview text into the live overlay. On
`SegmentBoundary` / `Eof` a single final POST against the full
segment audio produces the finalized transcript.

Trade-off: roughly 25–40× the dollar cost per utterance vs the
single batch POST that the non-live `record` path uses, because each
preview tick re-uploads the trailing window. On a usage-billed Groq
plan, opt in deliberately.

Enable with the master live-dictation switch:

```toml
[interactive]
enabled = true

[stt.cloud]
provider = "groq"
api_key_ref = "GROQ_API_KEY"
model = "whisper-large-v3-turbo"
```

The wizard sets this automatically when you pick Groq and answer
"yes" to live mode — there is no separate streaming opt-in. To
bound cost, set `interactive.streaming_interval` above `3.0` (only
finalize requests fire on VAD boundaries; previews are disabled) or
set `interactive.budget_ceiling_per_minute_umicros` to a hard cap.

Design + cost rationale: [ADR 0020](decisions/0020-groq-pseudo-stream.md).

## LLM cleanup

| Backend            | Type         | Default model                 | API key env var        |
|--------------------|--------------|-------------------------------|------------------------|
| Llama local        | local GGUF   | `qwen2.5-1.5b-instruct` (q4_k_m) | —                    |
| Cerebras           | cloud HTTP   | `llama-3.3-70b`               | `CEREBRAS_API_KEY`     |
| Groq               | cloud HTTP   | `llama-3.3-70b-versatile`     | `GROQ_API_KEY`         |
| OpenAI-compatible  | cloud HTTP   | `gpt-4o-mini` (configurable)  | `OPENAI_API_KEY`       |
| Anthropic          | cloud HTTP   | `claude-3-5-haiku-latest`     | `ANTHROPIC_API_KEY`    |

GGUF model files land in `~/.cache/fono/models/llm/`. The `enabled` flag in
`[llm]` can be set to `false` to skip cleanup entirely — in which case Fono
types the raw STT output verbatim.

### Short-utterance handling and clarification refusals

Any chat-trained LLM — cloud or local — can occasionally interpret a
very short capture as a conversational fragment and reply with a
clarification question (*"Could you provide the full text…"*) instead
of a cleaned transcript. Observed across Cerebras, Groq, OpenAI,
OpenRouter, Ollama, Anthropic, and the local llama.cpp backend; not a
provider-specific quirk. Fono mitigates this uniformly across every
backend in three ways:

- `[llm].skip_if_words_lt` (default `3`) bypasses the LLM entirely for
  one- and two-word captures, regardless of which backend is active.
- The default cleanup prompt explicitly forbids clarification questions
  and wraps the user message in `<<<` / `>>>` delimiters so the
  transcript cannot be mistaken for a chat message. Same prompt for
  all backends.
- Clarification-shaped replies are detected post-hoc and rejected; the
  raw STT text is injected instead and the daemon logs `LLM returned a
  clarification reply… falling back to raw text.` Same detector for
  all backends.

### Multilingual STT and language stickiness

Fono treats every entry of `general.languages` as an equal peer — there is
no primary/secondary distinction. Cloud STT calls go out **without** a
forced `language=` so the provider's auto-detect handles language switching
(e.g. dictating Romanian, then English, then Romanian again) for free.

Some providers (notably Groq's `whisper-large-v3-turbo` for non-native
English speakers) occasionally misdetect — e.g. flagging accented English
as Russian. Fono's defence is a tiny in-memory cache of the most recently
correctly-detected language per backend:

- On every successful in-allow-list detection, the cache records the code.
- When the provider returns a *banned* (out-of-allow-list) detection and
  the cache holds a peer code for that backend, Fono re-issues the same
  audio once with `language=<cached>` — a self-healing rerun that recovers
  from one Turbo misfire per occurrence.
- Order of `general.languages` is **not** consulted. The cache reflects
  what was actually heard; the config is just an unordered set.

The cache is in-memory only (no file I/O). On daemon start the OS locale's
alpha-2 code is used to seed the cache *if* it appears in the configured
allow-list; otherwise the cache stays empty and the first banned
detection is accepted as-is, with the cache populating from the next
correctly-detected utterance.

Knob: `[stt.cloud].cloud_rerun_on_language_mismatch` (default `true`).
Tray submenu: **Languages** → checkbox per peer + "Clear language memory".

Design rationale: [ADR 0017](decisions/0017-cloud-stt-language-stickiness.md).

## Default picks (rationale)

* **Local default:** `whisper small` (466 MB, multilingual) + `Qwen2.5-1.5B-Instruct`
  (1.0 GB, Apache-2.0). Runs on any 4-core x86_64 at ~2 s latency for a
  10-second utterance; idle RAM ~30 MB, active ~1.3 GB.
* **Cloud default:** Groq whisper-large-v3 + Cerebras llama-3.3-70b — sub-1 s
  latency end-to-end, generous free tiers, permissive TOS.

## Text-to-speech (assistant audio replies)

The voice assistant streams its reply sentence-by-sentence to whichever
TTS backend is selected in `[tts].backend`. v2 (issue #11) adds four
cloud providers next to the existing Wyoming + OpenAI options so
assistant audio works without an OpenAI key.

| Backend       | Type       | Default model       | Endpoint                                               | Auth header                  |
|---------------|------------|---------------------|--------------------------------------------------------|------------------------------|
| Wyoming       | local LAN  | server-side voice   | `tcp://<host>:10200`                                   | —                            |
| OpenAI        | cloud HTTP | `tts-1`             | `https://api.openai.com/v1/audio/speech`               | `Authorization: Bearer <k>`  |
| Groq          | cloud HTTP | `playai-tts`        | `https://api.groq.com/openai/v1/audio/speech`          | `Authorization: Bearer <k>`  |
| OpenRouter    | cloud HTTP | `hexgrad/kokoro-82m`| `https://openrouter.ai/api/v1/audio/speech`            | `Authorization: Bearer <k>`  |
| Cartesia      | cloud HTTP | `sonic-2`           | `https://api.cartesia.ai/tts/bytes`                    | `X-API-Key: <k>`             |
| Deepgram      | cloud HTTP | `aura-2-thalia-en`  | `https://api.deepgram.com/v1/speak`                    | `Authorization: Token <k>`   |

`CARTESIA_API_KEY` and `DEEPGRAM_API_KEY` may already be in
`secrets.toml` from STT usage — the assistant reuses them, so flipping
the assistant onto Cartesia or Deepgram TTS doesn't require a fresh
key prompt for existing users.

### Groq TTS

Groq exposes an OpenAI-compatible TTS endpoint at
`https://api.groq.com/openai/v1/audio/speech`. Fono points its
parameterised OpenAI-compat client at that base URL with model
`playai-tts` and voice `Fritz-PlayAI` (neutral male, close to OpenAI's
`alloy` baseline). Request/response shape is identical to OpenAI's —
24 kHz raw PCM in the response body.

**Note:** the PlayAI model family is currently flagged as **beta-tier**
on Groq's plan dashboard. Rate limits are tighter than the rest of the
Groq catalogue. If you plan to drive Fono's assistant heavily, set up
rate-limit alerts on the Groq dashboard so a sudden 429 cascade
surfaces explicitly. (Fono's `critical_notify` already pops a desktop
notification on the first 429 per session, but you'll want billing-side
visibility too.)

### OpenRouter TTS (Kokoro)

OpenRouter ships its own OpenAI-compatible TTS endpoint at
`https://openrouter.ai/api/v1/audio/speech`. The default model is
`hexgrad/kokoro-82m` (Kokoro, $0.62 / 1M chars at the time of writing),
with default voice `af_heart`. Same body shape as OpenAI; the response
is 24 kHz raw PCM. Useful for users who already route their LLM through
OpenRouter and want a single key covering chat + audio.

### Cartesia TTS (Sonic-2)

Cartesia uses a native (non-OpenAI-compatible) `POST /tts/bytes`
endpoint at `https://api.cartesia.ai/tts/bytes`. Fono pins model
`sonic-2` and voice id `a0e99841-438c-4a64-b679-ae501e7d6091`
(Cartesia's neutral English preset). The request asks for raw
`pcm_s16le` @ 24 kHz to match the assistant's audio pipeline; the
response body is contiguous PCM with no header. Auth header is
`X-API-Key: <CARTESIA_API_KEY>`. Sonic-2 is the lowest-latency premium
voice in the catalogue — recommended for users who want the most
natural-sounding replies.

### Deepgram TTS (Aura-2)

Deepgram's `POST /v1/speak` endpoint at
`https://api.deepgram.com/v1/speak` takes a JSON body of the shape
`{"text": "..."}`. The voice is encoded in the model id; the default
is `aura-2-thalia-en` (English, female, calm). Response is linear16
PCM at 24 kHz. Auth header is `Authorization: Token <DEEPGRAM_API_KEY>`
(the literal word `Token`, not `Bearer` — a frequent confusion).

## Assistant capabilities

The voice assistant (F8 by default) can opt into two server-side
extras when the chosen primary provider supports them. See
[ADR 0024](decisions/0024-assistant-multimodal-and-search.md) for the
full design.

| Provider   | Vision (multimodal model)              | Web search (native tool)            |
|------------|-----------------------------------------|--------------------------------------|
| OpenAI     | `gpt-5.4-mini` (same as text default)   | `web_search_preview`                 |
| Anthropic  | `claude-haiku-4-5-20251001`             | `web_search_20250305`                |
| Gemini     | `gemini-1.5-flash`                      | `google_search` *(not yet wired)*    |
| Groq       | `llama-4-maverick-17b-128e-instruct`    | —                                    |
| Cerebras   | —                                       | —                                    |
| OpenRouter | *(route-dependent — deferred)*          | *(route-dependent — deferred)*       |

Two config flags in `[assistant]` drive the runtime behaviour:

* `prefer_vision = true` — the assistant builder swaps the provider's
  `text_model` for `multimodal_model` at startup. If the chosen
  provider has no multimodal variant (e.g. Cerebras), Fono logs a
  warning and stays on the text model. **Screen-capture is not yet
  implemented** — the model variant is selected but Fono does not
  yet attach images to user turns. Manual image input via the
  assistant remains a follow-up.
* `prefer_web_search = true` — the assistant's per-provider chat
  client appends the matching native tool to every request body:
  * OpenAI: `tools: [{"type":"web_search_preview"}]`.
  * Anthropic: `tools: [{"type":"web_search_20250305","name":"web_search","max_uses":3}]`.
  * Gemini: `tools: [{"google_search": {}}]` *(declared but not yet
    wired — Gemini chat client is a follow-up).*
  For providers whose catalogue entry says `WebSearchSupport::None`,
  the flag is a no-op (no tool is injected). Each invocation logs a
  one-line `info!` at target `fono.assistant` when the tool is active.

Both flags default to `false` and can be toggled from the wizard's
**Optional extras** MultiSelect after picking an assistant backend, or
edited directly in `~/.config/fono/config.toml`:

```toml
[assistant]
prefer_vision = false
prefer_web_search = false
```

## Adding a new backend

Implement the `fono_stt::SpeechToText` or `fono_llm::TextCleanup` async
trait, register the factory in `crates/fono-{stt,llm}/src/registry.rs`,
then expose the new variant in `fono_core::config::{SttBackend,LlmBackend}`.
See `CONTRIBUTING.md` for full coding guidelines.
