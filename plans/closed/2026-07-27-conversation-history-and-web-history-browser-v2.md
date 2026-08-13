# Conversation History Persistence + Web History Browser

## Status: Completed (all 20 tasks landed; not recorded in docs/status.md at the time)

## Objective

Two user-facing deliverables:

1. **Speaker attribution in conversation history** — show which enrolled speaker was
   detected for a given turn, when verification produced a match.
2. **A `#/history` page in the existing web settings UI** — browse both dictation
   transcriptions and assistant conversations in the browser at
   `http://127.0.0.1:10808/#/history`, mirroring how `#/doctor` already works.

Delivering (1) for assistant turns requires the assistant conversation store, since
assistant history currently never reaches disk. Delivering (2) requires that store
plus a read-only API surface.

**Explicitly out of scope:** new CLI subcommands. The browser page is the interface.
Also out of scope: merging the four SQLite files, or collapsing the
`.config` / `.cache` / `.local/share` / `.local/state` split — both are correct as
they stand (rationale recorded in the Audit Notes below).

## Findings that shape the plan

### Dictation speaker attribution already exists and works
`Transcription` carries a `speaker` field (`crates/fono-core/src/history.rs:31-36`),
the column is in the schema, `insert` writes it
(`crates/fono-core/src/history.rs:189-205`), and both read paths select it
(`crates/fono-core/src/history.rs:233-260`). The daemon populates it on both the live
and batch dictation paths (`crates/fono/src/session.rs:4652`,
`crates/fono/src/session.rs:5101`). The doc comment already states the correct privacy
posture: only the name is stored, never the embedding.

**Implication:** for dictation, requirement (1) is a *display* task only — the data is
there, nothing surfaces it.

### The assistant already knows the speaker but discards it
`assistant_speaker` is resolved per turn (`crates/fono/src/session.rs:3055-3058`), fed
into the system prompt (`crates/fono/src/session.rs:3329-3333`) and carried on
`AssistantTurnInputs.speaker` (`crates/fono/src/session.rs:3341`, type at
`crates/fono/src/assistant.rs:143`). But assistant history lives only in the in-memory
buffer on `AssistantSessionState` (`crates/fono/src/session.rs:679-683`) and is
dropped by `on_assistant_forget` (`crates/fono/src/session.rs:3445-3452`). Nothing is
written to disk.

**Implication:** requirement (1) for assistant turns, and the assistant half of
requirement (2), both need a new persisted store. The speaker value itself needs no new
detection work — just a destination.

### The web UI already has exactly the pattern needed
Hash routing lives at `crates/fono-net/src/web_settings/assets/app.js:2297-2310`, with
`currentView()` at line 2301 and view toggling at lines 2304-2305. View containers are
declared in `crates/fono-net/src/web_settings/assets/index.html:23` and `:32`. The
doctor page is a self-contained render function
(`crates/fono-net/src/web_settings/assets/app.js:2348+`) fed by a single token-gated
route `GET /api/doctor` (`crates/fono-net/src/web_settings/mod.rs:517-520`), wired
through a hook on `WebSettingsHooks` (`crates/fono-net/src/web_settings/mod.rs:230`).

**Implication:** `#/history` is an additive change following a proven three-part
shape — a hook field, a route arm, a render function. Low risk.

### Audit notes (asked and answered; no action)
- The six tables in `history.sqlite` are one real table plus five FTS5 shadow tables
  SQLite creates and manages itself (`crates/fono-core/src/history.rs:113`). Not
  reducible without dropping search.
- The multi-directory layout (`crates/fono-core/src/paths.rs:44-53`, Windows
  equivalent at `:66-87`) is the XDG spec and encodes backup semantics: cache holds
  gigabytes of re-downloadable models, config and data must be preserved.
- The four DB files carry four different security/lifecycle profiles: transcripts
  (retention-purged), credentials (must survive a history wipe), biometric
  voice-prints (independent erasure), tool catalogue (disposable).
- **Real inconsistency found:** `tools.sqlite` is mode `0644` while every sibling is
  `0600`; its `device_name` / `place_name` tables disclose the user's smart-home
  topology to any local account. Included as a small task below.
- `notes_db()` (`crates/fono-core/src/paths.rs:142-144`) is declared with no store and
  no file on disk. Included as a small cleanup task.

## Assumptions

- Assistant conversations go in a new `conversations.sqlite` in the data dir,
  following the one-file-per-concern pattern, clamped `0600`, with its own retention.
- Threads are segmented by an idle timeout plus explicit "forget", matching how the
  in-memory rolling window already behaves.
- Persistence is on by default, disableable in config; when disabled, no file is
  created at all.
- The history page is **read-only plus delete**. No editing.
- Both history APIs are token-gated `/api/*` routes like every other, and are
  loopback-trusted by the existing auth path
  (`crates/fono-net/src/web_settings/mod.rs:271-280`).
- Speaker display is name-only. Voice embeddings never cross the wire, consistent
  with the existing speakers API note (`crates/fono-net/src/web_settings/mod.rs:238-240`).
- No new crates. `rusqlite`, `hyper`, and `serde_json` are all already linked; the UI
  is vanilla JS in existing asset files.

## Implementation Plan

### Part A — assistant conversation store

- [x] Task 1. **Write an ADR for assistant conversation persistence.** Record what is
  now written to disk (spoken user turns, assistant replies, tool calls, detected
  speaker names), where, the `0600` clamp, redaction reuse, retention default, thread
  segmentation rule, and the opt-out. Rationale: this changes what Fono durably
  records about the user; every prior storage decision in this project has an ADR and
  this one most needs the paper trail. Fold in the "we deliberately did not merge the
  DBs or the XDG dirs" conclusions so that question is settled by reference.
  *Status: DONE*

- [x] Task 2. **Add `conversations_db()` to `Paths`.** Place it beside the existing DB
  accessors in `crates/fono-core/src/paths.rs`, doc-commented in the same style as
  `api_keys_db()` and `speakers_db()` (desktop vs system-service location, `0600`
  clamp). Extend the layout assertion in `rooted_at_produces_expected_layout`
  (`crates/fono-core/src/paths.rs:277-285`). Rationale: every DB path resolves through
  one place; bypassing it breaks both the test harness and the `/var/lib/fono` system
  service deployment.
  *Status: DONE*

- [x] Task 3. **Design the conversation schema with speaker as a first-class column.**
  A `thread` table (id, started/ended timestamps, assistant backend, model, originating
  app class/title, turn count) and a `turn` table (id, thread id, ordinal, role, kind,
  text, timestamp, **speaker**, latency). Store the speaker per *turn*, not per thread —
  a conversation can involve more than one person, and that is precisely the
  information the user asked to see. Include a turn-kind discriminator so tool calls
  and their results are distinct rows rather than being flattened into assistant text.
  Include a schema-version marker. Rationale: threads are what the user recalls, turns
  are what the model consumes on resume, and tool calls are the audit-relevant part;
  flattening any of the three loses unrecoverable information.
  *Status: DONE*

- [x] Task 4. **Implement `ConversationStore` in `fono-core`.** Mirror the shape of
  `crates/fono-core/src/history.rs` and `crates/fono-core/src/speakers.rs`:
  `open`/`open_in_memory`, idempotent schema creation, `0600` clamp, secret redaction
  on insert reusing `history::redact` (`crates/fono-core/src/history.rs:287`), and
  `purge_older_than` matching the signature at
  `crates/fono-core/src/history.rs:209-217`. Operations: open thread, append turn,
  close thread, load a thread's turns, list recent threads (with a text preview and
  the distinct speakers seen), delete a thread. Rationale: reusing the established
  store shape keeps the review surface small and inherits the security behaviours
  rather than reinventing them.
  *Status: DONE*

- [x] Task 5. **Add config keys for conversation persistence.** Enable/disable,
  retention days, and the idle timeout that closes a thread — following the existing
  history-retention pattern in `crates/fono-core/src/config.rs`. Rationale:
  transcripts of spoken conversations are more sensitive than dictation snippets;
  users wanting zero disk footprint must be able to get exactly that, and the absence
  of the file is the only credible proof.
  *Status: DONE*

- [x] Task 6. **Open the store in the daemon and thread it into the session.** Open it
  where `HistoryDb` is opened (`crates/fono/src/session.rs:866`) and add the handle to
  `AssistantSessionState` alongside the existing in-memory buffer. RAM stays
  authoritative for prompt construction; the store is a write-through sink. Rationale:
  keeps disk I/O off the latency-critical assistant path while still guaranteeing
  durability.
  *Status: DONE*

- [x] Task 7. **Persist turns, carrying the already-resolved speaker.** Write the user
  turn when the transcript is final and the assistant turn when generation completes
  or is cancelled (flagging partial turns). Pass `assistant_speaker`
  (`crates/fono/src/session.rs:3058`) straight through to the turn row — it is already
  computed and already handed to `AssistantTurnInputs.speaker`
  (`crates/fono/src/session.rs:3341`), so this is a wiring change, not new detection.
  Cover both the push-to-talk and full-duplex live paths, and record tool
  invocations as they occur. Rationale: cancelled turns are exactly what a user wants
  to review afterwards, and the speaker value already exists — discarding it is the
  only reason requirement (1) isn't already met for the assistant.
  *Status: DONE*

- [x] Task 8. **Implement idle-timeout thread segmentation and redefine "forget".**
  Close the open thread after the configured idle interval so the next utterance
  starts a new one; close cleanly on daemon shutdown; treat a crash-orphaned open
  thread as valid and resumable rather than corrupt. Update `on_assistant_forget`
  (`crates/fono/src/session.rs:3445-3452`) to close the current thread rather than
  only clearing RAM. Recommendation: "forget" ends the thread but does **not** delete
  it; deletion is an explicit action on the history page. Rationale: without
  segmentation the store degenerates into one unbounded thread; and silently turning a
  familiar "fresh start" control into "destroy history" would be a surprising
  regression.
  *Status: DONE*

- [x] Task 9. **Resume the most recent thread on startup when within the idle window.**
  Rehydrate the in-memory buffer from it, respecting the configured rolling-window
  size (see the window re-tune at `crates/fono/src/session.rs:1260`). Rationale: this
  is the user-visible payoff — the difference between "Fono restarted and forgot
  everything" and "Fono picked up where we left off".
  *Status: DONE*

- [x] Task 10. **Hook conversation retention into the existing purge cycle.** Call the
  new `purge_older_than` wherever dictation-history retention already runs, using the
  conversation-specific setting. Rationale: an unbounded store grows forever; reusing
  the existing scheduled purge avoids a second cleanup mechanism.
  *Status: DONE*

### Part B — the `#/history` web page

- [x] Task 11. **Add read-only history hooks to `WebSettingsHooks`.** Extend the struct
  at `crates/fono-net/src/web_settings/mod.rs:223-261` with closures for: list recent
  transcriptions (with speaker), search transcriptions via the existing FTS5 path
  (`crates/fono-core/src/history.rs:233`), list recent conversation threads, load one
  thread's turns, and delete a thread. Follow the existing closure type-alias
  conventions established for the doctor and speakers hooks. Rationale: the server is
  deliberately a thin wire adapter with no storage semantics
  (`crates/fono-net/src/web_settings/mod.rs:220-222`); keeping the DB access
  daemon-side preserves that boundary.
  *Status: DONE*

- [x] Task 12. **Add the `/api/history/*` route arms.** Following the `/api/doctor`
  arm at `crates/fono-net/src/web_settings/mod.rs:517-520` and the grouped-dispatch
  pattern used for speakers and tools (`:501-506`), add a token-gated group:
  transcription list/search, thread list, thread detail, thread delete. Extract into a
  `route_history` helper to stay under clippy's `too_many_lines`, exactly as
  `route_api_keys` does (`crates/fono-net/src/web_settings/mod.rs:554-556`). Rationale:
  matching the established route shape means auth, error mapping, and JSON responses
  are all inherited rather than re-implemented.
  *Status: DONE*

- [x] Task 13. **Implement the hooks in the daemon layer.** Wire them where the
  speakers and tools hooks are already constructed (around
  `crates/fono/src/daemon.rs:4881-5013`), reading through `HistoryDb` and the new
  `ConversationStore`. Return the speaker name plainly; never expose embeddings.
  Rationale: this is where the existing store handles live, so the hooks are short
  closures rather than new plumbing.
  *Status: DONE*

- [x] Task 14. **Add the `view-history` container and hash route.** Add the container
  to `crates/fono-net/src/web_settings/assets/index.html` beside `view-doctor` (`:32`),
  extend `currentView()` (`crates/fono-net/src/web_settings/assets/app.js:2301`) to
  recognise `#/history`, and extend the toggle at `:2304-2305`. Note the existing
  comment at `:2299-2300`: hash routing is deliberate because it preserves `?token=…`
  across navigation — a real path would drop it. Rationale: additive, follows the
  proven pattern, and inherits token survival for free.
  *Status: DONE*

- [x] Task 15. **Build the history view.** Two tabs or sections — Dictation and
  Conversations. Dictation: newest-first list showing time, text (cleaned with raw
  available), app, backend, language, and **speaker when present**, plus a search box
  wired to the FTS5 route. Conversations: thread list with time, turn count, backend
  and the speakers involved; clicking a thread expands its turns with per-turn role
  and speaker, tool calls rendered distinctly. Render speaker absence neutrally
  (verification may simply be off) rather than as an error. Include a delete control
  per thread with an explicit confirm. Reuse the existing CSS vocabulary in
  `app.css` — no new styling system. Rationale: this is the single deliverable the
  user actually asked for; everything above exists to make it possible.
  *Status: DONE*

- [x] Task 16. **Add a navigation entry point.** A link to `#/history` beside the
  existing doctor affordance, and a "← Settings" back link inside the history view
  mirroring `crates/fono-net/src/web_settings/assets/app.js:2350`. Rationale: a page
  reachable only by typing the URL is not discoverable.
  *Status: DONE*

### Part C — small consistency fixes found during the audit

- [x] Task 17. **Clamp `tools.sqlite` to `0600`.** Apply the same clamp the other
  stores use, in `crates/fono-core/src/tool_catalog.rs`, tightening pre-existing
  `0644` files on open. Verify the headless system service (running as user `fono`)
  still reads it. Note the behavioural change in `CHANGELOG.md`. Rationale: the
  `device_name` / `place_name` tables disclose the user's smart-home topology to any
  local account; the `0600` posture applied to every sibling store belongs here too.
  *Status: DONE*

- [x] Task 18. **Resolve `notes.sqlite`.** Decide whether `notes_db()`
  (`crates/fono-core/src/paths.rs:142-144`) backs a planned feature. If so, doc-comment
  it with the owning slice; if not, remove the accessor. Rationale: a path accessor
  with no store and no file is a standing source of confusion.
  *Status: DONE*

### Part D — verification

- [x] Task 19. **Add tests.** Store level: schema idempotency, `0600` clamp on fresh
  and pre-existing files, turn append and ordering, **speaker round-trip on both
  dictation and conversation turns**, idle segmentation, resume honouring the
  rolling-window size, retention purge at the boundary, redaction of secrets in
  persisted turns, and disabled-persistence creating no file. Server level: extend
  `crates/fono-net/tests/web_settings_round_trip.rs` to cover the new routes including
  auth gating and thread delete. Rationale: the store touches privacy-relevant
  behaviour, so the ADR's guarantees must be mechanically enforced.
  *Status: DONE*

- [x] Task 20. **Update docs and run the gates.** Document the new file, its config
  keys, and the history page; add the `CHANGELOG.md` entry (including the
  `tools.sqlite` permission change); update `docs/status.md`. Then run, in order, each
  under `nice -n 10`: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`, and `./tests/check.sh --size-budget`.
  Rationale: these gates are mandatory per project rules, and a new store plus UI
  assets is exactly the kind of change that can move the binary-size needle.
  *Status: DONE*

## Verification Criteria

- `http://127.0.0.1:10808/#/history` loads and lists dictation transcriptions
  newest-first, with the detected speaker shown on rows that have one.
- The same page lists assistant conversation threads; opening one shows its turns with
  per-turn role and detected speaker.
- A conversation held with speaker verification enabled shows the correct speaker name
  on both the user turns and the thread summary.
- With verification off, speaker fields render neutrally as absent — not as an error.
- Searching dictation history from the page returns FTS5-matched results.
- Deleting a thread from the page removes it; a page refresh confirms it is gone.
- A daemon restart within the idle window resumes the prior conversation context; a
  restart outside it starts a new thread while the old one stays browsable.
- Cancelling an assistant turn mid-generation persists a turn flagged partial with the
  text produced up to cancellation.
- Tool invocations appear as distinct rows with arguments and results.
- `conversations.sqlite` is mode `0600`; `tools.sqlite` is `0600` after upgrade from a
  pre-existing `0644` file.
- With persistence disabled, no `conversations.sqlite` is ever created and the
  Conversations section reports that persistence is off.
- Navigating to `#/history` with `?token=…` preserves the token, as `#/doctor` does.
- The new routes reject unauthenticated non-loopback requests.
- No voice embedding appears in any API response.
- All four gates pass: fmt, clippy with `-D warnings`, workspace tests, size budget.

## Potential Risks and Mitigations

1. **Privacy regression — Fono starts durably recording spoken conversations that
   previously evaporated on restart, now with speaker names attached.**
   Mitigation: document prominently, clamp `0600`, reuse the existing redaction path,
   ship a finite default retention, guarantee the opt-out creates no file, and store
   only speaker *names* (never embeddings), matching the posture already documented at
   `crates/fono-core/src/history.rs:31-36`.

2. **The history page exposes transcripts to anyone who can reach port 10808.**
   Mitigation: token-gate the routes like every other `/api/*` route and rely on the
   existing loopback-only default (`crates/fono-net/src/web_settings/mod.rs:271-280`).
   Add explicit round-trip tests for the auth gate rather than assuming inheritance.

3. **Latency regression on the assistant hot path from synchronous writes.**
   Mitigation: keep RAM authoritative for prompt construction; write only at turn
   boundaries, never mid-token. Measure the push-to-talk path before and after.

4. **Binary size growth past the 25 MiB `cpu` budget.**
   Mitigation: no new crates — `rusqlite`, `hyper`, `serde_json` are already linked and
   the UI is vanilla JS in existing files. Defer FTS5 search over *conversations* to a
   later slice; dictation search reuses the index that already exists. Run the
   size-budget gate before pushing, per project rule.

5. **Rendering a very long thread or a large transcript list stalls the browser.**
   Mitigation: server-side limits on both list routes with pagination, mirroring the
   `limit` parameter already present on `recent`
   (`crates/fono-core/src/history.rs:250`).

6. **Thread segmentation feels wrong in practice — threads split mid-thought or run
   unrelated topics together.**
   Mitigation: make the idle timeout configurable with a conservative default, keep
   explicit "forget" as the reliable manual boundary, and revisit the default after
   real use rather than tuning speculatively.

7. **Clamping `tools.sqlite` to `0600` breaks the headless system service.**
   Mitigation: verify that deployment explicitly, tighten on open rather than failing,
   and call the change out in `CHANGELOG.md`.

8. **Speaker names shown in history become misleading if a speaker is later renamed or
   deleted.**
   Mitigation: store the name as it was at turn time (a historical fact, not a foreign
   key) — consistent with how dictation history already behaves, and it means deleting
   a speaker's biometric enrollment does not silently rewrite past history.

## Alternative Approaches

1. **Skip persistence; show only the in-memory conversation on the history page.**
   Much smaller change. Trade-off: the page would be empty after every restart, and
   requirement (1) for assistant turns would still be unmet. Rejected — it does not
   deliver what was asked.

2. **Put conversation tables inside `history.sqlite`.** One fewer file, one
   connection, and conversations arguably are "history". Trade-off: entangles two
   different retention policies and makes "purge my dictation history" ambiguous with
   respect to assistant conversations. Rejected; the extra file is cheap.

3. **Serve history as a plain server-rendered HTML page instead of extending the SPA.**
   Avoids touching `app.js`. Trade-off: diverges from the established hash-routed
   pattern, and a real path would drop the `?token=…` the UI depends on
   (`crates/fono-net/src/web_settings/assets/app.js:2299-2300`). Rejected.

4. **Add CLI subcommands as well as the page.** More scriptable. Trade-off: the user
   explicitly asked not to over-complicate with CLI tooling. Excluded by request; the
   store API leaves it available later at low cost.

5. **Persist only on explicit "keep this conversation".** Strongest privacy posture.
   Trade-off: the decision arrives too late — by the time you know you wanted it kept,
   the daemon has restarted. Rejected as a default; viable as an opt-in mode.
