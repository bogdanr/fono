// SPDX-License-Identifier: GPL-3.0-only
//! Pure-Rust X11 XTEST per-character typing.
//!
//! Synthesizes a sequence of `KeyPress` / `KeyRelease` events for each
//! character of the dictation, with shift handling for shifted ASCII
//! and on-the-fly keymap remapping (via `ChangeKeyboardMapping`) for
//! characters not present in the user's active layout. **No clipboard
//! involvement** — text appears at the cursor exactly like a real
//! keyboard would deliver it, regardless of clipboard-manager state or
//! the user's clipboard-synchronisation preferences.
//!
//! This is the X11 fallback when no real key-injection tool
//! (`xdotool`/`wtype`/`ydotool`/`enigo`) is installed.

use anyhow::{anyhow, Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as XprotoExt;
use x11rb::protocol::xtest::ConnectionExt as XtestExt;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as WrapperExt;

/// X11 keysyms we need (from `<X11/keysymdef.h>`).
const XK_SHIFT_L: u32 = 0xffe1;
const XK_RETURN: u32 = 0xff0d;
const XK_TAB: u32 = 0xff09;
const XK_BACKSPACE: u32 = 0xff08;

/// X event type codes (core protocol).
const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;

/// Probe whether XTEST is available on the user's display. Used by
/// `Injector::detect()` so we only choose this backend when it actually works.
pub fn xtest_type_available() -> bool {
    let Ok((conn, _screen)) = x11rb::connect(None) else {
        return false;
    };
    conn.xtest_get_version(2, 2).ok().and_then(|c| c.reply().ok()).is_some()
}

/// Type `text` into the currently focused X window, one character at a
/// time, via XTEST. Returns once every keystroke event has been sent to
/// the X server.
pub fn type_via_xtest(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    let (conn, _screen) = x11rb::connect(None)
        .context("xtest-type: cannot connect to display (DISPLAY not set or unreachable)")?;

    let setup = conn.setup();
    let min_kc = setup.min_keycode;
    let max_kc = setup.max_keycode;
    let count = max_kc.saturating_sub(min_kc).saturating_add(1);

    let mapping = conn
        .get_keyboard_mapping(min_kc, count)
        .context("xtest-type: GetKeyboardMapping failed")?
        .reply()
        .context("xtest-type: GetKeyboardMapping reply failed")?;
    let per = mapping.keysyms_per_keycode as usize;
    if per == 0 {
        return Err(anyhow!("xtest-type: server reported zero keysyms_per_keycode"));
    }
    let per_u8 = mapping.keysyms_per_keycode;

    // Build a quick lookup over columns 0 (unshifted) and 1 (shifted).
    // AltGr / level-3 columns are intentionally skipped: handling them
    // would require tracking the user's current layout group, and any
    // character we can't reach via cols 0/1 falls through to the
    // remap-spare path below, which works for any keysym.
    let lookup = |target: u32| -> Option<(u8, bool)> {
        for (i, chunk) in mapping.keysyms.chunks(per).enumerate() {
            let kc = min_kc.checked_add(u8::try_from(i).ok()?)?;
            if chunk.first().copied() == Some(target) {
                return Some((kc, false));
            }
            if per >= 2 && chunk.get(1).copied() == Some(target) {
                return Some((kc, true));
            }
        }
        None
    };

    let shift_kc = lookup(XK_SHIFT_L).map(|(kc, _)| kc);

    // Find a "spare" keycode: one whose every keysym slot is `NoSymbol`
    // (0). We use this slot to temporarily map characters absent from
    // the user's active layout (e.g. accented letters on a plain US
    // layout, emoji, CJK), fire the synthetic press, then restore the
    // slot to `NoSymbol` at the end.
    let spare_kc: Option<u8> = mapping
        .keysyms
        .chunks(per)
        .enumerate()
        .find(|(_, chunk)| chunk.iter().all(|&s| s == 0))
        .and_then(|(i, _)| u8::try_from(i).ok())
        .and_then(|i| min_kc.checked_add(i));

    let mut spare_was_used = false;

    for ch in text.chars() {
        let keysym: u32 = match ch {
            '\n' => XK_RETURN,
            '\t' => XK_TAB,
            '\u{0008}' => XK_BACKSPACE,
            c if (c as u32) < 0x80 => c as u32,
            // X11 Unicode keysym range. See "A proposal for adding
            // Unicode keysyms to X" (Markus Kuhn, 1998).
            c => 0x0100_0000 | (c as u32),
        };

        if let Some((kc, shifted)) = lookup(keysym) {
            if shifted {
                if let Some(skc) = shift_kc {
                    fake_input(&conn, KEY_PRESS, skc)?;
                    fake_input(&conn, KEY_PRESS, kc)?;
                    fake_input(&conn, KEY_RELEASE, kc)?;
                    fake_input(&conn, KEY_RELEASE, skc)?;
                } else {
                    // No Shift key in keymap (very exotic) — best-effort
                    // tap without modifier; will produce the unshifted
                    // variant but never silently drops the character.
                    fake_input(&conn, KEY_PRESS, kc)?;
                    fake_input(&conn, KEY_RELEASE, kc)?;
                }
            } else {
                fake_input(&conn, KEY_PRESS, kc)?;
                fake_input(&conn, KEY_RELEASE, kc)?;
            }
        } else if let Some(skc) = spare_kc {
            // Remap the spare keycode to this keysym, tap it, leave the
            // mapping in place for the next character (we restore once
            // at the end of the run rather than per character to halve
            // the number of round-trips).
            let new_syms: Vec<u32> = (0..per).map(|_| keysym).collect();
            conn.change_keyboard_mapping(1, skc, per_u8, &new_syms)
                .context("xtest-type: ChangeKeyboardMapping (remap) failed")?;
            conn.sync().context("xtest-type: sync after remap failed")?;
            fake_input(&conn, KEY_PRESS, skc)?;
            fake_input(&conn, KEY_RELEASE, skc)?;
            spare_was_used = true;
        } else {
            tracing::warn!(
                "xtest-type: character {ch:?} (keysym 0x{keysym:x}) not in active keymap and \
                 no spare keycode available — skipping. Install xdotool/wtype for full Unicode \
                 typing support."
            );
        }
    }

    // Restore the spare keycode to NoSymbol so we leave the user's
    // keymap exactly as we found it. Only needed if we actually used it.
    if spare_was_used {
        if let Some(skc) = spare_kc {
            let zeros: Vec<u32> = (0..per).map(|_| 0_u32).collect();
            if let Err(e) = conn.change_keyboard_mapping(1, skc, per_u8, &zeros) {
                tracing::warn!("xtest-type: failed to restore spare keycode {skc}: {e}");
            }
        }
    }
    conn.sync().context("xtest-type: final server sync failed")?;
    tracing::debug!(
        target: "fono::inject::xtest_type",
        chars = text.chars().count(),
        bytes = text.len(),
        used_remap = spare_was_used,
        "typed via XTEST"
    );
    Ok(())
}

fn fake_input(conn: &RustConnection, event_type: u8, keycode: u8) -> Result<()> {
    conn.xtest_fake_input(event_type, keycode, 0, x11rb::NONE, 0, 0, 0)
        .map_err(|e| anyhow!("xtest_fake_input failed: {e}"))?;
    Ok(())
}
