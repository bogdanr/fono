// SPDX-License-Identifier: GPL-3.0-only
//! `fono doctor` — diagnostic report.

use std::fmt::Write;
use std::io::IsTerminal;
use std::sync::OnceLock;

use anyhow::Result;
use fono_core::hwcheck;
use fono_core::{Config, Paths, Secrets};

/// Whether to emit ANSI color escapes in the report. True iff stdout
/// is a TTY and `NO_COLOR` is unset. Cached on first call.
fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}

fn paint(code: &str, s: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
#[rustfmt::skip] fn ok(s: &str) -> String { paint("32", s) } // green
#[rustfmt::skip] fn bad(s: &str) -> String { paint("31;1", s) } // bold red
#[rustfmt::skip] fn warn(s: &str) -> String { paint("33", s) } // yellow
#[rustfmt::skip] fn dim(s: &str) -> String { paint("2", s) } // dim
#[rustfmt::skip] fn head(s: &str) -> String { paint("1;36", s) } // bold cyan
#[rustfmt::skip]
fn star(active: bool) -> String {
    if active { paint("1;36", "*") } else { " ".into() }
}

#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
pub async fn report(paths: &Paths) -> Result<String> {
    let mut out = String::new();
    let variant = crate::variant::VARIANT;
    writeln!(
        out,
        "{} v{} ({} variant — {})",
        head("Fono doctor —"),
        env!("CARGO_PKG_VERSION"),
        variant.label(),
        variant.description(),
    )?;
    writeln!(out)?;

    // ----------------------------------------------------------------
    // Hardware probe + tier (drives wizard recommendations + helps
    // diagnose "why is local STT slow on my machine?")
    // ----------------------------------------------------------------
    let snap = hwcheck::probe(&paths.cache_dir);
    let tier = snap.tier();
    let ram_gb = snap.total_ram_bytes / (1024 * 1024 * 1024);
    let disk_gb = snap.free_disk_bytes / (1024 * 1024 * 1024);
    let isa = if snap.cpu_features.avx2 {
        "AVX2"
    } else if snap.cpu_features.neon {
        "NEON"
    } else {
        "no-vec"
    };
    writeln!(out, "{}", head("Hardware:"))?;
    writeln!(
        out,
        "  cores : {} physical / {} logical  ({isa})",
        snap.physical_cores, snap.logical_cores
    )?;
    writeln!(
        out,
        "  ram   : {ram_gb} GB total · disk free : {disk_gb} GB · arch : {}/{}",
        snap.os, snap.arch
    )?;
    writeln!(
        out,
        "  local-tier : {} (recommends whisper-{})",
        tier.as_str(),
        tier.default_whisper_model()
    )?;
    if let Err(reason) = snap.suitability() {
        writeln!(out, "  {} {reason}", bad("unsuitable because:"))?;
    }
    writeln!(out)?;

    // Compute backends — what's compiled into this variant + what the
    // host's Vulkan loader reports. Slice 2 of
    // `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`.
    {
        use crate::variant::{Variant, VARIANT};
        use fono_core::vulkan_probe::probe;
        writeln!(out, "{}", head("Compute backends:"))?;
        writeln!(out, "  variant  : {} ({})", VARIANT.label(), VARIANT.description())?;
        let outcome = probe();
        writeln!(out, "  vulkan   : {}", outcome.summary_line())?;
        if matches!(VARIANT, Variant::Cpu) && outcome.is_usable() {
            writeln!(
                out,
                "  hint     : your machine has Vulkan-capable GPU(s); the GPU release \
                 variant runs inference faster on this hardware. Download \
                 `fono-gpu-vX.Y.Z-x86_64` from the Releases page (or upgrade in-place \
                 once `fono update --variant gpu` lands)."
            )?;
        }
        writeln!(out)?;
    }

    writeln!(out, "{}", head("Paths:"))?;
    writeln!(out, "  config : {}", paths.config_file().display())?;
    writeln!(out, "  data   : {}", paths.data_dir.display())?;
    writeln!(out, "  cache  : {}", paths.cache_dir.display())?;
    writeln!(out, "  state  : {}", paths.state_dir.display())?;
    writeln!(out)?;

    writeln!(out, "{} {}", head("Install:"), crate::install::doctor_state())?;
    writeln!(out)?;

    let config_exists = paths.config_file().exists();
    writeln!(
        out,
        "{} {}",
        head("Config :"),
        if config_exists { ok("present") } else { bad("MISSING (run `fono setup`)") }
    )?;
    let cfg = if config_exists {
        match Config::load(&paths.config_file()) {
            Ok(c) => {
                writeln!(out, "  version        : {}", c.version)?;
                writeln!(out, "  stt.backend    : {:?}", c.stt.backend)?;
                writeln!(out, "  stt.local.model: {}", c.stt.local.model)?;
                writeln!(out, "  polish.backend    : {:?}", c.polish.backend)?;
                writeln!(out, "  polish.local.model: {}", c.polish.local.model)?;
                writeln!(
                    out,
                    "  hotkeys        : dictation={} assistant={} (short=toggle, long=hold)",
                    c.hotkeys.dictation, c.hotkeys.assistant,
                )?;
                Some(c)
            }
            Err(e) => {
                writeln!(out, "  {} {e}", bad("FAILED TO LOAD:"))?;
                None
            }
        }
    } else {
        None
    };
    writeln!(out)?;

    // ----------------------------------------------------------------
    // Backend factories — if the user picked a cloud backend, exercise
    // the factory so they see a clear "API key missing" or "feature
    // missing" message right here rather than having to start the
    // daemon and read the log.
    // ----------------------------------------------------------------
    let secrets = Secrets::load(&paths.secrets_file()).unwrap_or_default();
    if let Some(c) = cfg.as_ref() {
        writeln!(out, "{}", head("Backends:"))?;
        match fono_stt::build_stt(&c.stt, &c.general, &secrets, &paths.whisper_models_dir()) {
            Ok(s) => writeln!(out, "  stt: {} {}", s.name(), ok("ready"))?,
            Err(e) => writeln!(out, "  stt: {} {e:#}", bad("FAIL —"))?,
        }
        match fono_polish::build_polish(&c.polish, &secrets, &paths.polish_models_dir()) {
            Ok(Some(l)) => writeln!(out, "  polish: {} {}", l.name(), ok("ready"))?,
            Ok(None) => writeln!(out, "  polish: {}", dim("disabled (cleanup off)"))?,
            Err(e) => writeln!(out, "  polish: {} {e:#}", bad("FAIL —"))?,
        }
        match fono_assistant::build_assistant(&c.assistant, &secrets) {
            Ok(Some(a)) => writeln!(out, "  assistant: {} {}", a.name(), ok("ready"))?,
            Ok(None) => writeln!(out, "  assistant: {}", dim("disabled"))?,
            Err(e) => writeln!(out, "  assistant: {} {e:#}", bad("FAIL —"))?,
        }
        match fono_tts::build_tts(&c.tts, &secrets) {
            Ok(Some(t)) => writeln!(out, "  tts: {} {}", t.name(), ok("ready"))?,
            Ok(None) => {
                writeln!(out, "  tts: {}", warn("disabled (assistant replies will be silent)"))?;
            }
            Err(e) => writeln!(out, "  tts: {} {e:#}", bad("FAIL —"))?,
        }
        writeln!(out)?;

        // ------------------------------------------------------------
        // Per-provider key + reachability matrix (provider-switching
        // plan task S18). One line per known backend with active marker
        // so users see at a glance which providers are ready to switch
        // to via `fono use stt …` / `fono use polish …`.
        // ------------------------------------------------------------
        writeln!(out, "{}", head("Providers (STT):"))?;
        for b in fono_core::providers::all_stt_backends() {
            let active = b == c.stt.backend;
            let mark = star(active);
            let name = fono_core::providers::stt_backend_str(&b);
            let needs_key = fono_core::providers::stt_requires_key(&b);
            let key_env = fono_core::providers::stt_key_env(&b);
            let key_status = if !needs_key {
                dim("no key needed")
            } else if secrets.resolve(key_env).is_some() {
                ok(&format!("{key_env} present"))
            } else {
                dim(&format!("{key_env} missing"))
            };
            let model = if needs_key {
                fono_stt::defaults::default_cloud_model(name).to_string()
            } else {
                c.stt.local.model.clone()
            };
            writeln!(out, "  {mark} {name:<14} model: {model:<32} {key_status}")?;
        }
        writeln!(out)?;

        writeln!(out, "{}", head("Providers (LLM):"))?;
        for b in fono_core::providers::all_polish_backends() {
            let active = b == c.polish.backend;
            let mark = star(active);
            let name = fono_core::providers::polish_backend_str(&b);
            let needs_key = fono_core::providers::polish_requires_key(&b);
            let key_env = fono_core::providers::polish_key_env(&b);
            let key_status = if !needs_key {
                dim("no key needed")
            } else if secrets.resolve(key_env).is_some() {
                ok(&format!("{key_env} present"))
            } else {
                dim(&format!("{key_env} missing"))
            };
            let model = if matches!(b, fono_core::config::PolishBackend::None) {
                "—".to_string()
            } else if needs_key || matches!(b, fono_core::config::PolishBackend::Ollama) {
                fono_polish::defaults::default_cloud_model(name).to_string()
            } else {
                c.polish.local.model.clone()
            };
            writeln!(out, "  {mark} {name:<14} model: {model:<32} {key_status}")?;
        }
        writeln!(out)?;

        writeln!(out, "{}", head("Providers (assistant):"))?;
        for b in fono_core::providers::all_assistant_backends() {
            let active = b == c.assistant.backend;
            let mark = star(active);
            let name = fono_core::providers::assistant_backend_str(&b);
            let needs_key = fono_core::providers::assistant_requires_key(&b);
            let key_env = fono_core::providers::assistant_key_env(&b);
            let key_status = if !needs_key {
                dim("no key needed")
            } else if secrets.resolve(key_env).is_some() {
                ok(&format!("{key_env} present"))
            } else {
                dim(&format!("{key_env} missing"))
            };
            writeln!(out, "  {mark} {name:<14} {key_status}")?;
        }
        writeln!(out)?;

        writeln!(out, "{}", head("Providers (TTS):"))?;
        for b in fono_core::providers::all_tts_backends() {
            let active = b == c.tts.backend;
            let mark = star(active);
            let name = fono_core::providers::tts_backend_str(&b);
            let needs_key = fono_core::providers::tts_requires_key(&b);
            let key_env = fono_core::providers::tts_key_env(&b);
            let extra = match b {
                fono_core::config::TtsBackend::Wyoming => c
                    .tts
                    .wyoming
                    .as_ref()
                    .map(|w| format!("uri={}", w.uri))
                    .unwrap_or_else(|| dim("uri=(unset)")),
                fono_core::config::TtsBackend::OpenAI
                | fono_core::config::TtsBackend::Groq
                | fono_core::config::TtsBackend::OpenRouter
                | fono_core::config::TtsBackend::Cartesia
                | fono_core::config::TtsBackend::Deepgram => {
                    if secrets.resolve(key_env).is_some() {
                        ok(&format!("{key_env} present"))
                    } else {
                        dim(&format!("{key_env} missing"))
                    }
                }
                fono_core::config::TtsBackend::None => dim("—"),
            };
            let _ = needs_key;
            writeln!(out, "  {mark} {name:<14} {extra}")?;
        }
        writeln!(out)?;
        writeln!(out, "(* = active. Switch with `fono use stt|polish|assistant|tts <backend>`.)")?;
        writeln!(out)?;
    }

    writeln!(out, "{}", head("Session:"))?;
    writeln!(
        out,
        "  XDG_SESSION_TYPE : {}",
        std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "(unset)".into())
    )?;
    writeln!(
        out,
        "  WAYLAND_DISPLAY  : {}",
        std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "(unset)".into())
    )?;
    writeln!(
        out,
        "  DISPLAY          : {}",
        std::env::var("DISPLAY").unwrap_or_else(|_| "(unset)".into())
    )?;
    writeln!(out)?;

    writeln!(out, "{} {:?}", head("Audio stack :"), fono_audio::mute::detect())?;
    // Input device matrix: list every device the active stack
    // (PulseAudio / PipeWire via pactl, or cpal as fallback) reports,
    // marking whichever the OS currently considers default. Fono no
    // longer keeps an `[audio].input_device` override; microphone
    // selection is delegated to the OS layer (pavucontrol / GNOME /
    // KDE settings on Linux, Sound preferences on macOS / Windows).
    let devices = fono_audio::devices::list_input_devices();
    writeln!(out, "{}", head("Audio inputs:"))?;
    if devices.is_empty() {
        writeln!(
            out,
            "  {}",
            bad("(no input devices reported — check pactl / cpal permissions, or that your microphone is plugged in)")
        )?;
    } else {
        for d in &devices {
            let mark = star(d.is_default);
            writeln!(out, "  {mark} {}", d.display_name)?;
        }
        writeln!(
            out,
            "(* = system default. Change via the tray Microphone submenu, \
             pavucontrol, or your OS sound settings.)"
        )?;
    }
    let injector = fono_inject::inject::Injector::detect();
    writeln!(out, "{} {injector:?}", head("Injector    :"))?;
    // Show the configured XTEST paste shortcut so users can confirm
    // it before reporting "doesn't paste in app X".
    let shortcut_label = fono_inject::PasteShortcut::from_env_or_default().label();
    let cfg_value = cfg
        .as_ref()
        .map(|c| c.inject.paste_shortcut.clone())
        .unwrap_or_else(|| "shift-insert".into());
    let env_value = std::env::var("FONO_PASTE_SHORTCUT").ok();
    writeln!(
        out,
        "{} {shortcut_label} (config={cfg_value:?} env={env_value:?})",
        head("Paste keys  :"),
    )?;
    // Clipboard fallback — fono copies the cleaned text here when no
    // key-injection backend works, so the dictation is never lost.
    let mut clip_tools = Vec::new();
    for t in ["wl-copy", "xclip", "xsel"] {
        if which_in_path(t) {
            clip_tools.push(t);
        }
    }
    if clip_tools.is_empty() {
        writeln!(
            out,
            "{} {}",
            head("Clipboard   :"),
            bad("NONE (install one of: wl-clipboard, xclip, xsel — without these, dictation cannot be recovered when key injection fails)")
        )?;
    } else {
        writeln!(out, "{} {} {}", head("Clipboard   :"), clip_tools.join(", "), dim("(fallback)"))?;
    }
    writeln!(
        out,
        "{} {} ({})",
        head("IPC socket  :"),
        paths.ipc_socket().display(),
        if paths.ipc_socket().exists() { ok("exists") } else { warn("absent") }
    )?;

    writeln!(out)?;
    writeln!(out, "{} ({}):", head("Log tail"), paths.log_file().display())?;
    match tail_log(&paths.log_file(), 10) {
        Ok(lines) if lines.is_empty() => writeln!(out, "  {}", dim("(log is empty)"))?,
        Ok(lines) => {
            for line in lines {
                writeln!(out, "  {line}")?;
            }
        }
        Err(e) => writeln!(out, "  {} {e}", bad("(cannot read log:"))?,
    }

    Ok(out)
}

/// Read up to the last `n` lines of `path`. Preserves embedded ANSI.
fn tail_log(path: &std::path::Path, n: usize) -> std::io::Result<Vec<String>> {
    let data = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = data.lines().collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].iter().map(|s| (*s).to_string()).collect())
}

/// `fono doctor -f`: stream the log file forever via `tail -f`.
/// ANSI escapes pass through to the terminal unchanged.
pub async fn follow_log(paths: &Paths) -> Result<()> {
    let path = paths.log_file();
    println!();
    println!("Following {} (Ctrl-C to stop):", path.display());
    let status = tokio::process::Command::new("tail")
        .arg("-n")
        .arg("0")
        .arg("-F")
        .arg(&path)
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("tail exited with {status}");
    }
    Ok(())
}

/// Best-effort PATH lookup; mirrors fono-inject's `which` so doctor
/// reports the same set of clipboard tools the real fallback will try.
fn which_in_path(tool: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|p| p.join(tool).is_file())
}
