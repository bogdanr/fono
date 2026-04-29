// SPDX-License-Identifier: GPL-3.0-only
//! Recovery hook fired when a long-enough recording produces an
//! empty transcript — typical cause: an external dock advertises a
//! passive capture endpoint with no microphone wired to it, and the
//! OS elects it as the default source. Whisper/cloud STT then gets a
//! flat-line buffer and returns nothing.
//!
//! The hook is invoked from both the batch (`run_pipeline`) and live
//! (`on_stop_live_dictation`) paths in `crate::session`, after a
//! recording of at least [`EMPTY_NOTIFY_THRESHOLD_MS`] returns no
//! transcribed text. It pops a single desktop notification naming the
//! current input, the recording duration, and the recourse — point
//! the user at the tray "Microphone" submenu (Pulse-managed) or the
//! OS sound-control panel.
//!
//! No auto-switch and no config-rewrite. The active microphone is
//! resolved from the audio server (Pulse `default-source` /
//! `pactl get-default-source`) or, on the cpal branch, cpal's
//! default input device — Fono no longer keeps an `input_device`
//! override.
//!
//! Plan: `plans/2026-04-29-empty-transcript-microphone-recovery-v2.md`,
//! refined by `plans/2026-04-29-pulseaudio-first-microphone-enumeration-v1.md`.

use fono_audio::devices::{is_likely_microphone, list_input_devices};

/// Capture must be at least this many milliseconds long before the
/// empty-transcript notification fires. Below this we assume the user
/// either misfired the hotkey or genuinely held it on silence; the
/// existing `WARN STT returned empty text` log line is enough.
pub const EMPTY_NOTIFY_THRESHOLD_MS: u64 = 3_000;

/// Pop a fire-and-forget desktop notification when a long capture
/// produced no transcribed text. The active device is resolved from
/// the audio server (or cpal default) — there is no longer a
/// configured override to consult. Returns `true` when a notification
/// was actually dispatched (capture met the threshold).
pub fn notify_empty_capture(capture_ms: u64) -> bool {
    if capture_ms < EMPTY_NOTIFY_THRESHOLD_MS {
        return false;
    }
    let devices = list_input_devices();

    // Resolve the human-readable label of the device that produced
    // silence: whichever row enumeration flagged as the OS default.
    let active_name = devices
        .iter()
        .find(|d| d.is_default)
        .map(|d| d.display_name.clone())
        .unwrap_or_default();
    let active_label = if active_name.is_empty() {
        "system-default".to_string()
    } else {
        format!("'{active_name}'")
    };

    // Auto-suggest candidates: real-microphone-shaped names, excluding
    // the active device. The Pulse branch drops sink-monitors at the
    // source, so the heuristic only matters on cpal hosts.
    let candidates: Vec<String> = devices
        .iter()
        .filter(|d| d.display_name != active_name)
        .filter(|d| is_likely_microphone(&d.display_name))
        .map(|d| d.display_name.clone())
        .collect();

    let body = build_body(capture_ms, &active_label, &candidates);
    fono_core::notify::send(
        "Fono — no audio captured",
        &body,
        "audio-input-microphone",
        10_000,
        fono_core::notify::Urgency::Critical,
    );
    tracing::warn!(
        "no audio captured during {capture_ms} ms recording (input={active_label}); \
         {} alternative microphone(s) available",
        candidates.len()
    );
    true
}

fn build_body(capture_ms: u64, active_label: &str, candidates: &[String]) -> String {
    let secs = (capture_ms + 500) / 1000;
    let lead =
        format!("Recording lasted {secs}s but the {active_label} microphone produced no signal.");
    match candidates.len() {
        0 => format!(
            "{lead} No alternative microphone was detected — check that the device \
             isn't muted or unplugged."
        ),
        1 => {
            let one = &candidates[0];
            format!(
                "{lead} Switch to '{one}' via the tray icon's Microphone submenu, \
                 or use pavucontrol / your OS sound settings."
            )
        }
        _ => {
            let preview = candidates
                .iter()
                .take(3)
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(", ");
            let more = if candidates.len() > 3 {
                format!(" (+{} more)", candidates.len() - 3)
            } else {
                String::new()
            };
            format!(
                "{lead} Choose a different microphone via the tray Microphone submenu, \
                 or open pavucontrol / your OS sound settings. Available: {preview}{more}."
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_is_three_seconds() {
        assert_eq!(EMPTY_NOTIFY_THRESHOLD_MS, 3_000);
    }

    #[test]
    fn body_zero_alternatives_is_actionable() {
        let body = build_body(6000, "system-default", &[]);
        assert!(body.contains("6s"), "body should round to seconds: {body}");
        assert!(body.contains("No alternative microphone"));
        assert!(body.contains("muted"));
    }

    #[test]
    fn body_one_alternative_names_it_and_tray() {
        let body = build_body(7500, "'HDMI 1 Capture'", &["USB Headset".into()]);
        assert!(body.contains("USB Headset"));
        assert!(body.contains("Microphone submenu"));
        assert!(body.contains("pavucontrol"));
        // The deprecated CLI advice must be gone.
        assert!(!body.contains("fono use input"));
    }

    #[test]
    fn body_many_alternatives_points_to_tray_with_preview() {
        let body = build_body(
            5000,
            "system-default",
            &[
                "Mic A".into(),
                "Mic B".into(),
                "Mic C".into(),
                "Mic D".into(),
            ],
        );
        assert!(body.contains("Microphone submenu"));
        assert!(body.contains("Mic A"));
        assert!(body.contains("Mic B"));
        assert!(body.contains("Mic C"));
        assert!(body.contains("+1 more"));
        assert!(body.contains("pavucontrol"));
    }

    #[test]
    fn body_two_alternatives_no_more_suffix() {
        let body = build_body(5000, "system-default", &["Mic A".into(), "Mic B".into()]);
        assert!(body.contains("'Mic A'"));
        assert!(body.contains("'Mic B'"));
        assert!(!body.contains("more"));
    }
}
