# Fono — Interactive / Live Dictation (R-plan)

Date: 2026-04-27
Status: Proposed
Supersedes: extends the L-plan (streaming LLM/inject was end-of-pipeline only); this
plan changes the *shape* of the pipeline from batch to streaming end-to-end.

## Objective

Land a live-dictation mode where users see their words appear within ~300 ms of
speaking, with later context correcting earlier hypotheses (LocalAgreement-2),
working uniformly across local and cloud STT backends. Mode is opt-in for v0.1
and becomes default in v0.2 once polished.

## Architectural decisions (locked before tasks)

1. **Hybrid display by default.** Overlay HUD shows live text during recording;
   final text is injected into the focused app on commit. Live-inject (Mode B)
   is opt-in and gated behind hardware/backend allow-lists.
2. **Unified streaming primitive.** Every backend implements `StreamingStt`
   producing `Stream<TranscriptUpdate { stable, unstable, seq, t_audio }>`.
   `unstable` is replaced wholesale by each new update; `stable` is monotonic.
3. **LocalAgreement-2** is the single mechanism for promoting unstable → stable
   across all backends, whether the underlying API is true-streaming or
   pseudo-streaming via repeated batch calls.
4. **LLM cleanup runs once at finalize**, not streamed mid-utterance — full
   context produces stable grammar fixes; partial-context cleanup oscillates.
5. **Overlay lives in a separate process** addressed via the existing
   `fono-ipc` socket — keeps GPU/window code out of the daemon, matches the
   original Phase 7 intent for `fono-overlay`.

## Implementation Plan

### R1 — Foundations (trait + types)

- [ ] R1.1. Define `TranscriptUpdate { stable, unstable, seq, t_audio }` in
  `crates/fono-stt/src/lib.rs`. Document monotonicity contract.
- [ ] R1.2. Add `StreamingStt` trait alongside existing `Stt`. Provide a
  blanket impl over `Stt` that does chunked pseudo-streaming so every
  backend has a baseline live mode without bespoke code.
- [ ] R1.3. Introduce a crate-private `LocalAgreement` helper that, given
  N consecutive overlapping hypotheses, returns the longest common
  token-prefix to promote to stable. N=2 default; N configurable for
  high-noise environments.

### R2 — Audio path

- [ ] R2.1. Replace `fono-audio`'s "drain-on-stop" buffer with a frame-level
  `tokio::sync::broadcast` (or per-consumer `mpsc`) of resampled 16 kHz PCM.
  Existing batch `record()` becomes a thin "collect-until-end" consumer.
- [ ] R2.2. Expose `AudioFrameStream = impl Stream<Item = Vec<f32>>` so
  STT backends consume frames directly, not files.
- [ ] R2.3. VAD emits `SegmentBoundary` markers on ≥ 350 ms silence — STT
  backends use them to aggressively commit unstable → stable at natural
  utterance breaks.

### R3 — Local STT streaming (`WhisperLocal`)

- [ ] R3.1. Sliding window in `crates/fono-stt/src/whisper_local.rs`: 1.2 s
  windows, 0.3 s overlap, on the existing `spawn_blocking` pool. Rolling
  audio ring sized to ~6 s.
- [ ] R3.2. Apply LocalAgreement-2 at token level between consecutive windows.
- [ ] R3.3. On `SegmentBoundary`, re-decode the segment with up to 5 s of
  context for higher-quality final promotion (dual-pass: live preview +
  high-quality final).
- [ ] R3.4. Hardware-tier gate: `Minimum` → 2.0 s windows + lag warning;
  `Unsuitable` → refuse and recommend cloud streaming providers.

### R4 — Cloud STT streaming

- [ ] R4.1. **OpenAI realtime** — new `openai_realtime.rs` using
  `tokio-tungstenite` against `gpt-4o-transcribe`. Map server events
  (`transcript.delta` / `transcript.completed`) to `TranscriptUpdate`.
  Selected when `cfg.stt.backend = "openai"` AND `cfg.stt.realtime = true`.
- [ ] R4.2. **Groq pseudo-stream** — every 700 ms re-POST the trailing N
  seconds of audio; run results through `LocalAgreement`. Cap in-flight
  requests at 1 (drop overlap, never queue).
- [ ] R4.3. Optional providers behind cargo features (no v0.1 commitment):
  `cloud-deepgram`, `cloud-assemblyai` — both native WebSocket streaming.
- [ ] R4.4. Backend registry (`crates/fono-core/src/providers.rs`) gains a
  `streams_natively: bool` field surfaced in `fono doctor` + wizard.

### R5 — Display layer (HUD overlay)

- [ ] R5.1. Promote `fono-overlay` from stub to a borderless always-on-top
  transparent window (egui/winit or iced/tao+softbuffer). Single text line
  pinned bottom-center; `stable` opaque, `unstable` 60% opacity italic.
- [ ] R5.2. Wayland: `wlr-layer-shell` via `smithay-client-toolkit` where
  available; otherwise borderless top-window fallback. X11:
  `_NET_WM_STATE_ABOVE` + `override-redirect`.
- [ ] R5.3. IPC channel: orchestrator sends `OverlayMsg::Update`,
  `OverlayMsg::Commit`, `OverlayMsg::Hide` over `fono-ipc`; overlay process
  subscribes. Reuses existing socket, no new transport.
- [ ] R5.4. Per-frame coalescing — redraw at most every 33 ms (30 fps);
  back-pressure when updates outpace draws.

### R6 — Inject layer for live-inject (opt-in Mode B)

- [ ] R6.1. `InjectStream` capability: `commit(text)` for stable additions,
  `revise(unstable_now, unstable_prev)` that backspaces the diff and types
  the new tail.
- [ ] R6.2. Word-boundary commits only — never inject mid-word unstable text.
- [ ] R6.3. Backend allow-list — only `xtest-paste`, `wtype`, or `xdotool`
  with measured < 5 ms/key in `fono test-inject` benchmark; refuse on
  `Clipboard` / `NoBackend` outcomes.
- [ ] R6.4. Hard cap on backspace count per revision (12 chars). Overflow
  freezes the unstable region until the next `SegmentBoundary`.

### R7 — Orchestrator + FSM

- [ ] R7.1. New FSM state `LiveDictating` in `crates/fono-hotkey/src/fsm.rs`,
  parameterized variant of `Recording` (same transitions, different
  downstream consumers).
- [ ] R7.2. End-of-utterance handling: on hotkey release, drain pending STT
  updates with a 500 ms grace window, run LLM cleanup once over full final
  text, then inject (Mode A) or finalize live-injected text (Mode B).
- [ ] R7.3. Cancel hotkey (Escape): drop STT stream, hide overlay, no inject.
  In Mode B, retract live-injected text via one backspace burst.
- [ ] R7.4. History persistence: only final committed text lands in SQLite.
  Intermediate hypotheses are discarded.

### R8 — LLM cleanup interaction

- [ ] R8.1. During `LiveDictating`, overlay shows raw STT only. On commit,
  briefly show "polishing…" then update the overlay one last time with
  cleaned text before injection.
- [ ] R8.2. Config knob `[interactive].cleanup_on_finalize` (default `true`);
  `false` injects raw STT — useful for code dictation.

### R9 — Config + wizard + CLI

- [ ] R9.1. New `[interactive]` config block: `enabled` (default `false` v0.1,
  `true` v0.2), `mode: overlay | live-inject | hybrid` (default `hybrid`),
  `overlay_position`, `cleanup_on_finalize`, `chunk_ms`,
  `max_session_seconds`.
- [ ] R9.2. Wizard adds a "Live dictation" step explaining overlay vs
  live-inject; probes Wayland/X11 capability; falls back to overlay-only
  on KDE Wayland.
- [ ] R9.3. CLI: `fono record --live` / `--no-live` overrides;
  `fono test-overlay` smoke command analogous to `fono test-inject`.
- [ ] R9.4. Tray menu: "Live dictation: On/Off" toggle entry mirroring the
  existing STT/LLM submenu pattern.

### R10 — Observability + tests

- [ ] R10.1. Tracing spans: `live.first_partial`, `live.first_stable`,
  `live.commit_lag`, `live.revision_count`. `fono doctor` reports last
  session p50/p95 first-partial latency.
- [ ] R10.2. Integration test with a scripted fake `StreamingStt` impl;
  assert overlay receives the right sequence and committed history matches
  expected final string.
- [ ] R10.3. `fono-bench`: `fono bench live --fixture <wav>` plays a fixture
  through the streaming pipeline and reports first-partial / first-stable
  / total-lag percentiles per backend.

### R11 — Docs + ADR

- [ ] R11.1. ADR `docs/decisions/0009-interactive-live-dictation.md`: hybrid
  default, LocalAgreement-2, cleanup-on-finalize, overlay-via-IPC.
- [ ] R11.2. `docs/interactive.md` user guide: mode comparison table,
  per-environment notes, `chunk_ms` tuning, overlay troubleshooting.
- [ ] R11.3. README "Live dictation" section + asciinema demo.

## Sequencing (deliverable slices)

- **Slice 1 — Local-only overlay live dictation:** R1 + R2 + R3 + R5 +
  partial R7/R9/R10/R11. Behind `[interactive].enabled = false`.
- **Slice 2 — Cloud streaming + UX polish:** R4.1 + R4.2 + remainder of
  R7/R9/R10/R11. Flip default to `enabled = true` when overlay feature
  compiled in.
- **Slice 3 — Live-inject + extended providers (post-v0.2):** R6 + R4.3.

## Verification Criteria

- First partial transcript ≤ 400 ms (p95) on `Recommended` tier with
  `WhisperLocal`.
- First partial ≤ 250 ms (p95) on OpenAI realtime; ≤ 800 ms on Groq
  pseudo-stream.
- Zero "stable rewrite" violations across a 3-minute fixture (stable text
  is monotonic and never mutates post-promotion).
- Overlay survives focus changes, virtual-desktop switches, fullscreen apps
  without flicker.
- Escape cancel leaves focused app pristine in Mode A; restores pre-
  recording state in Mode B.
- All existing 79 tests green; new streaming/overlay tests added; clippy
  pedantic + nursery clean.

## Potential Risks and Mitigations

1. **Whisper-local streaming WER regression vs batch.**
   Mitigation: dual-pass — live preview at 1.2 s windows; on
   `SegmentBoundary` re-decode the segment with 5 s context for the final
   stable promotion.
2. **Wayland overlay portability gaps (KWin < 5.27, Mutter without ext).**
   Mitigation: feature-detect `wlr-layer-shell`; fall back to borderless
   always-on-top window with documented focus-steal caveat.
3. **Live-inject backspace storms break terminals/IMEs.**
   Mitigation: opt-in only, off by default, hard 12-char cap, allow-listed
   to fast inject backends, falls back to overlay on first failure.
4. **Cloud cost spike from continuous streaming.**
   Mitigation: document pricing in `docs/providers.md`; tray shows session
   cost estimate; per-session hard cap (`max_session_seconds`, default 120).
5. **Two LLM passes (live raw + final clean) feels slow.**
   Mitigation: `cleanup_on_finalize = false` mode; wizard warns when local
   LLM is paired with live dictation on `Minimum` tier.
6. **Hotkey FSM regression from a third top-level state.**
   Mitigation: implement `LiveDictating` as a parameterized `Recording`
   variant; existing FSM tests cover both branches.

## Alternative Approaches

1. **Overlay-only, never live-inject** — simpler/safer/faster to ship; less
   "magical" than competitors. Reasonable if conservative UX is preferred.
2. **Live-inject-only, skip overlay (R5)** — matches macOS Dictation;
   slashes scope but breaks terminals and Wayland portability. Not
   recommended.
3. **Defer cloud streaming to v0.2** — halves surface area but contradicts
   the "both local and cloud" user requirement.
4. **Bind to `whisper.cpp` `stream` C++ binary or vendor Python
   `whisper-streaming` via PyO3** — fastest demo path, but violates the
   single-static-Rust-binary project rule (AGENTS.md).
