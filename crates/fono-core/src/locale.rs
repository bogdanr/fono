// SPDX-License-Identifier: GPL-3.0-only
//! OS-locale detection for the language-cache bootstrap (plan v3
//! task 3). Returns deduplicated lowercase BCP-47 alpha-2 codes in
//! priority order (highest-confidence first). Pure best-effort —
//! every error path collapses to an empty `Vec`. The wizard and the
//! STT factory consume this; runtime never depends on it succeeding.

use std::process::Command;

/// Detect the user's preferred language codes from the operating
/// system. Returns an empty `Vec` on any failure.
///
/// Sources, in priority order:
/// * `LC_ALL`, `LC_MESSAGES`, `LANG` (POSIX);
/// * `localectl status` first line of `Locale=` (systemd Linux);
/// * `defaults read .GlobalPreferences AppleLanguages` (macOS);
/// * `Get-WinUserLanguageList` (Windows PowerShell).
///
/// Output is lowercase alpha-2, deduplicated, first-seen order.
#[must_use]
pub fn detect_os_languages() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |out: &mut Vec<String>, code: String| {
        let lc = code.trim().to_ascii_lowercase();
        if lc.is_empty() || lc == "c" || lc == "posix" || out.contains(&lc) {
            return;
        }
        out.push(lc);
    };

    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(raw) = std::env::var(var) {
            if let Some(code) = parse_posix_locale(&raw) {
                push(&mut out, code);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(o) = Command::new("localectl").arg("status").output() {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout);
                for line in s.lines() {
                    let line = line.trim();
                    if let Some(rest) = line.strip_prefix("System Locale: LANG=") {
                        if let Some(code) = parse_posix_locale(rest) {
                            push(&mut out, code);
                        }
                    } else if let Some(rest) = line.strip_prefix("LANG=") {
                        if let Some(code) = parse_posix_locale(rest) {
                            push(&mut out, code);
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(o) = Command::new("defaults")
            .args(["read", ".GlobalPreferences", "AppleLanguages"])
            .output()
        {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout);
                for line in s.lines() {
                    let trimmed = line.trim().trim_matches(|c: char| c == '"' || c == ',');
                    if trimmed.is_empty() || trimmed == "(" || trimmed == ")" {
                        continue;
                    }
                    if let Some(code) = parse_posix_locale(trimmed) {
                        push(&mut out, code);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(o) = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-WinUserLanguageList | ForEach-Object LanguageTag",
            ])
            .output()
        {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout);
                for line in s.lines() {
                    if let Some(code) = parse_posix_locale(line) {
                        push(&mut out, code);
                    }
                }
            }
        }
    }

    let _ = Command::new::<&str>; // silence unused-import on platforms without the cfg arm
    out
}

/// Parse a POSIX-style locale tag (`en_US.UTF-8`, `ro_RO`, `pt-BR`)
/// down to the lowercase alpha-2 language subtag. Returns `None` for
/// `C`, `POSIX`, empty, or unrecognised tags.
fn parse_posix_locale(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('"');
    if trimmed.is_empty() {
        return None;
    }
    // Strip codeset (`.UTF-8`) and modifier (`@euro`).
    let head = trimmed.split(['.', '@']).next()?;
    // Take the language subtag before `_` or `-`.
    let lang = head.split(['_', '-']).next()?;
    let lc = lang.trim().to_ascii_lowercase();
    if lc.is_empty() || lc == "c" || lc == "posix" {
        return None;
    }
    // BCP-47 language subtags are 2–3 ASCII letters.
    if lc.len() < 2 || lc.len() > 3 || !lc.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(lc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_posix_forms() {
        assert_eq!(parse_posix_locale("en_US.UTF-8").as_deref(), Some("en"));
        assert_eq!(parse_posix_locale("ro_RO").as_deref(), Some("ro"));
        assert_eq!(parse_posix_locale("pt-BR").as_deref(), Some("pt"));
        assert_eq!(parse_posix_locale("de_DE@euro").as_deref(), Some("de"));
        assert_eq!(parse_posix_locale("\"fr_FR.UTF-8\"").as_deref(), Some("fr"));
    }

    #[test]
    fn rejects_c_and_posix_and_empty() {
        assert_eq!(parse_posix_locale("C"), None);
        assert_eq!(parse_posix_locale("POSIX"), None);
        assert_eq!(parse_posix_locale(""), None);
        assert_eq!(parse_posix_locale("   "), None);
        assert_eq!(parse_posix_locale("123"), None);
    }
}
