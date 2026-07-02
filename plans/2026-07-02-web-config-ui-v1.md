# Fono Web Configuration UI

## Objective

Design and later implement a browser-based configuration screen for Fono that covers every user-relevant option in `config.toml`, organized into clear sections, with modern minimal UI — without meaningfully growing the binary. Layout/IA first; technology decision second; implementation third.

## Key architectural fact

Hyper + hyper-util + http-body-util are **already in the dependency graph** (default `llm-server` feature, `crates/fono-net/Cargo.toml:31`, ADR 0036 — raw hyper, hand-rolled router, no axum). A `[server.web]` config endpoint can reuse the exact same stack. UI assets (one HTML + one CSS + one JS file) get embedded via `include_str!` — expected binary growth: **tens of KB, well under budget**. No new crates required if the API serves/accepts JSON via the already-present serde. TOML round-trip stays server-side (`Config::load`/`Config::save` already atomic).

## Config inventory → proposed UI sections (information architecture)

Sidebar navigation, 8 user pages + 1 advanced page:

1. **General** — `general.languages` (tag input with BCP-47 picker), `general.startup_autostart`, `general.also_copy_to_clipboard`, `general.auto_mute_system`
2. **Hotkeys & Wake Word** — `hotkeys.dictation`, `hotkeys.assistant`, `hotkeys.cancel` (key-capture widget); `wakeword.enabled`, `wakeword.phrases[]` (model, sensitivity slider, target)
3. **Speech to Text** — `stt.backend` (segmented: Local / Cloud provider grid / Wyoming), `stt.local.model` + `quantization`, `stt.cloud` (provider, model, API-key status — write-only secret field), `stt.wyoming.uri`, `stt.prompts` (per-language, collapsed)
4. **Cleanup (Polish)** — `polish.enabled` master toggle, `polish.backend`, local model / cloud provider+model+key, `polish.prompt.dictionary` (tag list), `polish.prompt.main`/`advanced` (collapsed textareas with "reset to default")
5. **Assistant** — `assistant.enabled`, `assistant.backend`, model/key, `prompt_main`, `realtime.live_mode`, `realtime.max_session_secs`, `prefer_vision`, `prefer_web_search`, history window/turns
6. **Voice (TTS)** — `tts.backend`, `tts.voice` (palette picker), `tts.output_device`, `tts.local.voice`, `tts.wyoming.uri`, `tts.cloud`
7. **Overlay & Audio** — `overlay.waveform`, `overlay.style` (visual style cards with tiny previews), `audio.trim_silence`, `audio.auto_stop_silence_ms` (Off/3s/5s/custom)
8. **History & Privacy** — `history.enabled`, `retention_days`, `redact_secrets`; wake-word Wyoming client privacy warning surface
9. **Servers & Advanced** (single page, sub-grouped) — `server.wyoming.*`, `server.llm.*`, `network.instance_name`, `mcp.*` (voices map, gender, filters), `inject.backend`, `update.*`, `interactive.*` tuning, `general.cloud_rerun_on_language_mismatch`, `polish.skip_if_words_lt`, `polish.stream_injection`, `stt.local.threads`, `wakeword.refractory_ms`, `context_rules[]`

## Options to REMOVE or keep config-file-only (simplification pass)

- [ ] **Remove candidate:** `audio.sample_rate` — pipeline assumes 16 kHz Whisper input; freezing at 16000 and dropping the key (with a migrate arm) removes a footgun. Verify no code path honours other rates before removal.
- [ ] **Remove candidate:** `interactive.mode` and `interactive.quality_floor` — both documented as "reserved"; only one value each is implemented. Drop keys or leave parsed-but-hidden.
- [ ] **Advanced-page-only:** `stt.local.languages` override, `stt.local.threads`, `interactive.*` timing knobs, `polish.stream_injection`, `general.cloud_rerun_on_language_mismatch`, `mcp.relevance_*`, `update.channel`.

## UX principles (locked before technology)

- Sidebar nav (collapses to top tabs on narrow windows), one section per page, search-across-settings box.
- Master on/off toggle at the top of every feature page (Polish, Assistant, TTS, Wake word, each server) — disabled features grey out their controls.
- Sticky dirty-state save bar ("You have unsaved changes — Save / Discard"); save = one PUT; server does atomic TOML write and hot-reload signal.
- Secrets are write-only: show "configured ✓ / not set", accept new value, never echo back.
- Per-page "Advanced" disclosure instead of burying everything in one advanced dungeon (Servers & Advanced page is the exception for genuinely rare knobs).
- Every control mirrors the config doc-comment as helper text; defaults shown; "reset to default" affordance on prompts and numeric fields.
- Dark theme default with light option; system font stack; no icon fonts, no external assets, works fully offline.

## Implementation Plan

- [ ] Task 1. Finalize layout via Claude-design mockup iteration (prompt below); lock the 9-section IA and interaction patterns before any code.
- [ ] Task 2. Simplification pre-work: audit + remove/freeze `audio.sample_rate`, `audio.vad_backend`, `interactive.mode`, `interactive.quality_floor` with migrate arms and tests — shrinks the surface the UI must cover.
- [ ] Task 3. Define the HTTP surface in fono-net behind a new `[server.web]` block (enabled, bind=127.0.0.1, port, auth_token_ref) mirroring `ServerLlm`; hand-rolled hyper routes: `GET /` (embedded HTML), `GET /api/config`, `PUT /api/config`, `GET /api/meta` (enums, defaults, provider catalogue, secret-presence flags), `POST /api/secrets/{key}`.
- [ ] Task 4. Serve embedded assets: single `index.html` + `app.css` + `app.js` via `include_str!`, vanilla JS (no framework, no build step), rendering forms from a JSON schema-ish `/api/meta` payload so new config fields need minimal UI work.
- [ ] Task 5. Config write path: server-side validation, atomic save via existing `Config::save`, daemon hot-reload/notification; never write secrets into config.toml (route through the existing secrets mechanism).
- [ ] Task 6. Entry points: `fono config web` CLI to open browser, tray menu item; loopback-only default, size-budget gate run before push.
- [ ] Task 7. Docs + status: providers/config docs updated, `docs/status.md` session log, changelog entry at release time.

## Verification Criteria

- Every non-removed config key is reachable in the UI or explicitly catalogued as config-file-only.
- `./tests/check.sh --size-budget` stays green (≤ 25 MiB cpu budget) after asset embedding.
- No new crates in `Cargo.lock` (hyper stack reused); `deny.toml` untouched.
- Round-trip safety: load → edit one field → save leaves all other keys and comments-equivalent structure intact (TOML re-serialization acceptable per current `Config::save` behaviour).
- Secrets never appear in any HTTP response body.
- UI usable offline, keyboard-navigable, and legible at 320 px width.

## Potential Risks and Mitigations

1. **Binary growth from UI assets** — Mitigation: single hand-written page, no framework, no fonts/images (inline SVG only); gate with size-budget check.
2. **Exposing config (and secret-setting) over HTTP** — Mitigation: loopback bind default, optional bearer token, secrets write-only, no CORS.
3. **Config drift: new fields added but UI forgotten** — Mitigation: `/api/meta` generated from the serde schema side where possible + a test asserting every serialized default key is claimed by a UI section or the explicit config-file-only allow-list.
4. **Concurrent edits (tray/CLI vs web)** — Mitigation: version/etag on GET, conflict response on stale PUT.

## Alternative Approaches

1. **TUI settings screen instead of web** — zero HTTP exposure, but worse discoverability and no rich widgets; rejected as primary, possible later complement.
2. **egui/native settings window** — heavier binary impact (egui not currently a dep for a full settings surface) and duplicated layout work per platform.
3. **htmx-style server-rendered forms** — even less JS, but full-page reloads make the dirty-state save bar and live validation clunkier; vanilla-JS SPA-lite chosen instead.
