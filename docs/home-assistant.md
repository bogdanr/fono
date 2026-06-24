# Home Assistant (Docker container)

Fono ships a Vulkan-capable server container that exposes the
[Wyoming protocol](https://www.home-assistant.io/integrations/wyoming/)
on TCP `10300`. Point Home Assistant at it and Fono becomes the
speech-to-text **and** text-to-speech engine for your Assist pipelines —
local-first, no cloud required.

The image is multi-arch (`amd64` + `arm64`), so the same tag runs on an
Intel/AMD mini-PC and on arm64 boards like the Raspberry Pi 5 or the
NVIDIA Jetson Orin Nano.

> Looking for the native, non-Docker server install? See
> [install.md → Server mode](install.md#server-mode-wyoming-stt-host).

## Two ways to run it

- **One-click add-on** (Home Assistant OS / Supervised): install Fono from the
  Supervisor Add-on Store via the
  [fono-hassio](https://github.com/bogdanr/fono-hassio) repository — no Docker
  commands, and the options are a form. Add the repository URL
  `https://github.com/bogdanr/fono-hassio`, install **Fono**, start it, then
  connect the Wyoming integration as described below.
- **Container anywhere** (any HA install type, or a separate host): run the
  image yourself as described next. This also works on Home Assistant Core and
  generic Docker setups, where add-ons are not available.

## Run it

### docker compose (recommended)

Grab [`packaging/container/compose.yaml`](../packaging/container/compose.yaml),
then:

```sh
docker compose up -d
```

### docker run

```sh
docker run -d --name fono \
  --restart unless-stopped \
  -p 10300:10300 \
  -v fono-data:/data \
  ghcr.io/bogdanr/fono:latest
```

First start downloads the chosen Whisper model (and, for spoken
responses, a Piper voice) into the `/data` volume, then the Wyoming
endpoint answers on `10300`. Models persist across restarts because
`/data` is a named volume.

## What you get out of the box

| Service | Default | Notes |
| --- | --- | --- |
| Speech-to-text | local Whisper `small` | always served |
| Text-to-speech | local Piper | served automatically whenever a TTS backend is set; `local` is the default |
| Languages | `en` | comma-separated, e.g. `en,ro` |
| Listener | `0.0.0.0:10300` | the Wyoming/Home Assistant port |

No API keys, no internet calls — everything runs on the box.

## Connect Home Assistant

1. In Home Assistant: **Settings → Devices & Services → Add Integration**.
2. Choose **Wyoming Protocol**.
3. Enter the container host's IP (or hostname) and port `10300`.
4. Open **Settings → Voice assistants**, edit (or create) a pipeline, and
   select **Fono** for **Speech-to-text** and/or **Text-to-speech**.

That's the whole integration — Wyoming support is built into Home
Assistant core, so there is nothing to install on the HA side.

> **mDNS note.** With Docker's default bridge network the container's
> mDNS advertisement does not cross onto your LAN, so add it by IP/port
> as above (this always works). If you want auto-discovery, run the
> container with `network_mode: host`.

## Wake word ("hey fono")

Fono has an optional always-on wake word that triggers dictation or the
assistant when you say a fixed phrase — no hotkey, no button. It is **off
by default**; enable it in `[wakeword]` (see
[configuration.md → `[wakeword]`](configuration.md#wakeword--always-on-wake-word)).
The built-in clean-licence phrase is **"hey fono"**; the matcher is
English-first and tied to whichever phrase model is loaded (it is not
free-form speech).

For Home Assistant, Fono's wake word works **exactly like its STT and
TTS**: whenever the `[server.wyoming]` listener is enabled, Fono
automatically advertises and serves its *own* local detector as a Wyoming
wake `Detection` service over that listener (TCP `10300`). There is **no
extra switch to flip** — turn the Wyoming server on and wake is offered
alongside STT and TTS. Home Assistant can then use Fono as a drop-in
wake-word provider for an Assist pipeline, and **the microphone audio
never leaves the Fono box** — the server *is* the detector. Until the
clean-licence "hey fono" model is published, the auto-served default
phrase is **"hey jarvis"** (a community openWakeWord model).

To connect it in Home Assistant, add the **Wyoming Protocol** integration
pointing at the Fono host's `10300` (the same endpoint that serves
STT/TTS) and select Fono as the wake-word provider in your Assist
pipeline.

> **Opt-in client direction (NOT default).** With `[wakeword].wyoming`
> enabled **and** a `uri` pointing at an external `wyoming-openwakeword`
> service, Fono instead delegates *its own* activation to that box.
>
> > ⚠️ **Privacy warning.** The client direction **streams idle
> > microphone audio over the LAN** to the external service and therefore
> > **breaks the "audio never leaves the machine while idle"
> > guarantee**. It is never a default, must be explicitly opted into, and
> > `fono doctor` prints a prominent warning whenever it is active. Prefer
> > the automatic server direction unless you have a specific reason to
> > centralise wake detection elsewhere.

## GPU acceleration

The image bundles the Vulkan loader and Mesa drivers but runs on the CPU
by default, which works on every host. To transcribe on an Intel or AMD
GPU, give the container the host render node — in compose:

```yaml
    devices:
      - /dev/dri:/dev/dri
```

Leave this off on headless servers, VMs, NVIDIA-only hosts, and Docker
Desktop, where `/dev/dri` is absent and would stop the container from
starting (Fono falls back to CPU on its own). For **NVIDIA** (including
Jetson), install the NVIDIA Container Toolkit and use its runtime instead
of the `/dev/dri` mapping.

## Tuning (environment variables)

The entrypoint generates `/data/.config/fono/config.toml` on first start.
Set `FONO_CONTAINER_WRITE_CONFIG=always` to regenerate it from the
environment on **every** start, so editing your compose file and running
`docker compose up -d` re-applies changes.

| Variable | Default | Purpose |
| --- | --- | --- |
| `FONO_LANGUAGES` | `en` | recognised languages, comma-separated BCP-47 codes |
| `FONO_STT_MODEL` | `small` | `tiny`, `small`, `large-v3-turbo`, … (local Whisper) |
| `FONO_STT_BACKEND` | `local` | `local` or a cloud provider (see below) |
| `FONO_TTS_BACKEND` | `local` | `local` Piper, a cloud provider, or `none` to disable TTS |
| `FONO_TTS_LOCAL_VOICE` | (auto) | a Piper voice, e.g. `en_US-amy-medium` |
| `FONO_CONTAINER_WRITE_CONFIG` | `missing` | `always` regenerates config from env each start |

### Cloud backends (optional)

Cloud STT/TTS replaces the local engine. Set the backend and its API key;
the model/voice is chosen automatically. For example, Groq STT:

```yaml
    environment:
      FONO_STT_BACKEND: groq
      GROQ_API_KEY: "sk-..."
```

Supported STT/TTS backends include `groq`, `openai`, `deepgram`,
`gemini`, `elevenlabs`, `cartesia`, `speechmatics`, and `openrouter`.
Each reads its key from the matching `*_API_KEY` environment variable.
See [providers.md](providers.md) for the full matrix.

## Security

Wyoming v1 has no in-band authentication that Home Assistant sends, so
binding to `0.0.0.0` exposes inference to every host that can reach
TCP/10300. Keep the container on a trusted LAN and, if needed:

- publish the port only on a specific interface (`-p 192.168.1.5:10300:10300`), or
- block port 10300 at your firewall.

Audio and transcripts stay on the box with the default local backends;
cloud backends are strictly opt-in via the variables above.
