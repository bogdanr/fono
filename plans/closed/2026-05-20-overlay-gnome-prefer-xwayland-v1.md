# Wayland Overlay: Prefer Xwayland Over xdg Fallback on GNOME

## Status: Completed

## Objective

Fix the user-reported behaviour on Ubuntu 24.04 GNOME where the Fono overlay
appears in Alt+Tab, doesn't stay on top, and lands at compositor-chosen
positions. The overlay is functionally a regular application window because
the `wayland-xdg-fallback` backend is using `xdg_toplevel`, which the
protocol explicitly defines as "application toplevel" — Mutter treats it
accordingly. No client-side `xdg_toplevel` change can fix this.

The fix reorders the backend candidate list so that on Wayland sessions
where Xwayland is also present (the default on Ubuntu 24.04 GNOME, Fedora
Workstation, etc.), the existing `x11-override-redirect` backend is tried
**before** `wayland-xdg-fallback`. Override-redirect Xwayland windows bypass
the window manager entirely: client controls position, no Alt+Tab entry, no
taskbar entry, stacks above normal windows, ARGB transparency. Same UX as
on native X11.

`wayland-xdg-fallback` remains the last-resort path for Wayland-only
sessions with Xwayland disabled (rare).

## Implementation Plan

- [ ] Task 1. **Reorder `candidate_list_with` in `crates/fono-overlay/src/backend.rs:281-294`.**
  Split today's `(true, true) | (true, false)` arm into two arms so the
  ordering can differ between "Wayland + Xwayland present" and "Wayland
  only". On `(true, true)`: `[WlrLayerShell, X11OverrideRedirect,
  WaylandXdg, Noop]`. On `(true, false)`: `[WlrLayerShell, WaylandXdg,
  Noop]`. Add a comment block explaining the GNOME rationale.

- [ ] Task 2. **Update selection unit tests in `crates/fono-overlay/src/lib.rs:170-211`.**
  Rename `selection_prefers_wayland_native_under_xwayland` to
  `selection_prefers_xwayland_over_xdg_on_wayland`. Assert the new exact
  order `[WlrLayerShell, X11OverrideRedirect, WaylandXdg, Noop]` for the
  Wayland+DISPLAY case. Update or add `selection_wayland_only_skips_x11`
  asserting `[WlrLayerShell, WaylandXdg, Noop]` for the
  WAYLAND_DISPLAY-only case. Add a module-level comment explaining that the
  list is a *candidate order*; protocol-availability probing happens at
  `try_spawn` time, and GNOME's fallthrough to X11 emerges from
  `zwlr_layer_shell_v1` not being advertised at runtime.

- [ ] Task 3. **Update `probe_selection` reason strings in `crates/fono-overlay/src/backend.rs:396-417`.**
  Keep the current "return first candidate" semantics (cheap, no probing).
  Update reasons to: WlrLayerShell → `"Wayland + layer-shell preferred
  (will fall through to Xwayland on GNOME / Mutter)"`,
  X11OverrideRedirect → `"X11 (or Xwayland on Wayland sessions)"`,
  WaylandXdg → `"Wayland xdg fallback (degraded placement)"`,
  Noop → `"no graphics session detected"`. Rationale for not implementing
  runtime layer-shell probing here: a registry roundtrip per `fono doctor`
  invocation is acceptable but adds protocol-coupling that doesn't pay off
  for a diagnostic command; the truth is exposed by the daemon's startup
  log entry which records the winning backend.

- [ ] Task 4. **Rewrite the backend table in `docs/wayland.md:8-11`.**
  Reflect the new ordering. Promote `x11-override-redirect` to row 2 with
  "(via **Xwayland**)" annotation. Demote `wayland-xdg-fallback` to "last
  resort, Wayland-only sessions with Xwayland disabled". Note Xwayland's
  override-redirect WM-bypass properties (no Alt+Tab, client-positioned,
  on-top, transparency via ARGB visuals).

- [ ] Task 5. **Add HiDPI caveat section to `docs/wayland.md`.**
  Insert "HiDPI on GNOME" subsection right after the backend table.
  Explain that Xwayland's bitmap up-scaler renders fractional scales
  (125 %, 150 %, 175 %) slightly fuzzy. Integer scales (100 %, 200 %) are
  crisp. Escape hatch: `FONO_OVERLAY_BACKEND=xdg` to opt back into the
  native Wayland fallback at the cost of placement.

- [ ] Task 6. **Update troubleshooting in `docs/wayland.md`.**
  Replace the "Overlay appears in the wrong place on GNOME" item with
  "Overlay text is fuzzy on GNOME with fractional scaling" pointing to
  the HiDPI section and the env-var escape hatch.

- [ ] Task 7. **Add `[Unreleased]` `Changed` entry in `CHANGELOG.md`.**
  Single bullet: "Overlay backend selection now prefers Xwayland (X11
  override-redirect) over the Wayland xdg fallback on Wayland sessions,
  so GNOME / Mutter users get a properly anchored, always-on-top overlay
  that doesn't appear in Alt+Tab."

- [ ] Task 8. **Append a dated entry to `docs/status.md`.**
  Date 2026-05-20. Summarise: user report, protocol-level root cause,
  the reorder fix, the HiDPI trade-off, and the gate result.

- [ ] Task 9. **Run the pre-commit gate.**
  `cargo fmt --all -- --check && cargo clippy --workspace --all-targets
  -- -D warnings && cargo test --workspace --tests --lib`. All three must
  exit 0. Confirm the renamed/added selection tests pass with
  `cargo test -p fono-overlay --lib selection_`.

- [ ] Task 10. **Commit with DCO sign-off.**
  Single focused commit. Suggested subject:
  `overlay(wayland): prefer Xwayland over xdg fallback on GNOME`.
  No `Co-authored-by: Forge` trailer (permanent rule). Do NOT push;
  leave for the user to push after live verification on the Ubuntu
  24.04 reproducer.

## Verification Criteria

- `crates/fono-overlay/src/backend.rs` `candidate_list_with(|k|
  k=="WAYLAND_DISPLAY" || k=="DISPLAY")` returns exactly
  `[WlrLayerShell, X11OverrideRedirect, WaylandXdg, Noop]`.
- `candidate_list_with(|k| k=="WAYLAND_DISPLAY")` returns exactly
  `[WlrLayerShell, WaylandXdg, Noop]`.
- `candidate_list_with(|k| k=="DISPLAY")` returns exactly
  `[X11OverrideRedirect, Noop]`.
- `candidate_list_with(|_| false)` returns exactly `[Noop]`.
- All three pre-commit gate commands exit 0; `cargo test -p fono-overlay
  --lib selection_` reports `0 failed`.
- On Ubuntu 24.04 GNOME (`192.168.0.112`) after `git pull && cargo build
  --release -p fono && fono`: `fono doctor` reports
  `Overlay : x11-override-redirect (transparency=yes positioning=client
  focus-passthrough=yes click-passthrough=yes)`. Overlay anchors
  bottom-centre. Overlay does NOT appear in Alt+Tab. Overlay stays
  above other windows. Typing into the previously-focused app during
  dictation continues to land in that app.
- `FONO_OVERLAY_BACKEND=xdg fono` still works (regression check
  for the fallback path on a Wayland session).

## Potential Risks and Mitigations

1. **HiDPI fractional scaling looks fuzzy under Xwayland on GNOME.**
   This is real and unavoidable — Xwayland's up-scaler is bitmap-based.
   Mitigation: document the trade-off in `docs/wayland.md`; provide
   `FONO_OVERLAY_BACKEND=xdg` as an explicit user-side escape.
   Long-term mitigation (not in this plan): render the overlay at native
   resolution by querying `_NET_WM_FRAME_DRAWN` and the GDK scale-factor
   X property, and scale our own framebuffer accordingly.

2. **Clicks on the overlay don't pass through under X11 override-redirect.**
   The overlay receives button events because no `XShape` input region
   is set. This is the same on native X11 (no regression). The overlay
   is small (≈ 640×80) and transient (only during dictation); accepted.
   Tracked as a future improvement using `XFixesSetWindowShapeRegion`.

3. **Xwayland disabled on minimal Wayland installations.**
   The user falls through to `wayland-xdg-fallback` with its known
   placement weirdness. This is strictly an improvement over today
   (those users currently get the same xdg fallback); their UX doesn't
   regress.

4. **GNOME without Xwayland but with a future layer-shell implementation
   appearing.** Selection order already tries `wlr-layer-shell` first;
   when Mutter eventually ships it (or a user installs an extension
   that provides it), Fono picks it up automatically.

5. **The xdg backend's surface still briefly appears during a future
   re-arrangement of selection logic.** Not a risk for this change
   because `try_spawn` is only called on the winning candidate's
   predecessor failing. The Xwayland backend's `try_spawn` succeeds
   whenever `DISPLAY` resolves to a reachable server, so on GNOME we
   never reach `WaylandXdg::try_spawn`.

## Alternative Approaches

1. **Implement runtime layer-shell probing in `probe_selection`.**
   Trade-off: more accurate `fono doctor` output on GNOME (would correctly
   report `x11-override-redirect` ahead of time). Cost: extra protocol
   coupling in a diagnostic-only code path, ~50 ms registry roundtrip on
   every doctor call. Rejected for this fix; the daemon's startup log
   already records the winning backend, which is the source of truth.

2. **Drop the `wayland-xdg-fallback` backend entirely.**
   Trade-off: simpler codebase, ~600 LOC removed. Cost: regressive UX
   on the rare Wayland-only-no-Xwayland setup (users get the `noop`
   backend → no overlay at all). Rejected: keeping the fallback costs
   little and degrades gracefully.

3. **Force-set window properties on the xdg_toplevel surface to hint
   "don't list in Alt+Tab".**
   Investigated: there is no such hint in the `xdg_toplevel` protocol.
   `set_app_id` doesn't suppress task-switcher entry. `wp_single_pixel`
   and `xdg_dialog_v1` exist but don't apply. Confirmed dead end.

4. **Ship a GNOME Shell extension that exposes a private "panel layer"
   D-Bus interface.**
   Trade-off: proper GNOME-native solution. Cost: every user must
   manually install the extension; ecosystem fragmentation across
   GNOME versions; ongoing maintenance burden. Deferred to a
   future Phase 3 plan.

5. **Use `gtk-layer-shell` via a thin C shim.**
   Trade-off: a real layer-shell on every Wayland desktop that supports
   it (which is the same set we already cover with `wlr-layer-shell`).
   Cost: adds GTK4 dependency tree (8–12 MiB). Rejected per ADR 0022
   binary-size budget; provides no incremental compositor coverage.
