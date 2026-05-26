# Hover-Context Injection — Zero-Friction Implementation Plan

## Objective

Automatically enrich both the Whisper `initial_prompt` and the LLM cleanup prompt based on
which window the user was focused on when they pressed the hotkey — with no user configuration
required. A built-in classifier covers the most common window classes out of the box. The
existing user-configurable `[[context_rules]]` mechanism becomes an override layer on top.

**Constraint**: zero new config fields required for the feature to work. Users who never touch
their config get full context injection automatically. Power users can override or extend via
the existing `[[context_rules]]` schema.

---

## Current State (what already exists)

- `FocusInfo { window_class, window_title }` — `crates/fono-inject/src/focus.rs:6-10`
- `detect_focus()` — X11 path fully implemented; Wayland always returns `None` —
  `crates/fono-inject/src/focus.rs:15-25`
- `FormatContext.rule_suffix` — injected into LLM system prompt via `system_prompt()` —
  `crates/fono-polish/src/traits.rs:36-39`
- `matched_rule_suffix()` — matches user `[[context_rules]]` against focus info —
  `crates/fono/src/session.rs:3074-3095`
- `WhisperLocal::resolve_prompt()` — keyed on language code only, no context awareness —
  `crates/fono-stt/src/whisper_local.rs:127-139`
- User `context_rules` — `Vec<ContextRule>` in config, default empty —
  `crates/fono-core/src/config.rs:700-714`

**Gaps to close**: Wayland focus detection, built-in classifier with no-config defaults,
Whisper hint flowing from context (not just language), `/proc` deep terminal enrichment,
cloud STT `prompt` field threading.

---

## Implementation Plan

### Phase A — Built-in context classifier (the zero-friction core)

- [ ] A.1. Define a `ContextProfile` struct in `fono-core` (or as a new lightweight module
  within `fono-inject`) carrying `whisper_hint: Option<&'static str>` and
  `llm_suffix: Option<&'static str>`. This is the output of the classifier; it is never
  serialised and never stored in config.

- [ ] A.2. Define a static built-in rule table as a `&[BuiltinRule]` — a compile-time
  constant, no heap allocation, no file I/O. Each `BuiltinRule` holds:
  - `classes: &[&str]` — window class names, matched case-insensitively
  - `title_fragments: &[&str]` — optional title substrings for finer matching
  - a reference to a `ContextProfile`

  Minimum built-in profiles to ship:

  | Profile | Classes covered |
  |---|---|
  | `Terminal` | `Alacritty`, `kitty`, `gnome-terminal`, `konsole`, `xterm`, `urxvt`, `foot`, `wezterm`, `tilix`, `st-256color`, `terminator`, `xfce4-terminal`, `lxterminal`, `sakura`, `rxvt-unicode` |
  | `CodeEditor` | `code`, `code-oss`, `vscodium`, `codium`, `kate`, `lapce`, `helix`, `neovide`, `zed` |
  | `TextEditor` | `gedit`, `mousepad`, `xed`, `pluma`, `mousepad`, `geany` |
  | `Browser` | `firefox`, `chromium`, `google-chrome`, `brave-browser`, `librewolf`, `falkon`, `epiphany` |
  | `Email` | `thunderbird`, `evolution`, `kmail`, `geary` |
  | `Chat` | `slack`, `discord`, `telegram-desktop`, `signal-desktop`, `element`, `fractal` |
  | `Spreadsheet` | `libreoffice-calc`, `gnumeric` |
  | `Document` | `libreoffice-writer`, `abiword` |

- [ ] A.3. Implement `ContextClassifier::classify(window_class, window_title) -> Option<ContextProfile>`:
  1. Walk built-in rules; first match wins. Class check is `eq_ignore_ascii_case`. Title
     check is a case-insensitive substring scan against `title_fragments`.
  2. Return `None` on no match (unknown app) — treated as "no enrichment, use base prompts".

- [ ] A.4. Craft the built-in prompt text for each profile. Key design decisions:

  **Terminal `whisper_hint`** (≤ 200 chars, examples Whisper uses as prior tokens):
  > `ls -la, grep -r, chmod 755, git commit, sudo apt install, cd /etc, rm -rf, | grep, > /dev/null, ./script.sh, ssh user@host`

  **Terminal `llm_suffix`**:
  > `The user is dictating shell commands. Output exactly what was said as shell syntax.
  > Use lowercase. Preserve flags verbatim (e.g., -rf, -la, --verbose). Convert spoken
  > paths to filesystem notation (e.g., "home dot config" → ~/.config, "dot slash" → ./,
  > "pipe" → |, "redirect" → >, "dev null" → /dev/null). No prose punctuation. Do not
  > capitalise the first word.`

  **CodeEditor `whisper_hint`**:
  > `function, struct, impl, async, await, const, return, println!, cargo build, git diff`

  **CodeEditor `llm_suffix`**:
  > `The user is dictating code or identifiers. Prefer snake_case for variables, PascalCase
  > for types. No sentence-ending punctuation unless inside a string literal.`

  **Browser `llm_suffix`** (no whisper_hint — vocabulary is too general):
  > `The user is dictating into a web browser. If the content looks like a URL fragment or
  > search query, format it accordingly. Avoid adding punctuation not spoken.`

  **Email `llm_suffix`**:
  > `The user is dictating an email. Use formal punctuation, salutations, and paragraphs.`

  **Chat `llm_suffix`**:
  > `The user is dictating a chat message. Keep it casual. No formal punctuation required.`

---

### Phase B — Wayland focus detection

The X11 path covers XWayland apps even on Wayland desktops (most Electron and Qt apps run
under XWayland). Native Wayland compositors need explicit paths:

- [ ] B.1. **sway / wlroots** path in `detect_focus()`: read `$SWAYSOCK` (or `$I3SOCK`).
  If set, open a Unix socket, send the sway IPC `get_tree` message (type 4), parse the
  minimal JSON to find the focused node's `app_id` and `name` fields. Map `app_id` →
  `window_class`, `name` → `window_title`. Add `swayipc` crate (MIT) or implement the raw
  IPC framing directly (header is 14 bytes: magic `i3-ipc`, u32 length, u32 type) to avoid
  a heavy dependency for what is a simple request/response.

  Priority: **highest** — sway/Hyprland are the dominant tiling compositors for Fono's
  target user base.

- [ ] B.2. **Hyprland** path: read `$HYPRLAND_INSTANCE_SIGNATURE`. If set, call
  `hyprctl activewindow -j` as a subprocess (already acceptable latency at hotkey press
  time; ≤ 5 ms). Parse the JSON `class` and `title` fields. Map directly to `window_class`
  / `window_title`.

  Alternative: use the `hyprland` crate's `ActiveWindow::get_active()`. Evaluate crate size
  vs. subprocess cost; subprocess is safer for a feature this peripheral.

- [ ] B.3. **GNOME Shell** path: call D-Bus `org.gnome.Shell.Introspect.GetWindows()`,
  find the entry with `"is-focused": true`, extract `"wm-class"` and `"title"`. Use
  `zbus` (already a workspace dep, or add if not) for the D-Bus call. Gate behind an
  `if std::env::var("GNOME_SETUP_DISPLAY").is_ok() || desktop_is_gnome()` check.

  Note: GNOME does not expose XWayland window classes through this interface in all
  versions. Acceptable fallback: return `None` for those windows.

- [ ] B.4. **KDE Plasma / Wayland** path: KDE runs many apps under XWayland, so the
  existing X11 path may already fire. For native Wayland clients, `org.kde.KWin.Script`
  or the `PlasmaWindow` protocol are options but require more work. Defer to a follow-up;
  document the gap in `docs/wayland.md`.

- [ ] B.5. Update `detect_focus()` to try paths in order:
  1. If `XDG_SESSION_TYPE == wayland`:
     - Try sway IPC (B.1) if `$SWAYSOCK` or `$I3SOCK` set
     - Try Hyprland (B.2) if `$HYPRLAND_INSTANCE_SIGNATURE` set
     - Try GNOME (B.3) if desktop session looks like GNOME
     - Fall through to X11 attempt anyway (covers XWayland)
  2. X11 path (existing)
  3. Return `FocusInfo::default()` (all None) — graceful degradation

  All paths must be non-blocking and complete within ~10 ms; any error silently returns
  `None` for that path rather than failing the pipeline.

---

### Phase C — Terminal deep enrichment via `/proc`

Triggered only when the classifier identifies a `Terminal` profile. All reads are
synchronous filesystem reads on `/proc` — owned by the same user, no elevated permissions
required.

- [ ] C.1. Extend `FocusInfo` with `Option<u32> window_pid`. Populate it on X11 by reading
  the `_NET_WM_PID` atom from the focused window (same `x11rb` connection already open in
  `x11_focus()`). For Wayland compositors, populate where the compositor provides PID
  (sway's tree JSON includes `pid`; Hyprland's `activewindow` JSON includes `pid`).

- [ ] C.2. Implement `terminal_context(terminal_pid: u32) -> TerminalContext`. Steps:
  1. Read `/proc/[terminal_pid]/task/[terminal_pid]/children` (or parse `/proc/[pid]/stat`
     with `ppid == terminal_pid` scan) to find child process PIDs.
  2. For each child, read `/proc/[child]/comm` to identify a shell (`bash`, `zsh`, `fish`,
     `sh`, `dash`). Take the first match.
  3. Read `/proc/[shell_pid]/cwd` (symlink) → resolve to absolute path.
  4. Probe CWD for project-type markers (in order of specificity):
     - `KUBECONFIG` in `/proc/[shell_pid]/environ`, or `k8s/` subdir, or `*.yaml` with
       `kind: Deployment` → `K8s`
     - `docker-compose.yml` or `Dockerfile` → `Docker`
     - `Cargo.toml` → `Rust`
     - `pyproject.toml` / `setup.py` / `setup.cfg` → `Python`
     - `package.json` → `Node`
     - `go.mod` → `Go`
     - `.git/` exists → `Git` (add git vocabulary even if language unknown)
     - No markers → generic `Shell`

- [ ] C.3. Map `TerminalContext` variant to a refined `ContextProfile` with a more targeted
  `whisper_hint`. Examples:

  | Variant | Additional `whisper_hint` |
  |---|---|
  | `Rust` | `cargo build, cargo test, cargo clippy, rustc, --release` |
  | `Python` | `python3, pip install, pytest, virtualenv, uv run` |
  | `Node` | `npm install, npx, yarn, node_modules, package.json` |
  | `K8s` | `kubectl apply, kubectl get pods, helm install, namespace` |
  | `Docker` | `docker build, docker run, docker compose up, --rm` |
  | `Git` | `git commit, git push, git rebase, git stash, --amend` |

- [ ] C.4. Gate the entire `/proc` enrichment behind a capability check on startup
  (`/proc/[self_pid]/cwd` readable → proceed). Fail silently to `TerminalContext::Shell`
  if any step fails (missing file, permission error, non-Linux platform).

---

### Phase D — Thread context through the STT pipeline

Currently `resolve_prompt()` at `crates/fono-stt/src/whisper_local.rs:127` only considers
language code. The context hint needs to flow alongside it.

- [ ] D.1. Extend the `SpeechToText` trait's `transcribe()` method (or add a parallel
  `transcribe_with_context()`) to accept an `Option<&str> context_hint`. For backends that
  ignore it, the default implementation discards it. Evaluate impact on all backend
  implementations before widening the trait signature — a `TranscribeOptions` struct param
  may be cleaner than extending positional args.

- [ ] D.2. Update `WhisperLocal::resolve_prompt()` to also accept an `Option<&str>
  context_hint`. When both a language prompt and a context hint are present, concatenate
  them separated by a space. When only a context hint is present and the language is
  auto-detect (would normally suppress the prompt), still inject the context hint — it does
  not bias language detection since it contains no spoken words, only representative tokens.

- [ ] D.3. For cloud STT backends that support an equivalent:
  - **OpenAI Whisper API** (`openai-stt`): the `prompt` field in the multipart form body.
    Thread `context_hint` through `GroqSTT` and `OpenAISTT` request builders.
  - **Deepgram**: uses a `keywords` query param (different mechanism, different semantics).
    Map a curated keyword list from the profile rather than the raw hint string.
  - **Other cloud backends**: no equivalent — skip silently.

- [ ] D.4. At the `session.rs` call site, after classifying the context profile, pass
  `profile.whisper_hint` as the `context_hint` argument to `stt_backend.transcribe()`.

---

### Phase E — Thread context through the polish pipeline

The `FormatContext.rule_suffix` field is already the injection point for the LLM. The work
here is wiring the built-in profile's `llm_suffix` into it, merged correctly with any
user-configured `[[context_rules]]` match.

- [ ] E.1. Change `build_format_context()` at `crates/fono/src/session.rs:3052` to accept
  an `Option<&ContextProfile>` in addition to the existing `(app_class, app_title)`. Build
  `rule_suffix` as:
  1. Try `matched_rule_suffix()` against user `[[context_rules]]` first (existing behaviour,
     user overrides win).
  2. If no user rule matched, use `profile.llm_suffix` from the built-in classifier.
  3. If both match, concatenate with `\n` (user rule appended after built-in — user intent
     is additive, not replacing).

- [ ] E.2. No changes needed to `FormatContext`, `system_prompt()`, or any `TextFormatter`
  backend — the `rule_suffix` field already wires straight into the system prompt at
  `crates/fono-polish/src/traits.rs:36-39`.

---

### Phase F — CodeEditor language sub-context from title

Code editors reliably expose the open file name in their window title
(`filename.ext — Visual Studio Code`, `kate — ~/project/src/main.rs`). This gives
language-specific injection for free.

- [ ] F.1. In `ContextClassifier::classify()`, when the primary match is `CodeEditor`,
  scan the window title for known file extensions:
  - `.rs` → Rust sub-profile
  - `.py` → Python sub-profile
  - `.ts` / `.tsx` / `.js` / `.jsx` → TypeScript/JS sub-profile
  - `.go` → Go sub-profile
  - `.java` / `.kt` → JVM sub-profile
  - `.sql` → SQL sub-profile
  - `.md` / `.rst` → Prose sub-profile (full punctuation, no identifier casing)

- [ ] F.2. Each sub-profile overrides only `whisper_hint` and adjusts `llm_suffix`
  minimally (casing convention). The base `CodeEditor` profile applies when no extension is
  detected.

---

### Phase G — Privacy guard for sensitive windows

- [ ] G.1. Define a `Private` built-in profile with `whisper_hint: None`,
  `llm_suffix: None`, and a `suppress_history: true` flag. Apply it to classes:
  `keepassxc`, `bitwarden`, `1password`, `gnome-keyring`, `seahorse`, `pass` (the
  terminal password manager, detected by title pattern `pass`).

- [ ] G.2. When `suppress_history` is set in the profile, skip writing the transcription
  to the SQLite history DB and skip the `redact_secrets` pass entirely (no data persisted).
  This is the *opposite* of context injection — the window context tells us to do *less*,
  not more.

---

### Phase H — Snapshot semantics and toggle-mode correctness

- [ ] H.1. Context classification must happen at **hotkey-press time** (recording start),
  not at paste time. The focused window may change during a long dictation in toggle mode.
  Capture the `ContextProfile` as part of the `Session` struct and carry it through to STT
  and polish — already the case since `focus.probe()` is called at the start of each
  session, but confirm the profile snapshot is stored and not re-evaluated at paste time.

- [ ] H.2. The Wayland detection paths (B.1–B.3) must not block the hotkey response. If
  a Wayland IPC call takes more than ~20 ms, fall through to `None` rather than delaying
  the recording start. Use `tokio::time::timeout` around each async path.

---

## Verification Criteria

- On X11 with Alacritty focused: `fono` transcribes `"cd home dot config fono"` as
  `cd ~/.config/fono` without any user configuration.
- On X11 with a Rust file open in VS Code (title contains `.rs`): identifiers are
  transcribed in snake_case and `cargo` commands are recognised cleanly.
- On sway (Wayland): `detect_focus()` returns a non-None `window_class` when a terminal
  emulator is focused.
- On Hyprland: same as above via the `hyprctl` path.
- On an unknown window class: pipeline behaves identically to current behaviour (base
  prompts only, no error).
- With a user `[[context_rules]]` entry for `firefox`: user rule takes precedence over the
  built-in `Browser` profile.
- With KeePassXC focused: transcription is not written to history DB.
- `/proc` enrichment identifies a Rust project (has `Cargo.toml` in CWD) and injects
  `cargo build, cargo clippy` into the Whisper hint.
- `cargo clippy --workspace --all-targets -- -D warnings` passes with all new code.
- No new config fields required; existing configs continue to work unchanged (all new
  built-in logic is additive).

---

## Potential Risks and Mitigations

1. **Wayland IPC latency delaying hotkey response**
   Mitigation: wrap each Wayland path in a tight timeout (Phase H.2). Miss the window
   → fall through to None, recording starts immediately. Context is a best-effort
   enhancement, never a blocking requirement.

2. **`_NET_WM_PID` absent on some X11 applications**
   Mitigation: `/proc` enrichment is gated on PID availability. If `_NET_WM_PID` is
   not set, skip the deep enrichment silently — generic Terminal profile still applies.

3. **Whisper `initial_prompt` length limit**
   The whisper.cpp encoder context is 1500 mel frames (30 s). The initial_prompt is
   tokenised and prepended; the practical useful limit is ~200 tokens (~150 words).
   All built-in hints are budgeted at ≤ 120 chars to leave headroom for merged
   language + context hints.
   Mitigation: enforce a character budget in `resolve_prompt()` and truncate with a
   `tracing::warn!` if exceeded.

4. **Built-in LLM suffix conflicting with the user's main prompt**
   The LLM sees: main_prompt → advanced_prompt → dictionary → rule_suffix. If the
   built-in suffix contradicts the user's main prompt (e.g., user has a custom formal
   prose prompt; built-in says "no punctuation" for terminal), the LLM may behave
   inconsistently.
   Mitigation: write built-in suffixes as *additive* instructions ("also apply X") rather
   than overrides ("instead do X"). Test against the default prompts to confirm no
   contradiction.

5. **`/proc` walk being slow on systems with many processes**
   Reading `/proc/[pid]/children` is a single file read, not a full `/proc` scan.
   Should complete in < 1 ms on any system.
   Mitigation: measure in a microbenchmark before shipping; add a 5 ms timeout guard.

6. **GNOME Shell D-Bus call failing or being slow**
   The `org.gnome.Shell.Introspect` interface was added in GNOME 3.34. Older GNOME
   releases will return a D-Bus error.
   Mitigation: wrap in `timeout(10ms)` and catch all errors; log at `TRACE` level only.

---

## Alternative Approaches

1. **Shell integration hook (opt-in)**: Users add a `PROMPT_COMMAND` hook that writes
   current shell state to a temp file Fono reads. More accurate than `/proc` walking, but
   requires user action — violates the zero-friction requirement. Could be offered as an
   opt-in power-user enhancement on top of Phase C.

2. **Accessibility API (AT-SPI)**: Works cross-compositor and cross-toolkit, can read
   focused widget type (text field, terminal, URL bar). But AT-SPI requires the
   accessibility service to be running (not default on many distros) and is significantly
   heavier than window-class detection. Best reserved for a future "deep context" tier.

3. **User-trained classifier**: Let users label a few sessions ("this was terminal, this
   was email") and train a tiny classifier. Far too much friction to set up, and the
   built-in rule table already covers 90% of cases correctly.

4. **Context injection only in LLM phase (skip Whisper)**:
   Simpler implementation, no STT trait changes. Would still deliver most of the value
   since the LLM is better at semantic reconstruction. The Whisper injection is additive
   and worth doing but could be deferred to a v2 if the STT trait change proves complex.
