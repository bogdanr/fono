# Prompt-cache panel redesign

## Objective

Rebuild the `#/cache` view in the web settings page so a non-expert can answer three
questions in under five seconds — *is the assistant resuming fast?*, *how much memory is
this costing me?*, *is anything wrong and can I fix it?* — while keeping the diagnostic
depth for the rare reader who wants it. Do this without a new dependency, without a
build step, and with net-zero or negative change to the embedded asset size.

## What is wrong today

Read off the screenshot and the source (`crates/fono-net/src/web_settings/assets/app.js:2422-2715`,
`crates/fono-net/src/web_settings/assets/app.css:79-141`).

1. **No headline.** The panel opens with a raw fingerprint (`runtime f22f1a6`) and never
   states a verdict. Five separate visual systems — two meters, four chips, a counters
   strip, a disclosure, a tree — are stacked with equal weight, so nothing leads
   (`app.js:2644-2675`).
2. **Red means "normal".** Both bars fill with the danger accent (`app.css:103`). The
   SLOTS bar reads as a half-full alarm at a perfectly healthy 4-of-10; the MEMORY bar is
   a 2 % sliver that conveys nothing at all. Colour is spent before there is a problem to
   report.
3. **Two bars that are not comparable** sit adjacent and look identical. Pins are drawn
   *outside* the budget (`app.js:2470-2475`) — correct, but only legible to someone who
   has read the source comment.
4. **Numbers visibly contradict each other.** "6.9 MiB held" directly above "One saved
   conversation costs 179 MiB" (`app.js:2637-2641`). Both are true — one is current, one
   is worst-case at full context — and nothing on the page says so.
5. **Chips carry trivia, not status.** "1 root" and "2 levels deep" describe an internal
   tree; no user decision turns on them. Their green/grey/amber borders imply a legend
   that does not exist (`app.js:2497-2518`).
6. **Prose essays are always-on.** Three explanatory paragraphs render unconditionally
   (`app.js:2525-2527`, `:2659-2660`, `:2671-2673`). The writing is good; the placement
   makes the panel look like documentation with a widget stuck in it.
7. **Internals leak into the rows.** `f8_system`, `history_prefix`, `f8_chat_prefix` are
   printed beside their own plain-English translation (`app.js:2621-2622`) — pure noise
   for everyone who is not editing the cache code.
8. **The eviction column is a puzzle.** `out #3 / out #4 / out next / out #2` in
   unsorted rows (`app.js:2610`) asks the reader to mentally re-sort the tree to extract
   an ordering they cannot act on.
9. **The most interesting content is tooltip-only.** The prompt preview is reachable only
   by hovering (`app.js:2616-2618`) — undiscoverable, unavailable on touch, invisible to
   a screen reader.
10. **Zero affordances.** The only control is Refresh. Nothing links to the setting that
    governs the budget; nothing lets the user act on any verdict the panel emits.

## Design direction

Invert the hierarchy: **verdict → three numbers → the tree → everything else collapsed.**
Reserve colour for real faults. Keep every existing fact — none is deleted, several move
behind a disclosure.

Target layout, top to bottom:

- **Status line.** One dot plus one sentence, in plain language, e.g. *"Resuming well —
  83% of prompts continued a saved conversation."* Model name and Refresh sit on the same
  row; the runtime fingerprint moves into a `title`.
- **Fault banner (conditional).** Renders only when a verdict is genuinely bad —
  `heads_over_slots`, `fragmented`, `pin_releases > 0`, or re-reads attributed to
  `eviction`. Carries the sentence *and* the suggested action.
- **Three stat tiles.** Large numeral, quiet caption: *prompts resumed* (`restores /
  (restores + cold_prefills)`), *memory used* (`bytes_resident` over `max_bytes + bytes_pinned`,
  with a thin inline track replacing the standalone bar), *conversations kept warm*
  (`floor(max_bytes / checkpoint_bytes)`). Slots demote to a caption under the memory
  tile: *"5 of 11 slots"*.
- **The tree, promoted.** Plain names only; raw layer id moves to the `title`. Preview
  renders inline as a dimmed truncated second line. The fate column marks only *pinned*
  and *next to go*; the remaining ordinals move to the `title`.
- **"What these numbers mean"** — one `<details>`, closed by default, holding the three
  prose paragraphs and the 179-MiB checkpoint-cost explanation, with an added sentence
  reconciling *held* against *costs at full context*.
- **"Detailed counters"** — one `<details>`, closed by default, holding the chips, the
  cold-prefill reasons, the re-read breakdown, and the unplaced-entries list.
- **One link out** to the setting that governs the memory budget.

## Implementation Plan

- [ ] Task 1. **Confirm the fact inventory before moving anything.** Enumerate every field
      of `CacheSnapshot`, `CacheNode`, `CacheVerdicts` and `CacheCountersSnapshot`
      (`crates/fono-core/src/prompt_cache_view.rs:28-114`,
      `crates/fono-core/src/prompt_cache.rs:492-505`) against the redesigned layout, and
      record where each one lands: headline, tile, tree row, disclosure, `title`, or
      deliberately unused. Rationale: the redesign must be a re-ranking, not a data loss;
      `entries_free` and `bytes_free` are already unread and should be noted as such
      rather than silently ignored again.

- [ ] Task 2. **Introduce the verdict function.** Add a single function that maps a
      snapshot onto `{level, sentence, action}` where level is one of good / fair /
      problem, replacing the first-match-wins chain at `app.js:2519-2534`. Fold the
      reuse-rate wording from `app.js:2547-2554` into it so the headline is derived in one
      place. Rationale: the panel currently computes its opinion in two places and
      displays neither at the top.

- [ ] Task 3. **Build the status line and conditional fault banner**, consuming the
      verdict from Task 2 and replacing the model/runtime line at `app.js:2706`. Keep the
      fingerprint reachable via `title`. Rationale: the reader's first fixation must land
      on a sentence, not a hash.

- [ ] Task 4. **Replace `occBar` with the three-tile block.** Retire the second bar; fold
      slots into a caption; fold memory into a thin inline track inside its tile. Delete
      `occBar` (`app.js:2470-2484`) and its call sites (`app.js:2649-2654`) once the tiles
      render. Rationale: two full-width meters is the panel's single largest density cost
      and its main source of false alarm.

- [ ] Task 5. **Recolour.** Change the used-segment and tile accents from the danger colour
      to a neutral/ink tone in `app.css:94-103` and the new tile rules; restrict red to
      the fault banner and to `pin_releases`. Verify against both themes. Rationale: an
      alarm that fires in the healthy case trains the user to ignore it.

- [ ] Task 6. **Clean the tree rows.** In `cacheRow` (`app.js:2604-2628`): drop `.pc-raw`
      from the emitted row and move the layer id into the `title`; render `n.preview`
      inline as a truncated dimmed second line; reduce `.pc-fate` to `pinned` /
      `next to go` / empty, moving the ordinal into the `title`. Adjust `.pc-node` and add
      a preview rule in `app.css:108-133`. Rationale: the tree is the panel's most
      informative element and is currently the one carrying the most noise.

- [ ] Task 7. **Move all prose behind "What these numbers mean."** Relocate the strings at
      `app.js:2525-2527`, `:2659-2660`, `:2671-2673` and `checkpointLine`
      (`app.js:2630-2642`) into one closed `<details>`, and add one new sentence
      reconciling *held now* with *costs at full context*. Keep the empty-cache message
      inline, since with nothing to show the explanation is the content. Rationale:
      explanation is valuable on first visit and noise on the tenth.

- [ ] Task 8. **Move the diagnostics behind "Detailed counters."** Relocate `cacheChips`,
      the residual `cacheCounters` items, `rereadLine` and the unplaced list into one
      closed `<details>`, promoting only the reuse percentage (now a tile) and any
      genuinely bad counter (now the fault banner) out of it. Rationale: preserves every
      diagnostic for the reader who wants it without taxing the reader who does not.

- [ ] Task 9. **Add the one action link** from the memory tile to the setting that governs
      the cache budget, using the existing hash-router and section-open mechanism
      (`app.js:2311-2351`, `app.js:1965-1980`). Rationale: a panel that reports a limit
      should reach the control for that limit.

- [ ] Task 10. **Reconcile the guard tests.** `crates/fono-net/src/web_settings/mod.rs:1176-1179`
      asserts `/api/promptcache` in the JS and `.pc-node` in the CSS — both survive.
      `mod.rs:1326-1381` requires every `chip-*` class painted by the JS to exist in the
      CSS — verify after chips move into the disclosure. `mod.rs:1237-1287` requires every
      rendered `<button>` to be click-handled — the new action link should be an `<a
      href="#/…">` to stay outside that rule, or carry a handled `data-` attribute.
      Rationale: these tests are the substitute for a compiler on untyped JS; a redesign
      is exactly when they earn their keep.

- [ ] Task 11. **Extend the guard set for the new invariants.** Add tests asserting that
      the raw layer id no longer appears in emitted row markup, and that every new
      `pc-*` class the JS paints is styled in `app.css` (generalising the `chip-*` scan at
      `mod.rs:1326-1381`). Rationale: the same drift the existing test catches on the
      actions page is now possible on this page.

- [ ] Task 12. **Verify size and run the full gate.** `nice -n 10 cargo fmt --all --
      check`, `nice -n 10 cargo clippy --workspace --all-targets -- -D warnings`,
      `nice -n 10 cargo test --workspace --tests --lib`, then
      `nice -n 10 ./tests/check.sh --size-budget`. Record the byte delta of `app.js` and
      `app.css`. Rationale: the assets sit uncompressed in `.rodata`, so every added line
      is shipped binary.

- [ ] Task 13. **Refresh the module documentation.** Update the section banner at
      `app.js:2422-2433` to describe the new layout, and correct the stale "tens of KB
      total" claim at `crates/fono-net/src/web_settings/mod.rs:28-29`. Rationale: the
      banner is the map anyone touching this view reads first.

## Optional follow-on slices (not in scope for this plan)

- [ ] Rename the view from *Prompt cache* to *Conversation memory* across the header
      button, the doctor-view link (`app.js:2392`) and the docs, keeping the `#/cache`
      route. Cheap and a real legibility win, but it touches user-facing docs, so it wants
      its own decision.
- [ ] A "Forget saved conversations" button. Genuinely useful, but it needs a new
      endpoint, a new hook, and daemon-side eviction — a behaviour change, not a redesign.

## Verification Criteria

- The panel's first line is a plain-English sentence; no hash, meter or chip renders above it.
- In a healthy snapshot the panel shows no red anywhere.
- A first-time reader can state the reuse rate, the memory in use, and how many
  conversations fit, without opening a disclosure.
- Every fact present in today's panel is still reachable — inline, in a disclosure, or in
  a `title` — with the mapping recorded from Task 1.
- The prompt preview is readable without hovering.
- No raw layer id (`f8_system`, `history_prefix`, `f8_chat_prefix`) appears in rendered
  row text.
- `nice -n 10 cargo test --workspace --tests --lib` passes, including the new guards.
- `nice -n 10 ./tests/check.sh --size-budget` passes and the combined `app.js` + `app.css`
  byte delta is ≤ 0, or a justified small positive.
- Both light and dark themes render the new tiles, track and banner correctly.

## Potential Risks and Mitigations

1. **Information loss disguised as simplification.** Moving a fact behind a disclosure is
   fine; dropping it is a regression the tests will not catch.
   Mitigation: Task 1's mapping table is a precondition for Tasks 4–8, and the
   verification criteria check it explicitly.
2. **Guard tests break in a way that invites weakening them.**
   Mitigation: Task 10 adapts the *page* to the tests, never the reverse; Task 11 adds
   coverage rather than removing it.
3. **Asset size creeps.** Tiles and a second tree line add markup and CSS.
   Mitigation: retiring `occBar`, the raw-id span, three inline prose blocks and the
   duplicated verdict logic should more than pay for it; Task 12 measures rather than
   assumes.
4. **The written rationale in the closed plan is lost.** The reasoning for the current
   layout lives in `plans/closed/2026-08-06-prompt-cache-tree-panel-v1.md` and in dense
   source comments (`app.js:2470-2475`, `:2537-2543`, `:2565-2569`, `:2588-2592`).
   Mitigation: read those before touching each region; carry any comment still true into
   the new code — most of them explain *why the number is what it is*, which the redesign
   does not change.
5. **Theme regression.** Colour changes are the easiest thing to get right in one theme
   and wrong in the other.
   Mitigation: Task 5 explicitly checks both; the CSS already routes through variables.

## Alternative Approaches

1. **Two-level view: a "Summary" tab and an "Advanced" tab.** Cleanest separation, and it
   would let the advanced side stay exactly as it is. Rejected as the primary approach —
   it doubles the render paths and adds a tab control for a page most users will open
   once, where two `<details>` achieve the same split for a fraction of the code.
2. **Leave the layout and fix only the language and colour.** Roughly a tenth of the work
   and it would address the false-alarm red and the worst jargon. Rejected because the
   core defect is ranking, not wording: the panel would still open with a fingerprint and
   still give five systems equal weight.
3. **Server-side rendering of the summary in Rust.** Would let the existing guard tests
   type-check the headline. Rejected — it breaks the module's deliberate "dumb wire
   adapter" design (`mod.rs:24-34`) and puts config semantics into `fono-net`.
4. **A sparkline of reuse rate over time.** The most genuinely useful addition on offer,
   since it turns an instantaneous reading into a trend. Deferred — the daemon keeps no
   history for it today, so it is a backend slice, not a UI one.
