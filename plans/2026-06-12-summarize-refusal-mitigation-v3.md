# Mitigate small-Gemma safety refusals + repetition loops in `fono summarize` (v3)

## Objective

`fono summarize` with the local `gemma-4-e2b` backend refuses on profane input and loops the refusal sentence until the 384-token cap (~13 s, 333 deltas). Fix it by porting the already-proven F7 polish fixes into the assistant backend, harden the prompt, add a fallback — **and make the decoding-level fixes structurally shared so the next model switch cannot reintroduce the bug in one backend but not the other.**

## Findings (carried from v2)

1. **Repetition guard exists in polish, not in the assistant.** `crates/fono-polish/src/llama_local.rs:65-79` documents the identical Gemma loop and the cure at `crates/fono-polish/src/llama_local.rs:233-236`: `chain_simple([penalties(128, 1.3, 0, 0), greedy()])`, with `accept()` on generated tokens only — deterministic, near-zero cost. The assistant uses bare greedy (`crates/fono-assistant/src/llama_local.rs:1509`).
2. **The assistant's stop detection is dead code on `gemma-4-e2b`** (primary cause of the loop). Per `crates/fono-polish/src/llama_local.rs:241-256`, this vocab's real turn markers are `<|turn>` (105) and `<turn|>` (106, control+eog); the standard `<start_of_turn>`/`<end_of_turn>` literals tokenize as plain text, so the assistant's `single_token` lookups (`crates/fono-assistant/src/llama_local.rs:1611-1617`) return `None` and `token_to_piece(special=false)` hides control tokens from the `STOP_MARKERS` scan. Polish's fix: stop on any `LlamaTokenAttr::Control` token.
3. **Templates are hand-rolled and selected by model-name substring** in BOTH backends (`crates/fono-assistant/src/llama_local.rs:1672-1677`; polish `build_prompt_split_for_model`, with per-model quirks like the Qwen `<think>` disable seed — see tests at `crates/fono-polish/src/llama_local.rs:1212-1238`).

## Generality assessment (v3 — "will this survive the next model switch?")

**Generalizes by construction (model-agnostic):**
- Control-attr stop — reads token attributes from whatever vocab is loaded; works for any GGUF.
- Repetition penalty over generated-only tokens — independent of template/vocab.
- Per-request token cap, prompt hardening, duplicate-collapse + metadata fallback — backend-agnostic (also covers cloud backends).

**Does NOT generalize as the code stands today — three structural gaps:**
- **G1: Duplicate decode cores.** Polish and assistant each own a sampler, stop logic, and decode loop. The present bug exists *precisely because* a polish fix wasn't ported. Fixing the assistant by copy-paste recreates the same trap for the next fix.
- **G2: Name-substring template dispatch.** A future model whose filename doesn't contain "gemma"/"qwen3.5" silently falls through to ChatML — wrong template, new failure modes, no warning.
- **G3: Hardcoded marker spellings.** `STOP_MARKERS` and the template literals assume standard spellings; gemma-4-e2b already proved a GGUF can ship non-standard control-token spellings undetected.

The plan therefore adds: a shared generation policy (closes G1), a load-time template/vocab diagnostic that warns loudly on mismatch (closes G3 detection and flags G2), and a documented follow-up for embedded-chat-template rendering (the full G2 fix, deferred for cache-invariant reasons).

## Implementation Plan

### Layer 0 — Port the proven polish fixes, but as SHARED code (primary fix + G1)

- [x] Task 1. Extract a shared "local generation policy" module (in `fono-core` next to `llama_backend`, or a small shared crate module): the sampler chain constructor (`penalties(PENALTY_LAST_N, PENALTY_REPEAT) + greedy`), the penalty constants, and a `should_stop(token, vocab_attrs)` predicate implementing the Control-attr stop with the textual `STOP_MARKERS` scan as secondary. Rationale: one definition both backends consume; the next decoding fix lands everywhere at once.
- [x] Task 2. Switch the assistant's `generate_from_prefilled_context` (`crates/fono-assistant/src/llama_local.rs:1498-1609`) to the shared policy, replacing bare greedy and the dead `single_token`/eos-only stop checks.
- [x] Task 3. Switch polish's `generate_from_prefilled` (`crates/fono-polish/src/llama_local.rs:223-236`) to the same shared policy (behaviour-preserving refactor; its semantics are the source of truth).
- [x] Task 4. Verify the assistant's prefix-cache interplay: the completed-turn checkpoint's canonical rendering picks the closer textually (`crates/fono-assistant/src/llama_local.rs:483-488`); with Control-attr stops on gemma-4-e2b confirm via the conversation replay bench that completed-turn checkpoints still match, adjusting the canonical closer if needed (graceful skip already exists at `crates/fono-assistant/src/llama_local.rs:429-439`).
- [x] Task 5. Re-run the prefix-cache replay benches; refresh any `outputs_match` fixtures changed by penalized-greedy (still deterministic).

### Layer 1 — Template/vocab mismatch detection (G2/G3 guard for every future model)

- [x] Task 6. Add a load-time template sanity check run once in `ensure_loaded` (both backends, shared helper): tokenize the markers the selected template will emit (`<start_of_turn>`, `<end_of_turn>`, `<|im_start|>`, `<|im_end|>`) against the loaded vocab and log a **prominent warning** when any does not resolve to a single control token — naming the model, the marker, and what it tokenized as. Rationale: the gemma-4-e2b anomaly stayed invisible until someone debugged a 13 s loop; the next model switch should surface it in the first log lines. Also emit a warning when template dispatch falls through to the ChatML default for an unrecognized model name.
- [x] Task 7. Document the follow-up (in the plan/status notes, not a new doc file): the fully general fix for template dispatch is rendering via the GGUF's embedded `tokenizer.chat_template` metadata. Deferred because the prompt-state cache's textual prefix/suffix split and pinned-base invariants (e.g. `crates/fono-polish/src/llama_local.rs:1250-1264`) are built on the hand-rolled templates; adopting embedded templates needs its own design pass to preserve cacheability.

### Layer 2 — Prompt hardening (cheap, model-independent)

- [x] Task 8. Extend `default_summarize_prompt()` (`crates/fono-core/src/config.rs:802`): neutral-relay framing; on profanity/hostility, do not refuse — characterize tone neutrally without repeating the offensive words.
- [x] Task 9. Unit-test the prompt directives (extend tests near `crates/fono-mcp-server/src/summarize.rs:320-333`).

### Layer 3 — Bounded cost + fallback (insurance, backend-agnostic)

- [x] Task 10. Optional per-request max-new-tokens on `AssistantContext` (clamped to `MAX_NEW_TOKENS`), ~96 for summarize; no effect on chat when unset.
- [x] Task 11. In `summarize_with` (`crates/fono-mcp-server/src/summarize.rs:232-236`): collapse consecutive duplicate sentences; when the collapsed reply is a bare refusal, return a deterministic metadata fallback built from `SummarizePayload`. Unit-test with the mock assistant.
- [x] Task 12. Re-run the repro (`echo '…' | fono summarize --sender bogdan --source test`): non-refusal summary or fallback, well under 5 s, no repeated sentences. Run the pre-commit gate (fmt, clippy, tests).

### Deferred / dropped

- Response-seed plumbing (v1): re-evaluate only if refusals persist after Layers 0+2.
- Streaming loop-breaker heuristic (v1): superseded by the shared Control-attr stop + penalty chain.
- Embedded-chat-template rendering (full G2 fix): deferred behind Task 7's documented design pass.

## Verification Criteria

- Profanity repro returns a single non-looping summary or the metadata fallback in well under 5 s; `llm.generate` shows a control/eos stop instead of `max_tokens`.
- Polish and assistant consume the SAME sampler/stop policy symbol — no copy-paste divergence remains (grep-level check).
- Loading gemma-4-e2b logs the marker-mismatch warning; loading a standard-vocab model logs nothing.
- Prefix-cache replay benches pass after fixture refresh; completed-turn checkpoints still match across turns.
- Chat (F8) and polish (F7) behaviour otherwise unchanged.

## Potential Risks and Mitigations

1. **Shared-policy refactor perturbs polish behaviour.** Mitigation: polish semantics are the source of truth; port assistant onto them, assert polish outputs unchanged via its existing tests/benches.
2. **Control-attr stop misfires on models emitting control tokens mid-reply.** Mitigation: polish has run this in production on the same model family; cover both template families in tests.
3. **Completed-turn cache checkpoints stop matching due to closer-spelling changes.** Mitigation: Task 4 verifies via the replay bench; graceful skip path already exists.
4. **Refusal persists (alignment, not decoding).** Mitigation: Layer 2 directives, Layer 3 fallback, response-seed as the next escalation.
5. **Warning fatigue from the load-time check on intentionally quirky models.** Mitigation: single warning per load, with actionable text; no behaviour change.

## Alternative Approaches

1. **Embedded chat-template rendering now**: fully general for any future model, but collides with the textual prefix/suffix cache invariants; kept as the documented follow-up (Task 7).
2. **Copy-paste the polish fixes into the assistant without sharing**: fastest, but recreates exactly the divergence that caused this bug; rejected.
3. **Textual loop-breaker / response seeding (v1)**: redundant or deferred as above.

## Execution notes (2026-06-12, all tasks done)

- Shared policy lives in `crates/fono-core/src/llama_gen.rs` (`generation_sampler`, `is_control_token`, `first_stop_marker`, `safe_stream_end`, `warn_on_template_vocab_mismatch`); both backends consume it (`crates/fono-assistant/src/llama_local.rs`, `crates/fono-polish/src/llama_local.rs`).
- Per-request cap: `AssistantContext.max_new_tokens` (clamped to the backend's 384 budget); summarize sets 96 (`crates/fono-mcp-server/src/summarize.rs`). Cloud backends ignore it.
- Dedupe + refusal fallback: `collapse_repeated_sentences` / `looks_like_refusal` / `metadata_fallback` in `crates/fono-mcp-server/src/summarize.rs`, unit-tested with the mock assistant.
- Repro result: gemma-4-e2b now answers the profane payload with one neutral sentence in ~3.7 s wall (incl. model load), stop via control token; the two marker-mismatch warnings fire at load as designed.
- **Task 7 follow-up (deferred design pass):** the fully general template fix is rendering prompts via the GGUF's embedded `tokenizer.chat_template` metadata instead of name-substring dispatch onto hand-rolled templates. Deferred because the prompt-state cache's textual prefix/suffix split and pinned-base invariants are built on the hand-rolled renderings; adopting embedded templates needs its own design pass to preserve cacheability (stable textual prefixes, completed-turn canonical closers). Until then, `warn_on_template_vocab_mismatch` is the tripwire that surfaces the next gemma-4-e2b-style anomaly at load time.
