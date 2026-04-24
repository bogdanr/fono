// SPDX-License-Identifier: GPL-3.0-only
//! `fono doctor` — diagnostic report.

use std::fmt::Write;

use anyhow::Result;
use fono_core::{Config, Paths};

pub async fn report(paths: &Paths) -> Result<String> {
    let mut out = String::new();
    writeln!(out, "Fono doctor — v{}", env!("CARGO_PKG_VERSION"))?;
    writeln!(out, "")?;

    writeln!(out, "Paths:")?;
    writeln!(out, "  config : {}", paths.config_file().display())?;
    writeln!(out, "  data   : {}", paths.data_dir.display())?;
    writeln!(out, "  cache  : {}", paths.cache_dir.display())?;
    writeln!(out, "  state  : {}", paths.state_dir.display())?;
    writeln!(out, "")?;

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
    if config_exists {
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
            }
            Err(e) => writeln!(out, "  FAILED TO LOAD: {e}")?,
        }
    }
    writeln!(out, "")?;

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
    writeln!(out, "")?;

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
