# Fono provider matrix

Fono ships with one **speech-to-text (STT)** engine and one **LLM cleanup**
engine active at a time. Both are selected in `~/.config/fono/config.toml`
and can be swapped at any time with `fono use`, `fono setup`, or by editing
the file directly. API keys are stored in `~/.config/fono/secrets.toml`
(mode 0600, never logged) or read from `$ENV_VAR`.

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

## Adding a new backend

Implement the `fono_stt::SpeechToText` or `fono_llm::TextCleanup` async
trait, register the factory in `crates/fono-{stt,llm}/src/registry.rs`,
then expose the new variant in `fono_core::config::{SttBackend,LlmBackend}`.
See `CONTRIBUTING.md` for full coding guidelines.
