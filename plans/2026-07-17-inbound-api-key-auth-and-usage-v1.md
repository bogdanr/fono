# Inbound API Keys: named-key auth, on/off config, and bounded usage tracking

## Objective

Replace Fono's single static bearer token for its **inbound** HTTP APIs (the
OpenAI/Ollama LLM surface **and** the `/v1/audio/transcriptions` STT +
`/v1/audio/speech` TTS routes, all served by `llm_server`) with a versatile,
multi-key authentication system:

- Multiple **named API keys**, each with a masked secret, creation date,
  last-used timestamp, optional expiry, and a per-interval request count —
  surfaced in a Groq-style "API Keys" table in the web settings UI (create /
  rename / set-expiry / revoke).
- `config.toml` (and the web UI) expose **only an on/off toggle** for server
  authentication, **ON by default**. The keys themselves are never in
  `config.toml`.
- Usage tracking must **not** turn any SQLite DB into an unbounded access log.
  Persist only **bounded per-interval aggregate counters** (per key, per
  day/month) plus a single `last_used_at` timestamp per key.

Non-goal: authenticating the Wyoming STT/TTS server (Wyoming v1 has no in-band
auth — documented, not solved here).

## Key facts grounding this plan

- Inbound surface + current single-token auth:
  `crates/fono-net/src/llm_server/mod.rs:92-96`,
  `crates/fono-net/src/llm_server/mod.rs:235-305`,
  `crates/fono-net/src/llm_server/mod.rs:315-327`.
- Web settings auth + `/api/*` + `/api/secret/{NAME}` + hooks:
  `crates/fono-net/src/web_settings/mod.rs:150-170`,
  `crates/fono-net/src/web_settings/mod.rs:300-420`.
- Ephemeral (non-persisted) access log today:
  `crates/fono-net/src/llm_server/access_log.rs:1-30`.
- Outbound provider-key store (separate concept, do not overload):
  `crates/fono-core/src/secrets.rs:20-90`.
- Transcript DB to keep decoupled from usage:
  `crates/fono-core/src/history.rs:88-140`.
- Config server subtables already present (`server.llm/web/wyoming`,
  `auth_token_ref`, `llm.model`): see the coverage test in
  `crates/fono-net/src/web_settings/mod.rs:520-560`.

## Assumptions (decisions made for ambiguous points)

1. **New dedicated store, new SQLite DB.** Inbound keys + counters live in a
   new `api_keys.sqlite` (desktop: `$XDG_DATA_HOME/fono/`; server mode:
   `/var/lib/fono/`), clamped `0600` like `history.sqlite`. Not in
   `secrets.toml` (that stays outbound-only) and not in `history.sqlite`
   (keeps transcripts decoupled from usage).
2. **Bounded counters, never per-request rows.** A `api_key_usage(key_id,
   bucket_kind, bucket_start, count)` table with `bucket_kind ∈ {day, month}`,
   incremented via UPSERT. Retention prune keeps ~62 daily + ~13 monthly
   buckets per key → total rows ≤ keys × ~75, independent of request volume.
3. **Secrets hashed at rest.** Store SHA-256(token) + a display prefix +
   last-4 chars; never the plaintext. Full secret shown **once** at creation.
   Reuse the `sha2`/`rand`/`rusqlite` crates already in the dependency graph
   (SHA-256 is already used for model download verification) — no new crate,
   consistent with the "no new crate" stance in ADR 0036.
4. **Token format.** `fono_sk_<base62 random ≥ 32 bytes>`, masked in UI as
   `fono_sk_…<last4>`.
5. **Config surface = booleans only.** `[server.llm].auth` and
   `[server.web].auth`, both `bool`, default `true`. Legacy
   `*.auth_token_ref` fields are dropped. We don't have any users yet. 
6. **Web-UI management stays loopback-trusted** to avoid a bootstrap lockout:
   a loopback caller can always reach the settings page and the API-key
   management endpoints (as today, where static assets are unauthenticated and
   loopback is trusted). The API-key requirement is enforced on the
   **inference** surface (LLM + `/v1/audio/*` + Ollama) and on any
   **non-loopback** access.
7. **Intervals.** Track and display both a rolling day ("USAGE (24h)") and a
   month count, matching the screenshot's per-interval column.

## Implementation Plan

### Phase 1 — Key store + usage model (fono-core)

- [x] Task 1. Add a new module `crates/fono-core/src/api_keys.rs` defining an
      `ApiKeyStore` over a dedicated `api_keys.sqlite`, mirroring
      `HistoryDb`'s open/migrate/owner-clamp pattern
      (`crates/fono-core/src/history.rs:88-140`). Schema: `api_keys(id, name
      UNIQUE, hash BLOB, prefix TEXT, last4 TEXT, created_at, expires_at NULL,
      last_used_at NULL, revoked INTEGER)` + `api_key_usage(key_id,
      bucket_kind, bucket_start, count, PRIMARY KEY(key_id,bucket_kind,
      bucket_start))`. Rationale: SQLite gives atomic UPSERT counters and cheap
      `last_used` updates without rewriting a TOML file on every request.
- [x] Task 2. Implement token lifecycle: `create(name, expires_at?) ->
      (row, plaintext_once)`, `list() -> Vec<ApiKeyView>` (metadata only, no
      secret), `rename`, `set_expiry`, `revoke`. Generate tokens with the
      existing RNG; store `sha2::Sha256` digest + prefix + last4. Rationale:
      matches the Groq-style table columns and "shown once" UX.
- [x] Task 3. Implement verification: `verify(presented: &str) ->
      Option<KeyId>` using constant-time comparison of the SHA-256 digest,
      rejecting `revoked` or expired keys. Add `add_path` helpers in
      `crates/fono-core/src/paths.rs` for the desktop vs server DB location.
      Rationale: correctness + timing-attack resistance on the hot auth path.
- [x] Task 4. Implement the **bounded** usage recorder: `record_hit(key_id,
      now)` that UPSERT-increments the current day+month buckets and updates
      `last_used_at` (debounced to ≤ once/N seconds), plus `prune()` that
      trims buckets beyond the retention window. Add `usage(key_id) ->
      {day_count, month_count}` and expose it in `ApiKeyView`. Rationale: this
      is the explicit "count per interval, never an access log" requirement.
- [x] Task 5. Unit tests: create/list/verify/expiry/revoke, hash-not-plaintext
      invariant, constant-time compare, UPSERT increment correctness, and a
      test proving row count stays bounded after simulating a high request
      volume across many days.

### Phase 2 — Config schema + migration (fono-core)

- [x] Task 6. In `crates/fono-core/src/config.rs`, add `auth: bool`
      (`#[serde(default = "default_true")]`) to the `server.llm` and
      `server.web` structs; deprecate/remove the `auth_token_ref` fields from
      the public schema. Keep `server.wyoming` unchanged but documented as
      unauthenticated.
- [x] Task 7. Add a load-time migration: if a legacy non-empty
      `auth_token_ref` resolves to a token, seed an `ApiKeyStore` entry named
      e.g. `migrated-<server>` from that value so existing clients keep
      working, set `auth = true`, drop the ref, and bump `version`. Rationale:
      preserves working LAN/Home-Assistant setups across upgrade.
- [x] Task 8. Update the config coverage allow-list/bindings expectation used
      by `crates/fono-net/src/web_settings/mod.rs:520-560` so the new `auth`
      booleans are UI-bound and the removed refs no longer required.

### Phase 3 — Enforcement wiring (fono-net + daemon)

- [x] Task 9. Replace `LlmServerConfig.auth_token: Option<String>` with an
      auth mode: `auth_enabled: bool` + an injected verifier
      `Arc<dyn Fn(&str) -> Option<KeyId>>` and a usage sink
      `Arc<dyn Fn(KeyId)>` (`crates/fono-net/src/llm_server/mod.rs:92-96`).
      Update `bearer_ok`/`route` (`:315-327`, `:285-305`) to: skip auth for
      loopback management if applicable, else look up the presented bearer via
      the verifier; on success capture `KeyId` and fire the usage sink after
      the response resolves (including deferred streaming completion).
      Rationale: single enforcement point covers LLM + STT + TTS + Ollama.
- [x] Task 10. Make the usage sink push `KeyId + timestamp` onto a bounded
      channel drained by a single background task that batches
      `record_hit`/`prune` writes. Rationale: keeps SQLite writes off the
      request hot path and naturally rate-limits `last_used`/counter writes.
- [x] Task 11. Mirror the enforcement in `web_settings` for the **inference**
      route it hosts (`/v1/audio/speech`) and for non-loopback access, while
      keeping loopback management of `/api/*` reachable (Assumption 6)
      (`crates/fono-net/src/web_settings/mod.rs:300-420`).
- [x] Task 12. Update the daemon layer that builds these servers from
      `[server.*]` to pass `auth_enabled`, wire the verifier/sink to the
      `ApiKeyStore`, and re-wire on `Reload` (hot-reload already supported).

### Phase 4 — Web settings "API Keys" section (fono-net assets + hooks)

- [x] Task 13. Extend `WebSettingsHooks` with `list_api_keys`,
      `create_api_key`, `update_api_key`, `revoke_api_key`
      (`crates/fono-net/src/web_settings/mod.rs:120-170`). Add routes:
      `GET /api/apikeys`, `POST /api/apikeys` (returns plaintext once),
      `PATCH /api/apikeys/{id}`, `DELETE /api/apikeys/{id}` — token/loopback
      gated like the other `/api/*` routes. Responses never echo stored
      secrets (parallels the write-only `/api/secret/{NAME}` design).
- [x] Task 14. Build the "API Keys" accordion in the embedded assets
      (`app.js`/`app.css`/`index.html` under
      `crates/fono-net/src/web_settings/assets/`): a table with NAME, SECRET
      KEY (masked `fono_sk_…last4`), CREATED, LAST USED, EXPIRES (red warning
      when near/after expiry), USAGE (24h/month), row delete, and a
      "Create API Key" flow that reveals the full secret exactly once with a
      copy button. Add the server-auth ON/OFF toggle in the key section.
      Show a warning if auth is OFF.
- [x] Task 15. Extend `GET /api/meta` so the UI knows auth state and can render
      the toggle; keep the config-coverage test green.

### Phase 5 — CLI, doctor, docs, tests

- [x] Task 16. Add a CLI group distinct from outbound `fono keys` — e.g.
      `fono server keys {create|list|rename|expire|revoke}` (in
      `crates/fono/src/cli.rs`) so inbound server keys are never confused with
      outbound provider keys. `create` prints the secret once.
- [x] Task 17. Extend `fono doctor` (`crates/fono/src/doctor.rs`) to report:
      auth on/off per server, count of active/expired keys, and a loud warning
      when a server binds non-loopback with auth OFF or zero keys (the
      "open relay to your paid cloud account" hazard already called out in the
      docs).
- [x] Task 18. Update `docs/configuration.md` (the `[server.llm]`/`[server.web]`
      sections and the "Settings in the browser" section) and `docs/install.md`
      / `docs/providers.md` server sections to describe the on/off toggle,
      the API Keys table, per-interval usage, and the upgrade/migration
      behavior. Add an ADR for the design.
- [x] Task 19. Add round-trip tests alongside
      `crates/fono-net/tests/llm_server_round_trip.rs` and
      `web_settings_round_trip.rs`: authorized vs unauthorized requests,
      expired/revoked rejection, loopback management still reachable, usage
      counters increment per request, and the migration path.

## Verification Criteria

- With `[server.llm].auth = true` (default) and no key, a non-loopback request
  to `/v1/chat/completions`, `/v1/audio/transcriptions`, and
  `/v1/audio/speech` returns `401`; a request bearing a valid key returns
  `200`.
- Setting `auth = false` disables enforcement; the only auth knob in
  `config.toml` and the web UI is the boolean toggle (no token strings).
- Creating a key in the web UI reveals the plaintext exactly once; reloading
  the page shows only the masked form; the DB stores a hash, never plaintext.
- The API Keys table renders NAME, masked SECRET KEY, CREATED, LAST USED,
  EXPIRES (with warning styling), and USAGE per interval, matching the
  reference screenshot's columns.
- After N thousand simulated requests over many simulated days, `api_keys.sqlite`
  row count stays within `keys × ~75` (proves counters are bounded, not an
  access log); `history.sqlite` is untouched by API usage.
- Upgrading a config that had a working `auth_token_ref` keeps existing clients
  authenticated via an auto-migrated key; `version` is bumped.
- `fono doctor` prints auth state, key counts, and warns on exposed-without-auth
  binds.

## Potential Risks and Mitigations

1. **Auth ON by default locks out existing LAN/Home-Assistant clients on
   upgrade.** Mitigation: migrate any existing `auth_token_ref` into a key
   (Task 7); for users with an exposed bind but no prior token, emit a loud
   `fono doctor` + notification with the exact `fono server keys create`
   command; document prominently in the changelog/upgrade notes.
2. **Bootstrap lockout of the settings UI (needed to create the first key).**
   Mitigation: loopback callers retain unauthenticated access to the settings
   page and key-management endpoints (Assumption 6); only inference and
   non-loopback traffic require a key.
3. **Write amplification / latency on the auth hot path.** Mitigation:
   background batched usage writer over a bounded channel; debounced
   `last_used`; UPSERT counters instead of row inserts (Tasks 4, 10).
4. **Unbounded growth (the explicit concern).** Mitigation: aggregate-only
   schema with day/month buckets and retention prune; a test asserts the row
   ceiling regardless of request volume.
5. **Timing attacks / plaintext at rest.** Mitigation: SHA-256 hashing +
   constant-time compare; DB clamped `0600`; secret shown once.
6. **Accidental new dependencies.** Mitigation: reuse `sha2`/`rand`/`rusqlite`
   already in the graph; hand-roll routes as the existing servers do (ADR 0036).
7. **Confusion between inbound and outbound keys.** Mitigation: separate store,
   separate CLI group (`fono server keys` vs `fono keys`), separate UI section.

## Alternative Approaches

1. **Store inbound keys in a TOML file** (like `secrets.toml`) instead of
   SQLite. Simpler, but every `last_used`/counter update would rewrite the
   file — poor fit for per-request writes and concurrency. Rejected.
2. **Log every request as a row and aggregate on read.** Matches an "access
   log" mental model and enables richer analytics, but grows unbounded with
   usage — explicitly ruled out. Rejected in favor of pre-aggregated buckets.
3. **Add tables to the existing `history.sqlite`.** Avoids a second DB file,
   but couples API usage to the transcript store and its retention policy;
   keeping them separate is cleaner and matches the stated intent. Deferred.
4. **Per-server independent key stores** vs one shared store. A single shared
   store is simpler and lets one key work across LLM + audio surfaces;
   per-server scoping could be a later enhancement (add a `scope` column).
