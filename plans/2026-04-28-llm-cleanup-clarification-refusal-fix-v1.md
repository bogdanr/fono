# LLM Cleanup Clarification-Refusal Fix

## Objective

Prevent the LLM cleanup phase from returning conversational clarification
replies (e.g. *"It seems like you're describing a situation, but the details
are incomplete. Could you provide the full text…"*) when the captured
transcript is short or ambiguous. The cleanup must always return either a
cleaned version of the input or the raw input verbatim — never a question,
apology, or meta-comment — regardless of whether the recording came from F8
(push-to-talk hold) or F9 (toggle).

## Background — verified findings

- The hotkey is **not** the cause. Both `HoldPressed` and `TogglePressed`
  funnel through the same `spawn_pipeline` → `TextFormatter::format` path
  at `crates/fono/src/session.rs:1213-1276`. F8 just tends to produce
  shorter recordings, so the model misbehaves more visibly there.
- The default system prompt at
  `crates/fono-core/src/config.rs:391-396` does not explicitly forbid
  clarification questions; it only asks the model to "Output only the
  cleaned text."
- The user message at `crates/fono-llm/src/openai_compat.rs:150-153`
  (and the Anthropic equivalent) is the raw transcript with no framing
  delimiters, so very short inputs read as conversational fragments.
- `Llm::skip_if_words_lt` defaults to `0` at
  `crates/fono-core/src/config.rs:322`, so even one-word captures are
  sent to the LLM despite the existing skip plumbing at
  `crates/fono/src/session.rs:1220-1232`.
- There is no post-call validator: whatever the LLM returns is
  written to the cursor at `crates/fono/src/session.rs:1278-1289`.

## Implementation Plan

- [x] Task 1. Harden the default system prompt in
  `crates/fono-core/src/config.rs` (`default_prompt_main`). Add explicit,
  non-negotiable directives: the user message *is* a transcript; never
  ask for clarification; never respond with questions, apologies, or
  meta-comments; if the input is empty, ambiguous, a single word, or
  already clean, echo it back verbatim with only filler/punctuation
  fixes. Keep the language short to avoid blowing the prompt budget on
  small models. Rationale: shifts behaviour at the source for every
  provider without per-backend code changes.

- [x] Task 2. Wrap the user-role message with a structural delimiter in
  every formatter implementation
  (`crates/fono-llm/src/openai_compat.rs:141-163`,
  `crates/fono-llm/src/anthropic.rs`, `crates/fono-llm/src/llama_local.rs`).
  Send the raw text inside a clearly labelled fenced block (e.g.
  `Transcript to clean (return ONLY the cleaned text, no quotes,
  no commentary):\n<<<\n{raw}\n>>>`). Do this in a single helper in
  `crates/fono-llm/src/traits.rs` (e.g. `FormatContext::user_prompt(raw)`)
  so all backends share identical framing. Rationale: gives the model an
  unambiguous syntactic signal that the user turn is data, not dialogue.

- [x] Task 3. Add a post-response refusal/clarification detector in
  `crates/fono-llm` (new private module, e.g. `refusal.rs`). Heuristic
  matches case-insensitive opening phrases such as "could you provide",
  "it seems like you", "the details are incomplete", "i'm not sure what",
  "please provide", "it looks like you", "i don't have enough", and
  similar. Apply in each `TextFormatter::format` impl after extracting
  the model output, before returning. On a positive hit, return an
  `Err` (or a sentinel) so the caller falls back to raw — re-using the
  existing fallback at `crates/fono/src/session.rs:1264-1273` which
  already drops `cleaned` to `None` on error, causing
  `final_text = raw` at line 1278. Rationale: belt-and-suspenders; even
  with a stronger prompt, some models will still occasionally refuse,
  and we already have a safe fallback path.

- [x] Task 4. Raise the default `skip_if_words_lt` to `3` (or `4`) in
  `crates/fono-core/src/config.rs:322`. Document the change in the field
  doc-comment and surface the rationale: "short captures (yes / no /
  send it / undo that) rarely need cleanup, save 150–800 ms, and avoid
  triggering clarification responses from chat-tuned models." Rationale:
  fixes the F8 push-to-talk experience without any model-side changes,
  and aligns with the existing L9 latency rationale already cited in
  the code comment.

- [x] Task 5. Add unit tests in `crates/fono-llm`:
  (a) refusal detector — table-driven cases covering the exemplar from
  the bug report plus 5–10 paraphrases, plus negative cases that look
  superficially similar but are valid cleaned transcripts (e.g. *"It
  seems like the meeting is at three."*). (b) framing helper — round-
  trips raw text through `user_prompt` and asserts the delimiters and
  raw payload are present and unmodified. Place near the existing test
  patterns in `crates/fono-llm/src/defaults.rs`.

- [x] Task 6. Add an integration-style test under
  `crates/fono/tests/` (mirror style of `provider_switching.rs`) that
  drives the pipeline with a stub `TextFormatter` whose `format` returns
  a clarification string, and asserts the injected text equals `raw`,
  not the clarification. Verifies the fallback wiring end-to-end.

- [x] Task 7. Update user-facing docs:
  - `docs/troubleshooting.md` — add an entry "LLM responds with a
    question instead of cleaning my text" pointing at the new defaults
    and the `skip_if_words_lt` knob.
  - `docs/providers.md` — note that hosted Llama-3.3-70B and other
    chat-tuned models occasionally refuse on very short inputs and
    that Fono now detects and discards such responses.
  - `CHANGELOG.md` — add a `Fixed` bullet under the unreleased section
    citing F8 push-to-talk false-clarification responses.

- [x] Task 8. Update `docs/status.md` session log with the fix summary
  and the new defaults, per the AGENTS.md hard rule about ending each
  session with a status update.

## Verification Criteria

- Unit tests for the refusal detector pass for the exact sentence in
  the bug report and its paraphrases, and *do not* fire on legitimate
  cleaned transcripts that happen to contain similar word fragments.
- Integration test confirms a stubbed clarification response is
  rejected and the raw transcript is what gets injected.
- Manual smoke test: pressing F8, saying "okay" or a single-word
  utterance, and releasing yields either the raw word at the cursor
  (because `skip_if_words_lt` skipped the LLM) or a cleaned single
  word — never a meta-question.
- Manual smoke test on Cerebras (Llama-3.3-70B), Groq, OpenAI
  (gpt-4o-mini), and Anthropic (Claude Haiku): five short utterances
  each ("yes", "send it", "no thanks", "undo that", "okay then") all
  produce transcript-shaped output, never clarification text.
- `cargo test -p fono-llm -p fono` green; clippy clean per
  `CONTRIBUTING.md`.
- All new Rust files start with the SPDX header per the AGENTS.md
  hard rule.

## Potential Risks and Mitigations

1. **Refusal detector false-positives on legitimate transcripts.**
   Some users may dictate sentences that begin with "It seems like…"
   or "Could you provide…" naturally.
   Mitigation: anchor patterns to sentence start *and* require a
   second tell-tale fragment ("the details are incomplete", "the full
   text", "more context", etc.); make the list narrowly-scoped; gate
   the detector behind a config flag (`llm.discard_clarifications =
   true` default) so power users can disable it; emit an
   `info!` log line whenever the detector fires so users can audit.

2. **Stricter prompt may cost a few extra tokens on every call,
   nudging latency up.**
   Mitigation: keep the additional prompt text under ~80 tokens;
   measure with `fono-bench` against the existing latency budget;
   if regression > 50 ms, move the strictest sentences into the
   `advanced` prompt slot which is already cleared when empty
   (`crates/fono/src/session.rs:1387-1389`).

3. **Bumping `skip_if_words_lt` to 3 changes existing user
   behaviour.**
   Mitigation: changelog entry under "Changed", not just "Fixed";
   leave the knob fully overridable in `config.toml`; document in
   `docs/providers.md` and the wizard hint at
   `crates/fono/src/wizard.rs`.

4. **Local llama backend (`crates/fono-llm/src/llama_local.rs`)
   might respond differently to the new framing than cloud chat
   models.**
   Mitigation: test against the supported local models (Qwen2.5
   tier-aware picker mentioned in `packaging/debian/changelog`); if a
   particular local model regresses, gate the framing helper behind
   a `formatter_kind` flag and keep raw-only input for that backend.

## Alternative Approaches

1. **Prompt-only fix (skip refusal detector + skip framing).**
   Cheapest change — only edit `default_prompt_main`. Trade-off:
   relies entirely on the model honouring the prompt; empirical
   evidence (this bug report) is that 70B-class chat models do not
   reliably honour soft "no commentary" instructions on short inputs.

2. **Skip-only fix (just raise `skip_if_words_lt`).**
   Smallest patch, fixes the F8 case immediately. Trade-off: longer
   ambiguous inputs (e.g. *"the response is this"*) still slip
   through; provides no protection on F9 toggles where the user
   genuinely wants cleanup of a short-but-real utterance.

3. **Switch defaults to instruction-tuned non-chat models** (e.g.
   `llama-3.3-70b-instruct` variants tuned for completion rather
   than chat). Trade-off: not all providers expose such variants;
   default-model curation is governed by
   `docs/decisions/0004-default-models.md` and would need a fresh
   ADR; doesn't help users on existing configurations.

4. **Two-shot self-check.** Send the raw text plus the model's
   first response back through the LLM with a verifier prompt
   ("does this look like a cleaned transcript?"). Trade-off: doubles
   latency and cost for every dictation; unacceptable per the L9
   latency budget. Not recommended.
