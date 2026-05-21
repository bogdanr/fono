// SPDX-License-Identifier: GPL-3.0-only
//! WirePlumber (`wpctl`) microphone enumeration and selection — the
//! native control surface on PipeWire systems.
//!
//! `wpctl` is the primary source of truth on the `PipeWire` audio
//! stack: it talks directly to WirePlumber (the PipeWire session
//! manager) and works whether or not the `pipewire-pulse` compat
//! daemon is loaded. `pactl` is retained as a fallback for the
//! handful of PipeWire setups that don't ship `wireplumber` or where
//! `wpctl` is missing from `PATH`, and remains primary on the
//! legacy `PulseAudio` audio stack.
//!
//! Identifier model: rows produced here store the **numeric
//! WirePlumber node id** (e.g. `"56"`) in
//! [`InputBackend::Pulse::pa_name`]. The selection helper at
//! [`crate::pulse::set_default_pulse_source`] detects all-digit
//! identifiers and dispatches to [`set_default_wpctl_source`], so
//! callers don't need to know which backend produced the row. Rows
//! from `pactl` continue to use the PA `Name:` (e.g.
//! `alsa_input.usb-...`) as before.
//!
//! All helpers degrade gracefully: missing `wpctl` on `PATH`, a
//! spawn failure, or unparseable output collapse to `None` / empty /
//! a clear `anyhow::Error`. None panic.

use std::process::Command;

use crate::devices::{InputBackend, InputDevice};

/// Enumerate PipeWire input sources via `wpctl status`. Returns an
/// empty vector when `wpctl` is unavailable or its output is
/// unparseable, so the caller can fall through to the `pactl` or
/// `cpal` paths.
#[must_use]
pub fn list_wpctl_sources() -> Vec<InputDevice> {
    let Some(raw) = run_wpctl(&["status"]) else {
        return Vec::new();
    };
    parse_sources(&raw)
}

/// Tell WirePlumber to use `id` as the default Audio/Source. `id` is
/// the numeric node id surfaced by `wpctl status` (and stored in
/// [`InputBackend::Pulse::pa_name`] for rows produced by
/// [`list_wpctl_sources`]). Errors when `wpctl` is missing or
/// returns non-zero (typically: stale node id after a hot-unplug).
pub fn set_default_wpctl_source(id: &str) -> anyhow::Result<()> {
    let out = Command::new("wpctl")
        .args(["set-default", id])
        .output()
        .map_err(|e| anyhow::anyhow!("spawn wpctl set-default: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        anyhow::bail!(
            "wpctl set-default {id:?} failed: {}",
            if stderr.is_empty() { format!("exit {}", out.status) } else { stderr }
        );
    }
    Ok(())
}

fn run_wpctl(args: &[&str]) -> Option<String> {
    let out = Command::new("wpctl").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `wpctl status` output, returning the rows under the
/// `Audio > Sources` subsection. Each row produces an
/// [`InputDevice`] whose `pa_name` is the numeric node id.
///
/// Sample input (abbreviated):
///
/// ```text
/// Audio
///  ├─ Sources:
///  │      55. Alder Lake ... Stereo Microphone [vol: 1.00]
///  │  *   56. Alder Lake ... Digital Microphone [vol: 1.00]
///  │
///  ├─ Filters:
/// ```
///
/// The leading `*` marks the system default. Box-drawing characters
/// are stripped before parsing so the row shape is uniform.
fn parse_sources(raw: &str) -> Vec<InputDevice> {
    let mut out = Vec::new();
    let mut in_audio = false;
    let mut in_sources = false;

    for line in raw.lines() {
        let stripped = strip_box_drawing(line);
        let trimmed = stripped.trim();

        // Top-level section headers ("Audio", "Video", "Settings",
        // "Clients", ...) are bare words with no trailing colon and
        // no leading box-drawing characters.
        if !trimmed.is_empty() && !trimmed.contains(':') && !trimmed.starts_with('*') {
            // Heuristic: a bare word in column 0 (before
            // strip_box_drawing the original starts with a non-space
            // ASCII letter) is a top-level header.
            if line.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                in_audio = trimmed == "Audio";
                in_sources = false;
                continue;
            }
        }

        if !in_audio {
            continue;
        }

        // Sub-section header: ends with ':'. "Sources:" enters the
        // Sources block; anything else (Sinks, Filters, Devices,
        // Streams) exits it.
        if trimmed.ends_with(':') {
            in_sources = trimmed == "Sources:";
            continue;
        }

        if !in_sources || trimmed.is_empty() {
            continue;
        }

        // Row shape after strip_box_drawing + trim:
        //   "*   56. Alder Lake ... [vol: 1.00]"   (default)
        //   "55. Alder Lake ... [vol: 1.00]"        (non-default)
        let (is_default, body) =
            trimmed.strip_prefix('*').map_or((false, trimmed), |rest| (true, rest.trim_start()));

        let Some(dot_idx) = body.find('.') else {
            continue;
        };
        let id_str = &body[..dot_idx];
        if id_str.is_empty() || !id_str.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let after = body[dot_idx + 1..].trim_start();

        // Strip the trailing " [vol: ...]" annotation; node names
        // never contain a `[` so the rightmost is always the volume.
        let name = after.rfind(" [").map_or_else(|| after.trim_end(), |i| after[..i].trim_end());
        // Reject rows with no real name — e.g. "56. [vol: 1.00]"
        // collapses to "[vol: 1.00]" after the rfind miss, which is
        // a volume leak, not a microphone.
        if name.is_empty() || name.starts_with('[') {
            continue;
        }
        out.push(InputDevice {
            display_name: name.to_string(),
            is_default,
            backend: InputBackend::Pulse { pa_name: id_str.to_string() },
        });
    }
    out
}

/// Replace Unicode box-drawing characters (U+2500..U+257F) with
/// spaces so the row prefix collapses to plain whitespace and
/// `trim()` removes it cleanly.
fn strip_box_drawing(line: &str) -> String {
    line.chars().map(|c| if ('\u{2500}'..='\u{257F}').contains(&c) { ' ' } else { c }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
PipeWire 'pipewire-0' [1.6.4, foo@bar, cookie:1]
 └─ Clients:
        32. WirePlumber [...]

Audio
 ├─ Devices:
 │      47. Alder Lake PCH-P [alsa]
 │  
 ├─ Sinks:
 │  *   54. Alder Lake PCH-P Speaker [vol: 0.80]
 │  
 ├─ Sources:
 │      55. Alder Lake PCH-P Stereo Microphone [vol: 1.00]
 │  *   56. Alder Lake PCH-P Digital Microphone [vol: 1.00]
 │  
 ├─ Filters:
 │  
 └─ Streams:

Video
 ├─ Sources:
 │      99. Some Camera [vol: 1.00]
 │  

Settings
 └─ Default Configured Devices:
         1. Audio/Source  alsa_input.foo
";

    #[test]
    fn parse_sources_extracts_audio_sources_only() {
        let rows = parse_sources(SAMPLE);
        assert_eq!(rows.len(), 2, "expected 2 audio sources, got {rows:?}");
        assert_eq!(rows[0].display_name, "Alder Lake PCH-P Stereo Microphone");
        assert!(!rows[0].is_default);
        assert_eq!(rows[1].display_name, "Alder Lake PCH-P Digital Microphone");
        assert!(rows[1].is_default);
    }

    #[test]
    fn parse_sources_stores_numeric_node_id_in_pa_name() {
        let rows = parse_sources(SAMPLE);
        let InputBackend::Pulse { pa_name } = &rows[0].backend else {
            panic!("expected Pulse backend, got {:?}", rows[0].backend);
        };
        assert_eq!(pa_name, "55");
        let InputBackend::Pulse { pa_name } = &rows[1].backend else {
            panic!("expected Pulse backend, got {:?}", rows[1].backend);
        };
        assert_eq!(pa_name, "56");
    }

    #[test]
    fn parse_sources_ignores_video_section() {
        let rows = parse_sources(SAMPLE);
        // The Video section has its own "Sources:" sub-header with a
        // bogus camera row at id 99. It must not leak into the
        // audio list.
        assert!(rows.iter().all(|r| r.display_name != "Some Camera"));
        assert!(rows.iter().all(|r| {
            let InputBackend::Pulse { pa_name } = &r.backend else {
                return true;
            };
            pa_name != "99"
        }));
    }

    #[test]
    fn parse_sources_empty_on_garbage() {
        assert!(parse_sources("").is_empty());
        assert!(parse_sources("not a wpctl status output\n").is_empty());
    }

    #[test]
    fn parse_sources_handles_no_default() {
        let raw = "\
Audio
 ├─ Sources:
 │      55. Foo [vol: 1.00]
 │      56. Bar [vol: 1.00]
";
        let rows = parse_sources(raw);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| !r.is_default));
    }

    #[test]
    fn parse_sources_skips_malformed_rows() {
        let raw = "\
Audio
 ├─ Sources:
 │      not_a_number. Foo [vol: 1.00]
 │      55. [vol: 1.00]
 │      56. Real Mic [vol: 1.00]
";
        let rows = parse_sources(raw);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].display_name, "Real Mic");
    }

    /// `list_wpctl_sources` must be infallible: a missing `wpctl`
    /// binary on `PATH` collapses to an empty vec so the caller can
    /// dispatch to the `pactl` fallback.
    #[test]
    fn list_wpctl_sources_does_not_panic() {
        let _ = list_wpctl_sources();
    }
}
