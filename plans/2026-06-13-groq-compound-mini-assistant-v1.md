# Default Groq Assistant to compound-mini (with always-on web search)

## Objective

Switch Groq's **assistant** chat default from `openai/gpt-oss-120b` to the
agentic `groq/compound-mini` system so the assistant gains built-in,
server-side web search. Polish stays on `openai/gpt-oss-20b` (deterministic
dictation cleanup must not trigger agentic tool use). Vision stays off
(compound is text-in, and the Llama vision route is license-blocked).

Scope decision (user-approved, 2026-06-13): **assistant only**.

## Background / verified facts

- `groq/compound-mini` is a hosted agentic system (Llama 3.3 70B + GPT-OSS
  120B), one tool per request, ~3x lower latency than `groq/compound`, with
  built-in web search + code execution. Invoked as a drop-in model id on
  `/chat/completions`; search is automatic (no `tools` param). Source: Groq
  live docs `console.groq.com/docs/compound/systems/compound-mini`.
- `WebSearchSupport::Always` already exists and is handled where it matters:
  - assistant factory injects a tool ONLY for `NativeTool(id)`
    (`crates/fono-assistant/src/factory.rs:103`); `Always` correctly injects
    nothing — compound-mini searches on its own.
  - wizard renders `Always` as "web search (always grounded)"
    (`crates/fono/src/wizard.rs:697`) and treats it as search-capable
    (`crates/fono/src/wizard.rs:234`).
- No exhaustive-match breakage: `Always` is a pre-existing variant.
- Licensing: compound-mini is partly Llama-powered. AGENTS.md forbids
  defaulting to Llama *weights*; this is a hosted API, not shipped weights,
  so it is outside that rule's intent. User accepted this trade-off.

## Implementation Plan

- [ ] Task 1. In the Groq `AssistantDefaults` (`crates/fono-core/src/provider_catalog.rs:268-285`),
      change `text_model` from `"openai/gpt-oss-120b"` to `"groq/compound-mini"`.
      Rationale: this is the entire search mechanism — search rides on the model id.
- [ ] Task 2. Change `web_search` from `WebSearchSupport::None` to
      `WebSearchSupport::Always`. Rationale: makes the wizard/matrix Search
      column reflect the now-real capability; the factory needs no change.
- [ ] Task 3. Add `Badge::Search` to the Groq assistant `badges` array
      (currently `&[Badge::Stt, Badge::Polish, Badge::Assistant, Badge::Tts, Badge::Fast]`).
      Rationale: keep the badge list consistent with the new capability.
- [ ] Task 4. Replace the search-via-model-swap TODO comment
      (`crates/fono-core/src/provider_catalog.rs:279-282`) with a short note
      recording the decision: compound-mini chosen for assistant; hosted-API
      Llama component judged acceptable vs the Llama-weights default rule;
      polish deliberately left on gpt-oss-20b. Leave the multimodal=None
      comment intact.
- [ ] Task 5. Leave `polish` (`openai/gpt-oss-20b`) and `multimodal_model`
      (`None`) UNCHANGED. Rationale: in-scope guardrails from the decision.
- [ ] Task 6. Update `docs/providers.md` Groq row/section: capability matrix
      Search cell for Groq → yes; note assistant uses compound-mini with
      always-on web search; polish unchanged.
- [ ] Task 7. Add a `CHANGELOG.md` `## [Unreleased]` → **Changed** entry:
      Groq assistant now defaults to `groq/compound-mini` with built-in web search.
- [ ] Task 8. Check catalogue/wizard tests for an assertion pinning Groq's
      assistant model id or "no search providers besides anthropic/openai"
      assumptions; update any that now expect Groq search.

## Verification Criteria

- `cargo fmt --all -- --check` exits 0.
- `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
- `cargo test --workspace --tests --lib` passes.
- Wizard primary matrix shows Groq with a Search capability; selecting Groq as
  the assistant provider yields a config whose assistant text_model is
  `groq/compound-mini`.
- Groq polish still resolves to `openai/gpt-oss-20b`.

## Potential Risks and Mitigations

1. **Agentic latency / unexpected tool use on every assistant turn.**
   Mitigation: documented as "always grounded"; scope limited to assistant
   (not polish). compound-mini is the low-latency variant (~3x faster than
   full compound).
2. **Licensing perception (Llama-powered default).**
   Mitigation: inline comment records the hosted-API-vs-shipped-weights
   distinction and the explicit user decision; consider promoting to an ADR
   note in docs/decisions/0024 if reviewers want a formal record.
3. **Model id with slash (`groq/compound-mini`) on OpenAI-compat path.**
   Mitigation: Groq's own examples use this exact id on
   `/openai/v1/chat/completions`; no client change required.
4. **A test pins the old gpt-oss-120b assistant id.**
   Mitigation: Task 8 audits and updates tests.

## Alternative Approaches

1. Keep `gpt-oss-120b` and wire search as an opt-in model swap only when the
   user enables web search — preserves the non-agentic default but adds
   conditional model-swap logic in the assistant factory (more code, the
   original deferred design in docs/decisions/0024).
2. Use full `groq/compound` instead of `-mini` — multi-tool per request and
   stronger search, at higher latency; worse fit for an interactive assistant.
