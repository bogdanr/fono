// SPDX-License-Identifier: GPL-3.0-only
//! Text injection backends.

use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};
use tracing::{debug, warn};

/// Chosen backend for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Injector {
    #[cfg(feature = "enigo-backend")]
    Enigo,
    Wtype,
    Ydotool,
    /// No working injection available — caller should fall back to clipboard.
    None,
}

impl Injector {
    /// Pick the best available injector for the current session.
    pub fn detect() -> Self {
        let wayland = std::env::var("XDG_SESSION_TYPE")
            .map(|v| v == "wayland")
            .unwrap_or(false)
            || std::env::var("WAYLAND_DISPLAY").is_ok();

        #[cfg(feature = "enigo-backend")]
        if !wayland {
            return Self::Enigo;
        }

        if which("wtype").is_some() {
            return Self::Wtype;
        }
        if which("ydotool").is_some() {
            return Self::Ydotool;
        }
        #[cfg(feature = "enigo-backend")]
        {
            // Last resort on Wayland: enigo can sometimes work via libei.
            return Self::Enigo;
        }
        let _ = wayland;
        Self::None
    }

    pub fn inject(self, text: &str) -> Result<()> {
        match self {
            #[cfg(feature = "enigo-backend")]
            Self::Enigo => inject_enigo(text),
            Self::Wtype => inject_subprocess("wtype", &[text]),
            Self::Ydotool => inject_subprocess("ydotool", &["type", text]),
            Self::None => Err(anyhow!(
                "no text-injection backend available; install wtype/ydotool \
                 or enable the enigo-backend feature"
            )),
        }
    }
}

/// Convenience: detect and inject in one call.
pub fn type_text(text: &str) -> Result<()> {
    let inj = Injector::detect();
    debug!("type_text via {inj:?}: {} bytes", text.len());
    inj.inject(text)
}

fn inject_subprocess(cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| anyhow!("spawning {cmd} failed: {e}"))?;
    if !status.success() {
        warn!("{cmd} exited with {status}");
    }
    Ok(())
}

#[cfg(feature = "enigo-backend")]
fn inject_enigo(text: &str) -> Result<()> {
    use enigo::{Enigo, Keyboard, Settings};
    let mut en = Enigo::new(&Settings::default()).map_err(|e| anyhow!("enigo init failed: {e}"))?;
    en.text(text)
        .map_err(|e| anyhow!("enigo text failed: {e}"))?;
    Ok(())
}

fn which(tool: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for p in std::env::split_paths(&path) {
        let c = p.join(tool);
        if c.is_file() {
            return Some(c);
        }
    }
    None
}
