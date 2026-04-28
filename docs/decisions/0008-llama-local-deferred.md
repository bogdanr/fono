# ADR 0008 — `LlamaLocal` deferred (then unblocked)

## Status

Reconstructed (original lost in filter-branch rewrite; rationale
recovered from `docs/status.md:741` and plan history, 2026-04-28).

## Context

The H-plan (`docs/plans/2026-04-25-fono-local-default-v1.md`) wanted
local LLM cleanup via `llama-cpp-2` for parity with local STT. The
v0.1 timeline allowed for `whisper-rs` integration but the
`llama-cpp-2` 0.1.x API exposed a low-level surface that needed several
hundred lines of safe-wrapper code. Shipping a stub
(`Err("not yet wired")`) avoided blocking the v0.1 release on what was
effectively a separate feature.

## Decision

Defer `LlamaLocal` to v0.2. The v0.1 slice ships local STT
(`WhisperLocal`) with cloud-only LLM cleanup. The wizard's local-LLM
path is gated behind the `llama-local` feature flag. Cloud LLM
backends (`OpenAiCompat`, `Anthropic`, Cerebras-via-OpenAI-compat)
remain the default LLM cleanup option for both cloud and mixed
pipelines.

## Consequences

- v0.1 ships in time with a documented gap rather than a half-working
  local LLM.
- Users who want local LLM cleanup build with `--features llama-local`
  starting in v0.1.x; binary releases stay cloud-LLM-only until v0.2.
- This decision was effectively unblocked in v0.2.0
  (commit cluster including the H8 work documented in
  `docs/status.md:311-343`) where `LlamaLocal` ships honest GGUF
  inference. Default builds since v0.2.0 include `llama-local` in the
  default feature set, contingent on the ggml link trick (ADR 0018)
  resolving the symbol collision with `whisper-rs`.
- This ADR is **not** marked superseded: it documents the v0.1-era
  tradeoff. The "local LLM is now first-class" outcome is captured in
  `docs/status.md`'s "Single-binary local STT + local LLM" section and
  in ADR 0018.
