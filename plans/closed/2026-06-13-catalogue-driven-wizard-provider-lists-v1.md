# Catalogue-Driven Wizard Provider Lists

## Status: Completed

## Objective

Eliminate per-provider hand-wiring in the setup wizard. Today, adding a
cloud provider to `CLOUD_PROVIDERS` does **not** automatically surface it
in three places — they carry hard-coded lists or per-provider match arms:

1. `configure_cloud_stt` — hard-coded STT menu + index match
   (`crates/fono/src/wizard.rs:1735-1775`).
2. `configure_cloud_llm` — hard-coded LLM menu + index match
   (`crates/fono/src/wizard.rs:1779-1818`).
3. `validate_cloud_key` — hard-coded per-provider probe URL/auth
   (`crates/fono/src/wizard.rs:1860-1911`); missing arms produce the
   "no validation endpoint configured for X; key not validated" error.

After this change, a new provider added to the catalogue (with the new
`key_validation` metadata) surfaces in every list and validates its key
with **zero** wizard edits. This is the generalisation requested after
Speechmatics had to be wired into each list individually.

Scope decision (Option B): the **primary capability matrix**
(`pick_primary_cloud_provider`) stays intentionally filtered to
LLM-capable providers via `is_primary_candidate`
(`crates/fono/src/wizard.rs:187-198`). Speech-only providers like
Speechmatics remain reachable through the (now fully catalogue-driven)
Customize STT path, the secondary-STT picker, and the TTS picker —
they are deliberately NOT forced into the "one key fills everything"
matrix. Promoting speech-only providers into that matrix is a separate
UX change, documented under Alternative Approaches.

> **AMENDED (user override, 2026-06-13):** Option B was superseded.
> `is_primary_candidate` now qualifies any provider with **at least one
> wired capability** (STT, polish, assistant, or TTS), so the primary
> matrix lists every drivable cloud provider — including speech-only
> Speechmatics/Deepgram/AssemblyAI/Cartesia — while still excluding the
> unwired stubs (azure/google/nemotron STT, gemini polish) via the
> per-capability `is_*_wired` predicates. When a chosen primary lacks a
> capability, `apply_primary_provider` / `configure_cloud` lean on the
> local backend (local Whisper, embedded GGUF cleanup, local TTS) so the
> user always lands on a complete, runnable config. Assistant chat stays
> optional. This realises Alternative Approach 1 below.

## Implementation Plan

### Section A — Catalogue validation metadata (`fono-core`)

- [x] Add two public types to
      `crates/fono-core/src/provider_catalog.rs` (after the
      `TtsEndpoint` enum, before `struct CloudProvider`):
  - `enum KeyAuth { Bearer, Header(&'static str),
    HeaderPrefixed { header: &'static str, prefix: &'static str },
    QueryParam(&'static str) }` — derives
    `Debug, Clone, Copy, PartialEq, Eq`.
  - `struct KeyValidation { url: &'static str, auth: KeyAuth,
    extra_headers: &'static [(&'static str, &'static str)] }` — same
    derives. Doc-comment: GET `url` with key attached per `auth` plus
    `extra_headers`; HTTP 2xx ⇒ valid.
- [x] Add field `pub key_validation: Option<KeyValidation>` to
      `struct CloudProvider` (`crates/fono-core/src/provider_catalog.rs:149`).
      Rationale: `Option` so STT-only stubs (azure/google/nemotron)
      can opt out; doc-comment notes `None` ⇒ key saved unvalidated.
- [x] Populate `key_validation` on **every** one of the 13 entries in
      `CLOUD_PROVIDERS` (const struct literals require all fields). Map
      each from the current `validate_cloud_key` match arms:
  - `openai` → `Some(KeyValidation { url: "https://api.openai.com/v1/models", auth: Bearer, extra_headers: &[] })`
  - `groq` → `url: "https://api.groq.com/openai/v1/models", auth: Bearer, extra_headers: &[]`
  - `anthropic` → `url: "https://api.anthropic.com/v1/models", auth: Header("x-api-key"), extra_headers: &[("anthropic-version", "2023-06-01")]`
  - `cerebras` → `url: "https://api.cerebras.ai/v1/models", auth: Bearer, extra_headers: &[]`
  - `gemini` → `url: "https://generativelanguage.googleapis.com/v1beta/models", auth: QueryParam("key"), extra_headers: &[]`
  - `openrouter` → `url: "https://openrouter.ai/api/v1/auth/key", auth: Bearer, extra_headers: &[("HTTP-Referer", crate::openrouter_attribution::REFERER), ("X-OpenRouter-Title", crate::openrouter_attribution::TITLE), ("X-OpenRouter-Categories", crate::openrouter_attribution::CATEGORIES)]`
  - `deepgram` → `url: "https://api.deepgram.com/v1/projects", auth: HeaderPrefixed { header: "Authorization", prefix: "Token" }, extra_headers: &[]`
  - `assemblyai` → `url: "https://api.assemblyai.com/v2/transcript", auth: Header("Authorization"), extra_headers: &[]`
  - `cartesia` → `url: "https://api.cartesia.ai/voices", auth: Header("X-Api-Key"), extra_headers: &[("Cartesia-Version", "2026-03-01")]`
  - `speechmatics` → `url: "https://asr.api.speechmatics.com/v2/jobs?limit=1", auth: Bearer, extra_headers: &[]`
  - `azure`, `google`, `nemotron` → `None` (unwired stubs)
- [x] Re-export `KeyValidation` / `KeyAuth` if the catalogue module is
      glob-imported elsewhere; otherwise reference via
      `fono_core::provider_catalog::{KeyValidation, KeyAuth}`.
- [x] Add a unit test in the catalogue's `mod tests`: every entry that
      is a primary candidate or exposes STT/TTS has
      `key_validation.is_some()` (guards against a future provider
      regressing to the unvalidated path). Stubs (azure/google/nemotron)
      explicitly allowed `None`.

### Section B — Data-driven `validate_cloud_key` (`fono`)

- [x] Rewrite `validate_cloud_key`
      (`crates/fono/src/wizard.rs:1860-1911`) to look up the catalogue
      entry by env-var name (`catalogue_by_key_env(key_name)`,
      `crates/fono/src/wizard.rs:480`) and build the request from its
      `key_validation`:
  - `None` entry or `key_validation == None` ⇒ keep the existing
    `bail!("no validation endpoint configured for {key_name}; key not validated")`.
  - Build the URL: for `KeyAuth::QueryParam(p)` append
    `?{p}={key}` (or `&` if the URL already contains `?`); otherwise
    use `url` verbatim.
  - Attach auth: `Bearer` → `.bearer_auth(key)`; `Header(h)` →
    `.header(h, key)`; `HeaderPrefixed { header, prefix }` →
    `.header(header, format!("{prefix} {key}"))`; `QueryParam` → no
    header (already in URL).
  - Loop `extra_headers` and attach each.
  - Keep the existing 5 s timeout, user-agent, and the downstream
    2xx/non-2xx handling unchanged (`crates/fono/src/wizard.rs:1912`+).
- [x] Delete the now-dead per-provider `match key_name { … }` block.

### Section C — Catalogue-driven `configure_cloud_stt` (`fono`)

- [x] Replace the hard-coded `stt_providers` array + index `match`
      (`crates/fono/src/wizard.rs:1740-1758`) with an enumeration of
      `CLOUD_PROVIDERS` filtered by
      `p.stt.is_some() && parse_stt_backend(p.id).is_some()` (mirror the
      already-catalogue-driven `offer_secondary_stt`,
      `crates/fono/src/wizard.rs:1195-1248`).
  - Build labels as `"{display_name} ({model})"` from
    `p.stt.unwrap().model`; append `" — recommended"` for `p.id == "groq"`.
  - Default cursor = position of `groq`, else 0 (preserves the current
    "Groq fastest, recommended" default).
  - After selection: `prompt_or_reuse_key(theme, secrets, entry.key_env, entry.display_name, entry.console_url)`,
    then set `config.stt.backend = parse_stt_backend(entry.id)` and
    `config.stt.cloud = Some(SttCloud { provider: entry.id.into(),
    api_key_ref: entry.key_env.into(), model: entry.stt.unwrap().model.into() })`.
    Use `entry.id` for `provider` (cleaner than the old
    `trim_end_matches("_API_KEY").to_lowercase()`).
- [x] Verify the streaming-auto-on comment block
      (`crates/fono/src/wizard.rs:1765-1768`) is preserved.

### Section D — Catalogue-driven `configure_cloud_llm` (`fono`)

- [x] Replace the hard-coded `llm_providers` array + index `match`
      (`crates/fono/src/wizard.rs:1784-1807`) with an enumeration of
      `CLOUD_PROVIDERS` filtered by `is_primary_candidate(p)` (polish
      present + `parse_polish_backend` round-trips + not gemini).
  - Build labels as `"{display_name} ({polish model})"` from
    `p.polish.unwrap().model`; append `" — recommended"` for
    `p.id == "cerebras"`.
  - Append a final "Skip polish" entry; selecting it sets
    `config.polish.backend = None`, `config.polish.enabled = false`.
  - Default cursor = position of `cerebras`, else 0.
  - After selection:
    `prompt_or_reuse_key(theme, secrets, entry.key_env, …)`, then
    `config.polish.backend = parse_polish_backend(entry.id)`,
    `config.polish.enabled = true`,
    `config.polish.cloud = Some(PolishCloud { provider: entry.id.into(),
    api_key_ref: entry.key_env.into(), model: entry.polish.unwrap().model.into() })`.
- [x] Confirm both call sites still compile:
      `crates/fono/src/wizard.rs:1002`, `:1307` (Customize and the
      mixed-mode path).

### Section E — Docs + verification

- [x] Note in `docs/providers.md` (near the provider tables) that the
      wizard's STT/LLM pickers and key validation are now driven entirely
      by the capability catalogue — a new provider needs only a
      `CLOUD_PROVIDERS` entry (with `key_validation`).
- [x] Add a short `## [Unreleased]` bullet to `CHANGELOG.md` under the
      existing Speechmatics entry: wizard provider lists + key validation
      are now catalogue-driven.
- [x] Run the AGENTS pre-commit gate in order: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --tests --lib`.

## Verification Criteria

- Adding a hypothetical provider to `CLOUD_PROVIDERS` (STT + polish +
  `key_validation`) makes it appear in the Customize STT picker, the LLM
  picker, the secondary-STT picker, and the TTS picker, and validates its
  key — with no edits to `wizard.rs`.
- Entering a valid Speechmatics key in the wizard prints `OK` (not the
  "no validation endpoint configured" failure); an invalid key fails.
- `cargo clippy --workspace --all-targets -- -D warnings` exits 0
  (in particular the new `match KeyAuth` is exhaustive).
- `cargo test --workspace --tests --lib` passes, including the new
  catalogue `key_validation` coverage test.
- The primary matrix still renders aligned and still excludes
  speech-only providers (`primary_picker_renders_aligned_table` test
  unchanged and green).

## Potential Risks and Mitigations

1. **Const slice referencing `openrouter_attribution` constants.**
   `extra_headers: &[("HTTP-Referer", crate::openrouter_attribution::REFERER), …]`
   must resolve in a `const` context.
   Mitigation: those are `pub const &str`
   (`crates/fono-core/src/openrouter_attribution.rs:31,34,40`), legal in
   a const slice literal; verified during compile.
2. **Adding a struct field breaks every const literal.**
   `CloudProvider` has 13 positional entries.
   Mitigation: Section A enumerates all 13; the compiler errors on any
   omission, so a missed entry cannot ship.
3. **Default-cursor regression in the STT/LLM pickers.**
   Catalogue order differs from the old hand-ordered menus.
   Mitigation: explicit `position(|p| p.id == "groq"/"cerebras")` default
   preserves the previous "recommended first" cursor.
4. **QueryParam URL already containing `?`.**
   Speechmatics' validation URL has `?limit=1` but uses `Bearer`, not
   QueryParam, so no conflict; Gemini's QueryParam URL has no `?`.
   Mitigation: build URL with `?` vs `&` based on whether `url` already
   contains `?`, so the helper is correct for future entries too.
5. **Provider-id vs key-env mismatch.**
   `configure_cloud_stt` previously derived `provider` from the env-var
   name; switching to `entry.id` could change persisted config strings.
   Mitigation: `entry.id` is the canonical lower-case id that
   `parse_stt_backend`/`stt_backend_str` already round-trip — it is the
   correct value; covered by existing catalogue round-trip tests.
6. **Speechmatics validation endpoint behaviour.**
   `GET /v2/jobs` must return 401 (not 200) for an invalid key.
   Mitigation: standard Speechmatics batch-API auth behaviour; the wizard
   already offers "save anyway" on validation failure so a false-negative
   never blocks setup.

## Alternative Approaches

1. **Promote speech-only providers into the primary matrix.** Relax
   `is_primary_candidate` to include STT/TTS-only providers and have the
   wizard continue to the LLM step when the chosen primary has no polish.
   Trade-off: makes the matrix a complete catalogue view (Speechmatics
   visible up front) but changes the "one key fills everything" contract
   and needs careful handling of the no-LLM follow-up flow. Deferred as a
   separate decision.
2. **Keep validation per-provider but move only the STT/LLM lists to the
   catalogue.** Smaller diff, but leaves the "no validation endpoint
   configured" bug class alive for every future provider — rejected
   because it doesn't meet the "don't wire each one individually" goal.
3. **Encode validation as a closure table keyed by env var in `fono`
   instead of catalogue metadata.** Keeps `fono-core` free of HTTP
   concepts, but re-introduces a hand-maintained per-provider table in
   the wizard — exactly what this plan removes. Rejected.
