# Fono — Project Status

Last updated: 2026-04-28

## 2026-04-28 — Wave 3 (Slice B1) — Threads A + B shipped; Thread C deferred

Two DCO-signed commits delivered the user-visible half of Slice B1
(driven by `plans/2026-04-28-wave-3-slice-b1-v1.md`); Thread C
(equivalence harness cloud rows) is deferred to a follow-up.

| Thread | SHA | Subject |
|---|---|---|
| A | `1e5682f` | `feat(fono-audio): cpal-callback push for live capture (Thread A / R10.x)` |
| B | `eaf46a3` | `feat(fono-stt): Groq streaming pseudo-stream backend (R4.2)` |
| C | _deferred_ | cloud-mock equivalence rows + recorded-HTTP Groq fixtures (R18.12) |

**Thread A** replaces the 30 ms-poll `RecordingBuffer` drain at
the live-dictation hot path with a true cpal-callback push pipeline:
each cpal data callback resamples to mono f32 and `try_send`s its
slice into a bounded(64) crossbeam SPSC; a dedicated `fono-live-bridge`
std::thread forwards into a tokio mpsc; the drain task pulls
straight into the streaming `Pump`. No 30 ms tick, no
`Mutex<RecordingBuffer>` middleman for live sessions. The batch
path (`run_oneshot`) still uses `RecordingBuffer` unchanged. New
unit test `forwarder_receives_every_callback_in_order` drives a
synthetic cpal stand-in 100x without a real device. Phase A4
manual latency measurement
(`live.first_partial < 400 ms` on the reference machine) cannot be
produced from a headless agent and is left for the operator to
record post-merge.

**Thread B** adds an opt-in Groq streaming STT backend implemented
as a "pseudo-stream": every 700 ms the streaming task re-POSTs the
trailing 28 s of buffered audio to Groq's existing batch endpoint,
pipes each decode through `LocalAgreement` to extract a stable
token-prefix preview, and emits a single finalize decode on
`SegmentBoundary` / `Eof`. In-flight cap = 1 (drop on overlap;
counted in `preview_skipped_count`). New ADR
`docs/decisions/0020-groq-pseudo-stream.md` captures the design
trade-offs (no Groq WebSocket today, 700 ms cadence trade-off,
~25-40× cost overhead vs single batch POST). Selectable via
`fono use stt groq` + `[interactive].enabled = true` +
`[stt.cloud].streaming = true`; the wizard prompts for the third
knob when the first two are set. `docs/providers.md` updated. The
backend takes a `GroqRequestFn` closure for production HTTPS, tests,
and the future cloud-mock equivalence path — keeping the Thread C
hook free.

**Thread C** is deferred. Scope:
1. New `--stt cloud-mock --provider groq` mode in
   `fono-bench equivalence` that swaps the real Groq client for a
   recorded-HTTP closure injected via
   `GroqStreaming::with_request_fn`.
2. Recording format (one JSON file per fixture per provider with
   `(request_audio_sha256, response_body)` exchange list) and at
   least one committed recording.
3. Second per-PR CI gate that runs the cloud-mock lane against a
   sibling baseline anchor (`docs/bench/baseline-cloud-mock-groq.json`).

Why deferred: Thread C is test infrastructure that doesn't block
users. The plumbing alone (mock client + recording format + JSON
fixture + manifest threshold extension + CI workflow change) is a
focused session in its own right; landing it half-done would leave
the equivalence report shape inconsistent. The `GroqRequestFn`
closure injection in Thread B's `groq_streaming.rs` already
preserves the hook Thread C will use, so deferring costs nothing
architecturally. Tracked as the next-session focus.

### Verification gate

`tests/check.sh` (full matrix incl. slim cloud-only build):
- `cargo fmt --check` — clean
- `cargo build` (default + default+interactive + slim + slim+interactive) — clean
- `cargo clippy` (same matrix) — clean
- `cargo test` (same matrix) — green (incl. new
  `forwarder_receives_every_callback_in_order` and
  `groq_streaming::tests::*`)

### Recommended next session

**Wave 3 Thread C** — drop in the cloud-mock equivalence lane.
Plan: `plans/2026-04-28-wave-3-slice-b1-v1.md` Thread C (Tasks
C1-C9). The closure-injection hook is already in
`crates/fono-stt/src/groq_streaming.rs::GroqStreaming::with_request_fn`;
the manifest threshold types are already typed (Wave 2). The work
is scoped to:
1. `crates/fono-bench/src/cloud_mock.rs` — recording loader +
   `SpeechToText` / `StreamingStt` impls keyed by request-WAV SHA.
2. `tests/fixtures/cloud-recordings/groq/<fixture>.json` recording
   fixture format + 1-2 committed recordings (real-key capture
   preferred; placeholder via local-Whisper output is the
   documented fallback).
3. `--stt cloud-mock --provider groq` flag wiring at
   `crates/fono-bench/src/bin/fono-bench.rs:288-333` and
   `:659-684`.
4. Sibling baseline `docs/bench/baseline-cloud-mock-groq.json` and
   second CI job in `.github/workflows/ci.yml`.

Once Thread C lands, the `v0.3.0` release tag becomes appropriate
(Slice B1 fully delivered; CHANGELOG entry + `release.yml`
auto-extracts CHANGELOG sections per `4577dd7`).

## 2026-04-28 — Wave 2: half-shipped plans closed out + real-fixture CI gate

Three DCO-signed commits delivered the trust-restoration leg of the
revised strategic plan (driven by
`plans/2026-04-28-wave-2-close-out-v1.md`).

| Thread | SHA | Subject |
|---|---|---|
| A | `76b9b08` | `feat(fono-bench): typed ModelCapabilities + split equivalence/accuracy thresholds` |
| B | `87221a2` | `feat(fono-update): per-asset sha256 sidecar verification + --bin-dir` |
| C | _this commit_ | `ci(fono-bench): real-fixture equivalence gate with tiny.en + baseline JSON anchor` |

**Thread A** lifted the inline `english_only` boolean
(`crates/fono-bench/src/bin/fono-bench.rs:339` pre-wave) into a typed
`ModelCapabilities` value at `crates/fono-bench/src/capabilities.rs`
with `for_local_whisper` / `for_cloud` resolvers, split the conflated
single threshold into `equivalence_threshold` and `accuracy_threshold`
on `ManifestFixture`, and added a typed `SkipReason` (`Capability` /
`Quick` / `NoStreaming` / `RuntimeError`) so `overall_verdict` no
longer needs to substring-match notes. New mock-STT capability-skip
integration test asserts `transcribe` is never invoked.

**Thread B** closed the supply-chain gap in `apply_update`: per-asset
`.sha256` sidecars are now fetched and verified during
`fetch_latest` / `apply_update`, with a `parse_sha256_sidecar` helper
covering bare-digest, text-mode, binary-mode, and multi-entry
sidecars. `--bin-dir <path>` is exposed on `fono update` for
non-default install layouts. Release workflow emits a `<asset>.sha256`
file per artefact alongside the aggregate `SHA256SUMS`.
`docs/dev/update-qa.md` carries the ten-scenario manual verification
checklist (bare-binary, `/usr/local/bin`, distro-packaged, offline,
rate-limited, mismatched sidecar, prerelease, `--bin-dir`, rollback).

**Thread C** replaced the compile-only `cargo bench --no-run` step at
`.github/workflows/ci.yml:64-68` with a real-fixture equivalence gate:
the workflow fetches the whisper `tiny.en` GGML weights (cached via
`actions/cache@v4` keyed on the model SHA, integrity-checked against
`921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f`),
runs `fono-bench equivalence --stt local --model tiny.en --baseline
--no-legend`, and diffs per-fixture verdicts against
`docs/bench/baseline-comfortable-tiny-en.json`. The `--baseline` flag
strips absolute timings (`elapsed_ms`, `ttff_ms`, `duration_s`) from
the JSON so the committed anchor is deterministic across CI runners.
Regeneration procedure + flapping-fixture mitigation documented in
`docs/bench/README.md`. R5.1 and R5.2 in
`docs/plans/2026-04-25-fono-roadmap-v2.md` now ticked as fully shipped.

Bonus: `tests/check.sh` lands as a single command that mirrors the CI
build/clippy/test matrix locally (full / `--quick` / `--slim` /
`--no-test` modes) so contributors can run the same gate before
pushing.

Verification (this session):

| Command | Result |
|---|---|
| `cargo build --workspace --all-targets` | clean |
| `cargo test --workspace --lib --tests` | green (all suites incl. new `parse_sidecar_*` tests) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |

## 2026-04-28 — Doc reconciliation pass

Pure-doc pass driven by `plans/2026-04-28-doc-reconciliation-v1.md`. No
Rust source touched. Highlights:

- **`crates/fono/tests/pipeline.rs` is not broken on `main`.** The earlier
  status entry below (line ~50) calling out an `Injector` signature
  mismatch was stale: the signatures align in the current source
  (`crates/fono/src/session.rs:140-142` vs
  `crates/fono/tests/pipeline.rs:54-58`) and the workspace test gate runs
  green. Verified this session: `cargo build --workspace`,
  `cargo test --workspace --lib --tests`, and `cargo clippy --workspace
  --no-deps -- -D warnings` are all clean.
- **Self-update plan `plans/2026-04-27-fono-self-update-v1.md`** —
  ~85% landed in commit `3e2c742` (2026-04-22) without ever being
  reflected in the plan tree. This pass ticks Tasks 1–11, 13–15
  (partial), 17–19 and adds an explicit Status header + Open
  follow-ups list. Remaining work (Tasks 12, 16, 20–22) carried
  forward as Wave 2 Task 8.
- **Equivalence accuracy gate plan
  `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md`**
  — ~50% landed in commits `b6596c0` and `7db29b5` (2026-04-28) as
  inline behaviour (`english_only = args.stt == "local" &&
  args.model.ends_with(".en")` at
  `crates/fono-bench/src/bin/fono-bench.rs:339`,
  `Metrics.stt_accuracy_levenshtein` at
  `crates/fono-bench/src/equivalence.rs:113-114`), without the typed
  `ModelCapabilities` API the plan describes. This pass ticks Tasks 7,
  8, 12, 17, 18 with annotations and carries the typed-API refactor
  forward as Wave 2 Task 7.
- **R3.1 in-wizard latency probe** shipped in commit `7bea0a9`
  (`crates/fono/src/wizard.rs:72, 720, 725`). The same commit advertised
  a "R5.1 CI bench gate" but only added `cargo bench --no-run`
  compile-sanity at `.github/workflows/ci.yml:64-68`; the real-fixture
  equivalence-harness gate is carried forward as Wave 2 Task 9.
  `docs/plans/2026-04-25-fono-roadmap-v2.md` Tier-1 reconciled to
  reality (R2.1, R3.1, R3.2, R3.3, R4.1, R4.2, R4.3, R4.4 ticked; R5.1
  demoted to partial).
- **Three obsolete plans superseded** by the
  `--allow-multiple-definition` link trick already live in
  `.cargo/config.toml:21-28`:
  `plans/2026-04-27-candle-backend-benchmark-v1.md`,
  `plans/2026-04-27-llama-dynamic-link-sota-v1.md`, and
  `plans/2026-04-27-shared-ggml-static-binary-v1.md` were moved to
  `plans/closed/` with `Status: Superseded` headers. None of the three
  was ever executed; the linker workaround landed first.
- **ADR backfill.** `docs/decisions/` previously listed only
  `0001`–`0004`, `0009`, `0015`, `0016` while plan history and status
  entries referenced `0005`–`0008` and `0010`–`0014`. Reconstructed
  stubs for the missing numbers landed this pass with `Status:
  Reconstructed (original lost in filter-branch rewrite)` headers, plus
  three new ADRs: `0017-auto-translation.md` (forward-reference for the
  pending feature), `0018-ggml-link-trick.md` (active `--allow-multiple-definition`
  decision), and `0019-platform-scope.md` (v0.x Linux-multi-package
  scope).

Verification (this session, `4517133` + doc edits only):

| Command | Result |
|---|---|
| `cargo build --workspace` | clean |
| `cargo test --workspace --lib --tests` | green |
| `cargo clippy --workspace --no-deps -- -D warnings` | clean |

## 2026-04-28 — Language allow-list (constrained Whisper auto-detect)

User reported: *"A lot of the people will use fono in more than one
language. But whisper might autodetect some of the other languages.
We need to be able to specify a list of languages that should be
considered and the others should essentially be banned."*

Plan: `plans/2026-04-28-stt-language-allow-list-v1.md`.

**Schema** — `[general]` and `[stt.local]` gain a new `languages:
Vec<String>` field. Empty = unconstrained Whisper auto-detect (today's
default); one entry = forced single language (today's `language = "ro"`);
two-or-more = constrained auto-detect: Whisper picks from the allow-list,
every other language is **banned**. The legacy scalar `language: String`
is still accepted on read and migrated into `languages` on first save
(`skip_serializing_if = "String::is_empty"` drops it from disk).

**Local Whisper** (`crates/fono-stt/src/whisper_local.rs`) — when an
allow-list is in effect, run `WhisperState::lang_detect` on the prefix
mel, mask probabilities to allow-list members only, argmax → run
`full()` with the picked code locked. Forced and Auto paths preserve
the previous one-pass behaviour (no extra cost).

**Cloud STT** (`groq.rs`, `openai.rs`) — banning is impossible at the
provider API. Two opt-in knobs on `[general]`:
`cloud_force_primary_language` (sends `languages[0]` instead of `auto`)
and `cloud_rerun_on_language_mismatch` (one extra round-trip when the
returned `language` is outside the allow-list). Defaults preserve the
current cost profile.

**New module** `crates/fono-stt/src/lang.rs` carries the
`LanguageSelection` enum (`Auto` / `Forced(code)` / `AllowList(Vec)`)
and the parser, so backends never compare sentinel strings like
`"auto"` directly.

**Wizard** — both `configure_cloud` and `configure_mixed` now persist
their language prompt (previously discarded into `_lang`) into
`general.languages` via `LanguageSelection::parse_csv`.

**Verification** — `cargo build --workspace`, `cargo test --workspace
--lib`, and `cargo clippy -p fono-stt -p fono-core -p fono --lib --bins
-- -D warnings` all green. New tests in `lang.rs` cover the parser /
normaliser; `config.rs::languages_round_trip_drops_legacy_field` and
`explicit_languages_wins_over_legacy_scalar` lock the migration.

The pre-existing `crates/fono/tests/pipeline.rs` `Injector` signature
mismatch is unrelated to this change and was already broken on
`main`.

## 2026-04-28 — Overlay focus-theft eliminated (X11 override-redirect)

User reported: *"The overlay window still seems to be stealing focus
twice; when it appears in live mode and when it does cleanup."*

The previous mitigation (`.with_active(false)` +
`WindowType::Notification`, landed in `1f23194`) is correct in spirit,
but X11 window managers disagree about how aggressively to honour
those hints across multiple map cycles. The overlay is shown → hidden
→ shown again twice per dictation (live state, then
processing/finalize state), and many WMs default to "give focus on
map" on the second-and-subsequent map even for notification toplevels.
Net result was that every overlay state transition re-stole focus
from the user's editor / terminal / browser, and the synthesized
`Shift+Insert` paste then landed in the overlay itself rather than
the original target window.

**Fix landed in `d2823f1`** (`crates/fono-overlay/src/real.rs:488-494`):
add `.with_override_redirect(true)` to the X11 window attributes on
top of the existing `.with_active(false)` and
`WindowType::Notification` hints. Override-redirect windows are
completely outside WM management — the X server never asks the WM
about focus, mapping, or stacking for them. This is what tooltips,
dmenu, and rofi all do; it makes focus theft physically impossible
on X11 regardless of WM behaviour.

**Trade-offs**

- WM-managed always-on-top is lost. Mitigation: borderless
  override-redirect windows naturally stack above normal toplevels
  because the WM never moves them on focus changes; no observable
  regression vs the prior `WindowLevel::AlwaysOnTop` hint.
- Compositor-managed transparency varies slightly across compositors
  for OR windows. picom honours it; KWin and Mutter compose it
  correctly. The solid-charcoal fallback at `COLOR_BG = 0xEE17171B`
  still applies if the compositor refuses the alpha channel.

**Wayland deferred to Slice B.** On Wayland the compositor controls
focus completely; the proper solution is `xdg_activation_v1` /
`wlr-layer-shell` from a dedicated overlay subprocess, which is the
Slice B subprocess-overlay refactor (ADR 0009 §5). For Slice A this
X11-only fix matches the dominant target environment.

**Verification**

| Command | Result |
|---|---|
| `cargo build  -p fono-overlay --features real-window` | clean |
| `cargo clippy -p fono-overlay --features real-window -- -D warnings` | clean |
| `cargo test   -p fono-overlay --lib` | 2/0 |

(Workspace clippy currently reports unrelated in-flight bench errors
from the v7 equivalence-fixtures swap; tracked separately.)

## 2026-04-27 — Slice A v7 delta landed (boundary heuristics)

Plan v7 (`plans/2026-04-27-fono-interactive-v7.md`) extends Slice A with
boundary-quality heuristics. Four DCO-signed commits on top of v6 Slice A:

| SHA       | Title |
|-----------|-------|
| `ce6a21e` | fono-core(config): v7 `[interactive]` keys (boundary heuristics) |
| `d0e21a0` | fono(live): R2.5 prosody/punct chunk-boundary + R7.3a hold-on-filler drain |
| `beae861` | fono-bench(equivalence): pin v7 boundary knobs + A2 row variants |
| `6a6c6c1` | docs: ADR 0015 + interactive.md tuning section |

**What landed**

- R9.1 — `[interactive]` config grew from 4 keys to 18, covering the v6
  carryover (`mode`, `chunk_ms_initial/steady`, `cleanup_on_finalize`,
  `max_session_seconds/cost_usd`) and the v7 heuristic knobs
  (`commit_use_prosody`, `commit_use_punctuation_hint`,
  `commit_hold_on_filler`, `commit_filler_words`,
  `commit_dangling_words`, plus matching `*_ms` extensions). Reserved
  `eou_adaptive` / `resume_grace_ms` defined but inert until Slice D.
- R2.5 — prosody pitch-tail tracker (hand-rolled time-domain
  autocorrelation, no FFT dep) wired into the FrameEvent → StreamFrame
  translator; punctuation-hint pure function shipped, full wiring
  deferred to Slice B (translator can't yet see preview text).
- R7.3a — filler/dangling-word suffix detection; ships as informational
  signal on `LiveTranscript` rather than a true drain extension to
  avoid an >80 LoC pump refactor. Daemon can act on the flags now;
  Slice D's adaptive-EOU work will make the extension first-class.
- R10.5 / R10.6 — tracing fields on `live.first_stable` + 13 new
  heuristic-isolation unit tests + 2 new equivalence-harness tests.
- R18.10 / R18.23 — pinned heuristic knobs in equivalence reports;
  four A2 row variants (`A2-no-heur`, `A2-default`, `A2-prosody`,
  `A2-filler`); `A2-default` gates Tier-1 + Tier-2.
- ADR 0015 — boundary-heuristics architecture, additive-only invariant,
  forward-reference to adaptive EOU in Slice D.

Verification gate (slim + `interactive` feature): build clean, clippy
clean with `-D warnings`, all tests green (no regressions).

## 2026-04-27 — Slice A landed (interactive / live dictation)

Plan v6 (`plans/2026-04-27-fono-interactive-v6.md`) Slice A is in.
Five commits on `main`, each DCO-signed:

| SHA       | Title |
|-----------|-------|
| `7fbf974` | Slice A checkpoint: streaming primitives, overlay, budget, live session |
| `92d4cc3` | Slice A: live pipeline integration tests (plan v6 R10.2) |
| `074a6c7` | Slice A: equivalence harness foundation + 2 fixtures (plan v6 R18) |
| `c3f2b68` | Slice A: ADR 0009 + interactive.md user guide (plan v6 R11) |
| (this)    | Slice A: docs/status.md — Slice A complete, Slice B queued |

The four Forge follow-up commits to `7fbf974` cover deliverables R10.2,
R18 (foundation), R11.1, R11.2, and R17 (status update).

### What Slice A actually ships

- **R1 / R3** — `fono-stt::StreamingStt` trait + `LocalAgreement`
  helper + dual-pass finalize lane on top of `WhisperLocal`. Gated
  behind the `streaming` cargo feature on `fono-stt`.
- **R2** — `fono-audio::AudioFrameStream` + `FrameEvent` enum + VAD-
  driven segment-boundary heuristic. Gated behind `fono-audio/streaming`.
- **R5** — Live overlay (`fono-overlay::OverlayState::LiveDictating`
  + `RealOverlay` winit window) painting preview / finalize text.
  In-process; sub-process refactor deferred to Slice B (see ADR 0009 §5).
- **R7.4 / R10.2** — `fono::live::LiveSession` orchestrator that wires
  `Pump` → `AudioFrameStream` → `StreamingStt` → overlay. Two new
  integration tests (`crates/fono/tests/live_pipeline.rs`) drive it
  with a synthetic `StreamingStt` and assert (a) two-segment
  concatenation under preview→finalize lanes and (b) clean
  cancellation when no voiced frames arrive.
- **R10.4** — `fono record --live` CLI — record-then-replay-through-
  streaming. Realtime cpal-callback push lands in Slice B.
- **R11.1** — ADR `docs/decisions/0009-interactive-live-dictation.md`
  capturing the six locked architectural decisions for Slice A.
- **R11.2** — User-facing guide `docs/interactive.md` covering
  `[interactive].enabled`, the `interactive` cargo feature, the
  `fono record --live` and `fono test-overlay` flows, and the two
  known issues (hostile compositors, Wayland focus theft).
- **R12** — `fono-core::BudgetController` (price table + per-minute
  ceiling + `BudgetVerdict::{Continue, StopStreaming}`) wired into
  `LiveSession::run`. Gated behind `fono-core/budget`.
- **R17.1 / R18 (foundation)** — Streaming↔batch equivalence harness
  in `crates/fono-bench/src/equivalence.rs` + `fono-bench equivalence`
  subcommand + two synthetic-tone WAV fixtures
  (`tests/fixtures/equivalence/{short-clean,medium-pauses}.wav`,
  ~410 KB total). 7 new unit tests cover the levenshtein
  normalization, JSON round-trip, overall-verdict aggregation, and
  manifest parsing. End-to-end smoke (`--stt local --model tiny.en`)
  produced PASS on both fixtures.

### Bug fixed in passing

`LiveSession::run` previously called `pump.subscribe()` *after* the
caller had pushed PCM and called `pump.finish()` — which loses every
frame because `tokio::sync::broadcast` does not deliver pre-subscribe
messages to fresh subscribers. `Pump` now pre-subscribes a primary
receiver at construction and exposes it via
`Pump::take_receiver()`; `LiveSession::run` takes a
`broadcast::Receiver<FrameEvent>` directly, and `fono record --live`
spawns the run task before pushing so the broadcast buffer drains
between pushes. Caught while landing the live integration tests; not
in scope of `7fbf974` itself.

### Build matrix (verified this session)

| Command | Result |
|---|---|
| `cargo build --workspace` | ✅ |
| `cargo build --workspace --features fono/interactive` | ✅ |
| `cargo clippy --workspace --no-deps -- -D warnings` | ✅ |
| `cargo clippy --workspace --no-deps --features fono/interactive -- -D warnings` | ✅ |
| `cargo test --workspace --lib --tests` | ✅ 110 ok, 0 fail (was 103 at HEAD) |
| `cargo test --workspace --lib --tests --features fono/interactive` | ✅ 126 ok, 0 fail |
| `cargo run -p fono-bench --features equivalence,whisper-local -- equivalence --stt local --model tiny.en --output report.json` | ✅ both fixtures PASS |

### Deferred to Slice B (next session candidates)

- **R4 / R8 / R10.4 (realtime)** — Cloud streaming providers (Groq,
  OpenAI realtime, Deepgram, AssemblyAI) and the realtime cpal-
  callback audio push so the overlay paints text *while* you speak.
- **R5.6** — Overlay sub-process refactor for crash isolation.
- **R18 cloud rows** — Cloud-streaming equivalence rows of R18
  (`--stt groq` and friends). Requires the cloud-mock recordings
  pipeline that the v6 plan R18.12 sketches.
- **R18 Tier-2** — With-LLM equivalence comparison (`--llm local
  qwen-0.5b`). The Tier-1 (whisper-only) gate is in; Tier-2 needs
  the deterministic-LLM scaffolding (n_threads=1 + seed-pinning) to
  produce stable outputs.
- **R18.6 fixture set completion** — The remaining 10 fixtures of the
  curated 12-fixture set (long-monologue, noisy-cafe, accented-EN,
  numbers/commands, whispered, with-music, multi-speaker,
  code-dictation, long-with-pauses, short-noisy-quick). Needs real
  CC0 audio sources.
- **R16** — Tray icon-state palette refactor.

### Recommended next session

1. **Slice B kickoff** — wire the realtime cpal-callback push and the
   first cloud streaming provider (Groq's faster-whisper streaming
   endpoint is the obvious first target — same auth flow as the
   existing Groq batch backend).
2. **Or, if Slice B is too big a chunk to start cold:** drop the
   remaining 10 R18 fixtures into `tests/fixtures/equivalence/` from
   real CC0 LibriVox / Common Voice clips, recompute SHA-256s, set
   `synthetic_placeholder = false` in the manifest, and tighten
   `TIER1_LEVENSHTEIN_THRESHOLD` from `0.05` back to the v6 plan's
   strict `0.01` in the same commit. Self-contained, fast feedback.

## Hotkey ergonomics — single-key defaults

Default hotkeys switched from three-key chords to single function keys:

- `toggle = "F9"` (was `Ctrl+Alt+Space`)
- `hold = "F8"` (was `Ctrl+Alt+Grave`)
- `cancel = "Escape"` (unchanged — only grabbed while recording)
- `paste_last` hotkey **removed**. The tray's "Recent transcriptions"
  submenu and the `fono paste-last` CLI cover the same need with a
  better UX (re-paste any of the last 10, not just the newest).

Touched: `crates/fono-core/src/config.rs`, `crates/fono-hotkey/{fsm,listener,parse}.rs`,
`crates/fono-ipc/src/lib.rs` (kept `Request::PasteLast` for CLI), `crates/fono/src/{daemon,wizard}.rs`,
`crates/fono-tray/src/lib.rs`, `README.md`, `docs/troubleshooting.md`, `docs/wayland.md`.

`Request::PasteLast` now routes directly to `orch.on_paste_last()` instead of
through the FSM, since there is no longer a hotkey path for it.

## Single-binary local STT + local LLM (ggml symbol collision resolved)

Default builds now ship **both** local STT (`whisper-rs`) and local LLM
(`llama-cpp-2`) statically linked into one self-contained `fono` binary —
the previous `compile_error!` guard in `crates/fono/src/lib.rs` is gone, and
`crates/fono/Cargo.toml` re-enables `llama-local` in `default`.

The `ggml` duplicate-symbol collision (each sys crate vendors its own static
`ggml`) is resolved at link time via `-Wl,--allow-multiple-definition` in
the new `.cargo/config.toml`. Both crates' `ggml` copies originate from the
same `ggerganov` upstream and are ABI-compatible; the linker keeps one set
of symbols and discards the duplicate. Verified post-link with
`nm target/release/fono | grep ' [Tt] ggml_init$'` → exactly one entry.

A new smoke test `crates/fono/tests/local_backends_coexist.rs` constructs a
`WhisperLocal` and a `LlamaLocal` in the same process to guard against
runtime breakage from any future upgrade of either sys crate.

### Hardware acceleration banner

Every daemon start now logs an `info`-level summary of the actual
accelerator path the binary will use, e.g.:

```
hw accel     : CPU AVX2+FMA+F16C
```

Implemented in `crates/fono/src/daemon.rs::hardware_acceleration_summary`.
GPU backends are wired through opt-in cargo features
(`accel-cuda` / `accel-metal` / `accel-vulkan` / `accel-rocm` /
`accel-coreml` / `accel-openblas`) on `fono`, `fono-stt`, and `fono-llm`;
flipping any of them prepends the matching label (e.g. `CUDA + CPU AVX2`).
The default ship build stays CPU-only — single binary, runs everywhere,
auto-picks the best SIMD kernel ggml has compiled in.

## H8 landed — real local LLM cleanup via `llama-cpp-2`

`crates/fono-llm/src/llama_local.rs` is no longer a stub. The `llama-local`
feature now runs honest GGUF inference: process-wide `LlamaBackend` cached in
a `OnceLock`, lazy model load via `Arc<Mutex<Option<LlamaModel>>>` (mirrors
`WhisperLocal`), greedy sampling, ChatML prompt template that fits both
Qwen2.5 and SmolLM2, `MAX_NEW_TOKENS = 256`, EOS + `<|im_end|>` stop tokens,
and a `tokio::task::spawn_blocking` boundary so the async runtime keeps
moving while llama.cpp grinds. The factory grew an `llm_models_dir` parameter
that resolves `cfg.local.model` (a name) to `<dir>/<name>.gguf` — the
existing scaffold's "model NAME passed as a path" bug is gone.

A cleanup that takes > 5 s emits a `warn!` recommending the user pick a
cloud provider (`fono use llm groq` / `cerebras`) or a smaller model. CPU-only
Q4_K_M inference of a 1.5B-parameter model is on the order of 5–15 tok/s on
a laptop, so this matters: the wizard continues to default-skip the local
LLM for tiers ≤ `Recommended`. Local LLM model auto-download (H9 / H10) is
still open — follow-up.

**Build constraint.** `whisper-rs-sys` and `llama-cpp-sys-2` each statically
link their own copy of ggml; combining both in one binary collides on every
`ggml_*` symbol. We keep the static-binary stance (no sidecar `libllama.so`)
by guarding the combo with a `compile_error!` in `crates/fono/src/lib.rs`.
Default-features build (whisper-local + cloud LLM) works as before. Users
who want local LLM cleanup build cloud-STT instead:

```
cargo build --release --no-default-features --features tray,llama-local,cloud-all
```

Lifting this constraint requires moving llama.cpp to a shared library
(`llama-cpp-sys-2/dynamic-link`), which is **not** the path forward — fono
ships as a single self-contained binary.

## Recent fix — silenced GTK/GDK startup warnings

User reported a `Gdk-CRITICAL: gdk_window_thaw_toplevel_updates: assertion ...
freeze_count > 0 failed` line at startup. This is a benign assertion fired by
libappindicator/GTK3 when the indicator first paints on KDE's StatusNotifier
host; the tray works correctly. The tray thread now installs `glib`
log handlers for the `Gdk`, `Gtk`, `GLib-GObject`, and `libappindicator-gtk3`
domains and demotes their warning/critical messages to `tracing::debug`, so
default startup is clean.

## Recent fix — cancel hotkey only grabbed while recording

User reported Fono was holding a global grab on `Escape`, blocking it in other
apps. The cancel hotkey is now registered with the OS only when entering the
Recording state and unregistered as soon as recording stops or is cancelled.
Implemented via a new `HotkeyControl` channel between the daemon's FSM event
loop and the `fono-hotkey` listener thread, plus an `unregister(...)` call in
the listener using the existing `global-hotkey` API.

## Recent fix — quieter whisper logging

User reported there were still too many startup messages coming from whisper.
The default CLI log filters now keep `whisper-rs` whisper.cpp/GGML `info`
chatter hidden behind explicit module-level `FONO_LOG` overrides while keeping
warnings and errors visible.

## Recent fix — quieter daemon startup logging

User reported too many `info` messages when starting Fono. Startup-only details
such as XDG paths, tray/hotkey internals, model-present checks, warmup timings,
inject backend discovery, and paste-shortcut setup now log at `debug`; default
`info` startup keeps only the concise daemon start/ready lines and warnings.

## Recent fix — setup wizard API key paste feedback

User reported that pasting a cloud LLM API key gave no immediate visual
indication that the paste landed. The wizard now reads API keys with a masked
prompt that prints one `*` per accepted character, then reports the received
character count before validation. The key contents remain hidden.

## Recent fix — setup wizard nested Tokio runtime panic

User reported a setup crash after adding a Groq key:
`Cannot start a runtime from within a runtime` at `crates/fono/src/wizard.rs:627`.
Root cause: the local-STT latency probe built a new Tokio runtime and called
`block_on()` while the setup wizard was already running inside Tokio. The probe
is now async and awaits `stt.transcribe(...)` on the existing wizard runtime.

## Recent fixes — tray menu hardening (env-var leak + stale binary)

User reported: "I can still see backends that aren't configured for STT and
LLM and switching through them doesn't seem to dynamically switch while the
software is running." Two distinct issues; both fixed.

1. **Env-var leak into the tray submenu.** The previous filter used
   `Secrets::resolve()` which falls through to the process environment.
   On a typical dev machine with `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`
   etc. exported in the shell, every one of those backends was wrongly
   marked "configured" and listed in the menu — clicking them then
   produced a 401 on the next dictation. New strict filter:
   `crates/fono-core/src/secrets.rs` exposes `has_in_file()` /
   `resolve_in_file()` and `crates/fono-core/src/providers.rs:178-218`
   (`configured_stt_backends` / `configured_llm_backends`) only consult
   `secrets.toml`. Two regression tests
   (`configured_filter_ignores_env`, `configured_filter_includes_explicit_keys`)
   pin the new contract.
2. **Stale release binary.** The binary at `target/release/fono` was
   older than the daemon's tray-filter source — the user was running
   the pre-fix version and the menu still listed every backend. Rebuilt
   so the live binary matches the source.

## Recent fixes — tray polish + whisper log noise + repo URL

- **Tray menu trimmed.** Removed the broken `Open history folder` entry
  (`xdg-open` on the data directory just opened the parent in Dolphin and
  was useless). The `Recent transcriptions` submenu is the supported way to
  revisit history.
- **Provider submenus restricted to configured backends.** STT/LLM submenus
  now only list backends whose API key is present in `secrets.toml` (plus
  `Local` and `None`). New helpers in `crates/fono-core/src/providers.rs`:
  `configured_stt_backends` / `configured_llm_backends`. Eliminates the
  "click OpenAI in tray, get a 401 on next dictation" trap.
- **Whisper.cpp log noise silenced.** `whisper-rs 0.16` ships a
  `whisper_rs::install_logging_hooks()` redirector that funnels GGML and
  whisper.cpp logs through `tracing`. Enabled via the new `log_backend`
  feature in workspace `Cargo.toml` and a `Once` guard in
  `crates/fono-stt/src/whisper_local.rs`. With the default `info` filter
  the formerly noisy timing dumps stay silent; `FONO_LOG=whisper_rs=debug`
  re-enables them when needed.
- **Repo URL → `bogdanr/fono`.** Replaced every reference in `Cargo.toml`,
  `README.md`, `CHANGELOG.md`, `packaging/**`, and systemd units with
  `github.com/bogdanr/fono`.

## Recent fixes (Tier-1 roadmap pass — wizard + docs polish)

- **Wizard rewrite** (`fono/src/wizard.rs`): now offers four explicit
  paths instead of a binary local/cloud choice — `Local`, `Cloud`,
  `Mixed (Cloud STT + Local LLM)`, `Mixed (Local STT + Cloud LLM)`. Path
  recommendation order is hardware-tier aware (Recommended/High-end →
  local first; Minimum → cloud first; Unsuitable → cloud only).
- **Cloud key validation** (R3.2): every API key entered in the wizard
  is hit against the provider's `/v1/models` endpoint with a 5 s
  timeout before persistence. 401/403 responses re-prompt for the key;
  network errors warn but allow override (offline-first install).
- **`docs/inject.md`** — full reference for the injection stack: priority
  table, paste-shortcut precedence, per-environment recipes (Wayland /
  KDE-Wayland / X11 / terminals / Vim / tmux), and troubleshooting.
- **`docs/troubleshooting.md`** — symptom-first guide covering hotkey,
  pipeline, STT, latency, tray, audio, provider switches, and bug
  reporting checklist.

## Recent fixes (Tier-1 roadmap pass — provider-switching tray + docs)

- **Tray STT/LLM submenus** (`fono-tray/src/lib.rs`, `fono/src/daemon.rs`).
  Right-click the tray icon → `STT: <active> ▸` or `LLM: <active> ▸` shows
  every backend with the active one ticked; click another item to hot-swap.
  Same code path as `fono use stt … / llm …` (atomic config rewrite +
  orchestrator `Reload`); tray notification confirms the switch.
- **README v0.1.0 pass** — added CLI cheatsheet entries for `fono use`,
  `fono keys`, `fono test-inject`, `fono hwprobe`, plus a tray-menu visual
  reference and a Text-Injection section explaining the Shift+Insert default
  + override layers.
- **CHANGELOG v0.1.0 entry** drafted (`CHANGELOG.md`) — pipeline, providers,
  hardware tiers, injection, tray, observability, bench harness, model
  matrix, known limitations.

## Recent fixes (delivery path — clipit/Wayland)

- **Default paste shortcut → Shift+Insert** (`fono-inject/src/xtest_paste.rs`).
  Was Ctrl+V — captured by shells/tmux/vim normal mode/terminal verbatim-
  insert bindings. Shift+Insert is the X11 legacy paste binding hard-coded
  into virtually every toolkit (xterm/urxvt/st PRIMARY, GTK/Qt CLIPBOARD,
  VTE-based PRIMARY, alacritty/kitty CLIPBOARD, Vim/Emacs in insert mode);
  fono populates **both** PRIMARY and CLIPBOARD on every dictation so the
  toolkit's selection choice is invisible. Net effect: text now lands in
  terminals as well as GUI apps.
- **`PasteShortcut` enum** with `ShiftInsert` (default), `CtrlV`,
  `CtrlShiftV`. Generalized XTEST sender: presses modifiers in order,
  presses key, releases in reverse, with `Insert` ↔ `KP_Insert` keysym
  fallback for exotic keymaps.
- **Two override layers** for the rare app that needs a different binding:
  - `[inject].paste_shortcut = "ctrl-v"` in `~/.config/fono/config.toml`
    (validated at startup; typos surface as a warn-level log line).
  - `FONO_PASTE_SHORTCUT=ctrl-v` env var (highest precedence; useful for
    one-shot testing without editing config).
  - `fono test-inject "..." --shortcut ctrl-v` flag for the smoke command.
- **Diagnostic surfaces**:
  - `fono doctor` now prints `Paste keys  : Shift+Insert (config="..."  env=...)`.
  - `fono test-inject` prints the active shortcut at the top.
  - Inject path logs `xtest-paste: synthesizing Shift+Insert (mod_keycodes=...)`
    so users can confirm what was actually sent.
- **Pure-Rust XTEST paste backend** (`fono-inject/src/xtest_paste.rs`,
  `x11-paste` feature, **on by default**). Synthesizes the configured
  shortcut against the focused X11 / XWayland window after writing to the
  clipboard. **No system tools required** — works on any X session even
  without `wtype`/`ydotool`/`xdotool`/`enigo`. Auto-selected by
  `Injector::detect()` on X11 when no other backend is available; verified
  live: `typed via xtest-paste in 15ms`.
- **`FONO_INJECT_BACKEND=xtest|paste|xtestpaste`** override for forcing
  the backend during testing.

- **Multi-target clipboard write** (`fono-inject/src/inject.rs`) — new
  `copy_to_clipboard_all()` writes to **every** detected backend
  (wl-copy + xclip clipboard + xsel + xclip primary) so X11-only managers
  like clipit catch the entry on Wayland sessions, and Wayland-native
  managers like Klipper catch it on hybrid setups.
- **Per-tool stderr capture** — silent failures (no `DISPLAY`, missing
  protocol support, non-zero exit) are now surfaced in logs and in
  `fono test-inject` output instead of being swallowed.
- **`Injector::Xdotool` subprocess backend** — independent of the
  `libxdo` C dep; XWayland fallback for KWin sessions where `wtype` is
  accepted but silently dropped.
- **`FONO_INJECT_BACKEND=…` override** — forces a specific injector for
  testing.
- **`fono test-inject "<text>"`** — bypasses STT/LLM, prints per-tool
  diagnostic + clipboard readback verification.
- **readback_clipboard `.ok()?` short-circuit fix** — verifier no longer
  aborts when the first read tool isn't installed.

## Current milestone

**v0.1.0-rc: provider switching without daemon restart.** Local-models
default + hardware-adaptive wizard (previous slice) plus a one-command
provider-switching UX: `fono use stt groq`, `fono use cloud cerebras`,
`fono use local`, plus `fono keys add/list/remove/check` and per-call
`fono record --stt … --llm …` overrides. All flips hot-reload through a
new `Request::Reload` IPC; the orchestrator hot-swaps STT/LLM behind a
`RwLock<Arc<dyn _>>` and re-prewarms on every reload.

## Active plans

| Plan | Status |
|---|---|
| `docs/plans/2026-04-24-fono-design-v1.md` (Phases 0–10) | ✅ Phases 0–10 landed |
| `docs/plans/2026-04-25-fono-pipeline-wiring-v1.md` (W1–W22) | ✅ 22/22 |
| `docs/plans/2026-04-25-fono-latency-v1.md` (L1–L30) | ✅ 17/30 landed, 13 deferred-to-v0.2 |
| `docs/plans/2026-04-25-fono-local-default-v1.md` (H1–H25) | ✅ 11/25 landed, 14 deferred-to-v0.2 |
| `docs/plans/2026-04-25-fono-provider-switching-v1.md` (S1–S27) | ✅ 16/27 landed, 11 deferred-to-v0.2 |
| `plans/2026-04-27-fono-self-update-v1.md` | ~85% landed in `3e2c742`; finishing pass tracked as Wave 2 Task 8 |
| `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md` | ~50% landed in `b6596c0`/`7db29b5`; typed-API refactor tracked as Wave 2 Task 7 |
| `plans/2026-04-28-fono-auto-translation-v1.md` | Not started (Wave 4 of revised strategic plan) |
| `plans/closed/` (candle / dynamic-link / shared-ggml) | Superseded by `--allow-multiple-definition` link trick (ADR 0018) |

## Phase progress

| Phase | Description                                                        | Status |
|-------|--------------------------------------------------------------------|--------|
| 0     | Repo bootstrap + workspace + CI skeleton                           | ✅ Complete |
| 1     | fono-core: config, secrets, XDG paths, SQLite schema, hwcheck      | ✅ Complete |
| 2     | fono-audio: cpal capture + VAD stub + resampler + silence trim     | ✅ Complete |
| 3     | fono-hotkey: global-hotkey parser + hold/toggle FSM + listener     | ✅ Complete |
| 4     | fono-stt: trait + WhisperLocal + Groq/OpenAI + factory + prewarm   | ✅ Complete |
| 5     | fono-llm: trait + LlamaLocal stub + OpenAI-compat/Anthropic + factory + prewarm | ✅ Complete |
| 6     | fono-inject: enigo wrapper + focus detection + warm_backend        | ✅ Complete |
| 7     | fono-tray (real appindicator backend) + fono-overlay stub          | ✅ Complete |
| 8     | First-run wizard + CLI (+ tier-aware probe + `fono hwprobe`)       | ✅ Complete |
| 9     | Packaging: release.yml + NimbleX SlackBuild + AUR + Nix + Debian   | ✅ Complete |
| 10    | Docs: README, providers, wayland, privacy, architecture            | ✅ Complete |
| W     | Pipeline wiring (audio→STT→LLM→inject orchestrator)                | ✅ Complete |
| L     | Latency optimisation v0.1 wave (warm + trim + skip + defaults)     | ✅ Complete |
| H     | Local-models out of box + hardware-adaptive wizard (v0.1 slice)    | ✅ Complete |
| S     | Easy provider switching: `fono use`, `fono keys`, IPC Reload, hot-swap | ✅ Complete |

## What landed in this session (2026-04-25, provider switching)

* **S1/S2/S3** — `crates/fono-core/src/providers.rs` central registry of
  every backend's CLI string + canonical env-var name + paired-cloud
  preset. Factories in `fono-stt` / `fono-llm` now resolve a missing
  `cloud` sub-block by falling through to the canonical env var, so the
  smallest valid cloud config is just `stt.backend = "groq"` plus a key
  in `secrets.toml` or env.
* **S4/S5/S6** — `fono use stt|llm|cloud|local|show` subcommand tree in
  `crates/fono/src/cli.rs`; per-call `--stt` / `--llm` overrides on
  `fono record` and `fono transcribe` clone the in-memory config, never
  persist. `set_active_stt` / `set_active_llm` clear the stale `cloud`
  sub-block but preserve every unrelated user customisation.
* **S7** — `fono keys list|add|remove|check`. Atomic 0600 writes;
  `check` runs the same 2-second reachability probe as `fono doctor`.
* **S11/S12/S13** — new `Request::Reload` IPC variant; orchestrator
  holds STT + LLM + Config each behind a `RwLock<Arc<…>>`; `reload()`
  re-reads config + secrets, rebuilds via factories, swaps in place,
  and re-runs `prewarm()` so the first dictation after a switch is
  warm. `fono use` automatically calls Reload on the running daemon.
* **S18** — `fono doctor` Providers section: per-row marker for the
  active backend, key-presence flag, resolved model string, hint to
  switch via `fono use`.
* **S20/S21/S23** — new tests: `crates/fono-stt/src/factory.rs` covers
  cloud-optional resolution; `crates/fono/tests/provider_switching.rs`
  asserts `set_active_stt` / `set_active_llm` preserve unrelated fields,
  TOML round-trip survives swap, and provider-string parsers form a
  bijection with their printers.
* **S24/S25/S27** — `docs/providers.md` rewritten around the new flow;
  README has a "Switching providers" subsection; status.md updated.

## Hotfix this session (2026-04-25, tray Recent submenu + clipboard safety net)

User reported two issues after a real dictation on KDE:

1. *"I can't see any notification or anything in the clipboard after
   doing my last recording"* — root cause was a **subprocess-stdin
   deadlock**: `copy_to_clipboard` borrowed `child.stdin.as_mut()` but
   never closed the pipe, so `xsel`/`xclip`/`wl-copy` (all of which
   read stdin to EOF before daemonizing) hung forever waiting for EOF
   that never came. `child.wait()` then deadlocked, the pipeline
   returned without populating the clipboard, and any notification
   that depended on the outcome never fired. Compounding it: KDE
   Wayland's KWin doesn't implement the wlroots virtual-keyboard
   protocol that `wtype` uses, so even when the inject log read
   `inject: 27ms ok`, no keys actually reached the focused window.
2. *"OpenHistory tray action … should work in a similar fashion to
   clipit"* — clicking the tray entry only opened the parent dir;
   recent dictations weren't visible at all from the tray.

Fixes:

* **`crates/fono-tray/src/lib.rs`** — replaced single `OpenHistory`
  entry with a **"Recent transcriptions" submenu** holding 10
  pre-allocated slots refreshed every ~2 s by a `RecentProvider`
  closure (passed in by the daemon). Click any slot to re-paste that
  dictation. Clipit-style. Slots refresh in place via `set_text` to
  avoid KDE/GNOME indicator flicker. Added `OpenHistoryFolder` as a
  separate entry for power users. New `TrayAction::PasteHistory(usize)`
  carries the slot index.
* **`crates/fono/src/daemon.rs`** — provides the `RecentProvider` that
  reads `db.recent(10)` and returns the cleaned (or raw) labels.
  Handles `PasteHistory(idx)` by fetching the row and calling
  `fono_inject::type_text_with_outcome` on the blocking pool, with a
  notify-rust toast on `Clipboard` outcome.
* **`crates/fono-core/src/config.rs`** — two new `[general]` knobs,
  both default `true`:
  - `also_copy_to_clipboard` — every successful pipeline also copies
    the cleaned text to the system clipboard so the user can Ctrl+V
    even when key injection silently no-op'd.
  - `notify_on_dictation` — every successful pipeline pops a
    notify-rust toast with the dictated text (truncated to 240 chars).
* **`crates/fono-inject/`** — `copy_to_clipboard` made `pub` and
  re-exported so the orchestrator can call it directly.
* **`crates/fono/src/session.rs`** — pipeline now copies-to-clipboard
  + notifies after every successful inject; gives the user reliable
  feedback even on KDE Wayland.

User saw `WARN inject failed: no text-injection backend available` on a
host without `wtype`/`ydotool` and without the `enigo-backend` feature
compiled in. Cleaned text was lost.

* **`crates/fono-inject/src/inject.rs`** — added `Injector::Clipboard`
  fallback that shells out to `wl-copy` (Wayland) → `xclip` → `xsel`
  (X11) and a `wtype --version` page-cache warm step. New
  `InjectOutcome { Typed, Clipboard, NoBackend }` returned from
  `type_text_with_outcome()` so callers can tell the user which path
  ran. `wtype`/`ydotool` failures now fall through to the clipboard
  rather than swallowing the text.
* **`crates/fono/src/session.rs`** — pipeline calls
  `type_text_with_outcome`; on `Clipboard` shows a toast "Fono — text
  copied to clipboard, paste with Ctrl-V"; on `NoBackend` shows a toast
  with a one-line install hint (`pacman -S wtype` / `apt install xsel`).
  The toast prevents a "press hotkey, nothing happens" failure mode
  even when no injector + no clipboard tool exists.
* **`crates/fono/src/doctor.rs`** — Injector section now also lists the
  detected clipboard tool (or "none — text will be lost"); printed near
  the active injector to make the gap obvious.

### Deferred to v0.2 (documented in the plan)

* **S8** wizard multi-key (S7 already lets users add keys post-wizard).
* **S9/S10** named profiles + cycle hotkey (hold for real demand).
* **S14** auto-reload on file change (notify watcher).
* **S15/S16/S17** tray submenu for switching (depends on tray-icon API).
* **S19** dedicated `fono provider list` (covered by `fono use show` + doctor).
* **S22** full reload integration test (covered by S20 unit tests +
  manual; deferred until profiles arrive).
* **S26** ADR `0009-multi-provider-switching.md` (rationale captured in
  this plan + commit messages).

## Build matrix (verified this session, provider switching)

| Command | Result |
|---|---|
| `cargo build --workspace` | ✅ |
| `cargo test --workspace --lib --tests` | ✅ **79 tests pass** (66 unit + 13 hwcheck), 2 ignored (latency smoke) |
| `cargo clippy --workspace --no-deps -- -D warnings` | ✅ pedantic + nursery clean |
| `fono use show` | (manual) prints active stt + llm + key references |
| `fono keys list` | (manual) masked listing |

## What landed in this session (2026-04-25, local-default + hwcheck)

### Tasks fully landed (11 of 25 from the local-default plan)

* **H1** — `crates/fono/Cargo.toml:22-32`: default features now include
  `local-models` (transitively `fono-stt/whisper-local`) so the released
  binary runs whisper out of the box. Slim cloud-only build available
  via `--no-default-features --features tray`.
* **H5/H6/H21** — new `crates/fono-core/src/hwcheck.rs` (478 lines, 13
  unit tests). `HardwareSnapshot::probe()` reads `/proc/cpuinfo`,
  `/proc/meminfo`, `statvfs`, and `std::is_x86_feature_detected!` to
  produce a `LocalTier` ∈ { Unsuitable, Minimum, Comfortable,
  Recommended, HighEnd } with documented thresholds (`MIN_CORES = 4`,
  `MIN_RAM_GB = 4`, `MIN_DISK_GB = 2`, etc.) duplicated as `pub const`
  so docs and tests stay in sync.
* **H11/H12/H13** — wizard rewritten around the tier:
    * `crates/fono/src/wizard.rs` prints the hardware summary up-front.
    * `Recommended`/`HighEnd`/`Comfortable` → local first, default.
    * `Minimum` → cloud first ("faster on your machine"), local kept
      as the second option with a "~2 s" warning.
    * `Unsuitable` → local hidden behind a `Confirm` showing the
      specific failed gate (e.g. "only 2 physical cores; minimum is 4").
    * Local-model menu narrowed to the tier's recommended model + one
      safer fallback (no longer shows whisper-medium on a 4-core box).
* **H16** — `fono doctor` now prints the hardware snapshot and tier
  alongside the existing factory probes, so users see at a glance
  whether their config matches their hardware.
* **H17** — new `fono hwprobe [--json]` subcommand:

  ```
  cores : 10 physical / 12 logical  (AVX2)
  ram   : 15 GB total · disk free : 11 GB · linux/x86_64
  tier  : comfortable (recommends whisper-small)
  ```

  JSON output is consumable by packaging scripts and the bench crate.
* **H20** — `README.md` reflects v0.1.0-rc reality: default release
  bundles whisper.cpp, build-flavour matrix, `fono hwprobe` mention.
* **H24/H25** — plan persisted at
  `docs/plans/2026-04-25-fono-local-default-v1.md`; this status entry.

### Toolchain bumps

* `Cargo.toml:73` — `whisper-rs = "0.13" → "0.16"` (0.13.2 had an
  internal API/ABI mismatch with its sys crate; 0.16 is the current
  upstream and is what whisper.cpp tracks).
* `crates/fono-stt/src/whisper_local.rs:84-92` — adapt to the 0.16
  segment API (`get_segment(idx) -> Option<WhisperSegment>` +
  `to_str_lossy()`).

### Tasks intentionally deferred to v0.2 (all annotated in plan)

* **H8** — Real `LlamaLocal` implementation against `llama-cpp-2`.
  `llama-cpp-2 0.1.x` exposes a low-level API that needs several hundred
  lines of safe-wrapper code; the v0.1 slice ships local STT only with
  optional cloud LLM cleanup. New ADR
  `docs/decisions/0008-llama-local-deferred.md` captures the rationale.
* **H2/H3** — Release CI matrix (musl-slim + glibc-local-capable
  artifacts) — Phase 9 release work, separate from this slice.
* **H4** — OpenBLAS / Metal compile flags (would speed local inference
  another 2–3× on capable hosts) — opt-in v0.2 work.
* **H7/H14/H22** — In-wizard smoke bench + tier-profile bench in
  `fono-bench` — static rule + `fono doctor` are sufficient for v0.1.
* **H15/H18/H19** — Persisting tier in config + flipping
  `LlmBackend::default()` to Local + auto-migration — blocked on H8.
* **H23** — Wizard tier-decision unit test — covered by H21 tier tests
  + manual run; full `dialoguer` mock not worth the dependency.

## Build matrix (verified this session)

| Command | Result |
|---|---|
| `cargo build -p fono` (default features) | ✅ — bundles whisper.cpp |
| `cargo build -p fono --no-default-features --features tray` | (slim, cloud-only — covered by H1's feature graph) |
| `cargo test --workspace --lib --tests` | ✅ **67 tests pass** (54 unit + 13 hwcheck), 2 ignored (latency smoke) |
| `cargo clippy --workspace --no-deps -- -D warnings` | ✅ pedantic + nursery clean |
| `cargo run -p fono -- hwprobe` | ✅ classified host as `comfortable` (10c/16GB/AVX2) |
| `cargo run -p fono -- hwprobe --json` | ✅ structured snapshot + tier |

## Recommended next session

> Recommended next session: execute **Wave 3** of the revised strategic
> plan (Slice B1 — realtime cpal-callback push + first cloud streaming
> provider). Wave 2 landed in three DCO-signed commits:
> `76b9b08` (typed `ModelCapabilities` + split equivalence/accuracy
> thresholds), `87221a2` (per-asset `.sha256` sidecar verification +
> `--bin-dir` CLI flag), and the Thread-C CI gate commit (real-fixture
> `fono-bench equivalence` run against
> `docs/bench/baseline-comfortable-tiny-en.json` on every PR).
>
> Wave 3 concretely:
>
> 1. **Realtime cpal-callback push** (R4 / R10.4 of
>    `plans/2026-04-27-fono-interactive-v6.md`). Replace the
>    record-then-replay live path so the overlay paints text *as the
>    user speaks*. The `Pump` / `broadcast` plumbing landed in
>    Slice A; this is now scope-bounded.
> 2. **Groq streaming STT backend** (R8). Same auth path as the
>    existing Groq batch backend; the `StreamingStt` trait already
>    lives at `crates/fono-stt/src/streaming.rs`. Selectable via
>    `fono use stt groq` with `[interactive].enabled = true`.
> 3. **Equivalence harness cloud rows** (R18.12). Mocked-HTTP
>    recordings so the CI gate runs offline; extend
>    `docs/bench/baseline-comfortable-tiny-en.json` (or sibling) once
>    cloud rows produce stable verdicts.

### Earlier next-session notes (preserved for context)

1. Implement **H8** (`LlamaLocal` against `llama-cpp-2`) so the local
   path also covers LLM cleanup. Keep behind `llama-local` feature flag
   until proven; flip the wizard's local LLM offer back on once H9's
   integration test passes.
2. Land **L7+L8** (streaming LLM + progressive injection) — the next
   biggest perceived-latency win.
3. Pin real fixture SHA-256s via
   `crates/fono-bench/scripts/fetch-fixtures.sh` and commit
   `docs/bench/baseline-*.json` for CI regression gating.
4. Tag `v0.1.0` once `fono-bench` passes on the reference machine.
