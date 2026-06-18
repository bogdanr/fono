# Cloud Quota / Hard-Failure Resilience (STT + TTS)

> Supersedes `2026-06-16-tts-error-failover-and-notify-v1.md`. That plan
> scoped only the TTS side; this version broadens to the real problem:
> a single provider's quota exhaustion (ElevenLabs is the trigger, but the
> pattern is general) breaks **both** transcription (STT) **and** spoken
> output (TTS), across **both** the daemon dictation pipeline **and** the
> MCP voice tools — and today most of those paths neither fail over nor
> notify.

## Objective

When a cloud provider rejects a request at runtime because credits are
exhausted (or the key/network is bad), Fono should:

1. **Classify quota exhaustion correctly** (not as a generic "bad key").
2. **Notify the user** on every affected path — STT and TTS, daemon and MCP.
3. **Fail over to a local engine** where one is available (local Whisper for
   STT, local Piper for TTS) so dictation and spoken replies keep working.

The trigger that surfaced this:

```
fono.speak: TTS synthesis failed: elevenlabs TTS returned 401 Unauthorized:
{"detail":{"code":"quota_exceeded","message":"This request exceeds your quota
of 10000. You have 55 credits remaining, while 284 credits are required ..."}}
```

The same ElevenLabs key powers an STT backend
(`crates/fono-stt/src/elevenlabs.rs`, wired at
`crates/fono-stt/src/factory.rs:107`), so once credits run out **transcription
stops too** — confirmed by the user.

## Root-Cause Findings (ranked by impact)

1. **Quota exhaustion is misclassified as an auth error.** ElevenLabs returns
   quota errors as **HTTP 401** with a typed `quota_exceeded` code.
   `critical_notify::classify` (`crates/fono-core/src/critical_notify.rs:125`)
   matches `contains_status(401)` → `ErrorClass::Auth` **before** any quota
   check, and no quota/usage class exists. Every downstream consumer therefore
   tells the user "key rejected — run `fono setup`" when the real fix is "top
   up credits or switch backends." This single bug poisons both STT and TTS
   notification copy.

2. **The MCP voice path never notifies — neither STT nor TTS.** The entire
   `fono-mcp-server` crate contains **zero** `critical_notify` calls.
   `fono.speak` turns the error into a `ToolCallResult::failure` string
   (`crates/fono-mcp-server/src/tools/speak.rs:91`); `speak_text` itself just
   `?`-propagates (`crates/fono-mcp-server/src/voice_io.rs:536`). The
   `listen_once` STT path (driving `fono.listen` / `fono.confirm`) likewise
   never notifies. In voice mode the agent is the only consumer, so the human
   gets **no desktop signal at all** — exactly what the user observed.

3. **No error-driven failover exists for either STT or TTS.** The only TTS
   "fallback", `EnglishOnlyFallback`
   (`crates/fono-tts/src/english_only_fallback.rs:41`), is a *language-
   capability* router — it inspects the text's language, never the synthesis
   error. On the STT side, the only resilience knob is
   `cloud_rerun_on_language_mismatch` (also language-driven). Neither has any
   notion of "primary errored → try a local engine." A quota failure aborts
   the call and (for STT) silently loses the transcription.

4. **The daemon STT path notifies but cannot recover.** The dictation pipeline
   does call `critical_notify::notify(Stage::Stt, …)`
   (`crates/fono/src/session.rs:3908`) — so on the daemon path the user at
   least gets a (mis-classified) popup — but the captured audio is dropped with
   no local fallback, so the dictation is simply lost.

## Assumptions

- Local failover targets are the existing on-device engines: `whisper-local`
  for STT, `tts-local` (Piper) for TTS. When the relevant feature is not
  compiled in (or the model/voice asset is absent), failover degrades to
  "notify only" and the original error is preserved — matching the existing
  `EnglishOnlyFallback` skip posture.
- Failover should be **automatic and transparent** (no new required config),
  consistent with the "no new config knobs" precedent. An opt-out may be
  layered on later.
- "Credits exhausted" is user-actionable and distinct from "key rejected" and
  deserves its own notification copy and dedup key.
- Notifications fired from the short-lived MCP process are acceptable
  (`notify::send` shells out to `notify-send` / `notify-rust`, process-
  agnostic). Per-process dedup/session state is independent and fine.
- A local STT failover transcribes the **already-captured** audio buffer; we
  do not re-record. (The daemon already holds the PCM; the MCP `listen_once`
  path holds the capture buffer.)

## Implementation Plan

### A. Shared classifier: recognise quota exhaustion (foundation)

- [ ] Task A1. Add `ErrorClass::QuotaExceeded` to
  `fono_core::critical_notify` (`crates/fono-core/src/critical_notify.rs:87`).
  Rationale: a valid key with no remaining credits is a distinct, actionable
  state; conflating it with `Auth` misdirects the user.

- [ ] Task A2. In `classify` (`crates/fono-core/src/critical_notify.rs:125`),
  detect quota exhaustion **before** the 401/403 → `Auth` branch and before
  the 402 → `PaymentRequired` branch. Match typed signals
  (`quota_exceeded`, `insufficient_quota`) and phrases (`exceeds your quota`,
  `credits remaining`, `credits are required`), case-insensitive. Rationale:
  the bare-status check must not win first, since ElevenLabs wraps quota in a
  401 and OpenAI wraps it in a 429.

- [ ] Task A3. Add `QuotaExceeded` arms to `render`
  (`crates/fono-core/src/critical_notify.rs:401`) for **every** stage that can
  hit it (`Stt`, `Tts`, `Polish`, `Assistant`). STT copy must convey that the
  dictation was transcribed locally (or lost, when no local engine), e.g.
  "Fono — STT quota exhausted ({provider}). Transcribed with the local model
  instead. Top up credits or switch backends in the tray." Mirror for TTS.

- [ ] Task A4. Add `QuotaExceeded` to the user-actionable set in
  `notify_actionable` (`crates/fono-core/src/critical_notify.rs:373`).

- [ ] Task A5. Unit tests: the exact ElevenLabs 401 `quota_exceeded` body
  classifies as `QuotaExceeded` (not `Auth`); an OpenAI 429 `insufficient_quota`
  body classifies as `QuotaExceeded` (not `RateLimit`); a generic 401 still
  maps to `Auth`; rendered bodies mention quota/credits and never say "update
  the key".

### B. TTS error-driven failover wrapper

- [ ] Task B1. Add a `FailoverTts` wrapper in `fono-tts` (new module mirroring
  `english_only_fallback.rs`'s lazy-build/cache/warn-once structure) holding
  the primary cloud backend plus a lazily-built local Piper engine.

- [ ] Task B2. In `FailoverTts::synthesize`, call the primary; on `Err`,
  classify with `critical_notify::classify`. For recoverable classes
  (`QuotaExceeded`, `Auth`, `RateLimit`, `Network`, `PaymentRequired`),
  lazily build/cache the local engine for the utterance's language and
  re-synthesize; return that audio on success. Genuine `Other`/parse errors
  still propagate.

- [ ] Task B3. On failover, fire `critical_notify::notify(Stage::Tts,
  primary.name(), class, err)` once per session.

- [ ] Task B4. `#[cfg(not(feature = "tts-local"))]` no-op constructor mirroring
  `maybe_wrap_english_only` (`crates/fono-tts/src/factory.rs:88`): still emit
  the notification, then propagate the original error.

- [ ] Task B5. Wire into `build_tts` (`crates/fono-tts/src/factory.rs:46`):
  wrap the primary (outermost, after the English-only wrap) only for cloud
  backends when `tts-local` is available. Single construction site covers
  every caller (`speak_text`, `speak --stream`, daemon assistant).

- [ ] Task B6. Factor the local-engine lazy-build/cache helper shared between
  `EnglishOnlyFallback` (`crates/fono-tts/src/english_only_fallback.rs:131`)
  and `FailoverTts` to avoid duplicated download/load/warn logic.

- [ ] Task B7. Unit tests: fake primary returning a quota error triggers the
  local path; fake primary returning `Other` propagates; warn/notify fires at
  most once. Use the unreachable-mirror trick
  (`crates/fono-tts/src/english_only_fallback.rs:258`) to stay offline.

### C. STT error-driven failover wrapper

- [ ] Task C1. Add a `FailoverStt` wrapper in `fono-stt` (mirroring B) holding
  the primary cloud `SpeechToText` plus a lazily-built local `WhisperLocal`
  engine. Rationale: a lost transcription is worse than degraded audio; local
  Whisper can transcribe the already-captured buffer.

- [ ] Task C2. In the wrapper's `transcribe`, on a recoverable-class error
  (same set as B2), run the local Whisper engine over the same audio buffer
  and return its transcript; fire `critical_notify::notify(Stage::Stt,
  primary.name(), class, err)` once. Propagate non-recoverable errors.

- [ ] Task C3. `#[cfg(not(feature = "whisper-local"))]` no-op path: notify and
  propagate the original error (the local model may also be absent even when
  the feature is compiled — surface the existing "run `fono models install`"
  hint in that branch's notification).

- [ ] Task C4. Wire into `build_stt` (`crates/fono-stt/src/factory.rs:91`):
  wrap cloud backends when `whisper-local` is available and a local model is
  resolvable. Decide and document the interaction with the existing
  `cloud_rerun_on_language_mismatch` rerun so the two resilience mechanisms
  compose rather than conflict.

- [ ] Task C5. Consider the streaming path (`build_streaming_stt`,
  `crates/fono-stt/src/factory.rs:490`): a mid-stream quota failure should at
  minimum notify; full streaming failover may be deferred to a follow-up
  (document the decision). Rationale: streaming recovery is materially harder
  and the batch path is the common case.

- [ ] Task C6. Unit tests mirroring B7 for the STT wrapper (fake primary
  quota-fails → local consulted; `Other` propagates; notify once).

### D. MCP voice path: notify on STT and TTS failure

- [ ] Task D1. In `speak_text` (`crates/fono-mcp-server/src/voice_io.rs:515`),
  when synthesis ultimately fails (after the B-wrapper), classify and
  `critical_notify::notify(Stage::Tts, backend_name, …)` before returning the
  error. This is the second half of the originally reported bug.

- [ ] Task D2. In `listen_once` (`crates/fono-mcp-server/src/voice_io.rs`),
  when STT ultimately fails, classify and
  `critical_notify::notify(Stage::Stt, backend_name, …)` before returning.
  Rationale: `fono.listen` / `fono.confirm` currently fail silently to the
  human on a quota-dead STT key.

- [ ] Task D3. Verify the other MCP voice callers — `fono.confirm`
  (`crates/fono-mcp-server/src/tools/confirm.rs:154`), `fono.listen`
  (`crates/fono-mcp-server/src/tools/listen.rs:156`), `fono.summarize`
  (`crates/fono-mcp-server/src/tools/summarize.rs:190`) — inherit the notify
  via the centralised `speak_text` / `listen_once` changes and do not swallow
  the error earlier.

- [ ] Task D4. Add a session-reset call appropriate to the short-lived MCP
  process (or rely on the 120 s auto-reset at
  `crates/fono-core/src/critical_notify.rs:41`) so persistent failures across
  separate invocations re-notify rather than being permanently suppressed.

### E. Documentation

- [ ] Task E1. Document the quota-vs-auth distinction and the automatic local
  failover (STT + TTS) in `docs/providers.md` (the ElevenLabs section already
  covers plan-gating / 402 at `crates/fono-tts/src/elevenlabs.rs:25`).

- [ ] Task E2. Update `docs/status.md` session log per the one-phase-at-a-time
  rule.

## Verification Criteria

- ElevenLabs `401 quota_exceeded` and OpenAI `429 insufficient_quota` both
  classify as `QuotaExceeded`; a generic invalid-key 401 still classifies as
  `Auth`.
- With local engines compiled in and assets present: a quota/auth/network
  error on the cloud backend yields a usable **local transcription** (STT) and
  audible **local playback** (TTS), each accompanied by exactly one desktop
  notification per session.
- Without the local feature/asset: the same errors still fire a notification
  and propagate the original error (no regression).
- `fono.speak`, `fono.listen`, `fono.confirm`, `fono.summarize` all surface a
  desktop notification on unrecoverable failure; the daemon dictation path's
  STT/TTS notifications now carry accurate quota copy.
- Pre-commit gate passes: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`.

## Potential Risks and Mitigations

1. **Provider-specific quota wire formats differ** (401 vs 429 vs 402; typed
   code vs prose). Mitigation: match typed codes *and* phrases (A2); add a
   regression test per known provider shape; default unknown shapes to the
   safest actionable class.

2. **Local STT/TTS asset missing at failover time** (feature compiled but no
   model/voice downloaded). Mitigation: the no-op branches (B4/C3) notify with
   the existing `fono models install` / voice-download remediation and
   propagate the original error rather than appearing to succeed.

3. **Failover masks a broken paid backend.** Mitigation: the mandatory
   one-shot notification (B3/C2/D1/D2) guarantees the degradation is reported
   even when local output succeeds.

4. **Local cold-start latency mid-utterance / mid-dictation.** Mitigation:
   lazy-build-and-cache; optional pre-warm after the first failure; document
   the one-time delay.

5. **Composition order bugs.** `FailoverTts` over `EnglishOnlyFallback`, and
   `FailoverStt` over `cloud_rerun`. Mitigation: define and unit-test a clear
   order (failover outermost so language routing/rerun happens on whichever
   engine ultimately runs).

6. **Notification spam across many short MCP calls** (fresh process each time).
   Mitigation: per-process session gate + 120 s auto-reset; tune D4 so
   persistent failures re-notify at a reasonable cadence, not every call.

7. **Streaming STT failover complexity.** Mitigation: scope C5 to notify-only
   for the streaming path in v1 and defer full streaming failover, documented
   as a known limitation.

## Alternative Approaches

1. **Config-driven explicit fallback chains** (`[stt].fallback_backend`,
   `[tts].fallback_backend`): user names a second backend per stage. More
   flexible/explicit but adds config surface and onboarding cost; contradicts
   the "no new config knobs" precedent. Could layer on later atop the wrappers.

2. **Notify-only, defer all failover** (ship A + D, skip B/C): smallest change
   that fixes the "I wasn't told" half of the bug. Lower value for voice-mode
   users who want dictation/replies to keep working, but a valid first
   increment if failover must wait.

3. **Per-backend retry-then-fail inside each client**: keeps logic near the
   wire format but duplicates failover across every backend and cannot reach a
   *different* engine. Rejected in favour of the shared wrappers (B/C).

4. **Proactive credit-balance polling** (warn before the quota hits zero):
   nicer UX but provider-specific, adds polling/auth surface, and doesn't
   address the failure-time recovery. Out of scope; note as a future idea.
