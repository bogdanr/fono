# ADR 0012 — Wake-word activation

## Status

Accepted 2026-06-23.

(Supersedes the 2026-04-28 *Reconstructed* stub, which recorded the
feature as out-of-scope for the v0.x line. Wake-word now exists,
behind a default-off `[wakeword]` config block. This ADR also
supersedes the single wake-word line in ADR 0004 — "Transducer KWS
(k2-fsa / sherpa) … chosen over openWakeWord" — which is reversed
here; see the engine rationale below. ADR 0004 should be updated to
point at this ADR for the wake-word engine choice.)

## Context

Always-on wake-word activation ("hey fono, ...") lets users trigger
dictation without a hotkey: Fono idles, listens for a short phrase
on the default source, and on detection drives the same FSM path the
hotkey does. Implementations need either an on-device wake-word
model (Picovoice, openWakeWord, sherpa-onnx KWS, etc.) or a
cloud-streaming pass that pays per-second for idle audio and breaks
the privacy promise.

Two hard constraints frame the choice:

- **The idle-privacy promise.** No idle-state network traffic; audio
  must never leave the machine while Fono is merely listening for the
  wake phrase. This rules out any default that streams idle audio off
  the device.
- **The binary-size budget (ADR 0022).** The canonical `cpu` ship
  binary is capped at ≤ 32 MiB with a four-entry `NEEDED` allowlist
  (`libc`, `libm`, `libgcc_s`, `ld-linux`). Any wake-word engine that
  adds a second native runtime or a new dynamic dependency fails the
  size-budget gate, full stop.

The original 2026-04-28 stub deferred all of this to a future ADR and
kept hotkey-first activation (ADR 0011) as the only entry point, while
the interactive-mode plan reserved inert config keys against the day
the work landed. That day is this ADR.

## Decision

Wake-word activation ships as an **optional, default-off** capability
behind a `[wakeword]` config block. With `[wakeword].enabled = false`
(the default) behaviour is identical to today and no capture stream is
opened for it.

### (a) Engine: openWakeWord on the existing `ort` runtime

The detector is [openWakeWord](https://github.com/dscripka/openWakeWord),
run through the **minimal ONNX Runtime already linked via `ort`** for
the local voice stack (ADR 0032). No new runtime, no new native
dependency, no new `NEEDED` entry. The model is a small per-phrase
melspectrogram → embedding → classifier graph; a custom phrase is a
trained `.onnx` artifact rather than a token spec.

### (b) Rejected: sherpa-onnx open-vocabulary KWS

sherpa-onnx KWS was the engine named in ADR 0004 (a custom phrase is
specified by tokens, with no per-word model training — attractive on
paper). It is **rejected for v1 on size grounds**: sherpa needs a
*second* full ONNX runtime (or an ASR-sized op expansion of a
from-source build), which blows the ADR 0022 size-budget gate and adds
a runtime/`NEEDED` surface the allowlist forbids. It remains a
documented fallback to revisit only if arbitrary/multilingual phrases
become a hard requirement (see the plan's Alternative 2).

### (c) Phase-A size-gate result

The Phase-A size spike measured the openWakeWord op set against the
already-minimal `ort` build: only **7 small new ONNX operators**, **no
new `NEEDED` entry**, and the `release-slim` `cpu` delta stayed
**comfortably under the ≤ 32 MiB cap** with the four-entry allowlist
intact. openWakeWord rides the existing runtime essentially for free;
sherpa would have doubled it. This measured result is the deciding
factor between (a) and (b). (See ADR 0022 for the budget and the
allowlist; record the exact bytes there when the default model and its
ops land.)

### (d) Model-licensing policy — one clean default, NC opt-in only

The default wake phrase, `hey_fono`, is published under a **clean
Apache-2.0** license and is the **only** model enabled by default.
This mirrors ADR 0004's model-licensing posture exactly.

Upstream openWakeWord community phrases are distributed under
**CC-BY-NC-SA** (a NonCommercial license). They are treated as the
same "opt-in only, never default" carve-out ADR 0004 applies to
restricted models (the Llama-family / non-OSI-Gemma gate): the
community catalog is **opt-in, downloaded on demand, never bundled in
the shipped artifact, and accompanied by a NonCommercial-license
notice** shown when the user picks a community model. The notice
informs the choice; it does not block the download, and no acceptance
is recorded. No NonCommercial model is ever a default and none is
present in the release binary. Both default and opt-in models are
SHA-verified.

> **Human-owned policy point.** Whether to surface CC-BY-NC-SA
> community phrases at all is a licensing-stance decision for the
> maintainer, because NonCommercial terms sit awkwardly beside a
> GPL-3.0 project's ethos even when the NC artifact is never linked or
> shipped. This ADR records the implemented stance (opt-in /
> on-demand / notice-on-download / never bundled, no recorded consent);
> it does not unilaterally settle the policy.

### (e) Wyoming relationship — local server preserves privacy; client is opt-in

Fono exposes a Wyoming wake **Detection server**: an external Home
Assistant voice pipeline can discover Fono and receive `Detection`
events while **the audio stays on the local machine**. This direction
preserves the idle-privacy promise and is the recommended LAN
integration.

The Wyoming **client** direction — Fono forwarding idle mic audio to
an external `wyoming-openwakeword` service over the LAN — is **opt-in
and default-off**, behind an explicit privacy warning (surfaced in
config and in `fono doctor`) that idle microphone audio leaves the
machine over the network. The local embedded detector remains the
default; a Wyoming-only wake with no embedded detector was rejected as
a default because it breaks idle privacy and adds a hard LAN
dependency.

### (f) Idle-no-AEC invariant and the speaking sub-case seam

The headline path — Fono idle, nothing playing — reads the default
source directly and **must not load PipeWire AEC**. AEC is a no-op
when there is no Fono playback to cancel, cannot help reject TV /
music / non-wake speech (the AEC sink only carries Fono's own TTS),
and is Linux/PipeWire-only; idle wake-word must work cross-platform off
the default source. This is an invariant, not an optimisation.

The "wake / interrupt while the assistant is speaking" sub-case — where
the detector would consume `fono_aec_source_<pid>` to hear the phrase
over Fono's own TTS — is left as an **inert seam**: the capture +
detector abstraction is in place, but the AEC-source switching for the
speaking sub-case is not wired up in this slice. It can be engaged
later as an optional upgrade without rearchitecture.

### Default model status

The default `hey_fono` artifact is **not yet trained or hosted.** The
offline training pipeline exists (no model trained / hosted yet); the
registry SHAs are the **all-zeros `UNPINNED` sentinel with TODOs**
until the trained artifact is produced and pinned. The feature is
therefore complete in code but not usable out of the box until the
default model is published — see Consequences.

## Consequences

- Wake-word activation **exists** in the codebase, default-off behind
  `[wakeword]`. Enabling it with a model starts dictation/assistant on
  the wake phrase via the same FSM path as the hotkey.
- **No idle network traffic on the default local path**, and no
  always-listening footprint unless the user opts in. The Wyoming
  client path is the only way idle audio leaves the machine, and it is
  off by default behind a loud warning.
- **No new dependency and no new `NEEDED` entry**: openWakeWord reuses
  the `ort` runtime already present for the voice stack; the size
  budget (ADR 0022) holds with its four-entry allowlist.
- The interactive-mode reserved keys (`eou_adaptive`,
  `resume_grace_ms` in `[interactive]`) are no longer the only hook —
  the detector seam from the AEC slice is now realised with a KWS
  trigger.
- **Not yet shippable as a turnkey default**: the clean Apache-2.0
  `hey_fono` model must be trained and hosted (SHAs pinned) before a
  tagged release can advertise wake-word as working out of the box. The
  ROADMAP item stays in "up next / landed-pending-model" until then.
- `fono doctor` reports wake-word configuration, the active model, and
  the Wyoming client privacy warning when that path is enabled.

## Relationship to PipeWire AEC

Wake-word has two modes with opposite acoustic needs, and only one of
them touches the PipeWire echo-cancel work from
`plans/2026-05-25-double-talk-barge-in-pipewire-aec-v1.md`:

- **Idle always-on listening** (the headline feature: Fono idle,
  nothing playing). There is no Fono playback to cancel, so AEC is a
  no-op and must **not** be loaded. The detector reads the default
  source directly. AEC also cannot help with the real idle challenge —
  rejecting TV / music / non-wake speech — because the AEC sink only
  carries Fono's *own* TTS; ambient audio never passes through it.
  Idle wake-word must therefore work on every platform off the default
  source and must **not** depend on AEC (which is Linux/PipeWire-only).
  This is the **idle-no-AEC invariant** recorded in Decision (f), and
  it is enforced in the shipped idle path.
- **Wake / interrupt while the assistant is speaking** is exactly the
  "talk over the assistant" case AEC was built for. To detect a wake
  phrase over Fono's own TTS, the TTS must be cancelled from the mic —
  here wake-word **reuses** the AEC, consuming the same
  `fono_aec_source_<pid>`. In this slice that path is an inert seam
  (Decision (f)); the capture + detector abstraction exists but the
  AEC-source switch is not wired.

What is reusable is the **capture + detector seam**, not the
echo-canceller itself. Barge-in ("read a source → envelope/VAD → fire
`AssistantPressed`") and wake-word are the same shape with an energy
VAD vs a KWS model swapped in as the trigger. The detector abstraction
is landed once with interchangeable triggers, AEC is treated as an
optional upgrade engaged only while Fono is making noise, and the
lifecycle mismatch is accounted for (AEC is per-utterance and
short-lived; wake-word listening is always-on, so the detector
switches its input to the AEC source only while one exists and back to
the default source otherwise).

## Surviving artefacts

- `crates/fono-audio` — the `WakeWord` detector and `OnnxWakeWord`
  (feature `wakeword-onnx`).
- `plans/2026-06-23-wake-word-openwakeword-v2.md` — the implementation
  plan (Phases A–K).
- `plans/2026-04-27-fono-interactive-v3.md:204`
- `plans/2026-04-27-fono-interactive-v4.md:224-227`
- `plans/2026-04-27-fono-interactive-v5.md:228-229`
- ADR 0004 (default-models licensing policy), ADR 0022 (binary-size
  budget / `NEEDED` allowlist), ADR 0032 (ONNX voice stack on `ort`).
