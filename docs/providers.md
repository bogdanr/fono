# Fono provider matrix

Fono ships with one **speech-to-text (STT)** engine and one **LLM cleanup**
engine enabled at a time. Both are selected in `~/.config/fono/config.toml`
and can be swapped at any time with `fono setup` or by editing the file
directly. API keys are stored either in `~/.config/fono/secrets.toml`
(mode 0600, never logged) or referenced by `$ENV_VAR` name.

## Speech-to-text

| Backend       | Type       | Model(s)                               | API key env var       | Streaming |
|---------------|------------|----------------------------------------|-----------------------|-----------|
| Whisper local | local      | ggml tiny · tiny.en · base · base.en · small · small.en · medium · large-v3 | — | no |
| Groq          | cloud HTTP | `whisper-large-v3`, `whisper-large-v3-turbo` | `GROQ_API_KEY`        | no (batch) |
| OpenAI        | cloud HTTP | `whisper-1`, `gpt-4o-transcribe`       | `OPENAI_API_KEY`      | no |
| Deepgram      | cloud WS   | `nova-2`, `nova-3`                     | `DEEPGRAM_API_KEY`    | yes |
| Cartesia      | cloud HTTP | `sonic-transcribe`                     | `CARTESIA_API_KEY`    | yes |
| AssemblyAI    | cloud HTTP | `best`, `nano`                         | `ASSEMBLYAI_API_KEY`  | yes |

Whisper model files land in `~/.cache/fono/models/whisper/ggml-<name>.bin`.
Override the download host with `FONO_MODEL_MIRROR=https://your.mirror`.

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
