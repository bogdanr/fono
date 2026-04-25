# Fono — Easy Provider Switching (local ↔ cloud, cloud ↔ cloud)

Plan persisted 2026-04-25. Execution order is the "Recommended Sequencing" at
the bottom; check off boxes as tasks land.

## Objective

- `fono use stt groq` flips active STT live (no daemon restart).
- `fono use cloud cerebras` flips both STT + LLM to a paired preset.
- `fono use local` flips back to whisper-local (LLM cleanup off).
- API keys for many providers coexist in `secrets.toml`.
- `fono record --stt openai --llm anthropic` overrides for one call only.
- (v0.2) Tray menu shows current STT/LLM and lets user switch with a click.

## Tasks

### Schema + factory smarts (foundation)

* [x] **S1.** Make the `cloud` sub-block optional in factories: when missing,
  fall through to canonical env-var name (`GROQ_API_KEY`) + default model.
* [x] **S2.** Add `ProviderKey` registry in `fono-core::providers` mapping each
  `SttBackend` / `LlmBackend` to its canonical env-var name. One source of
  truth for factories, doctor, wizard, `fono use`, and `fono keys`.
* [x] **S3.** Allow `stt.backend = "groq"` / `llm.backend = "cerebras"` with
  no other fields to produce a fully working pipeline as long as the matching
  key is present.

### CLI — quick switching

* [x] **S4.** New `fono use` subcommand tree:
  - `fono use stt <backend>`
  - `fono use llm <backend>`
  - `fono use cloud <provider>` (paired STT+LLM preset)
  - `fono use local`
  - `fono use show`
* [x] **S5.** `fono use` writes via a helper that preserves all unrelated
  config fields and atomically rewrites the file.
* [x] **S6.** Per-call overrides: `fono record --stt … --llm …` and
  `fono transcribe --stt … --llm …`.

### Multi-key management

* [x] **S7.** New `fono keys` subcommand: `list / add / remove / check`.
* [~] **S8.** Wizard multi-key step — deferred to v0.2 (S7 already lets users
  add keys post-wizard with one command).

### Profiles (deferred to v0.2)

* [~] **S9.** `[profiles.<name>]` tables + `fono profile` subcommand.
* [~] **S10.** Optional `cycle_profile` hotkey.

### Hot-reload (no daemon restart)

* [x] **S11.** New IPC variant `Request::Reload` re-reads config + secrets
  and rebuilds STT/LLM in-place.
* [x] **S12.** Orchestrator holds STT and LLM as `RwLock<Arc<dyn _>>`;
  in-flight pipelines clone the `Arc` once at task spawn.
* [x] **S13.** Reload handler re-runs `prewarm()` on the freshly built
  backends so the first dictation after a switch isn't cold.
* [~] **S14.** Auto-reload on config-file change — deferred to v0.2.

### Tray menu (deferred to v0.2)

* [~] **S15.** `TrayAction::UseStt / UseLlm / UseProfile`.
* [~] **S16.** Daemon dispatches tray actions through the same handler.
* [~] **S17.** Tray "Add API key…" item.

### Doctor + provider list

* [x] **S18.** `fono doctor` Providers section — every backend with active
  marker, key-present flag, resolved model.
* [~] **S19.** `fono provider list [--json]` — covered by `fono use show`
  and the doctor section; deferred unless asked.

### Tests

* [x] **S20.** Factory test: backend with no `cloud` block + key present
  succeeds; key absent fails with a clear error.
* [x] **S21.** `Config` round-trip preserves unrelated fields after
  `set_active_stt` / `set_active_llm`.
* [x] **S22.** Reload integration test: mutate config + Reload → orchestrator
  reports new backends without restart.
* [~] **S23.** Profile round-trip — deferred with profiles (S9/S10).

### Docs + status

* [x] **S24.** Update `docs/providers.md` with the simplified switching flow.
* [x] **S25.** README "Switching providers" subsection.
* [~] **S26.** ADR `0009-multi-provider-switching.md` — deferred; rationale
  captured in this plan + commit messages for now.
* [x] **S27.** Status bump for `v0.1.0-rc provider-switching` milestone.

## Verification Criteria

- `fono use stt groq` then `fono use stt openai` flips active backend twice
  with daemon already running; subsequent `fono history --limit 1` rows show
  matching `stt_backend`.
- `fono record --stt groq --llm anthropic` runs once with that pairing; the
  next `fono record` reverts to persisted defaults.
- `fono keys list` after `fono keys add GROQ_API_KEY` + `fono keys add
  CEREBRAS_API_KEY` shows both, masked.
- `fono doctor` Providers section enumerates all backends with the active
  marked.
- `cargo test --workspace` passes new S20–S22 tests.
- `cargo clippy --workspace --no-deps -- -D warnings` stays clean.
- No daemon restart required between any switch above.

## Recommended Sequencing

1. S1–S3 — schema + factory smarts.
2. S4–S6 — `fono use` CLI + per-call overrides.
3. S11–S13 — IPC Reload + hot-swap.
4. S7 — `fono keys`.
5. S18 — doctor providers section.
6. S20–S22 — tests.
7. S24/S25/S27 — docs + status.

(S8/S9/S10/S14/S15–S17/S19/S23/S26 explicitly deferred to v0.2 with
rationale; this plan keeps v0.1 scope tight while unlocking the headline
"easy switching" UX.)
