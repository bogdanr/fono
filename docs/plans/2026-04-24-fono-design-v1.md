# Fono — Lightweight Native Voice Dictation

## Objective

Design, build, and ship **Fono**: a lightweight, GPL-3.0, native single-binary voice-dictation
tool for Linux (with Windows/macOS as follow-on targets). Fono supersedes heavy stacks like
Tambourine (Tauri + Python/Pipecat) and OpenWhispr (Electron) by delivering the feature union
of both in a single Rust binary that self-configures on first run, statically links everything
it needs, and integrates with any desktop (i3, sway, KDE, Hyprland, XFCE, GNOME, Wayland, X11).

**v0.1** = Tambourine-parity dictation (hotkey → STT → LLM cleanup → paste + tray + history +
personal dictionary + per-app context).
**v0.2** = meeting transcription with on-device speaker diarization.
**v0.3** = notes store with folders + semantic search.
**v0.4** = local REST API + MCP server.

## Core Constraints (from user feedback, non-negotiable)

- **Language:** Rust.
- **Name:** Fono. Binary = `fono`. Crate = `fono`.
- **License:** GPL-3.0-only (pure GPLv3, not LGPL, not AGPL).
- **Delivery:** single static-musl ELF downloadable from GitHub Releases; runs on any x86_64
  Linux kernel ≥ 3.2 with zero runtime dependencies.
- **First run:** interactive wizard that asks "local models or cloud APIs?" and offers ≥ 2
  options under each branch.
- **Multilingual** STT and LLM models in the Balanced tier defaults.
- **Runs well on light distros** (NimbleX, Alpine, Void, Artix) under i3/sway/KDE; must work
  under both X11 and Wayland sessions.

## Architecture Overview

### Workspace layout (cargo workspace)

```
fono/
├── Cargo.toml                 # workspace root
├── LICENSE                    # GPL-3.0-only
├── README.md
├── CHANGELOG.md
├── crates/
│   ├── fono/                  # bin: entry point, CLI, first-run wizard
│   ├── fono-core/             # lib: config, errors, DB schema, paths
│   ├── fono-audio/            # lib: cpal capture, VAD, resampling
│   ├── fono-stt/              # lib: STT trait + local + cloud backends
│   ├── fono-llm/              # lib: LLM trait + local + cloud backends
│   ├── fono-hotkey/           # lib: global-hotkey wrapper + hold/toggle FSM
│   ├── fono-inject/           # lib: enigo + Wayland fallback (wtype/ydotool)
│   ├── fono-tray/             # lib: tray-icon wrapper, menu
│   ├── fono-overlay/          # lib: minimal winit/softbuffer recording indicator
│   ├── fono-ipc/              # lib: Unix-socket IPC between daemon and CLI
│   └── fono-download/         # lib: HuggingFace model downloader w/ progress
├── assets/
│   ├── fono.png               # tray + overlay icon (SVG source + rasterised)
│   └── fono.desktop
├── packaging/
│   ├── slackbuild/            # NimbleX SlackBuild (mirrors earlyoom layout)
│   ├── arch/PKGBUILD
│   ├── nix/default.nix
│   ├── debian/                # .deb control files
│   └── systemd/fono.service   # optional user-session autostart
├── docs/
│   ├── architecture.md
│   ├── providers.md           # STT + LLM provider matrix
│   ├── wayland.md             # Wayland caveats + setup
│   └── contributing.md
└── .github/
    └── workflows/
        ├── ci.yml             # test + clippy + fmt on Linux/Mac/Win
        └── release.yml        # cross-compile musl/aarch64/win/mac, draft release
```

### Runtime model

Single process:

```
   ┌──────────────────────────────────────────────┐
   │                  fono daemon                  │
   │  ┌────────────┐  ┌────────────┐  ┌─────────┐ │
   │  │  hotkeys   │  │   tray     │  │  IPC    │ │
   │  │ (FSM)      │  │ (SNI)      │  │ socket  │ │
   │  └─────┬──────┘  └────────────┘  └────┬────┘ │
   │        ▼                                ▼     │
   │  ┌──────────────────────────────────────────┐ │
   │  │        session orchestrator (tokio)      │ │
   │  └─┬────────┬────────┬────────┬────────┬────┘ │
   │    ▼        ▼        ▼        ▼        ▼      │
   │  audio    STT      LLM     inject   overlay   │
   │  (cpal) (whisper  (llama  (enigo)  (winit+    │
   │         -rs or    -rs or                      │
   │         HTTPS)    HTTPS)           softbuffer)│
   └──────────────────────────────────────────────┘
                          │
                          ▼ writes
               SQLite (history, dict, config)
```

Subcommand CLI `fono <verb>` talks to the running daemon via a Unix socket at
`$XDG_STATE_HOME/fono/fono.sock` for cases where a window manager binds a shortcut to e.g.
`fono toggle` instead of using Fono's own global hotkey. The same binary is both the daemon
and the client.

### Key crate choices (stable, battle-tested, permissive-licensed)

| Purpose | Crate | License | Rationale |
|---|---|---|---|
| Async runtime | `tokio` | MIT | Streaming STT, cloud HTTP, IPC |
| CLI | `clap` v4 (derive) | MIT/Apache | Standard |
| Config | `serde` + `toml` | MIT/Apache | TOML for human-editable config |
| Audio capture | `cpal` | Apache | ALSA/Pulse/JACK/PipeWire/WASAPI/CoreAudio |
| VAD | `webrtc-vad` (Rust bindings) or `silero-vad` via ORT | BSD / MIT | Silero-vad via `ort` for quality; webrtc-vad as lightweight fallback |
| Resampling | `rubato` | MIT | 48 kHz → 16 kHz for whisper input |
| STT local | `whisper-rs` | MIT | whisper.cpp FFI, vendored C++ |
| STT cloud HTTP | `reqwest` + `rustls` | MIT/Apache | No OpenSSL; truly static |
| STT cloud streaming | `tokio-tungstenite` + `rustls` | MIT | Deepgram / AssemblyAI WebSocket |
| LLM local | `llama-cpp-2` | MIT | Mature llama.cpp bindings |
| SQLite | `rusqlite` (bundled) | MIT | Static-linked SQLite amalgamation |
| HTTP | `reqwest` (rustls-only feature) | MIT/Apache | See above |
| Tray | `tray-icon` | Apache | SNI + XEmbed + Win + Mac |
| Global hotkey | `global-hotkey` | Apache | X11 + Win + Mac; Wayland via portal or compositor hooks |
| Text injection | `enigo` | MIT/Apache | X11 (libxdo vendored), Win, Mac |
| Wayland inject fallback | spawn `wtype` / `ydotool` | — | Optional, autodetected |
| Overlay window | `winit` + `softbuffer` | Apache/MIT | Minimal, works X11 + Wayland + Win + Mac |
| Window focus detection | `x11rb` (X11) / `swayipc` / `hyprland` (Wayland) | MIT/Apache | For per-app context prompts |
| Notifications | `notify-rust` | MIT | Errors / setup prompts |
| Progress bars | `indicatif` | MIT | First-run download UI |
| Paths | `directories` (XDG) | MIT/Apache | ~/.config, ~/.local/share, ~/.cache |
| Logging | `tracing` + `tracing-subscriber` | MIT | Structured logs to file |
| Errors | `anyhow` + `thiserror` | MIT/Apache | Standard Rust error idioms |

### Paths (XDG-compliant)

| Purpose | Path |
|---|---|
| Config | `~/.config/fono/config.toml` |
| Secrets (API keys) | `~/.config/fono/secrets.toml` (mode 0600) |
| History DB | `~/.local/share/fono/history.sqlite` |
| Notes DB (v0.3+) | `~/.local/share/fono/notes.sqlite` |
| Whisper models | `~/.cache/fono/models/whisper/` |
| LLM models | `~/.cache/fono/models/llm/` |
| Sherpa-onnx models (v0.2+) | `~/.cache/fono/models/sherpa/` |
| IPC socket | `$XDG_STATE_HOME/fono/fono.sock` |
| Log | `$XDG_STATE_HOME/fono/fono.log` |
| PID file | `$XDG_STATE_HOME/fono/fono.pid` |

### Config schema (TOML, first-run wizard populates this)

```toml
version = 1

[general]
language = "auto"                    # "auto" | BCP47 code e.g. "ro" "en" "es"
startup_autostart = false
sound_feedback = true
auto_mute_system = true              # mute other apps while dictating

[hotkeys]
hold = "Ctrl+Alt+Grave"              # press-and-hold
toggle = "Ctrl+Alt+Space"            # press to start, press to stop
paste_last = "Ctrl+Alt+Period"
cancel = "Escape"                    # while recording

[audio]
input_device = ""                    # "" = system default
sample_rate = 16000
vad_backend = "silero"               # "silero" | "webrtc" | "none"

[stt]
backend = "local"                    # "local" | provider name
[stt.local]
model = "small"                      # tiny | tiny.en | base | base.en | small | small.en | medium | medium.en
quantization = "q5_1"
language = "auto"

[stt.cloud]
# populated when user picks cloud path
# provider = "groq"
# api_key_ref = "GROQ_API_KEY"       # name of env var OR key in secrets.toml
# model = "whisper-large-v3"

[llm]
enabled = true
backend = "local"                    # "local" | "none" | provider name
[llm.local]
model = "qwen2.5-1.5b-instruct"
quantization = "q4_k_m"
context = 4096

[llm.cloud]
# provider = "groq"
# api_key_ref = "GROQ_API_KEY"
# model = "llama-3.3-70b-versatile"

[llm.prompt]
main = """..."""                     # core cleanup rules (installed with defaults)
advanced = """..."""                 # backtrack corrections, list formatting
dictionary = []                      # personal terms/names

[[context_rules]]                    # per-app prompt overrides
match.window_class = "firefox"
match.window_title_regex = "Gmail"
prompt_suffix = "Use polite email salutations and sign-off."

[overlay]
enabled = true
position = "bottom-right"            # + top-left, top-right, bottom-left
opacity = 0.85

[history]
enabled = true
retention_days = 90
redact_secrets = true                # scrub obvious key patterns
```

## Implementation Plan

### Phase 0 — Repository bootstrap

- [ ] Task 0.1. Create a new GitHub repo `fono` (or under the user's org). Initialise with
  `LICENSE` = full GPL-3.0 text, `README.md` with a short pitch and a pointer to CONTRIBUTING,
  `CONTRIBUTING.md` requiring `git commit -s` (DCO sign-off) and `cargo fmt`/`cargo clippy`
  clean PRs, `CODE_OF_CONDUCT.md` (Contributor Covenant), `.gitignore` for Rust + editor dirs.

- [ ] Task 0.2. Initialise cargo workspace at the repo root with the crate layout above. Pin
  toolchain via `rust-toolchain.toml` to stable 1.82+. Configure `[workspace.lints]` for
  `clippy::pedantic` minus noise lints.

- [ ] Task 0.3. Set up GitHub Actions CI (`.github/workflows/ci.yml`): matrix over
  ubuntu-latest, macos-latest, windows-latest; steps = `cargo fmt --check`, `cargo clippy -- -D
  warnings`, `cargo test --workspace`. Add `release.yml` that fires on tag `v*` and produces
  cross-compiled artifacts (see Phase 9).

### Phase 1 — Core types, config, and paths (crates: `fono-core`)

- [ ] Task 1.1. Implement XDG path resolver using `directories::ProjectDirs` with explicit
  overrides honouring `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME`.
  Rationale: NimbleX and other Slackware derivatives sometimes ship incomplete XDG defaults.

- [ ] Task 1.2. Define `Config` struct (serde + `#[serde(default)]` everywhere so missing
  fields always fall back). Implement load-or-create-defaults, atomic write via tempfile+rename,
  and a `config migrate` step keyed on the `version` field for forward compat.

- [ ] Task 1.3. Define `Secrets` struct stored separately in `~/.config/fono/secrets.toml`
  mode 0600, never logged, never serialized back to the main config. Fono reads API keys from
  either `$ENV_VAR` or `secrets.toml` depending on the `api_key_ref` field.

- [ ] Task 1.4. Define the SQLite schema for `history.sqlite`:
  ```
  CREATE TABLE transcriptions(
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,             -- unix seconds
    duration_ms INTEGER,
    raw TEXT NOT NULL,               -- STT output
    cleaned TEXT,                    -- LLM output
    app_class TEXT, app_title TEXT,  -- focus context
    stt_backend TEXT, llm_backend TEXT,
    language TEXT
  );
  CREATE VIRTUAL TABLE transcriptions_fts USING fts5(raw, cleaned, content='transcriptions');
  ```
  Implement automatic FTS sync triggers, a retention cleanup job on daemon startup, and a
  `redact_secrets` pass that masks `[A-Za-z0-9_-]{20,}` patterns if enabled.

### Phase 2 — Audio capture & VAD (crate: `fono-audio`)

- [ ] Task 2.1. Wrap `cpal` to enumerate input devices, pick the configured one (or system
  default), and open a 16 kHz mono f32 stream. Handle devices that don't support 16 kHz
  natively by opening at their native rate and resampling with `rubato` on the fly.

- [ ] Task 2.2. Implement ring-buffer capture with a hard cap of 5 minutes to prevent runaway
  memory use, and a soft prompt at 2 minutes ("Recording is getting long — stop?" notification).

- [ ] Task 2.3. Integrate Silero VAD via `ort` (ONNX Runtime) for end-of-speech detection in
  hold-to-talk mode (release keybind = stop immediately; in toggle mode, VAD can optionally
  auto-stop on silence). Ship the Silero VAD ONNX model (~2 MB) vendored in the binary via
  `include_bytes!` — tiny enough not to bloat.

- [ ] Task 2.4. Implement `auto_mute_system`: use `pactl set-sink-mute @DEFAULT_SINK@ 1` (and
  PipeWire equivalent `wpctl set-mute @DEFAULT_AUDIO_SINK@ 1`) before recording, restore after.
  Autodetect PulseAudio vs PipeWire via `pactl info` output.

### Phase 3 — Global hotkeys and FSM (crate: `fono-hotkey`)

- [ ] Task 3.1. Wrap `global-hotkey` crate. Parse human-readable strings
  (`Ctrl+Alt+Space`, `Ctrl+Alt+Grave`) into its native representation. Register on a dedicated
  thread with its own event loop (required on Linux by `global-hotkey`).

- [ ] Task 3.2. Implement a 3-state FSM: `Idle`, `Recording(hold|toggle)`, `Processing`. Guards
  prevent re-entry when Processing. Emit typed events on a tokio `mpsc` channel consumed by
  the orchestrator.

- [ ] Task 3.3. Wayland fallback plan: `global-hotkey` on Wayland only works if the compositor
  exposes the `org.freedesktop.portal.GlobalShortcuts` portal (KDE 6+, GNOME 45+). Detect
  availability at startup; if absent, print a clear message recommending the user bind their
  compositor's native shortcut to `fono toggle` (sway/hyprland/i3-native-via-Xwayland) and
  continue without in-process hotkeys. Document in `docs/wayland.md`.

### Phase 4 — STT backends (crate: `fono-stt`)

- [ ] Task 4.1. Define the `SpeechToText` trait:
  ```rust
  #[async_trait]
  pub trait SpeechToText: Send + Sync {
      async fn transcribe(&self, pcm: &[f32], sample_rate: u32, lang: Option<&str>)
          -> Result<Transcription>;
      fn supports_streaming(&self) -> bool { false }
      async fn transcribe_stream(&self, stream: impl Stream<Item=Vec<f32>>) -> ...;
  }
  ```

- [ ] Task 4.2. Implement `WhisperLocal` backend via `whisper-rs`. Lazy-load model on first
  use, cache in-memory across invocations, configurable threads (default = num physical
  cores). Expose `language="auto"` by letting whisper's built-in lang-detect run on the first
  30 s of audio.

- [ ] Task 4.3. Implement cloud STT backends as separate modules, each gated by a feature
  flag in the Cargo.toml so users building from source can trim unused providers:
  - `GroqSTT` (HTTPS, very fast `whisper-large-v3`)
  - `DeepgramSTT` (WebSocket streaming)
  - `OpenAISTT` (HTTPS, `whisper-1`)
  - `CartesiaSTT`
  - `AssemblyAISTT` (streaming)
  - `AzureSTT`
  - `SpeechmaticsSTT`
  - `GoogleSTT`
  - `NemotronSTT`
  All backends use `reqwest` + `rustls` (never OpenSSL) and `tokio-tungstenite` for WebSocket
  streaming providers.

- [ ] Task 4.4. Shipping default defined by the first-run wizard but recommended:
  `whisper small` **multilingual** (~180 MB Q5_1) — matches user's Balanced+multilingual
  requirement. Ship a `ModelRegistry` with SHA256 hashes for every supported whisper variant
  (tiny, tiny.en, base, base.en, small, small.en, medium, medium.en) and Silero VAD.

### Phase 5 — LLM backends (crate: `fono-llm`)

- [ ] Task 5.1. Define the `TextFormatter` trait:
  ```rust
  #[async_trait]
  pub trait TextFormatter: Send + Sync {
      async fn format(&self, raw: &str, ctx: &FormatContext) -> Result<String>;
  }
  ```
  where `FormatContext` bundles the main/advanced/dictionary prompts, matched context-rule
  suffix, and focus metadata.

- [ ] Task 5.2. Implement `LlamaLocal` backend via `llama-cpp-2`. Same lazy-load pattern as
  whisper. Configurable context size (default 4096). Default sampler settings tuned for
  deterministic cleanup (temp 0.3, top_p 0.9, no repetition penalty — cleanup is not creative
  writing).

- [ ] Task 5.3. Implement cloud LLM backends, each feature-gated:
  - `OpenAILLM`
  - `AnthropicLLM`
  - `GeminiLLM`
  - `GroqLLM`
  - `CerebrasLLM` (fastest for this use case at < 1 s latency)
  - `OpenRouterLLM`
  - `OllamaLLM` (for users running Ollama locally)
  All via `reqwest` + `rustls`.

- [ ] Task 5.4. Default local model: **Qwen2.5-1.5B-Instruct Q4_K_M** (~1 GB, Apache-2.0,
  multilingual — satisfies Balanced+multilingual requirement). Ship a `ModelRegistry` with
  SHA256 hashes and HuggingFace URLs for: Qwen2.5-0.5B, Qwen2.5-1.5B, Qwen2.5-3B (Apache-2.0),
  SmolLM2-1.7B (Apache-2.0). Do **not** ship Llama / Gemma in defaults because their licenses
  are not OSI-approved and would conflict with the project's GPL-3.0 ethos (still available
  as opt-in downloads).

- [ ] Task 5.5. Write the baked-in default prompts (installed into `config.toml` on first
  run): `main` (strip fillers, add punctuation, capitalize, match language), `advanced`
  (backtrack corrections like "scratch that", list formatting), `dictionary` (empty list).
  Derive these from Tambourine's proven prompts (AGPL-3.0 upstream — we can read them as
  inspiration but must write fresh text for a GPL-3.0 project; no copy-paste).

### Phase 6 — Text injection and focus detection (crates: `fono-inject`)

- [ ] Task 6.1. Wrap `enigo` for cross-platform key injection. On Linux, `enigo`'s default
  backend uses libxdo (X11); for Wayland, detect `$XDG_SESSION_TYPE=wayland` and use libei
  (via newer enigo versions) or fall back to spawning `wtype` / `ydotool` (autodetected).

- [ ] Task 6.2. Implement focus detection for per-app context rules:
  - X11: use `x11rb` to read `_NET_ACTIVE_WINDOW` and `WM_CLASS`/`WM_NAME`.
  - sway: shell out to `swaymsg -t get_tree | jq …` or use `swayipc` crate.
  - Hyprland: use `hyprland` crate.
  - Others / Wayland generic: gracefully degrade — no context match, use base prompt.

- [ ] Task 6.3. Implement the "paste last" action: read the most recent cleaned transcription
  from SQLite and re-type it (not clipboard-paste — matches Tambourine's behaviour and works
  even when the target field doesn't accept paste).

### Phase 7 — UI: tray + overlay (crates: `fono-tray`, `fono-overlay`)

- [ ] Task 7.1. Tray icon via `tray-icon` crate with menu entries: Show status / Pause /
  Open history / Open config / Quit. Icon changes colour based on FSM state (idle = grey,
  recording = red, processing = amber).

- [ ] Task 7.2. Minimalist floating recording indicator via `winit` + `softbuffer`: a 140×36
  borderless, always-on-top, click-through window positioned per config. Renders a simple
  pulsing dot + live dB meter during recording. No compositor-specific tricks (layer-shell
  etc.) in v0.1 — plain always-on-top is enough on i3/openbox; Wayland compositors can
  manage it via standard xdg-toplevel rules.

- [ ] Task 7.3. Sound feedback: tiny embedded WAV files (start-ding, stop-ding, error-buzz),
  played via `rodio`. Respect `general.sound_feedback = false`.

### Phase 8 — First-run wizard and CLI (crate: `fono`)

- [ ] Task 8.1. On startup, check for `~/.config/fono/config.toml`. If absent, enter the
  interactive wizard. The wizard uses `dialoguer` for prompts and `indicatif` for download
  bars; it never touches TUI widgets that break on weird terminals, and supports
  `--non-interactive` + env-var overrides for automation.

- [ ] Task 8.2. Wizard flow (pseudocode):
  ```
  Print banner.
  Q: "local models" or "cloud APIs"? [1/2]
    If 1 (local):
      Q: STT model? [base/small/multilingual-base/multilingual-small]  default = small multilingual
      Q: LLM model? [Qwen-0.5B / Qwen-1.5B / SmolLM2-1.7B / skip]       default = Qwen-1.5B
      Download each selected model with progress bar, verify SHA256.
    If 2 (cloud):
      Q: STT provider? [groq / deepgram / openai / cartesia / ...]     default = groq
      Prompt for API key → write to secrets.toml (0600) OR env var reference
      Q: LLM provider? [cerebras / groq / openai / anthropic / ...]    default = cerebras
      Prompt for API key → write to secrets.toml OR env var reference
  Write config.toml.
  Offer to set up autostart (systemd user unit or equivalent).
  Print the default hotkeys and a one-liner demo tip.
  ```
  Both branches offer ≥ 2 options (requirement from user feedback).

- [ ] Task 8.3. CLI subcommands (via `clap`):
  - `fono` — start daemon + tray.
  - `fono daemon --no-tray` — headless daemon (for TTY-only users).
  - `fono toggle` — IPC → daemon toggle recording.
  - `fono record` — one-shot: record until silence/Esc, STT+clean+paste, exit.
  - `fono paste-last` — re-type last cleaned transcription.
  - `fono history [--search QUERY] [--json]` — browse history.
  - `fono config [edit|show|path]`.
  - `fono models [list|install NAME|remove NAME|verify]`.
  - `fono setup` — re-run first-run wizard.
  - `fono doctor` — diagnostic report (audio device, hotkey engine, Wayland/X11, installed
    models, tray backend, network reachability to configured providers).
  - `fono --version`, `--help`.

- [ ] Task 8.4. IPC (crate `fono-ipc`): bincode-serialized frames over Unix socket. Short-
  circuit if daemon isn't running: CLI commands that require a running daemon print
  `fono: daemon not running; start it with 'fono' or install the autostart unit`.

### Phase 9 — Packaging, releases, and distro integration

- [ ] Task 9.1. GitHub Actions `release.yml`: on tag push `v*`, build:
  - `x86_64-unknown-linux-musl` via `cross` (fully static, works on every Linux).
  - `aarch64-unknown-linux-musl` via `cross`.
  - `x86_64-pc-windows-msvc`.
  - `x86_64-apple-darwin` + `aarch64-apple-darwin` (lipo into universal).
  All with `--profile release-slim` (codegen-units=1, lto=fat, panic=abort, strip=symbols)
  to minimise size. Generate SHA256SUMS and a minisign signature. Upload as GitHub Release
  assets.

- [ ] Task 9.2. NimbleX SlackBuild at `packaging/slackbuild/` mirroring the existing
  `earlyoom/` layout (reference: `/mnt/nvme0n1p5/Work/slackbuilds/earlyoom/`). Two modes:
  - **Binary mode (default)**: download the pre-built `fono-x86_64-unknown-linux-musl` from
    the GitHub release pinned in `.info` (so hashes can be verified), install to
    `/usr/bin/fono`, install `fono.desktop` + icon, install optional systemd user unit at
    `/lib/systemd/user/fono.service` (not enabled by default).
  - **Source mode**: if user sets `FROM_SOURCE=1`, `git clone` upstream and build with
    `cargo build --release --target x86_64-unknown-linux-musl` (requires `rust`, `musl-gcc`).
  Neither mode ships a system-wide daemon — Fono is per-user software.

- [ ] Task 9.3. Optional systemd **user** unit `fono.service`:
  ```
  [Unit]
  Description=Fono voice dictation daemon
  After=default.target
  [Service]
  Type=simple
  ExecStart=/usr/bin/fono daemon
  Restart=on-failure
  [Install]
  WantedBy=default.target
  ```
  Do not enable by default; `fono setup` offers to enable it via
  `systemctl --user enable --now fono.service`.

- [ ] Task 9.4. AUR PKGBUILD, Nix flake, and `debian/` packaging in parallel folders so
  downstream packagers can cherry-pick without reverse-engineering. Not a blocker for v0.1
  release; community can contribute.

- [ ] Task 9.5. First-run download endpoints: pin HuggingFace URLs + SHA256 in a compiled-in
  `ModelRegistry`. Allow env var `FONO_MODEL_MIRROR` to override the host (for users behind
  restrictive networks or wanting a local registry).

### Phase 10 — Documentation and release readiness

- [ ] Task 10.1. Write `README.md`: project pitch, screenshots (once v0.1 builds), install
  snippets for Linux (tarball + SlackBuild + AUR), Wayland caveat, first-run demo, hotkey
  cheat sheet, provider matrix with API-key env-var names.

- [ ] Task 10.2. Write `docs/architecture.md` (modules + runtime dataflow), `docs/providers.md`
  (full STT+LLM matrix with tested models per provider), `docs/wayland.md` (compositor-
  specific notes), `docs/privacy.md` (data leaves your machine only when cloud providers are
  selected; no telemetry; how to delete history).

- [ ] Task 10.3. Write `CONTRIBUTING.md`: DCO sign-off requirement, `cargo fmt`/`clippy`
  rules, how to add a new STT/LLM backend (implement trait + register in factory), how to
  run tests + benchmarks.

- [ ] Task 10.4. Tag `v0.1.0`, let CI produce binaries, draft release notes listing the
  exact STT + LLM models shipped as defaults with their SHA256s.

## Verification Criteria

- `cargo build --release --target x86_64-unknown-linux-musl` from a clean checkout on NimbleX
  produces a single-file `fono` ELF ≤ 25 MB stripped, with `ldd` reporting "not a dynamic
  executable".
- Running the binary on a fresh user (`HOME=/tmp/fresh-user ./fono`) triggers the first-run
  wizard, and choosing any of the ≥ 2 local or cloud options results in a working daemon
  without manual config editing.
- Default local-only path downloads `whisper small` (multilingual) + `Qwen2.5-1.5B-Instruct
  Q4_K_M`, verifies SHA256, and has end-to-end dictation latency ≤ 2 seconds on a 4-core
  x86_64 CPU for a 10-second utterance.
- `Ctrl+Alt+Space` in an X11 session triggers recording; release → text appears at cursor.
  Same holds in a sway or Hyprland Wayland session when Fono is launched (with the
  documented fallback for compositors lacking the GlobalShortcuts portal).
- `fono doctor` returns a green report on NimbleX + i3 + PipeWire.
- Memory footprint idle ≤ 30 MB RSS (without models loaded); ≤ 1.3 GB RSS with default local
  models loaded; drops back after inference.
- `cargo clippy -- -D warnings` and `cargo fmt --check` pass on CI for all supported targets.
- GitHub Release builds six artifacts (x86_64 + aarch64 linux musl, Windows MSVC, Intel +
  ARM macOS, universal macOS) with SHA256SUMS + minisign signature.
- NimbleX SlackBuild in binary mode produces
  `fono-0.1.0-x86_64-1_NimbleX.txz` that installs cleanly and removes cleanly with no leaked
  files (except preserved `~/.config/fono/*` which is user data).
- `fono history` shows past transcriptions after three dictation sessions.
- License file present at `LICENSE` is the unmodified GPL-3.0 text; every source file has an
  SPDX header `// SPDX-License-Identifier: GPL-3.0-only`.

## Potential Risks and Mitigations

1. **Wayland global-hotkey portal not available on NimbleX's default compositors.**
   Mitigation: detect at startup; print a one-liner instructing user to bind compositor
   native shortcut to `fono toggle`; document per-compositor in `docs/wayland.md`. The CLI
   subcommand IPC path is the fallback that always works.

2. **`tray-icon` fails under bare i3 without a StatusNotifierItem host (polybar/waybar).**
   Mitigation: auto-detect SNI host via D-Bus introspection; fall back to XEmbed (also
   supported by `tray-icon` crate); if even that fails, run headless and print a notice.

3. **First-run download fails mid-stream or HF rate-limits.**
   Mitigation: HTTP Range + resume in `fono-download`; SHA256 verification aborts corrupt
   downloads cleanly; retry 3× with exponential backoff; document `FONO_MODEL_MIRROR`
   override.

4. **`whisper-rs` / `llama-cpp-2` build failures on musl (C++ toolchain quirks).**
   Mitigation: use `cross` images with a known-good `musl-cross` + libstdc++-musl build
   chain (the maintained `ghcr.io/cross-rs/x86_64-unknown-linux-musl:main` image works);
   fallback target is `gnu` with a Debian oldstable glibc baseline if musl proves hostile.

5. **Model licensing surprises** (a HuggingFace URL flipping to a restricted revision).
   Mitigation: `ModelRegistry` pins exact revision hashes (HF rev SHAs), not just repo
   names; `fono models verify` re-checks; `fono doctor` warns on drift.

6. **GPL-3.0 + dependency licensing.** Mitigation: every direct dependency in this plan is
   MIT/Apache/BSD — all GPL-compatible. `cargo-deny` in CI enforces the allow-list so a
   future PR can't silently introduce a non-compatible license.

7. **Audio stack fragmentation (ALSA vs Pulse vs PipeWire).**
   Mitigation: `cpal` already abstracts this; `fono doctor` reports what it found; Silero
   VAD runs downstream of `cpal` so it doesn't care; auto-mute probes both `pactl` and
   `wpctl` and uses whichever exists.

8. **User's ICE moment: "why did Fono eat 1.2 GB of RAM?"** because Qwen2.5-1.5B loaded.
   Mitigation: document memory expectations in README + first-run wizard; offer "Lite" tier
   at ~600 MB RSS; tray menu has "Unload models" that shrinks RSS back to ~30 MB.

9. **Security: API keys on disk.** Mitigation: `secrets.toml` mode 0600, ownership check on
   startup, refuse to read if world-readable; support `$ENV_VAR` reference as alternative
   so CI/container users never need a file.

10. **libxdo not available on NimbleX default.** Mitigation: `enigo` vendors the libxdo C
    source and compiles it into the binary statically — no system libxdo needed.

## Alternative Approaches Considered (and why rejected)

1. **C++ + CMake.** Rejected: the static-musl + rustls + bundled-sqlite story is the entire
   point; replicating it in C++ is months of release-engineering tax with zero user-visible
   benefit. Deep analysis in the v3 response to the user.

2. **Go.** Rejected: cgo's FFI overhead into whisper.cpp is material, and Go's GUI/tray
   crate ecosystem is weaker than Rust's on Linux. No cross-DE global-hotkey library.

3. **Electron (OpenWhispr's path).** Rejected: the thing we're specifically trying to beat.

4. **Tauri (Tambourine's path).** Rejected: the desktop shell is fine but Tauri drags in a
   WebKit2GTK runtime on Linux — exactly the NimbleX build problem we already hit. A
   headless `winit` overlay + tray is strictly lighter.

5. **Zig.** Rejected: ecosystem (tray, hotkeys, ONNX, sqlite) is still young; can revisit
   for a v2 rewrite if Zig matures.

6. **Shipping models bundled inside the binary.** Rejected: ~1 GB binaries are hostile to
   distribute; first-run download with SHA256 verification + mirror override is the standard
   Rust/Go OSS approach and gives users choice.

7. **Copyleft Qwen3 / Llama as default.** Rejected on license grounds (Llama Community
   License is not OSI-approved and its acceptable-use clauses are incompatible with GPL-3.0
   project ethos). Available as opt-in only.

## Handoff Notes for Implementation

The plan is ready for Forge to execute. Recommended sequencing for Forge:
1. Phase 0 (repo bootstrap) in one session.
2. Phases 1–3 (core + audio + hotkeys) in parallel where independent.
3. Phases 4–5 (STT + LLM) can land incrementally: `WhisperLocal` + `LlamaLocal` + one cloud
   STT (Groq) + one cloud LLM (Cerebras) is enough for v0.1 public release; remaining
   cloud providers follow as separate PRs.
4. Phases 6–7 (inject + UI) unblock end-to-end dictation.
5. Phase 8 (wizard + CLI) is the last piece before cutting a release.
6. Phase 9 (packaging) runs in parallel with Phase 10 (docs).
7. Tag v0.1.0 when verification criteria pass on NimbleX.
