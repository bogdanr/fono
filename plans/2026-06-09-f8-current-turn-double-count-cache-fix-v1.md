# F8 prompt-cache: current-turn double-count defeats prefix reuse

*Created 2026-06-09. Investigation complete; fix not yet implemented.*

## TL;DR

The F8 assistant prompt-state cache restores only the static 78-token
`f8_system` base every turn and never a prior turn's `F8ChatPrefix`
checkpoint, so prefill (and TTFB) still grows linearly with conversation
length. **Root cause is now confirmed empirically and it is NOT tokenizer
boundary instability** (that hypothesis was tested and disproven). The cause
is structural: the current user turn is represented twice, which makes each
turn's cached prefix end in a trailing `<start_of_turn>user\n` marker that the
next turn overwrites with `<start_of_turn>model\n{reply}`. Consecutive cached
prefixes therefore diverge and no checkpoint is ever a token-prefix of the
next.

## Evidence

A throwaway probe (since removed) loaded the real Gemma tokenizer
(`ggml-vocab-gemma-4.gguf`, vocab-only) and replicated the exact live
store/lookup comparison. Two runs:

1. **Clean append-only history** (current turn NOT in history):
   `prefix_tokens(turn N).starts_with(stored prefix turn N-1)` = **true** for
   every turn. Tokenization is append-stable. Boundary-merge hypothesis
   disproven.

2. **Live daemon flow replicated** (current user turn pushed into history
   before snapshot, per `crates/fono/src/assistant.rs:306-307`, AND passed as
   `user_text`): nesting = **false** every turn. First divergence is always at
   the same place — the stored prefix ends with tokens `[2364, 107]`
   (`user\n`), while the next turn has `[4368, 107, …]` (`model\n{reply}…`) at
   that index. i.e. the current turn's trailing `<start_of_turn>user\n` marker
   becomes `<start_of_turn>model\n` + the reply once it scrolls into history.

Run-2 reproduces the live trace exactly (only `f8_system` (78) matches; prefill
= prefix_len − 78 every turn).

## Mechanism

- `crates/fono/src/assistant.rs:306` pushes the current user turn into
  `ConversationHistory` and then snapshots (`:307`), so `ctx.history`'s **last
  entry is the current user turn**.
- `reply_stream(&user_text, &ctx)` (`assistant.rs:353`) is also handed the same
  text as `user_text`.
- `build_gemma_prompt_split` (`crates/fono-assistant/src/llama_local.rs:1583`)
  iterates **all** of `ctx.history` (emitting the current user turn as a
  completed `…<end_of_turn>\n` turn) and then appends the trailing
  `<start_of_turn>user\n` marker (`:1614`); the suffix re-emits `user_text`.
  Net effect: the current user message appears **twice** in the full prompt,
  and the cached prefix's tail is an unstable `<start_of_turn>user\n` marker.
- `F8ChatPrefix` is stored with these prefix tokens
  (`llama_local.rs:447-450`); next turn `find_longest_prefix`
  (`fono-core/src/prompt_cache.rs:294`) rejects it because the stored tail
  isn't a prefix of the new prompt, and falls back to `f8_system`.

### Secondary correctness concern

Because the current user message is in both `ctx.history` and `user_text`, the
rendered prompt contains the user's message **twice**. Confirm whether this is
intended (it almost certainly is not) — it inflates the prompt and may degrade
reply quality. Cloud backends that build messages from `ctx.history` may or may
not double-count depending on whether they also append `user_text`; audit
`openai_compat_chat.rs` / `anthropic_chat.rs`.

## Fix options

The cache prefix must end at a point the next turn reproduces verbatim. Pick
one:

- **Option A (recommended) — snapshot history *without* the current turn; let
  `user_text` be the sole current-turn source.** Change `assistant.rs` so the
  snapshot used to build `ctx` excludes the just-pushed user turn (e.g. snapshot
  before pushing, or drop the last entry for the prompt build), keeping the push
  for persistence into the *next* turn's history. Then:
  - prefix = system + completed turns + `<start_of_turn>user\n`
  - suffix = `user_text` + `<end_of_turn>\n<start_of_turn>model\n`

  This removes the double-count. But note the cached prefix still ends in the
  *current* turn's trailing user marker — which is fine for the **base**
  restore, but for prior-turn `F8ChatPrefix` checkpoints to nest, the checkpoint
  must be stored at a point that survives. See Option C.

- **Option B — make `build_*_prompt_split` current-turn-aware.** If the last
  history entry equals the current user turn, treat it as the current turn (do
  not emit it in the prefix loop). Equivalent prompt to Option A but localized
  to the prompt builder; riskier (relies on last-entry identity).

- **Option C (the durable multi-turn win) — checkpoint the *completed* turn
  after generation.** For prefill to stay flat across turns, the cache must
  store the KV state through the end of each completed exchange (system + all
  completed user/assistant turns, ending at `<end_of_turn>\n`), so turn N+1
  restores turn N's completed state and prefills only the new user turn +
  trailing marker. This is a larger change (store state post-generation, key it
  on the completed-history prefix) but is the only option that makes TTFB
  independent of conversation length. Options A/B alone fix the double-count and
  let the *base* restore cleanly, but prior-turn checkpoints still won't nest
  because each turn's stored prefix ends in the volatile current-turn marker.

**Recommendation:** do Option A (correctness: kill the double-count) **and**
Option C (performance: flat prefill). Option A is small and independently
valuable; Option C delivers the actual waterfall win the user is chasing.

## Tasks

- [ ] **A1.** Fix the double-count in `crates/fono/src/assistant.rs` so the
      prompt build sees history *excluding* the current user turn (snapshot
      before push, or build from `history[..len-1]`). Preserve the push so the
      turn persists for the next snapshot.
- [ ] **A2.** Audit cloud backends (`openai_compat_chat.rs`,
      `anthropic_chat.rs`) for the same double-count; fix if present.
- [ ] **A3.** Add a **model-free** regression test (string level) in
      `fono-assistant`: simulate the live push-then-snapshot flow for 3 turns and
      assert `prefix_{N+1}.starts_with(prefix_N)` (the property that was false
      and is the cache-reuse invariant). This needs no GGUF — the divergence is
      structural (`user\n` vs `model\n`).
- [ ] **C1.** (Performance) Checkpoint completed-turn KV state post-generation,
      keyed on the completed-history prefix, and restore it via
      `find_longest_prefix` on the next turn. Add a multi-turn test asserting the
      restored token count grows turn-over-turn (flat suffix prefill).
- [ ] **V.** Re-record `FONO_ASSISTANT_TRACE` traces for 3+ consecutive
      assistant turns; confirm `llm.prompt_cache_restored` shows a growing
      `matched_tokens` (not a flat 78) and `llm_ttfb_ms` stops growing with
      history.
- [ ] **D.** Update `docs/status.md` with the corrected root cause (supersedes
      the 2026-06-09 "framing fix" entry, which addressed only the base and not
      this double-count).

## Gate

Run before commit, in order: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --features llama-local -- -D warnings`,
`cargo test --workspace --tests --lib`. Do not push unless the user says to.
