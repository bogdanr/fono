# Fono privacy

Fono is designed so that audio and transcripts leave your machine
**only** when you have explicitly chosen a cloud provider.

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
* Crash dumps or telemetry — there are none. Fono makes zero analytics
  calls. `rg -i 'telemetr|analytic|sentry|posthog|mixpanel'` over the
  source returns nothing.

## What leaves your machine (and when)

| Scenario                               | Data sent                          | To                           |
|----------------------------------------|------------------------------------|------------------------------|
| `stt.backend` = local                  | nothing                            | —                            |
| `stt.backend` = Groq / OpenAI / etc.   | recorded audio (WAV)               | configured STT endpoint      |
| `polish.backend` = local                  | nothing                            | —                            |
| `polish.backend` = Cerebras / OpenAI / … | raw transcript text + prompt      | configured LLM endpoint      |
| Model download (`fono models install`) | HTTP GET (no auth, no identifiers) | `https://huggingface.co` or `FONO_MODEL_MIRROR` |

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
