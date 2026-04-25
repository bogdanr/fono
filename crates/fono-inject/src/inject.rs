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
    /// `xdotool type --delay 0 -- <text>` subprocess (X11 / XWayland).
    /// Independent of the `enigo-backend` feature (no libxdo C dep).
    Xdotool,
    /// Pure-Rust XTEST `Ctrl+V` after a clipboard write. No system tools
    /// needed; only requires that the focused app honours Ctrl+V as paste
    /// (every text input on every desktop does). Compiled when
    /// `x11-paste` feature is on (default).
    #[cfg(feature = "x11-paste")]
    XtestPaste,
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

        // X11 / XWayland: prefer xdotool subprocess (works on KDE/GNOME X11
        // and via XWayland on most Wayland sessions). Honour
        // FONO_INJECT_BACKEND=xdotool|wtype|ydotool|enigo|xtest|none for forced override.
        if let Ok(forced) = std::env::var("FONO_INJECT_BACKEND") {
            return match forced.to_ascii_lowercase().as_str() {
                "xdotool" => Self::Xdotool,
                "wtype" => Self::Wtype,
                "ydotool" => Self::Ydotool,
                #[cfg(feature = "enigo-backend")]
                "enigo" => Self::Enigo,
                #[cfg(feature = "x11-paste")]
                "xtest" | "xtestpaste" | "paste" => Self::XtestPaste,
                "none" => Self::None,
                _ => Self::None,
            };
        }

        if !wayland {
            #[cfg(feature = "enigo-backend")]
            {
                return Self::Enigo;
            }
            #[cfg(not(feature = "enigo-backend"))]
            if which("xdotool").is_some() {
                return Self::Xdotool;
            }
        }

        if which("wtype").is_some() {
            return Self::Wtype;
        }
        if which("ydotool").is_some() {
            return Self::Ydotool;
        }
        // XWayland fallback for Wayland sessions where wtype isn't routed
        // (e.g. KWin/KDE Wayland). xdotool can still type into XWayland apps.
        if which("xdotool").is_some() {
            return Self::Xdotool;
        }
        // Pure-Rust XTEST keystroke (Ctrl+V) against an already-populated
        // clipboard. Last resort before declaring None — works on any X11
        // session even without xdotool/wtype/enigo installed.
        #[cfg(feature = "x11-paste")]
        if !wayland && crate::xtest_paste::xtest_available() {
            return Self::XtestPaste;
        }
        #[cfg(feature = "enigo-backend")]
        {
            // Last resort: enigo can sometimes work via libei.
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
            Self::Xdotool => inject_subprocess("xdotool", &["type", "--delay", "0", "--", text]),
            #[cfg(feature = "x11-paste")]
            Self::XtestPaste => {
                // Two-step: populate the X CLIPBOARD via the standard
                // helper, then synthesize Ctrl+V via XTEST. Robust against
                // Unicode and per-app keymaps because the actual character
                // bytes flow through the clipboard, not through synthetic
                // per-key events.
                copy_to_clipboard(text)
                    .map_err(|e| anyhow!("xtest-paste: clipboard prep failed: {e}"))?;
                crate::xtest_paste::paste_via_xtest_default()
            }
            Self::None => Err(anyhow!(
                "no text-injection backend available; on X11 set DISPLAY so the built-in \
                 xtest-paste backend can connect, or install wtype/ydotool/xdotool, \
                 or enable the enigo-backend feature"
            )),
        }
    }
}

/// Convenience: detect and inject in one call. When no key-injection
/// backend is available, falls back to copying the text to the system
/// clipboard so the user can paste it manually with Ctrl+V (rather
/// than silently dropping the dictation). Returns the *outcome* so the
/// caller can surface an appropriate notification.
pub fn type_text(text: &str) -> Result<()> {
    match type_text_with_outcome(text)? {
        InjectOutcome::Typed(_) => Ok(()),
        InjectOutcome::Clipboard(tool) => {
            warn!(
                "no key-injection backend worked; copied to clipboard via {tool} \
                 — press Ctrl+V to paste"
            );
            Ok(())
        }
    }
}

/// Result of an injection attempt — either keys were typed via a
/// keyboard backend, or the text was placed on the clipboard as a
/// last-resort fallback. The orchestrator uses this to decide which
/// notification to show.
#[derive(Debug, Clone)]
pub enum InjectOutcome {
    /// Text was typed via the named backend (`"enigo"`, `"wtype"`, …).
    Typed(&'static str),
    /// Text was copied to the clipboard via the named tool
    /// (`"wl-copy"`, `"xclip"`, `"xsel"`). User must paste manually.
    Clipboard(&'static str),
}

/// Like [`type_text`] but tells the caller whether the text was typed
/// or merely copied to the clipboard. Always succeeds if at least one
/// of {key-injection, clipboard tool} works on the host; only fails
/// when neither path is available so fono can never silently lose a
/// dictation.
pub fn type_text_with_outcome(text: &str) -> Result<InjectOutcome> {
    let inj = Injector::detect();
    debug!("type_text via {inj:?}: {} bytes", text.len());
    match inj.inject(text) {
        Ok(()) => Ok(InjectOutcome::Typed(match inj {
            #[cfg(feature = "enigo-backend")]
            Injector::Enigo => "enigo",
            Injector::Wtype => "wtype",
            Injector::Ydotool => "ydotool",
            Injector::Xdotool => "xdotool",
            #[cfg(feature = "x11-paste")]
            Injector::XtestPaste => "xtest-paste",
            Injector::None => "none",
        })),
        Err(key_err) => match copy_to_clipboard(text) {
            Ok(tool) => Ok(InjectOutcome::Clipboard(tool)),
            Err(clip_err) => Err(anyhow!(
                "key injection failed ({key_err}) and clipboard fallback \
                 failed ({clip_err}); on X11 ensure DISPLAY is exported so \
                 xtest-paste can connect, or install wtype/ydotool/xclip/wl-clipboard \
                 or enable the enigo-backend feature"
            )),
        },
    }
}

/// Best-effort clipboard copy via subprocess. Probes wl-copy, xclip,
/// then xsel in that order and returns the tool name that worked.
/// Fails only when none of them are available.
pub fn copy_to_clipboard(text: &str) -> Result<&'static str> {
    let outcomes = copy_to_clipboard_all(text);
    // Return the first tool/target pair that worked; otherwise concatenate errors.
    if let Some((t, target)) = outcomes
        .iter()
        .find_map(|o| o.success.then_some((o.tool, o.target)))
    {
        // Log secondary successes so users on Wayland sessions with X11 clipboard
        // managers (e.g. clipit) see that the X clipboard was also populated.
        let extras: Vec<String> = outcomes
            .iter()
            .filter(|o| o.success && (o.tool != t || o.target != target))
            .map(|o| format!("{} [{}]", o.tool, o.target))
            .collect();
        if !extras.is_empty() {
            tracing::info!("clipboard: also wrote via {}", extras.join(", "));
        }
        return Ok(t);
    }
    let errs: Vec<String> = outcomes
        .into_iter()
        .map(|o| format!("{}: {}", o.tool, o.detail))
        .collect();
    Err(anyhow!(
        "no clipboard tool worked. Diagnostics: [{}]. \
         On Wayland install `wl-clipboard` (provides wl-copy). \
         On X11 install `xclip` or `xsel`. \
         Verify DISPLAY/WAYLAND_DISPLAY are set in the daemon's environment.",
        if errs.is_empty() {
            "no clipboard tools installed".into()
        } else {
            errs.join(" | ")
        }
    ))
}

/// One attempt's result for diagnostic surfacing.
#[derive(Debug)]
pub struct ClipboardAttempt {
    pub tool: &'static str,
    /// Human-readable target ("clipboard", "primary", "wayland") so users
    /// can tell two attempts of the same tool apart in `fono test-inject`.
    pub target: &'static str,
    pub success: bool,
    pub detail: String,
}

/// Try every detected clipboard tool (wl-copy, xclip, xsel). Useful on
/// Wayland sessions where users run an X11-only clipboard manager (clipit)
/// alongside a Wayland-native one (Klipper) — writing to both ensures the
/// entry appears in whichever manager the user actually has.
///
/// Each attempt's stderr is captured and returned in `detail` so silent
/// failures (no DISPLAY, no WAYLAND_DISPLAY, missing protocol support) are
/// always visible in logs.
pub fn copy_to_clipboard_all(text: &str) -> Vec<ClipboardAttempt> {
    use std::io::Write;

    // Each tuple is (tool, args, target_label, requires_env). We write to
    // BOTH the Wayland and X11 clipboards (and CLIPBOARD + PRIMARY on X11)
    // so any clipboard manager (clipit, Klipper, parcellite, copyq) catches
    // the entry regardless of which selection it watches.
    let candidates: &[(&str, &[&str], &str, &str)] = &[
        ("wl-copy", &[], "wayland", "WAYLAND_DISPLAY"),
        (
            "xclip",
            &["-selection", "clipboard"],
            "clipboard",
            "DISPLAY",
        ),
        ("xsel", &["--clipboard", "--input"], "clipboard", "DISPLAY"),
        ("xclip", &["-selection", "primary"], "primary", "DISPLAY"),
        ("xsel", &["--primary", "--input"], "primary", "DISPLAY"),
    ];

    let mut out = Vec::new();
    let mut tools_seen = std::collections::HashSet::new();
    for (tool, args, target, env_required) in candidates {
        if which(tool).is_none() {
            // Only record once per tool to avoid duplicate "not installed" rows
            // when the same binary is tried with two arg sets.
            if tools_seen.insert(*tool) {
                out.push(ClipboardAttempt {
                    tool,
                    target: "-",
                    success: false,
                    detail: "not installed".into(),
                });
            }
            continue;
        }
        if std::env::var(env_required).is_err() {
            out.push(ClipboardAttempt {
                tool,
                target,
                success: false,
                detail: format!("{env_required} not set in environment"),
            });
            continue;
        }
        tools_seen.insert(*tool);
        let mut child = match Command::new(tool)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                out.push(ClipboardAttempt {
                    tool,
                    target,
                    success: false,
                    detail: format!("spawn failed: {e}"),
                });
                continue;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(text.as_bytes()) {
                let _ = child.kill();
                out.push(ClipboardAttempt {
                    tool,
                    target,
                    success: false,
                    detail: format!("write_all failed: {e}"),
                });
                continue;
            }
            drop(stdin); // close pipe so the tool sees EOF
        }
        let output = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => {
                out.push(ClipboardAttempt {
                    tool,
                    target,
                    success: false,
                    detail: format!("wait failed: {e}"),
                });
                continue;
            }
        };
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if output.status.success() {
            out.push(ClipboardAttempt {
                tool,
                target,
                success: true,
                detail: if stderr.is_empty() {
                    "ok".into()
                } else {
                    format!("ok (stderr: {stderr})")
                },
            });
        } else {
            out.push(ClipboardAttempt {
                tool,
                target,
                success: false,
                detail: format!("exit {}: {stderr}", output.status),
            });
        }
    }
    out
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
        Injector::Xdotool => {
            let _ = Command::new("xdotool")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .status();
            Ok("xdotool")
        }
        #[cfg(feature = "x11-paste")]
        Injector::XtestPaste => {
            // Pre-warm the X connection so the first dictation doesn't
            // pay TCP/Unix-socket setup + XTEST QueryVersion roundtrip.
            let _ = crate::xtest_paste::xtest_available();
            Ok("xtest-paste")
        }
        Injector::None => Err(anyhow!(
            "no text-injection backend available — on X11 the built-in xtest-paste \
             backend should work when DISPLAY is set; otherwise install \
             `wtype`/`ydotool`/`xdotool` on Linux, or enable the enigo-backend feature"
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
