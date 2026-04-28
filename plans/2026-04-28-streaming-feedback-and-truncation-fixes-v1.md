# Streaming feedback gaps + F8 truncation fixes

## Objective

Three independent symptoms reported on `v0.3.3` smoke-test:

1. **Rate-limit invisibility.** The cloud rejected requests with HTTP 429 but nothing visible reached the user — the existing `tracing::info!` line in `groq.rs` was either filtered by the active log level or never fired. The user wants a desktop notification, deduped to **once per dictation** (per F8/F9 press).
2. **F8 message truncation.** Push-to-talk dictations sometimes drop the trailing portion of the user's utterance. The streaming pipeline finalizes too aggressively when the hotkey is released, before the audio thread has flushed the last cpal callback chunks.
3. **Hotkey logs invisible in streaming mode.** Pressing F8/F9 does not produce any log entries at default INFO level. The log channel goes silent immediately after `live update: lane=…` debug lines disappear (which require `RUST_LOG=debug`).

Closes the feedback gap that's making it impossible to smoke-test future streaming changes.

## Implementation Plan

- [ ] Task 1. **Promote hotkey-dispatch log to INFO.** In `crates/fono/src/daemon.rs:463` change `tracing::debug!` to `tracing::info!`. Rationale: this is the single line that says "I received your F8/F9 press and routed it to the FSM" — it is the canonical evidence of hotkey reception and belongs at INFO. Add a similar INFO line in the `HotkeyEvent::StartLiveDictation` and `HotkeyEvent::StopLiveDictation` branches at `daemon.rs:388-411` so the user sees the *outcome* (live session started / stopped) at the same verbosity. Format: `"live: started ({mode:?})"` and `"live: stopped"`. Keep the existing FSM-internal `live update: lane=…` debug lines at `live.rs:330` at DEBUG so they don't drown the log under steady-state operation.

- [ ] Task 2. **Add `interactive.hold_release_grace_ms` config knob.** New field in `Interactive` struct at `crates/fono-core/src/config.rs:585-658`, `u32`, default `300`. Documents that this is the delay applied between `LiveHoldReleased` arriving at `on_stop_live_dictation` and the actual `capture_stop_tx.send(())` call at `crates/fono/src/session.rs:958`. Rationale: cpal's host-side buffer typically holds 100-200 ms of samples that have been captured but not yet drained through the bridge channel; releasing F8 immediately stops the cpal `Stream` and abandons that buffer's contents. A 300 ms grace gives the bridge thread time to drain into the broadcast channel, and the audio FSM time to emit the final `Voiced` chunks before the bridge sees a closed Sender and emits `Eof`.

- [ ] Task 3. **Wire the grace into `on_stop_live_dictation`.** In `crates/fono/src/session.rs:932-944`, after the `Some(mut session)` extraction and *before* the `capture_stop_tx.send(())` call at `:958`, insert `tokio::time::sleep(Duration::from_millis(u64::from(grace_ms))).await;` where `grace_ms = self.current_config().interactive.hold_release_grace_ms`. The grace runs *after* taking the session out of the mutex (so a re-press during the grace window cleanly errors out the late session rather than racing) and *before* stopping audio capture. Update the teardown-order comment at `session.rs:946-957` to document the new step 0 ("wait `hold_release_grace_ms` so cpal's pending callback samples reach the bridge").

- [ ] Task 4. **Toggle-mode handling.** F9 (toggle-stop) should also benefit from this grace; the same code path at `session.rs:932` handles both modes. Confirm in code review that the `LiveTogglePressed` → `StopLiveDictation` path goes through `on_stop_live_dictation` (it does — see `daemon.rs:406`), so Task 3 covers it automatically. No separate work needed.

- [ ] Task 5. **Add `notify-rust` to `fono-stt` deps.** In `crates/fono-stt/Cargo.toml`, add `notify-rust = { workspace = true, optional = true }` under a new `notify` feature. Wire the feature transitively from `fono`'s `cloud-groq` and similar features so any cloud-enabled build pulls it in. Rationale: keeping it feature-gated means slim local-only builds don't inherit a cross-platform notification dependency they will never exercise.

- [ ] Task 6. **Add session-scoped 429 dedup to `fono-stt::groq`.** New module `crates/fono-stt/src/rate_limit_notify.rs` exposing:
  - `static NOTIFIED_THIS_SESSION: AtomicBool` (false initial).
  - `pub fn reset_session_flag()` — clears the bool. Called by `SessionOrchestrator` on every `on_start_recording` and `on_start_live_dictation` entry point.
  - `pub fn notify_once(provider: &str, body: &str)` — `compare_exchange(false, true)`; on success, if `notify-rust` feature is enabled, fires a `notify_rust::Notification` with title `"Fono — cloud rate-limited"` and body suggesting `interactive.streaming_interval = 2.0` or higher; on failure (already notified this session), no-op.
  Make the AtomicBool reset itself defensively after `Duration::from_secs(120)` from the most recent `notify_once` call, so a long-running daemon that misses a `reset_session_flag` call (e.g. due to a panic mid-session) eventually re-arms.

- [ ] Task 7. **Wire `notify_once` into `groq_post_wav` + `groq_post_wav_verbose`.** Replace the two existing `tracing::info!` 429 sites in `crates/fono-stt/src/groq.rs:200-211` and `:240-251` with calls to `crate::rate_limit_notify::notify_once("groq", body)` *plus* a `tracing::warn!` (promote from info → warn so the log line shows at default verbosity even without an explicit `RUST_LOG=info`). Both sites share the same body string — extract a `const RATE_LIMIT_HINT` to keep them in sync.

- [ ] Task 8. **Wire `reset_session_flag` from session.rs.** In `SessionOrchestrator::on_start_recording` and `on_start_live_dictation` (at `crates/fono/src/session.rs` — exact line numbers depend on the current working tree), add `fono_stt::rate_limit_notify::reset_session_flag();` as the first executable statement. Re-export the function from `fono_stt::lib.rs` so the call site stays one-line.

- [ ] Task 9. **Update tests.**
  - `crates/fono-core/src/config.rs` test for `Interactive` defaults — add `assert_eq!(i.hold_release_grace_ms, 300);` to the relevant test in the `tests` module.
  - `crates/fono-stt/src/rate_limit_notify.rs` unit tests — `reset_session_flag` clears the bool; first `notify_once` after reset returns `true`; second returns `false`; auto-reset after 120 s simulated.
  - `crates/fono/tests/live_pipeline.rs` integration test — add `live_session_includes_trailing_audio_after_grace()` that simulates a fast hold-release and asserts the final transcript contains audio captured up to 250 ms before the release (to verify the grace actually buffers it through).

- [ ] Task 10. **Docs.**
  - `CHANGELOG.md` — `[Unreleased]` `Added` (hold-release grace, 429 desktop notification) and `Changed` (hotkey-dispatch log INFO).
  - `docs/troubleshooting.md` — new "F8 cuts off the end of my message" section pointing at `interactive.hold_release_grace_ms`.
  - `docs/providers.md` — extend the existing "Streaming and rate limits" section with a note that 429s now surface as desktop notifications, deduped per dictation.

- [ ] Task 11. **Verify.** `cargo build -p fono`, `cargo test --workspace --lib`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`. Confirm `target/debug/fono --version` reports the bumped version. Smoke-test manually: with `RUST_LOG=info`, press F8 and confirm `INFO hotkey: HoldPressed -> ...` appears; release F8 mid-sentence and confirm the log shows `INFO live: stopped` then a transcript including the trailing word; force a 429 (set `streaming_interval = 0.5` and dictate for 30 s) and confirm exactly one desktop notification per F8 press.

- [ ] Task 12. **Do not auto-tag.** Per the user's standing rule, after Task 11 verifies clean, **stop and ask** before bumping the version, updating ROADMAP, or tagging. The user runs the smoke test against `target/debug/fono` first.

## Verification Criteria

- `RUST_LOG=info ./target/debug/fono` with default config: pressing F8 produces an INFO line containing `HoldPressed`; pressing F9 produces one containing `TogglePressed`. No DEBUG-level filter required.
- Recording a sentence, then releasing F8 mid-pause (within ~100 ms of the last word): the injected text contains the last word. Repeating 10 times: ≥ 9/10 trials include the last word (small residual risk for sub-100 ms releases).
- With `streaming_interval = 0.5` and continuous dictation: the first 429 produces one `notify_rust` desktop notification with the cadence-bump suggestion. Subsequent 429s within the same F8 press produce zero notifications. A new F8 press re-arms the flag — the next 429 produces one notification again.
- Slim build (`--no-default-features --features tray,cloud-all`) compiles cleanly with the `notify-rust` feature off; the 429 path falls back to `tracing::warn!` only.
- All tests in Task 9 pass; existing 191 tests still pass.

## Potential Risks and Mitigations

1. **Hold-release grace makes F8 feel laggy.** 300 ms is at the edge of perceptibility. Users who press-release F8 in rapid bursts (e.g. dictating short phrases like "yes", "send it") will notice an extra ~quarter-second before the transcript appears.
   Mitigation: 300 ms is the *capture-extension* delay; the user-visible "processing" overlay state can switch to `Processing` immediately on `LiveHoldReleased` so there's instant feedback. Make the grace configurable so power users can lower it to 150 ms; users on slow audio interfaces can raise it.

2. **Desktop notification spam if the dedup AtomicBool is never reset.** If a panic in `on_start_live_dictation` skips the reset call, the user gets exactly one 429 notification ever, then silence — masking future rate-limit problems.
   Mitigation: the 120-second auto-reset in Task 6 covers this. Additionally, log the dedup decision (`tracing::debug!("rate_limit_notify: suppressed (already fired this session)")`) so the dedup is observable in debug logs without firing more notifications.

3. **`notify-rust` feature plumbing is fragile across the cloud-* features.** Adding an `optional = true` dep that the cloud-* features must each list separately is error-prone — easy to forget one.
   Mitigation: a single `notify` feature on `fono-stt` that is **always** enabled by `fono` itself (since `fono` already depends on `notify-rust`), with `default-features = false` only for `fono-stt`'s own bench/test targets. That moves the gate to one place.

4. **F8 truncation could have a deeper cause.** If a 300 ms grace doesn't fix it for the user, the audio thread may be dropping samples at the cpal callback level (e.g. backpressure on the realtime SPSC channel under load).
   Mitigation: add a `tracing::debug!("audio: dropped N samples due to SPSC backpressure")` counter in the cpal forwarder and surface the total in the `pipeline ok:` summary line. Lets us distinguish "grace too short" from "audio queue overflowed" if the bug recurs.

## Alternative Approaches

1. **Skip the desktop notification, use stderr prominently.** Promote the 429 log line to `tracing::error!` so it shows at every default verbosity. Trade-off: no system-bus notification, no popup; only users who follow the daemon log will notice. Cheaper to implement (no `notify-rust` plumbing) but worse UX.
2. **Dedup 429 notifications by time window only, no session reset.** A `last_notified_at: Mutex<Option<Instant>>` with a "no more often than every 60 s" rule. Simpler than tying to session lifecycle, but a user who triggers a 429 then presses F8 immediately after would not get a fresh notification — counter to the user's "once per dictation" requirement.
3. **F8 truncation: lengthen the audio FSM's voiced-tail window instead of adding a grace.** Modify the VAD-driven stream to keep emitting `Voiced` for ~300 ms after the hotkey release. More invasive (changes audio FSM semantics globally) but doesn't add a fixed delay to teardown — the grace only fires when the user actually had trailing audio.
4. **Session-orchestrator-owned notification channel.** Instead of `fono-stt` knowing about `notify-rust`, route 429s through a generic `BackendEvent::RateLimited { provider }` enum that `SessionOrchestrator` subscribes to and converts to notifications. Cleanest separation of concerns; biggest plumbing change. Worth it for future events (auth-failed, model-not-available) but overkill for a single event.
