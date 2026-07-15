# Web Doctor Integration

## Objective

Bring `fono doctor` to the web settings UI. Add a minimal hash router and app
shell to the SPA so multiple pages become possible (doctor is the first), an
aggregate health icon in the header (green ✓ / yellow ⚠ / red ✕) next to an
icon-ified theme toggle, and a dedicated `#/doctor` view showing per-check
results with a re-run action. Also implement the long-stubbed IPC
`Request::Doctor` so `fono doctor` can be served by a running daemon.

## Background / Current State

- `doctor::report()` is ~800 lines of `writeln!` producing one ANSI-colored
  `String` — no structured check model, no severity types
  (`crates/fono/src/doctor.rs:38-860`; color helpers at
  `crates/fono/src/doctor.rs:27-31`). It is exported from the lib surface
  (`crates/fono/src/lib.rs:27`), so the daemon can call it in-process.
- Doctor is cheap (< 1 s), offline, side-effect-free by design; the only
  subprocess-heavy probes are the Vulkan probe (process-cached via `OnceLock`,
  `crates/fono-core/src/vulkan_probe.rs:205-220`) and audio device enumeration
  shelling out to `wpctl`/`pactl`.
- The web settings server is raw hyper with a hand-rolled
  `match (method, path)` router (`crates/fono-net/src/web_settings/mod.rs:244-312`)
  and closure hooks supplied by the daemon
  (`crates/fono-net/src/web_settings/mod.rs:86-96`,
  `crates/fono/src/daemon.rs:3753-3835`). The LLM server uses the same router
  pattern on a separate port (`crates/fono-net/src/llm_server/mod.rs:279-302`);
  the two stay separate (different exposure/auth models).
- The SPA is a single view: header (`index.html:11-17`, text "Theme" button at
  `index.html:16`), search bar, accordion settings list. No client-side router.
  The auth token travels as `?token=…` and is forwarded as a Bearer header
  (`app.js:18-22`) — hash routing preserves the query string; path routing
  would not.
- Every served asset is individually `include_str!`-embedded with its own
  route arm (`web_settings/mod.rs:250-256`). The
  `config_coverage_ui_or_allowlist` test guard scans `app.js` for config-key
  references (`web_settings/mod.rs:422-524`) — keep all frontend JS in the one
  `app.js` for now.
- IPC: `Request::Doctor` exists on the wire (`crates/fono-ipc/src/lib.rs:83`)
  but the daemon handler returns a "not yet available" stub
  (`crates/fono/src/daemon.rs:1801-1803`). `Request::Status` is the precedent
  for text responses (`crates/fono/src/daemon.rs:1772-1782`).

## Design Decisions

- **Structured model first.** All web/IPC surfaces are renderers over one
  typed `DoctorReport`; the CLI text output becomes a renderer too, preserving
  today's output byte-for-byte (colors included).
- **Three-state header icon.** Warn (yellow) is distinct from Fail (red);
  doctor emits many advisory findings that should not render a scary red
  icon. Aggregation: any Fail ⇒ Fail; else any Warn ⇒ Warn; else Ok.
- **No caching.** The doctor page is rarely refreshed; run the report on
  demand. Only hygiene: run it on a blocking-friendly task and deduplicate
  concurrent in-flight runs (share one result, not a TTL cache).
- **Hash router, not paths.** `#/settings` (default) and `#/doctor`;
  preserves `?token=`, requires no server-side fallbacks, deep-linkable.
- **Icons are CSS-colored text glyphs** (✓ ⚠ ✕ for status, ◐ for theme), not
  emoji — emoji rendering is font/platform-dependent and ignores theme
  colors. All icon buttons carry `title` tooltips and `aria-label`s.
- **No new dependencies.** serde/hyper are already in the graph; frontend
  stays vanilla JS in the existing embedded assets. Binary grows only by the
  embedded asset delta — run the size-budget gate.

## Implementation Plan

- [x] Task 1. **Structured doctor model.** In `crates/fono/src/doctor.rs`,
  introduce `Severity { Ok, Warn, Fail, Info }`,
  `Check { label, detail, severity }`, `Section { title, checks }`, and
  `DoctorReport { sections, generated_at, version, variant }` with an
  `aggregate() -> Severity` helper; derive `Serialize` (serde already in
  graph). Rationale: the header icon and JSON endpoint are impossible without
  typed results; this is the foundation for everything else.
- [x] Task 2. **Decompose `report()` into gather + render.** Split the
  existing function into `gather(&Paths) -> Result<DoctorReport>` (all
  probing, same order as today) and a text renderer that reproduces the
  current ANSI output from the model. `report()` becomes
  `render_text(&gather()?)`. Before refactoring, capture the current output
  shape in a snapshot-style test so regressions are caught. Rationale: keeps
  the CLI contract stable while unlocking JSON.
- [x] Task 3. **Frontend shell + hash router.** Restructure
  `crates/fono-net/src/web_settings/assets/app.js` (and `index.html`) into a
  shell — header, toast, theme handling, token/api wrapper — plus a view
  registry keyed by `location.hash` with a `hashchange` listener; `#/settings`
  is the default view and is the existing settings render moved behind the
  registry (move, don't rewrite: it keeps owning its search filter,
  unsaved-changes bar, and open-state preservation). Rationale: multiple
  pages are planned; retrofitting a shell after several ad-hoc pages is far
  costlier than doing it now with one view.
- [x] Task 4. **`GET /api/doctor` endpoint.** Add a `doctor` hook to
  `WebSettingsHooks` (`crates/fono-net/src/web_settings/mod.rs:86-96`) and one
  match arm in `route()` (`web_settings/mod.rs:266-312`), token-gated like the
  other `/api/*` routes (do not exempt it like static assets — the report
  leaks system topology). Daemon-side hook
  (`crates/fono/src/daemon.rs:3753-3835`) runs `doctor::gather()` on a
  blocking-friendly task with a trivial in-flight guard so overlapping
  requests share one run; serialize the `DoctorReport` to JSON. No cache.
- [x] Task 5. **Header icons.** Replace the "Theme" text button
  (`index.html:16`) with a `◐` glyph button, and add a doctor status button
  beside it: `…`/neutral while loading, then ✓ (green) / ⚠ (yellow) / ✕ (red)
  from the report's aggregate severity, colored via CSS variables so both
  themes work; tooltip + aria-label; click navigates to `#/doctor`. The icon
  state comes from one report fetch on page load (no polling) and is updated
  after any explicit re-run.
- [x] Task 6. **Doctor view.** Render the report at `#/doctor` using the
  existing `<details class="sec">` accordion pattern: one section per report
  section, per-check severity dot + label + detail, sections containing
  Warn/Fail auto-opened, last-run timestamp, and a "Re-run checks" button
  that re-fetches `/api/doctor` and updates both the view and the header
  icon. A back affordance (the wordmark or an explicit link) returns to
  `#/settings`.
- [x] Task 7. **Implement IPC `Request::Doctor`.** Replace the stub at
  `crates/fono/src/daemon.rs:1801-1803`: run `doctor::gather()` (same
  in-flight guard as Task 4) and return `Response::Text` of the rendered
  text report (color-free, since IPC output isn't a TTY-checked stream).
  Optionally teach the CLI `fono doctor` path nothing new — the direct
  one-shot remains the primary UX; IPC doctor exists for tooling and parity.
- [x] Task 8. **Tests.** (a) text-renderer parity snapshot from Task 2;
  (b) `aggregate()` severity logic; (c) route test: `/api/doctor` returns 401
  without token when a token is configured, 200 JSON with it; (d) in-flight
  dedup guard behaviour; (e) confirm `config_coverage_ui_or_allowlist`
  (`web_settings/mod.rs:422-524`) still passes after the `app.js`
  restructure; (f) IPC doctor round-trip returning non-stub text.
- [x] Task 9. **Gates.** `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`, then
  `./tests/check.sh --size-budget` (embedded assets grow the binary).
  Update `docs/status.md` at session end per project rules.

## Verification Criteria

- `fono doctor` CLI output is unchanged after the refactor (snapshot test
  green).
- `GET /api/doctor` returns structured JSON with per-check severities and an
  aggregate; requires the bearer token when one is configured.
- The header shows the correct three-state icon within one fetch of page
  load; "Re-run checks" updates both the doctor view and the icon.
- Navigating `#/settings` ↔ `#/doctor` preserves the token, theme, and the
  settings view's unsaved-changes state.
- `fono` IPC `Request::Doctor` returns the rendered report instead of the
  "not yet available" stub, and works while the daemon is running.
- All four pre-commit/size gates pass.

## Potential Risks and Mitigations

1. **Refactor regressions in the CLI report** — 800 lines of interleaved
   probing and formatting are easy to subtly reorder.
   Mitigation: snapshot the current output first; keep probe order identical;
   gather/render split touches structure, not logic.
2. **Shell refactor destabilizes the settings view** (search filter,
   unsaved-changes bar, accordion open-state are stateful).
   Mitigation: move code rather than rewrite; the settings view keeps its own
   state, the shell owns only header/toast/routing.
3. **Doctor probes stalling the daemon runtime** (Vulkan probe subprocess,
   `wpctl`/`pactl` spawns).
   Mitigation: run `gather()` on a blocking-friendly task; in-flight guard
   deduplicates concurrent requests.
4. **Glyph rendering differences across platforms/fonts.**
   Mitigation: near-universal glyphs (✓ ⚠ ✕ ◐) with CSS color carrying the
   semantics; fall back to tiny inline SVGs only if testing shows gaps.
5. **Binary size growth from embedded assets.**
   Mitigation: keep the doctor view lean, single `app.js`; the size-budget
   gate is a hard check in Task 9.

## Deferred / Out of Scope

- `--json` flag on the CLI doctor subcommand (trivial once Task 1–2 land;
  defer until there's a consumer).
- `/api/doctor/summary` lightweight endpoint (unnecessary — the icon derives
  from the single full-report fetch).
- Any caching/TTL layer (revisit only if a future page starts polling).
- Splitting frontend assets into multiple JS files / asset manifest (revisit
  when more pages make `app.js` unwieldy).
- Live provider reachability probes (doctor's provider matrix checks key
  presence only; changing that is a separate feature).

## Alternative Approaches (considered and rejected)

1. **Raw text report in a `<pre>`** — days less work but no per-check
   severity, no aggregate icon, forfeits the structured model that also
   unlocks IPC doctor. Rejected: the icon requirement inherently demands
   structure.
2. **Separate embedded `/doctor.html` page** — matches "different page"
   literally but duplicates theming/auth/toast plumbing and adds an asset per
   future page. Rejected in favor of the hash-routed SPA view.
3. **Path-based routing** — breaks `?token=` propagation and needs
   server-side fallbacks per view. Rejected in favor of hash routing.
