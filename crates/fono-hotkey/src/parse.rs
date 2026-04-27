// SPDX-License-Identifier: GPL-3.0-only
//! Parse human-readable hotkey strings such as `F9` or `Ctrl+Alt+Space`.

use anyhow::{anyhow, Result};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct ParsedHotkey {
    pub modifiers: Modifiers,
    pub code: Code,
}

impl ParsedHotkey {
    #[must_use]
    pub fn into_hotkey(self) -> HotKey {
        HotKey::new(Some(self.modifiers), self.code)
    }
}

/// Parse a `+`-separated hotkey. Case-insensitive; supports standard
/// modifier aliases (`Ctrl`/`Control`, `Alt`/`Option`, `Shift`, `Super`/`Meta`/`Cmd`/`Win`).
pub fn parse_hotkey(s: &str) -> Result<ParsedHotkey> {
    let mut modifiers = Modifiers::empty();
    let mut code: Option<Code> = None;

    for raw in s.split('+') {
        let tok = raw.trim();
        if tok.is_empty() {
            continue;
        }
        match tok.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "alt" | "option" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            "super" | "meta" | "cmd" | "command" | "win" => modifiers |= Modifiers::SUPER,
            other => {
                code = Some(
                    parse_code(other)
                        .ok_or_else(|| anyhow!("unknown key code in hotkey {s:?}: {tok:?}"))?,
                );
            }
        }
    }

    Ok(ParsedHotkey {
        modifiers,
        code: code.ok_or_else(|| anyhow!("hotkey {s:?} has no non-modifier key"))?,
    })
}

fn parse_code(tok: &str) -> Option<Code> {
    // Normalise a handful of common aliases first.
    let normalised = match tok {
        "grave" | "`" | "backtick" => "backquote",
        "period" | "." => "period",
        "comma" | "," => "comma",
        "space" => "space",
        "escape" | "esc" => "escape",
        "enter" | "return" => "enter",
        "tab" => "tab",
        "minus" | "-" => "minus",
        "equal" | "=" => "equal",
        other => other,
    };

    // Single letters and digits.
    if normalised.len() == 1 {
        let ch = normalised.chars().next().unwrap();
        if ch.is_ascii_alphabetic() {
            return Code::from_str(&format!("Key{}", ch.to_ascii_uppercase())).ok();
        }
        if ch.is_ascii_digit() {
            return Code::from_str(&format!("Digit{ch}")).ok();
        }
    }

    // Function keys F1..F24.
    if let Some(rest) = normalised
        .strip_prefix('f')
        .or_else(|| normalised.strip_prefix('F'))
    {
        if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() {
            return Code::from_str(&format!("F{rest}")).ok();
        }
    }

    // Title-case the token: "backquote" -> "Backquote" (Code::from_str uses CamelCase).
    let mut chars = normalised.chars();
    let first = chars.next()?.to_ascii_uppercase();
    let camel: String = std::iter::once(first).chain(chars).collect();
    Code::from_str(&camel).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_combos() {
        let p = parse_hotkey("F9").unwrap();
        assert!(p.modifiers.is_empty());
        assert_eq!(p.code, Code::F9);

        let p = parse_hotkey("F8").unwrap();
        assert_eq!(p.code, Code::F8);

        let p = parse_hotkey("Ctrl+Alt+Space").unwrap();
        assert!(p.modifiers.contains(Modifiers::CONTROL | Modifiers::ALT));
        assert_eq!(p.code, Code::Space);

        let p = parse_hotkey("Ctrl+Alt+Grave").unwrap();
        assert_eq!(p.code, Code::Backquote);

        let p = parse_hotkey("Escape").unwrap();
        assert_eq!(p.code, Code::Escape);
        assert!(p.modifiers.is_empty());

        let p = parse_hotkey("Super+F5").unwrap();
        assert!(p.modifiers.contains(Modifiers::SUPER));
        assert_eq!(p.code, Code::F5);
    }

    #[test]
    fn rejects_modifiers_only() {
        assert!(parse_hotkey("Ctrl+Alt").is_err());
    }
}
