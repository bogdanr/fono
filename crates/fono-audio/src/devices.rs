// SPDX-License-Identifier: GPL-3.0-only
//! Input-device enumeration helpers for the dictation pipeline.
//!
//! On Linux desktops where `AudioStack::detect()` returns `PulseAudio`
//! or `PipeWire` (the Pulse compat layer), enumeration is delegated to
//! the audio server via [`crate::pulse::list_pulse_sources`]. The optional
//! cpal branch is only for explicit `cpal-backend` builds on `Unknown`
//! hosts: macOS, Windows, and bare-ALSA Linux without any user-session
//! audio server. See `crates/fono-audio/src/pulse.rs` for the
//! parse-and-delegate model.
//!
//! Filtering: monitor / loopback / HDMI / S/PDIF capture endpoints are
//! common decoys on bare-ALSA Linux (HDMI sinks register a passive
//! capture endpoint). They appear in [`list_input_devices`] unfiltered
//! on the cpal branch so the tray submenu is honest, but
//! [`is_likely_microphone`] returns `false` for them — recovery code
//! uses that filter when offering "switch to X" advice on cpal hosts.
//! On the Pulse path, sink monitors are dropped at the source by
//! [`crate::pulse::list_pulse_sources`], so the heuristic doesn't need
//! to fire there.

#[cfg(feature = "cpal-backend")]
use cpal::traits::{DeviceTrait, HostTrait};

/// Which audio backend produced this row, and the backend-specific
/// identifier needed to switch to it. `display_name` is always the
/// user-facing label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputBackend {
    /// PulseAudio / PipeWire (Pulse compat) source. `pa_name` is the
    /// raw PA name (e.g. `alsa_input.usb-Logitech_BRIO_…`) — pass it
    /// to [`crate::pulse::set_default_pulse_source`] to switch.
    Pulse { pa_name: String },
    /// cpal-enumerated device. `cpal_name` is whatever cpal reported.
    /// Selection on this backend is informational only — Fono no
    /// longer rewrites a config field to force a specific cpal name;
    /// capture always opens cpal's default input device.
    Cpal { cpal_name: String },
}

/// One enumerated input device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    /// User-facing label. On the Pulse branch this is the audio
    /// server's `Description:` (e.g. "Built-in Audio Analog Stereo");
    /// on the cpal branch it's the raw cpal device name.
    pub display_name: String,
    /// `true` when this is the OS default input — Pulse's
    /// `default-source` on the Pulse branch, cpal's
    /// `default_input_device` on the cpal branch.
    pub is_default: bool,
    /// Backend that produced this row + the identifier needed to
    /// switch to it. See [`InputBackend`].
    pub backend: InputBackend,
}

/// Enumerate input devices, dispatching on the active audio stack:
///
/// * `PulseAudio` / `PipeWire` → [`crate::pulse::list_pulse_sources`].
/// * `Unknown` → cpal's default host enumeration (macOS, Windows,
///   pure-ALSA Linux).
///
/// Failures collapse to an empty `Vec` — the caller treats "no devices"
/// identically to a probe failure.
#[must_use]
pub fn list_input_devices() -> Vec<InputDevice> {
    match crate::mute::detect() {
        crate::mute::AudioStack::PulseAudio | crate::mute::AudioStack::PipeWire => {
            crate::pulse::list_pulse_sources()
        }
        crate::mute::AudioStack::Unknown => enumerate_cpal_inputs(),
    }
}

/// Bare-cpal enumeration. Used only on explicit `cpal-backend` builds
/// for the `Unknown` audio-stack branch (macOS / Windows / pure-ALSA
/// Linux without Pulse or PipeWire). On Pulse / PipeWire systems we never
/// call this — the audio server is the authority on what's a microphone.
#[cfg(feature = "cpal-backend")]
#[must_use]
fn enumerate_cpal_inputs() -> Vec<InputDevice> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();
    let Ok(iter) = host.input_devices() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for d in iter {
        let Ok(name) = d.name() else { continue };
        if !seen.insert(name.clone()) {
            continue;
        }
        let is_default = !default_name.is_empty() && name == default_name;
        out.push(InputDevice {
            display_name: name.clone(),
            is_default,
            backend: InputBackend::Cpal { cpal_name: name },
        });
    }
    out
}

/// Default release builds do not include cpal, so `Unknown` hosts report no
/// switchable inputs instead of linking ALSA/libasound into the binary.
#[cfg(not(feature = "cpal-backend"))]
#[must_use]
fn enumerate_cpal_inputs() -> Vec<InputDevice> {
    Vec::new()
}

/// Heuristic: does this device look like a real microphone (vs an
/// HDMI passive capture, an ALSA `Monitor of …` loopback, or an
/// S/PDIF input)? Used by the cpal-branch recovery helper to filter
/// the auto-suggest candidate list. The Pulse path drops sink
/// monitors at the source, so this filter is only needed on the
/// `Unknown` audio-stack branch.
#[must_use]
pub fn is_likely_microphone(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !(lower.contains("monitor")
        || lower.contains("loopback")
        || lower.contains("hdmi")
        || lower.contains("s/pdif")
        || lower.contains("spdif")
        || lower.contains("digital output"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_monitor_and_hdmi_endpoints() {
        // Decoys: should not appear in auto-suggest.
        assert!(!is_likely_microphone("Monitor of Built-in Audio"));
        assert!(!is_likely_microphone("HDMI 1 Capture"));
        assert!(!is_likely_microphone("S/PDIF Digital Input"));
        assert!(!is_likely_microphone("SPDIF passthrough"));
        assert!(!is_likely_microphone("Loopback: PCM"));
        // Real microphones: kept.
        assert!(is_likely_microphone("Built-in Microphone"));
        assert!(is_likely_microphone("USB Headset"));
        assert!(is_likely_microphone("Logitech BRIO"));
        assert!(is_likely_microphone("alsa_input.usb-…"));
    }

    /// `list_input_devices` is intentionally infallible: probe
    /// failures (no `pactl` on PATH and no cpal devices) collapse to
    /// an empty Vec rather than propagating, so headless CI
    /// environments and broken sound stacks don't crash callers.
    #[test]
    fn list_returns_a_vec_without_panicking() {
        let _ = list_input_devices();
    }
}
