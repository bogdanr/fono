# Fono Web Configuration UI

## Status: Completed

## Objective

Implement a browser-based settings screen for Fono covering every user-relevant `config.toml` option, based on the **authoritative design handoff at `/root/design_handoff_fono_settings`** (search-first accordion, dark/light themes, high-fidelity — recreate pixel-perfectly per its README). Served over localhost by the daemon itself, reusing the already-shipped hyper stack, with embedded assets and no new dependencies.

## Design source of truth

`/root/design_handoff_fono_settings/` — `v4-accordion.html` (chrome), `controls.css` (design tokens + all control styles, dark default / light via `data-theme`), `sections.js` (all 9 sections' content + behaviors), `README.md` (full spec: tokens, row anatomy, controls, interactions, state model). Key points adopted verbatim:

- **Layout:** centered 740px column, search-first accordion (`<details>` per section, live mono value summaries in headers), unsaved-changes bar bottom-center, privacy footer with config path.
- **Controls:** toggles, segmented backend switchers with sub-panels, tag inputs, hotkey capture keycaps, provider card grids, write-only API-key status rows, sliders, collapsed prompt editors with reset, server sub-cards, pure-CSS overlay style previews.
- **State model:** one config object mirroring TOML; secrets write-only (`set | not set`); dirty tracking by diff against loaded config; master toggles grey out (`.section-off`) their body.
- **No assets:** no images/fonts/icons — CSS + unicode only (binary-size aligned).
- Prototype caveats per README: inline `onclick` handlers are mockup shortcuts — production uses proper listeners; generate section DOM from a schema/registry rather than hand-writing rows.

## Reconciliation deltas (handoff vs config schema — resolve during implementation)

- [x] Handoff's "Advanced tuning" lists *VAD aggressiveness, chunk size, pre-roll ms* — map to real keys: `interactive.chunk_ms_initial`/`chunk_ms_steady`, `interactive.hold_release_grace_ms`, `interactive.streaming_interval`, `audio.vad_backend` (pending removal decision). Drop "VAD aggressiveness" if `vad_backend` is removed. *(Done — all mapped to real keys; `vad_backend` kept as an energy/off select in Advanced tuning.)*
- [x] Handoff omits: `stt.prompts` (per-language STT prompts), `stt.local.threads`, `polish.skip_if_words_lt`, `polish.stream_injection`, `general.cloud_rerun_on_language_mismatch`, `mcp.*` detail (voices map, gender, relevance filter), `wakeword.refractory_ms`, `context_rules[]`, `update.channel` values. Decide per key: add to section 9 "Advanced tuning" disclosure or keep config-file-only (documented list). *(Done — split between the Advanced-tuning disclosure and the coverage test's `FILE_ONLY` allow-list.)*
- [x] Handoff endpoint names (`GET /config`, `PUT /config`, `PUT /secret/{name}`) supersede the v1 plan's `/api/*` names — adopt the handoff's, mounted under the web-settings server. *(Deviation: kept an `/api/` prefix — `GET/PUT /api/config`, `GET /api/meta`, `PUT /api/secret/{NAME}` — to cleanly separate the JSON API from the unauthenticated static assets at `/`.)*
- [x] Config path shown in footer must be the real resolved path (handoff hardcodes `~/.config/fono/fono.toml`).
- [x] Version chip (`settings · v0.9.2`) ← real `CARGO_PKG_VERSION`.

## Options to REMOVE or keep config-file-only (unchanged from v1)

- [x] **Remove candidates (do before UI work):** `audio.sample_rate` (pipeline is 16 kHz-only), `audio.vad_backend` (only energy/off), `interactive.mode` + `interactive.quality_floor` (reserved, single implemented value) — with migrate arms + tests. *(Removed `sample_rate`, `interactive.mode`, `quality_floor`; kept `vad_backend` — the tray's VAD toggle and energy/off switch still ride it.)*
- [x] **Never in UI:** `version`, `overlay.volume_bar = "advanced"` (debug, hand-edit only), `wakeword.wyoming` client mode (privacy-breaking opt-in), superseded-prompt machinery. *(`volume_bar` did land as an Advanced-tuning select since the tray already pairs it with the Transcript style; `version` and `wakeword.wyoming` are allow-listed file-only.)*

## Implementation Plan

- [x] Task 1. Simplification pre-work: audit + remove/freeze the four removal-candidate keys with migration arms and tests, shrinking the UI surface.
- [x] Task 2. Add `[server.web]` config block (enabled, bind=127.0.0.1, port, auth_token_ref) mirroring `ServerLlm`; off by default like all servers.
- [x] Task 3. Hand-rolled hyper routes in fono-net (same pattern as llm-server, ADR 0036): `GET /` (embedded HTML), `GET /config` (secrets redacted to booleans), `PUT /config` (validate → atomic `Config::save` → hot-reload signal), `PUT /secret/{name}` (routes to secrets store, never into config.toml), plus a meta payload (enums, defaults, provider catalogue, resolved config path, version) either inlined into the page or as `GET /meta`.
- [x] Task 4. Port the handoff into production assets: one embedded HTML + CSS + JS via `include_str!`; keep `controls.css` tokens/styles pixel-per-README; replace inline handlers with event listeners; generate section DOM from a schema registry (the handoff's `sections.js` pattern) bound to the loaded config object; implement dirty-diff tracking, search filter, `/` focus, Esc-cancels-capture, theme persistence, master-toggle greying, segmented sub-panels.
- [x] Task 5. Two-way binding + save flow: live accordion summaries from config values; Save = one PUT; Discard reverts to last-loaded snapshot; etag/version guard for concurrent tray/CLI edits. *(Etag guard deferred — hooks re-read disk per request and PUT replaces the whole document, so the stale-write window is a single browser session with concurrent tray edits; revisit if it bites.)*
- [x] Task 6. Entry points: `fono config web` CLI (start if needed + open browser) and tray menu item; loopback-only default; optional bearer token for non-loopback binds. *(Tray "Settings…" lazy-starts the listener in-process; the CLI enables the flag, probes the port, and opens the browser or prints restart guidance — it cannot start a listener inside an already-running daemon.)*
- [x] Task 7. Coverage test: assert every serialized default config key is claimed by a UI section or the explicit config-file-only allow-list, so new fields can't silently go missing.
- [x] Task 8. Gates + docs: `cargo fmt/clippy/test`, `./tests/check.sh --size-budget`; update providers/config docs, `docs/status.md`; changelog + roadmap at release time.

## Verification Criteria

- Rendered UI matches the handoff pixel-per-README (tokens, spacing, radii, focus rings, both themes).
- Every non-removed config key reachable in UI or on the documented config-file-only list (enforced by the coverage test).
- Secrets never appear in any HTTP response; write-only flow works end-to-end.
- No new crates in `Cargo.lock`; size-budget gate green (≤ 25 MiB cpu budget).
- Round-trip: load → edit one field → save preserves all other keys.
- Fully offline, keyboard-navigable, `/` search shortcut, Esc cancels hotkey capture.

## Potential Risks and Mitigations

1. **Handoff/schema drift** — the mockup invents a few knobs and omits others. Mitigation: the reconciliation checklist above is a blocking pre-implementation step; the coverage test enforces it permanently.
2. **HTTP exposure of config + secret-setting** — Mitigation: off by default, loopback bind default, bearer token required for non-loopback, secrets write-only, no CORS.
3. **Concurrent edits (tray/CLI vs web)** — Mitigation: version/etag on GET, conflict response on stale PUT.
4. **Hotkey capture in browser can't represent all keys** (media keys, etc.) — Mitigation: allow typing a key name manually as fallback.

## Alternative Approaches

1. **Ship the prototype JS structure as-is** (hand-written section HTML strings): faster initially, but every new config field needs hand-edited markup; schema-registry generation chosen per the handoff README's own recommendation.
2. **Sidebar layout (v1 plan)**: superseded by the approved accordion handoff.
