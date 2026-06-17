# Fono provider matrix

Fono ships with one **speech-to-text (STT)** engine and one **polish**
engine active at a time. Both are selected in `~/.config/fono/config.toml`
and can be swapped at any time with `fono use`, `fono setup`, or by editing
the file directly. API keys are stored in `~/.config/fono/secrets.toml`
(mode 0600, never logged) or read from `$ENV_VAR`.

## Capability matrix

The wizard, tray, `fono use cloud`, and `fono doctor` all consume the
single capability catalogue defined in
`fono_core::provider_catalog::CLOUD_PROVIDERS`. The matrix below mirrors
that catalogue. See
[ADR 0025](decisions/0025-cloud-provider-catalogue.md) for the design
rationale.

| Provider       | STT | polish | Assistant chat | Vision                       | Web search                      | TTS                       |
|----------------|-----|-------------|----------------|------------------------------|----------------------------------|---------------------------|
| **OpenAI**     | ✓   | ✓           | ✓              | ✓ (`gpt-5.4-mini`)          | ✓ `web_search_preview`           | ✓ `tts-1`                 |
| **Groq**       | ✓   | ✓           | ✓              | —                            | —                                | ✓ Orpheus                 |
| **Anthropic**  | —   | ✓           | ✓              | ✓ (Claude Haiku 4.5)         | ✓ `web_search_20250305`          | —                         |
| **Cerebras**   | —   | ✓           | ✓              | —                            | —                                | —                         |
| **Gemini**     | ✓   | ✓           | ✓              | ✓ (`gemini-flash-lite-latest`) | native `google_search` *(not wired)* | ✓ native (24 kHz, 30 voices) |
| **OpenRouter** | ✓   | ✓           | ✓              | *(route-dependent)*          | *(route-dependent)*              | ✓ OpenAI Mini TTS         |
| **Cartesia**   | ✓   | —           | —              | —                            | —                                | ✓ Sonic-3.5               |
| **Deepgram**   | ✓   | —           | —              | —                            | —                                | ✓ Aura-2                  |
| **AssemblyAI** | ✓   | —           | —              | —                            | —                                | —                         |
| **Speechmatics** | ✓ | —           | —              | —                            | —                                | ✓ preview (English)       |
| **ElevenLabs** | ✓   | —           | —              | —                            | —                                | ✓ Eleven v3               |

Picking any cloud provider as the *primary* now walks its whole row:
every capability the provider covers with a wired backend is taken
from the catalogue, and every capability it doesn't cover transparently
leans on the local backend (local Whisper for STT, embedded GGUF for
cleanup, on-device Piper/Kokoro for TTS). The primary picker therefore
lists **every** provider with at least one wired capability — including
speech-only ones like Speechmatics, Deepgram, AssemblyAI, and Cartesia
— not just the LLM-capable rows. Assistant chat stays opt-in. Five
providers can drive the assistant end-to-end today: OpenAI, Groq,
Anthropic + any cloud TTS, Cerebras + any cloud TTS, and OpenRouter.

The wizard's cloud STT picker, cloud LLM/cleanup picker, and API-key
reachability validation are generated entirely from this catalogue
(each entry carries a `key_validation` probe descriptor). Adding a new
provider is a single `CLOUD_PROVIDERS` edit — it then surfaces in every
wizard list and validates its key with no changes to the wizard code.

## Switching providers (no daemon restart)

The smallest valid cloud config is two lines plus one key:

```toml
[stt]
backend = "groq"     # or openai, deepgram, …
[polish]
backend = "cerebras" # or none, openai, anthropic, groq, openrouter, ollama, local
enabled = true
```

…and `GROQ_API_KEY` + `CEREBRAS_API_KEY` either in `secrets.toml` or
exported in the environment. The factories fall through to the canonical
env-var name when the optional `[stt.cloud]` / `[polish.cloud]` sub-block is
absent — there is no need to repeat the provider name twice.

Once that is in place, switching providers is one command:

```sh
fono use stt groq         # flip STT only
fono use polish cerebras     # flip polish only
fono use cloud cerebras   # paired preset (STT=Groq + Polish=Cerebras)
fono use local            # whisper-local + skip polish
fono use show             # print active selection + key refs
```

Each `fono use` writes the change atomically and then issues a hot-reload
to any running daemon (no restart, no lost state). Per-call overrides
without persisting use the same backend names:

```sh
fono record --stt openai --polish anthropic
fono transcribe sample.wav --stt groq --polish none
```

API keys for as many providers as you like coexist in `secrets.toml`:

```sh
fono keys add GROQ_API_KEY
fono keys add CEREBRAS_API_KEY
fono keys list                   # masked listing
fono keys check                  # reachability probe per key
```

## Debugging slow or stalled cloud requests

Every cloud-backed pipeline (STT, polish, voice-assistant chat,
TTS, wizard key validation) emits one structured log line per HTTP
request under the `fono.http` tracing target. The lines are silent at
the default `info` log level and turn on per session via the
`RUST_LOG` env var:

```
RUST_LOG=info,fono.http=debug fono
```

Schema (one line per HTTP request):

| Field             | Meaning                                                |
|-------------------|--------------------------------------------------------|
| `stage`           | `stt` / `polish` / `assistant` / `tts` / `wizard`         |
| `provider`        | `openrouter` / `openai` / `groq` / `cerebras` / ...    |
| `endpoint`        | last URL segment, e.g. `audio/speech`                  |
| `status`          | HTTP status                                            |
| `headers_ms`      | time to response headers                               |
| `ttfb_ms`         | time from headers to first body byte                   |
| `body_ms`         | time from first byte to last byte                      |
| `decode_ms`       | post-processing (WAV strip, JSON parse, ...)           |
| `total_ms`        | request-start → decode-done                            |
| `body_bytes`      | actual bytes read                                      |
| `chunks`          | stream chunk count (1 for one-shot bodies)             |
| `request_id`      | upstream `x-request-id` (paste into provider support)  |
| `attempt`         | 1 on first try, 2 on retried                           |
| `outcome`         | `ok` / `stalled` / `http_error` / `decode_error` / ... |

Each backend uses an inter-chunk watchdog so a stalled body fails
fast rather than waiting for the overall reqwest timeout: TTS 15 s,
STT 30 s, polish 30 s, assistant SSE 20 s inter-event. TTS
retries once automatically on a stall.

## Speech-to-text

| Backend       | Type       | Model(s)                               | API key env var       | Streaming |
|---------------|------------|----------------------------------------|-----------------------|-----------|
| Whisper local | local      | ggml `tiny` · `tiny.en` · `small` · `small.en` · `large-v3-turbo` (per ADR 0027) | — | no |
| Groq          | cloud HTTP | `whisper-large-v3`, `whisper-large-v3-turbo` | `GROQ_API_KEY`        | yes (pseudo-stream, opt-in) |
| OpenAI        | cloud HTTP | `whisper-1`, `gpt-4o-transcribe`       | `OPENAI_API_KEY`      | no |
| Deepgram      | cloud WS   | `nova-2`, `nova-3`                     | `DEEPGRAM_API_KEY`    | yes |
| Cartesia      | cloud HTTP | `ink-whisper` (batch only; `ink-2` is realtime-only and arrives in a Phase 2 streaming slice) | `CARTESIA_API_KEY`    | no  |
| AssemblyAI    | cloud HTTP | `best`, `nano`                         | `ASSEMBLYAI_API_KEY`  | yes |
| Speechmatics  | cloud WS   | `enhanced`, `standard` (accuracy tiers) | `SPEECHMATICS_API_KEY` | yes |
| ElevenLabs    | cloud HTTP | `scribe_v1` (Scribe; batch only) | `ELEVENLABS_API_KEY`  | no  |

Whisper model files land in `~/.cache/fono/models/whisper/ggml-<name><suffix>.bin`
where `<suffix>` is empty for fp16 or `-q5_1` / `-q8_0` for the
shipped quantizations. The pick per model is driven by the
acceptance rule in [ADR 0027](decisions/0027-stt-quantization-ladder.md):

| Rung | Multilingual | English-only | Default file | Approx size |
|---|---|---|---|---:|
| T1 minimal | `tiny` | `tiny.en` | `ggml-<name>-q5_1.bin` | 31 MB |
| T2 sweet spot | `small` | `small.en` | `ggml-small-q5_1.bin` / `ggml-small.en-q8_0.bin` | 182 / 253 MB |
| T3 quality | `large-v3-turbo` | `large-v3-turbo` | `ggml-large-v3-turbo-q8_0.bin` | 834 MB |

Users override the picked quantization with `[stt.local].quantization`
(`auto` | `fp16` | `q8_0` | `q5_1`). `auto` is the default and resolves
to the table above. `base` / `base.en` are intentionally absent: the
perf-pass found them dominated by T2 on every reference host
(strictly better English-fixture accuracy at similar RTF for ~40 MB
more disk).

Override the download host with `FONO_MODEL_MIRROR=https://your.mirror`.

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
[overlay]
style = "transcript"

[stt.cloud]
provider = "groq"
api_key_ref = "GROQ_API_KEY"
model = "whisper-large-v3-turbo"
```

Pick the Transcript overlay style (tray *Preferences → Waveform style →
Transcript*, or set `[overlay].style = "transcript"` by hand) and Fono
auto-routes the Groq STT through the pseudo-stream lane. To bound cost,
set `interactive.streaming_interval` above `3.0`: only finalize requests
fire on VAD boundaries, previews are disabled.

Design + cost rationale: [ADR 0020](decisions/0020-groq-pseudo-stream.md).

### Deepgram STT (Nova-3)

Deepgram's `POST /v1/listen` endpoint at
`https://api.deepgram.com/v1/listen` takes the raw audio as the
request body (no multipart form). Fono uploads WAV — the same encoder
shared with the Groq path — so the sample rate and channel count
travel with the audio and we don't have to thread them through query
parameters. Per-request settings (`model`, `language` or
`language=multi`, `smart_format`, `punctuate`) go on the URL.

```toml
[stt]
backend = "deepgram"

[stt.cloud]
provider = "deepgram"
api_key_ref = "DEEPGRAM_API_KEY"
model = "nova-3"   # or "nova-2" for broader multilingual coverage
```

Auth header gotcha: Deepgram uses `Authorization: Token <key>` —
literally the word `Token`, **not** `Bearer`. A copy-paste from
Groq / OpenAI will return 401 with no hint that the prefix is wrong.

Language stickiness behaviour is the same as Groq: forced
(`general.languages = ["en"]`) sends `language=en`; auto / allow-list
sends `language=multi` and Fono post-validates the returned
alpha-2 code against the allow-list. On a mismatch with
`cloud_rerun_on_language_mismatch = true` (default), Fono runs one
forced request per peer and picks the response with the highest
top-alternative `confidence` (Deepgram's batch endpoint doesn't
expose per-segment `avg_logprob`, so confidence is the tiebreak signal).

Model menu:

* `nova-3` (default) — production-default Deepgram model. Lowest
  latency, best English WER. Multilingual matrix is smaller than
  `nova-2`.
* `nova-2` — broader multilingual coverage. Pin
  `[stt.cloud].model = "nova-2"` when `general.languages` lists a
  code Nova-3 doesn't cover at full quality.

### Deepgram streaming dictation (WebSocket)

Unlike Groq, Deepgram has a first-class realtime endpoint. With
`[overlay].style = "transcript"` Fono opens a single
`wss://api.deepgram.com/v1/listen` WebSocket per session and streams
16 kHz s16le mono PCM as binary frames. Partial transcripts (`Results`
with `is_final: false`) paint into the overlay at ~150 ms cadence;
`is_final: true` finalize frames commit a segment. Deepgram's
`UtteranceEnd` VAD event drives the same `SegmentBoundary` signal the
local Whisper streaming path emits, so the overlay's "Pondering…" +
auto-stop hook works without backend-specific code.

Cost note: Deepgram bills by audio seconds processed, **not** per
request. That makes the streaming path *cheaper* than Groq's
pseudo-stream (which re-uploads the trailing window on every cadence
tick) — opt in freely.

### Speechmatics STT (realtime WebSocket)

Speechmatics has no batch-vs-stream split that matters for
push-to-talk: Fono opens the realtime endpoint at
`wss://eu.rt.speechmatics.com/v2`, sends a `StartRecognition`
message, streams the buffered capture as binary `AddAudio` frames,
then `EndOfStream`, and collects the `AddTranscript` finals as the
buffer drains. This reuses the same `tokio-tungstenite` dependency
the Deepgram streaming path already pulls in — no new crates.

```toml
[stt]
backend = "speechmatics"

[stt.cloud]
provider = "speechmatics"
api_key_ref = "SPEECHMATICS_API_KEY"
```

Auth header gotcha: Speechmatics uses `Authorization: Bearer <key>`
on the WebSocket handshake — the **opposite** of Deepgram's literal
`Token` prefix. A unit test pins the `Bearer` form so a copy-paste
from the Deepgram client can't regress it.

The `model` knob selects the accuracy tier via the realtime
`operating_point` (`enhanced` for best accuracy, `standard` for
lower latency/cost). Region is the EU realtime host by default; set
a different `[stt.cloud]` endpoint to target another region.

### ElevenLabs STT (Scribe)

ElevenLabs' Scribe model transcribes via a batch
`POST https://api.elevenlabs.io/v1/speech-to-text` multipart upload:
`model_id` (`scribe_v1`) plus the buffered capture as a `file` part.
A forced language goes in the optional `language_code` form field
(ISO 639-1/3), not a query parameter. Auth is the `xi-api-key: <key>`
header.

```toml
[stt]
backend = "elevenlabs"

[stt.cloud]
provider = "elevenlabs"
api_key_ref = "ELEVENLABS_API_KEY"
```

Scribe returns `{ language_code, language_probability, text, words }`
with **no** per-segment confidence scores, so — like Cartesia's
`ink-whisper` — Fono cannot run the Whisper-style logprob rerun or the
silence-hallucination filter. When `general.cloud_rerun_on_language_mismatch
= true` the backend logs a single warning per process to flag the
degradation and otherwise accepts the detected response. Scribe also
has no equivalent of Whisper's `prompt` field; any `[stt.prompts]`
entries are accepted for forward compatibility but unused on the wire.

## polish

| Backend            | Type         | Default model                 | API key env var        |
|--------------------|--------------|-------------------------------|------------------------|
| `local`            | embedded GGUF (llama.cpp) | `gemma-4-e2b` (q4_0) | —                  |
| `ollama`           | local/remote HTTP server (manual) | `[polish.cloud].model` | — |
| Cerebras           | cloud HTTP   | `llama-3.3-70b`               | `CEREBRAS_API_KEY`     |
| Groq               | cloud HTTP   | `llama-3.3-70b-versatile`     | `GROQ_API_KEY`         |
| OpenAI-compatible  | cloud HTTP   | `gpt-4o-mini` (configurable)  | `OPENAI_API_KEY`       |
| Anthropic          | cloud HTTP   | `claude-3-5-haiku-latest`     | `ANTHROPIC_API_KEY`    |
| Gemini             | cloud HTTP (OpenAI-compat) | `gemini-flash-lite-latest` | `GEMINI_API_KEY`       |

`backend = "local"` always runs the **embedded** `llama-cpp-2` engine on a
local GGUF — it never talks to an Ollama server. The GGUF is downloaded to
`~/.cache/fono/models/polish/<model>.gguf` on first run; if it is missing,
Fono surfaces a one-shot notification pointing at `fono models install
<model>` and injects the raw transcript until the model is present.

To use an Ollama (or any OpenAI-compatible) **server** for cleanup instead,
set `backend = "ollama"` and point `[polish.cloud].api_key_ref` at the
endpoint URL (default `http://localhost:11434/v1/chat/completions`) with the
served model in `[polish.cloud].model`. This is a manual opt-in; the setup
wizard never configures a server for the "local polish" choice.

The `enabled` flag in
`[polish]` can be set to `false` to skip cleanup entirely — in which case Fono
types the raw STT output verbatim.

### Gemini (single key, free tier)

Google support in Fono is the **Gemini API** (Google AI Studio), not Google
Cloud Speech. A single `GEMINI_API_KEY` from
<https://aistudio.google.com/apikey> configures every Gemini capability, and it
works on Google's **free tier** — no billing account, just an active project.
See [ADR 0034](decisions/0034-google-via-gemini-single-key.md) for the rationale
(and why the Cloud Speech / Chirp service-account path was dropped).

```toml
[polish]
backend = "gemini"          # gemini-flash-lite-latest via the OpenAI-compatible surface
```

Polish, STT, the staged assistant chat, and native TTS are all wired today on
the single key; the realtime (Live API) assistant is the remaining follow-up.
Polish and the staged assistant reuse Gemini's OpenAI-compatible endpoint
(`/v1beta/openai/chat/completions`, `Authorization: Bearer <key>`); STT, TTS, and
Live use the native `generateContent` / `BidiGenerateContent` surfaces
(`x-goog-api-key: <key>`).

**TTS.** `[tts] backend = "gemini"` uses the native
`gemini-3.1-flash-tts-preview` model (`generateContent` with
`responseModalities: ["AUDIO"]`), returning 24 kHz mono PCM. There are 30
prebuilt voices (`Kore`, `Puck`, `Charon`, …); Fono curates a gender-balanced
subset of ten into the positional voice palette (`fono voices`), default
`Kore`. Gemini TTS is multilingual (40+ languages incl. Romanian) and
auto-detects the spoken language from the text, so it is **not** wrapped by the
English-only local fallback.

**Streaming TTS.** Gemini TTS streams via `streamGenerateContent?alt=sse`:
Fono plays each PCM frame gaplessly as it arrives instead of waiting for the
whole clip, cutting assistant time-to-first-audio. Streaming is automatic for
streaming-capable cloud backends (the assistant pump, `fono speak`, and the MCP
`fono.speak` tool all use it); local engines and batch-only cloud backends keep
the synthesize-then-enqueue path unchanged. A small fixed jitter buffer
(300 ms, not configurable) is held back before the first frame so the device
never underruns mid-utterance.

**Free-tier limits.** Each model has its own requests-per-minute (RPM),
tokens-per-minute (TPM), and requests-per-day (RPD) caps; the daily counts reset
at **midnight Pacific**. A burst of dictation or a long assistant session can hit
the RPD wall — Fono surfaces the 429 as an actionable error and keeps running.
The TTS and Live models are **Preview**; their model ids and wire shapes may
change, so they live only in the catalogue and `fono doctor` reports the active
id. Gemini STT is prompt-driven transcription (no per-segment confidence,
batch-only), so it is an opt-in choice rather than the default — streaming
dictation (F7) stays on the dedicated streaming STT backends.

### Short-utterance handling and clarification refusals

Any chat-trained LLM — cloud or local — can occasionally interpret a
very short capture as a conversational fragment and reply with a
clarification question instead of a cleaned transcript. Fono mitigates
this uniformly across every backend: `[polish].skip_if_words_lt`
(default `3`) bypasses the polish step for one- and two-word captures,
the default prompt forbids clarification questions and delimits the
transcript with `<<<` / `>>>`, and clarification-shaped replies are
detected post-hoc and replaced with the raw STT text. See
[the troubleshooting recipe](troubleshooting.md#polish-responds-with-a-question-instead-of-cleaning-my-text)
for the user-facing symptoms and tuning options.

### Multilingual STT and language stickiness

Fono treats every entry of `general.languages` as an equal peer — there is
no primary/secondary distinction. Cloud STT calls go out **without** a
forced `language=` so the provider's auto-detect handles language switching
for free. When the provider returns a banned (out-of-allow-list) detection
Fono re-issues the same audio once with `language=<cached>` from a tiny
in-memory per-backend cache of recently-correct detections — a self-healing
rerun that recovers from one Turbo misfire per occurrence. Knob:
`[stt.cloud].cloud_rerun_on_language_mismatch` (default `true`); tray
submenu: **Languages** → checkbox per peer + "Clear language memory".

Design rationale:
[ADR 0017](decisions/0017-cloud-stt-language-stickiness.md); the
user-facing recipe lives in
[troubleshooting.md](troubleshooting.md#cloud-stt-keeps-detecting-the-wrong-language).

## Default picks (rationale)

* **Local default:** `whisper small` (resolves to `small-q5_1`, 182 MB, multilingual)
  + `Qwen2.5-1.5B-Instruct` (1.0 GB, Apache-2.0). Runs on any 4-core x86_64 at
  ~2 s latency for a 10-second utterance; idle RAM ~30 MB, active ~800 MB
  (down from ~1.3 GB on fp16). Per-model quantization picks are recorded
  in [ADR 0027](decisions/0027-stt-quantization-ladder.md).
* **Cloud presets:** `fono use cloud groq` pairs Groq whisper-large-v3-turbo
  with Groq llama-3.3-70b-versatile (single key, sub-1 s end-to-end).
  `fono use cloud cerebras` pairs Groq STT with Cerebras llama-3.3-70b
  (Cerebras has no STT, so STT falls back to Groq). Both have generous
  free tiers and permissive TOS. `fono use show` prints the active pair.

## Text-to-speech (assistant audio replies)

The voice assistant streams its reply sentence-by-sentence to whichever
TTS backend is selected in `[tts].backend`. v2 (issue #11) adds four
cloud providers next to the existing Wyoming + OpenAI options so
assistant audio works without an OpenAI key.

| Backend       | Type       | Default model       | Endpoint                                               | Auth header                  |
|---------------|------------|---------------------|--------------------------------------------------------|------------------------------|
| Wyoming       | local LAN  | server-side voice   | `tcp://<host>:10200`                                   | —                            |
| OpenAI        | cloud HTTP | `tts-1`             | `https://api.openai.com/v1/audio/speech`               | `Authorization: Bearer <k>`  |
| Groq          | cloud HTTP | `canopylabs/orpheus-v1-english` | `https://api.groq.com/openai/v1/audio/speech`          | `Authorization: Bearer <k>`  |
| OpenRouter    | cloud HTTP | `openai/tts-1`      | `https://openrouter.ai/api/v1/audio/speech`            | `Authorization: Bearer <k>`  |
| Cartesia      | cloud HTTP | `sonic-3.5`         | `https://api.cartesia.ai/tts/bytes`                    | `X-API-Key: <k>`             |
| Deepgram      | cloud HTTP | `aura-2-thalia-en`  | `https://api.deepgram.com/v1/speak`                    | `Authorization: Token <k>`   |
| Speechmatics  | cloud HTTP | preview (voice in URL) | `https://preview.tts.speechmatics.com/generate/<voice>` | `Authorization: Bearer <k>`  |
| ElevenLabs    | cloud HTTP | `eleven_v3` (voice in URL) | `https://api.elevenlabs.io/v1/text-to-speech/<voice>` | `xi-api-key: <k>`            |
| Gemini        | cloud HTTP | `gemini-3.1-flash-tts-preview` (voice in body) | `https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent` | `x-goog-api-key: <k>` |

`CARTESIA_API_KEY` and `DEEPGRAM_API_KEY` may already be in
`secrets.toml` from STT usage — the assistant reuses them, so flipping
the assistant onto Cartesia or Deepgram TTS doesn't require a fresh
key prompt for existing users.

### Groq TTS

Groq exposes an OpenAI-compatible TTS endpoint at
`https://api.groq.com/openai/v1/audio/speech`. Fono points its
parameterised OpenAI-compat client at that base URL with model
`canopylabs/orpheus-v1-english` (Canopy Labs' Orpheus) and voice
`hannah` (neutral female). Groq's hosted Orpheus exposes a curated
six-voice set — `autumn`, `diana`, `hannah`, `austin`, `daniel`,
`troy` — which is narrower than Canopy's open-source Orpheus
checkpoint (`tara`, `leah`, `jess`, `leo`, `dan`, `mia`, `zac`,
`zoe`); requesting one of those upstream-only voices against
Groq returns HTTP 400 (`voice must be one of ...`).
Request/response shape is identical to OpenAI's — 24 kHz raw PCM in
the response body.

Orpheus replaces the PlayAI family that previously powered Groq TTS,
which Groq decommissioned in 2026; requests against the retired model
ids now return `model_not_found`.

### OpenRouter TTS (OpenAI `tts-1`)

OpenRouter ships its own OpenAI-compatible TTS endpoint at
`https://openrouter.ai/api/v1/audio/speech`. The default model is
`openai/tts-1` (OpenAI's classical single-pass TTS, priced at
$15 / 1 M characters at the time of writing), with default voice
`alloy`. Same body shape as OpenAI; the response is 24 kHz raw PCM.
Useful for users who already route their LLM through OpenRouter and
want a single key covering chat + audio.

`tts-1` produces audio in roughly 0.5-2 s regardless of input
length, so the user hears the assistant's reply within a couple of
seconds.

The full OpenAI voice catalogue is available: `alloy`, `echo`,
`fable`, `onyx`, `nova`, `shimmer`, `sage`, `coral`, `ash`, `verse`.
Override the default by setting `voice` in `[tts.cloud]` of
`config.toml`, e.g.:

```toml
[tts.cloud]
provider = "openrouter"
voice = "sage"
```

#### Why not `gpt-4o-mini-tts`?

`openai/gpt-4o-mini-tts-2025-12-15` produces noticeably more
expressive voices and is natively multilingual (no per-language
voice map required), but OpenRouter's `/audio/speech` proxy was
empirically unable to forward that model's output reliably: the
proxy flushed an ~9.6 KB preamble immediately and then buffered
the rest of the synthesised body until upstream finished, which
for a typical 200-character assistant reply exceeded every
reasonable client timeout. Verified via the `fono.http`
instrumentation's one-shot stall hex dump — bytes were valid PCM,
just never delivered. `tts-1` sidesteps the buffering problem
because its synthesis is fast and single-pass; the whole body is
forwarded in one go before any proxy buffer matters.

Users who explicitly want the LLM-based voice can pin it in
`config.toml` and accept the failure mode on long replies (or
switch to OpenAI direct, where streaming works correctly):

```toml
[tts.cloud]
provider = "openrouter"
model = "openai/gpt-4o-mini-tts-2025-12-15"
voice = "coral"
```

When/if OpenRouter fixes their proxy's streaming behaviour, the
default may flip back.

### OpenRouter app attribution

Every outbound request Fono makes to `openrouter.ai` — STT, polish
cleanup, voice-assistant chat, TTS, and the wizard's
`validate_cloud_key` probe — carries three static app-attribution
headers per <https://openrouter.ai/docs/app-attribution>:

| Header | Value |
|---|---|
| `HTTP-Referer` | `https://fono.page` |
| `X-OpenRouter-Title` | `Fono` |
| `X-OpenRouter-Categories` | `personal-agent,writing-assistant` |

These values are baked into the binary and are identical across every
install — no per-user or per-machine identifier is embedded, and no
request body changes. The effect is that Fono appears on OpenRouter's
public rankings (https://openrouter.ai/rankings), in the "Apps" tab of
each model it routes through, and gets a public usage dashboard at
https://openrouter.ai/apps?url=https://fono.page. The shared source
of truth is `fono_core::openrouter_attribution`.

Kokoro (`hexgrad/kokoro-82m`, voice `af_heart`) was the previous
default. It is deferred to a future local-and-cloud-symmetric backend
with a shared `KokoroVoiceRouter` so picking Kokoro local vs cloud
yields the same audio output for the same `(text, lang, voice)`
triple — see
`plans/2026-05-14-kokoro-local-and-cloud-parity-v1.md`. Existing users
who prefer Kokoro today can pin
`[tts.cloud] model = "hexgrad/kokoro-82m"` and `voice = "af_heart"`
manually.

### Cartesia TTS (Sonic-3.5)

Cartesia uses a native (non-OpenAI-compatible) `POST /tts/bytes`
endpoint at `https://api.cartesia.ai/tts/bytes`. Fono pins model
`sonic-3.5` and voice id `a0e99841-438c-4a64-b679-ae501e7d6091`
(Cartesia's neutral English preset) as the fallback voice. The
request asks for raw `pcm_s16le` @ 24 kHz to match the assistant's
audio pipeline; the response body is contiguous PCM with no header.
Auth headers are `X-API-Key: <CARTESIA_API_KEY>` and
`Cartesia-Version: 2026-03-01` (the version pin is required — the
API 400s without it). Sonic-3.5 is the latest premium voice in the
catalogue with broad multilingual support — recommended for users
who want natural-sounding replies in any language.

**Per-language voice selection.** Each non-English code in
`general.languages` (and each language STT detects at runtime) gets
its own native voice, fetched lazily via
`GET /voices?language=<code>&limit=1` the first time we need to
synthesise in that language and cached for the process lifetime. A
multilingual user dictating in Romanian plays through a Romanian
voice with `language = "ro"` on the wire; the same user dictating
in English plays through the catalogue's dedicated English voice
with `language = "en"` — both native, not a single voice forced to
bilingual duty. Resolution order: explicit `tts.voice` config pin
wins; else the language STT detected on the current utterance; else
the first non-English entry in `general.languages`; else the English
fallback. Any failure (offline, auth, no voices for that language,
or a language code the model itself rejects) silently falls back to
the English voice — TTS never errors out *because of* voice routing.

### Deepgram TTS (Aura-2)

Deepgram's `POST /v1/speak` endpoint at
`https://api.deepgram.com/v1/speak` takes a JSON body of the shape
`{"text": "..."}`. The voice is encoded in the model id; the default
is `aura-2-thalia-en` (English, female, calm). Response is linear16
PCM at 24 kHz. Auth header is `Authorization: Token <DEEPGRAM_API_KEY>`
(the literal word `Token`, not `Bearer` — a frequent confusion).

### Speechmatics TTS (preview)

Speechmatics' preview TTS endpoint at
`https://preview.tts.speechmatics.com/generate/<voice>` takes a JSON
body of the shape `{"text": "..."}` and returns raw signed 16-bit
little-endian PCM at 16 kHz. The voice is chosen via the URL path —
there is no model selector. Auth header is
`Authorization: Bearer <SPEECHMATICS_API_KEY>` (the same `Bearer`
form as the STT socket).

Limitation: the preview service is **English-only** with four voices
(`sarah`, `theo`, `megan`, `jack`); the default is `sarah`. Set
`[tts].voice` to pick another. This is a documented preview
limitation, not a bug — broaden voice/language coverage when
Speechmatics promotes the endpoint out of preview.

### ElevenLabs TTS (Eleven v3)

ElevenLabs synthesises via
`POST https://api.elevenlabs.io/v1/text-to-speech/<voice_id>?output_format=pcm_24000`
with a JSON body of `{"text": "...", "model_id": "eleven_v3"}` and the
`xi-api-key: <ELEVENLABS_API_KEY>` header. The **voice is the path
segment** (not a body/query field); the catalogue default is
`EXAVITQu4vr4xnSDxMaL` ("Sarah" — a current default premade voice
present in every account, multilingual, and usable on the free tier).
Set `[tts].voice` to a different voice id to pin another speaker. The
response is raw signed 16-bit little-endian PCM at 24 kHz.

Eleven v3 is the expressive flagship model: it understands inline
**audio tags** like `[whispers]`, `[laughs]`, or `[excited]` and IPA
pronunciation hints for emotional/phonetic control (see the
[v3 prompting best-practices](https://elevenlabs.io/docs/overview/capabilities/text-to-speech/best-practices#prompting-eleven-v3)).
Fono posts plain dictation text and adds none of these itself, but
tags typed into an assistant reply pass through verbatim.

Plan note: the **free tier can use this API** (you get a monthly
character quota), but only with voices your account actually owns —
the *default premade* voices and any you add to "My Voices". Voices
from the shared **Voice Library** (and *professional*/cloned voices)
require a paid plan and are rejected for free users with
`402 paid_plan_required` ("Free users cannot use library voices via
the API"). The catalogue default ("Sarah") is a premade voice, so it
works out of the box; if you pin `[tts].voice` to a library voice on a
free key you'll get that 402. Eleven v3 renders 70+ languages, so it
is **not** flagged English-only.

### Gemini TTS (native, single key)

Gemini synthesises via
`POST https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent`
with the `x-goog-api-key: <GEMINI_API_KEY>` header and a body of
`{"contents":[{"parts":[{"text":"..."}]}], "generationConfig":{
"responseModalities":["AUDIO"], "speechConfig":{"voiceConfig":{
"prebuiltVoiceConfig":{"voiceName":"Kore"}}}}}`. The **voice is a body
field** (`voiceName`), and the catalogue default model is
`gemini-3.1-flash-tts-preview`. The response carries base64 raw signed
16-bit little-endian PCM in `candidates[0].content.parts[0].inlineData`;
the companion `mimeType` (`audio/L16;codec=pcm;rate=24000`) is parsed for
the sample rate, defaulting to 24 kHz.

There are 30 prebuilt voices (`Kore`, `Puck`, `Charon`, `Aoede`,
`Fenrir`, …); Fono curates a gender-balanced subset of ten into the
positional voice palette, default `Kore`. A per-call `[tts].voice`
override (or `fono voices`) selects any voice by name. Gemini TTS renders
40+ languages and auto-detects the spoken language from the text, so it
is **not** flagged English-only. The TTS model is **Preview** — its id
and wire shape may change — so it lives only in the catalogue.

For low-latency playback Gemini overrides `synthesize_stream` to call the
same model with `:streamGenerateContent?alt=sse`, decoding each SSE event's
`inlineData` PCM frame and pushing it to the gapless playback session as it
arrives (see *Streaming TTS* above).

### English-only voices and the automatic local fallback

Some cloud voices only render intelligible English: Groq's Orpheus
`…-english`, the Speechmatics preview above, and Deepgram's
`aura-2-…-en` voices. Sending them non-English text yields an English
phonemization of the foreign words — gibberish, not speech in that
language. The capability catalogue flags these backends with a single
`english_only` boolean (it defaults to `false`, so any unflagged or
newly-added provider is treated as multilingual — the historical
behaviour).

When the active backend is flagged English-only and an utterance is
reliably non-English, Fono transparently synthesizes that one
utterance with the local multilingual Piper voice for its language
instead of the cloud backend. English (or text whose language can't be
determined) still goes to the cloud voice unchanged, so the common
path is untouched and pays no detection cost. The utterance's language
is taken from the signal Fono already has where one exists (e.g. the
assistant's transcribed language) and otherwise detected with the
bundled `whatlang` trigram classifier — no model files, well under a
millisecond, and run only for English-only backends.

This requires a local TTS voice to fall back to: build Fono with the
`tts-local` feature, and have (or let Fono download) a catalogue voice
for the target language. When no local engine is available, the
non-English utterance is skipped with a single warning rather than
spoken as gibberish. There is no configuration for any of this — it is
automatic and only engages for English-only backends.

## Assistant capabilities

The voice assistant (F8 by default) can opt into two server-side
extras when the chosen primary provider supports them. See
[ADR 0024](decisions/0024-assistant-multimodal-and-search.md) for the
full design.

| Provider   | Vision (multimodal model)              | Web search (native tool)            |
|------------|-----------------------------------------|--------------------------------------|
| OpenAI     | `gpt-5.4-mini` (same as text default)   | `web_search_preview`                 |
| Anthropic  | `claude-haiku-4-5-20251001`             | `web_search_20250305`                |
| Gemini     | `gemini-flash-lite-latest`              | `google_search` *(not yet wired)*    |
| Groq       | —                                       | —                                    |
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

Both flags default to **`true`** in `[assistant]` and are no-ops for
providers whose catalogue entry doesn't carry the matching capability
(e.g. Cerebras gets neither). The wizard auto-enables them as part of
the assistant fast path and reports the resulting set on a single
`Extras:` info line; no MultiSelect prompt is shown. To disable, edit
`~/.config/fono/config.toml` (a future tray submenu will offer the
same toggles):

```toml
[assistant]
prefer_vision = false
prefer_web_search = false
```

## Hosting a Wyoming STT server

`sudo fono install --server` sets up a hardened systemd unit that
exposes Whisper over the Wyoming protocol on TCP/10300 so other Fono
clients, Home Assistant, and Rhasspy can route transcription through
this host (auto-discovery via mDNS). The installer seeds a minimal
`/etc/fono/config.toml` and verifies the listener bound. See
[install.md → Server mode](install.md#server-mode-wyoming-stt-host) for
the install, security, and key-management story.

## Screen capture

Fono can capture screenshots to give agents and the voice assistant visual
context. Two modes are available: **automatic** (grabs the focused window
instantly) and **interactive** (opens the OS-native region picker).

A **privacy gate** blocks capture when the focused window belongs to a
known sensitive application (KeePassXC, Bitwarden, 1Password, GNOME
Keyring, Seahorse). Fono returns an error instead of leaking the screen
contents.

All screen-capture tools are **entirely optional runtime dependencies** —
Fono probes PATH at startup and builds a tool ladder from whichever subset
is installed, falling back gracefully when tools are absent. No tool is
required; the feature degrades to "unavailable" only when none are present.

| Tool | Distro package | Ladder rungs covered | Example install |
|------|---------------|----------------------|-----------------|
| `scrot` | `scrot` (Debian/Ubuntu/Fedora/Arch) | X11 auto (rung 1), X11 interactive (rung 1) | `sudo apt install scrot` |
| `maim` | `maim` (Debian/Ubuntu/Fedora/Arch) | X11 auto (rung 2), X11 interactive (rung 2) | `sudo apt install maim` |
| `xdotool` | `xdotool` (Debian/Ubuntu/Fedora/Arch) | Required helper for `maim` focused-window mode | `sudo apt install xdotool` |
| `grim` | `grim` (Debian/Ubuntu/Fedora/Arch) | Wayland interactive (rung 1, paired with `slurp`) | `sudo apt install grim` |
| `slurp` | `slurp` (Debian/Ubuntu/Fedora/Arch) | Wayland interactive region picker (paired with `grim`) | `sudo apt install slurp` |
| `spectacle` | `kde-spectacle` / `spectacle` (Debian/Ubuntu/Fedora/Arch) | Wayland auto (rung 3), Wayland interactive (rung 3) | `sudo apt install kde-spectacle` |
| `gnome-screenshot` | `gnome-screenshot` (Debian/Ubuntu/Fedora/Arch) | All ladders (last resort, rung 4) | `sudo apt install gnome-screenshot` |
| `import` (ImageMagick) | `imagemagick` / `ImageMagick` (Debian/Ubuntu/Arch/Fedora) | Wayland auto (rung 2, Xwayland only when `DISPLAY` set); X11 auto (rung 3), X11 interactive (rung 3) | `sudo apt install imagemagick` |

Recommended minimal install per desktop:

```bash
# Wayland / wlroots (sway, Hyprland)
sudo apt install grim slurp

# X11
sudo apt install scrot

# KDE (Wayland or X11)
sudo apt install kde-spectacle

# GNOME
sudo apt install gnome-screenshot
```

No configuration is needed; `fono doctor` shows which tools are
available and which rung is active.

## Adding a new backend

Implement the `fono_stt::SpeechToText` or `fono_polish::TextCleanup` async
trait, register the factory in `crates/fono-{stt,polish}/src/registry.rs`,
then expose the new variant in `fono_core::config::{SttBackend,PolishBackend}`.
See `CONTRIBUTING.md` for full coding guidelines.
