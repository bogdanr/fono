# Publishing Fono as a Home Assistant Add-on (App)

## Objective

Make Fono installable from the Home Assistant Supervisor "Add-on Store"
(now branded "Apps") with a few clicks, so Home Assistant OS / Supervised
users get push-button local speech-to-text **and** text-to-speech over the
Wyoming protocol without touching Docker, compose files, or environment
variables by hand. The add-on must **reuse the existing multi-arch GHCR
image** (`ghcr.io/bogdanr/fono`) rather than rebuilding Fono inside Home
Assistant, translate the entrypoint's `FONO_*` environment contract into
Supervisor add-on options, and track Fono releases automatically.

This is a separate, additive distribution channel layered on top of the
**already-working** path (run the container, add it via the built-in
Wyoming Protocol integration). It does **not** change any Fono source code
or the existing container/packaging. All new artifacts live in a **new,
dedicated add-on repository**, not in this repo.

## Progress — 2026-06-22 (scaffold landed)

The add-on repository **`github.com/bogdanr/fono-hassio`** was created and
scaffolded (commit `a9e4c94`, not yet pushed). Phases 0–2 are essentially
complete; selected Phase 3/4/6 items are done, with the rest deferred to
real-hardware testing. Decisions/deviations from the original draft:

- **Repo name** is `fono-hassio` (not `fono-hassio-addons`).
- **Distribution model = local build** (`fono/Dockerfile` + `fono/build.yaml`,
  `FROM ghcr.io/bogdanr/fono`) rather than a CI-published wrapper image. The
  wrapper only adds the `run.sh` shim, so the local build is trivial and avoids
  standing up a second image pipeline now. (Resolves Task 0.2 toward variant
  (a), local-build form.)
- **`build_from` pins `:latest`** for bootstrap because the `0.11.0` image tag
  predates the container workflow; `.github/workflows/sync-version.yml` pins it
  per Fono release (Task 4.2).
- **GPU is CPU-fallback only** in v1 — `/dev/dri` is intentionally **not**
  mapped, because a missing device would hard-fail start on GPU-less hosts
  (same trade-off as `compose.example.yaml`). A GPU variant is a follow-up
  (Task 3.1 remains open).

Scaffolded files: `repository.yaml`, `LICENSE`, `README.md`,
`fono/config.yaml`, `fono/build.yaml`, `fono/Dockerfile`, `fono/run.sh`,
`fono/translations/en.yaml`, `fono/DOCS.md`, `fono/CHANGELOG.md`,
`.github/workflows/sync-version.yml`.

**Done:** 0.1–0.4, 1.1–1.6, 2.1–2.5, 3.2, 3.4, 3.6, 4.1, 4.2, 4.5, 6.1, 6.3.
**Open / deferred:** 3.1 (GPU `/dev/dri` variant), 3.3 (custom AppArmor),
3.5 (Supervisor Wyoming discovery), 4.3 (Fono-repo `repository_dispatch`
trigger on release), 4.4 (N/A under local build), 5.1–5.8 (real HA OS / GPU
testing — cannot be done in this environment), 6.2 (`icon.png`/`logo.png`),
6.4–6.5 (community/brands listing). Push the repo and validate on a real HA OS
instance next.

## Background / Current State

Verified against the repository:

- **Image.** `packaging/container/Dockerfile:26-50` builds a `scratch`-based
  image bundling the Vulkan loader + Mesa drivers + busybox + the Fono binary
  (built `--features accel-vulkan`, default features = `tts-local` statically
  linking onnxruntime + `local-models = whisper`). `EXPOSE 10300/tcp`,
  `VOLUME ["/data"]`, `ENV HOME=/data`, entrypoint
  `/usr/local/bin/fono-entrypoint`, default `CMD ["fono"]`. Image is ~375 MB.
- **Multi-arch + tags.** `.github/workflows/container.yml:24-139` builds
  `linux/amd64` (ubuntu-24.04) and `linux/arm64` (ubuntu-24.04-arm) per-arch
  images, then `docker buildx imagetools create` combines them into a manifest
  publishing `:latest`, `:vulkan`, `:<version>`, `:<version>-vulkan` to
  `ghcr.io/${repo}` (lowercased → `ghcr.io/bogdanr/fono`). Only 64-bit arches
  are produced; there is no armv7/armhf/i386 build.
- **Config generation.** `packaging/container/entrypoint.sh:19-115` regenerates
  `$HOME/.config/fono/config.toml` from `FONO_*` env vars when
  `FONO_CONTAINER_WRITE_CONFIG=always` or the file is missing. Recognised vars
  include `FONO_LANGUAGES`, `FONO_STT_BACKEND`, `FONO_STT_MODEL`,
  `FONO_STT_QUANTIZATION`, `FONO_STT_THREADS`, `FONO_TTS_BACKEND`,
  `FONO_TTS_VOICE`, `FONO_TTS_LOCAL_VOICE`, `FONO_WYOMING_BIND`,
  `FONO_WYOMING_PORT`, `FONO_MDNS_NAME`, `FONO_UPDATE_AUTO_CHECK`,
  `FONO_POLISH_ENABLED`, `FONO_ASSISTANT_ENABLED`. Cloud `*_API_KEY` vars are
  read by the binary itself (e.g. `GROQ_API_KEY`). Persistent data lives under
  `/data` (config, models cache, state). The entrypoint already enables the
  Wyoming server (`[server.wyoming] enabled = true`) and defaults
  `FONO_TTS_BACKEND=local`, so STT **and** TTS are served out of the box.
- **No web UI.** The container is a headless Wyoming TCP server (port 10300);
  there is no HTTP server. **Home Assistant Ingress and `webui` do not apply.**
- **Working-today integration path.** A separate `docs/home-assistant.md` page
  documents running the container and adding it through HA's built-in **Wyoming
  Protocol** integration with zero HA-side packaging.

Authoritative HA developer docs reviewed (June 2026 revisions; HA has renamed
"add-ons" to "apps" but the on-disk schema/filenames are unchanged):

- Introduction — https://developers.home-assistant.io/docs/add-ons
- Configuration (`config.yaml` schema, options/schema, `map`, `image`,
  devices, apparmor, host_network) —
  https://developers.home-assistant.io/docs/add-ons/configuration
- Communication (internal network, discovery, naming) —
  https://developers.home-assistant.io/docs/add-ons/communication
- Local testing (devcontainer, local build/run) —
  https://developers.home-assistant.io/docs/add-ons/testing
- Publishing (pre-built vs locally built, `image:` naming, multi-arch
  manifest) — https://developers.home-assistant.io/docs/add-ons/publishing
- Repository (`repository.yaml`, install URL) —
  https://developers.home-assistant.io/docs/add-ons/repository
- Security (rating, AppArmor) —
  https://developers.home-assistant.io/docs/add-ons/security
- Example repo — https://github.com/home-assistant/addons-example

## The Two-Path Recommendation (and sequencing)

There are two distinct ways HA users can consume Fono. They are complementary,
not mutually exclusive.

**Path A — Wyoming Protocol integration (works today, docs only).**
The user runs the Fono container anywhere (a NAS, a mini-PC, the HA host via
Portainer), then in Home Assistant adds the built-in **Wyoming Protocol**
integration pointing at `host:10300`. Zero HA-side packaging, works on **every**
HA install type (Core/Container/Supervised/OS) because it only needs network
reachability. This is the existing `docs/home-assistant.md` story.

**Path B — Home Assistant Add-on (this plan).**
Ship a Supervisor add-on that wraps the prebuilt GHCR image so the user
installs Fono from the Apps store, configures it with a form, and starts it
with one click. The add-on still speaks Wyoming on 10300; the user still adds
the Wyoming Protocol integration (or relies on Supervisor discovery) to consume
it. **Add-ons only run on Home Assistant OS and Supervised installs** (they
require the Supervisor); Core and generic-Container users must use Path A.

**Recommended sequencing:** ship Path A docs first (already in flight), then
Path B as the convenience layer. Path B's value is purely UX — it removes the
manual `docker run`/compose step for the large HAOS user base; the underlying
transport, image, and config contract are identical. Build Path B only after
Path A is verified end-to-end, because Path B reuses the same image, port, and
`FONO_*` contract and inherits any fixes made while validating Path A.

## Assumptions

- The add-on **references the prebuilt multi-arch GHCR image via `image:`**;
  it does not ship a `Dockerfile`/`build.yaml` that recompiles Fono. This
  reuses the existing CI multi-arch build and keeps install fast and
  low-failure (HA's own "preferred method" per the Publishing doc).
- The add-on supports exactly the two arches the image provides: `aarch64`
  and `amd64`. HA's current schema only documents those two anyway.
- A **thin `run.sh` shim** reads `/data/options.json`, exports the
  corresponding `FONO_*` env vars, sets `FONO_CONTAINER_WRITE_CONFIG=always`,
  then `exec`s the existing `/usr/local/bin/fono-entrypoint fono`. **Open
  question flagged below:** the scratch image has only busybox `sh` and no
  bashio, so the shim cannot simply assume bashio — see Phase 2 and Risks for
  the two resolution options (a thin add-on-local wrapper image `FROM` the GHCR
  image that adds `run.sh`, vs. an env-flagged busybox `options.json` parser
  baked into the entrypoint).
- The add-on lives in a **new public repo**, proposed name
  `github.com/bogdanr/fono-hassio-addons` (HA convention: a repo can host
  multiple add-ons; the `-hassio-addons` suffix mirrors the community
  ecosystem).
- Versioning: the add-on `config.yaml` `version` equals the Fono release tag
  (without `v`) and pins the GHCR tag, so HA's "this needs to match the tag of
  the image" rule (Publishing doc) holds.
- Cloud API keys are optional `password`-type options; local STT+TTS is the
  default so the add-on is fully functional with an empty form.

## Implementation Plan

### Phase 0 — Decisions & repository bootstrap

- [ ] Task 0.1. Confirm the add-on **distribution model = reference the
  prebuilt GHCR image** (not local build). Rationale: reuses the existing
  multi-arch manifest, fast install, HA's preferred path
  (https://developers.home-assistant.io/docs/add-ons/publishing).
- [ ] Task 0.2. Decide the **shim delivery mechanism** and record the choice as
  a short ADR-style note in the new repo:
  (a) a minimal add-on-local `Dockerfile` that does
  `FROM ghcr.io/bogdanr/fono:<tag>` and only `COPY run.sh` + sets `CMD`,
  published as a second tiny image; or (b) bake an `options.json`→`FONO_*`
  busybox shim into the Fono image's entrypoint behind an env flag so the
  add-on can point `image:` straight at the GHCR image with no rebuild.
  Recommend **(a)** to keep the Fono binary image unchanged and isolate the
  HA-specific glue in the add-on repo (net-zero on the shipped desktop binary).
  Flag (a)'s cost: a second small image build in the add-on repo's CI.
- [ ] Task 0.3. Create `github.com/bogdanr/fono-hassio-addons` (GPL-3.0,
  DCO/sign-off, SPDX headers on any scripts, no agent co-author trailers — same
  rules as this repo). Add top-level `repository.yaml`
  (`name`, `url`, `maintainer`) per
  https://developers.home-assistant.io/docs/add-ons/repository.
- [ ] Task 0.4. Reserve the per-add-on folder `fono/` and a unique `slug: fono`
  (unique within the repo, URI-friendly).

### Phase 1 — Add-on `config.yaml` (metadata, image, ports, storage)

- [ ] Task 1.1. Author `fono/config.yaml` required keys: `name: "Fono"`,
  `version: "<fono-release>"`, `slug: fono`, `description`, and
  `arch: [aarch64, amd64]`. Rationale: these are the five required keys
  (Configuration doc); restrict arch to the two the image actually ships.
- [ ] Task 1.2. Set `image: "ghcr.io/bogdanr/fono"` (or the thin wrapper image
  from Task 0.2(a), e.g. `ghcr.io/bogdanr/fono-hassio`) so Supervisor pulls the
  multi-arch manifest and selects the per-arch image; `version` selects the tag.
  Rationale: Publishing doc "Image naming" — the generic manifest name is the
  preferred reference.
- [ ] Task 1.3. Declare the Wyoming port:
  `ports: { "10300/tcp": 10300 }` plus
  `ports_description: { "10300/tcp": "Wyoming protocol (STT + TTS)" }`.
  Rationale: lets the user reach Fono from the Wyoming integration and other
  LAN devices; description surfaces in the UI.
- [ ] Task 1.4. Map persistent storage. Use `map: [ "addon_config:rw" ]` for
  any user-facing config files if needed, and rely on the **always-present
  writable `/data`** for models/state (the HA add-on `/data` is exactly the
  volume the Fono entrypoint already uses for `HOME=/data`). Rationale: the
  entrypoint writes config + caches models under `/data`; HA persists `/data`
  across restarts and includes it in backups by default.
- [ ] Task 1.5. Set lifecycle metadata: `startup: services` (start before HA so
  the Wyoming endpoint is up when the integration loads), `boot: auto`,
  `stage: experimental` initially (graduate to `stable` after field testing),
  and `url:` pointing at the Fono repo/site. The image has no s6 overlay, so
  the default init is fine (do not set `init: false`). Rationale: matches the
  Configuration doc semantics for a long-running network service.
- [ ] Task 1.6. Do **not** set `ingress`, `webui`, or `ingress_port` — Fono has
  no HTTP UI. Set branding/`panel_*` only if a menu entry is desired.

### Phase 2 — Options schema ↔ `FONO_*` env translation

- [ ] Task 2.1. Define `options:` defaults and a `schema:` mapping each
  entrypoint variable to a typed option (option → env → schema type):
  - `languages` → `FONO_LANGUAGES` → `str` (comma list, e.g. `"en"`).
  - `stt_backend` → `FONO_STT_BACKEND` →
    `list(local|groq|openai|deepgram|gemini|elevenlabs|cartesia|speechmatics|openrouter)`.
  - `stt_model` → `FONO_STT_MODEL` → `str` (default `"small"`).
  - `tts_backend` → `FONO_TTS_BACKEND` → `list(local|...)` (default `local`).
  - `tts_local_voice` → `FONO_TTS_LOCAL_VOICE` → `"str?"` (optional).
  - `mdns_name` → `FONO_MDNS_NAME` → `str` (default `"Fono"`).
  - `wyoming_port` → `FONO_WYOMING_PORT` → `port` (default `10300`).
  - cloud keys: `groq_api_key`, `openai_api_key`, `deepgram_api_key`,
    `gemini_api_key`, `elevenlabs_api_key`, `cartesia_api_key`,
    `speechmatics_api_key`, `openrouter_api_key` → matching `*_API_KEY` env →
    `"password?"` each (optional, masked in UI).
  - `gpu` → toggles device passthrough behaviour (see Phase 3) → `bool`.
  Rationale: `str`, `list(...)`, `port`, `password`, `bool`, and the optional
  `?` suffix are exactly the validators documented in the Options/Schema
  section; `password` masks keys in the UI.
- [ ] Task 2.2. Keep `FONO_WYOMING_BIND=0.0.0.0` fixed in the shim (not a user
  option) so the service is reachable on the add-on network; do not expose it.
- [ ] Task 2.3. Write `fono/run.sh` (the shim): read each option (bashio
  `bashio::config 'languages'` … or a busybox parser per Task 0.2), export the
  corresponding `FONO_*`, only export a cloud `*_API_KEY` when its option is
  non-empty, set `FONO_CONTAINER_WRITE_CONFIG=always` and
  `FONO_WYOMING_BIND=0.0.0.0`, then `exec /usr/local/bin/fono-entrypoint fono`.
  Rationale: the entrypoint already renders `config.toml` from env — the shim
  only surfaces options as env; no config templating is duplicated.
- [ ] Task 2.4. If Task 0.2(a) was chosen, place a minimal `fono/Dockerfile`
  (`FROM ghcr.io/bogdanr/fono:<tag>`, `COPY run.sh /run.sh`,
  `CMD ["/run.sh"]`, plus required
  `io.hass.version`/`io.hass.type`/`io.hass.arch` LABELs) and supply bashio (or
  use a busybox `options.json` parser if bashio is absent). Rationale: the
  scratch image lacks bashio/a full shell; the wrapper supplies the shim
  cleanly.
- [ ] Task 2.5. Add `fono/translations/en.yaml` describing each option
  (`configuration:` names/descriptions) and the port (`network:` block) so the
  Supervisor UI shows human-readable labels.

### Phase 3 — Hardware / GPU access, AppArmor, networking

- [ ] Task 3.1. GPU passthrough for Intel/AMD: expose `devices: ["/dev/dri"]`
  (gated behind the `gpu` option, or always when present). Rationale: the image
  is Vulkan-capable via Mesa; `/dev/dri` is the render-node path. The
  Configuration doc's `gpu`/`video`/`udev` flags exist, but explicit
  `devices:` is the least-privilege route.
- [ ] Task 3.2. Document (do not silently enable) that **NVIDIA** acceleration
  on HA OS is not generally available — the NVIDIA Container Toolkit is not part
  of HA OS — so NVIDIA users are steered to Path A on a host they control.
- [ ] Task 3.3. AppArmor: start with the default profile (`apparmor: true`). If
  `/dev/dri` access trips the default profile, ship a custom `fono/apparmor.txt`
  granting the Vulkan/DRI file accesses and reference it by name. Rationale:
  Security doc — a tailored profile keeps the security rating high while
  permitting GPU.
- [ ] Task 3.4. Networking: prefer the **internal add-on network** with the
  published `ports:` mapping (Wyoming reachable on the host's LAN IP:10300).
  Only consider `host_network: true` if mDNS advertisement of the Wyoming
  service must reach other LAN devices directly; document the trade-off
  (host_network reduces isolation and the security rating). Rationale:
  Communication doc — the internal network is the default and is sufficient for
  the Wyoming integration, which connects by host/port.
- [ ] Task 3.5. Evaluate Supervisor **Wyoming discovery**: investigate adding
  `discovery: [wyoming]` (and any required service declaration) so HA
  auto-discovers the add-on as a Wyoming service and offers one-click setup,
  matching the official Piper/Whisper add-ons. Flag as needs-verification — the
  exact discovery key/handshake must be confirmed against the current Supervisor
  and the Wyoming integration before relying on it.
- [ ] Task 3.6. Seccomp: rely on the **default** profile. The container build's
  legacy-seccomp fallback (`packaging/container/build-image.sh:35-87`) is a
  *build-time* workaround only; at runtime the published image needs no special
  seccomp. Do not set `full_access`/`privileged`. Confirm the Vulkan runtime
  starts under default seccomp during testing (Phase 5).

### Phase 4 — CI / release wiring (version tracking)

- [ ] Task 4.1. Decide repo location: keep the add-on in the **separate
  `fono-hassio-addons` repo** (recommended) so its release cadence and HA-only
  CI stay decoupled from the Fono monorepo's size-gated workflows. Rationale:
  avoids loading HA-specific concerns into `container.yml`/`release.yml`.
- [ ] Task 4.2. Add a GitHub Actions workflow in the add-on repo that, on a
  `repository_dispatch`/`workflow_dispatch` (or a scheduled poll of the Fono
  GHCR tags / GitHub releases), bumps `fono/config.yaml` `version` to the new
  Fono tag and opens a PR. Rationale: keeps the add-on tag in lockstep with the
  image tag (HA requires `version` == image tag).
- [ ] Task 4.3. In the Fono repo's existing release flow, add a single outbound
  trigger (a `repository_dispatch` to the add-on repo) at tag time, alongside
  the CHANGELOG/ROADMAP steps already mandated by AGENTS.md. Rationale: one
  source of truth for "a release happened"; mirrors how `container.yml` already
  keys off `tags: v*`.
- [ ] Task 4.4. If Task 0.2(a) (wrapper image) was chosen, add a builder
  workflow in the add-on repo using the Home Assistant **builder** composite
  actions (per the Publishing doc) to build+push the thin wrapper image for
  `aarch64`+`amd64` and the multi-arch manifest. Rationale: the wrapper image
  must exist for both arches under the tag `config.yaml` references.
- [ ] Task 4.5. Add a `fono/CHANGELOG.md` updated per add-on release (HA shows
  it in the UI) and keep it in sync with the upstream Fono CHANGELOG section.

### Phase 5 — Local testing & verification

- [ ] Task 5.1. Stand up the HA **devcontainer** (clone
  `home-assistant/devcontainer`, copy `devcontainer.json` + `tasks.json`,
  "Start Home Assistant"), and confirm the add-on appears under **Local Apps**.
  Rationale: documented fastest dev loop (Testing doc).
- [ ] Task 5.2. Smoke test: install the add-on, verify it pulls the GHCR image,
  starts, and the log shows Fono binding Wyoming on `0.0.0.0:10300`.
- [ ] Task 5.3. Protocol test: from the HA host, confirm a Wyoming client/probe
  gets a valid `info`/handshake on 10300 (STT + TTS advertised).
- [ ] Task 5.4. Integration test: add the **Wyoming Protocol** integration (or
  exercise discovery if Phase 3.5 lands) pointing at the add-on; confirm HA
  Assist uses Fono for **STT** and for **TTS**.
- [ ] Task 5.5. Options round-trip: change `languages`, `stt_model`,
  `tts_backend`, a cloud key, and `gpu`; restart; confirm the regenerated
  `/data/.config/fono/config.toml` reflects them (entrypoint
  `FONO_CONTAINER_WRITE_CONFIG=always` path).
- [ ] Task 5.6. GPU test on a `/dev/dri`-equipped host (amd64 Intel/AMD): verify
  Vulkan acceleration engages and the default AppArmor/seccomp don't block it;
  capture the fallback-to-CPU behaviour when no `/dev/dri` is mapped.
- [ ] Task 5.7. Persistence test: confirm downloaded Whisper/Piper models
  survive an add-on restart (stored under `/data`) and that an add-on backup
  captures `/data`.
- [ ] Task 5.8. Arch test: install on both an `amd64` HA OS box and an
  `aarch64` board (Raspberry Pi 5 / Jetson Orin Nano class) to prove the
  manifest selects the right per-arch image.

### Phase 6 — Distribution & publishing

- [ ] Task 6.1. Document the **custom repository** install: users add
  `https://github.com/bogdanr/fono-hassio-addons` via Supervisor → Apps →
  store ⋮ → Repositories, then install Fono. Provide a `my.home-assistant.io`
  add-repo button in the README (Repository doc).
- [ ] Task 6.2. Write `fono/README.md` + `fono/DOCS.md` (the in-UI docs tab),
  `fono/icon.png` + `fono/logo.png` (Presentation doc), covering options, GPU
  setup, and how to connect the Wyoming integration.
- [ ] Task 6.3. Cross-link from this repo's `docs/home-assistant.md`: Path A
  (manual container) **and** Path B (one-click add-on) with guidance on which
  to choose by install type.
- [ ] Task 6.4. (Optional, later) Pursue listing in the **Home Assistant
  Community Add-ons** org — note the high bar (code review, AppArmor profile,
  maintenance commitment, security rating) and that the **custom repository is
  the recommended starting point**; community/official inclusion is a stretch
  goal after the add-on is proven in the wild.
- [ ] Task 6.5. (Optional) Add Fono to the Home Assistant **brands** repo so the
  Wyoming-discovered service shows the Fono logo in HA.

## Verification Criteria

### Phase 1–2 (config + options)
- `config.yaml` validates in the Supervisor (no schema errors); the add-on
  appears in the store with name, description, and the 10300/tcp port shown.
- Every documented `FONO_*` entrypoint variable has a corresponding option,
  and changing an option is observably reflected in the rendered
  `config.toml` after restart.
- Cloud API keys render as masked `password` fields and are omitted from env
  when left blank.

### Phase 3 (hardware/security)
- With `/dev/dri` mapped, the container starts under the default AppArmor +
  seccomp profiles (or a named custom AppArmor profile) and engages Vulkan.
- Without GPU mapping, Fono still starts and serves on CPU.
- The add-on runs on the internal network with 10300 reachable; the security
  rating is acceptable (no `full_access`/`privileged`).

### Phase 4 (CI/release)
- Tagging a new Fono release results (via dispatch) in an automatic
  `config.yaml` `version` bump PR in the add-on repo within one run.
- The image tag referenced by `config.yaml` exists for both `aarch64` and
  `amd64`.

### Phase 5 (testing)
- Devcontainer: add-on installs, starts, Wyoming answers on 10300.
- HA Assist performs STT and TTS through Fono end-to-end.
- Models persist across restart; backup includes `/data`.
- Both arches install from the manifest.

### Phase 6 (distribution)
- A clean HA OS instance can add the custom repo and one-click install Fono.
- README/DOCS/icon/logo render correctly in the Supervisor UI.

## Potential Risks and Open Questions

1. **Scratch image has no bashio / general shell for the shim.**
   The published Fono image is `FROM scratch` with only busybox. The HA shim
   pattern assumes bashio. Mitigation: Task 0.2 — either a thin wrapper image
   (`FROM ghcr.io/bogdanr/fono` + `run.sh`, recommended) or an env-flagged
   busybox `options.json` parser baked into the entrypoint. Open question:
   which is preferred long-term given the binary-size-first discipline (the
   wrapper does not affect the desktop binary; it only adds an HA-only image).

2. **GPU passthrough limits on HA OS.** `/dev/dri` works for Intel/AMD on HA OS,
   but NVIDIA needs the Container Toolkit which HA OS lacks. Mitigation: document
   NVIDIA users to Path A; default the add-on to CPU and treat GPU as opt-in
   (Task 3.1/3.2).

3. **Image size on constrained boards (~375 MB).** Pull time and SD-card wear on
   small Pis. Open question (see Risk 6): whether to also publish a **non-Vulkan
   smaller image** variant for tiny boards (CPU-only, no Mesa/Vulkan loader),
   selectable as a second add-on or an option. Mitigation: flag as a follow-up;
   aarch64 Vulkan still benefits Pi 5 / Jetson.

4. **First-start model download time/space.** Local Whisper (`small`) + Piper
   voices download into `/data` on first run — minutes on slow links, hundreds
   of MB of space. Mitigation: document sizing/first-run latency in DOCS.md;
   models persist after first download (Task 5.7).

5. **32-bit arch gap.** The Vulkan 64-bit binary has no armv7/armhf/i386 build,
   and HA's current schema only lists `aarch64`/`amd64` anyway. Mitigation:
   declare only `aarch64`+`amd64`; document that 32-bit HA hosts are unsupported
   (they would need a separate non-Vulkan 32-bit build — out of scope).

6. **Non-Vulkan small-image variant (open question).** Should a CPU-only,
   Vulkan-stripped image be offered for tiny boards to cut size and complexity?
   Decision deferred; weigh CI/maintenance cost vs. the constrained-board
   audience.

7. **Wyoming Supervisor discovery uncertainty (Task 3.5).** The exact
   `discovery`/service declaration that makes HA auto-offer the add-on as a
   Wyoming provider must be verified against the current Supervisor + Wyoming
   integration; if it can't be confirmed, fall back to the manual Wyoming
   Protocol integration step (still one extra click, fully functional).

8. **`host_network` vs mDNS.** If the Wyoming integration's discovery relies on
   mDNS reaching the HA host, the internal network may need `host_network`,
   which lowers the security rating. Mitigation: prefer explicit host/port setup
   (no mDNS dependency) and only enable `host_network` if testing proves it
   necessary.

9. **Version/tag drift.** If `config.yaml` `version` and the GHCR image tag
   diverge, Supervisor fails to pull. Mitigation: the automated bump (Phase 4)
   makes the tag the single source; CI asserts the referenced tag exists.

## Alternative Approaches

1. **Local-build add-on (ship a `Dockerfile`/`build.yaml` that compiles Fono in
   HA).** Rejected as the primary path: slow, high-failure, SD-card wear, and it
   discards the existing multi-arch CI build. Useful only as an experimental
   fallback while iterating (Testing doc's "comment out `image:`" trick).

2. **Docs-only (Path A) forever, no add-on.** Lowest effort; already works on
   all install types. Loses the one-click UX for the large HAOS audience —
   acceptable only if add-on maintenance can't be committed to.

3. **Bake the HA shim into the Fono image entrypoint directly** (env-flagged
   `options.json` parsing). Avoids a second image, points `image:` straight at
   GHCR. Trade-off: adds HA-specific logic to the shared entrypoint; keep it
   behind a flag so non-HA users are unaffected. Viable; weigh against the
   wrapper-image option in Task 0.2.

4. **Aim straight for the official Community Add-ons org.** High bar (review,
   AppArmor, ongoing maintenance, security rating). Recommended only after the
   custom-repo add-on is proven; start with the custom repository (Task 6.4).
