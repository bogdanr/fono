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

/// Probe and warm the injection backend at daemon startup so the first
/// dictation doesn't pay the binary's cold page-cache cost. Returns the
/// detected backend's name. Latency plan task L5.
pub fn warm_backend() -> Result<&'static str> {
    let inj = Injector::detect();
    match inj {
        #[cfg(feature = "enigo-backend")]
        Injector::Enigo => {
            // Constructing Enigo touches the X server / libei state;
            // doing it once at startup cuts ~5–20 ms off first inject.
            use enigo::{Enigo, Settings};
            let _ = Enigo::new(&Settings::default())
                .map_err(|e| anyhow!("enigo prewarm failed: {e}"))?;
            Ok("enigo")
        }
        Injector::Wtype => {
            // `wtype --version` is a 1–2 ms exec that page-caches the
            // binary so the real inject doesn't pay the disk fault.
            let _ = Command::new("wtype")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .status();
            Ok("wtype")
        }
        Injector::Ydotool => {
            let _ = Command::new("ydotool")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .status();
            Ok("ydotool")
        }
        Injector::None => Err(anyhow!(
            "no text-injection backend available — install `wtype`/`ydotool` \
             on Wayland, or enable the enigo-backend feature on X11"
        )),
    }
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
