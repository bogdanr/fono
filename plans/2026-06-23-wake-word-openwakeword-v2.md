# Wake-Word Activation via openWakeWord (with Wyoming + on-demand models)

> v2 supersedes `plans/2026-06-23-wake-word-openwakeword-v1.md`. Changes:
> (1) adds optional **Wyoming wake-word server** integration so Fono can
> act as a drop-in wake service for Home Assistant et al.; (2) adds
> **opt-in, on-demand download of the upstream openWakeWord models**
> (CC-BY-NC-SA, NonCommercial) alongside the clean-license default "hey
> fono" model; (3) clarifies the local-only stance and licensing policy.

## Objective

Add optional, always-on, **fully on-device** wake-word activation ("hey
fono, …") to Fono using the **openWakeWord architecture** running on the
**existing minimal `ort` runtime** already shipped for Kokoro/Piper. On
detection, the feature synthesizes the same `HotkeyAction` the physical
hotkey produces, so dictation or the assistant starts through zero new
orchestrator paths. The feature ships **disabled by default**, satisfies
the "no idle-state network traffic" promise on every platform for its
default local path, and stays within the binary-size budget (ADR 0022 /
`release-slim` / 4-entry `NEEDED` allowlist).

Two scope extensions over v1:

- **Wyoming wake-word server:** expose Fono's local detector over the
  Wyoming `Detection` protocol via the existing `fono serve wyoming`
  surface (`crates/fono-net/src/wyoming/server.rs`), so Home Assistant
  and other Wyoming consumers can use Fono as their wake service. This
  preserves the privacy promise (Fono *is* the detector; audio stays
  local). Historically scoped as a "separate ROADMAP item"
  (`plans/2026-05-25-local-tts-piper-kokoro-and-wyoming-server-v1.md:311`,
  `plans/2026-05-31-local-tts-ggml-piper-kokoro-and-wyoming-server-v2.md:456`);
  this plan folds it in as an **optional phase** that can ship later.
- **On-demand upstream models:** ship the clean Apache "hey fono" model
  as the only default, but let users **opt in to downloading** the
  upstream openWakeWord phrases ("hey jarvis", "alexa", …) on request,
  behind a clear NonCommercial-license notice.

This plan implements the decision reached in prior analysis: openWakeWord
over sherpa-onnx, because sherpa cannot be linked without a second full
ONNX runtime (size-gate failure) whereas openWakeWord rides the runtime
Fono already ships.

### Expected outcomes
- A `[wakeword]` config block (default off) and a tray toggle mirroring
  the existing VAD toggle.
- A `WakeWord` detector in `fono-audio` behind a trait, interchangeable
  with an energy/stub implementation, consuming the existing 16 kHz mono
  forwarder.
- A daemon-owned always-on listener that suspends during any real
  recording/assistant session and fires a `HotkeyAction` into the
  existing `action_tx`.
- A clean-license "hey fono" ONNX model as the default, plus an opt-in
  on-demand fetch path for upstream CC-BY-NC-SA models, all SHA-pinned
  through `fono-download`.
- An optional Wyoming wake-word server that advertises a `Detection`
  service and emits detections from the same detector.
- ADR 0012 promoted to **accepted** (recording the openWakeWord choice,
  the NonCommercial-opt-in policy, and the Wyoming relationship); ROADMAP
  item moved to Shipped at release; CHANGELOG entry added.

## Assumptions

- **English-first, fixed wake phrases.** Default is the clean "hey fono"
  model; opt-in upstream models add more *fixed English phrases*. Truly
  arbitrary / multilingual open-vocabulary phrases remain out of scope
  (the documented sherpa-onnx fallback trigger).
- **Idle local detection must NOT depend on AEC** (Linux/PipeWire-only);
  AEC reuse is only for the "wake while Fono is speaking" sub-case and is
  an optional upgrade, per `docs/decisions/0012-wake-word-activation.md:39-68`.
- **Upstream pretrained classifiers are CC-BY-NC-SA 4.0 (NonCommercial)**:
  they **cannot be a default** and **cannot be bundled** in the release
  artifact, but **may be offered as an explicit opt-in on-demand
  download** because (a) the model is runtime data loaded by `ort`, not
  linked into the GPL binary, and Fono never ships the bytes; (b) the
  NonCommercial restriction binds the end user, who is shown the license
  before download. This mirrors the existing "opt-in only, never default"
  carve-out for Llama/Gemma variants (AGENTS hard rules; ADR 0004).
- **The default "hey fono" model is a freshly-trained clean-license
  artifact** (Apache melspectrogram + Apache Google `speech_embedding`
  backbone + Piper-synthetic positives + openly-licensed negatives), the
  same pattern used for Kokoro and the espeak core.
- **Wyoming wake-word has two directions.** The **server** direction
  (Fono exposes its detector over Wyoming) preserves the idle-privacy
  promise and is the recommended integration. The **client** direction
  (Fono delegates its own activation to an external wyoming-openwakeword
  box) streams idle mic audio over the LAN and therefore **breaks** the
  "audio never leaves the machine while idle" guarantee; it is included
  only as an explicitly-labelled opt-in, never a default.
- Detector runs single-threaded on a fraction of one core, mirroring
  `with_intra_threads(1)` in `crates/fono-tts/src/kokoro.rs:230` and
  `crates/fono-tts/src/piper.rs:223`.
- Trigger target is configurable: dictation (`TogglePressed`) or assistant
  (`AssistantPressed`); both `HotkeyAction` variants exist in
  `crates/fono-hotkey/src/fsm.rs:131-156`.
- The detector reuses the existing `ort` workspace dependency
  (`Cargo.toml:127`) and the existing Wyoming codec/server in `fono-net`
  / `fono-net-codec` — no new runtime dependency to flag. Any genuinely
  new crate (e.g. an FFT/melspectrogram helper) MUST be flagged for size
  sign-off before adding.

## Implementation Plan

### Phase A — Engine spike & size measurement (GATE; do first)

- [ ] Obtain the openWakeWord ONNX graphs (Apache melspectrogram + Google
  Apache `speech_embedding` backbone) and one throwaway classifier for
  op-set analysis. Rationale: the whole feature is gated on size; this is
  the one step that could change the answer (cf. the Kokoro Phase-A spike
  in `docs/status.md`, 2026-06-03).
- [ ] Convert the graphs with `scripts/gen-ort-models.sh` (which already
  anticipates "KWS" — `scripts/gen-ort-models.sh:8`) to `.ort` and emit a
  `required_operators_and_types.config`. Rationale: produces the exact op
  delta the minimal runtime must absorb.
- [ ] Diff the new ops against the union `ops.config` via
  `scripts/merge-ort-configs.py`; rebuild the minimal runtime with
  `scripts/build-onnxruntime-minimal.sh`. Rationale: quantifies runtime
  growth before any wiring.
- [ ] Measure `release-slim` size and confirm the 4-entry `NEEDED`
  allowlist is unchanged (ADR 0022); record the delta. Rationale: pass/
  fail gate for the whole feature.
- [ ] **Decision checkpoint:** if preprocessing ops blow the budget, fall
  back to computing the melspectrogram in Rust and shipping only the
  embedding + classifier graphs (Alternative 1). Rationale: keeps the
  size promise in the worst case.

### Phase B — Clean-license default "hey fono" model

- [ ] Build a one-time **offline training pipeline** (in `scripts/` or a
  `calibration/` subtree, not shipped in the binary): Piper-synthetic
  "hey fono" positives + openly-licensed negatives only. Rationale: the
  upstream classifiers are NonCommercial; the *default* must be clean.
- [ ] Train the per-phrase classifier on the frozen Apache Google
  embedding; target < ~0.5 false-accepts/hour, < ~5% false-reject.
  Rationale: idle always-on is unusable if it false-fires on TV/music.
- [ ] Document model provenance (positive synthesis source, negative
  corpus licenses, training config) for the ADR/licensing record.
  Rationale: GPL-3.0 license hygiene (AGENTS hard rule).
- [ ] Pin the artifact's SHA-256 and publish it to the release model
  host. Rationale: reproducible, verifiable fetch.

### Phase C — `WakeWord` detector behind a trait (fono-audio)

- [ ] Add a `WakeWord` trait in `crates/fono-audio/` shaped like the
  `Vad` trait (`crates/fono-audio/src/vad.rs:17`): consume 10–30 ms
  frames of 16 kHz mono f32, return a scored fire/no-fire decision.
  Rationale: matches the established seam and the ADR's "interchangeable
  triggers" intent (`docs/decisions/0012-wake-word-activation.md:59-68`).
- [ ] Provide an **energy/no-op stub** (like `WebRtcVadStub`,
  `crates/fono-audio/src/vad.rs:25`) and the **openWakeWord ONNX
  detector** on `ort` with `Session::builder().with_intra_threads(1)`
  (cf. `crates/fono-tts/src/kokoro.rs:230`). Rationale: keeps the pipeline
  functional with no model present; isolates the ONNX dependency.
- [ ] Implement the streaming front-end: melspectrogram → embedding →
  classifier with the correct hop (~80 ms), ring-buffering partial frames
  from the forwarder. Support **loading multiple classifiers at once**
  (default "hey fono" plus any opted-in phrases) sharing the one frozen
  embedding pass. Rationale: the shared backbone makes extra phrases
  nearly free; the forwarder delivers arbitrary-length slices.
- [ ] Add an optional **VAD pre-gate** (reuse the energy stub) so the
  classifier only runs on plausible speech. Rationale: cuts idle CPU and
  false-accepts.
- [ ] Unit-test the detector contract with synthetic frames the way
  `vad.rs` and `crates/fono-audio/src/capture.rs:648` do — no real audio
  device. Rationale: deterministic CI.

### Phase D — Always-on listener lifecycle (daemon)

- [ ] When `[wakeword].enabled`, open a capture stream via
  `AudioCapture::start_with_forwarder`
  (`crates/fono-audio/src/capture.rs:176`) feeding the detector on a
  bounded channel (`try_send`, drop-on-overflow). Rationale: reuses the
  proven real-time tap; never blocks the capture thread.
- [ ] **Suspend the listener** whenever a real session opens
  (`Recording`, `LiveDictating`, `Assistant*` —
  `crates/fono-hotkey/src/fsm.rs:91-126`) and **resume on `Idle`**.
  Rationale: single mic source; concurrent streams contend. This is the
  lifecycle mismatch ADR 0012:65-68 calls out.
- [ ] Wire the listener into daemon startup / config-reload alongside the
  existing hotkey/IPC listeners near the `action_tx` setup
  (`crates/fono/src/daemon.rs:192`). Rationale: single ownership beside
  the other input sources.

### Phase E — Trigger into the existing FSM

- [ ] On confirmed detection, synthesize the configured `HotkeyAction`
  into the existing `action_tx`
  (`mpsc::UnboundedSender<HotkeyAction>`, `crates/fono/src/daemon.rs:192`):
  `TogglePressed` (dictation) or `AssistantPressed` (assistant). Where a
  per-phrase target mapping is configured, fire the action bound to the
  detected phrase. Rationale: starts through the *same* path as the
  physical hotkey — zero new orchestrator branches.
- [ ] Add a **post-fire refractory window** so one utterance can't
  double-trigger and the suspend (Phase D) races cleanly with the new
  session's state transition. Rationale: prevents self-retrigger.

### Phase F — Config surface + tray toggle

- [ ] Add a `[wakeword]` struct to `crates/fono-core/src/config.rs`
  (mirroring `Audio`, `crates/fono-core/src/config.rs:220`) with at least:
  `enabled: bool` (default `false`); a list of active phrases/models each
  with `sensitivity: f32` and `target: enum {Dictation, Assistant}`
  (default `Dictation`); and a `wyoming` sub-block (Phase H). Use
  `#[serde(default)]` so existing configs load unchanged. Rationale:
  discoverable, additive, off by default; supports multi-phrase.
- [ ] Add a `SetWakeWordEnabled(bool)` tray message mirroring
  `SetVadEnabled(bool)` (`crates/fono-tray/src/lib.rs:281`) and translate
  it in the daemon's tray-IPC arm like `vad_backend`
  (`crates/fono/src/daemon.rs:1154`). Rationale: established pattern,
  menu legibility.
- [ ] Honour `[wakeword]` on config reload so toggling and phrase changes
  take effect without a daemon restart. Rationale: parity with the live
  VAD toggle.

### Phase G — Model fetch + verification (default + opt-in upstream)

- [ ] Register the default "hey fono" model (and the shared Apache
  embedding + melspectrogram graphs, if shipped separately) with pinned
  SHA-256 and fetch via
  `fono_download::download(url, dest, expected_sha256)`
  (`crates/fono-download/src/lib.rs:18`), co-located with existing
  registry conventions (cf. `crates/fono-stt/src/registry.rs`).
  Rationale: every model is fetched/verified this way.
- [ ] Add a **catalog of opt-in upstream openWakeWord phrases** ("hey
  jarvis", "alexa", "hey mycroft", …), each SHA-pinned, marked with its
  **CC-BY-NC-SA 4.0 (NonCommercial)** license. These are **never default,
  never bundled in the release artifact** — only fetched on explicit user
  request. Rationale: gives users the broader phrase set without tainting
  the GPL distribution.
- [ ] Gate the opt-in download behind a **visible NonCommercial license
  notice** the user must acknowledge (CLI prompt + wizard/tray copy) and
  record the acknowledgement. Rationale: informed consent; Fono surfaces
  the restriction rather than hiding it (BY/attribution + NC disclosure).
- [ ] Surface the opt-in models in the wizard/tray phrase picker and in
  `fono use`-style CLI, clearly badged "community / NonCommercial".
  Rationale: discoverability with honest labelling.

### Phase H — Wyoming wake-word integration (OPTIONAL; may ship later)

- [ ] **Server (recommended):** extend the existing Wyoming server
  (`crates/fono-net/src/wyoming/server.rs`) and codec
  (`crates/fono-net-codec/src/wyoming.rs`) to advertise a wake-word
  **`Detection`** service in its `info`/`describe` handshake and emit a
  `Detection` event when the local detector fires, driven by the same
  `WakeWord` detector from Phase C. Rationale: lets Home Assistant and
  other Wyoming consumers use Fono as a drop-in wake service while audio
  stays local; aligns with `plans/2026-06-22-home-assistant-addon-v1.md`.
- [ ] Advertise the wake service over the existing mDNS discovery
  (`crates/fono-net/src/discovery/`) so HA auto-discovers it, mirroring
  how the STT/TTS Wyoming services are advertised. Rationale: zero-config
  pairing parity with the existing services.
- [ ] **Client (opt-in only, NOT default):** allow `[wakeword].wyoming`
  to point Fono's *own* activation at an external `wyoming-openwakeword`
  service, with config copy and `fono doctor` output that **explicitly
  warns this streams idle mic audio over the LAN and breaks the
  audio-never-leaves-the-machine-while-idle guarantee**. Rationale: some
  users (e.g. an HA-centric setup) want centralised detection; make the
  privacy trade-off loud and opt-in, never silent.
- [ ] Round-trip tests mirroring `crates/fono-net/tests/wyoming_server_round_trip.rs`
  for the new `Detection` message path. Rationale: protocol correctness
  without external services.

### Phase I — AEC relationship (idle vs. speaking)

- [ ] Keep the **idle** detector reading the default source directly,
  **no AEC**, on every platform. Rationale: ADR 0012:45-52 — AEC is
  Linux/PipeWire-only and can't reject ambient TV/music anyway.
- [ ] Where the barge-in AEC source (`fono_aec_source_<pid>`) exists
  *while Fono is speaking*, let the detector switch input to it for the
  wake/interrupt sub-case, then back to default when it disappears. The
  barge-in plan already names this seam
  (`plans/2026-05-25-double-talk-barge-in-pipewire-aec-v1.md:435-444`).
  Rationale: ADR 0012:53-68 — reuse the capture+detector seam; AEC is an
  optional upgrade. May defer to a follow-up slice.

### Phase J — `fono doctor` + documentation

- [ ] Extend `fono doctor` (`crates/fono/src/doctor.rs`) to report:
  active phrases + their license badges, detector backend (stub vs ONNX),
  measured idle CPU, and — if the Wyoming client path is enabled — a
  prominent privacy warning. Rationale: discoverability + honest field
  debugging.
- [ ] Document the feature in existing docs (`docs/providers.md`,
  `docs/configuration.md`, `docs/home-assistant.md`): enabling it, the
  fixed-phrase/English-first limit, the **NonCommercial opt-in download**
  and its terms, the Wyoming server/client directions and their privacy
  implications, and the custom-phrase training story. Update existing
  docs; do not invent new top-level docs. Rationale: AGENTS doc hygiene.

### Phase K — ADR, roadmap, changelog (release hygiene)

- [ ] Promote `docs/decisions/0012-wake-word-activation.md` to
  **Accepted**, recording: the openWakeWord choice; the sherpa-onnx
  rejection (second-runtime/size-gate); the Phase-A size result; the
  clean-license default model; the **policy that NonCommercial upstream
  models are opt-in/on-demand only, never default or bundled**; and the
  Wyoming server/client relationship + privacy stance. Cross-reference
  ADR 0004. Rationale: these are human-owned policy decisions that must
  be recorded.
- [ ] At release, move the wake-word item in `ROADMAP.md` (advertised
  around `ROADMAP.md:116-124`) to **Shipped**, tagged and dated.
  Rationale: AGENTS release rule — roadmap sync is non-negotiable.
- [ ] Add a `## [X.Y.Z] — YYYY-MM-DD` CHANGELOG entry before tagging.
  Rationale: the release workflow extracts it into the Release body.
- [ ] Batch the ADR/roadmap/doc changes into a **single commit**; run the
  full pre-commit gate (`cargo fmt --all -- --check`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo test --workspace
  --tests --lib`); sign off (`git commit -s`); do **not** push without
  explicit instruction.

## Verification Criteria

- Phase-A spike documents the `release-slim` size delta and confirms the
  `NEEDED` allowlist still has exactly 4 entries; the feature proceeds
  only if within the ADR 0022 budget.
- With `[wakeword].enabled = false` (default), behaviour is identical to
  today and no capture stream is opened.
- With the default model enabled, speaking "hey fono" while idle starts
  the configured action via the same FSM path as the hotkey, observable
  as the same `fsm event: StartRecording(...)` / assistant transition in
  the daemon log (cf. `docs/troubleshooting.md:19`).
- The listener provably suspends during any active session and resumes on
  `Idle`; only one capture stream is ever open.
- Idle CPU is a fraction of one core, with **no network traffic while
  idle on the default local path** (zero outbound connections during idle
  listening).
- Opt-in upstream models cannot be installed without the NonCommercial
  license being shown and acknowledged; they are absent from the shipped
  release artifact; both default and opt-in models are SHA-verified.
- If the Wyoming **server** is enabled, an HA voice pipeline can discover
  Fono and receive `Detection` events with audio remaining local; if the
  Wyoming **client** is enabled, `fono doctor` shows the explicit
  idle-audio-leaves-the-machine warning.
- Detector + Wyoming round-trip unit tests pass deterministically without
  hardware or external services.
- `cargo fmt --check`, `cargo clippy -D warnings`, and the workspace test
  suite pass; every added Rust file starts with the SPDX header.
- ADR 0012 is Accepted (incl. the NC-opt-in policy and Wyoming stance),
  ROADMAP shows the item Shipped, CHANGELOG has the release section — all
  in a single batched commit.

## Potential Risks and Mitigations

1. **New ONNX ops blow the size budget.**
   Mitigation: Phase A measures the delta first; fall back to a Rust
   melspectrogram shipping only embedding+classifier (Alternative 1).
2. **Idle false-accepts on TV / music / background speech.**
   Mitigation: per-phrase tunable `sensitivity`, an energy/VAD pre-gate,
   and a training quality bar (Phase B).
3. **Mic contention with push-to-talk and the assistant.**
   Mitigation: strict suspend-on-session lifecycle (Phase D) + post-fire
   refractory window (Phase E) guarantee one source at a time.
4. **NonCommercial models mishandled (bundled, defaulted, or installed
   without consent).**
   Mitigation: never default, never bundled; opt-in download gated behind
   a shown+acknowledged license; policy recorded in ADR 0012/0004
   (Phase G, Phase K).
5. **Wyoming client path silently leaking idle audio.**
   Mitigation: client direction is opt-in only, default-off, with loud
   config + `fono doctor` warnings; the local detector remains the
   default and the server direction keeps audio on-device (Phase H).
6. **Cross-platform capture parity for the idle path.**
   Mitigation: detector consumes the platform-agnostic
   `start_with_forwarder` output; idle path never depends on PipeWire AEC
   (Phase I).

## Alternative Approaches

1. **Melspectrogram in Rust, only embedding+classifier on `ort`.**
   Primary fallback within the openWakeWord choice if Phase A shows the
   melspec ONNX ops are the costly part. Trade-off: more Rust DSP to
   maintain, smaller op delta, full front-end control.
2. **sherpa-onnx open-vocabulary KWS (documented fallback, NOT now).**
   Revisit only if arbitrary/multilingual phrases become a hard
   requirement. Trade-off: open-vocabulary with no training, but needs a
   second ONNX runtime or a from-source build with an ASR-sized op
   expansion — rejected for v1 on the size constraint.
3. **Wyoming-only wake (no embedded detector).**
   Rely entirely on an external wyoming-openwakeword service and ship no
   local detector. Rejected as the default: it breaks the idle-privacy
   promise and adds a hard LAN dependency; retained only as the opt-in
   client path in Phase H.
4. **Defer the "wake while speaking" AEC sub-case and/or the Wyoming
   phase to later slices.** Ship the idle local headline path in v1 and
   add AEC-source switching (Phase I) and Wyoming (Phase H) once the
   barge-in/HA-addon work matures. Trade-off: simpler v1, fewer
   integrations at first ship.
