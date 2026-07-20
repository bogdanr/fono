# Fono privacy

Fono is designed so that audio and transcripts leave your machine
when you have explicitly chosen a cloud provider.

## What never leaves your machine

* Raw audio buffers (they live in RAM, get handed to the STT backend,
  then dropped).
* The SQLite history database (`~/.local/share/fono/history.sqlite`).
* API keys (`~/.config/fono/secrets.toml`, mode 0600, refuses to load if
  world-readable; `$ENV_VAR` references never touch disk).
* Audio device names or application focus metadata.
* Voice embeddings (speaker verification). When `speaker.enabled` is on,
  Fono computes a numeric voiceprint locally to recognise who is
  speaking. That embedding **never** leaves the machine and is **never**
  attached to a cloud STT or LLM request — only the raw audio (to STT)
  and the transcript text (to a polish LLM, if configured) are sent,
  exactly as when verification is off. At most a matched speaker's
  **name** is stored in the local history database; the embedding itself
  stays in the local speakers database. This is covered by a regression
  test (`pipeline_speaker_verification_never_leaks_audio_or_embedding_to_stt`)
  asserting the STT payload is byte-for-byte unchanged with verification
  enabled.
* There isn't any telemetry at this point. Fono makes zero analytics calls.

## What leaves your machine (and when)

Every pipeline stage defaults to a local backend that sends nothing.
Each row below fires when you have explicitly pointed that stage at a cloud
or LAN backend.

| Scenario                               | Data sent                          | To                           |
|----------------------------------------|------------------------------------|------------------------------|
| `stt.backend` = local                  | nothing                            | —                            |
| `stt.backend` = Groq / OpenAI / etc.   | recorded audio (WAV)               | configured STT endpoint      |
| `polish.backend` = local                  | nothing                            | —                            |
| `polish.backend` = Cerebras / OpenAI / … | raw transcript text + prompt      | configured LLM endpoint      |
| `assistant.backend` = local            | nothing                            | —                            |
| `assistant.backend` = OpenAI / Gemini / … | your transcribed question + recent conversation turns | configured chat LLM endpoint |
| `tts.backend` = local / none           | nothing                            | —                            |
| `tts.backend` = a cloud provider       | the reply or dictation text being spoken | configured TTS endpoint |
| Realtime assistant (Gemini Live model selected) | live microphone audio for the duration of the session, plus one screenshot of the focused window when vision is enabled | Google (`generativelanguage.googleapis.com`) |
| Screen vision (assistant `fono_screen` tool) | a screenshot of the focused window or a region you pick — only when the tool is invoked, never in the background; capture is blocked for password-manager windows | configured cloud vision model |
| Wake word, default local detection     | nothing — idle audio is analysed on-device | — |
| Wake word, opt-in `[wakeword.wyoming]` client mode | idle microphone audio, continuously | the `wyoming-openwakeword` host on your LAN |
| `stt.backend` / `tts.backend` = wyoming | recorded audio out / synthesized audio back | the Wyoming peer on your LAN |
| Model download (`fono models install`) | HTTP GET (no auth, no identifiers) | `https://huggingface.co` or `FONO_MODEL_MIRROR` |

The wake-word Wyoming **client** mode is the one flow that streams idle
audio off the machine. It is never a default, requires both
`enabled = true` and an explicit `uri`, and `fono doctor` prints a
prominent warning while it is active. The reverse direction — Fono
*serving* wake detection over `[server.wyoming]` — keeps audio on the
box; the server is the detector.

Fono can also *serve* inbound APIs (the OpenAI/Ollama chat API, the
STT/TTS routes, and the web settings page). Those listeners are off by
default and bind to loopback when enabled. Access from other machines is
gated by inbound API keys stored as SHA-256 hashes in `api_keys.sqlite`
(mode 0600); the settings page treats secrets as write-only (stored
values are never sent to the browser), and request logs record metadata
only — prompt and reply content are never logged.

Cloud providers' retention and training policies are **their** policies,
not Fono's. `docs/providers.md` lists each endpoint's documented TOS
link; please read before pasting a key.

## Deleting history

```sh
fono history clear          # truncates the SQLite table
rm ~/.local/share/fono/history.sqlite   # wipe the file entirely
```

## Removing Fono

The SlackBuild / PKGBUILD / dpkg `prerm` scripts **never** delete your
`~/.config/fono`, `~/.cache/fono`, `~/.local/share/fono`, or
`~/.local/state/fono` directories. Those are user data. Remove them by
hand if you want a clean slate.

## Reporting a vulnerability

See [SECURITY.md](../SECURITY.md).
