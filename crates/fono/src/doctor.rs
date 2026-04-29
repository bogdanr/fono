// SPDX-License-Identifier: GPL-3.0-only
//! `fono doctor` — diagnostic report.

use std::fmt::Write;

use anyhow::Result;
use fono_core::hwcheck;
use fono_core::{Config, Paths, Secrets};

#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
pub async fn report(paths: &Paths) -> Result<String> {
    let mut out = String::new();
    writeln!(out, "Fono doctor — v{}", env!("CARGO_PKG_VERSION"))?;
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
    writeln!(out, "Hardware:")?;
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
        writeln!(out, "  unsuitable because: {reason}")?;
    }
    writeln!(out)?;

    writeln!(out, "Paths:")?;
    writeln!(out, "  config : {}", paths.config_file().display())?;
    writeln!(out, "  data   : {}", paths.data_dir.display())?;
    writeln!(out, "  cache  : {}", paths.cache_dir.display())?;
    writeln!(out, "  state  : {}", paths.state_dir.display())?;
    writeln!(out)?;

    let config_exists = paths.config_file().exists();
    writeln!(
        out,
        "Config : {}",
        if config_exists {
            "present"
        } else {
            "MISSING (run `fono setup`)"
        }
    )?;
    let cfg = if config_exists {
        match Config::load(&paths.config_file()) {
            Ok(c) => {
                writeln!(out, "  version        : {}", c.version)?;
                writeln!(out, "  stt.backend    : {:?}", c.stt.backend)?;
                writeln!(out, "  stt.local.model: {}", c.stt.local.model)?;
                writeln!(out, "  llm.backend    : {:?}", c.llm.backend)?;
                writeln!(out, "  llm.local.model: {}", c.llm.local.model)?;
                writeln!(
                    out,
                    "  hotkeys        : hold={} toggle={}",
                    c.hotkeys.hold, c.hotkeys.toggle
                )?;
                Some(c)
            }
            Err(e) => {
                writeln!(out, "  FAILED TO LOAD: {e}")?;
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
        writeln!(out, "Backends:")?;
        match fono_stt::build_stt(&c.stt, &c.general, &secrets, &paths.whisper_models_dir()) {
            Ok(s) => writeln!(out, "  stt: {} ready", s.name())?,
            Err(e) => writeln!(out, "  stt: FAIL — {e:#}")?,
        }
        match fono_llm::build_llm(&c.llm, &secrets, &paths.llm_models_dir()) {
            Ok(Some(l)) => writeln!(out, "  llm: {} ready", l.name())?,
            Ok(None) => writeln!(out, "  llm: disabled (cleanup off)")?,
            Err(e) => writeln!(out, "  llm: FAIL — {e:#}")?,
        }
        writeln!(out)?;

        // ------------------------------------------------------------
        // Per-provider key + reachability matrix (provider-switching
        // plan task S18). One line per known backend with active marker
        // so users see at a glance which providers are ready to switch
        // to via `fono use stt …` / `fono use llm …`.
        // ------------------------------------------------------------
        writeln!(out, "Providers (STT):")?;
        for b in fono_core::providers::all_stt_backends() {
            let active = b == c.stt.backend;
            let mark = if active { "*" } else { " " };
            let name = fono_core::providers::stt_backend_str(&b);
            let needs_key = fono_core::providers::stt_requires_key(&b);
            let key_env = fono_core::providers::stt_key_env(&b);
            let key_status = if !needs_key {
                "no key needed".to_string()
            } else if secrets.resolve(key_env).is_some() {
                format!("{key_env} present")
            } else {
                format!("{key_env} MISSING")
            };
            let model = if needs_key {
                fono_stt::defaults::default_cloud_model(name).to_string()
            } else {
                c.stt.local.model.clone()
            };
            writeln!(out, "  {mark} {name:<14} model={model:<32} {key_status}")?;
        }
        writeln!(out)?;

        writeln!(out, "Providers (LLM):")?;
        for b in fono_core::providers::all_llm_backends() {
            let active = b == c.llm.backend;
            let mark = if active { "*" } else { " " };
            let name = fono_core::providers::llm_backend_str(&b);
            let needs_key = fono_core::providers::llm_requires_key(&b);
            let key_env = fono_core::providers::llm_key_env(&b);
            let key_status = if !needs_key {
                "no key needed".to_string()
            } else if secrets.resolve(key_env).is_some() {
                format!("{key_env} present")
            } else {
                format!("{key_env} MISSING")
            };
            let model = if matches!(b, fono_core::config::LlmBackend::None) {
                "—".to_string()
            } else if needs_key || matches!(b, fono_core::config::LlmBackend::Ollama) {
                fono_llm::defaults::default_cloud_model(name).to_string()
            } else {
                c.llm.local.model.clone()
            };
            writeln!(out, "  {mark} {name:<14} model={model:<32} {key_status}")?;
        }
        writeln!(out)?;
        writeln!(
            out,
            "(* = active. Switch with `fono use stt <backend>` / `fono use llm <backend>`.)"
        )?;
        writeln!(out)?;
    }

    writeln!(out, "Session:")?;
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

    writeln!(out, "Audio stack : {:?}", fono_audio::mute::detect())?;
    // Input device matrix: list every device the active stack
    // (PulseAudio / PipeWire via pactl, or cpal as fallback) reports,
    // marking whichever the OS currently considers default. Fono no
    // longer keeps an `[audio].input_device` override; microphone
    // selection is delegated to the OS layer (pavucontrol / GNOME /
    // KDE settings on Linux, Sound preferences on macOS / Windows).
    let devices = fono_audio::devices::list_input_devices();
    writeln!(out, "Audio inputs:")?;
    if devices.is_empty() {
        writeln!(
            out,
            "  (no input devices reported — check pactl / cpal permissions, \
             or that your microphone is plugged in)"
        )?;
    } else {
        for d in &devices {
            let mark = if d.is_default { "*" } else { " " };
            writeln!(out, "  {mark} {}", d.display_name)?;
        }
        writeln!(
            out,
            "(* = system default. Change via the tray Microphone submenu, \
             pavucontrol, or your OS sound settings.)"
        )?;
    }
    let injector = fono_inject::inject::Injector::detect();
    writeln!(out, "Injector    : {injector:?}")?;
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
        "Paste keys  : {shortcut_label} (config={cfg_value:?} env={env_value:?})"
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
            "Clipboard   : NONE (install one of: wl-clipboard, xclip, xsel — \
             without these, dictation cannot be recovered when key injection fails)"
        )?;
    } else {
        writeln!(out, "Clipboard   : {} (fallback)", clip_tools.join(", "))?;
    }
    writeln!(
        out,
        "IPC socket  : {} ({})",
        paths.ipc_socket().display(),
        if paths.ipc_socket().exists() {
            "exists"
        } else {
            "absent"
        }
    )?;

    Ok(out)
}

/// Best-effort PATH lookup; mirrors fono-inject's `which` so doctor
/// reports the same set of clipboard tools the real fallback will try.
fn which_in_path(tool: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|p| p.join(tool).is_file())
}
