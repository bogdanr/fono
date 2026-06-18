# TTS Error-Driven Failover & Quota Notification

## Objective

When the configured cloud TTS backend rejects a request at runtime (quota
exhausted, auth/key, network), Fono should (a) **fail over** to a local
on-device voice so the user still hears the reply, and (b) **surface a
desktop notification** so the user knows the cloud backend is degraded.
Today neither happens on the MCP voice path (`fono.speak`,
`fono.summarize`, `fono.confirm`, `fono.listen`), which is exactly the
path that produced the reported failure:

```
fono.speak: TTS synthesis failed: elevenlabs TTS returned 401 Unauthorized:
{"detail":{"code":"quota_exceeded","message":"This request exceeds your quota
of 10000. You have 55 credits remaining, while 284 credits are required ..."}}
```

## Root-Cause Findings

The user's mental model ("we have a TTS fallback") is partly true but does
not cover this case. Three concrete gaps, ranked by impact:

1. **No error-driven failover exists.** The only TTS fallback wrapper,
   `EnglishOnlyFallback` (`crates/fono-tts/src/english_only_fallback.rs:41`),
   routes *non-English text* to a local Piper voice. It is purely a
   **language-capability** router — it never inspects synthesis errors. When
   the primary cloud backend returns an error, that error propagates straight
   up through `synthesize` and aborts playback. There is no cloud→local
   failover on quota/auth/network errors anywhere in `fono-tts`. Source: the
   factory only ever wraps with `maybe_wrap_english_only`
   (`crates/fono-tts/src/factory.rs:64`), and `speak_text` calls
   `tts.synthesize(...).await.context("TTS synthesis failed")?`
   (`crates/fono-mcp-server/src/voice_io.rs:536`) with no recovery branch.

2. **The MCP voice path never notifies.** `critical_notify` is only invoked
   from the daemon dictation/assistant pipeline (`crates/fono/src/session.rs`
   and `crates/fono/src/assistant.rs` — e.g. the TTS notify at
   `crates/fono/src/assistant.rs:912`). The MCP server crate
   (`fono-mcp-server`) contains **zero** `critical_notify` calls. The
   `fono.speak` tool simply turns the error into a `ToolCallResult::failure`
   string (`crates/fono-mcp-server/src/tools/speak.rs:91`), which the agent
   prints but the user never sees as a desktop popup. In voice mode the agent
   *is* the consumer, so the failure was invisible to the human.

3. **The quota error would be misclassified even if notify were wired.**
   ElevenLabs returns quota exhaustion as **HTTP 401** with a typed
   `quota_exceeded` code. `critical_notify::classify`
   (`crates/fono-core/src/critical_notify.rs:125`) checks
   `contains_status(401)` → `ErrorClass::Auth` **before** any quota check,
   and there is no quota/usage class at all. The user would get
   "Fono — TTS key rejected … run `fono setup` to update the key"
   (`crates/fono-core/src/critical_notify.rs:452-459`), which is misleading:
   the key is valid, the credits ran out. The existing `PaymentRequired`
   class (402 / `paid_plan_required`) is the closest analogue but is not
   matched because the 401 branch wins first.

## Assumptions

- The `tts-local` feature is the intended failover target (it already powers
  `EnglishOnlyFallback` and `TtsBackend::Local`). When `tts-local` is not
  compiled in, failover degrades to "notify only, no audio" — same posture as
  the existing English-only skip path.
- Failover should be **transparent and automatic** (no new required config),
  matching the precedent set by `EnglishOnlyFallback` ("no new config knobs").
  An opt-out is acceptable but not required for v1.
- Desktop notification from the MCP process is acceptable: `critical_notify`
  → `notify::send` shells out to `notify-send` / `notify-rust`, which works
  from any process. The per-process dedup/session state in `critical_notify`
  is independent per process, which is fine for the short-lived MCP callers.
- "Quota exhausted" is treated as user-actionable and distinct from "key
  rejected" — it deserves its own notification copy.

## Implementation Plan

### A. Classifier: recognise quota exhaustion

- [ ] Task A1. Add a `QuotaExceeded` variant to
  `fono_core::critical_notify::ErrorClass`
  (`crates/fono-core/src/critical_notify.rs:87`). Rationale: a valid key with
  no remaining credits is a distinct, user-actionable state from an invalid
  key; conflating it with `Auth` sends the user to `fono setup` for no reason.

- [ ] Task A2. In `classify` (`crates/fono-core/src/critical_notify.rs:125`),
  detect quota exhaustion **before** the 401/403 → `Auth` branch. Match the
  typed signals `quota_exceeded`, `insufficient_quota`, and the phrase
  `exceeds your quota` / `credits remaining` (case-insensitive). Rationale:
  ElevenLabs (and some others) wrap quota errors in a 401, so the bare-status
  check must not win first.

- [ ] Task A3. Extend the `(Stage::Tts, …)` arm of `render`
  (`crates/fono-core/src/critical_notify.rs:452`) with `QuotaExceeded` copy,
  e.g. "Fono — TTS quota exhausted ({provider}) … reply spoken with the local
  voice instead. Top up your {provider} credits or switch backends in the
  tray." Add matching copy for the `Stt` / `Assistant` / `Polish` stages for
  symmetry. Rationale: actionable, accurate remediation.

- [ ] Task A4. Add `QuotaExceeded` to the user-actionable set in
  `notify_actionable` (`crates/fono-core/src/critical_notify.rs:379`) so
  build/reload-site failures of this class also surface.

- [ ] Task A5. Unit tests in `critical_notify.rs` `mod tests`: feed the exact
  ElevenLabs 401 `quota_exceeded` body and assert `classify` →
  `QuotaExceeded` (not `Auth`); assert a generic 401 still maps to `Auth`;
  assert the rendered body mentions quota and does **not** say "update the
  key".

### B. TTS error-driven failover wrapper

- [ ] Task B1. Introduce a `FailoverTts` wrapper in `fono-tts` (new module,
  mirroring `english_only_fallback.rs`'s structure) that holds the primary
  cloud backend plus a lazily-built local engine, and implements
  `TextToSpeech`. Rationale: reuse the proven lazy-load + cache + warn-once
  pattern; keep the failover concern isolated from each backend client.

- [ ] Task B2. In `FailoverTts::synthesize`, call the primary; on `Err`,
  classify the error with `fono_core::critical_notify::classify`. For the
  recoverable classes (`QuotaExceeded`, `Auth`, `RateLimit`, `Network`,
  `PaymentRequired`), lazily build/cache the local engine for the utterance's
  language and re-synthesize through it; on success return that audio.
  Rationale: these are exactly the cases where a local voice keeps the user
  unblocked; genuine `Other`/parse errors should still surface as failures.

- [ ] Task B3. On the failover path, invoke `critical_notify::notify` with
  `Stage::Tts`, the primary's `name()`, the classified class, and the error
  text — at most once per session via the existing dedup. Rationale: the user
  must learn the cloud backend is degraded even though audio still played.

- [ ] Task B4. When `tts-local` is **not** compiled in (or the local engine
  cannot be built), do not fail the call silently: still emit the
  notification, then propagate the original error so callers behave as today.
  Provide a `#[cfg(not(feature = "tts-local"))]` no-op constructor mirroring
  `maybe_wrap_english_only` (`crates/fono-tts/src/factory.rs:88`).

- [ ] Task B5. Wire the wrapper into `build_tts`
  (`crates/fono-tts/src/factory.rs:46-65`): wrap the primary (after the
  existing English-only wrap) only when the backend is a **cloud** backend
  and `tts-local` is available. Compose cleanly so an English-only cloud voice
  still gets both wrappers (language routing *and* error failover). Rationale:
  single construction site keeps every caller (`speak_text`, `speak --stream`,
  daemon assistant) covered without per-path plumbing.

- [ ] Task B6. Factor the local-engine lazy-build/cache helper so it is shared
  between `EnglishOnlyFallback` and `FailoverTts` rather than duplicated
  (both need `build_local_engine` /
  `crates/fono-tts/src/english_only_fallback.rs:131` semantics). Rationale:
  avoid two copies of the download/load/warn logic drifting apart.

- [ ] Task B7. Unit tests for `FailoverTts`: a fake primary that returns a
  quota error must trigger the local path (assert local engine consulted); a
  fake primary returning `Other` must propagate the error; the warn/notify
  must fire at most once. Use the unreachable-mirror trick from
  `english_only_fallback.rs:258` to keep tests offline.

### C. MCP voice path: notify on hard failure

- [ ] Task C1. In `speak_text` (`crates/fono-mcp-server/src/voice_io.rs:515`),
  when synthesis ultimately fails (after the B-wrapper has had its chance),
  classify and call `critical_notify::notify(Stage::Tts, backend_name, …)`
  before returning the error. Rationale: even when failover is impossible
  (no `tts-local`, or a non-recoverable error) the human still gets a popup
  instead of a silent agent-only failure — the second half of the reported
  bug.

- [ ] Task C2. Confirm the same treatment covers the other MCP voice callers
  that synthesize prompts — `fono.confirm`
  (`crates/fono-mcp-server/src/tools/confirm.rs:154`), `fono.listen`
  (`crates/fono-mcp-server/src/tools/listen.rs:156`), and `fono.summarize`
  (`crates/fono-mcp-server/src/tools/summarize.rs:190`) — since they all route
  through `speak_text`. Centralising the notify in `speak_text` (Task C1)
  should be sufficient; verify no caller swallows the error earlier.

- [ ] Task C3. Add a session-reset call (or rely on the auto-reset window at
  `crates/fono-core/src/critical_notify.rs:41`) appropriate to the
  short-lived MCP process so repeated independent failures across separate
  invocations are not permanently suppressed. Rationale: the daemon resets the
  flag per dictation; the MCP process needs an equivalent so the user keeps
  getting told if the situation persists across turns.

### D. Documentation

- [ ] Task D1. Document the new failover behaviour and quota notification in
  `docs/providers.md` (the ElevenLabs section already discusses plan-gating /
  402 at `crates/fono-tts/src/elevenlabs.rs:25-29`) so the quota-vs-auth
  distinction and the automatic local failover are discoverable.

- [ ] Task D2. Update `docs/status.md` session log per the project's
  one-phase-at-a-time rule.

## Verification Criteria

- A simulated ElevenLabs `401 quota_exceeded` body classifies as
  `QuotaExceeded`, not `Auth`, and renders quota-specific copy.
- With `tts-local` compiled in, a primary-backend quota/auth/network error
  results in audible local-voice playback (non-empty PCM) instead of an
  aborted call, plus exactly one desktop notification per session.
- Without `tts-local`, the same error produces a desktop notification and the
  original error is still returned (no behaviour regression for callers).
- `fono.speak`, `fono.confirm`, `fono.listen`, and `fono.summarize` all
  surface a desktop notification on unrecoverable TTS failure.
- A generic invalid-key 401 still classifies as `Auth` and still tells the
  user to update the key.
- Pre-commit gate passes: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`.

## Potential Risks and Mitigations

1. **Provider-specific quota wire formats differ.** OpenAI uses 429 +
   `insufficient_quota`; ElevenLabs uses 401 + `quota_exceeded`; others vary.
   Mitigation: match on the typed code strings/phrases (Task A2) in addition
   to status, and add a regression test per known provider shape.

2. **Failover masks a misconfiguration.** Silent fallback to a robotic local
   voice could hide that the user's paid backend is broken. Mitigation: the
   mandatory one-shot notification (Task B3/C1) ensures the degradation is
   always reported even though audio still plays.

3. **Local voice cold-start latency on first failover.** Downloading/loading
   the Piper voice adds delay mid-utterance. Mitigation: lazy-build-and-cache
   (reuse `english_only_fallback`'s cache), and document that the first
   failover utterance may be delayed; optionally pre-warm when the cloud
   backend's last call failed.

4. **Double-wrapping ordering bug.** Composing `FailoverTts` over
   `EnglishOnlyFallback` (or vice versa) could route incorrectly. Mitigation:
   define a clear, tested composition order in `build_tts` (failover outermost
   so language routing happens on whichever engine ultimately runs) with a
   wiring unit test.

5. **Notification spam across many short MCP calls.** Each MCP invocation is a
   fresh process with its own dedup state. Mitigation: rely on the per-process
   session gate plus the 120 s auto-reset; tune Task C3 so persistent failures
   re-notify at a reasonable cadence rather than on every single call.

## Alternative Approaches

1. **Config-driven explicit fallback chain** (`[tts].fallback_backend = ...`):
   user names a second backend to try on failure. More flexible and explicit,
   but adds a config knob and onboarding surface; contradicts the
   "no new config knobs" precedent. Could be layered on later.

2. **Notify-only, no failover** (Task A + C, skip B): smallest change — the
   user always learns the cloud backend failed but hears nothing. Lower value
   for voice-mode users who specifically want the reply spoken; recommended
   only if local-failover work must be deferred.

3. **Per-backend retry-then-fail inside each client** (e.g. inside
   `ElevenLabsTts::synthesize`): keeps logic close to the wire format but
   duplicates failover across every backend and cannot reach a *different*
   engine. Rejected in favour of the single wrapper (B).
