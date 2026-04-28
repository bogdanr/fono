# Streaming config collapse — drop redundant overlap with `[interactive].enabled`

## Objective

Remove the two configuration knobs whose decision is already implicit
in `[interactive].enabled`:

1. **`[stt.cloud].streaming`** — the per-cloud-backend opt-in that
   today gates whether the Groq pseudo-stream client is constructed.
2. **`[interactive].overlay`** — the per-section overlay toggle that
   today sits alongside the global `[overlay].enabled` and produces a
   warn-log-and-ignore when the user actually flips it off.

After this change there is exactly one streaming switch
(`[interactive].enabled`) and exactly one overlay switch for batch mode
(`[overlay].enabled`); the overlay is unconditionally shown during
streaming because it is the only feedback surface for live previews.

## Why

Today's three-knob layering makes "see my words appear as I speak"
require setting:

- `interactive.enabled = true`
- `stt.cloud.streaming = true` (Groq users only — the wizard prompts
  for it as a separate question after the first toggle)
- and *not* setting `interactive.overlay = false` (which the warn-log
  would override anyway)

Both deletions encode questions the user shouldn't have to answer
separately:

- `stt.cloud.streaming = false` while `interactive.enabled = true` on
  Groq is incoherent — the live pipeline runs, the overlay paints, and
  no preview text ever shows. There is no diagnostic for the
  user-facing failure mode "I turned on live mode and nothing
  happened." Cost is the natural concern, but `interactive`
  already owns `budget_ceiling_per_minute_umicros` and
  `streaming_interval` (which can disable previews entirely above
  3.0 s, leaving only finalize requests) for that.
- `interactive.overlay = false` is, per `session.rs:327-334`, a
  setting the daemon already silently overrides with a warn-log
  because turning off the only feedback channel for live previews is
  user-incoherent.

## Implementation Plan

### Layer A — Schema removals (`crates/fono-core/src/config.rs`)

- [ ] Task A1. Remove `pub streaming: bool` from `SttCloud` (line
  308). Serde silently ignores unknown keys, so legacy configs with
  `streaming = true` continue to parse without warning. The
  `synthetic_cloud` helper (`crates/fono-stt/src/factory.rs:82-89`)
  drops the field.

- [ ] Task A2. Remove `pub overlay: bool` from `Interactive` (line
  576) and from its `Default` impl (line 680). Same serde
  silent-ignore for legacy configs with `overlay = false`.

- [ ] Task A3. Update the `interactive_v7_keys_round_trip` test
  (`crates/fono-core/src/config.rs:912-959`): remove
  `overlay = false` from the input TOML and the
  `assert!(!i.overlay)` line.

- [ ] Task A4. Update `empty_interactive_block_yields_defaults`
  (`crates/fono-core/src/config.rs:961-986`) — no `overlay`
  reference to remove (it isn't asserted), but verify no test
  references `i.overlay`.

- [ ] Task A5. New unit test `legacy_streaming_and_overlay_keys_silently_ignored`:
  parse a TOML containing `[stt.cloud] streaming = true` and
  `[interactive] overlay = false` and assert it succeeds without
  error and that the resulting struct has the deleted-field absent.
  (Trivial — serde's default behaviour — but pins the contract for
  the next refactor.)

### Layer B — Factory wiring (`crates/fono-stt/src/factory.rs`)

- [ ] Task B1. `synthetic_cloud` (line 82-89): drop the
  `streaming: false` field from the constructed `SttCloud`.

- [ ] Task B2. `build_streaming_stt` (line 299-341): replace
  `let cloud_streaming = cfg.cloud.as_ref().is_some_and(|c| c.streaming);`
  (line 310) with `let cloud_streaming = interactive.enabled;`. The
  Groq arm's `if cloud_streaming` guard is preserved; the only
  semantic change is which knob it consults.

- [ ] Task B3. Update doc-comment at line 297 — drop
  "opt-in via `[stt.cloud].streaming = true`" and replace with a
  note that streaming for Groq follows `[interactive].enabled`.

- [ ] Task B4. Update warn-log at line 333-336 — drop the
  `(or `[stt.cloud].streaming = false`)` parenthetical. The new
  message: streaming STT not yet supported for backend `<X>`; live
  dictation will fall back to batch.

- [ ] Task B5. Update existing test
  `build_streaming_stt_returns_none_for_cloud_backend` at line 478:
  the test name is now misleading (Groq IS a streaming backend
  since Slice B1 Thread B); rename to
  `build_streaming_stt_returns_none_when_interactive_disabled` and
  assert that with `Interactive::default()` (where `enabled = false`)
  Groq returns `None`. Add a sibling test
  `build_streaming_stt_returns_groq_when_interactive_enabled` that
  flips `interactive.enabled = true` and asserts `Some(_)`.

### Layer C — Session/daemon wiring

- [ ] Task C1. `crates/fono/src/session.rs:324-348`: drop the
  `if !config.interactive.overlay { warn!(...) }` block (lines
  327-334). Spawn the overlay unconditionally inside the
  `interactive.enabled` block.

- [ ] Task C2. `crates/fono/src/daemon.rs:606-618`: drop the
  `overlay = config.interactive.overlay` formatter argument from the
  `info!("interactive  : {} (mode={}, overlay={})")` line. The new
  line: `info!("interactive  : {} (mode={})", ...)`.

### Layer D — Wizard

- [ ] Task D1. `crates/fono/src/wizard.rs:519-541`: drop the
  Confirm dialog asking about Groq streaming (lines 524-535). The
  `streaming` local variable is removed; the `SttCloud { ... }`
  literal at line 536-541 drops its `streaming` field per A1.

### Layer E — Docs

- [ ] Task E1. `docs/providers.md:91-106`: rewrite the "Enable with
  both knobs on" snippet to use one knob (`[interactive].enabled =
  true`). Drop the "wizard prompts for `streaming`" sentence at line
  104-106 — there is no separate prompt anymore.

- [ ] Task E2. `docs/decisions/0020-groq-pseudo-stream.md:49`:
  update bullet 6 from "Opt-in via `[stt.cloud].streaming = true`.
  Default `false`." to "Opt-in via `[interactive].enabled = true`.
  Default `false`. (Until 2026-04-29 a separate `[stt.cloud].streaming`
  knob also gated this; collapsed into the master switch in v0.3.5.)"

- [ ] Task E3. `CHANGELOG.md` `[Unreleased]`: add a `Removed`
  section entry for the two fields and a `Changed` entry that the
  overlay is now always shown when streaming, no escape hatch.
  Cross-reference the ADR update.

- [ ] Task E4. `docs/interactive.md` (if it documents the dropped
  knobs — check first).

### Layer F — Verification

- [ ] Task F1. `cargo build --workspace` — clean.
- [ ] Task F2. `cargo test --workspace --lib --tests` — green
  (existing 196 tests + the new legacy-keys-ignored test +
  refactored streaming factory tests).
- [ ] Task F3. `cargo clippy --workspace --all-targets -- -D warnings`
  — clean.
- [ ] Task F4. Slim cloud-only build sanity:
  `cargo build --no-default-features --features tray,cloud-all`.
- [ ] Task F5. Interactive build sanity:
  `cargo build --features fono/interactive`.

## Verification Criteria

- A user upgrading from v0.3.4 with an existing config that contains
  `[stt.cloud] streaming = true` and `[interactive] overlay = false`
  starts the daemon successfully, no warn/error log, and live mode
  is enabled iff `[interactive].enabled = true`.
- The wizard, run from scratch on a host with no config, does not
  ask about streaming as a separate question. Picking Groq + live
  mode just-works.
- Workspace test gate passes.
- No reference to `stt.cloud.streaming` or `interactive.overlay`
  remains in source, tests, or user-facing docs (status.md and
  closed plans are historical and stay).

## Potential Risks and Mitigations

1. **Existing user has `interactive.enabled = true` with
   `stt.cloud.streaming = false` on Groq, expecting live UI without
   live cloud calls.** This config produces broken UX today (overlay
   paints, never updates). After the change, those users get
   working streaming + the cost overhead. Mitigation: CHANGELOG
   `Changed` entry calls out the cost flip explicitly; users who
   want to bound cost have `interactive.streaming_interval > 3.0`
   (finalize-only) and `budget_ceiling_per_minute_umicros`.

2. **Existing user has `interactive.enabled = false` and a stale
   `stt.cloud.streaming = true` lingering.** Silently ignored —
   same as today.

3. **External code referencing the removed fields.** `synthetic_cloud`
   is a `pub` helper but only used by tests and docs; the field
   removal is one line. No third-party consumers expected (the crate
   isn't published).

## Alternative Approaches

1. **Rename `[interactive]` → `[streaming]` in the same pass.**
   Cleaner long-term name, but a serde-alias migration ripples
   through the wizard, the docs, the cargo `interactive` feature
   flag, and the design plan history. Reject for this pass; revisit
   if a v0.5 schema bump consolidates other names too.

2. **Keep the fields with a `#[deprecated]` attribute and a startup
   warn-log when set.** More noisy, no real ergonomics win — serde's
   default unknown-field-tolerance gets the migration safety for
   free, and the warn-log requires a second pass over the raw TOML
   string just to read the legacy keys before serde swallows them.
   Reject.

3. **Auto-migrate `streaming = true` → `interactive.enabled = true`.**
   Surprising — flipping `interactive.enabled` has cost
   implications. Better to let the user opt in explicitly via the
   wizard or the existing `[interactive].enabled` knob.
