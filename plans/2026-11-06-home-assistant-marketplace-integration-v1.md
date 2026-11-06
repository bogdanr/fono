# Fono for Home Assistant: one-click app + Marketplace integration

## Context

Goal: Fono replaces Piper (TTS), Whisper (STT), openWakeWord (wake word) and,
optionally, the conversation agent in Home Assistant. Users pick any subset of
these. Setup should take as few steps as possible, and the UI should be as good
as Home Assistant's own voice apps.

**Constraints the research turned up:**
- The **Marketplace** (HACS built into Home Assistant 2026.11) carries integrations, cards and
  themes. It **cannot carry apps/add-ons**. Piper and Whisper are not on the Marketplace
  either; they are **apps** that wrap a container image. The same is true for us: the
  engine must ship as an **app** built from our existing GHCR image
  (`ghcr.io/bogdanr/fono`). The Marketplace can only carry a **companion
  integration**.
- **Home Assistant's built-in `wyoming` integration** already creates STT, TTS, wake-word
  and conversation entities from whatever services the `info` reply lists. The mapping is in
  `hass-tmp/core/homeassistant/components/wyoming/data.py:27-43`. It also already supports
  streaming TTS (`tts.py:144`) and Wyoming `handle` with home control
  (`conversation.py:90`). If Fono speaks Wyoming completely, the core features need no
  Python of our own.
- If the `info` reply lists a **satellite**, Home Assistant suppresses all the other services
  on that endpoint (`data.py:29`). The desktop satellite therefore needs its own
  endpoint.
- **What already exists in Fono:**
  - a Wyoming server for asr, tts and wake (`crates/fono-net/src/wyoming/server.rs`) with
    token auth and mDNS advertising;
  - an Ollama/OpenAI-compatible LLM server (`crates/fono-net/src/llm_server/`);
  - the web settings UI and API (`crates/fono-net/src/web_settings/`, assets included);
  - speaker verification (`crates/fono-audio/src/speaker.rs`);
  - the assistant, which controls Home Assistant through HA's MCP server
    (`crates/fono-assistant/src/mcp_client.rs`).
- **Gaps in Fono:**
  - no Wyoming `handle`, no streaming, no satellite, no per-service toggles;
  - the container leaves the LLM server and web UI off;
  - the default wake word `hey_jarvis` is CC-BY-NC-SA, so it cannot be the default;
  - `fono-hassio` was never tested and is a bare option form. We **rewrite it**.

**Decisions taken:**
- Engine ships as an app.
- A companion integration goes on the Marketplace.
- Conversation works both ways: Fono's own agent over Wyoming `handle`, and Fono as the
  LLM behind Home Assistant's own agent. The user picks.
- First release includes speaker recognition, streaming STT and TTS, the desktop as a
  satellite, and a GPU variant.

## Target user experience

1. The user clicks one **"Add to my Home Assistant"** button (a my.home-assistant link)
   that adds the repository and opens the Fono app page. They click Install, then Start.
   The option form is short: capabilities, language, and a "GPU" toggle. Everything else
   lives in the web UI.
2. The app announces itself to Home Assistant, which shows a **"Discovered: Fono"** card.
   One click on Submit gives the user STT, TTS, wake word and conversation entities,
   limited to the capabilities they enabled.
3. **Fono panel** in the sidebar (ingress, no port or token): the existing web settings UI,
   adapted for Home Assistant:
   - **First-run wizard:** capability checkboxes (STT / TTS / wake word / assistant),
     language, voice picker with an audio **Preview** button, and a model-download
     progress bar.
   - Then tabs: Voices, Speech recognition, Wake words, Assistant (local or cloud LLM,
     Home Assistant control on or off), Speakers (enrol your voice with the browser
     microphone), Logs.
4. Optional **Fono integration** from the Marketplace adds:
   - **"Create voice assistant"**: builds and optionally pre-selects an Assist pipeline
     that uses the Fono pieces, which saves the five-dropdown pipeline form;
   - speaker entities and events;
   - selects for voice, STT model and agent;
   - diagnostics, repairs and an update entity.
5. On any desktop running Fono, the user ticks "Use this PC as a Home Assistant voice
   satellite". Home Assistant discovers it over zeroconf and it shows up as an Assist
   satellite device.

Users on Home Assistant Container or Core get the same experience minus the app. They run the
existing `ghcr.io/bogdanr/fono` image (compose file) or a desktop Fono, zeroconf
finds it, and the integration connects by host:port.

## Phase 1: Fono engine (this repo, Rust)

All work lands in existing crates with no new dependencies, so the size budget is
unaffected. Every new event type goes into the existing codec in
`crates/fono-net-codec/src/wyoming.rs`.

1. **Per-service toggles.**
   - Add `services = ["asr","tts","wake","handle"]` to `ServerWyoming`
     (`crates/fono-core/src/config.rs:1843`).
   - `spawn_wyoming_server_if_enabled` (`crates/fono/src/daemon.rs` ~3725) wires only the
     enabled ones, so `info` lists exactly what the user chose.
   - Changing services sends a reload hint; the integration and repairs tell the user to
     reload the entry.
2. **Wyoming `handle` (Fono as the conversation agent).**
   - Map `transcript{text, context}` to the existing `Assistant` trait
     (`crates/fono-assistant/src/traits.rs:485`), then reply with
     `handled{text, continue_conversation}`.
   - Home Assistant's `conversation_id` maps to the conversations stored on disk
     (`conversations.rs`).
   - Advertise `supports_home_control: true` when the MCP client to Home Assistant is
     configured. In the app, configure it automatically with the Supervisor token and
     `http://supervisor/core/mcp_server` (verify the path in testing).
   - Stream the reply with `handled-start`/`-chunk`/`-stop` and
     `supports_handled_streaming`.
3. **LLM endpoint.** No code change: the Ollama-compatible server already exists. The
   container entrypoint gains a switch to enable `[server.llm]` (see item 7).
4. **Streaming.**
   - TTS: `synthesize-start`/`-chunk`/`-stop` → audio → `synthesize-stopped`. Synthesize
     sentence by sentence through the existing local router. Still answer plain
     `synthesize`. Set `supports_synthesize_streaming`.
   - STT: `transcript-start`/`-chunk`/`-stop` using the Wyoming 1.11 rule (each chunk
     carries the whole text so far). Advertise `supports_transcript_streaming` only when
     the active backend actually streams: the cloud streaming variants yes, local whisper
     no.
5. **Wake word.**
   - Honour `detect{names}` per connection and list every installed model in `info.wake`.
   - **Ship a permissively licensed default** (`hey_fono` trained and its SHA pinned in
     `wake_registry.rs`). `hey_jarvis` and the other non-commercial models stay opt-in.
6. **Speaker recognition.**
   - Run `speaker.rs` on the STT audio and put `speaker` into the `transcript` context.
     Also fill the existing `detection.speaker` field (codec wyoming.rs:255).
   - Pass the identity to the `handle` path, matched on `conversation_id` or recency, so
     the assistant knows who is speaking.
   - Publish `speaker_identified` events on a new Server-Sent Events endpoint `/api/events`
     in `web_settings`, which the integration consumes.
7. **Container and app runtime** (`packaging/container/entrypoint.sh`, `Dockerfile`):
   - add `FONO_SERVICES`, `FONO_LLM_ENABLED` and `FONO_WEB_ENABLED`;
   - `EXPOSE 11434 10808`.
8. **Ingress-ready web UI** (`crates/fono-net/src/web_settings/`):
   - Honour the `X-Ingress-Path` header so assets work under a prefix.
   - Trust only the Supervisor ingress peer (`172.30.32.2`) when `FONO_HA_INGRESS=1`.
   - Add the first-run wizard, voice preview through `POST /v1/audio/speech`, model
     download progress, and microphone enrolment for speakers.
   - Follow Home Assistant's light and dark theme via `prefers-color-scheme`.
9. **Desktop satellite** (new module `crates/fono-net/src/wyoming/satellite.rs`):
   - A **separate** port (10700) and mDNS service that lists only `satellite`.
   - Implements `run-satellite`, wake detection on the PC (existing detector),
     `run-pipeline`, mic streaming, and playback of `snd` audio.
   - The tray or settings toggle is `[satellite] enabled`.
10. **ADR** `docs/decisions/00xx-home-assistant-integration.md` recording the app plus
    companion split, why core Wyoming is used, and the separate satellite port.

## Phase 2: App rewrite (`../fono-hassio`)

- **`config.yaml`:**
  - `arch: [amd64, aarch64]`, `image: ghcr.io/bogdanr/fono-hassio` (prebuilt,
    multi-arch, no local build);
  - `discovery: [wyoming]`, `ingress: true`, `panel_icon: mdi:account-voice`;
  - `homeassistant_api: true` for MCP control;
  - `video: true` for `/dev/dri` with automatic CPU fallback. Verify in testing that this
    doesn't hard-fail on hosts without a GPU. If it does, ship a second app,
    "Fono (GPU)".
  - Backups exclude the model cache.
- **Options:** `capabilities` (multi-select list), `language`, `gpu`. That is all. Cloud keys
  and voices move to the web UI, stored in `/data`. The non-commercial wake models stay
  off by default.
- **`run.sh`** (busybox):
  - translate the options to `FONO_*` and start Fono;
  - in the background, wait for `describe` to answer, then
    `wget --post-data` to `http://supervisor/discovery` with
    `{"service":"wyoming","config":{"uri":"tcp://<hostname>:10300"}}` and the
    `SUPERVISOR_TOKEN` (the same contract the Piper app uses).
- **CI:** Home Assistant builder actions publish the wrapper image for both architectures. The
  existing `repository_dispatch` from `container.yml` bumps the version. Stage edge, then
  beta, then stable.
- **Presentation:**
  - rewrite `DOCS.md` around the three-click flow;
  - add `translations/en.yaml`, icon and logo;
  - add a my.home-assistant "add repository" badge to the Fono README and the site.

## Phase 3: Companion integration (new repo `bogdanr/fono-homeassistant`, Marketplace)

- Python, `custom_components/fono/`, plus `hacs.json`, released through GitHub Releases.
- **Dependencies:** none beyond Home Assistant core; it reuses the `wyoming` library already
  pinned by core.
- **It does not duplicate the core Wyoming entities.** It adds features on top of them:
  - **Config flow:** zeroconf `_fono._tcp` and hassio discovery, or manual host:port.
    Authenticates to the web API with an API key the flow creates (`/api/apikeys`).
    Inside the app it uses ingress trust.
  - **Pipeline wizard step:** creates an Assist pipeline (Fono STT / TTS / wake word,
    agent = Fono or Home Assistant + Fono LLM) through the `assist_pipeline` storage API,
    and offers to make it preferred. It also creates the core Wyoming entry if the user
    skipped the discovery card.
  - **Entities:**
    - `sensor.fono_last_speaker`, plus the event `fono_speaker_identified`;
    - selects for TTS voice, STT model, assistant backend;
    - switches per capability;
    - an `update` entity;
    - binary sensor for model download status.
  - Diagnostics (redacted config), and a repairs issue when a non-commercial wake model is
    active or a model download failed.
  - **Quality target:** Gold-equivalent rules (config flow, discovery, translations,
    diagnostics, repairs, tests ≥ 95%).
- **Submission:** a PR to `hacs/default` for Marketplace listing, and a PR to the
  `home-assistant/brands` repo so Home Assistant shows the Fono logo.

## Phase 4: Docs and release (this repo)

- Rewrite `docs/home-assistant.md`:
  - app path, Container/Core path, satellite path;
  - fix the stale "default is Piper" note.
- `CHANGELOG.md`, `ROADMAP.md` (move items to Shipped) and `docs/status.md`, following the
  AGENTS.md release rules.

## Verification

- **Rust gates:** `nice -n 10 cargo fmt --all -- --check`, clippy `-D warnings`,
  `cargo test --workspace --tests --lib`, `./tests/check.sh --size-budget`.
- **Wyoming conformance:**
  - Integration tests in `crates/fono-net/tests/` drive the server with recorded event
    sequences for each service, including streaming and `handle`.
  - Cross-check with the Python `wyoming==1.11` client (`tmp/venv`) and core's own test
    suite for the `wyoming` component in `../hass-tmp/core`, pointed at a live Fono.
- **End to end on a real Home Assistant OS VM** (qemu, amd64) and a Raspberry Pi 5:
  - add the repository, install the app, see the discovery card, run the wizard;
  - an Assist voice command turns on a light;
  - a streaming TTS reply starts before the LLM finishes;
  - the speaker sensor updates;
  - toggling a capability removes its entity after reload;
  - start the app with and without `/dev/dri`.
- **Desktop satellite:** discovered over zeroconf, wake word on the PC, the reply plays on
  the PC.
- **Integration:** `pytest` with `pytest-homeassistant-custom-component`; the HACS Action
  and hassfest pass in CI.

## Open risks

- Whether `video: true` works on hosts without a GPU (fallback: a second app).
- The MCP endpoint path through the Supervisor proxy.
- Training `hey_fono` well enough that the default can stop being `hey_jarvis`.
- The app image (~375 MB with Mesa) on Raspberry Pi SD cards. A CPU-only slim tag is
  worth considering.
