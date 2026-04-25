// SPDX-License-Identifier: GPL-3.0-only
//! Pure-Rust X11 XTEST keystroke synthesis. Used as a "paste from clipboard"
//! fallback when no real key-injection tool (wtype/ydotool/xdotool/enigo) is
//! available — we copy the dictation to the X CLIPBOARD selection and then
//! send a synthetic paste shortcut (Shift+Insert by default) to the focused
//! window via the XTEST extension.
//!
//! No system binaries required; only `libxcb` which every X11 session
//! already has loaded.
//!
//! ## Why Shift+Insert is the default
//!
//! Shift+Insert is the X11 legacy paste binding, hard-coded into virtually
//! every toolkit's text input handler — xterm/urxvt/st (PRIMARY), GTK/Qt
//! (CLIPBOARD), VTE-based terminals (PRIMARY), modern GPU terminals
//! (CLIPBOARD), Vim/Emacs in insert mode, etc. We populate **both** PRIMARY
//! and CLIPBOARD before sending the keystroke, so the toolkit's choice of
//! selection is invisible. By contrast Ctrl+V is captured by shells, tmux,
//! Vim normal mode and various terminal "verbatim insert" bindings — it
//! works in GUI text fields but breaks in every terminal context.
//!
//! Override via `[inject] paste_shortcut = "ctrl-v"` in config or
//! `FONO_PASTE_SHORTCUT=ctrl-v` env var for the rare app that needs a
//! different binding.

use anyhow::{anyhow, Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as XprotoExt;
use x11rb::protocol::xtest::ConnectionExt as XtestExt;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as WrapperExt;

/// X11 keysyms we need (from `<X11/keysymdef.h>`).
const XK_SHIFT_L: u32 = 0xffe1;
const XK_CONTROL_L: u32 = 0xffe3;
const XK_INSERT: u32 = 0xff63;
const XK_KP_INSERT: u32 = 0xff9e;
const XK_V: u32 = 0x0076;

/// Which keystroke combo to synthesize after writing the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PasteShortcut {
    /// Universal X11 paste — works in terminals (xterm/urxvt/st/alacritty/
    /// kitty/gnome-terminal/konsole/foot/wezterm) and every GUI text
    /// widget. Default.
    #[default]
    ShiftInsert,
    /// GUI-style paste. Captured by shells/tmux/vim — terminals usually
    /// won't accept it. Use only when an app remaps Shift+Insert.
    CtrlV,
    /// Modern desktop terminal "official" paste binding (gnome-terminal,
    /// konsole, alacritty). Not bound by xterm/urxvt/st by default.
    CtrlShiftV,
}

impl PasteShortcut {
    /// Parse a config / env-var value. Accepts hyphens, underscores, and
    /// case-insensitive variants. Returns `None` for unknown strings so
    /// callers can fall back to the default and log a warning.
    pub fn parse(s: &str) -> Option<Self> {
        let normalised: String = s
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        let normalised = normalised.replace('_', "-");
        match normalised.as_str() {
            "shift-insert" | "shiftinsert" | "shift+insert" | "s-insert" => Some(Self::ShiftInsert),
            "ctrl-v" | "ctrlv" | "ctrl+v" | "c-v" => Some(Self::CtrlV),
            "ctrl-shift-v" | "ctrlshiftv" | "ctrl+shift+v" | "c-s-v" => Some(Self::CtrlShiftV),
            _ => None,
        }
    }

    /// Read `FONO_PASTE_SHORTCUT` from env; fall back to default
    /// (Shift+Insert). Logs at `warn` if the env value is set but
    /// unparseable so users notice their typo.
    pub fn from_env_or_default() -> Self {
        let Ok(v) = std::env::var("FONO_PASTE_SHORTCUT") else {
            return Self::default();
        };
        Self::parse(&v).unwrap_or_else(|| {
            tracing::warn!(
                "FONO_PASTE_SHORTCUT={v:?} unrecognised; using default Shift+Insert"
            );
            Self::default()
        })
    }

    /// Modifier keysyms to press before the main key (in order).
    fn modifiers(self) -> &'static [u32] {
        match self {
            Self::ShiftInsert => &[XK_SHIFT_L],
            Self::CtrlV => &[XK_CONTROL_L],
            Self::CtrlShiftV => &[XK_CONTROL_L, XK_SHIFT_L],
        }
    }

    /// Main key keysym; for Insert we also accept the keypad variant
    /// because some keymaps map the dedicated Insert key to KP_Insert.
    fn key_candidates(self) -> &'static [u32] {
        match self {
            Self::ShiftInsert => &[XK_INSERT, XK_KP_INSERT],
            Self::CtrlV | Self::CtrlShiftV => &[XK_V],
        }
    }

    /// Human-readable label for logging.
    pub fn label(self) -> &'static str {
        match self {
            Self::ShiftInsert => "Shift+Insert",
            Self::CtrlV => "Ctrl+V",
            Self::CtrlShiftV => "Ctrl+Shift+V",
        }
    }
}

/// Probe whether XTEST is available on the user's display. Used by
/// `Injector::detect()` so we only choose this backend when it actually works.
pub fn xtest_available() -> bool {
    let Ok((conn, _screen)) = x11rb::connect(None) else {
        return false;
    };
    // Querying the XTEST version both confirms the extension is loaded
    // *and* warms the protocol round-trip so the first inject is faster.
    conn.xtest_get_version(2, 2)
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some()
}

/// Synthesize the configured paste shortcut, reading
/// `FONO_PASTE_SHORTCUT` from env if set and falling back to
/// Shift+Insert otherwise. Used by `Injector::XtestPaste::inject`.
pub fn paste_via_xtest_default() -> Result<()> {
    paste_via_xtest(PasteShortcut::from_env_or_default())
}

/// Synthesize the given paste shortcut into the currently focused
/// X window. Returns Ok when every keystroke event was sent to the X
/// server (note: we cannot verify the *receiving* application actually
/// accepted it — that's down to the app's own keymap. Most do).
pub fn paste_via_xtest(shortcut: PasteShortcut) -> Result<()> {
    let (conn, _screen) = x11rb::connect(None)
        .context("x11rb: cannot connect to display (DISPLAY not set or unreachable)")?;

    // Resolve modifier keycodes once.
    let mut mod_codes = Vec::with_capacity(shortcut.modifiers().len());
    for sym in shortcut.modifiers() {
        let kc = keysym_to_keycode(&conn, *sym).with_context(|| {
            format!("xtest: cannot resolve modifier keysym 0x{sym:x} to a keycode")
        })?;
        mod_codes.push(kc);
    }

    // Resolve main-key keycode; try candidates in order so Insert and
    // KP_Insert both work.
    let mut key_code = None;
    let mut last_err = None;
    for cand in shortcut.key_candidates() {
        match keysym_to_keycode(&conn, *cand) {
            Ok(kc) => {
                key_code = Some(kc);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let key_code = key_code.ok_or_else(|| {
        last_err.unwrap_or_else(|| anyhow!("xtest: no candidate keysym present in active keymap"))
    })?;

    tracing::info!(
        "xtest-paste: synthesizing {} (mod_keycodes={:?} key_keycode={})",
        shortcut.label(),
        mod_codes,
        key_code
    );

    // Press modifiers in order, press main key, release main key,
    // release modifiers in reverse. `time = 0` lets the server choose.
    for kc in &mod_codes {
        fake_input(&conn, true, *kc)?;
    }
    fake_input(&conn, true, key_code)?;
    fake_input(&conn, false, key_code)?;
    for kc in mod_codes.iter().rev() {
        fake_input(&conn, false, *kc)?;
    }
    conn.sync().context("xtest: server sync failed")?;
    Ok(())
}

fn fake_input(conn: &RustConnection, press: bool, keycode: u8) -> Result<()> {
    // Event type 2 = KeyPress, 3 = KeyRelease per the core X protocol.
    let ty: u8 = if press { 2 } else { 3 };
    conn.xtest_fake_input(ty, keycode, 0, x11rb::NONE, 0, 0, 0)
        .map_err(|e| anyhow!("xtest_fake_input failed: {e}"))?;
    Ok(())
}

/// Map an X11 keysym → keycode using the running server's keymap.
/// Iterates the full keycode range (`min_keycode..=max_keycode`) and the
/// keysyms-per-keycode group, returning the first match. ~2 ms one-shot;
/// not worth caching for our 4-keystroke sequence.
fn keysym_to_keycode(conn: &RustConnection, target: u32) -> Result<u8> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let max = setup.max_keycode;
    let count = (max - min).saturating_add(1);
    let reply = conn
        .get_keyboard_mapping(min, count)
        .context("xtest: GetKeyboardMapping failed")?
        .reply()
        .context("xtest: GetKeyboardMapping reply failed")?;
    let per = reply.keysyms_per_keycode as usize;
    if per == 0 {
        return Err(anyhow!("xtest: server reported zero keysyms_per_keycode"));
    }
    for (i, chunk) in reply.keysyms.chunks(per).enumerate() {
        if chunk.contains(&target) {
            return Ok(min + u8::try_from(i).unwrap_or(0));
        }
    }
    Err(anyhow!(
        "xtest: keysym 0x{target:x} not present in the active X keymap"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_shift_insert_variants() {
        for v in [
            "shift-insert",
            "Shift-Insert",
            "SHIFT_INSERT",
            "shift+insert",
            "ShiftInsert",
            "shiftinsert",
            "s-insert",
        ] {
            assert_eq!(
                PasteShortcut::parse(v),
                Some(PasteShortcut::ShiftInsert),
                "failed to parse {v:?}"
            );
        }
    }

    #[test]
    fn parse_ctrl_v_variants() {
        for v in ["ctrl-v", "Ctrl-V", "CTRL_V", "ctrl+v", "ctrlv", "c-v"] {
            assert_eq!(
                PasteShortcut::parse(v),
                Some(PasteShortcut::CtrlV),
                "failed to parse {v:?}"
            );
        }
    }

    #[test]
    fn parse_ctrl_shift_v_variants() {
        for v in [
            "ctrl-shift-v",
            "Ctrl-Shift-V",
            "CTRL_SHIFT_V",
            "ctrl+shift+v",
            "ctrlshiftv",
            "c-s-v",
        ] {
            assert_eq!(
                PasteShortcut::parse(v),
                Some(PasteShortcut::CtrlShiftV),
                "failed to parse {v:?}"
            );
        }
    }

    #[test]
    fn parse_unknown_returns_none() {
        for v in ["", "ctrl", "paste", "ctrl+x", "shift+v", "garbage"] {
            assert_eq!(PasteShortcut::parse(v), None, "should reject {v:?}");
        }
    }

    #[test]
    fn default_is_shift_insert() {
        assert_eq!(PasteShortcut::default(), PasteShortcut::ShiftInsert);
    }

    #[test]
    fn modifiers_match_label() {
        assert_eq!(PasteShortcut::ShiftInsert.modifiers(), &[XK_SHIFT_L]);
        assert_eq!(PasteShortcut::CtrlV.modifiers(), &[XK_CONTROL_L]);
        assert_eq!(
            PasteShortcut::CtrlShiftV.modifiers(),
            &[XK_CONTROL_L, XK_SHIFT_L]
        );
    }

    #[test]
    fn key_candidates_match_label() {
        assert_eq!(
            PasteShortcut::ShiftInsert.key_candidates(),
            &[XK_INSERT, XK_KP_INSERT]
        );
        assert_eq!(PasteShortcut::CtrlV.key_candidates(), &[XK_V]);
        assert_eq!(PasteShortcut::CtrlShiftV.key_candidates(), &[XK_V]);
    }
}
