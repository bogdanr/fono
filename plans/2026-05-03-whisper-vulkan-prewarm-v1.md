# Whisper Local: Vulkan Pipeline Prewarm

## Objective

Eliminate the one-off 5–10 s "first dictation" stall observed on Vulkan-accelerated
hosts (e.g. RTX 4090: 7.8 s on fixture #1, 0.1–0.2 s afterwards) by extending
`WhisperLocal::prewarm()` to drive a tiny no-op inference. This forces
`whisper.cpp`'s Vulkan backend to materialise all `VkPipeline` objects,
allocate the KV cache / encoder workspace, and prime the driver shader cache
**before** the user presses the dictation hotkey. The change must be a no-op
for the CPU build path (where the cost doesn't exist) and must not regress the
existing async load behaviour described in `docs/plans/2026-04-25-fono-latency-v1.md`.

## Scope and Assumptions

- The cost reproduces on any Vulkan driver/GPU because it's pipeline-creation
  work, not host-side init. Severity scales inversely with driver maturity
  (NVIDIA proprietary fastest, RADV middle, lavapipe worst).
- Driver-side persistent shader caches (`~/.nv/GLCache`,
  `~/.cache/mesa_shader_cache`) help across process launches but not within a
  single process — `VkPipeline` creation still re-runs.
- Only `whisper-local` STT is affected; cloud arms (Groq/OpenAI/Wyoming) have
  no GPU pipelines.
- `llama-local` (Vulkan) has the *same* class of problem and should be
  addressed by a sibling change in a follow-up task; out of scope here to keep
  the diff focused and reviewable.
- We assume `whisper.cpp` exposes enough of `WhisperState::full` through
  `whisper-rs` to feed a short silent buffer; this is already used by
  `crates/fono-stt/src/whisper_local.rs` for normal transcription, so no new
  FFI surface is needed.

## Implementation Plan

- [ ] Task 1. Audit the current prewarm path in `crates/fono-stt/src/whisper_local.rs:220-239`
      and confirm it only constructs `WhisperContext` without creating a
      `WhisperState` or running inference. Document the gap inline (comment
      block) so future readers understand why a dummy decode is necessary on
      GPU backends. Rationale: anchors the change to the observed bench
      result and prevents a future "simplify by removing the dummy decode"
      regression.

- [ ] Task 2. Extend `WhisperLocal::prewarm()` to additionally run a single
      short, silent decode after the context is loaded. Approach:
      synthesise a `Vec<f32>` of ~1.0 s of silence at 16 kHz (16000 zero
      samples), build a `FullParams` with `n_threads = num_cpus()`,
      `language = Some("en")`, `translate = false`, `no_context = true`,
      `single_segment = true`, `print_*` all off, `temperature = 0.0`, and
      call `state.full(params, &silence)` on a fresh `WhisperState` created
      from the loaded context. Discard the (empty) result. Rationale: this
      forces whisper.cpp's Vulkan backend to materialise every pipeline used
      by the encoder + decoder hot path against the real tensor shapes for
      the loaded model variant, so the *user's* first dictation pays only the
      sub-second steady-state cost we measured (~0.1–0.2 s batch on RTX
      4090).

- [ ] Task 3. Make the dummy decode best-effort: wrap the `state.full(...)`
      call so a failure logs at `debug!` and returns `Ok(())` rather than
      surfacing as a prewarm error. Rationale: prewarm is already documented
      as best-effort (`crates/fono-stt/src/traits.rs:31-38`); a hypothetical
      driver bug that breaks the silent-decode pass must not block the user
      from invoking real dictation, where a non-silent buffer might still
      succeed.

- [ ] Task 4. Run the dummy decode on the same `tokio::task::spawn_blocking`
      thread that already loads the model, *after* the `*guard = Some(c)`
      assignment, while still holding the mutex briefly enough to create a
      `WhisperState` from the freshly-loaded context. Drop the state at the
      end of the closure so it doesn't pin GPU memory. Rationale: keeps the
      prewarm a single bounded blocking job from the orchestrator's point of
      view (`crates/fono/src/session.rs:562-585`), preserves the existing
      timing log line ("warmup: stt whisper-local ready in {}ms"), and
      avoids leaking a long-lived `WhisperState` across the lock.

- [ ] Task 5. Gate the dummy-decode step behind a runtime check that's a
      no-op on CPU builds. Two acceptable strategies; pick the simpler one
      after a quick read of `whisper-rs`'s feature surface:
        a. Cargo feature plumbing — only execute the dummy decode when the
           workspace was built with `accel-vulkan` (or any future
           `accel-*`). Add a `#[cfg(feature = "accel-vulkan")]` arm in
           `whisper_local.rs` that runs the silent decode and a
           `#[cfg(not(any(feature = "accel-vulkan")))]` arm that's a no-op.
        b. Always run the silent decode unconditionally — it costs ~50–150
           ms on CPU and ~5–10 s on Vulkan but moves both into the
           background warmup phase. Acceptable if (a) is awkward to plumb
           because `fono-stt`'s own Cargo features don't currently expose
           the accel toggle.
      Rationale: the user's complaint is GPU-specific; CPU dictation
      already starts in <100 ms TTFF and shouldn't grow. Strategy (a) is
      preferred but (b) is acceptable because the cost is paid in the
      background warmup that already runs at session startup.

- [ ] Task 6. Add an integration-style smoke test under
      `crates/fono-stt/tests/` (or extend an existing whisper-local test)
      that calls `prewarm()` against a tiny model fixture and asserts it
      returns `Ok(())` within a generous timeout. Skip the test by default
      (`#[ignore]` or env-gated, mirroring the pattern in
      `tests/wyoming_round_trip.rs`) so CI runners without the model file
      don't fail. Rationale: regression guard against a future change that
      accidentally bypasses the silent decode.

- [ ] Task 7. Re-run the bench on `ai` with the updated binary
      (`FONO_BENCH_NO_BUILD=0 ./tests/bench.sh`) and confirm fixture #1's
      `batch_s` drops from ~7.8 s to the same 0.1–0.2 s range as the rest of
      the Vulkan column. Capture the new "Whisper STT comparison" speedup
      table in the PR description. Rationale: closes the loop on the
      original benchmark observation that motivated the change.

- [ ] Task 8. Update `docs/plans/2026-04-25-fono-latency-v1.md` (or the
      successor latency plan if one supersedes it) with a one-line entry
      noting that whisper-local prewarm now covers Vulkan pipeline
      materialisation, and add a `## [Unreleased]` bullet to `CHANGELOG.md`
      under `### Performance`. Rationale: per `AGENTS.md`, every shippable
      perf change needs a CHANGELOG line so the next release tag's notes are
      accurate.

- [ ] Task 9. Update `docs/status.md` with a session-log entry recording the
      bench numbers before/after and the prewarm extension. Rationale:
      project rule — every session ends with a status update.

- [ ] Task 10. (Follow-up, separate change) Mirror the same pattern in
      `crates/fono-llm/src/llama_local.rs::prewarm` so the *first* LLM
      cleanup call after session start doesn't pay the equivalent
      pipeline-compile cost on Vulkan-accelerated hosts. Out of scope for
      the STT change but worth filing as a tracking issue / roadmap bullet
      when the STT fix lands.

## Verification Criteria

- On the `ai` host (RTX 4090 + Vulkan), `tests/bench.sh` reports `batch_s`
  for `en-single-sentence` within ±50 % of the median Vulkan `batch_s`
  across the other nine fixtures (currently 0.1–0.2 s), instead of the
  current 7.8 s outlier.
- Total `tests/bench.sh` wall time on `ai` drops by ~5–7 s.
- CPU benchmark numbers on the same host are unchanged within noise (±5 %).
- `warmup: stt whisper-local ready in {}ms` log line on a Vulkan build is
  the only place the multi-second cost appears; subsequent
  `Transcribe` / `StreamingTranscribe` operations do not log a
  multi-second first-call delay.
- All existing `cargo test -p fono-stt` cases still pass.
- Driver shader cache (`~/.nv/GLCache` or `~/.cache/mesa_shader_cache`)
  populates after the first run, making subsequent process launches even
  faster (informational, not gating).

## Potential Risks and Mitigations

1. **Silent-audio decode triggers a different pipeline subset than real
   speech.** Some Vulkan kernels are only used when the decoder emits
   non-trivial token sequences (e.g. specific quant×shape combinations for
   long-context attention). If the silent decode short-circuits early, the
   user's first real dictation may still pay a smaller-but-noticeable cost.
   Mitigation: feed a 1.0 s buffer (not 100 ms) and keep
   `single_segment = true` *off* if needed; if measurement shows residual
   first-call cost, switch the prewarm input to a low-amplitude sine sweep
   or a tiny clip from `tests/fixtures/equivalence/audio/` so the decoder
   actually emits tokens.

2. **Prewarm latency budget regression.** Today `prewarm()` is bounded by
   model load (~200–600 ms mmap). Adding a silent decode pushes it to
   5–10 s on Vulkan. The `spawn_warmups()` call site in
   `crates/fono/src/session.rs:559-585` is already fire-and-forget at
   session startup, so this is acceptable — but if any caller awaits
   prewarm synchronously, that path stalls. Mitigation: audit callers and
   confirm all are detached (`tokio::spawn` or `spawn_blocking` without an
   `.await` on the join handle from a UI-blocking task); add a doc comment
   on the trait method clarifying expected upper bound.

3. **whisper-rs API drift.** If the silent-decode requires a newer
   `whisper-rs` than the one pinned in `Cargo.toml`, we'd need a dep bump
   and a `deny.toml` license review per `AGENTS.md`. Mitigation: confirm
   the existing version already exposes `WhisperState::full` (it does — it's
   the same call used in normal transcription at
   `crates/fono-stt/src/whisper_local.rs` around the main transcribe path).

4. **CPU-only users see a small (~50–150 ms) addition to startup.**
   Mitigation: prefer feature-gating (Task 5 strategy a) so CPU builds skip
   the dummy decode entirely.

5. **Driver shader cache can be invalidated** by GPU driver upgrades, so
   the "second-launch is fast" property is not permanent. Mitigation: out
   of scope — the prewarm covers the common case (driver cache miss /
   first-ever launch); we can't influence driver behaviour.

## Alternative Approaches

1. **Persist a Vulkan pipeline cache file ourselves** (`VkPipelineCache`
   serialised under `~/.cache/fono/vulkan_pipeline_cache.bin`). Trade-off:
   `whisper.cpp`/`ggml-vulkan` doesn't expose a way to inject a
   pre-populated `VkPipelineCache` through the public API, so this would
   require patching upstream. Rejected as too invasive for the gain.

2. **Block the *first* `Transcribe` and run the silent decode under the
   wait spinner** instead of at session start. Trade-off: the user still
   waits, just at a different moment, and the wait is now visible
   (hotkey-pressed → 5 s nothing → text). Worse UX than today's silent
   background warmup. Rejected.

3. **Run prewarm on a timer that fires when the user is "likely about to
   dictate"** (e.g. on tray menu open, focus change, hotkey-modifier press
   without the trigger key). Trade-off: complex heuristics for marginal
   gain — the current background warmup at session start is already fast
   enough on a typical desktop boot, and re-running it speculatively wastes
   GPU cycles. Defer.

4. **Move whisper inference to a long-lived child process** so pipeline
   cost is amortised across dictations and hidden behind IPC.
   Trade-off: large architectural change, conflicts with the
   single-binary design goal in `AGENTS.md`. Rejected for v0.x.
