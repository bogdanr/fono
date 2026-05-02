// SPDX-License-Identifier: GPL-3.0-only
//! `fono` library surface — re-exports the modules used by the binary
//! entrypoint and the integration tests under `crates/fono/tests/`.
//!
//! All real logic lives in submodules; `main.rs` is a thin entrypoint
//! and `tests/pipeline.rs` exercises the pipeline orchestrator without
//! a microphone or a network.

// `whisper-rs-sys` and `llama-cpp-sys-2` each statically link their own
// copy of ggml. The workspace-level `.cargo/config.toml` passes
// `-Wl,--allow-multiple-definition` to the GNU/musl linker so the
// duplicate ggml symbols dedupe at link time instead of aborting the
// build. Both bundled ggml versions come from the same upstream
// (`ggerganov/ggml`) and are ABI-compatible by construction, so the
// linker keeping the first copy and discarding the second is safe;
// the smoke test in `crates/fono/tests/pipeline.rs` exercises both
// engines in the same process to catch any regression. See
// `plans/2026-04-27-shared-ggml-static-binary-v1.md` for the full
// rationale and the long-term shared-ggml plan.

pub mod audio_recovery;
pub mod cli;
pub mod daemon;
pub mod doctor;
pub mod install;
pub mod models;
pub mod session;
pub mod wizard;

#[cfg(feature = "interactive")]
pub mod live;

/// True when the daemon's environment indicates a graphical session —
/// either an X11 server (`DISPLAY` set) or a Wayland compositor
/// (`WAYLAND_DISPLAY` set). Used to runtime-gate the tray, overlay, and
/// text-injection surfaces so the same single binary runs cleanly on
/// headless servers and on graphical desktops.
///
/// See `plans/2026-04-30-fono-single-binary-size-v1.md` Phase 3 +
/// `docs/decisions/0022-binary-size-budget.md` for the contract.
#[must_use]
pub fn is_graphical_session() -> bool {
    is_graphical_session_in(|k| std::env::var_os(k))
}

/// Testable variant: takes a closure that resolves env-var lookups so
/// unit tests can drive both branches without mutating the real
/// process environment (which is racy with parallel test runners).
fn is_graphical_session_in<F>(lookup: F) -> bool
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    let nonempty = |k: &str| lookup(k).is_some_and(|v| !v.is_empty());
    nonempty("DISPLAY") || nonempty("WAYLAND_DISPLAY")
}

#[cfg(test)]
mod graphical_session_tests {
    use std::ffi::OsString;

    use super::is_graphical_session_in;

    fn lookup<'a>(map: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k: &str| {
            map.iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| OsString::from(*v))
        }
    }

    #[test]
    fn headless_when_neither_display_var_set() {
        assert!(!is_graphical_session_in(lookup(&[])));
    }

    #[test]
    fn graphical_when_x11_display_set() {
        assert!(is_graphical_session_in(lookup(&[("DISPLAY", ":0")])));
    }

    #[test]
    fn graphical_when_wayland_display_set() {
        assert!(is_graphical_session_in(lookup(&[(
            "WAYLAND_DISPLAY",
            "wayland-0"
        )])));
    }

    #[test]
    fn graphical_when_both_display_vars_set() {
        assert!(is_graphical_session_in(lookup(&[
            ("DISPLAY", ":0"),
            ("WAYLAND_DISPLAY", "wayland-0"),
        ])));
    }

    #[test]
    fn empty_string_treated_as_unset() {
        // systemd `Environment=DISPLAY=` produces an empty string; the
        // x11rb/winit code paths fail in confusing ways on that. Treat
        // it as "no graphical surface available" — same as unset.
        assert!(!is_graphical_session_in(lookup(&[("DISPLAY", "")])));
        assert!(!is_graphical_session_in(lookup(&[("WAYLAND_DISPLAY", "")])));
    }
}
