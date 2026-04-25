// SPDX-License-Identifier: GPL-3.0-only
//! `fono doctor` — diagnostic report.

use std::fmt::Write;

use anyhow::Result;
use fono_core::hwcheck;
use fono_core::{Config, Paths, Secrets};

pub async fn report(paths: &Paths) -> Result<String> {
    #![allow(clippy::too_many_lines)]
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
        match fono_stt::build_stt(&c.stt, &secrets, &paths.whisper_models_dir()) {
            Ok(s) => writeln!(out, "  stt: {} ready", s.name())?,
            Err(e) => writeln!(out, "  stt: FAIL — {e:#}")?,
        }
        match fono_llm::build_llm(&c.llm, &secrets) {
            Ok(Some(l)) => writeln!(out, "  llm: {} ready", l.name())?,
            Ok(None) => writeln!(out, "  llm: disabled (cleanup off)")?,
            Err(e) => writeln!(out, "  llm: FAIL — {e:#}")?,
        }
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
    writeln!(
        out,
        "Injector    : {:?}",
        fono_inject::inject::Injector::detect()
    )?;
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
