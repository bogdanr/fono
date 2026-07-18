# Speaker verification: local voice biometrics with per-action gating

## Objective

Give Fono a local, private "who is speaking" capability (Google Voice
Match analogue), so that:

- Every transcript / assistant turn can carry `speaker: { name,
  confidence }` metadata.
- Sensitive actions (Home Assistant automations, protected assistant
  tools) can be gated on an enrolled, verified speaker.
- Everything runs on-device on the existing ONNX voice stack; nothing
  biometric ever leaves the machine.

This is **identification + a convenience gate, not authentication**.
Modern voice cloning and replay attacks defeat voice biometrics; the
design enforces asymmetric gating (see "Security posture").

## Key facts grounding this plan

- ADR 0032 already names speaker-ID as a planned consumer of the ONNX
  runtime (`docs/decisions/0032-onnx-voice-stack-runtime.md:28`), and
  fixes the stack as **CPU-only (XNNPACK)** — ONNX Runtime has no
  Vulkan EP (`docs/decisions/0032-onnx-voice-stack-runtime.md:116-131`).
- The wake-word engine is the structural template: mel front-end →
  `ort::Session` → score vs stored references
  (`crates/fono-audio/src/wakeword.rs:353-427`), with a registry for
  user-managed phrases (`crates/fono-audio/src/wake_registry.rs`).
- Model distribution playbook: hosted pinned pack on the fono-voice
  mirror, `.ort` conversion, `ops.config` union, minimal-runtime
  rebuild, size gate — the Supertonic slices
  (`plans/2026-07-12-supertonic3-local-tts-engine-v1.md`, status.md
  2026-07-14 entries).
- Model-registry precedent: ADR 0027 (STT quantization ladder) — a
  registry of named models with one default; note we deliberately do
  **not** replicate its tier ladder here yet (see "Model selection").
- **The daemon is often remote.** Users drive Fono over the network:
  the web settings UI, the OpenAI-compatible
  `/v1/audio/transcriptions` upload, and Wyoming satellites (Home
  Assistant) all deliver audio that never touched the daemon's own
  microphone. Enrollment and verification must both work over these
  channels, not just the local mic.
- Web settings SPA has a hash router (`#/settings`, `#/doctor`) and
  token/loopback auth plumbing (status.md 2026-07-15 web-doctor entry;
  `crates/fono-net/src/web_settings/`), plus Web Audio testers.
- Per-key store pattern for private data: `api_keys.sqlite`, mode 0600
  (`crates/fono-core/src/api_keys.rs`).
- Research grounding (arXiv 2606.22369, Kiwano toolkit, Apache-2.0):
  - Small models are competitive: ReDimNet-B6 (15 M params) reaches
    0.58 % EER on VoxCeleb1-O; ECAPA2 generalizes worst out-of-domain
    (14.68 % EER on CN-Celeb) — avoid the ECAPA family.
  - **Out-of-domain reality check**: far-field / cross-lingual EER is
    3.6–6 % (DiPCo), not the 0.3–0.5 % headline. Fono's living-room
    mic *is* the out-of-domain condition. Thresholds must be tuned on
    the user's own audio.
  - Cheap inference-time back-end wins ~30 % relative EER: length-norm
    + centering, **AS-Norm** (impostor cohort of ~200 embeddings,
    ~200 KB shipped in the pack), **QMF** score calibration. All
    implementable in plain Rust.
  - Verification wants ≥ 3–5 s of speech; short commands must
    accumulate wake-phrase + command audio.

## Metric note (WER vs EER)

STT quality is WER; speaker verification quality is **EER / minDCF**
(false-accept vs false-reject trade-off). The per-environment balance
maps to: *strict* deployments (alarm/gate, banking-style) want a
low-false-accept operating point on a capable model; *efficient*
deployments (laptop, battery) want a small model at a balanced
threshold. The knob surface is: selected model × decision threshold ×
minimum-speech duration.

## GPU / NPU analysis (decision: CPU-only for v1)

- **No latency need.** Embedding models in scope are 2–30 M params and
  run once per utterance (event-driven, not continuous). A 3–5 s
  utterance embeds in tens of milliseconds on AVX2 CPU — invisible
  next to STT. The always-on component (wake word) already runs CPU
  within budget; speaker scoring adds nothing continuous.
- **Battery**: per-utterance CPU burst is cheaper than waking a
  GPU/NPU; on laptops CPU-only is the *better* power story.
- **No viable EP anyway** under Fono's constraints: ONNX Runtime has
  no Vulkan EP; OpenVINO (Intel NPU), QNN (Qualcomm), DirectML are
  dynamic-library EPs that would break the 4-entry `NEEDED` allowlist
  and the minimal-build discipline (ADR 0032). The ggml-Vulkan side is
  inapplicable (these models are ONNX, not GGUF).
- **Doors left open, explicitly deferred**: (a) macOS CoreML EP (ANE)
  — CoreML is a system framework already conceptually inside the
  Mach-O allowlist model; revisit only if a larger model proves too
  slow on Apple Silicon CPU (unlikely); (b) if batch workloads ever
  appear (retro-diarizing history), re-evaluate.

## Model selection (no ladder yet — deliberate)

We have no first-hand benchmarks to base a quality/efficiency ladder
on, so **no tiers, no `"auto"`, no profiles in v1**. Instead:

- A plain model **registry** (name → pack + checksums + fbank config),
  user-selectable via `[speaker].model` and the web UI picker.
- **Default: `redimnet-b6`** — the strongest published
  efficiency/accuracy point we know of (15 M params, 0.58 % EER
  VoxCeleb1-O, Apache-2.0, ONNX exports available).
- Additional models are additive registry rows later (new pack, no
  schema change). A tier ladder à la ADR 0027 comes **only after** we
  have our own calibration benchmarks (own trial lists, own hosts) to
  ground it — explicitly out of scope here.
- Acceptance gate for any model we host: measured EER on a pinned
  trial list (Slice 5) within a documented bound of the published
  figure, and Rust-engine scores matching the Python oracle within
  tolerance.

## Security posture (recorded in the ADR)

- Speaker match alone may gate **fail-safe** actions (arm, lock,
  lights). **Fail-deadly** actions (disarm, unlock, open) require
  speaker match **plus** a second factor outside the voice channel.
  2nd factor might be a PIN which we register with the user.
  Consistent with the voice-preset rule that voice never authorizes
  irreversible actions.
- Quantitative basis: far-field EER 3.6–6 % (Kiwano/DiPCo), plus
  replay/cloning attacks that no threshold stops.
- Anti-spoofing models (AASIST-class) are out of scope for v1;
  documented as a possible later add-on.
- Embeddings are biometric data: local-only, `0600`

## Config surface

```toml
[speaker]
enabled  = false          # off by default
model    = "redimnet-b6"  # registry model name; default redimnet-b6
threshold = "auto"        # "auto" (from calibration) | explicit float
min_speech_secs = 3.0     # audio accumulated before a decision
```

- `threshold = "auto"` resolves from the shipped impostor cohort plus
  the user's own calibration stats (the "test my voice" flow); an
  explicit float pins it for strict deployments.
- Per-speaker data lives in a dedicated **`speakers.sqlite`** under
  the data dir (mode 0600, open/migrate/owner-clamp per the
  `api_keys.rs` pattern): `speakers(id, name UNIQUE, created_at,
  updated_at, calibration stats…)` + `speaker_utterances(speaker_id,
  embedding BLOB, capture_source, created_at)`. Embeddings are ~1 KB
  each (256 × f32) — BLOBs in the same DB, no sidecar files. Never
  in `config.toml`.
- Per-action requirements live where the action lives: assistant tool
  ACL entries and HA/Wyoming metadata (the consumer decides), not a
  parallel policy engine inside the speaker module.

## Web settings (primary surface)

New `#/speakers` page on the existing hash router:

- **Speakers table**: name, enrolled utterance count, last verified,
  per-speaker "re-enroll" / rename / delete.
- **Enrollment flow**: guided capture of 3–5 utterances of ≥ 5 s each,
  recorded **in the browser** and uploaded — one path for local and
  remote daemons alike:
  - The SPA records via `getUserMedia` with browser DSP disabled
    (`echoCancellation`/`noiseSuppression`/`autoGainControl: false`)
    and an input-device picker, downsamples to 16 kHz mono PCM in JS
    (AudioWorklet), and uploads plain WAV to `/api/speakers/enroll` —
    decodable by the existing `fono_core::wav::decode_wav`, no new
    codec dependency. Matches the Web Audio tester precedent.
  - **No daemon-capture web endpoints** (deliberate): a start/status/
    finish session API would add state and contend with the wake-word
    listener holding the mic, for marginal gain — desktop users who
    want true daemon-channel enrollment use the CLI (below), which
    reuses the existing capture machinery.
  The UI recommends enrolling through **the same channel you speak
  commands through** and in real room conditions; each utterance
  records its capture source so a mismatch can be warned about later.
- **Calibration card**: "test my voice" — runs N verification trials
  (browser-recorded), plots the user's genuine-score distribution vs
  the shipped impostor cohort, and shows where the active threshold
  sits (the Kiwano lesson: tune on the user's own room and channel,
  not paper numbers).
- **Settings card**: enable toggle, model picker from the registry
  (size/CPU hints per model), threshold (auto/manual), min-speech
  duration.
- API: `/api/speakers/*` on the settings server, same loopback-trust +
  API-key rules as the rest of `/api/*`.

## CLI (parity for scripting/headless)

`fono speaker` subcommand group:

- `fono speaker enroll <name> [--wav <file>…]` — interactive terminal
  enrollment via the daemon mic, or non-interactive from WAV files
  (the remote/scripted path: record anywhere, enroll from files).
- `fono speaker list | rename | remove`
- `fono speaker test [<name>]` — calibration trials, prints score
  distributions and the active threshold verdict.
- `fono speaker identify <wav>` — offline identification of a file
  (debugging, scripting).
- `fono doctor` gains a Speaker section: enabled state, model present,
  enrolled count, threshold source (auto/pinned), and a warning when
  enabled with zero enrolled speakers.

## Implementation plan

### Slice 1 — Model + distribution

- [~] Task 1.1. Default model pinned to **ReDimNet2-B3** (B6 optional
      tier) — see the design note above; superseded ReDimNet-B6. ONNX
      self-exported from the pinned `.pt` and verified faithful
      (torch-vs-onnxruntime embedding cosine 1.000000). **Remaining:**
      the Python-oracle EER cross-check on a pinned trial list and the
      CPU-RTF measurement are Slice 5.
- [x] Task 1.2. Converted both tiers to `.ort`, unioned the three
      net-new ops (`InstanceNormalization`, `ReduceProd`, `FastGelu`)
      into fono-voice `ops.config`, rebuilt the minimal runtime via the
      `build-onnxruntime` workflow, and re-pinned all five triples in
      `scripts/fetch-onnxruntime.sh`.
- [~] Task 1.3. Graphs (`redimnet2-b3.ort` / `-b6.ort`) + fbank config
      hosted on the `ort-1.24.2` mirror release and checksum-pinned in
      the registry + `manifest.json`. **Remaining:** the impostor-cohort
      sidecar (~200 KB) is generated in Slice 4/5, so its rows stay
      UNPINNED and AS-Norm degrades to plain cosine until it lands.
- [x] Task 1.4. `./tests/check.sh --size-budget` — passes at 21.79 MiB
      (budget 25 MiB); the larger op-set added no measurable binary
      delta (the linker drops unreferenced kernels), no ADR 0022
      sign-off needed.

### Slice 2 — Engine core (`fono-audio` or new module, behind a feature)

- [x] Task 2.1. Fbank front-end (80-dim log-mel, 25 ms / 10 ms, CMN) —
      reuse/extend the wakeword mel machinery where sensible.
- [x] Task 2.2. `ort` session + embedding extraction; length-norm +
      centering.
- [ ] Task 2.3. Back-end scoring: cosine, **AS-Norm** against the
      shipped cohort, optional **QMF**. Unit tests against
      oracle-generated fixtures.
- [x] Task 2.4. Enrollment store: **`speakers.sqlite`** (schema per
      "Config surface") — speaker rows + per-utterance embedding
      BLOBs with capture-source tags + calibration stats; 0600,
      `api_keys.rs` open/migrate pattern; delete-on-remove wipes the
      rows. Enrollment accepts raw 16 kHz PCM regardless of origin
      (daemon mic via CLI, uploaded WAV) — the engine is
      channel-agnostic by construction.
- [x] Task 2.5. Decision layer: (threshold, min-duration) from
      config; audio accumulation across wake phrase + command until
      `min_speech_secs` is met; emits `SpeakerDecision { name, score,
      confidence, sufficient_audio }`.

### Slice 3 — Config, web UI, CLI, doctor

- [x] Task 3.1. `[speaker]` config block + model registry (named
      rows, `redimnet-b6` default; no auto/tier logic).
- [~] Task 3.2. `/api/speakers/*` + `#/speakers` surface. **Done (model-
      independent):** `GET/PATCH/DELETE /api/speakers` (list/rename/
      remove) wired through daemon hooks over `SpeakerStore`; the
      Speakers settings section (enable/model/threshold/min-speech card
      + roster table with rename/delete) in the SPA. **Blocked on the
      model pack:** WAV enrollment upload, browser capture flow with
      DSP-off constraints + device picker, and the calibration card.
- [~] Task 3.3. `fono speaker …` CLI group + doctor section. **Done:**
      `fono speaker list|rename|remove` over `SpeakerStore`; `fono
      doctor` Speaker section (enabled state, registry model check,
      threshold source, enrolled count, zero-speaker warning).
      **Blocked on the model pack:** `enroll` (mic + `--wav`), `test`
      (calibration), `identify`.

### Slice 4 — Pipeline + consumer wiring

- [ ] Task 4.1. Tag transcripts and assistant turns with the speaker
      decision (runs in parallel with STT on the same 16 kHz buffer).
- [ ] Task 4.2. Expose identity metadata on the HA/Wyoming and MCP
      surfaces so automations/agents can gate on it.
- [ ] Task 4.3. Assistant tool ACL: per-tool `require_speaker` list +
      minimum confidence; refusal path speaks/prints a clear reason.
- [ ] Task 4.4. History: record the decision (name + score), never the
      embedding.

### Slice 5 — Verification + docs

- [ ] Task 5.1. Python-oracle cross-check (Kiwano or sherpa-onnx):
      same audio in → embedding cosine within tolerance, EER on the
      pinned trial list within the acceptance gate.
- [ ] Task 5.2. Deterministic E2E: enrollment fixtures → verify
      accept/reject around the threshold; regression-test the
      accumulation logic on short commands and the WAV-upload
      enrollment path end to end (the remote-daemon scenario).
- [ ] Task 5.3. Docs: ADR (model choice, CPU-only decision, security
      posture with the DiPCo numbers), `docs/privacy.md`,
      `docs/configuration.md`, `docs/home-assistant.md` gating recipe.

## Open questions (need sign-off before the relevant slice)

1. New crate check: expected **zero new crates** (`ort`, `sha2`,
   `rusqlite`, audio plumbing all present) — confirm at Slice 2.
2. Where the engine lives: `fono-audio` (next to wakeword) vs a new
   `fono-speaker` crate. Default proposal: `fono-audio` module behind
   a `speaker-onnx` feature, promoted only if it grows.
3. Whether Wyoming metadata extension is upstream-compatible or needs
   a Fono-specific field (check Wyoming spec at Slice 4).
4. Channel mismatch severity: enrolled-on-browser-mic vs
   verified-on-satellite-mic will cost accuracy; decide at Slice 5
   whether the UI should support per-channel enrollment sets or just
   warn (start with the warning).
