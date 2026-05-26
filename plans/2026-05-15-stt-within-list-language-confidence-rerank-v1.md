# Within-Allow-List Language Confidence Reranking

## Objective

Stop cloud STT backends from silently accepting the wrong in-allow-list
language for an utterance. Today, when `general.languages = ["ro", "en"]`,
a misclassification between the two configured peers (e.g. Groq tagging
short English audio as `"romanian"`) is recorded as a successful detection
and the Romanian-prior transcription is returned verbatim. Extend the
post-validation lane to challenge low-confidence in-list detections by
reranking against the other configured peers, using the same
`avg_logprob`-based mechanism that already exists for out-of-list reruns.

## Scope

In scope:

- `crates/fono-stt/src/groq.rs` (batch `SpeechToText::transcribe`)
- `crates/fono-stt/src/groq_streaming.rs` (`StreamingStt` finalize lane)
- `crates/fono-stt/src/openai.rs` (batch `SpeechToText::transcribe`)
- `crates/fono-stt/src/openrouter.rs` (pre-call language picker — cache
  bias must not stick when fresh confidence disagrees)
- `crates/fono-core/src/config.rs` — new tunables in `[general]`
- `crates/fono-stt/src/lang.rs` and `crates/fono-stt/src/lang_cache.rs` —
  helper functions if any new shared logic emerges

Out of scope (this plan):

- Local whisper backend — already runs `lang_detect` masked by the
  allow-list (`whisper_local.rs:360-415`); the bug is cloud-only.
- Wyoming backend — relies on the remote server's `detect=` flag, no
  client-side ambiguity to resolve.
- Changing OS-locale detection or wizard behaviour.

## Background

`crates/fono-stt/src/groq.rs:404-433` is the canonical site of the bug:

```text
if selection.contains(&detected) {
    self.lang_cache.record(BACKEND_KEY, &detected);   // ← accepted blindly
} else if self.cloud_rerun_on_mismatch {
    /* verbose-mode per-peer rerank via pick_best_peer */
}
```

The same structure repeats verbatim in `groq_streaming.rs:511-572` and
`openai.rs:230-257`. `pick_best_peer` (`groq.rs:485-506`) already
implements confidence-based reranking — we just never call it on
in-list detections.

OpenRouter has a different shape: it forces a language **before** the
call from `lang_cache` (`openrouter.rs:127-138`). The OS-locale
bootstrap (`factory.rs:23-38`) seeds that cache with the first
in-allow-list code OS detection ranks (on a Romanian NimbleX box: `ro`,
per the test fixture at `locale.rs:1189-1223`). With no post-call
challenge, OpenRouter is permanently biased toward whichever language
the seed picked.

## Implementation Plan

- [ ] Task 1. Extend the language-related config surface in
      `crates/fono-core/src/config.rs`:
  - Add `general.cloud_language_rerank_mode` enum with variants
    `Off` / `OnMismatch` (current behaviour) / `OnLowConfidence`
    (new default) / `Always`. The legacy bool
    `cloud_rerun_on_language_mismatch` becomes a deprecated alias that
    maps `true → OnLowConfidence`, `false → Off` (one-release shim).
  - Add `general.cloud_language_confidence_threshold: f32` with a
    sensible default (start at `-0.5`, document in plan rationale
    section below).
  - Update `Default for General` accordingly and add round-trip
    serde tests covering the legacy bool alias.
  - Rationale: a single threshold knob exposes the cost/accuracy
    trade-off without forcing users to read the source. `OnLowConfidence`
    is the right default because steady-state cost is identical to
    today, but ambiguous utterances are now disambiguated.

- [ ] Task 2. Refactor `crates/fono-stt/src/groq.rs` post-validation
      lane:
  - Swap the first-pass call from `do_request` (plain JSON) to
    `do_request_verbose` for AllowList(≥2) selections so we get
    `mean_logprob` on the first pass. Keep plain JSON for Auto and
    Forced (no rerun lane there).
  - Introduce `decide_rerun(detected, mean_logprob, selection,
    cache_value, mode, threshold) -> RerunDecision` returning one of
    `Accept`, `RerankAllPeers`, `RerankOtherPeers(skip=detected)`.
  - `OnLowConfidence` triggers `RerankOtherPeers` when (a) detection
    is in-list AND `mean_logprob < threshold`, OR (b) detection is in-
    list but disagrees with cache and `mean_logprob < higher_threshold`
    (e.g. `-0.3`) — i.e. small confidence gap relative to a recent
    confirmed peer.
  - `Always` triggers `RerankAllPeers` unconditionally for AllowList(≥2).
  - On out-of-list detection, behaviour matches today
    (`RerankAllPeers` regardless of mode, except `Off` which accepts
    and logs).
  - Implement the new `pick_best_peer_excluding(wav, peers, skip)`
    that runs N−1 verbose requests in parallel via `futures::join_all`
    and compares against the first-pass score we already have.
  - The winning verbose response feeds the existing
    `filter_hallucinated_segments` path for the finalize text.
  - Cache writes: only record after a winner survives reranking
    (current behaviour for out-of-list reruns; extend uniformly).
  - Rationale: keeps all the existing safety nets, but adds the
    missing within-list confidence gate.

- [ ] Task 3. Mirror Task 2 in `crates/fono-stt/src/groq_streaming.rs`
      finalize lane (`groq_streaming.rs:501-585`):
  - Preview lane stays unchanged — it already suppresses overlay
    updates for out-of-list detections (`groq_streaming.rs:393-410`)
    and we don't want to multiply the request cost on every cadence
    tick. Preview consistency is restored once finalize emits the
    correct text.
  - Finalize: switch the same `selection.contains(&detected)` branch
    to call the shared `decide_rerun` and run peer reranks via the
    injected `verbose_fn` closure (parallel where possible — the
    closure is `Send + Sync`).
  - Tests: extend `finalize_drops_hallucinated_segment_via_verbose`-
    style fixtures to cover the new in-list rerank path (banned-
    detection coverage stays as-is).

- [ ] Task 4. Mirror Task 2 in `crates/fono-stt/src/openai.rs`
      (`openai.rs:213-271`):
  - Same refactor against the existing `VerboseResp::mean_logprob`
    helper (`openai.rs:200-209`).
  - OpenAI lacks the rate-limit notification path Groq has, so error
    handling is simpler — propagate failures of any single peer rerank
    as warnings, fall back to the first-pass response.

- [ ] Task 5. Fix the OpenRouter bias path in
      `crates/fono-stt/src/openrouter.rs:127-318`:
  - The pre-call `pick_language` reads the cache as the chosen
    language. That's correct steady-state, but combined with the OS-
    locale bootstrap it locks in the wrong language on first session.
  - Change behaviour: when `cloud_language_rerank_mode != Off` AND
    selection is `AllowList(≥2)`, send the first pass with **no**
    forced language (`pick_language → None` for AllowList) so the
    model auto-detects, then apply the same `decide_rerun` logic on
    the response. The cache remains useful as a tiebreak when peer
    rerun scores tie.
  - For providers that genuinely refuse to auto-detect (per
    `openrouter.rs` model capability table — verify before implementing),
    fall back to the existing cache-forced path with a `warn!` on
    daemon start naming the affected models.

- [ ] Task 6. Re-validate `factory.rs:23-38`
      (`bootstrap_language_cache`):
  - Keep the OS-locale seed (cheap warm-start signal) but document
    that it only biases tiebreaks under the new logic, not first-pass
    forcing for Groq/OpenAI. For OpenRouter, document that the seed
    is now consulted only for capability-restricted models per
    Task 5.

- [ ] Task 7. Add a `--language auto` per-utterance escape hatch
      surface review:
  - Confirm `LanguageSelection::with_override(Some("auto"))`
    (`lang.rs:136-142`) still collapses to `Auto`, bypassing the
    rerank lane entirely. Add a CLI test in
    `crates/fono/tests/pipeline.rs` exercising `fono record --language
    auto` against a stub backend to lock in that behaviour as a
    user-visible recovery knob.

- [ ] Task 8. Tracing + observability:
  - Replace the existing `tracing::info!("groq detected banned …")`
    with structured fields (`backend=`, `first_pass_lang=`,
    `first_pass_score=`, `rerank_decision=`, `winner=`,
    `winner_score=`). The current logs aren't enough to retroactively
    diagnose "why did this come out in Romanian" reports.
  - Add a per-session counter exposed via the tray "Diagnostics"
    sheet: `language_reranks_triggered`,
    `language_reranks_changed_pick`. Distinguishes "the rerank fixed
    it" from "the rerank ran but agreed".

- [ ] Task 9. Documentation refresh:
  - Update `docs/providers.md` (language section, if present) and the
    `[general]` table comments in any sample config to mention the
    new mode + threshold, with a one-paragraph explanation of the
    `Off / OnMismatch / OnLowConfidence / Always` trade-off.
  - Add an entry to `CHANGELOG.md` under the next unreleased section
    describing the fix and the new knob.
  - Add a brief ADR or amendment to
    `plans/2026-04-28-multi-language-stt-no-primary-v3.md` noting
    that v3.1's "banned-only" rerun was insufficient and pointing at
    this plan.

- [ ] Task 10. Test matrix:
  - Unit: `decide_rerun` truth table per mode × (in-list /
    out-of-list) × (above / below threshold) × (cache hit / miss).
  - Unit: `pick_best_peer_excluding` returns the higher-score peer,
    handles single-peer skip (no candidates → fall through).
  - Integration: scripted closures in `groq_streaming.rs` tests
    extended to simulate an in-list-but-low-confidence finalize and
    assert the second peer's text wins.
  - Equivalence harness (`fono-bench`): add a fixture pair —
    "English-clip-misdetected-as-RO" + reference English text —
    plumbed through `cloud-mock --provider groq` to gate regressions
    via the existing two-gate verdict path.

## Verification Criteria

- A scripted in-list misclassification (Groq returns `"romanian"` for
  an English clip while `general.languages = ["ro", "en"]` and
  `mean_logprob = -0.65`) triggers `pick_best_peer_excluding(skip=ro)`,
  recovers the English text, and records `"en"` in `lang_cache`.
- Steady-state cost on a confident in-list detection
  (`mean_logprob = -0.2`, default threshold `-0.5`) remains exactly one
  cloud request — no rerank fires.
- `Always` mode reliably issues N requests per finalize; visible via
  the new structured tracing fields.
- `Off` mode reproduces today's behaviour byte-for-byte (legacy
  alias path round-trips).
- OpenRouter no longer locks the first session to the OS-locale-
  seeded language: a `--language auto` override and a fresh English
  utterance produce English text on a Romanian-locale box.
- All existing tests in `crates/fono-stt/`, `crates/fono/tests/`,
  and `crates/fono-bench/` continue to pass; new tests above are
  green.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo deny check` clean (no new deps expected; `futures` already
  in tree for streaming).

## Potential Risks and Mitigations

1. **Doubled / N-tupled request cost on noisy free-tier accounts.**
   Mitigation: default mode is `OnLowConfidence`, not `Always`. The
   threshold is configurable. Rate-limit notifier
   (`crate::rate_limit_notify`) already triggers a 60 s suppression
   window; tie the new rerun path into the same guard so 429s don't
   cascade through every peer attempt.
2. **Threshold tuning is empirical.** Mitigation: ship a default
   that errs toward "rerank slightly too often" rather than "never";
   surface the new diagnostic counters (Task 8) so users / the bench
   harness can tune. Document the threshold semantics with example
   `avg_logprob` values per spoken-volume condition.
3. **Per-peer rerun text quality differs subtly** (`pick_best_peer`
   today returns the verbose response text, not the filtered text;
   `groq.rs:419` skips `filter_hallucinated_segments`). Mitigation:
   plumb the filter through the rerun winner uniformly. Already
   listed in Task 2.
4. **OpenRouter capability matrix isn't exhaustively documented.**
   Mitigation: Task 5 explicitly calls for verification before flipping
   the auto-detect default. If any model requires a forced language,
   leave that model on the cache-forced path with a warn-on-start.
5. **Whisper's language echo is sometimes absent** (`whisper-1`
   plain JSON drops the `language` field). Mitigation: AllowList(≥2)
   path already routes through verbose endpoints. Plain-JSON paths
   only run for Auto or Forced, where the rerun lane is moot.
6. **Latency regression on the streaming finalize.** Mitigation:
   parallelise peer reruns via `futures::join_all`. The added wall-
   clock cost is one extra round-trip, not N×.

## Alternative Approaches

1. **Always-rerank on AllowList(≥2), no confidence gate.** Simpler
   code, deterministic cost. Trade-off: doubles request cost in the
   common 2-peer case. Worth shipping as the `Always` mode (Task 1)
   for users who'd rather pay for maximal accuracy; not the right
   default.
2. **Add per-language initial prompts that explicitly say "do not
   translate; if the speech is English, transcribe as English"**.
   Cheap and works for some Whisper variants, but only the
   `prompt=` form field affects the *decoder* prior, not the
   language classifier. Empirically thin remedy on
   `whisper-large-v3-turbo`. Pair this with reranking as belt-and-
   braces, not as a replacement.
3. **VAD-driven length floor on AllowList responses.** Short clips
   (< 1.5 s) are the worst offenders for cross-language confusion.
   Auto-rerank only when audio duration is below a threshold.
   Cheaper than full confidence gating but more brittle — long
   accented English clips also misclassify.
4. **Client-side language model voting** (e.g. run cld3 / lingua-rs
   on the first-pass text and override the provider's language
   when the lexical detector strongly disagrees). Adds a dep,
   doesn't help when the speech is short or numeric. Defer.
5. **Default to Forced(en) when OS locale is mixed.** Sidesteps the
   bug entirely for the most common bilingual setup. Hostile to
   users who actually dictate in both languages — the exact
   scenario this plan exists to support.
