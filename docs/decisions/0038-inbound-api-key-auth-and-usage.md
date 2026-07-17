# ADR 0038 — Inbound API-key authentication with bounded usage

- **Status:** Accepted
- **Date:** 2026-07-17
- **Supersedes:** the single `auth_token_ref` pre-shared token on
  `[server.llm]` / `[server.web]` / `[server.wyoming]`
- **Related:** [ADR 0036 — Local LLM server (OpenAI + Ollama)](0036-local-llm-server-openai-ollama.md)
- **Plan:** [`plans/2026-07-17-inbound-api-key-auth-and-usage-v1.md`](../plans/2026-07-17-inbound-api-key-auth-and-usage-v1.md)

## Context

Fono exposes several HTTP surfaces on the LAN: the OpenAI/Ollama chat API
plus its speech-to-text (`/v1/audio/transcriptions`) and text-to-speech
(`/v1/audio/speech`) routes (ADR 0036), and the web settings page. The
original design guarded these with a single optional pre-shared bearer
token per server, referenced from `config.toml` via `auth_token_ref` and
resolved from an env var or `secrets.toml`.

That model had three problems:

1. **One token, no lifecycle.** You cannot name, rotate, expire, or
   revoke individual clients; a leaked token means rotating the one
   secret for everyone.
2. **Config surface.** Users had to understand token *references* and
   wire them to env/secrets, when all they wanted was "auth on/off".
3. **No visibility.** There was no record of which client last called,
   or how much — only ephemeral `debug` tracing that is neither
   persisted nor queryable.

Users asked for a Groq-style **API Keys table** (name, masked secret,
created / last-used, expiry, per-interval usage) and an explicit
requirement: **do not turn the transcript history DB into an access log
that grows unbounded with usage.**

## Decision

Replace the per-server token with a **multi-key store** and reduce the
config surface to a single boolean.

### Config: one on/off toggle, on by default

`[server.llm].auth` and `[server.web].auth` are booleans, defaulting to
`true`. No token strings live in `config.toml`. Loopback callers are
always trusted (the local owner), so enabling auth never causes a
bootstrap lockout — the first key can be created from the local browser
or CLI. The legacy `auth_token_ref` is accepted at load time, migrated
into a named key, then cleared.

### Store: hashed keys in a dedicated SQLite DB

Keys live in `api_keys.sqlite` (mode `0600`), separate from
`history.sqlite`. Each key stores a SHA-256 hash of the secret (never the
plaintext), a display prefix + last-4 for masking, created/expiry
timestamps, a revoked flag, and a debounced `last_used_at`. The plaintext
secret (`fono_sk_…`) is returned **exactly once**, at creation.
Verification hashes the presented token and compares constant-time
against all candidates.

### Usage: pre-aggregated counters, never a log

Usage is stored as **bounded per-interval counters** — one row per
`(key, day)` and `(key, month)` bucket, incremented with an UPSERT — not
one row per request. Stale buckets are pruned (≈62 day + 13 month buckets
per key kept), so the DB size is a function of *key count*, not request
volume. This satisfies the "no unbounded access log" requirement while
still answering "when was this key last used, and how much this month".

### Shared enforcement seam

Both servers call a single pure `fono_net::auth::decide(auth_enabled,
is_loopback, presented, verifier)` so the rules are identical and unit
testable. The daemon injects an `AuthVerifier` (token → key id) and a
`UsageSink` (record one hit) closure so the servers stay decoupled from
`rusqlite` (whose `Connection` is not `Sync`).

## Consequences

- **Simpler mental model:** users flip auth on/off; keys are managed in a
  dedicated table (web UI) or `fono server keys …` (CLI), cleanly
  separated from the outbound provider keys in `secrets.toml`.
- **Real lifecycle:** name, expire, revoke, and delete per client;
  rotating one client does not disturb the others.
- **Visibility without bloat:** last-used + monthly counts per key, with
  a DB that never grows into an access log.
- **Migration:** existing `auth_token_ref` deployments keep working; the
  migrated key's secret is logged once so clients can be updated.
- **Wyoming unchanged:** the Wyoming STT/TTS/wake server (v1 protocol)
  has no in-band auth and is out of scope here; it remains loopback-first.
