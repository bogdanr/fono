# Polish `local` backend must mean embedded llama.cpp, not Ollama

## Objective

Make the polish (LLM cleanup) `local` backend behave exactly like the
assistant: `PolishBackend::Local` always runs the embedded `llama-cpp-2`
engine on a local GGUF, and an Ollama / OpenAI-compatible **server** is
reached only when the user explicitly opts in. Eliminate the silent
misroute where the default local model (`gemma-4-e2b`) is shunted to
`http://localhost:11434` and fails invisibly, leaving dictation
un-cleaned.

The reference implementation is `crates/fono-assistant/src/factory.rs`
(`build_ollama` / `build_embedded_local` / `manual_local_server_endpoint`).

## Background (root cause, confirmed)

Two compounding defects:

1. **Factory misroute** — `crates/fono-polish/src/factory.rs:202-223`
   `build_local` calls `is_gemma_model(&cfg.local.model)`
   (`factory.rs:121-123`, substring match on `"gemma"`) and, when true,
   returns `build_gemma_local_server` — an `OpenAiCompat::ollama(...)`
   HTTP client. The default local model is
   `DEFAULT_POLISH_LOCAL_MODEL = "gemma-4-e2b"` (`config.rs:534`), so the
   default `local` backend **never** uses embedded llama.cpp.
2. **Wizard writes a stale Ollama block** —
   `crates/fono/src/wizard.rs:1718-1728` (`configure_local_llm`) sets
   `backend = Local` but also writes
   `polish.cloud = Some(PolishCloud { provider: "ollama", api_key_ref:
   "http://localhost:11434/v1/chat/completions", .. })`. The assistant's
   counterpart `enable_local_assistant_with_voice` (`wizard.rs:1028-1035`)
   sets `cloud = None`.

Net effect: local cleanup silently POSTs to a non-existent Ollama model,
404s, is classified `ErrorClass::Other` (no notification,
`critical_notify.rs:166`, `session.rs:3792-3804`), and the raw transcript
is injected (`session.rs:3812`).

## Implementation Plan

- [ ] Task 1. **Make `PolishBackend::Local` always build embedded
  llama.cpp.** In `crates/fono-polish/src/factory.rs`, rewrite
  `build_local` (the `#[cfg(feature = "llama-local")]` arm,
  `factory.rs:202-211`) to drop the `is_gemma_model` branch entirely and
  always resolve a GGUF path + construct `LlamaLocal::new(...)`. Mirror
  the assistant's `build_embedded_local` (`fono-assistant/factory.rs:220-234`):
  resolve `<polish_models_dir>/<model>.gguf`, check `path.exists()`, and
  on miss return a clear error pointing at `fono models install <model>`
  or choosing a cloud/Ollama backend. Rationale: a missing model must
  fail loudly with actionable guidance, not silently degrade.

- [ ] Task 2. **Fix the `not(llama-local)` arm to stop hijacking Gemma.**
  Rewrite the `#[cfg(all(not(feature = "llama-local"), feature =
  "openai-compat"))]` `build_local` (`factory.rs:213-223`) so it no
  longer special-cases Gemma into `build_gemma_local_server`. It should
  return the "rebuild with `llama-local` or pick a cloud/Ollama backend"
  error in all cases, matching the assistant's
  `#[cfg(not(feature = "llama-local"))] build_embedded_local`
  (`fono-assistant/factory.rs:236-241`).

- [ ] Task 3. **Route the manual server path through
  `PolishBackend::Ollama` only.** Keep `PolishBackend::Ollama =>
  build_oa_ollama(...)` (`factory.rs:83-91, 160-167`) as the *only* way
  to reach an HTTP Ollama / OpenAI-compatible server. Decide and document
  one of two equivalent shapes, preferring (a) for minimal churn:
  - (a) Treat `PolishBackend::Ollama` as the explicit "local server"
    backend (endpoint from `cloud.api_key_ref`, model from
    `cloud.model`/`local.model`). This is the polish analogue of the
    assistant choosing the server when explicitly configured.
  - (b) Mirror the assistant 1:1 by adding a
    `manual_local_server_endpoint(cfg)` helper that only honors
    `provider ∈ {"ollama-server", "openai-compatible-local"}` with an
    `http(s)://` `api_key_ref`, and have `build_local` defer to it before
    falling back to embedded — making `Ollama` redundant.
  Rationale: a stale `provider = "ollama"` block (as written by the old
  wizard) must NOT silently activate a server; it should be ignored in
  favor of embedded local. Add the equivalent of the assistant's
  `wizard_legacy_ollama_provider_ignores_stale_endpoint` guarantee.

- [ ] Task 4. **Delete `is_gemma_model` and `build_gemma_local_server`**
  (`factory.rs:121-123`, `factory.rs:225-230`) once Tasks 1–3 remove all
  call sites, plus the now-unused `local_openai_endpoint` /
  `local_openai_model` helpers (`factory.rs:105-119`) if no other caller
  remains. Rationale: removing the conflation at the source prevents
  regressions.

- [ ] Task 5. **Stop the wizard writing a stale Ollama cloud block for
  the Local choice.** In `crates/fono/src/wizard.rs` `configure_local_llm`
  (`wizard.rs:1718-1728`), set `config.polish.cloud = None` and remove the
  `PolishCloud { provider: "ollama", .. }` assignment, mirroring
  `enable_local_assistant_with_voice` (`wizard.rs:1028-1035`). Rationale:
  the wizard's local choice must produce an embedded-local config, not a
  server config.

- [ ] Task 6. **Migrate existing on-disk configs.** Add config-load
  normalization (in `fono-core` config load/migration, alongside the
  existing superseded-model handling near `config.rs:534-548`) so that a
  loaded config with `polish.backend = Local` and a stale
  `polish.cloud.provider = "ollama"` block has that block dropped (or
  ignored at build time per Task 3). Rationale: users who already ran the
  old wizard (e.g. the reporter's `config.toml:38-51`) must get embedded
  local without hand-editing.

- [ ] Task 7. **Verify the local polish model is actually downloadable /
  present.** Confirm `crate::models::ensure_models` (referenced from
  `wizard.rs:1048`) and the tray "switch to local" path fetch
  `gemma-4-e2b.gguf` into `paths.polish_models_dir()` so Task 1's
  existence check passes after a normal setup. Rationale: making `local`
  mean embedded is only useful if the GGUF lands on disk.

- [ ] Task 8. **Improve failure visibility (defense in depth).** Consider
  upgrading a polish "model not found / 404" from `ErrorClass::Other` to
  a user-visible class, OR ensure the embedded-missing error from Task 1
  surfaces via `critical_notify` at daemon start (`session.rs:635-642`
  currently only `warn!`s and downgrades to no-cleanup). Rationale: the
  original report was hard to diagnose precisely because the failure was
  silent.

- [ ] Task 9. **Tests.** Add unit tests in `fono-polish/src/factory.rs`
  mirroring the assistant suite (`fono-assistant/factory.rs:375-421`):
  (a) `local_polish_uses_embedded_model_by_default` — `backend = Local`,
  default gemma model, nonexistent models dir ⇒ `Err` (not an Ollama
  client); (b) `legacy_ollama_provider_ignored_for_local` — stale
  `provider = "ollama"` block ⇒ still embedded/`Err`, not a server client;
  (c) `explicit_ollama_server_still_builds` — the chosen manual-server
  shape from Task 3 ⇒ `Ok(Some)` without a model file. Add a wizard test
  asserting `configure_local_llm` leaves `polish.cloud == None`.

- [ ] Task 10. **Docs + changelog.** Update `docs/providers.md` /
  `docs/configuration.md` to state that `[polish].backend = "local"`
  means the embedded engine and that Ollama is manual-only, and add a
  `CHANGELOG.md` entry describing the fix and the config migration.

## Verification Criteria

- With `[polish] backend = "local"`, `model = "gemma-4-e2b"` and the GGUF
  present, a dictation turn shows `polish.*` spans
  (`fono-polish/src/llama_local.rs:328`) in the chrome trace and the
  injected text is cleaned.
- With the GGUF absent, daemon start surfaces a clear, actionable error
  (notification and/or log) naming `fono models install gemma-4-e2b`;
  cleanup is not silently skipped without explanation.
- A config carrying a stale `[polish.cloud] provider = "ollama"` block no
  longer causes any HTTP call to `localhost:11434` when `backend = local`.
- `PolishBackend::Ollama` (or the chosen manual-server marker) still
  reaches the HTTP server and works.
- `is_gemma_model` / `build_gemma_local_server` no longer exist.
- New factory + wizard tests pass; `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace --tests --lib` all pass.

## Potential Risks and Mitigations

1. **Users intentionally relying on the current Gemma→Ollama behavior.**
   Mitigation: preserve that capability behind the explicit
   `PolishBackend::Ollama` / manual-server marker (Task 3); document the
   migration in the changelog (Task 10).
2. **Config migration dropping a block a user hand-authored on purpose.**
   Mitigation: only normalize the specific legacy shape
   (`backend = local` + `provider = "ollama"`); leave `backend = ollama`
   configs untouched.
3. **Embedded local too slow on low-tier hardware** (llama_local.rs:7-13
   notes 7–20 s per cleanup). Mitigation: this is pre-existing wizard
   tiering behavior (`should_use_high_tier_local_polish`); no change here,
   but ensure the "faster hardware recommended" copy still applies.
4. **`llama-local` feature not compiled into the shipped binary.**
   Mitigation: Task 2's error message tells the user to rebuild or pick a
   cloud/Ollama backend; verify the default release build enables
   `llama-local` so embedded local works out of the box.

## Alternative Approaches

1. **Minimal patch (factory-only).** Just delete the `is_gemma_model`
   branch (Tasks 1–2, 4) and rely on the existing `PolishBackend::Ollama`
   variant for servers, leaving the wizard as-is. Trade-off: faster, but
   the wizard keeps writing a stale `cloud` block that does nothing —
   confusing, and the on-disk configs of existing users still carry it.
2. **Full assistant parity (recommended).** Tasks 1–10: factory fix +
   wizard fix + config migration + visibility + tests. Trade-off: more
   surface area, but eliminates the conflation end-to-end and prevents
   the silent-failure class entirely.
3. **Collapse `PolishBackend::Local` and `::Ollama` into one variant**
   like the assistant (which has only `Ollama`, defaulting to embedded).
   Trade-off: cleanest conceptual parity, but a breaking enum/config
   change requiring broader migration; higher risk than (2).
