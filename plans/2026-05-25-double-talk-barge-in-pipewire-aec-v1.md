# Double-talk barge-in via PipeWire echo-cancel

## Objective

Let the user **talk over the voice assistant** to interrupt it, without
pressing F8 or Escape first. Today the assistant speaks the reply
through `paplay` and the only way to stop it mid-utterance is a key
press (`crates/fono/src/assistant.rs:88-101` and the
`AssistantSpeaking → AssistantPressed` barge-in arm in
`crates/fono-hotkey/src/fsm.rs`). With this slice, the moment the user
starts a sentence aloud, Fono auto-emits `AssistantPressed` — the
existing FSM does the rest (cut playback, retain rolling history,
start a new STT turn).

The catch every speakerphone designer hits: while TTS is playing
through speakers, the mic also hears the TTS, so a naive VAD on the
mic stream trips the instant the assistant starts speaking and you
get a feedback loop. The solution is **acoustic echo cancellation
(AEC)**. Rather than link an AEC library into the binary, we delegate
to PipeWire's `module-echo-cancel` (WebRTC AEC under the hood), which
is already present on 70–80 % of Fono's user base via the same
`pw-cat`/`paplay` shell-out pattern Fono already uses for capture and
playback.

**No new config option.** Either the probe at daemon start succeeds
and barge-in is on, or it fails and Fono stays exactly where it is
today (manual F8 / Escape). `fono doctor` is the single surface that
explains which state you're in.

## Background — what already ships

The plumbing is mostly in place. Audited 2026-05-25:

- **Linux capture** prefers `pw-cat` and falls back to `parec` —
  `crates/fono-audio/src/capture.rs:243-298`. Both are shell-outs, no
  linked audio library.
- **Linux playback** spawns `paplay` per utterance —
  `crates/fono-audio/src/playback.rs:6-10, 67-79`. Already has a
  per-stream rubato resampler so a different output rate on the AEC
  sink is free.
- **Envelope follower** (`inst_rms` + asymmetric `voiced_rms`) —
  `crates/fono-audio/src/envelope.rs:20-46`. The same primitive
  drives the existing PONDERING indicator.
- **Silence-watch state machine** (`Armed → Speaking → Pondering →
  Committed`) — `crates/fono-audio/src/silence_watch.rs:1-50`. The
  `Armed → Speaking` transition is exactly the "user started talking"
  event we need; the rest of the machine is unused on this path.
- **Auto-stop commit channel pattern** — slice 4 of the auto-stop
  plan already wires a `SilenceEvent` into `action_tx.send(
  HotkeyAction::TogglePressed)` (`crates/fono/src/session.rs:1025`).
  We follow the same shape, swapping the action for
  `HotkeyAction::AssistantPressed`.
- **Overlay state** `OverlayState::AssistantSpeaking` —
  `crates/fono-overlay/src/lib.rs:90`. No new state needed; we just
  arm the watch while the overlay is in this state.
- **`KeyHeldFlags::assistant`** — `crates/fono-hotkey/src/lib.rs`,
  added in the 2026-05-22 Pondering-parity slice. Suppresses
  auto-barge-in while the user is physically holding F8 (push-to-talk
  mode), preventing a self-trigger when the user's own held-key
  speech leaks into the mic.
- **Doctor row pattern for missing system tools** —
  `crates/fono-audio/src/capture.rs:262-268` already prints
  "Tried `pw-cat` … and `parec` … — install `pipewire-bin` or
  `pulseaudio-utils`". We mirror that style for
  `module-echo-cancel`.

What is **missing**:

1. A `fono-audio::aec_pipewire` module that loads
   `module-echo-cancel` with WebRTC AEC, returns a sink + source pair
   bound to a unique per-pid name, and unloads them on `Drop` (RAII).
2. A "keep capture alive during `AssistantSpeaking`" path. Today the
   capture pipeline is torn down at the end of STT
   (`crates/fono/src/session.rs` lifecycles); we need a short-lived
   capture+VAD spawned for the duration of TTS playback.
3. Routing the per-utterance `paplay` invocation at the AEC sink when
   capability is available.
4. `fono doctor` row reporting capability + actionable install hint.

## Pinned decisions

| Decision | Choice |
|---|---|
| Config knob exposed to users | **None.** Auto-detect at daemon start; on by default when capability probes green, silently off otherwise. |
| Platform scope (v1) | **Linux + PipeWire only.** macOS / Windows ship in a later slice (system-AEC APIs differ enough to warrant separate plans). |
| Headphone users | **Same code path.** AEC over a headphone-bound sink is a near no-op (no echo to cancel) and the VAD is what matters. No detection needed. |
| Default sink behaviour | **Never touch it.** TTS is routed at our AEC sink explicitly via `paplay --device=<our-sink>`; everyone else's audio (Zoom, music, browser) is untouched. |
| Module lifetime | **Per-utterance.** Load the AEC sink when assistant starts speaking, unload when playback drains. Per-pid names so two Fono instances can coexist; doctor sweeps stale leaked modules. |
| AEC algorithm | **WebRTC** (`aec_method=webrtc`). The classic Speex `null` method exists but WebRTC-AEC is the modern default and ships in the same module. |
| Fallback when probe fails | **Silent.** No notification, no warning toast. `fono doctor` is the surface. Manual F8 / Escape still works exactly as today. |
| Barge-in suppression while F8 held | **Yes.** `KeyHeldFlags::assistant.load(Acquire)` short-circuits the dispatch. Push-to-talk users keep their existing UX. |

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                  fono (the daemon), assistant turn              │
│                                                                 │
│  STT done ──► chat stream ──► TTS chunks ──► AudioPlayback     │
│                                                  │              │
│                                                  ▼              │
│                                          paplay --device=$AEC  │
│                                                  │              │
└──────────────────────────────────────────────────┼──────────────┘
                                                   │
                                                   ▼
   ┌───────────────────────────────────────────────────────────┐
   │           PipeWire / pipewire-pulse (system)              │
   │                                                           │
   │   sink   fono_aec_sink_<pid>  ◄─── ref ───┐               │
   │                                            │               │
   │   module-echo-cancel (WebRTC AEC)          │               │
   │                                            │               │
   │   source fono_aec_source_<pid>  ◄──────────┘               │
   │       (mic minus playback echo)                            │
   └────────────────────────┬──────────────────────────────────┘
                            │
                            ▼
   ┌───────────────────────────────────────────────────────────┐
   │   parec/pw-cat --device=fono_aec_source_<pid>             │
   │       │                                                    │
   │       ▼                                                    │
   │   EnvelopeFollower ──► SilenceWatch (Armed→Speaking)       │
   │       │                                                    │
   │       ▼  on Speaking transition (and !KeyHeldFlags)        │
   │   action_tx.send(HotkeyAction::AssistantPressed)           │
   │       │                                                    │
   │       ▼  (existing FSM)                                    │
   │   AssistantSpeaking → barge-in: cancel TTS, drop sink,     │
   │   start STT for the user's interjection                    │
   └───────────────────────────────────────────────────────────┘
```

Critical invariants:

- The AEC sink is **dedicated to assistant TTS**. The user's default
  sink is never touched. Music in Firefox / a Zoom call / `paplay`
  from another app all play through their normal route.
- The AEC source is **read only by Fono's barge-in capture task**,
  never the dictation capture path. F7 dictation continues to use
  the user's default source.
- Resource names embed the daemon PID so a crashed previous instance
  cannot collide with a newly-launched one.
- The whole lifecycle is RAII: load on `AecSession::new`, unload on
  `Drop`. Drop is called from both the normal end-of-utterance path
  and panic / shutdown paths via tokio cancellation. A best-effort
  `pactl unload-module` is also issued by the daemon's signal
  handler.

## Phase plan

### Phase 1 — capability probe + RAII wrapper

**Deliverable.** A pure-function `aec_pipewire::probe()` that returns
`Capability::{Available, MissingPactl, MissingModule(reason)}`, plus
an `AecSession` RAII type that loads/unloads the module pair.

Files to touch (new + existing):

- **new** `crates/fono-audio/src/aec_pipewire.rs`:
  - `pub fn probe() -> Capability`. Shells out
    `pactl load-module module-echo-cancel
        aec_method=webrtc
        source_name=fono_aec_probe_<pid>
        sink_name=fono_aec_probe_sink_<pid>
        use_master_format=true`
    then immediately unloads. Captures stdout for the module-id (first
    line is `<int>`). On non-zero exit, classifies stderr:
    `Failed to load module` → `MissingModule`;
    `command not found` / `pactl: ENOENT` → `MissingPactl`.
  - `pub struct AecSession { sink: String, source: String, module_id: u32, }`.
  - `pub fn load() -> Result<AecSession>`; `Drop` runs
    `pactl unload-module <id>`. Drop must be `try` — if PipeWire is
    already gone we log at `debug!` and move on.
  - Module-leak sweep: `sweep_stale_modules()` lists modules via
    `pactl list short modules`, finds any whose argument string
    contains `fono_aec_` with a PID that is not the current daemon's,
    and unloads them. Called once at daemon startup before any
    `probe()` is attempted.
- `crates/fono-audio/src/lib.rs`: re-export `aec_pipewire`.

Unit tests via a small `Pactl` trait so the shell-out is mockable
(same pattern as `HeadlessProbes` in `crates/fono/src/install.rs`).
Covers: success path, missing `pactl`, missing module, ENOENT on
unload (idempotent), stale-module sweep with mixed PIDs.

**Verification.** `cargo test -p fono-audio aec_pipewire`. Manual:
`pactl list short modules | grep echo-cancel` before / during /
after running a smoke test that constructs an `AecSession`,
sleeps 1 s, drops it.

### Phase 2 — barge-in capture task

**Deliverable.** A short-lived capture+VAD task spawned during
`AssistantSpeaking` that emits `HotkeyAction::AssistantPressed` on
the first sustained voiced frame.

Files to touch:

- `crates/fono/src/session.rs`: add `spawn_barge_in_watch_task(
    aec_source: &str,
    held_assistant: Arc<AtomicBool>,
    action_tx: Sender<HotkeyAction>,
    cancel_token: CancellationToken,
  )`. Mirrors `spawn_silence_watch_task` (slice 4 of the
  auto-stop plan) but with a stripped `SilenceWatch` config:
  only the `Armed → Speaking` transition matters. Reuses
  `EnvelopeFollower` with the existing defaults.
- `crates/fono/src/assistant.rs:88-101` and the AssistantSpeaking
  state transitions: own a `barge_in_handle: Option<AecHandle>`
  alongside the existing `playback: AudioPlayback`. The handle
  bundles `AecSession` + `JoinHandle` for the watch task and is
  cancelled on natural-completion + on FSM
  `AssistantSpeaking → Idle`.
- Barge-in dispatch: on `Speaking` event, if
  `held_assistant.load(Acquire) == false`, send
  `HotkeyAction::AssistantPressed` through `action_tx`. The
  existing daemon loop (`crates/fono/src/daemon.rs:733-770`)
  translates it the same way as a physical F8 press, including
  the rolling-history-retained barge-in path in
  `crates/fono/src/assistant.rs:255-280`.

Suppression rules to lock down with tests:

1. `KeyHeldFlags::assistant == true` → no dispatch (push-to-talk
   user's own held-key speech leaks into mic).
2. AssistantSpeaking is over (FSM moved on) → task is cancelled,
   no late dispatch.
3. Less than ~300 ms after AssistantSpeaking starts → ignore.
   Rationale: the AEC takes a few hundred ms to converge on the
   reference signal, before that the residual echo can spike
   inst_rms briefly. Use the existing
   `speech_confirm_arm_ms = 100` plus a one-shot 300 ms grace.

### Phase 3 — wiring `paplay` at the AEC sink

**Deliverable.** When `AecSession` is live, `AudioPlayback`'s
`paplay` invocation gains `--device=<aec_sink>`. When the session
isn't available, it falls through to today's default-sink play.

Files to touch:

- `crates/fono-audio/src/playback.rs`: thread an
  `Option<String>` device override into the worker. Today
  `AudioPlayback::new(device: Option<&str>)` already accepts a
  cpal device name and ignores it on the pulse path
  (`crates/fono-audio/src/playback.rs:67-71`); we honour the
  same field on the pulse path now.
- `crates/fono/src/assistant.rs`: construct
  `AudioPlayback::new(Some(&aec_session.sink))` when the
  capability is available, otherwise `None`.

The assistant lifecycle therefore becomes:

```
on AssistantSpeaking enter:
    if Capability::Available:
        aec = AecSession::load().ok()
        if aec:
            playback = AudioPlayback::new(Some(&aec.sink))
            watch = spawn_barge_in_watch_task(&aec.source, ...)
        else:
            playback = AudioPlayback::new(None)
    else:
        playback = AudioPlayback::new(None)

on AssistantSpeaking exit (natural / barge-in / Escape):
    watch.cancel()           // drops barge-in capture
    playback.stop()          // drains queue, kills paplay
    drop(aec)                // unloads module
```

### Phase 4 — `fono doctor` row + ROADMAP shipped entry

- `crates/fono/src/doctor.rs`: new section "Assistant barge-in" that
  prints the `Capability` result. On `MissingModule`, the line ends
  with the same install hint pattern the audio capture path uses
  (`install `pipewire-module-echo-cancel` (Debian/Ubuntu),
  `pipewire-pulse` already provides it on Fedora / Arch / openSUSE`).
- Same row should fold a "stale leaked modules: N (cleaned)" count
  on daemon startup so users notice if a previous crash left junk
  behind.
- ROADMAP `Up next` row dropped, new `Shipped` entry added when this
  releases.

## Behavioural matrix

| State | Default sink user audio | TTS playback target | Mic capture target | Barge-in fires? |
|---|---|---|---|---|
| Capability available, assistant speaking | unchanged | `fono_aec_sink_<pid>` | `fono_aec_source_<pid>` | yes, on confirmed voiced frame |
| Capability available, F8 held during TTS | unchanged | `fono_aec_sink_<pid>` | `fono_aec_source_<pid>` | **no** (push-to-talk gate) |
| Capability unavailable | unchanged | default sink (today's behaviour) | not running | no — manual F8/Esc only |
| Idle, F7 dictation | unchanged | n/a | default source (today's behaviour) | n/a |

## Risks and how each is contained

1. **Leaked module on crash.**
   *Risk:* `pactl load-module` succeeds, daemon `kill -9`s, module
   stays loaded forever.
   *Mitigation:* per-pid resource names, startup sweep that finds and
   unloads any `fono_aec_*` modules whose embedded PID is not the
   current daemon's PID. Signal handler (`fono/src/main.rs` SIGTERM
   path) also issues a best-effort unload.

2. **`module-echo-cancel` package missing.**
   *Risk:* probe fails on first install.
   *Mitigation:* explicit `Capability::MissingModule` with install
   hint in `fono doctor`. Daemon continues normally; manual F8/Esc
   barge-in still works (today's behaviour).

3. **PipeWire AEC poisons the user's default sink.**
   *Risk:* a config typo routes other apps' audio through the AEC.
   *Mitigation:* contract test in Phase 1's unit suite asserting the
   `pactl load-module` argument string does **not** contain
   `set-default-sink=true` or any other default-sink mutation. Our
   sink is targeted exclusively by `paplay --device=…`.

4. **Stacking on top of a user's existing echo-cancel.**
   *Risk:* user already loaded `module-echo-cancel` for Zoom; our
   second instance wastes CPU.
   *Mitigation:* Phase 4 doctor row notes the duplicate. We don't
   refuse to load — the two instances operate on different
   sinks/sources so there's no functional conflict, just ~1-2 % extra
   CPU during assistant playback.

5. **AEC residual misfires VAD.**
   *Risk:* WebRTC AEC isn't perfect; residual echo at TTS sentence
   starts could trip the VAD.
   *Mitigation:* 300 ms grace at AssistantSpeaking entry, then
   `speech_confirm_arm_ms = 100` of contiguous voiced frames before
   dispatching. The grace covers the AEC's convergence window.

6. **First-utterance latency.**
   *Risk:* loading the module on every utterance adds 50-200 ms before
   `paplay` can stream.
   *Mitigation:* warm the AEC session **once per assistant turn**
   (covers the streaming sentence-by-sentence playback within a single
   reply), not per chunk. Unload on AssistantSpeaking exit. If 50 ms
   still hurts perceived responsiveness, we can move to "warm the
   session on AssistantThinking enter" as a follow-up — the LLM
   first-token latency hides it entirely.

7. **Non-Linux platforms.**
   *Risk:* macOS / Windows have no PipeWire.
   *Mitigation:* `probe()` returns `MissingPactl` on non-Linux,
   barge-in stays off. Out of scope for v1; tracked as a follow-up.

8. **AEC sink rate mismatch with TTS.**
   *Risk:* TTS outputs 22.05/24 kHz; AEC sink prefers 48 kHz.
   *Mitigation:* `use_master_format=true` on the module argument lets
   PipeWire pick a sensible format; `paplay` already resamples to
   match the device. The existing rubato chain in
   `crates/fono-audio/src/playback.rs` is the safety net.

## Binary size

Effectively zero growth. No new linked dependency. Estimate:
**< 30 KB** of `.text` for the new `aec_pipewire` module + the
barge-in watch task + the doctor row + tests' string constants
(stripped in release-slim). The CI 22 MiB size-budget gate has
~2 MiB of headroom today (`docs/status.md` 2026-05-20 entry, ~21.24
MiB measured); this fits with a comfortable margin.

## Verification

1. **Unit:**
   `cargo test -p fono-audio aec_pipewire` — Pactl trait mocks cover
   probe success, missing pactl, missing module, ENOENT on unload,
   stale-module sweep.
   `cargo test -p fono` for the barge-in dispatch suppression rules
   (held flag, grace window, post-state cancellation).

2. **Integration smoke (manual, Linux desktop with PipeWire):**
   - `fono doctor` shows "Assistant barge-in : available
     (sink/source named fono_aec_…)".
   - Press F8, ask a long question ("explain the Linux audio stack
     in detail"), let TTS start playing through the speakers, then
     speak ("wait, just the PipeWire part"). Expected: TTS audio
     stops within ~200 ms of your interjection, overlay flips to
     `AssistantRecording`, a new STT turn captures your follow-up,
     assistant replies with the refined answer. Rolling history
     preserved (the assistant knows we were just talking about
     Linux audio).
   - Repeat while wearing headphones — same flow, no feedback.
   - Repeat with the same question but stay silent during TTS —
     assistant should complete the whole reply naturally. No false
     trigger.
   - Repeat with F8 held — barge-in suppression confirmed
     (no auto-trigger; release behaves like today's push-to-talk
     stop).

3. **Negative-path smoke (Linux desktop without `module-echo-cancel`):**
   Uninstall the module package (or rename the .so for the test),
   restart Fono. `fono doctor` shows the `MissingModule` row.
   Assistant turn plays normally; voice during TTS does **not**
   trigger barge-in; manual F8 / Escape still works.

4. **Leak sweep smoke:**
   Launch Fono, send `SIGKILL` while AssistantSpeaking is mid-
   utterance. Confirm `pactl list short modules | grep fono_aec_`
   shows the leaked module. Restart Fono. Confirm doctor reports
   "stale leaked modules: 1 (cleaned)" and the module is gone.

5. **Pre-commit gate:** `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace --tests --lib`.

## Files touched, summary

| File | Change |
|---|---|
| **new** `crates/fono-audio/src/aec_pipewire.rs` | RAII session, probe, sweep, Pactl trait |
| `crates/fono-audio/src/lib.rs` | re-export `aec_pipewire` |
| `crates/fono-audio/src/playback.rs` | thread `device` into pulse path |
| `crates/fono/src/session.rs` | add `spawn_barge_in_watch_task` |
| `crates/fono/src/assistant.rs` | own `AecSession` + watch handle across AssistantSpeaking |
| `crates/fono/src/doctor.rs` | "Assistant barge-in" row |
| `crates/fono/src/main.rs` | SIGTERM best-effort unload |
| `CHANGELOG.md` | `[Unreleased] Added` entry |
| `ROADMAP.md` | move "Talk over the assistant" from Up next to Shipped at release tag |

## Out of scope (follow-ups)

- macOS (CoreAudio Voice Processing IO) and Windows (Voice Capture
  DSP) AEC paths. Each needs a separate plan; the in-process
  `webrtc-audio-processing` LGPL alternative remains an option if
  the per-OS APIs prove unwieldy.
- Smooth playback duck during *suspected* barge-in (drop TTS volume
  to 30 % when the gate is in its 100 ms confirm window, full cancel
  only after confirm). Pure UX polish; doesn't change correctness.
- Pre-warm the AEC session at AssistantThinking entry to amortise
  load latency. Trigger on observed user complaints; defer until
  Phase 4 has dogfooded.
- **Wake-word reuse (ADR 0012).** The barge-in capture+detector task
  (`spawn_barge_in_watch_task`, Phase 2) is the seam wake-word will
  share: "read a source → run a detector → fire an action", with an
  energy VAD (barge-in) and a KWS model (wake-word) as interchangeable
  triggers. Only the *wake-while-assistant-is-speaking* sub-case reuses
  the AEC source; idle always-on wake-word reads the default source and
  must not depend on AEC (it is Linux/PipeWire-only, and the AEC sink
  carries only Fono's own TTS — it cannot reject ambient TV/music).
  When this slice lands, prefer a detector abstraction with a pluggable
  trigger over hard-coding the VAD, so the wake-word work doesn't have
  to re-plumb capture. See `docs/decisions/0012-wake-word-activation.md`.

## Done when

- `cargo build --workspace --release` produces an artefact whose
  `target/release/fono` size delta vs the pre-change tip is
  ≤ 64 KiB.
- All five verification protocols above pass on at least two
  hosts: one with `module-echo-cancel` available, one without.
- ROADMAP `Up next` row removed and a `Shipped` entry landed at
  the release tag that ships this slice.
