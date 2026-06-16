// SPDX-License-Identifier: GPL-3.0-only
//! Clap-powered CLI surface + dispatch to daemon / subcommands.

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use fono_core::{Config, Paths, Secrets};
use fono_ipc::{Request, Response};

use crate::{agent_setup, daemon, doctor, wizard};

#[derive(Debug, Parser)]
#[command(
    name = "fono",
    version,
    about = "Lightweight native voice dictation for Linux, Windows, and macOS.",
    long_about = "Lightweight native voice dictation for Linux, Windows, and macOS.\n\n\
        Run `fono` with no subcommand to start the background daemon (the \
        first-run wizard launches automatically when no config exists)."
)]
pub struct Cli {
    /// Enable debug logging (`-vv` for trace + file/line).
    #[arg(long = "debug", short = 'v', action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Silence everything below `warn`.
    #[arg(long = "quiet", short = 'q', global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

/// Effective log verbosity derived from `--debug` / `--quiet` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Info,
    Debug,
    Trace,
}

impl Verbosity {
    pub fn as_filter(self) -> &'static str {
        // Targets demoted at default verbosity so the INFO-level model-load
        // chatter from whisper.cpp / llama.cpp / ggml stays out of normal
        // startup output. Re-enable any of them on demand with e.g.
        // `FONO_LOG=llama-cpp-2=info`.
        //
        // `llama-cpp-2=error` (rather than `=warn`) intentionally hides two
        // chronic, harmless warnings that fire on every model load and every
        // inference call respectively:
        //   1. `control-looking token: ... '</s>' was not control-type` —
        //      cosmetic Qwen2.5 GGUF metadata quirk; llama.cpp overrides
        //      the type internally and continues correctly.
        //   2. `n_ctx_seq (N) < n_ctx_train (M) -- the full capacity of the
        //      model will not be utilized` — informational, not an error;
        //      cleanup prompts never need the model's full training ctx.
        // Real load / inference errors propagate via `anyhow` from
        // `LlamaModel::load_from_file` / `ctx.decode` and surface with full
        // context regardless of this filter.
        match self {
            Self::Quiet => {
                "warn,whisper_rs::ggml_logging_hook=warn,whisper_rs::whisper_logging_hook=warn,\
                 llama-cpp-2=error"
            }
            Self::Info => {
                "info,whisper_rs::ggml_logging_hook=warn,whisper_rs::whisper_logging_hook=warn,\
                 llama-cpp-2=error"
            }
            Self::Debug => {
                "fono=debug,fono_core=debug,fono_hotkey=debug,fono_tray=debug,\
                fono_audio=debug,fono_stt=debug,fono_polish=debug,fono_inject=debug,\
                fono_ipc=debug,fono_download=debug,whisper_rs::ggml_logging_hook=warn,\
                whisper_rs::whisper_logging_hook=warn,llama-cpp-2=warn,info"
            }
            Self::Trace => {
                "fono=trace,fono_core=trace,fono_hotkey=trace,fono_tray=trace,\
                fono_audio=trace,fono_stt=trace,fono_polish=trace,fono_inject=trace,\
                fono_ipc=trace,fono_download=trace,whisper_rs::ggml_logging_hook=warn,\
                whisper_rs::whisper_logging_hook=warn,llama-cpp-2=info,debug"
            }
        }
    }

    pub fn is_trace(self) -> bool {
        matches!(self, Self::Trace)
    }
}

impl Cli {
    pub fn verbosity(&self) -> Verbosity {
        if self.quiet {
            Verbosity::Quiet
        } else {
            match self.verbose {
                0 => Verbosity::Info,
                1 => Verbosity::Debug,
                _ => Verbosity::Trace,
            }
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Toggle recording on the running daemon.
    Toggle,
    /// Cancel any in-flight activity on the running daemon: aborts an
    /// active recording (batch or live dictation) and stops in-flight
    /// assistant playback. Idempotent — no-op when nothing is active.
    /// On Wayland the daemon already grabs the configured cancel key
    /// (default `Escape`) through the portal while a recording is
    /// running; this CLI verb is the fallback for environments where
    /// the portal isn't available, and for scripted invocations.
    Cancel,
    /// Record once from the mic, transcribe, inject, exit.
    ///
    /// Captures audio until silence, Ctrl-C, or the `--max-seconds`
    /// timeout, then runs the configured STT (and polish) and
    /// types the result into the focused window.
    Record {
        /// Print the cleaned text to stdout instead of typing it.
        #[arg(long)]
        no_inject: bool,
        /// Stop recording after this many seconds (0 = no limit).
        #[arg(long, default_value_t = 30)]
        max_seconds: u64,
        /// One-shot STT backend override (e.g. `local`, `groq`).
        #[arg(long)]
        stt: Option<String>,
        /// One-shot polish backend override (`none` to skip cleanup).
        #[arg(long)]
        polish: Option<String>,
        /// Use live streaming mode (requires the `interactive` feature).
        #[arg(long)]
        live: bool,
    },
    /// Transcribe a WAV file without touching the microphone.
    ///
    /// Accepts 16-bit PCM mono (any sample rate). Useful for testing
    /// API keys or batch-processing recordings.
    Transcribe {
        /// Path to a WAV file.
        path: std::path::PathBuf,
        /// Skip the polish step.
        #[arg(long)]
        no_polish: bool,
        /// One-shot STT backend override.
        #[arg(long)]
        stt: Option<String>,
        /// One-shot polish backend override.
        #[arg(long)]
        polish: Option<String>,
    },
    /// Re-type the last cleaned transcription.
    PasteLast,
    /// Voice-assistant push-to-talk control. Subcommands match the
    /// IPC contract used by the F8 hotkey: `press` starts audio
    /// capture, `release` runs the streaming pump. Use `fono cancel`
    /// to abort an in-flight reply (it covers both dictation and the
    /// assistant). Useful for end-to-end smoke tests and for
    /// scripted invocations.
    Assistant {
        #[command(subcommand)]
        action: AssistantCmd,
    },
    /// Browse the transcription history.
    History {
        /// Filter to entries containing this substring.
        #[arg(long)]
        search: Option<String>,
        /// Maximum number of entries to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Emit machine-readable JSON instead of plain text.
        #[arg(long)]
        json: bool,
        /// Show only the most recent entry with full STT/LLM detail.
        #[arg(long)]
        last: bool,
    },
    /// View or edit the configuration file.
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// List, install, or remove local Whisper models.
    Models {
        #[command(subcommand)]
        action: ModelsCmd,
    },
    /// Re-run the first-run setup wizard.
    Setup,
    /// Print a diagnostic report (config, paths, providers, audio).
    Doctor {
        /// Follow the log file (like `tail -f`); colors preserved.
        #[arg(short = 'f', long = "follow")]
        follow: bool,
    },
    /// Type literal text to verify the inject + clipboard pipeline.
    ///
    /// Bypasses audio, STT, and LLM. Use this to confirm text can
    /// actually reach your focused window or the clipboard.
    TestInject {
        /// Text to inject and copy to clipboard.
        text: String,
        /// Skip key-injection (only copy to clipboard).
        #[arg(long)]
        no_inject: bool,
        /// Skip clipboard copy (only key-injection).
        #[arg(long)]
        no_clipboard: bool,
    },
    /// Open the live-dictation overlay for a few seconds (smoke test).
    ///
    /// Verifies the `interactive` feature was compiled in and that
    /// winit/softbuffer can paint on this compositor.
    TestOverlay,
    /// Probe hardware and recommend a local-model tier.
    Hwprobe {
        /// Emit machine-readable JSON instead of the text report.
        #[arg(long)]
        json: bool,
    },
    /// Switch the active STT / polish backend (no daemon restart needed).
    Use {
        #[command(subcommand)]
        action: UseCmd,
    },
    /// Manage API keys in `~/.config/fono/secrets.toml`.
    Keys {
        #[command(subcommand)]
        action: KeysCmd,
    },
    /// Print shell completions (bash, zsh, fish, powershell, elvish).
    Completions {
        /// Target shell.
        #[arg(value_enum)]
        shell: Shell,
    },
    /// List LAN STT servers discovered via mDNS.
    ///
    /// Reads the running daemon's live registry — if no daemon is
    /// running, no peers are listed.
    Discover {
        /// Emit machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Check for a newer release and self-update in place.
    ///
    /// The daemon also checks automatically in the background when
    /// `[update] auto_check` is enabled (the default).
    Update {
        /// Only check; do not download. Exits 0 if up-to-date, 1 if not.
        #[arg(long)]
        check: bool,
        /// Skip the confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Resolve and verify, but do not replace the running binary.
        #[arg(long)]
        dry_run: bool,
        /// Release channel: `stable` or `prerelease`.
        #[arg(long, default_value = "stable")]
        channel: String,
        /// Do not re-exec into the new binary after updating.
        #[arg(long)]
        no_restart: bool,
        /// Override the install directory (e.g. `/usr/local/bin`).
        #[arg(long)]
        bin_dir: Option<std::path::PathBuf>,
    },
    /// Install fono system-wide. Requires root.
    ///
    /// Default (auto-detect): inspects the host for an active
    /// graphical session (loginctl / display-manager unit / X11 or
    /// Wayland socket / `systemctl get-default`). On a headless box
    /// the systemd-unit lane is picked automatically; otherwise the
    /// desktop lane runs.
    ///
    /// With `--server`: binary, hardened systemd unit (running as a
    /// dedicated `fono` user), and shell completions. The unit is
    /// enabled and started immediately.
    ///
    /// With `--desktop`: binary, menu entry, XDG autostart entry,
    /// icon, and shell completions. Forces the desktop lane even on
    /// hosts that look headless.
    Install {
        /// Force headless server mode (systemd unit, no desktop entries).
        #[arg(long, conflicts_with = "desktop")]
        server: bool,
        /// Force desktop mode (menu + autostart + icon, no systemd unit).
        #[arg(long)]
        desktop: bool,
        /// Print what would be done without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Uninstall fono. Reverses a previous `fono install`.
    ///
    /// Removes every system path the installer wrote (binary, desktop
    /// entries, icon, systemd unit, completions) and — in desktop mode
    /// — also wipes the per-user `~/.cache/fono` (model weights,
    /// downloaded archives, hwcheck JSON), which is fully reproducible
    /// on next `fono setup`. User data under `~/.config/fono` and
    /// `~/.local/share/fono` is never touched.
    Uninstall {
        /// Print what would be done without removing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Read text from stdin and speak it through the configured TTS backend.
    ///
    /// Segments input into sentences, strips markdown (code blocks,
    /// bold/em, headings, links), and synthesises each sentence for
    /// playback. Backpressure prevents a fast producer from outrunning
    /// the listener (at most 5 sentences queue ahead).
    ///
    /// Example: `echo "Hello there. This is sentence two." | fono speak --stream`
    ///
    /// Pipe from a coding agent: `forge | fono speak --stream`
    Speak {
        #[command(subcommand)]
        action: SpeakCmd,
    },
    /// Read notification content from stdin, summarize it into 1-2 spoken
    /// sentences via the configured assistant backend, and speak the summary
    /// through the configured TTS backend (unless `--silent`).
    ///
    /// The raw input (chat message, log dump, alert) is summarized, never
    /// read aloud verbatim. By default stdin is treated as raw text; with
    /// `--json`, stdin is parsed as the same JSON payload accepted by the
    /// `fono.summarize` MCP tool.
    ///
    /// Example: `echo "Mihai: staging deploy fails after migration" | fono summarize --sender Mihai`
    Summarize {
        /// Parse stdin as a JSON payload (same schema as the
        /// `fono.summarize` MCP tool) instead of raw text.
        #[arg(long)]
        json: bool,
        /// Sender display name attached to the notification.
        #[arg(long)]
        sender: Option<String>,
        /// Chat / channel name attached to the notification.
        #[arg(long)]
        chat: Option<String>,
        /// Originating application label (e.g. `chat-cli`).
        #[arg(long)]
        source: Option<String>,
        /// Extra instructions appended to the summarization prompt.
        #[arg(long)]
        instructions: Option<String>,
        /// TTS voice override (backend-specific).
        #[arg(long)]
        voice: Option<String>,
        /// Print the summary to stdout and skip TTS playback.
        #[arg(long)]
        silent: bool,
    },
    /// Manage per-program TTS voices: list the active backend's voice
    /// palette, pin a program to a voice, set a gender preference, or
    /// preview a voice.
    ///
    /// Voices are addressed by positional gendered labels ("Female 1",
    /// "Male 2") so you never touch the cryptic backend-specific ids.
    /// Example: `fono voices set chat-cli "male 1"`
    Voices {
        #[command(subcommand)]
        action: VoicesCmd,
    },
    /// Run Fono as an MCP (Model Context Protocol) server over stdio.
    ///
    /// Exposes voice tools: `fono.speak`, `fono.listen`, `fono.confirm`,
    /// and `fono.summarize`. The server is disabled by default; enable it
    /// with `fono use mcp-server on`. Only stdio transport is available
    /// in v1; SSE/HTTP transport follows in v2.
    Mcp {
        #[command(subcommand)]
        action: McpCmd,
    },
    /// One-command setup: enable the MCP server, write the agent MCP
    /// JSON, and inject the shared voice-mode preset — all idempotent.
    ///
    /// Example: `fono agent-setup forge`
    ///
    /// Use `--list` to print all known agents without configuring any.
    AgentSetup {
        /// Agent name (e.g. `forge`, `claude-code`, `cursor`).
        /// Omit when using `--list`.
        #[arg(conflicts_with = "list")]
        agent: Option<String>,
        /// Project directory for preset-file injection (AGENTS.md etc.).
        /// Defaults to the current directory.
        #[arg(long, default_value = ".")]
        project_dir: std::path::PathBuf,
        /// Print what would be done without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Print all registered agents and exit.
        #[arg(long)]
        list: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum UseCmd {
    /// Switch the active STT backend.
    Stt {
        /// local | groq | openai | deepgram | assemblyai | cartesia | azure | speechmatics | google | nemotron
        backend: String,
    },
    /// Switch the active polish backend.
    Polish {
        /// none | local | cerebras | groq | openai | anthropic | openrouter | ollama | gemini
        backend: String,
    },
    /// Switch the active voice-assistant chat backend. Independent of
    /// the polish pipeline (`fono use polish`).
    Assistant {
        /// none | local | cerebras | groq | openai | anthropic | openrouter
        backend: String,
    },
    /// Switch the active TTS backend (assistant audio replies).
    Tts {
        /// none | wyoming | piper | openai
        backend: String,
        /// Optional Wyoming server URI when `backend = wyoming`,
        /// e.g. `tcp://localhost:10200`.
        #[arg(long)]
        uri: Option<String>,
    },
    /// Switch STT + LLM to a paired cloud preset.
    Cloud {
        /// groq | cerebras | openai | anthropic | openrouter | deepgram | assemblyai
        provider: String,
    },
    /// Switch the text-injection backend. On GNOME-Wayland Fono
    /// defaults to `clipboard` to avoid GNOME's "Allow input emulation"
    /// permission dialog; users who want one-key paste can opt in to
    /// `xdotool` (which will trigger that GNOME prompt the first time
    /// it types). Every other session keeps its session-appropriate
    /// auto-detected backend unless overridden here.
    Inject {
        /// auto | clipboard | xdotool | wtype | ydotool | xtest | none
        backend: String,
    },
    /// Switch to local STT (whisper) and disable polish.
    Local,
    /// Print the active STT/LLM and the running daemon's view.
    Show,
    /// Enable or disable the MCP server (sets `[mcp.server].enabled`).
    ///
    /// When enabled, `fono mcp serve` exposes `fono.speak`,
    /// `fono.listen`, and `fono.confirm` over stdio for MCP-capable
    /// coding agents.
    McpServer {
        /// `on` to enable, `off` to disable.
        state: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum KeysCmd {
    /// List all API keys (values are masked).
    List,
    /// Add or replace an API key (prompts on stdin if `--value` is omitted).
    Add {
        /// Key name, e.g. `GROQ_API_KEY`.
        name: String,
        /// Inline value (prefer the interactive prompt for safety).
        #[arg(long)]
        value: Option<String>,
    },
    /// Remove a key from secrets.toml.
    Remove {
        /// Key name to remove.
        name: String,
    },
    /// Show which provider keys are configured.
    Check,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Open the config file in `$EDITOR` (defaults to `nano`).
    Edit,
    /// Print the current configuration to stdout.
    Show,
    /// Print the path to the config file.
    Path,
}

#[derive(Debug, Subcommand)]
pub enum AssistantCmd {
    /// Start assistant audio capture. Mirrors holding F8.
    Press,
    /// End capture and run the streaming reply (STT → chat → TTS).
    /// Mirrors releasing F8.
    Release,
}

#[derive(Debug, Subcommand)]
pub enum ModelsCmd {
    /// List available Whisper models and which are installed.
    List,
    /// Download and install a Whisper model (e.g. `small`, `large-v3-turbo`).
    Install {
        /// Model name (see `fono models list`).
        name: String,
        /// Quantization to install. `auto` (default) picks the
        /// registry default for the model. Use `fp16`, `q5_1`, or
        /// `q8_0` to pin a specific variant.
        #[arg(long, default_value = "auto")]
        quantization: String,
    },
    /// Remove a previously installed Whisper model. Removes every
    /// quantization variant of the named model.
    Remove {
        /// Model name to remove.
        name: String,
    },
    /// Verify the integrity of installed models.
    Verify,
}

#[derive(Debug, Subcommand)]
pub enum SpeakCmd {
    /// Read text from stdin and speak it through the configured TTS backend.
    ///
    /// Segments input into sentences, strips markdown formatting, and
    /// synthesises each sentence for playback. Useful standalone as a
    /// pipe: `echo "Hello. World." | fono speak --stream`
    Stream,
}

#[derive(Debug, Subcommand)]
pub enum McpCmd {
    /// Start an MCP server over stdio. Exposes `fono.speak`,
    /// `fono.listen`, and `fono.confirm` tools for any MCP-capable
    /// coding agent. Exits when stdin closes. The server must be
    /// enabled first with `fono use mcp-server on`.
    Serve,
}

#[derive(Debug, Subcommand)]
pub enum VoicesCmd {
    /// List the active TTS backend's voice palette (positional gendered
    /// labels with the intrinsic voice name beside each), the current
    /// per-program pins, and the gender preference.
    List,
    /// Pin a program to a voice. PROGRAM is the MCP `clientInfo.name` or
    /// the notification `source_app`; LABEL is a palette label
    /// ("female 1"), the literal "auto", or a raw backend voice id.
    Set {
        /// Program identity (MCP client name or notification source_app).
        program: String,
        /// Palette label ("male 1"), "auto", or a raw backend voice id.
        label: String,
    },
    /// Remove a program's voice pin (it reverts to automatic assignment).
    Unset {
        /// Program identity to clear.
        program: String,
    },
    /// Set the global gender preference that filters automatic
    /// assignment: `male`, `female`, or `any` (clears the preference).
    Gender {
        /// One of `male`, `female`, `any`.
        gender: String,
    },
    /// Speak a short sample through a palette voice so you can hear it
    /// before pinning. LABEL is a palette label or a raw backend id.
    Preview {
        /// Palette label ("female 1") or a raw backend voice id.
        label: String,
    },
}

#[allow(clippy::large_stack_frames, clippy::too_many_lines, clippy::cognitive_complexity)]
pub async fn run(cli: Cli) -> Result<()> {
    let paths = Paths::resolve().context("resolve XDG paths")?;
    paths.ensure()?;

    // Implicit first-run wizard: if there's no config and the user didn't
    // explicitly pick a non-interactive subcommand, enter the wizard.
    let needs_wizard = !paths.config_file().exists();

    match cli.cmd {
        None => {
            if needs_wizard {
                // The interactive wizard requires a TTY (`dialoguer` reads
                // arrow keys / "yes" prompts straight from stdin and aborts
                // with `IO error: not a terminal` otherwise). Under systemd,
                // SSH-without-pty, Docker, or any other non-interactive
                // launch we'd otherwise crash-loop on first run. Detect
                // that case and seed a sensible default config instead so
                // the daemon can come up; the user can re-run the wizard
                // interactively later (`fono setup`) or hand-edit
                // `~/.config/fono/config.toml`.
                use std::io::IsTerminal;
                if std::io::stdin().is_terminal() {
                    wizard::run(&paths).await?;
                } else {
                    let cfg_path = paths.config_file();
                    Config::default().save(&cfg_path).with_context(|| {
                        format!("write default config to {}", cfg_path.display())
                    })?;
                    eprintln!(
                        "fono: no interactive terminal detected; wrote default config to {}",
                        cfg_path.display()
                    );
                    eprintln!(
                        "fono: re-run `fono setup` from a terminal to configure STT/polish backends."
                    );
                }
            }
            // Startup-failure notification (issue #8): on autostart
            // via systemd --user, a failed daemon boot only writes
            // to the journal. Surface it as a desktop notification
            // so the user notices. This is *outside* the
            // critical_notify session cap because no session has
            // started yet — it's a one-shot at-most-one popup by
            // construction (the daemon either starts or it doesn't).
            let result = daemon::run(&paths, cli.verbosity()).await;
            if let Err(e) = &result {
                fono_core::notify::send(
                    "Fono — daemon failed to start",
                    &format!(
                        "{e:#}. Check `journalctl --user -u fono` and run \
                         `fono doctor` for diagnostics."
                    ),
                    "dialog-error",
                    10_000,
                    fono_core::notify::Urgency::Critical,
                );
            }
            result
        }
        Some(Cmd::Setup) => Box::pin(wizard::run(&paths)).await,
        Some(Cmd::Toggle) => ipc_simple(&paths, Request::Toggle).await,
        Some(Cmd::Cancel) => ipc_simple(&paths, Request::Cancel).await,
        Some(Cmd::PasteLast) => ipc_simple(&paths, Request::PasteLast).await,
        Some(Cmd::Assistant { action }) => {
            let req = match action {
                AssistantCmd::Press => Request::AssistantHoldPress,
                AssistantCmd::Release => Request::AssistantHoldRelease,
            };
            ipc_simple(&paths, req).await
        }
        Some(Cmd::Doctor { follow }) => {
            let report = doctor::report(&paths).await?;
            println!("{report}");
            if follow {
                doctor::follow_log(&paths).await?;
            }
            Ok(())
        }
        Some(Cmd::TestInject { text, no_inject, no_clipboard }) => {
            test_inject_cmd(&text, no_inject, no_clipboard);
            Ok(())
        }
        Some(Cmd::Hwprobe { json }) => {
            hwprobe_cmd(&paths, json);
            Ok(())
        }
        Some(Cmd::Use { action }) => use_cmd(&paths, action).await,
        Some(Cmd::Keys { action }) => keys_cmd(&paths, action).await,
        Some(Cmd::TestOverlay) => {
            test_overlay_cmd();
            Ok(())
        }
        Some(Cmd::Record { no_inject, max_seconds, stt, polish, live }) => {
            record_cmd(&paths, no_inject, max_seconds, stt.as_deref(), polish.as_deref(), live)
                .await
        }
        Some(Cmd::Transcribe { path, no_polish, stt, polish }) => {
            transcribe_cmd(&paths, &path, no_polish, stt.as_deref(), polish.as_deref()).await
        }
        Some(Cmd::History { search, limit, json, last }) => {
            history_cmd(&paths, search.as_deref(), limit, json, last)
        }
        Some(Cmd::Config { action }) => config_cmd(&paths, action),
        Some(Cmd::Models { action }) => models_cmd(&paths, action).await,
        Some(Cmd::Completions { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "fono", &mut std::io::stdout());
            Ok(())
        }
        Some(Cmd::Discover { json }) => discover_cmd(&paths, json).await,
        Some(Cmd::Update { check, yes, dry_run, channel, no_restart, bin_dir }) => {
            update_cmd(check, yes, dry_run, &channel, no_restart, bin_dir).await
        }
        Some(Cmd::Install { server, desktop, dry_run }) => {
            let mode = if server {
                crate::install::InstallModeArg::Server
            } else if desktop {
                crate::install::InstallModeArg::Desktop
            } else {
                crate::install::InstallModeArg::Auto
            };
            crate::install::run_install(mode, dry_run)
        }
        Some(Cmd::Uninstall { dry_run }) => crate::install::run_uninstall(dry_run),
        Some(Cmd::Speak { action }) => match action {
            SpeakCmd::Stream => crate::speak_stream::run(&paths).await,
        },
        Some(Cmd::Summarize { json, sender, chat, source, instructions, voice, silent }) => {
            summarize_cmd(&paths, json, sender, chat, source, instructions, voice, silent).await
        }
        Some(Cmd::Voices { action }) => voices_cmd(&paths, action).await,
        Some(Cmd::Mcp { action }) => match action {
            McpCmd::Serve => {
                let cfg = fono_core::Config::load(&paths.config_file())?;
                if !cfg.mcp.enabled {
                    anyhow::bail!(
                        "MCP server is disabled in your config (`[mcp] enabled = false`).\n\n\
                         Re-enable it with:\n\n  fono use mcp-server on"
                    );
                }
                let secrets = fono_core::Secrets::load(&paths.secrets_file()).unwrap_or_default();
                let ctx = fono_mcp_server::McpContext {
                    cfg,
                    secrets,
                    whisper_models_dir: paths.whisper_models_dir(),
                    polish_models_dir: paths.polish_models_dir(),
                    polish_classifier_cache: fono_mcp_server::McpContext::new_classifier_cache(),
                    daemon_ipc_candidates: paths.client_ipc_socket_candidates(),
                    client_identity: fono_mcp_server::McpContext::new_client_identity(),
                };
                let registry = fono_mcp_server::ToolRegistry::default_with_context(&ctx);
                let transport = fono_mcp_server::StdioTransport::new();
                let mut server = fono_mcp_server::McpServer::new(Box::new(transport), registry);
                server.run().await
            }
        },
        Some(Cmd::AgentSetup { agent, project_dir, dry_run, list }) => {
            agent_setup::run(agent.as_deref(), &project_dir, dry_run, list, &paths).await
        }
    }
}

async fn ipc_simple(paths: &Paths, req: Request) -> Result<()> {
    match fono_ipc::request_any(&paths.client_ipc_socket_candidates(), &req).await {
        Ok(Response::Ok) => Ok(()),
        Ok(Response::Text(t)) => {
            println!("{t}");
            Ok(())
        }
        Ok(Response::Discovered(_)) => Ok(()),
        Ok(Response::Error(e)) => Err(anyhow::anyhow!(e)),
        Ok(Response::McpListenCancelled) => Ok(()),
        Err(e) => Err(e),
    }
}

/// `fono summarize` — read notification content from stdin,
/// summarize it via the configured `[assistant]` backend, and speak
/// (or, with `--silent`, only print) the 1-2 sentence summary.
///
/// Shares `fono_mcp_server::summarize` with the `fono.summarize`
/// MCP tool, so both transports produce identical summaries for
/// identical payloads by construction. Flag overrides (`--sender`,
/// `--chat`, `--source`, `--instructions`) apply in both raw and
/// `--json` modes, taking precedence over payload fields.
#[allow(clippy::too_many_arguments)]
async fn summarize_cmd(
    paths: &Paths,
    json: bool,
    sender: Option<String>,
    chat: Option<String>,
    source: Option<String>,
    instructions: Option<String>,
    voice: Option<String>,
    silent: bool,
) -> Result<()> {
    use std::io::Read;

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).context("read stdin")?;
    if input.trim().is_empty() {
        anyhow::bail!(
            "no input on stdin — pipe the notification content, e.g. \
             `echo \"Mihai: build failed\" | fono summarize`"
        );
    }

    let mut payload = if json {
        serde_json::from_str::<fono_mcp_server::summarize::SummarizePayload>(&input)
            .context("parse stdin as `fono.summarize` JSON payload")?
    } else {
        fono_mcp_server::summarize::SummarizePayload { message_text: input, ..Default::default() }
    };
    if let Some(s) = sender {
        payload.sender_name = s;
    }
    if let Some(c) = chat {
        payload.chat_name = c;
    }
    if let Some(s) = source {
        payload.source_app = s;
    }
    if let Some(i) = instructions {
        payload.instructions = i;
    }

    let cfg = fono_core::Config::load(&paths.config_file())?;
    let secrets = fono_core::Secrets::load(&paths.secrets_file()).unwrap_or_default();

    let summary =
        fono_mcp_server::summarize::summarize(&cfg, &secrets, &paths.polish_models_dir(), &payload)
            .await?;

    println!("{summary}");
    if silent {
        return Ok(());
    }
    // Resolve the per-program voice from `source_app` (and the explicit
    // `--voice` override, which still wins) so a CLI-driven notifier
    // gets the same per-program voice the MCP tool would. Falls back to
    // the backend default when nothing matches.
    let program =
        if payload.source_app.trim().is_empty() { None } else { Some(payload.source_app.trim()) };
    let resolved =
        fono_mcp_server::voice_io::resolve_program_voice(&cfg, program, voice.as_deref());
    // `fono summarize` is typically driven headlessly by a notifier /
    // the `fono.summarize` MCP tool, so a failure on stderr is usually
    // invisible. Surface actionable TTS failures (key rejected, 402
    // paid-plan, network, missing key) as a desktop notification the
    // same way the daemon assistant path does, then still propagate the
    // error for the exit code / any attached terminal.
    let spoken = fono_mcp_server::voice_io::speak_text(
        &cfg,
        &secrets,
        &summary,
        resolved.as_deref(),
        &paths.client_ipc_socket_candidates(),
    )
    .await
    .map(|_| ());
    if let Err(e) = &spoken {
        let err_text = format!("{e:#}");
        let provider = fono_core::providers::tts_backend_str(&cfg.tts.backend);
        fono_core::critical_notify::notify_actionable(
            fono_core::critical_notify::Stage::Tts,
            provider,
            &err_text,
        );
    }
    spoken
}

/// `fono voices …` — inspect and manage per-program TTS voices.
///
/// All addressing is by positional gendered label ("Female 1", "Male 2")
/// against the **active** TTS backend's palette, so the user never deals
/// with cryptic backend-specific voice ids.
#[allow(clippy::too_many_lines)]
async fn voices_cmd(paths: &Paths, action: VoicesCmd) -> Result<()> {
    use fono_core::voice_palette::Gender;

    let path = paths.config_file();
    let mut cfg = Config::load(&path)?;
    let backend = fono_core::providers::tts_backend_str(&cfg.tts.backend);

    match action {
        VoicesCmd::List => {
            let palette = fono_mcp_server::voice_io::active_palette(&cfg);
            println!("Active TTS backend: {backend}");
            if palette.is_empty() {
                println!(
                    "\nNo curated voice palette for this backend — programs use the backend \
                     default voice."
                );
            } else {
                println!("\nVoice palette (address voices by the label on the left):");
                for (label, voice) in palette.labelled() {
                    println!("  {label:<10}  {}  [{}]", voice.backend_id, voice.gender);
                }
            }
            let pref = cfg.mcp.voice_gender.trim();
            println!(
                "\nGender preference: {}",
                if pref.is_empty() { "any (no preference)" } else { pref }
            );
            println!(
                "Automatic assignment: {}",
                if cfg.mcp.auto_assign_voices { "on" } else { "off" }
            );
            if cfg.mcp.voices.is_empty() {
                println!("\nNo per-program pins. Unpinned programs get a stable automatic voice.");
            } else {
                println!("\nPer-program pins:");
                for (program, label) in &cfg.mcp.voices {
                    println!("  {program:<20}  {label}");
                }
            }
            Ok(())
        }
        VoicesCmd::Set { program, label } => {
            let program = program.trim().to_string();
            if program.is_empty() {
                anyhow::bail!("program name must not be empty");
            }
            let palette = fono_mcp_server::voice_io::active_palette(&cfg);
            let stored = if label.trim().eq_ignore_ascii_case("auto") {
                "auto".to_string()
            } else if let Some((gender, position)) = fono_core::voice_palette::parse_label(&label) {
                // Positional label: validate the slot exists on the active
                // backend and store a canonical, re-readable form.
                if palette.by_label(&label).is_none() {
                    anyhow::bail!(
                        "no voice {:?} on the {backend} backend — run `fono voices list` to see \
                         the available labels",
                        fono_core::voice_palette::positional_label(gender, position)
                    );
                }
                fono_core::voice_palette::positional_label(gender, position)
            } else {
                // Treat as a raw backend id. Warn (don't fail) when it's
                // not in the curated palette — power users may pin an
                // off-palette id the backend still accepts.
                if !palette.is_empty() && palette.by_backend_id(label.trim()).is_none() {
                    eprintln!(
                        "warning: {:?} is not a positional label or a curated palette voice for \
                         the {backend} backend; pinning it as a raw backend id",
                        label.trim()
                    );
                }
                label.trim().to_string()
            };
            cfg.mcp.voices.insert(program.clone(), stored.clone());
            cfg.save(&path)?;
            println!("Pinned {program:?} → {stored:?} (backend: {backend}).");
            Ok(())
        }
        VoicesCmd::Unset { program } => {
            let program = program.trim();
            if cfg.mcp.voices.remove(program).is_some() {
                cfg.save(&path)?;
                println!("Removed pin for {program:?}; it now uses automatic assignment.");
            } else {
                println!("No pin for {program:?}; nothing to remove.");
            }
            Ok(())
        }
        VoicesCmd::Gender { gender } => {
            let g = gender.trim();
            if g.eq_ignore_ascii_case("any") || g.is_empty() {
                cfg.mcp.voice_gender.clear();
                cfg.save(&path)?;
                println!("Cleared gender preference (any).");
            } else {
                let parsed = Gender::parse(g).ok_or_else(|| {
                    anyhow::anyhow!("unknown gender {gender:?}; use male, female, or any")
                })?;
                cfg.mcp.voice_gender = parsed.as_str().to_string();
                cfg.save(&path)?;
                println!("Gender preference set to {}.", parsed.as_str());
            }
            Ok(())
        }
        VoicesCmd::Preview { label } => {
            let secrets = fono_core::Secrets::load(&paths.secrets_file()).unwrap_or_default();
            // Reuse the resolver so a positional label, "auto", or a raw id
            // all map exactly as they would at speak time.
            let resolved = fono_mcp_server::voice_io::resolve_program_voice(
                &cfg,
                Some("fono-voices-preview"),
                Some(label.trim()),
            );
            let sample = format!("This is the {} voice on the {backend} backend.", label.trim());
            println!(
                "Previewing {label:?} → {}",
                resolved.as_deref().unwrap_or("(backend default)")
            );
            fono_mcp_server::voice_io::speak_text(
                &cfg,
                &secrets,
                &sample,
                resolved.as_deref(),
                &paths.client_ipc_socket_candidates(),
            )
            .await
            .map(|_| ())
        }
    }
}

/// `fono discover [--json]` — print the daemon's live mDNS registry.
/// Slice 4 of the network plan.
async fn discover_cmd(paths: &Paths, json: bool) -> Result<()> {
    let resp = fono_ipc::request_any(
        &paths.client_ipc_socket_candidates(),
        &fono_ipc::Request::ListDiscovered,
    )
    .await?;
    let peers = match resp {
        fono_ipc::Response::Discovered(p) => p,
        fono_ipc::Response::Error(e) => return Err(anyhow::anyhow!(e)),
        _ => return Err(anyhow::anyhow!("unexpected response from daemon")),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&peers)?);
        return Ok(());
    }
    if peers.is_empty() {
        println!(
            "no LAN peers discovered (ensure the daemon is running and any LAN server has [server.wyoming].enabled = true)"
        );
        return Ok(());
    }
    println!(
        "KIND     HOST                         PORT   PROTO          MODEL                    AUTH"
    );
    for p in &peers {
        println!(
            "{:<8} {:<28} {:<6} {:<14} {:<24} {}",
            p.kind,
            p.hostname,
            p.port,
            p.proto,
            p.model.as_deref().unwrap_or("-"),
            if p.auth_required { "token" } else { "none" },
        );
    }
    Ok(())
}

fn history_cmd(
    paths: &Paths,
    search: Option<&str>,
    limit: usize,
    json: bool,
    last: bool,
) -> Result<()> {
    let db = fono_core::history::HistoryDb::open(&paths.history_db())?;
    let rows = if last {
        db.recent(1)?
    } else if let Some(q) = search {
        db.search(q, limit)?
    } else {
        db.recent(limit)?
    };
    if last {
        if rows.is_empty() {
            println!("(no history yet)");
            return Ok(());
        }
        let t = &rows[0];
        if json {
            let v = serde_json::json!({
                "id": t.id,
                "ts": t.ts,
                "duration_ms": t.duration_ms,
                "raw": t.raw,
                "cleaned": t.cleaned,
                "app_class": t.app_class,
                "app_title": t.app_title,
                "stt_backend": t.stt_backend,
                "polish_backend": t.polish_backend,
                "language": t.language,
            });
            println!("{}", serde_json::to_string_pretty(&v)?);
        } else {
            println!("id           : {:?}", t.id);
            println!("ts           : {}", t.ts);
            println!("duration_ms  : {:?}", t.duration_ms);
            println!("language     : {:?}", t.language);
            println!("app_class    : {:?}", t.app_class);
            println!("app_title    : {:?}", t.app_title);
            println!("stt_backend  : {:?}", t.stt_backend);
            println!("polish_backend  : {:?}", t.polish_backend);
            println!("raw          : {}", t.raw);
            println!("cleaned      : {}", t.cleaned.as_deref().unwrap_or("(none — no polish)"));
        }
        return Ok(());
    }
    if json {
        let arr: Vec<_> = rows
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "ts": t.ts,
                    "raw": t.raw,
                    "cleaned": t.cleaned,
                    "language": t.language,
                    "stt_backend": t.stt_backend,
                    "polish_backend": t.polish_backend,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
    } else if rows.is_empty() {
        println!("(no history yet)");
    } else {
        for t in rows {
            let text = t.cleaned.as_deref().unwrap_or(&t.raw);
            println!("[{}] {}", t.ts, text);
        }
    }
    Ok(())
}

fn config_cmd(paths: &Paths, action: ConfigCmd) -> Result<()> {
    match action {
        ConfigCmd::Path => {
            println!("{}", paths.config_file().display());
        }
        ConfigCmd::Show => {
            let cfg = Config::load(&paths.config_file())?;
            println!("{}", toml::to_string_pretty(&cfg)?);
        }
        ConfigCmd::Edit => {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".to_string());
            let status = std::process::Command::new(&editor).arg(paths.config_file()).status()?;
            if !status.success() {
                return Err(anyhow::anyhow!("{editor} exited with {status}"));
            }
        }
    }
    let _ = Secrets::load(&paths.secrets_file())?; // surface mode errors
    Ok(())
}

async fn models_cmd(paths: &Paths, action: ModelsCmd) -> Result<()> {
    use fono_stt::{ModelRegistry, Quantization, QuantizationPref};
    match action {
        ModelsCmd::List => {
            for m in ModelRegistry::all() {
                let default_quant = m.default_quantization;
                let dest =
                    paths.whisper_models_dir().join(ModelRegistry::filename(m.name, default_quant));
                let marker = if dest.exists() { "[installed]" } else { "           " };
                let kind = if m.multilingual { "multilingual" } else { "english-only" };
                println!(
                    "{marker} whisper:{:<15} default={:<5} {:>5} MB  {kind}",
                    m.name,
                    default_quant.as_str(),
                    m.approx_mb,
                );
                // List alternative quantizations (skip the default itself).
                for v in m.quantizations {
                    if v.quantization == default_quant {
                        continue;
                    }
                    let alt = paths
                        .whisper_models_dir()
                        .join(ModelRegistry::filename(m.name, v.quantization));
                    let alt_marker = if alt.exists() { "[installed]" } else { "           " };
                    println!(
                        "{alt_marker}   └─ {:<5} {:>5} MB  (install with \
                         `fono models install {} --quantization {}`)",
                        v.quantization.as_str(),
                        v.approx_mb,
                        m.name,
                        v.quantization.as_str(),
                    );
                }
            }
        }
        ModelsCmd::Install { name, quantization } => {
            let m = ModelRegistry::get(&name)
                .ok_or_else(|| anyhow::anyhow!("unknown model {name:?}"))?;
            let pref = QuantizationPref::parse(&quantization).ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid --quantization {quantization:?} — expected \
                     `auto`, `fp16`, `q5_1`, or `q8_0`"
                )
            })?;
            let quant = ModelRegistry::resolve_quantization(m, pref).map_err(anyhow::Error::msg)?;
            let variant = ModelRegistry::variant_for(m, quant)
                .expect("resolve_quantization guarantees the variant exists");
            let dest = paths.whisper_models_dir().join(ModelRegistry::filename(m.name, quant));
            if dest.exists() {
                println!("already installed: {}", dest.display());
                return Ok(());
            }
            let url = ModelRegistry::url_for(m, quant)
                .expect("variant lookup succeeded so URL must resolve");
            println!(
                "Downloading {} ({}) — {} MB\n  from {url}\n  to   {}",
                m.name,
                quant,
                variant.approx_mb,
                dest.display()
            );
            fono_download::download(&url, &dest, variant.sha256).await?;
            println!("Installed: {}", dest.display());
        }
        ModelsCmd::Remove { name } => {
            // Remove every quantization variant of the model (so users
            // don't have to remember which one was on disk). Unknown
            // model names fall through to a single best-effort attempt.
            let mut removed = 0usize;
            if let Some(m) = ModelRegistry::get(&name) {
                for q in [Quantization::Fp16, Quantization::Q5_1, Quantization::Q8_0] {
                    let path = paths.whisper_models_dir().join(ModelRegistry::filename(m.name, q));
                    if path.exists() {
                        std::fs::remove_file(&path)?;
                        println!("removed {}", path.display());
                        removed += 1;
                    }
                }
            } else {
                let path = paths.whisper_models_dir().join(format!("ggml-{name}.bin"));
                if path.exists() {
                    std::fs::remove_file(&path)?;
                    println!("removed {}", path.display());
                    removed += 1;
                }
            }
            if removed == 0 {
                println!("not installed: {name}");
            }
        }
        ModelsCmd::Verify => {
            println!("model verification scheduled for a follow-up phase");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// `fono record` — one-shot capture → STT → LLM → inject from CLI.
// ---------------------------------------------------------------------
async fn record_cmd(
    paths: &Paths,
    no_inject: bool,
    max_seconds: u64,
    stt_override: Option<&str>,
    llm_override: Option<&str>,
    live: bool,
) -> Result<()> {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use fono_audio::{AudioCapture, CaptureConfig};
    use fono_core::{Config, Secrets};

    let mut config = Config::load(&paths.config_file())?;
    apply_backend_overrides(&mut config, stt_override, llm_override)?;
    let config = Arc::new(config);
    let secrets = Secrets::load(&paths.secrets_file())?;

    if live {
        return record_cmd_live(paths, &config, &secrets, max_seconds, no_inject).await;
    }

    let cap_cfg = CaptureConfig { target_sample_rate: config.audio.sample_rate };
    let cap = AudioCapture::new(cap_cfg.clone());
    let handle = cap.start().context("start audio capture")?;
    eprintln!(
        "fono record: capturing from default input ({} Hz). Press Ctrl-C or wait \
         {max_seconds}s to stop.",
        cap_cfg.target_sample_rate
    );

    let started = Instant::now();
    let max = if max_seconds == 0 {
        Duration::from_secs(60 * 60)
    } else {
        Duration::from_secs(max_seconds)
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("fono record: stopped by Ctrl-C");
        }
        () = tokio::time::sleep(max) => {
            eprintln!("fono record: hit {max_seconds}s timeout");
        }
    }
    let elapsed = started.elapsed();
    let pcm = {
        let buf = handle.buffer.lock().expect("buffer mutex");
        buf.samples().to_vec()
    };
    drop(handle);

    let stt =
        fono_stt::build_stt(&config.stt, &config.general, &secrets, &paths.whisper_models_dir())?;
    let polish = fono_polish::build_polish(&config.polish, &secrets, &paths.polish_models_dir())?;

    eprintln!(
        "fono record: captured {} samples ({} ms); running STT…",
        pcm.len(),
        elapsed.as_millis()
    );
    let lang = config.general.language_override();
    let trans = stt.transcribe(&pcm, cap_cfg.target_sample_rate, lang).await?;
    let raw = trans.text.trim().to_string();
    if raw.is_empty() {
        eprintln!("fono record: STT returned empty text");
        return Ok(());
    }
    let final_text = if let Some(l) = polish.as_ref() {
        let ctx = fono_polish::FormatContext {
            main_prompt: config.polish.prompt.main.clone(),
            advanced_prompt: config.polish.prompt.advanced.clone(),
            dictionary: config.polish.prompt.dictionary.clone(),
            language: trans.language.clone(),
            ..Default::default()
        };
        match l.format(&raw, &ctx).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("fono record: polish failed ({e:#}); using raw transcript");
                raw.clone()
            }
        }
    } else {
        raw.clone()
    };
    println!("{final_text}");
    if !no_inject {
        if let Err(e) = fono_inject::type_text(&final_text) {
            eprintln!("fono record: inject failed: {e:#}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// `fono transcribe <PATH>` — WAV file → STT (+optional LLM) → stdout.
// ---------------------------------------------------------------------
async fn transcribe_cmd(
    paths: &Paths,
    wav: &std::path::Path,
    no_polish: bool,
    stt_override: Option<&str>,
    llm_override: Option<&str>,
) -> Result<()> {
    use fono_core::{Config, Secrets};

    let mut config = Config::load(&paths.config_file())?;
    apply_backend_overrides(&mut config, stt_override, llm_override)?;
    let secrets = Secrets::load(&paths.secrets_file())?;
    let (pcm, sample_rate) =
        read_wav_mono_f32(wav).with_context(|| format!("read wav {}", wav.display()))?;
    let stt =
        fono_stt::build_stt(&config.stt, &config.general, &secrets, &paths.whisper_models_dir())?;
    let polish = if no_polish {
        None
    } else {
        fono_polish::build_polish(&config.polish, &secrets, &paths.polish_models_dir())?
    };
    let trans = stt.transcribe(&pcm, sample_rate, None).await?;
    let raw = trans.text.trim().to_string();
    if let Some(l) = polish.as_ref() {
        let ctx = fono_polish::FormatContext {
            main_prompt: config.polish.prompt.main.clone(),
            advanced_prompt: config.polish.prompt.advanced.clone(),
            dictionary: config.polish.prompt.dictionary.clone(),
            language: trans.language.clone(),
            ..Default::default()
        };
        match l.format(&raw, &ctx).await {
            Ok(c) => println!("{c}"),
            Err(e) => {
                eprintln!("polish failed ({e:#}); raw transcript follows:");
                println!("{raw}");
            }
        }
    } else {
        println!("{raw}");
    }
    Ok(())
}

/// Minimal 16-bit-PCM mono WAV reader (no `hound` dep). Supports stereo
/// by averaging channels. Returns `(f32 samples in [-1.0, 1.0], rate)`.
fn read_wav_mono_f32(path: &std::path::Path) -> Result<(Vec<f32>, u32)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        anyhow::bail!("not a RIFF/WAVE file: {}", path.display());
    }
    let mut i = 12;
    let mut fmt_chans: u16 = 1;
    let mut fmt_rate: u32 = 16_000;
    let mut fmt_bps: u16 = 16;
    let mut data_off = 0;
    let mut data_len = 0;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let sz =
            u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        let body = i + 8;
        if id == b"fmt " {
            fmt_chans = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]);
            fmt_rate = u32::from_le_bytes([
                bytes[body + 4],
                bytes[body + 5],
                bytes[body + 6],
                bytes[body + 7],
            ]);
            fmt_bps = u16::from_le_bytes([bytes[body + 14], bytes[body + 15]]);
        } else if id == b"data" {
            data_off = body;
            data_len = sz;
            break;
        }
        i = body + sz;
    }
    if data_off == 0 {
        anyhow::bail!("no `data` chunk in {}", path.display());
    }
    if fmt_bps != 16 {
        anyhow::bail!("only 16-bit PCM supported (got {fmt_bps}-bit)");
    }
    let body = &bytes[data_off..data_off + data_len];
    let frames = body.len() / 2 / fmt_chans as usize;
    let mut out = Vec::with_capacity(frames);
    for f_i in 0..frames {
        let mut sum = 0f32;
        for c in 0..fmt_chans {
            let off = (f_i * fmt_chans as usize + c as usize) * 2;
            let s = i16::from_le_bytes([body[off], body[off + 1]]);
            sum += f32::from(s) / f32::from(i16::MAX);
        }
        out.push(sum / f32::from(fmt_chans));
    }
    Ok((out, fmt_rate))
}

// ---------------------------------------------------------------------
// `fono hwprobe` — print the hardware snapshot + recommended local tier.
// ---------------------------------------------------------------------

/// Recommendation surfaced by `fono hwprobe`. Computed by replaying the
/// wizard's shortlist logic (`build_local_stt_shortlist`) against this
/// host's snapshot so the report agrees with the model the wizard would
/// actually pick — not the static tier→model lookup table, which doesn't
/// know about per-model affordability.
struct HwprobeRecommendation {
    model: &'static str,
    accuracy: crate::wizard::AccuracyBucket,
}

fn compute_hwprobe_recommendation(
    paths: &Paths,
    snap: &fono_core::HardwareSnapshot,
) -> Option<HwprobeRecommendation> {
    // Pull configured languages if a config exists so the recommendation
    // matches the wizard's accuracy ranking for this user's selection.
    // Fall back to OS-detected locales, then to an empty list (which
    // makes `accuracy_for_langs` score against English WERs).
    let langs: Vec<String> = Config::load(&paths.config_file())
        .ok()
        .map(|c| c.general.languages)
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| {
            fono_core::locale::detect_user_languages_ranked().into_iter().map(|d| d.code).collect()
        });
    let english_only = !langs.is_empty() && langs.iter().all(|l| l == "en");
    // Affordability is computed against the inference path this binary
    // can actually use: the CPU variant has no Vulkan inference
    // backend, so a probed GPU must not feed into the shortlist on a
    // CPU build (see `HardwareSnapshot::for_inference`).
    let inference_snap =
        snap.for_inference(matches!(crate::variant::VARIANT, crate::variant::Variant::Gpu));
    let shortlist = crate::wizard::build_local_stt_shortlist(english_only, &langs, &inference_snap);
    shortlist
        .into_iter()
        .next()
        .map(|e| HwprobeRecommendation { model: e.model.name, accuracy: e.accuracy })
}

fn accuracy_label(acc: crate::wizard::AccuracyBucket) -> &'static str {
    use crate::wizard::AccuracyBucket;
    match acc {
        AccuracyBucket::Excellent => "excellent",
        AccuracyBucket::Good => "good",
        AccuracyBucket::Acceptable => "acceptable",
        AccuracyBucket::Inaccurate => "inaccurate",
        AccuracyBucket::Unknown => "untested",
    }
}

fn hwprobe_cmd(paths: &Paths, json: bool) {
    use fono_core::{hwcheck, vulkan_probe};
    let mut snap = hwcheck::probe(&paths.cache_dir);
    // Reuse the same Vulkan probe the update / tray flow does (loads
    // libvulkan.so.1 in a subprocess and enumerates physical devices).
    // The result is cached for the lifetime of the process, so calling
    // it here costs nothing if the daemon already probed at startup.
    let vulkan = vulkan_probe::probe();
    // Upgrade `host_gpu` based on the Vulkan probe (no-op on Apple
    // Silicon, which already starts at Integrated). See ADR 0028.
    if snap.host_gpu == hwcheck::HostGpu::None {
        snap.host_gpu = vulkan.host_gpu_class();
    }
    let tier = snap.tier();
    let rec = compute_hwprobe_recommendation(paths, &snap);
    let vulkan_usable = vulkan.is_usable();
    // Suggest an accelerated build only when (a) this is the CPU-only
    // ship and (b) the host has a usable Vulkan device. Matches the
    // logic that lights up the tray's "Update for GPU acceleration"
    // entry (`fono_update::desired_asset_prefix`).
    let suggest_vulkan_upgrade =
        crate::variant::VARIANT == crate::variant::Variant::Cpu && vulkan_usable;
    if json {
        let vulkan_devices: Vec<serde_json::Value> = match &vulkan {
            vulkan_probe::Outcome::Available { devices } => devices
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "name": d.name,
                        "class": match d.class {
                            vulkan_probe::DeviceClass::Integrated => "integrated",
                            vulkan_probe::DeviceClass::Discrete => "discrete",
                            vulkan_probe::DeviceClass::Virtual => "virtual",
                            vulkan_probe::DeviceClass::Cpu => "cpu",
                        },
                        "supports_fp16": d.supports_fp16,
                        "supports_cooperative_matrix": d.supports_cooperative_matrix,
                    })
                })
                .collect(),
            vulkan_probe::Outcome::NotAvailable { .. } => Vec::new(),
        };
        let v = serde_json::json!({
            "snapshot": snap,
            "tier": tier.as_str(),
            "default_whisper_model": rec.as_ref().map_or_else(
                || fono_stt::registry::ModelRegistry::pick_default_local(
                    &snap.for_inference(matches!(
                        crate::variant::VARIANT,
                        crate::variant::Variant::Gpu
                    )),
                ),
                |r| r.model,
            ),
            "recommendation": rec.as_ref().map(|r| serde_json::json!({
                "model": r.model,
                "accuracy": accuracy_label(r.accuracy),
            })),
            "local_default": tier.local_default(),
            "variant": crate::variant::VARIANT.label(),
            "host_gpu": match snap.host_gpu {
                fono_core::hwcheck::HostGpu::None => "none",
                fono_core::hwcheck::HostGpu::Integrated => "integrated",
                fono_core::hwcheck::HostGpu::IntegratedTensor => "integrated-tensor",
                fono_core::hwcheck::HostGpu::Discrete => "discrete",
            },
            "vulkan_available": vulkan_usable,
            "vulkan_devices": vulkan_devices,
            "suggest_vulkan_upgrade": suggest_vulkan_upgrade,
            "suitability": match snap.suitability() {
                Ok(()) => serde_json::Value::Null,
                Err(reason) => serde_json::Value::String(reason.to_string()),
            },
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return;
    }
    let ram_gb = snap.total_ram_bytes / (1024 * 1024 * 1024);
    let disk_gb = snap.free_disk_bytes / (1024 * 1024 * 1024);
    let isa = if snap.cpu_features.avx2 {
        "AVX2"
    } else if snap.cpu_features.neon {
        "NEON"
    } else {
        "no-vec"
    };
    println!("cores : {} physical / {} logical  ({isa})", snap.physical_cores, snap.logical_cores);
    println!("ram   : {ram_gb} GB total · disk free : {disk_gb} GB · {}/{}", snap.os, snap.arch);
    match &rec {
        Some(r) => println!(
            "tier  : {} (recommends whisper-{} — accuracy: {})",
            tier.as_str(),
            r.model,
            accuracy_label(r.accuracy),
        ),
        None => println!(
            "tier  : {} (no local model fits this host — cloud STT recommended)",
            tier.as_str(),
        ),
    }
    println!("build : {} ({})", crate::variant::VARIANT.label(), vulkan.summary_line());
    if let Err(reason) = snap.suitability() {
        println!("note  : unsuitable for local — {reason}");
    }
    if suggest_vulkan_upgrade {
        println!(
            "accel : GPU detected but this is the CPU-only build. \
             Run `fono update` to switch to the GPU fono build."
        );
    }
}

// ---------------------------------------------------------------------
// `fono use …` — switch active STT / LLM (provider-switching plan S4).
// ---------------------------------------------------------------------

/// Mutate `config` so that future `build_stt` / `build_polish` calls pick
/// up the requested backend. Used both by `fono use` (persisted) and
/// the per-call `--stt` / `--polish` overrides on `record` / `transcribe`
/// (provider-switching plan task S6).
fn apply_backend_overrides(
    cfg: &mut Config,
    stt: Option<&str>,
    polish: Option<&str>,
) -> Result<()> {
    use fono_core::providers::{parse_polish_backend, parse_stt_backend};
    if let Some(s) = stt {
        let backend = parse_stt_backend(s).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown STT backend {s:?}; valid: local, groq, openai, deepgram, \
                 assemblyai, cartesia, azure, speechmatics, google, nemotron"
            )
        })?;
        set_active_stt(cfg, backend);
    }
    if let Some(l) = polish {
        let backend = parse_polish_backend(l).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown polish backend {l:?}; valid: none, local, cerebras, groq, \
                 openai, anthropic, openrouter, ollama, gemini"
            )
        })?;
        set_active_llm(cfg, backend);
    }
    Ok(())
}

/// Atomically swap the active STT backend in the config struct without
/// touching unrelated fields. Provider-switching plan task S5 — never
/// drop user customisations (hotkeys, prompts, history settings).
pub fn set_active_stt(cfg: &mut Config, backend: fono_core::config::SttBackend) {
    cfg.stt.backend = backend;
    // Clear stale cloud sub-block so the factory falls through to the
    // canonical env-var. Local STT keeps cfg.stt.local.* intact.
    cfg.stt.cloud = None;
}

/// Atomically swap the active assistant backend, mirroring
/// [`set_active_llm`]. Enables the assistant when a real backend is
/// selected and disables it on `None`.
pub fn set_active_assistant(cfg: &mut Config, backend: fono_core::config::AssistantBackend) {
    use fono_core::config::AssistantBackend;
    let none = matches!(backend, AssistantBackend::None);
    cfg.assistant.backend = backend;
    cfg.assistant.enabled = !none;
    cfg.assistant.cloud = None;
}

/// Atomically swap the active TTS backend. `wyoming_uri` populates
/// `[tts.wyoming].uri` when the backend is Wyoming; ignored otherwise.
pub fn set_active_tts(
    cfg: &mut Config,
    backend: fono_core::config::TtsBackend,
    wyoming_uri: Option<String>,
) {
    use fono_core::config::{TtsBackend, TtsWyoming};
    use fono_core::providers::tts_backend_str;
    cfg.tts.backend = backend.clone();
    cfg.tts.cloud = None;
    // Reset `voice` to the new backend's catalogue default so a voice ID
    // valid for the previous provider (e.g. a Cartesia UUID, or an
    // OpenAI "alloy") doesn't leak into the next one and trigger a 400
    // ("voice must be one of ..."). Wyoming and None have no catalogue
    // entry — clear to empty so the server / factory picks its own
    // default. Mirrors `wizard::apply_primary_provider`.
    cfg.tts.voice = fono_core::provider_catalog::find(tts_backend_str(&backend))
        .and_then(|entry| entry.tts.as_ref())
        .map(|tts_def| tts_def.default_voice.to_string())
        .unwrap_or_default();
    if matches!(backend, TtsBackend::Wyoming) {
        let uri = wyoming_uri.unwrap_or_else(|| {
            cfg.tts
                .wyoming
                .as_ref()
                .map(|w| w.uri.clone())
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| fono_tts::defaults::DEFAULT_WYOMING_URI.to_string())
        });
        cfg.tts.wyoming = Some(TtsWyoming { uri, ..TtsWyoming::default() });
    } else {
        cfg.tts.wyoming = None;
    }
}

/// Atomically swap the active polish backend. Enables/disables cleanup as
/// appropriate (None → disabled, anything else → enabled).
pub fn set_active_llm(cfg: &mut Config, backend: fono_core::config::PolishBackend) {
    use fono_core::config::PolishBackend;
    let none = matches!(backend, PolishBackend::None);
    cfg.polish.backend = backend;
    cfg.polish.enabled = !none;
    cfg.polish.cloud = None;
}

#[allow(clippy::too_many_lines)]
async fn use_cmd(paths: &Paths, action: UseCmd) -> Result<()> {
    use fono_core::config::{PolishBackend, SttBackend};
    use fono_core::providers::{
        assistant_backend_str, cloud_pair, parse_assistant_backend, parse_polish_backend,
        parse_stt_backend, parse_tts_backend, polish_backend_str, stt_backend_str, tts_backend_str,
    };

    let path = paths.config_file();
    let mut cfg = Config::load(&path)?;
    let summary: String = match action {
        UseCmd::Stt { backend } => {
            let b = parse_stt_backend(&backend).ok_or_else(|| {
                anyhow::anyhow!("unknown STT backend {backend:?}; try local, groq, openai, …")
            })?;
            set_active_stt(&mut cfg, b.clone());
            cfg.save(&path)?;
            format!("stt = {}", stt_backend_str(&b))
        }
        UseCmd::Polish { backend } => {
            let b = parse_polish_backend(&backend).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown polish backend {backend:?}; try none, cerebras, groq, openai, …"
                )
            })?;
            set_active_llm(&mut cfg, b.clone());
            cfg.save(&path)?;
            format!("polish = {}", polish_backend_str(&b))
        }
        UseCmd::Assistant { backend } => {
            let b = parse_assistant_backend(&backend).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown assistant backend {backend:?}; try none, anthropic, cerebras, \
                     openai, groq, openrouter, ollama, local"
                )
            })?;
            set_active_assistant(&mut cfg, b.clone());
            cfg.save(&path)?;
            format!("assistant = {}", assistant_backend_str(&b))
        }
        UseCmd::Tts { backend, uri } => {
            let b = parse_tts_backend(&backend).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown TTS backend {backend:?}; try none, local, wyoming, openai, \
                     groq, openrouter, cartesia, deepgram, speechmatics"
                )
            })?;
            set_active_tts(&mut cfg, b.clone(), uri);
            cfg.save(&path)?;
            format!("tts = {}", tts_backend_str(&b))
        }
        UseCmd::Inject { backend } => {
            // Accepted values mirror `fono_inject::Injector::detect()`'s
            // env-override parser. We validate up front so a typo prints
            // a sensible error instead of silently routing to clipboard.
            let normalized = backend.trim().to_ascii_lowercase();
            let accepted =
                ["auto", "clipboard", "none", "xdotool", "wtype", "ydotool", "xtest", "enigo"];
            if !accepted.contains(&normalized.as_str()) {
                anyhow::bail!(
                    "unknown inject backend {backend:?}; try one of: \
                     auto, clipboard, xdotool, wtype, ydotool, xtest, enigo, none"
                );
            }
            cfg.inject.backend.clone_from(&normalized);
            cfg.save(&path)?;
            // Friendly footer for the GNOME-Wayland xdotool opt-in.
            if normalized == "xdotool" {
                eprintln!(
                    "Note: on GNOME-Wayland, the next dictation will trigger GNOME's \
                     \"Allow input emulation\" permission dialog once. \
                     This is GNOME's security gate for keystroke synthesis (not a network \
                     feature, not Fono-specific) — click Allow if you trust us."
                );
            }
            format!("inject = {normalized}")
        }
        UseCmd::Cloud { provider } => {
            let (s, l) = cloud_pair(&provider).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown cloud preset {provider:?}; try groq, cerebras, openai, anthropic, \
                     openrouter, deepgram, assemblyai"
                )
            })?;
            set_active_stt(&mut cfg, s.clone());
            set_active_llm(&mut cfg, l.clone());
            cfg.save(&path)?;
            format!(
                "cloud preset {provider}: stt = {}, polish = {}",
                stt_backend_str(&s),
                polish_backend_str(&l),
            )
        }
        UseCmd::Local => {
            set_active_stt(&mut cfg, SttBackend::Local);
            set_active_llm(&mut cfg, PolishBackend::None);
            cfg.save(&path)?;
            "local: stt = local (whisper), polish = none".to_string()
        }
        UseCmd::McpServer { state } => {
            let on = match state.trim().to_ascii_lowercase().as_str() {
                "on" | "true" | "1" | "enable" | "enabled" => true,
                "off" | "false" | "0" | "disable" | "disabled" => false,
                _ => anyhow::bail!("expected `on` or `off`, got {state:?}"),
            };
            cfg.mcp.enabled = on;
            cfg.save(&path)?;
            format!("mcp.server.enabled = {on}")
        }
        UseCmd::Show => {
            print_show(paths, &cfg).await;
            return Ok(());
        }
    };

    println!("{summary}");

    // Hot-reload the running daemon (provider-switching plan S11). When
    // the daemon is not running this is a no-op with a friendly hint.
    match fono_ipc::request_any(&paths.client_ipc_socket_candidates(), &fono_ipc::Request::Reload)
        .await
    {
        Ok(fono_ipc::Response::Text(t)) => println!("daemon: {t}"),
        Ok(fono_ipc::Response::Ok) => println!("daemon: reloaded"),
        Ok(fono_ipc::Response::Error(e)) => println!("daemon reload error: {e}"),
        Ok(fono_ipc::Response::Discovered(_)) => println!("daemon: reloaded"),
        Ok(fono_ipc::Response::McpListenCancelled) => println!("daemon: reloaded"),
        Err(_) => println!("daemon: not running (config saved; will apply on next start)"),
    }
    Ok(())
}

async fn print_show(paths: &Paths, cfg: &Config) {
    use fono_core::providers::{
        assistant_backend_str, polish_backend_str, stt_backend_str, tts_backend_str,
    };
    println!("config: {}", paths.config_file().display());
    println!("  stt      : {}", stt_backend_str(&cfg.stt.backend));
    println!(
        "  polish      : {}{}",
        polish_backend_str(&cfg.polish.backend),
        if cfg.polish.enabled { "" } else { " (disabled)" }
    );
    println!(
        "  assistant: {}{}",
        assistant_backend_str(&cfg.assistant.backend),
        if cfg.assistant.enabled { "" } else { " (disabled)" }
    );
    println!("  tts      : {}", tts_backend_str(&cfg.tts.backend));
    match fono_ipc::request_any(&paths.client_ipc_socket_candidates(), &fono_ipc::Request::Status)
        .await
    {
        Ok(fono_ipc::Response::Text(t)) => println!("daemon: {t}"),
        Ok(_) => println!("daemon: running"),
        Err(_) => println!("daemon: not running"),
    }
}

// ---------------------------------------------------------------------
// `fono keys …` — manage secrets.toml (provider-switching plan S7).
// ---------------------------------------------------------------------

async fn keys_cmd(paths: &Paths, action: KeysCmd) -> Result<()> {
    let secrets_path = paths.secrets_file();
    match action {
        KeysCmd::List => {
            let secrets = Secrets::load(&secrets_path)?;
            print_keys_list(&secrets);
        }
        KeysCmd::Add { name, value } => {
            let mut secrets = Secrets::load(&secrets_path).unwrap_or_default();
            let val = match value {
                Some(v) => v,
                None => prompt_for_secret(&name)?,
            };
            secrets.insert(&name, val);
            secrets.save(&secrets_path)?;
            println!("added {name} → {}", secrets_path.display());
            // Hot-reload so the daemon picks up the new key.
            let _ = fono_ipc::request_any(
                &paths.client_ipc_socket_candidates(),
                &fono_ipc::Request::Reload,
            )
            .await;
        }
        KeysCmd::Remove { name } => {
            let mut secrets = Secrets::load(&secrets_path).unwrap_or_default();
            if secrets.keys.remove(&name).is_some() {
                secrets.save(&secrets_path)?;
                println!("removed {name}");
                let _ = fono_ipc::request_any(
                    &paths.client_ipc_socket_candidates(),
                    &fono_ipc::Request::Reload,
                )
                .await;
            } else {
                println!("not found: {name}");
            }
        }
        KeysCmd::Check => {
            // Lightweight: list which env-keys are present; full
            // network reachability is in `fono doctor`.
            let secrets = Secrets::load(&secrets_path).unwrap_or_default();
            print_keys_list(&secrets);
            println!("\nFor live reachability probes, run `fono doctor`.");
        }
    }
    Ok(())
}

fn print_keys_list(secrets: &Secrets) {
    use fono_core::providers::{
        all_polish_backends, all_stt_backends, polish_key_env, polish_requires_key, stt_key_env,
        stt_requires_key,
    };
    println!("api keys (config + environment):");
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for b in all_stt_backends() {
        if !stt_requires_key(&b) {
            continue;
        }
        seen.insert(stt_key_env(&b).to_string());
    }
    for b in all_polish_backends() {
        if !polish_requires_key(&b) {
            continue;
        }
        seen.insert(polish_key_env(&b).to_string());
    }
    for name in seen {
        let from_secrets = secrets.keys.get(&name).cloned();
        let from_env = std::env::var(&name).ok();
        let v = from_secrets.or(from_env);
        match v {
            Some(val) => println!("  {name:<24} = {}", mask(&val)),
            None => println!("  {name:<24} = (unset)"),
        }
    }
    // Also print any extra keys not in the canonical set (e.g.,
    // user-added entries).
    for (k, v) in &secrets.keys {
        if !is_canonical_key(k) {
            println!("  {k:<24} = {} (custom)", mask(v));
        }
    }
}

fn is_canonical_key(name: &str) -> bool {
    use fono_core::providers::{
        all_polish_backends, all_stt_backends, polish_key_env, stt_key_env,
    };
    all_stt_backends().iter().any(|b| stt_key_env(b) == name)
        || all_polish_backends().iter().any(|b| polish_key_env(b) == name)
}

fn mask(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "(empty)".to_string();
    }
    let n = trimmed.chars().count();
    if n <= 6 {
        return "*".repeat(n);
    }
    let head: String = trimmed.chars().take(3).collect();
    let tail: String =
        trimmed.chars().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}…{tail}")
}

fn prompt_for_secret(name: &str) -> Result<String> {
    use std::io::Write;
    eprint!("Enter value for {name}: ");
    std::io::stderr().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    let trimmed = s.trim().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("empty value; aborting");
    }
    Ok(trimmed)
}

/// Smoke-test the inject + clipboard delivery path. Bypasses STT/LLM
/// so users can quickly verify whether text can actually reach their
/// focused window or clipboard.
fn test_inject_cmd(text: &str, no_inject: bool, no_clipboard: bool) {
    use std::time::Instant;
    println!("Fono — test-inject");
    println!("Build: v{}", env!("CARGO_PKG_VERSION"));
    println!("Detected key-injector: {:?}", fono_inject::Injector::detect());
    println!("Text ({} chars): {text:?}", text.chars().count());
    println!();

    if no_inject {
        println!("[1/2] Skipping key injection (--no-inject)");
    } else {
        println!("[1/2] Trying key injection (5s for you to focus a text field)...");
        std::thread::sleep(std::time::Duration::from_secs(5));
        let started = Instant::now();
        match fono_inject::type_text_with_outcome(text) {
            Ok(fono_inject::InjectOutcome::Typed(b)) => {
                println!("      ✓ typed via {b} in {}ms", started.elapsed().as_millis());
            }
            Ok(fono_inject::InjectOutcome::Clipboard(t)) => {
                println!(
                    "      ↳ key injection failed; fell back to clipboard via {t} \
                     in {}ms (press Ctrl+V to paste)",
                    started.elapsed().as_millis()
                );
            }
            Err(e) => {
                println!("      ✗ inject + clipboard both failed: {e:#}");
            }
        }
    }

    if no_clipboard {
        println!("[2/2] Skipping clipboard copy (--no-clipboard)");
    } else {
        println!("[2/2] Forcing clipboard copy via every available tool...");
        println!("      DISPLAY         = {:?}", std::env::var("DISPLAY").ok());
        println!("      WAYLAND_DISPLAY = {:?}", std::env::var("WAYLAND_DISPLAY").ok());
        println!("      XDG_SESSION_TYPE= {:?}", std::env::var("XDG_SESSION_TYPE").ok());
        let started = Instant::now();
        let attempts = fono_inject::copy_to_clipboard_all(text);
        for a in &attempts {
            let mark = if a.success { "✓" } else { "✗" };
            println!("      {mark} {:<8} [{:<9}] {}", a.tool, a.target, a.detail);
        }
        let any_ok = attempts.iter().any(|a| a.success);
        println!(
            "      {} total in {}ms",
            if any_ok {
                "at least one tool wrote the clipboard"
            } else {
                "NO tool wrote the clipboard"
            },
            started.elapsed().as_millis()
        );
        if let Some(readback) = readback_clipboard() {
            let ok = readback.trim() == text;
            println!(
                "      readback: {} ({} bytes via {})",
                if ok { "MATCHES" } else { "DIFFERS" },
                readback.trim().len(),
                if which("wl-paste").is_some() {
                    "wl-paste"
                } else if which("xclip").is_some() {
                    "xclip -o"
                } else {
                    "xsel -o"
                }
            );
        } else {
            println!("      readback: no read-tool installed (install wl-paste or xclip)");
        }
    }
}

fn which(cmd: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(|d| std::path::Path::new(d).join(cmd))
        .find(|p| p.is_file())
}

/// Best-effort readback of the X11/Wayland clipboard for verification.
/// Returns None when no read tool is available.
fn readback_clipboard() -> Option<String> {
    use std::process::{Command, Stdio};
    let candidates: &[(&str, &[&str])] = &[
        ("wl-paste", &["--no-newline"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
        ("xsel", &["--clipboard", "--output"]),
    ];
    for (tool, args) in candidates {
        let Ok(out) = Command::new(tool)
            .args(*args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .output()
        else {
            // Tool not installed or spawn failed — try the next one.
            continue;
        };
        if out.status.success() {
            return Some(String::from_utf8_lossy(&out.stdout).to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------
// `fono update` — check and (optionally) self-replace the binary.
// ---------------------------------------------------------------------
#[allow(clippy::fn_params_excessive_bools)]
async fn update_cmd(
    check_only: bool,
    yes: bool,
    dry_run: bool,
    channel: &str,
    no_restart: bool,
    bin_dir: Option<std::path::PathBuf>,
) -> Result<()> {
    use fono_update::{apply_update, check, ApplyOpts, Channel, UpdateStatus};
    use std::io::Write;

    let chan = Channel::parse(channel).ok_or_else(|| {
        anyhow::anyhow!("unknown channel {channel:?}; try `stable` or `prerelease`")
    })?;

    let current = env!("CARGO_PKG_VERSION");
    let current_prefix = crate::variant::VARIANT.release_asset_prefix();
    println!("fono {current} — checking for updates on the {channel} channel…");
    let status = check(current, current_prefix, chan).await;
    match &status {
        UpdateStatus::UpToDate { .. } => {
            println!("up-to-date {current}");
            if check_only {
                std::process::exit(0);
            }
            return Ok(());
        }
        UpdateStatus::CheckFailed { error, .. } => {
            return Err(anyhow::anyhow!("update check failed: {error}"));
        }
        UpdateStatus::Available { info, .. } => {
            if info.is_variant_switch_only(current) {
                println!(
                    "GPU build available for v{} ({} MB) — same version, different variant",
                    info.version,
                    info.asset_size / 1_048_576
                );
            } else {
                println!(
                    "available {current}->{} ({} MB)",
                    info.version,
                    info.asset_size / 1_048_576
                );
            }
            println!("  asset:  {}", info.asset_name);
            println!("  notes:  {}", info.html_url);
            if check_only {
                std::process::exit(1);
            }
        }
    }
    let info = status.available().ok_or_else(|| anyhow::anyhow!("no update available"))?;

    if !yes {
        eprint!("Apply update now? [y/N] ");
        std::io::stderr().flush().ok();
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        let confirmed = matches!(s.trim().to_ascii_lowercase().as_str(), "y" | "yes");
        if !confirmed {
            println!("aborted");
            return Ok(());
        }
    }

    let opts = ApplyOpts {
        dry_run,
        // Wave 2 Thread B: --bin-dir <path> overrides the autodetected
        // current_exe(). The is_package_managed refusal in apply_update
        // still fires for system paths regardless of the override.
        target_override: bin_dir.map(|d| d.join("fono")),
    };
    let outcome = apply_update(info, opts).await?;
    println!(
        "installed {} bytes (sha256={}) at {}",
        outcome.bytes,
        outcome.sha256,
        outcome.installed_at.display()
    );
    if let Some(bak) = outcome.backup_at.as_ref() {
        println!("previous binary kept at {}", bak.display());
    }
    if dry_run {
        println!("(dry-run; running binary unchanged)");
        return Ok(());
    }
    if no_restart {
        println!("restart fono to use the new binary");
        return Ok(());
    }
    println!("re-executing into new binary…");
    // `restart_in_place`'s Ok variant is `Infallible`; on success
    // execv replaces the process image and never returns. Pass the
    // post-update target path explicitly: `current_exe()` resolves
    // through `/proc/self/exe`, which on Linux still points at the
    // pre-rename inode (now at `<target>.bak`) — exec'ing that runs
    // the OLD binary and the update appears to silently fail.
    let Err(e) = fono_update::restart_in_place(&outcome.installed_at);
    Err(e)
}

// ---------------------------------------------------------------------
// `fono record --live` and `fono test-overlay`. Both are only fully
// functional when the binary was built with `--features interactive`;
// the slim build provides stubs that print a helpful hint.
// Plan v6 / Slice A.
// ---------------------------------------------------------------------

#[cfg(not(feature = "interactive"))]
async fn record_cmd_live(
    _paths: &Paths,
    _config: &fono_core::Config,
    _secrets: &fono_core::Secrets,
    _max_seconds: u64,
    _no_inject: bool,
) -> Result<()> {
    Err(anyhow::anyhow!(
        "live mode requires the `interactive` cargo feature; rebuild with \
         `cargo build --features interactive` (Slice A — see plans/2026-04-27-fono-interactive-v6.md)"
    ))
}

#[cfg(not(feature = "interactive"))]
fn test_overlay_cmd() {
    println!(
        "test-overlay: this binary was built without the `interactive` cargo feature.\n\
         Rebuild with `cargo build --features interactive` to exercise the real \
         winit+softbuffer overlay (plan v6 / Slice A)."
    );
}

#[cfg(feature = "interactive")]
#[allow(clippy::too_many_lines)]
async fn record_cmd_live(
    paths: &Paths,
    config: &fono_core::Config,
    _secrets: &fono_core::Secrets,
    max_seconds: u64,
    no_inject: bool,
) -> Result<()> {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use fono_audio::{AudioCapture, CaptureConfig};
    use fono_core::config::SttBackend;
    use fono_overlay::RealOverlay;
    use fono_stt::StreamingStt;

    use crate::live::{LiveSession, Pump};

    // Slice A is local-first. Cloud streaming lands in Slice B.
    if !matches!(config.stt.backend, SttBackend::Local) {
        return Err(anyhow::anyhow!(
            "live mode in Slice A only supports the local whisper backend; \
             active backend is {:?}. Run `fono use stt local` first, or wait for Slice B \
             cloud streaming.",
            config.stt.backend
        ));
    }

    // Build WhisperLocal directly so we get the StreamingStt impl
    // (the generic `build_stt` factory returns `Arc<dyn SpeechToText>`,
    // which doesn't expose the streaming method).
    let model = &config.stt.local.model;
    let (info, quant) = crate::models::resolve_local_stt(model, &config.stt.local.quantization)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "local whisper model {model:?} is not in the registry — run \
                 `fono models list` to see available names"
            )
        })?;
    let model_path =
        paths.whisper_models_dir().join(fono_stt::ModelRegistry::filename(info.name, quant));
    if !model_path.exists() {
        return Err(anyhow::anyhow!(
            "local whisper model {model:?} ({quant}) not found at {} — \
             run `fono models install {model}`",
            model_path.display()
        ));
    }
    let stt: Arc<dyn StreamingStt> =
        Arc::new(fono_stt::whisper_local::WhisperLocal::new(model_path));

    // Open the overlay; tolerate failure gracefully (headless / hostile compositor).
    let overlay = match RealOverlay::spawn(fono_core::config::WaveformStyle::Transcript) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("fono record --live: overlay unavailable ({e:#}); continuing without it");
            None
        }
    };

    let cap_cfg = CaptureConfig {
        target_sample_rate: 16_000, // streaming pipeline operates at 16 kHz
    };
    let cap = AudioCapture::new(cap_cfg.clone());
    let handle = cap.start().context("start audio capture")?;
    eprintln!(
        "fono record --live: capturing from default input ({} Hz). Press Ctrl-C or wait \
         {max_seconds}s to stop.",
        cap_cfg.target_sample_rate
    );

    // Slice A capture loop: record-then-replay-through-streaming.
    // True real-time push (cpal callback -> AudioFrameStream) lands in
    // Slice B alongside the cpal-callback refactor; the streaming code
    // path is still fully exercised below.
    let started = Instant::now();
    let max = if max_seconds == 0 {
        Duration::from_secs(60 * 60)
    } else {
        Duration::from_secs(max_seconds)
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("fono record --live: stopped by Ctrl-C");
        }
        () = tokio::time::sleep(max) => {
            eprintln!("fono record --live: hit {max_seconds}s timeout");
        }
    }
    let elapsed = started.elapsed();
    let pcm = {
        let buf = handle.buffer.lock().expect("buffer mutex");
        buf.samples().to_vec()
    };
    drop(handle);
    eprintln!(
        "fono record --live: captured {} samples ({} ms); running streaming STT…",
        pcm.len(),
        elapsed.as_millis()
    );

    // Replay the captured PCM through the streaming pipeline in
    // ~30 ms chunks so the preview/finalize lanes still exercise their
    // full code path.
    //
    // The receiver is taken from the pump *before* the run task is
    // spawned and *before* the first push, so the broadcast channel
    // has a live subscriber for every frame and nothing is lost.
    let mut pump = Pump::new(fono_audio::StreamConfig::default());
    let frame_rx = pump.take_receiver()?;
    let session =
        LiveSession::new(Arc::clone(&stt), cap_cfg.target_sample_rate).with_language(match config
            .general
            .languages
            .as_slice()
        {
            [] => None,
            [single] => Some(single.clone()),
            _ => None,
        });
    let session =
        if let Some(o) = overlay.as_ref() { session.with_overlay(o.clone()) } else { session };

    let task = tokio::spawn(session.run(frame_rx, fono_core::QualityFloor::Max));

    let chunk = (cap_cfg.target_sample_rate as usize / 1000) * 30; // 30 ms
    for window in pcm.chunks(chunk.max(1)) {
        pump.push(window);
        // Yield so the run task can drain the broadcast buffer between
        // pushes; otherwise a long replay can outpace the channel
        // capacity (default 64 frames).
        tokio::task::yield_now().await;
    }
    pump.finish();
    drop(pump);

    let transcript = task.await??;

    if let Some(o) = overlay.as_ref() {
        o.shutdown();
    }

    let final_text = transcript.committed.trim().to_string();
    if final_text.is_empty() {
        eprintln!("fono record --live: streaming STT returned empty text");
        return Ok(());
    }
    println!("{final_text}");
    if !no_inject {
        if let Err(e) = fono_inject::type_text(&final_text) {
            eprintln!("fono record --live: inject failed: {e:#}");
        }
    }
    Ok(())
}

#[cfg(feature = "interactive")]
fn test_overlay_cmd() {
    use std::time::Duration;

    use fono_overlay::{OverlayState, RealOverlay};

    println!("fono test-overlay: spawning real overlay window…");
    let handle = match RealOverlay::spawn(fono_core::config::WaveformStyle::default()) {
        Ok(h) => h,
        Err(e) => {
            println!("test-overlay: overlay failed to spawn: {e:#}");
            return;
        }
    };
    println!("[1/3] Recording (red), 1s");
    handle.set_state(OverlayState::Recording { db: -20 });
    std::thread::sleep(Duration::from_secs(1));
    println!("[2/3] LiveDictating with sample text (blue), 1s");
    handle.set_state(OverlayState::LiveDictating);
    handle.update_text("Hello from fono live mode");
    std::thread::sleep(Duration::from_secs(1));
    println!("[3/3] Processing (amber), 1s");
    handle.set_state(OverlayState::Processing);
    std::thread::sleep(Duration::from_secs(1));
    println!("test-overlay: shutting down");
    handle.shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every directive string produced by `Verbosity::as_filter`
    /// must round-trip through `tracing_subscriber::filter::Targets`
    /// because that is what `init_tracing` parses at startup. This
    /// guards against the directive-format regressions we used to
    /// not catch — most notably `llama-cpp-2=error` (hyphenated
    /// target name) and the bare-level default token at either end
    /// of the comma-separated list.
    /// Regression: switching the TTS backend used to leave
    /// `cfg.tts.voice` untouched, so a Cartesia UUID or an OpenAI
    /// "alloy" leaked into Groq and produced a 400 ("voice must be one
    /// of [autumn diana hannah …]"). The switch now resets the voice
    /// to the new backend's catalogue default (or empty for Wyoming /
    /// None) so each provider gets a voice it actually accepts.
    #[test]
    fn set_active_tts_resets_voice_to_new_backend_default() {
        use fono_core::config::TtsBackend;

        // Start on Cartesia with its default UUID voice.
        let mut cfg = Config::default();
        set_active_tts(&mut cfg, TtsBackend::Cartesia, None);
        let cartesia_voice = cfg.tts.voice.clone();
        assert!(!cartesia_voice.is_empty(), "Cartesia catalogue must define a default voice");

        // Switching to Groq must drop the Cartesia UUID and pick up a
        // Groq-valid voice from the catalogue.
        set_active_tts(&mut cfg, TtsBackend::Groq, None);
        assert_ne!(cfg.tts.voice, cartesia_voice, "voice from previous provider must not leak");
        assert!(!cfg.tts.voice.is_empty(), "Groq catalogue must define a default voice");

        // Switching to Wyoming has no catalogue entry — clear so the
        // server picks its own default.
        set_active_tts(&mut cfg, TtsBackend::Wyoming, None);
        assert!(cfg.tts.voice.is_empty(), "Wyoming switch must clear stale cloud voice");
    }

    #[test]
    fn verbosity_filters_parse_as_targets() {
        use tracing_subscriber::filter::Targets;
        for v in [Verbosity::Quiet, Verbosity::Info, Verbosity::Debug, Verbosity::Trace] {
            let s = v.as_filter();
            s.parse::<Targets>()
                .unwrap_or_else(|e| panic!("Verbosity::{v:?} filter {s:?} failed to parse: {e}"));
        }
    }

    /// `fono summarize` raw-text mode: all field flags land in
    /// the right payload slots and `--json` stays off by default.
    #[test]
    fn summarize_parses_raw_mode_flags() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from([
            "fono",
            "summarize",
            "--sender",
            "Mihai",
            "--chat",
            "Backend Alerts",
            "--source",
            "chat-cli",
            "--instructions",
            "Mention urgency.",
            "--voice",
            "alloy",
            "--silent",
        ])
        .expect("raw-mode flags must parse");
        match cli.cmd {
            Some(Cmd::Summarize { json, sender, chat, source, instructions, voice, silent }) => {
                assert!(!json);
                assert!(silent);
                assert_eq!(sender.as_deref(), Some("Mihai"));
                assert_eq!(chat.as_deref(), Some("Backend Alerts"));
                assert_eq!(source.as_deref(), Some("chat-cli"));
                assert_eq!(instructions.as_deref(), Some("Mention urgency."));
                assert_eq!(voice.as_deref(), Some("alloy"));
            }
            other => panic!("expected Summarize, got {other:?}"),
        }
    }

    /// `fono summarize --json`: JSON mode parses with no field
    /// flags and all overrides default to off/None.
    #[test]
    fn summarize_parses_json_mode() {
        use clap::Parser as _;
        let cli =
            Cli::try_parse_from(["fono", "summarize", "--json"]).expect("json mode must parse");
        match cli.cmd {
            Some(Cmd::Summarize { json, sender, chat, source, instructions, voice, silent }) => {
                assert!(json);
                assert!(!silent);
                assert!(sender.is_none());
                assert!(chat.is_none());
                assert!(source.is_none());
                assert!(instructions.is_none());
                assert!(voice.is_none());
            }
            other => panic!("expected Summarize, got {other:?}"),
        }
    }
}
