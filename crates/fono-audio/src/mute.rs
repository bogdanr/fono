// SPDX-License-Identifier: GPL-3.0-only
//! Auto-mute system sinks while recording. Detects PulseAudio vs PipeWire.

use std::process::Command;

use tracing::{debug, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioStack {
    PipeWire,
    PulseAudio,
    Unknown,
}

/// Probe which control utility is available.
#[must_use]
pub fn detect() -> AudioStack {
    if which("wpctl").is_some() {
        return AudioStack::PipeWire;
    }
    if which("pactl").is_some() {
        // `pactl info` mentions "PipeWire" when the Pulse compat layer is
        // backed by PipeWire.
        if let Ok(out) = Command::new("pactl").arg("info").output() {
            let s = String::from_utf8_lossy(&out.stdout);
            if s.contains("PipeWire") {
                return AudioStack::PipeWire;
            }
        }
        return AudioStack::PulseAudio;
    }
    AudioStack::Unknown
}

/// Mute / unmute the default sink. Best-effort: logs a warning if the tool
/// isn't available and returns without erroring (dictation still works).
pub fn set_default_sink_mute(muted: bool) {
    let flag = if muted { "1" } else { "0" };
    match detect() {
        AudioStack::PipeWire => {
            run("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", flag]);
        }
        AudioStack::PulseAudio => {
            run("pactl", &["set-sink-mute", "@DEFAULT_SINK@", flag]);
        }
        AudioStack::Unknown => {
            debug!("no known audio-control tool; skipping auto-mute");
        }
    }
}

fn run(cmd: &str, args: &[&str]) {
    match Command::new(cmd).args(args).output() {
        Ok(o) if o.status.success() => {}
        Ok(o) => warn!(
            "{cmd} {:?} failed: {}",
            args,
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => warn!("spawning {cmd} failed: {e}"),
    }
}

fn which(tool: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for p in std::env::split_paths(&path) {
        let candidate = p.join(tool);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
