// SPDX-License-Identifier: GPL-3.0-only
//! PulseAudio (and PipeWire-via-Pulse-compat) microphone enumeration via
//! `pactl`.
//!
//! Role in the dispatch hierarchy:
//!
//! * On the **`PulseAudio`** audio stack this module is the native
//!   path — `pactl` talks directly to the PulseAudio daemon.
//! * On the **`PipeWire`** audio stack this module is the
//!   **fallback** behind [`crate::wpctl`]: `wpctl` is the native
//!   WirePlumber control surface and is used first; `pactl` only
//!   engages when `wpctl` is missing from `PATH` or its output is
//!   unparseable (e.g. setups without `wireplumber`). Stock Ubuntu
//!   24.04 installs ship `wireplumber` (so `wpctl`) but not
//!   `pulseaudio-utils` (so no `pactl`), which is the case this
//!   ordering fixes.
//!
//! The parse-and-delegate model is otherwise unchanged: the audio
//! server is the authority on what's a real source vs. a sink
//! monitor (we drop `.monitor` names), it exposes friendly
//! `Description:` strings, and selection is delegated back via
//! `pactl set-default-source`. On the PipeWire branch
//! [`set_default_pulse_source`] auto-detects numeric node ids
//! (produced by [`crate::wpctl::list_wpctl_sources`]) and dispatches
//! to `wpctl set-default` instead.
//!
//! All helpers degrade gracefully: missing `pactl` on `PATH`, a spawn
//! failure, or a non-zero exit collapse to `None` / empty / a clear
//! `anyhow::Error`. None panic.

use std::process::Command;

use crate::devices::{InputBackend, InputDevice};

/// Enumerate PulseAudio / PipeWire input sources via `pactl`. Returns
/// an empty vector when `pactl` is unavailable so the caller can
/// dispatch to the cpal fallback. `.monitor` sources (sink loopbacks)
/// are filtered out — those are never microphones.
#[must_use]
pub fn list_pulse_sources() -> Vec<InputDevice> {
    let Some(short) = run_pactl(&["list", "sources", "short"]) else {
        return Vec::new();
    };
    let long = run_pactl(&["list", "sources"]).unwrap_or_default();
    let default_name = pulse_default_source_name().unwrap_or_default();
    let descriptions = parse_descriptions(&long);
    parse_sources_short(&short)
        .into_iter()
        .map(|name| {
            let display_name = descriptions
                .iter()
                .find(|(n, _)| n == &name)
                .map(|(_, d)| d.clone())
                .unwrap_or_else(|| name.clone());
            let is_default = !default_name.is_empty() && name == default_name;
            InputDevice { display_name, is_default, backend: InputBackend::Pulse { pa_name: name } }
        })
        .collect()
}

/// The audio server's currently-configured default source name (the
/// alpha-2-style PA name, not the friendly description). Returns
/// `None` if `pactl` is unavailable or the command failed.
#[must_use]
pub fn pulse_default_source_name() -> Option<String> {
    let raw = run_pactl(&["get-default-source"])?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Tell the audio server to use `name` as the default source.
///
/// Dispatch:
///
/// * An all-ASCII-digits `name` is a WirePlumber node id (produced
///   by [`crate::wpctl::list_wpctl_sources`] on the PipeWire branch)
///   and is forwarded to [`crate::wpctl::set_default_wpctl_source`].
/// * Anything else is treated as a PA source `Name:` and forwarded
///   to `pactl set-default-source`.
///
/// Errors when the chosen backend is missing or returns non-zero
/// (typically: stale identifier after a hot-unplug).
pub fn set_default_pulse_source(name: &str) -> anyhow::Result<()> {
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit()) {
        return crate::wpctl::set_default_wpctl_source(name);
    }
    let out = Command::new("pactl")
        .args(["set-default-source", name])
        .output()
        .map_err(|e| anyhow::anyhow!("spawn pactl set-default-source: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        anyhow::bail!(
            "pactl set-default-source {name:?} failed: {}",
            if stderr.is_empty() { format!("exit {}", out.status) } else { stderr }
        );
    }
    Ok(())
}

fn run_pactl(args: &[&str]) -> Option<String> {
    let out = Command::new("pactl").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `pactl list sources short` into a list of source names,
/// skipping `.monitor` sink-loopbacks and malformed lines. The short
/// format is tab-separated: `<id>\t<name>\t<driver>\t<spec>\t<state>`.
fn parse_sources_short(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let _id = parts.next();
        let Some(name) = parts.next() else { continue };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        if name.ends_with(".monitor") {
            continue;
        }
        out.push(name.to_string());
    }
    out
}

/// Parse the long-form `pactl list sources` output, mapping each
/// source's `Name:` to its `Description:` so callers can render
/// friendly labels.
fn parse_descriptions(raw: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current_name: Option<String> = None;
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Name:") {
            current_name = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("Description:") {
            if let Some(name) = current_name.take() {
                out.push((name, rest.trim().to_string()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sources_short_zero_lines_yields_empty() {
        assert!(parse_sources_short("").is_empty());
        assert!(parse_sources_short("\n\n").is_empty());
    }

    #[test]
    fn parse_sources_short_single_mic() {
        let raw =
            "0\talsa_input.usb-Logitech_BRIO\tmodule-alsa-card.c\ts16le 2ch 48000Hz\tSUSPENDED\n";
        assert_eq!(parse_sources_short(raw), vec!["alsa_input.usb-Logitech_BRIO"]);
    }

    #[test]
    fn parse_sources_short_two_with_one_monitor() {
        let raw = "\
0\talsa_input.pci-0000_00_1f.3.analog-stereo\tdriver\tspec\tIDLE
1\talsa_output.pci-0000_00_1f.3.analog-stereo.monitor\tdriver\tspec\tIDLE
2\talsa_input.usb-Logitech_BRIO\tdriver\tspec\tSUSPENDED
";
        assert_eq!(
            parse_sources_short(raw),
            vec!["alsa_input.pci-0000_00_1f.3.analog-stereo", "alsa_input.usb-Logitech_BRIO",]
        );
    }

    #[test]
    fn parse_sources_short_skips_only_monitor() {
        let raw = "0\talsa_output.foo.monitor\tdrv\tspec\tIDLE\n";
        assert!(parse_sources_short(raw).is_empty());
    }

    #[test]
    fn parse_sources_short_tolerates_whitespace_and_carriage_returns() {
        let raw = "  \r\n0\tname.with.dots\tdrv\tspec\tIDLE\r\n\n";
        assert_eq!(parse_sources_short(raw), vec!["name.with.dots"]);
    }

    #[test]
    fn parse_sources_short_skips_malformed_lines() {
        let raw = "not_tab_separated_at_all\n0\t\tdrv\tspec\tIDLE\n1\tgood_name\tdrv\tspec\tIDLE\n";
        // Line 1: only one field => no name => skip.
        // Line 2: empty name => skip.
        // Line 3: kept.
        assert_eq!(parse_sources_short(raw), vec!["good_name"]);
    }

    #[test]
    fn parse_descriptions_pairs_name_with_description() {
        let raw = "\
Source #0
\tState: SUSPENDED
\tName: alsa_input.usb-Logitech_BRIO
\tDescription: Logitech BRIO Mono
\tDriver: PipeWire
Source #1
\tName: alsa_input.pci-0000_00_1f.3.analog-stereo
\tDescription: Built-in Audio Analog Stereo
";
        let map = parse_descriptions(raw);
        assert_eq!(
            map,
            vec![
                ("alsa_input.usb-Logitech_BRIO".to_string(), "Logitech BRIO Mono".to_string()),
                (
                    "alsa_input.pci-0000_00_1f.3.analog-stereo".to_string(),
                    "Built-in Audio Analog Stereo".to_string(),
                ),
            ]
        );
    }

    /// `list_pulse_sources` is intentionally infallible. On a host
    /// without `pactl` (or a CI sandbox where the command fails), it
    /// must collapse to an empty vec rather than panic so the caller
    /// can dispatch to the cpal fallback.
    #[test]
    fn list_pulse_sources_does_not_panic() {
        let _ = list_pulse_sources();
    }
}
