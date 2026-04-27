# Fono — Interactive / Live Dictation + Context + Macros (R-plan v2)

Date: 2026-04-27
Status: Proposed (supersedes v1)
Scope expansion from v1: cost/quality budget engine, app-aware context,
voice command macros.

## Objective

Land a live-dictation experience that feels instant (≤ 300 ms first
feedback) without inflating cloud costs above the current batch baseline
or degrading the *committed* local-inference quality, while also adding:

1. **App-aware context** — focused app's category and identity bias both
   STT (prompt) and LLM cleanup (system prompt), and shape inject behavior.
2. **Voice command macros** — a separate hotkey path turns utterances into
   actions (keystrokes, exec, HTTP, window-manager control), with static
   templates first and LLM tool-calling fallback for free-form intents.

Both layers are opt-in and complementary to dictation.

## Locked architectural decisions

1. **Two-lane streaming** — *preview lane* (cheap, fast, low-quality, never
   committed) + *finalize lane* (high-quality, runs at silence boundaries
   or on hotkey release, becomes the committed text). The committed text
   matches today's batch quality exactly; only the preview is lossy.
2. **Speculative-local default** — when local STT is available, the preview
   lane uses local `whisper-tiny`/`base`. Cloud is used only for finalize.
   Net: cloud cost ≈ today's batch baseline.
3. **VAD-gated streaming** — silent frames are not streamed. Pre-roll
   200 ms preserves onsets.
4. **Hybrid display + opt-in live-inject** — overlay HUD by default; live
   inject Mode B opt-in (preserved from v1).
5. **LLM cleanup runs once at finalize**, takes app-context as input.
6. **Voice commands ride a separate hotkey** (default F7); wake-prefix in
   dictation is opt-in.
7. **Privacy default** — only app *category* leaves the machine; full
   window titles never reach cloud LLMs unless explicitly enabled.

## Implementation Plan

### R1 — Streaming foundations (carryover from v1)

- [ ] R1.1. `TranscriptUpdate { stable, unstable, seq, t_audio, source: Lane }`
  in `crates/fono-stt/src/lib.rs` — `Lane = Preview | Finalize`.
- [ ] R1.2. `StreamingStt` trait + blanket impl over `Stt` for pseudo-stream.
- [ ] R1.3. `LocalAgreement-N` helper, with N configurable per backend
  (default 2; 3 for high-noise environments).

### R2 — Audio path (carryover from v1)

- [ ] R2.1. Frame-level broadcast in `fono-audio`.
- [ ] R2.2. `AudioFrameStream` exposed to backends.
- [ ] R2.3. VAD emits `SegmentBoundary` markers (≥ 350 ms silence).

### R3 — Local STT streaming (carryover + tightening)

- [ ] R3.1. Sliding window in `whisper_local.rs`; window starts at 0.6 s
  and grows to 1.5 s after first stable promotion.
- [ ] R3.2. LocalAgreement-2 token-level promotion.
- [ ] R3.3. On `SegmentBoundary`, run high-quality dual-pass: 5 s context,
  beam-size 5, temperature 0; emit as fully stable.
- [ ] R3.4. Hardware-tier gates (Minimum → 2.0 s windows; Unsuitable →
  refuse + recommend cloud streaming).

### R4 — Cloud STT streaming (carryover + budget integration)

- [ ] R4.1. OpenAI realtime via `tokio-tungstenite`; respects VAD gating.
- [ ] R4.2. Groq pseudo-stream; only fires during voiced regions; in-flight
  cap = 1.
- [ ] R4.3. Optional: `cloud-deepgram`, `cloud-assemblyai` (post-v0.2).
- [ ] R4.4. Provider registry gains `streams_natively` and
  `cost_per_audio_minute_usd`.

### R5 — Display layer (carryover from v1)

- [ ] R5.1. `fono-overlay` becomes a real always-on-top transparent window.
- [ ] R5.2. Wayland: `wlr-layer-shell` where supported, fallback otherwise.
- [ ] R5.3. IPC over existing `fono-ipc` socket.
- [ ] R5.4. Frame coalescing at 30 fps.

### R6 — Live-inject opt-in mode (carryover from v1)

- [ ] R6.1. `InjectStream` capability with `commit` + `revise`.
- [ ] R6.2. Word-boundary commits only.
- [ ] R6.3. Backend allow-list + < 5 ms/key benchmark gate.
- [ ] R6.4. 12-char backspace cap per revision.

### R7 — Orchestrator + FSM (carryover from v1)

- [ ] R7.1. New `LiveDictating` FSM state, parameterized `Recording` variant.
- [ ] R7.2. End-of-utterance: 500 ms drain, finalize-lane completes, LLM
  cleanup, inject.
- [ ] R7.3. Cancel hotkey drops streams, hides overlay, retracts in Mode B.
- [ ] R7.4. Only finalized text persists in SQLite.

### R8 — LLM cleanup, context-aware (extended from v1)

- [ ] R8.1. Cleanup runs once at finalize. Overlay shows raw STT during
  recording; cleaned text replaces it just before inject.
- [ ] R8.2. `cleanup_on_finalize` config knob (default `true`).
- [ ] R8.3. **Context-aware system prompt** — pulls `AppContext` from R13
  and selects a per-category prompt template (terminal / browser / editor
  / chat / email / generic). Rationale: a single hardcoded prompt produces
  smart-quotes in shell commands and stilted prose in chat.
- [ ] R8.4. **Title redaction** — full window title is only included in
  the LLM prompt when LLM is local OR
  `[context].share_titles_with_cloud = true`. Otherwise only the
  category + app identity are sent.

### R9 — Config + wizard + CLI (extended)

- [ ] R9.1. `[interactive]` block: `enabled`, `mode`, `chunk_ms_initial`,
  `chunk_ms_steady`, `cleanup_on_finalize`, `max_session_seconds`,
  `max_session_cost_usd`, `quality_floor`.
- [ ] R9.2. Wizard "Live dictation" step explains the two-lane model and
  cost ceiling; probes Wayland overlay capability.
- [ ] R9.3. CLI: `fono record --live` / `--no-live`; `fono test-overlay`.
- [ ] R9.4. Tray menu: Live dictation On/Off toggle.
- [ ] R9.5. New `[context]` block: `enabled`, `share_titles_with_cloud`,
  `blocklist_patterns` (regex over WM_CLASS or title).
- [ ] R9.6. New `[macros]` block: `enabled`, `command_hotkey`,
  `wake_prefix` (Option<String>), `allow_unsigned_exec`,
  `llm_fallback_enabled`.

### R10 — Observability + tests (extended)

- [ ] R10.1. Tracing spans: `live.first_partial`, `live.first_stable`,
  `live.finalize_latency`, `live.preview_skipped_count`,
  `live.finalize_skipped_due_to_confidence`, `cost.session_estimate_usd`.
- [ ] R10.2. Integration test with scripted fake `StreamingStt` →
  overlay receives correct sequence; committed history matches expected.
- [ ] R10.3. `fono bench live --fixture <wav>` reports first-partial /
  first-stable / total-lag / cloud-call-count percentiles per backend.
- [ ] R10.4. Cost-regression test: replay a 60 s dictation fixture across
  all backends; assert cloud calls + bytes uploaded stay within 110% of
  today's batch baseline.

### R11 — Docs + ADRs (extended)

- [ ] R11.1. ADR `0009-interactive-live-dictation.md` — two-lane model,
  speculative-local rationale, VAD gating, cost guardrails.
- [ ] R11.2. ADR `0010-app-context-and-privacy.md` — category-only default,
  redaction policy, per-platform feasibility table.
- [ ] R11.3. ADR `0011-voice-commands.md` — hotkey-first activation,
  template-then-LLM intent layering, exec safety model.
- [ ] R11.4. `docs/interactive.md`, `docs/context.md`, `docs/macros.md` —
  user guides with mode comparison, per-environment notes, tuning.
- [ ] R11.5. README sections for live dictation + macros + asciinema demos.

### R12 — Cost / quality budget engine (NEW)

- [ ] R12.1. `BudgetController` in `crates/fono-core/src/budget.rs`. Owns:
  per-session frame counter, voiced-frame counter, estimated cost, hard
  caps from config. Gates STT calls and emits stop events when caps hit.
- [ ] R12.2. **VAD-gated streaming** — `fono-audio` exposes
  `voiced_frame_stream()` returning only frames inside voice regions
  (with 200 ms pre-roll + 150 ms post-roll); preview & finalize lanes
  consume from this, not raw frames.
- [ ] R12.3. **Speculative-local preview** — when local STT is available,
  preview lane is hard-wired to `whisper-tiny` (or `base` on Recommended+
  tier), independent of the user's configured backend. The configured
  backend serves only the finalize lane.
- [ ] R12.4. **Adaptive chunking** — `chunk_ms_initial = 600` for the first
  preview emission; grows to `chunk_ms_steady = 1500` after first stable
  promotion; resets at every `SegmentBoundary`.
- [ ] R12.5. **Confidence-aware finalize skip** — when preview-lane mean
  token logprob over the segment exceeds `-0.4` (configurable) AND
  `quality_floor != "max"`, skip the finalize call and promote preview to
  committed. Saves 30–50% of finalize calls on clean audio.
- [ ] R12.6. **Persistent-connection reuse** — keep WebSockets warm across
  consecutive dictations within a 90 s idle window.
- [ ] R12.7. **Per-provider price table** in `providers.rs`
  (cost-per-audio-minute); session estimator runs in tray and triggers
  hard cap when exceeded.
- [ ] R12.8. **Quality floor knob** — `preview` (allow speculative-only
  commits), `balanced` (default; preview + finalize, with confidence skip),
  `max` (always run finalize regardless of confidence).

### R13 — App-aware context provider (NEW)

- [ ] R13.1. New crate `crates/fono-context/`. Trait `ActiveAppProvider`
  → `AppContext { category, identity, title_redacted_or_full, language_hint }`.
- [ ] R13.2. Backend impls:
  - `x11.rs` via `x11rb` (`_NET_ACTIVE_WINDOW` + `WM_CLASS` + `_NET_WM_PID`
    + `/proc/<pid>/{cmdline,cwd}`).
  - `wayland_wlroots.rs` via `wayland-client` foreign-toplevel-management.
  - `wayland_kwin.rs` via DBus `org.kde.KWin`.
  - `fallback.rs` returning `category = Generic` for unsupported (Mutter
    without extension, headless).
- [ ] R13.3. **Category map** at `crates/fono-context/src/categories.toml` —
  WM_CLASS pattern → `Category` enum (`Terminal`, `Browser`, `Editor`,
  `Chat`, `Email`, `Office`, `Generic`). Curated list of ~80 common apps;
  user-overridable via `[context].overrides` in config.
- [ ] R13.4. **Privacy redaction** — title is redacted to category+identity
  when category is sensitive (`Banking`, `Password`) per a built-in
  blocklist, OR matches `[context].blocklist_patterns`. Sensitive apps
  also disable LLM cleanup (raw text injected).
- [ ] R13.5. **STT prompt biasing** — per-category vocabulary (terminal:
  shell built-ins; editor: language keywords if extension known; browser:
  domain keywords from title), passed as `initial_prompt` to whisper /
  `prompt` to cloud STT.
- [ ] R13.6. **LLM system-prompt selection** — per-category template in
  `crates/fono-llm/src/prompts/`; consumed by R8.3.
- [ ] R13.7. **Inject behavior overrides** — per-category: terminal
  suppresses trailing newline auto-add; markdown editor expands explicit
  "new paragraph" to `\n\n`; password fields refuse injection entirely.
- [ ] R13.8. CLI `fono context probe` prints the current detected app
  context for debugging.

### R14 — Voice command system (NEW)

- [ ] R14.1. New crate `crates/fono-macros/`. `MacroEngine` owns: parsed
  user macros, built-in macro library, intent matcher, action runner.
- [ ] R14.2. **Activation paths**:
  - Dedicated hotkey (`[macros].command_hotkey`, default `F7` hold).
  - Optional wake-prefix in dictation stream (`[macros].wake_prefix`,
    e.g., `"computer"`); off by default.
- [ ] R14.3. **Intent parser, three layers**:
  - Layer 1: static phrase templates (regex-like with `<slot>`
    placeholders). Match in O(1) per macro via a flat phrase index.
  - Layer 2: fuzzy alias match (Levenshtein + stem) for forgiving phrasing.
  - Layer 3: LLM tool-calling fallback when layers 1–2 miss
    (`[macros].llm_fallback_enabled`, default `false` until proven; uses
    the configured LLM with strict JSON output and a tool schema generated
    from registered macros).
- [ ] R14.4. **Action types**: `keystroke`, `type` (literal text or
  template), `exec` (allow-list), `http`, `wm` (auto-detect i3/sway/KWin/
  Hyprland), `chain` (sequence). Each action is pure-Rust where possible;
  `exec` uses `tokio::process::Command` with explicit allow-list.
- [ ] R14.5. **App-aware action selection** — a macro can declare per-
  category action variants; engine picks the matching variant from
  `AppContext` (R13).
- [ ] R14.6. **Built-in macro library** at
  `crates/fono-macros/src/builtin.toml` — paste, copy, undo, redo, save,
  find, new tab, close tab, next tab, prev tab, switch app, screenshot,
  lock screen, mute audio, volume up/down. User macros at
  `~/.config/fono/macros.toml` override by name.
- [ ] R14.7. **Safety model**:
  - `exec` actions outside the built-in allow-list (`xdg-open`,
    `flatpak run`, WM CLIs) require `[macros].allow_unsigned_exec = true`.
  - `[macros].sandbox_exec` (default `true`) wraps user-defined exec via
    `bwrap`/`firejail` if available.
  - Audit log at `~/.local/share/fono/macros.log`.
- [ ] R14.8. **Feedback** — tray notification on every executed macro;
  overlay flashes "✓ <macro name>" briefly; failures show a toast with the
  reason.
- [ ] R14.9. **Undo** — global `[macros].undo_hotkey` (default Ctrl+Alt+Z)
  replays an inverse action where defined; otherwise re-types the
  previously-replaced clipboard / inject buffer.
- [ ] R14.10. **CLI**: `fono macros list`, `fono macros test "<phrase>"`,
  `fono macros validate` (lints `macros.toml` for ambiguous phrases,
  unknown actions, allow-list violations).
- [ ] R14.11. **Wake-word detection** (post-v0.2, optional) — investigate
  `openWakeWord` (Apache-2.0) or similar local KWS for the wake-prefix
  path; avoids continuous-transcription cost when wake-prefix is enabled.

## Sequencing (deliverable slices)

1. **Slice A — Streaming + budget engine + overlay (local-first):**
   R1, R2, R3, R5, R7, R10 (partial), R12. Overlay-only display, local
   `whisper-tiny` preview + configured backend finalize, VAD-gated, cost
   caps wired. Behind `[interactive].enabled = false` initially. Ship as
   v0.2.0-alpha.
2. **Slice B — Cloud streaming + app context (privacy-aware):**
   R4.1, R4.2, R4.4, R8.3, R8.4, R9.5, R10.4, R11, R13. OpenAI realtime
   + Groq pseudo-stream wired through the budget engine; LLM cleanup
   becomes context-aware; redaction enforced. Flip `enabled = true`
   default. Ship as v0.2.0.
3. **Slice C — Voice command macros:**
   R9.6, R14, R11.3 (ADR + docs). Static templates + built-in library
   first; LLM fallback wired but off by default. Ship as v0.3.0.
4. **Slice D — Polish (post-v0.3):**
   R6 (live-inject Mode B), R4.3 (Deepgram/AssemblyAI), R14.11 (wake-word),
   richer app context (URL via WebExtension, editor file via Neovim/VS Code
   plugins).

## Verification Criteria

- **Latency:** first preview text ≤ 400 ms p95 on Recommended tier with
  local; ≤ 250 ms p95 on OpenAI realtime; ≤ 800 ms p95 on Groq pseudo.
- **Cost:** for a 60 s dictation fixture, total cloud audio-seconds
  uploaded stay within 110% of today's batch baseline across all cloud
  backends (verified by R10.4 test).
- **Local quality:** committed text WER on the LibriSpeech test-clean
  reference is within 1% absolute of today's batch-only WER (i.e., live
  mode does not degrade the *committed* output).
- **Stable monotonicity:** zero post-promotion rewrites of stable text in
  3-minute fixture runs.
- **Privacy:** title redaction unit-tested; no full title appears in any
  cloud HTTP body when `share_titles_with_cloud = false`.
- **Macros:** built-in library executes correctly across i3, sway, KWin
  Wayland, GNOME-X11; ambiguous phrase detection lints clean on the
  built-in set; LLM fallback produces valid JSON on a 50-utterance
  benchmark suite.
- **Tests:** existing 79 green; ≥ 30 new tests covering streaming,
  context, and macros; clippy pedantic + nursery clean; new ADRs merged.

## Potential Risks and Mitigations

1. **Speculative-local quality is too poor on `Minimum` tier.**
   Mitigation: on `Minimum`, disable preview lane; fall back to a single
   "transcribing…" overlay placeholder + finalize-only flow. User still
   sees feedback (the placeholder), no live text.
2. **Whisper streaming WER regression on noisy audio.**
   Mitigation: dual-pass (R3.3); confidence-aware finalize (R12.5) with
   `quality_floor = "max"` for noise-sensitive users.
3. **Wayland overlay portability gaps.**
   Mitigation: `wlr-layer-shell` + borderless fallback (carryover from v1).
4. **Live-inject backspace storms.**
   Mitigation: opt-in only, hard caps, allow-listed backends (carryover).
5. **Cloud cost spike from misconfigured streaming.**
   Mitigation: VAD gate + adaptive chunking + confidence-skip + hard
   `max_session_cost_usd` cap + tray live cost meter.
6. **Window-title leakage to cloud LLMs.**
   Mitigation: redaction default-on, blocklist patterns, sensitive-app
   blocklist, ADR + docs spelling out the policy.
7. **GNOME-Wayland blindness for app context.**
   Mitigation: graceful degradation to `Generic` category; warn at startup;
   docs point users at the GNOME extension that exposes window info.
8. **Voice command false positives.**
   Mitigation: dedicated hotkey is the default; wake-prefix off by
   default; LLM fallback off by default; per-macro confidence threshold;
   undo hotkey for one-key recovery.
9. **Voice command exec safety.**
   Mitigation: built-in allow-list; sandbox via bwrap/firejail when
   available; audit log; explicit opt-in for unsigned exec.
10. **Crate count growth (`fono-context`, `fono-macros`).**
    Mitigation: small focused crates with narrow public surfaces; reuse
    existing `fono-inject` for keystroke and `fono-ipc` for tray
    notifications; deny.toml audited per project rules.
11. **LLM tool-calling reliability.**
    Mitigation: layer 1+2 cover 80% of usage; LLM fallback off by default
    until measured; strict JSON schema with retry-on-malformed.
12. **Two-lane streaming complicates the FSM.**
    Mitigation: lanes are pure data flows below the FSM; FSM only sees
    aggregated `TranscriptUpdate`s. FSM tests unchanged.

## Alternative Approaches

1. **Single-lane cloud streaming** (no speculative-local).
   Simpler, but cloud cost goes up 5–10× for users who pick cloud STT.
   Rejected on cost.
2. **Single-lane local streaming, no cloud.**
   Cheapest, but contradicts the explicit "both local and cloud"
   requirement. Rejected.
3. **No app context, only LLM cleanup with a generic prompt.**
   Cuts R13 entirely. Loses 60% of the cleanup-quality win, especially in
   terminals where the model would otherwise insert smart quotes into
   shell commands. Rejected.
4. **Voice commands via wake-word only, no hotkey.**
   More natural but adds always-on KWS dependency, more false positives,
   and continuous mic capture (battery + privacy). Rejected as default;
   kept as opt-in path (R14.11).
5. **Voice commands via LLM tool-calling only, no static templates.**
   Maximum flexibility but slow and costly per command. Rejected as
   default; kept as fallback (R14.3 layer 3).
6. **Sidecar processes (Python `whisper-streaming`, Talon-style server).**
   Faster to demo but violates the single-static-Rust-binary project rule.
   Rejected.
