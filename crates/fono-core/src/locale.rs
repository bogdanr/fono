// SPDX-License-Identifier: GPL-3.0-only
//! Multi-signal OS language detection for the wizard and the daemon
//! startup banner. Best-effort — every probe collapses to "no
//! contribution" on error; the whole function is allowed to return an
//! empty `Vec` and runtime never depends on it succeeding.
//!
//! ## Pipeline
//!
//! 1. Each probe (env vars, `localectl`, `/etc/locale.conf`,
//!    timezone, X11/Wayland keyboard layout, OS-native APIs)
//!    classifies its findings as one of [`SignalKind`].
//! 2. The accumulator stores `(code, kind) → max weight seen`. The
//!    same kind firing from multiple sources (e.g. `LANG` in env *and*
//!    in `/etc/locale.conf` *and* in `localectl` output) contributes
//!    **once** at the strongest weight observed — no double-counting.
//! 3. Final score per language = sum of max weights across kinds.
//!    Allow-list = score ≥ 1.
//!
//! ## Signals & platforms
//!
//! | Kind         | Linux                              | macOS              | Windows                |
//! |--------------|------------------------------------|--------------------|------------------------|
//! | SystemLang   | env / localectl / locale.conf      | `AppleLocale`      | `Get-WinSystemLocale`  |
//! | FormatLocale | env LC_* / localectl LC_* / file   | —                  | —                      |
//! | Keyboard     | localectl + setxkbmap + gsettings + kxkbrc | input sources | (via UiList tags)      |
//! | Timezone     | `TZ` / `/etc/localtime` + `zone1970.tab` | same         | (via HomeRegion)       |
//! | UiList       | —                                  | `AppleLanguages`   | `Get-WinUserLanguageList` |
//! | HomeRegion   | —                                  | —                  | `Get-WinHomeLocation`  |

use std::collections::HashMap;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use std::process::Command;

/// Class of OS signal that pointed at a language code. Used for
/// dedup-by-kind (one contribution per `(code, kind)` at the max
/// weight observed) and for the friendly banner reason list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalKind {
    /// `LANG` / `LC_ALL` from any source — UI language.
    SystemLang,
    /// `LC_TIME` / `LC_NUMERIC` / `LC_MESSAGES` / `LC_MONETARY` /
    /// `LC_PAPER` from any source — the canonical "I live here"
    /// signal that survives a forced `LANG=en_US.UTF-8`.
    FormatLocale,
    /// Active keyboard layout from any source (X11 system default,
    /// runtime `setxkbmap`, GNOME `gsettings`, KDE `kxkbrc`, macOS
    /// `AppleEnabledInputSources`, Windows input methods).
    Keyboard,
    /// IANA timezone resolved via `TZ` env or `/etc/localtime`,
    /// mapped to a country via the system's `zone1970.tab`.
    Timezone,
    /// macOS `AppleLanguages` ordered list, Windows
    /// `Get-WinUserLanguageList`.
    UiList,
    /// Windows `Get-WinHomeLocation` (GeoID set at install).
    HomeRegion,
}

impl SignalKind {
    /// User-friendly label for the daemon banner and the wizard.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::SystemLang => "system locale",
            Self::FormatLocale => "formatting locale",
            Self::Keyboard => "keyboard layout",
            Self::Timezone => "timezone",
            Self::UiList => "preferred-language list",
            Self::HomeRegion => "home region",
        }
    }
}

/// One detected user language with the score it accumulated and the
/// list of `SignalKind`s that fired (first-seen order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedLanguage {
    /// Lowercase BCP-47 alpha-2 language subtag (e.g. `"ro"`).
    pub code: String,
    /// Sum of max weights across the kinds that pointed at this code.
    /// Score ≥ 1 ⇒ detected.
    pub score: u8,
    /// Distinct kinds that contributed. Rendered via
    /// [`SignalKind::label`] in the daemon banner.
    pub reasons: Vec<SignalKind>,
}

/// Detect the user's preferred languages with per-code confidence
/// scores. Returns an empty `Vec` on any failure. Codes sort by
/// descending score, then by first-seen order (stable on ties).
#[must_use]
pub fn detect_user_languages_ranked() -> Vec<DetectedLanguage> {
    let mut acc = Accumulator::default();

    // POSIX env vars are universal — work on every platform including
    // WSL and macOS shells. `LANG` / `LC_ALL` are the UI language;
    // `LC_TIME` / `LC_NUMERIC` / `LC_MESSAGES` / `LC_MONETARY` /
    // `LC_PAPER` are the formatting locale that survives a forced
    // `LANG=en_US.UTF-8`.
    if let Ok(raw) = std::env::var("LC_ALL") {
        push_locale(&mut acc, &raw, SignalKind::SystemLang, 2);
    }
    if let Ok(raw) = std::env::var("LANG") {
        push_locale(&mut acc, &raw, SignalKind::SystemLang, 2);
    }
    for var in ["LC_MESSAGES", "LC_TIME", "LC_NUMERIC", "LC_MONETARY", "LC_PAPER"] {
        if let Ok(raw) = std::env::var(var) {
            push_locale(&mut acc, &raw, SignalKind::FormatLocale, 2);
        }
    }

    #[cfg(target_os = "linux")]
    {
        probe_localectl(&mut acc);
        probe_locale_files(&mut acc);
        probe_timezone_unix(&mut acc);
        probe_runtime_keyboard_linux(&mut acc);
    }

    #[cfg(target_os = "macos")]
    {
        probe_macos(&mut acc);
        probe_timezone_unix(&mut acc);
    }

    #[cfg(target_os = "windows")]
    {
        probe_windows(&mut acc);
    }

    acc.finalize()
}

/// Flat list of language codes — compatibility shim for callers that
/// don't need scores (`fono_stt::factory`).
#[must_use]
pub fn detect_os_languages() -> Vec<String> {
    detect_user_languages_ranked().into_iter().map(|d| d.code).collect()
}

/// Render the ranked detection as a single user-friendly line for the
/// daemon startup banner, e.g.
/// `"Romanian (ro), English (en) — detected from formatting locale, keyboard layout, system locale, timezone"`.
/// Returns `None` when the list is empty (caller suppresses the line).
#[must_use]
pub fn format_detection_summary(detected: &[DetectedLanguage]) -> Option<String> {
    if detected.is_empty() {
        return None;
    }
    let langs: Vec<String> = detected
        .iter()
        .map(|d| format!("{} ({})", crate::languages::display_name(&d.code), d.code))
        .collect();
    // Union of kinds across all detected languages, first-seen order.
    let mut seen: Vec<SignalKind> = Vec::new();
    for d in detected {
        for k in &d.reasons {
            if !seen.contains(k) {
                seen.push(*k);
            }
        }
    }
    if seen.is_empty() {
        Some(langs.join(", "))
    } else {
        let reasons: Vec<&'static str> = seen.iter().map(|k| k.label()).collect();
        Some(format!("{} — detected from {}", langs.join(", "), reasons.join(", ")))
    }
}

// ─── Accumulator (dedup-by-kind) ────────────────────────────────────

/// Storage for the dedup-by-kind scoring. Each `(code, kind)` pair
/// tracks the maximum weight seen; the final score for a code is the
/// sum of max weights across all kinds it appeared in. First-seen
/// insertion order is tracked so reasons render deterministically.
#[derive(Default)]
struct Accumulator {
    weights: HashMap<(String, SignalKind), u8>,
    insertion: HashMap<(String, SignalKind), usize>,
    code_order: HashMap<String, usize>,
    next: usize,
}

impl Accumulator {
    fn push(&mut self, code: &str, kind: SignalKind, weight: u8) {
        let Some(lc) = normalize_lang(code) else {
            return;
        };
        let key = (lc.clone(), kind);
        // Max-merge the weight.
        let entry = self.weights.entry(key.clone()).or_insert(0);
        if weight > *entry {
            *entry = weight;
        }
        // Track first-seen ordinal for this (code, kind).
        if !self.insertion.contains_key(&key) {
            self.insertion.insert(key, self.next);
            self.next += 1;
        }
        // Track first-seen ordinal for the bare code.
        if !self.code_order.contains_key(&lc) {
            self.code_order.insert(lc, self.next);
            self.next += 1;
        }
    }

    fn push_country(&mut self, country: &str, kind: SignalKind, weight: u8) {
        for lang in country_to_langs(country) {
            self.push(lang, kind, weight);
        }
    }

    fn finalize(self) -> Vec<DetectedLanguage> {
        // Group `(code, kind)` entries by code.
        let mut by_code: HashMap<String, Vec<(SignalKind, u8, usize)>> = HashMap::new();
        for ((code, kind), weight) in &self.weights {
            let ord = self.insertion.get(&(code.clone(), *kind)).copied().unwrap_or(usize::MAX);
            by_code.entry(code.clone()).or_default().push((*kind, *weight, ord));
        }
        let mut out: Vec<DetectedLanguage> = by_code
            .into_iter()
            .map(|(code, mut kinds)| {
                kinds.sort_by_key(|(_, _, o)| *o);
                // Saturate at u8::MAX to be safe; in practice scores
                // are well below 30.
                let score: u8 = kinds
                    .iter()
                    .map(|(_, w, _)| u16::from(*w))
                    .sum::<u16>()
                    .min(u16::from(u8::MAX)) as u8;
                let reasons: Vec<SignalKind> = kinds.iter().map(|(k, _, _)| *k).collect();
                DetectedLanguage { code, score, reasons }
            })
            .collect();
        out.sort_by_key(|d| {
            (
                std::cmp::Reverse(d.score),
                self.code_order.get(&d.code).copied().unwrap_or(usize::MAX),
            )
        });
        out
    }
}

fn normalize_lang(code: &str) -> Option<String> {
    let lc = code.trim().to_ascii_lowercase();
    if lc.is_empty() || lc == "c" || lc == "posix" {
        return None;
    }
    // BCP-47 language subtags are 2–3 ASCII letters.
    if lc.len() < 2 || lc.len() > 3 || !lc.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(lc)
}

/// Parse a POSIX-style locale string and push its language + region
/// contributions through the accumulator under the given kind.
fn push_locale(acc: &mut Accumulator, raw: &str, kind: SignalKind, weight: u8) {
    let (lang, region) = parse_posix_locale_full(raw);
    if let Some(l) = lang {
        acc.push(&l, kind, weight);
    }
    if let Some(r) = region {
        acc.push_country(&r, kind, weight);
    }
}

// ─── Linux probes ───────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn probe_localectl(acc: &mut Accumulator) {
    let Ok(o) = Command::new("localectl").arg("status").output() else {
        return;
    };
    if !o.status.success() {
        return;
    }
    let s = String::from_utf8_lossy(&o.stdout);
    parse_localectl_block(&s, acc);
}

// The localectl block parser is exercised by cross-platform unit tests,
// so it stays compiled under `test` on every OS (pure text → signals).
#[cfg(any(target_os = "linux", test))]
fn parse_localectl_block(text: &str, acc: &mut Accumulator) {
    // `localectl status` prints System Locale as a block where
    // continuation lines surface (after `trim`) as bare `LC_*=…`.
    // Strip a leading `System Locale:` so the first line normalises
    // to the same shape as the continuations.
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("System Locale:").unwrap_or(line).trim();
        if let Some(rest) = line.strip_prefix("LANG=") {
            push_locale(acc, rest, SignalKind::SystemLang, 2);
        } else if let Some(rest) = line.strip_prefix("LC_ALL=") {
            push_locale(acc, rest, SignalKind::SystemLang, 2);
        } else if line.starts_with("LC_") {
            if let Some((_k, v)) = line.split_once('=') {
                push_locale(acc, v, SignalKind::FormatLocale, 2);
            }
        } else if let Some(rest) = line.strip_prefix("X11 Layout:") {
            for layout in rest.split([',', ' ']).filter(|t| !t.is_empty()) {
                if let Some(lang) = xkb_layout_to_lang(layout) {
                    acc.push(lang, SignalKind::Keyboard, 2);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn probe_locale_files(acc: &mut Accumulator) {
    // `/etc/locale.conf` is the systemd source of truth;
    // `/etc/default/locale` is the Debian/Ubuntu equivalent. Neither
    // is exported into the daemon's env under `systemd --user`, so
    // reading the file is the only way to see them. Missing file ⇒
    // signal silently skipped.
    for path in ["/etc/locale.conf", "/etc/default/locale"] {
        let Ok(contents) = std::fs::read_to_string(path) else {
            continue;
        };
        for (key, value) in parse_locale_kv(&contents) {
            match key.as_str() {
                "LANG" | "LC_ALL" => push_locale(acc, &value, SignalKind::SystemLang, 2),
                k if k.starts_with("LC_") => push_locale(acc, &value, SignalKind::FormatLocale, 2),
                _ => {}
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn probe_timezone_unix(acc: &mut Accumulator) {
    if let Some(country) = detect_system_timezone_country() {
        acc.push_country(&country, SignalKind::Timezone, 1);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn detect_system_timezone_country() -> Option<String> {
    // 1. Honour TZ env first (POSIX shells; portable).
    // 2. Fall back to `/etc/localtime` symlink target (Linux + macOS).
    let zone = match std::env::var("TZ") {
        Ok(s) if !s.is_empty() => {
            // POSIX `TZ` can be a bare IANA zone (`Europe/Bucharest`)
            // or a complex offset string (`EET-2EEST,M3.5.0/3,...`).
            // We only use the bare form; the offset form won't match
            // any `zone1970.tab` row and quietly contributes nothing.
            let s = s.trim().trim_start_matches(':').to_string();
            if s.is_empty() {
                return None;
            }
            s
        }
        _ => {
            // Symlink case (most systemd distros, macOS): `/etc/localtime`
            // points into `zoneinfo/`. Otherwise fall back to
            // `timedatectl show -p Timezone --value`, which works even
            // when `/etc/localtime` is a regular copy of the tzdata
            // binary (some Slackware-derived and embedded distros).
            if let Ok(target) = std::fs::read_link("/etc/localtime") {
                if let Some(z) = extract_iana_zone(&target.to_string_lossy()) {
                    z
                } else {
                    detect_zone_via_timedatectl()?
                }
            } else {
                detect_zone_via_timedatectl()?
            }
        }
    };
    for tab in [
        "/usr/share/zoneinfo/zone1970.tab",
        "/usr/share/zoneinfo/zone.tab",
        "/var/db/timezone/zoneinfo/zone1970.tab",
    ] {
        let Ok(contents) = std::fs::read_to_string(tab) else {
            continue;
        };
        if let Some(cc) = lookup_zone_in_tab(&contents, &zone) {
            return Some(cc.to_ascii_lowercase());
        }
    }
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn detect_zone_via_timedatectl() -> Option<String> {
    let out = std::process::Command::new("timedatectl")
        .args(["show", "-p", "Timezone", "--value"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let zone = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if zone.is_empty() {
        None
    } else {
        Some(zone)
    }
}

/// Strip the leading `/usr/share/zoneinfo/` (or equivalent) prefix off
/// a `/etc/localtime` symlink target. Falls back to the trailing two
/// path components.
fn extract_iana_zone(path: &str) -> Option<String> {
    if let Some(idx) = path.find("zoneinfo/") {
        let rest = path[idx + "zoneinfo/".len()..].trim_matches('/');
        if !rest.is_empty() {
            return Some(rest.to_string());
        }
    }
    let comps: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match comps.len() {
        0 => None,
        1 => Some(comps[0].to_string()),
        _ => Some(format!("{}/{}", comps[comps.len() - 2], comps[comps.len() - 1])),
    }
}

/// Look up an IANA zone name (`"Europe/Bucharest"`) in a `zone1970.tab`
/// or `zone.tab` body. Returns the first ISO 3166-1 alpha-2 country
/// code from column 0 (zone1970 lists multiple comma-separated codes
/// for shared zones — we take the first, which is the most-populous).
fn lookup_zone_in_tab(contents: &str, zone: &str) -> Option<String> {
    for line in contents.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() >= 3 && cols[2] == zone {
            return cols[0].split(',').next().map(str::to_string);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn probe_runtime_keyboard_linux(acc: &mut Accumulator) {
    // setxkbmap -query: live X11 / XWayland session layout. Catches
    // a runtime `setxkbmap ro` that doesn't touch any persistent
    // file — the case `localectl` cannot see.
    if let Ok(o) = Command::new("setxkbmap").arg("-query").output() {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            for layout in parse_setxkbmap_layouts(&s) {
                if let Some(lang) = xkb_layout_to_lang(&layout) {
                    acc.push(lang, SignalKind::Keyboard, 2);
                }
            }
        }
    }
    // GNOME / GTK input sources (Wayland-friendly).
    if let Ok(o) = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.input-sources", "sources"])
        .output()
    {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            for layout in parse_gsettings_input_sources(&s) {
                if let Some(lang) = xkb_layout_to_lang(&layout) {
                    acc.push(lang, SignalKind::Keyboard, 2);
                }
            }
        }
    }
    // KDE Plasma: `~/.config/kxkbrc`, key `LayoutList`.
    if let Ok(home) = std::env::var("HOME") {
        let path = format!("{home}/.config/kxkbrc");
        if let Ok(contents) = std::fs::read_to_string(&path) {
            for layout in parse_kxkbrc_layouts(&contents) {
                if let Some(lang) = xkb_layout_to_lang(&layout) {
                    acc.push(lang, SignalKind::Keyboard, 2);
                }
            }
        }
    }
}

// The XKB/locale parse helpers below are only *called* from the Linux
// probes, but they are pure text functions with cross-platform unit
// tests — hence `any(linux, test)` rather than `linux` (keeps the tests
// running on darwin/windows without `allow(dead_code)`).
#[cfg(any(target_os = "linux", test))]
fn parse_setxkbmap_layouts(text: &str) -> Vec<String> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("layout:") {
            return rest.split([',', ' ']).filter(|t| !t.is_empty()).map(str::to_string).collect();
        }
    }
    Vec::new()
}

#[cfg(any(target_os = "linux", test))]
fn parse_gsettings_input_sources(text: &str) -> Vec<String> {
    // gsettings prints e.g.
    //   [('xkb', 'us'), ('xkb', 'ro+std'), ('ibus', 'pinyin')]
    // We extract the xkb codes only; IBus / fcitx engine names would
    // need a separate map and aren't worth the size budget.
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(idx) = rest.find("('xkb', '") {
        rest = &rest[idx + "('xkb', '".len()..];
        if let Some(end) = rest.find('\'') {
            // `ro+std` → keep only the layout before `+` (variant).
            let token = rest[..end].split('+').next().unwrap_or("");
            if !token.is_empty() {
                out.push(token.to_string());
            }
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    out
}

#[cfg(any(target_os = "linux", test))]
fn parse_kxkbrc_layouts(text: &str) -> Vec<String> {
    let mut in_layout_section = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_layout_section = line.eq_ignore_ascii_case("[Layout]");
            continue;
        }
        // KDE writes the key both inside and outside the `[Layout]`
        // section depending on version; accept either.
        if let Some(rest) = line.strip_prefix("LayoutList=") {
            // KDE accepts the key inside `[Layout]` or at the top
            // of the file depending on version — both are valid.
            let _ = in_layout_section;
            for tok in rest.split(',').filter(|s| !s.is_empty()) {
                out.push(tok.trim().to_string());
            }
        }
    }
    out
}

/// Parse a `KEY=VALUE` config file (POSIX shell-ish) used by
/// `/etc/locale.conf`, `/etc/default/locale`, and similar files.
/// Quoted values have their wrapping `"` / `'` stripped. Comment
/// lines (`#`) and blanks are skipped. Returns an empty `Vec` on
/// garbage input — never panics.
#[cfg(any(target_os = "linux", test))]
fn parse_locale_kv(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().to_string();
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        if k.is_empty() || v.is_empty() {
            continue;
        }
        out.push((k, v));
    }
    out
}

// ─── macOS probes ───────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn probe_macos(acc: &mut Accumulator) {
    if let Ok(o) = Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output() {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            let mut position: usize = 0;
            for line in s.lines() {
                let trimmed = line.trim().trim_matches(|c: char| c == '"' || c == ',');
                if trimmed.is_empty() || trimmed == "(" || trimmed == ")" {
                    continue;
                }
                let weight = if position == 0 { 3 } else { 2 };
                push_locale(acc, trimmed, SignalKind::UiList, weight);
                position += 1;
            }
        }
    }
    if let Ok(o) = Command::new("defaults").args(["read", "-g", "AppleLocale"]).output() {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            push_locale(acc, s.trim(), SignalKind::SystemLang, 2);
        }
    }
    if let Ok(o) = Command::new("defaults")
        .args(["read", "com.apple.HIToolbox", "AppleEnabledInputSources"])
        .output()
    {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            for line in s.lines() {
                if let Some(name) = line
                    .trim()
                    .strip_prefix("KeyboardLayout Name = ")
                    .and_then(|v| v.strip_suffix(';'))
                {
                    if let Some(lang) = apple_kbd_name_to_lang(name) {
                        acc.push(lang, SignalKind::Keyboard, 2);
                    }
                }
            }
        }
    }
}

// ─── Windows probes ─────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn probe_windows(acc: &mut Accumulator) {
    let Ok(o) = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-WinUserLanguageList | ForEach-Object LanguageTag; \
             Write-Output '---'; \
             (Get-WinSystemLocale).Name; \
             Write-Output '---'; \
             (Get-WinHomeLocation).HomeLocation",
        ])
        .output()
    else {
        return;
    };
    if !o.status.success() {
        return;
    }
    let s = String::from_utf8_lossy(&o.stdout);
    let mut section: u8 = 0;
    let mut position: usize = 0;
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "---" {
            section += 1;
            position = 0;
            continue;
        }
        match section {
            0 => {
                let weight = if position == 0 { 3 } else { 2 };
                push_locale(acc, trimmed, SignalKind::UiList, weight);
                position += 1;
            }
            1 => push_locale(acc, trimmed, SignalKind::SystemLang, 2),
            _ => acc.push_country(trimmed, SignalKind::HomeRegion, 3),
        }
    }
}

// ─── POSIX parsing helpers ──────────────────────────────────────────

/// Test-only sugar over [`parse_posix_locale_full`] returning just the
/// language subtag.
#[cfg(test)]
fn parse_posix_locale(raw: &str) -> Option<String> {
    parse_posix_locale_full(raw).0
}

/// Parse a POSIX-style locale tag into `(language, region)` lowercase
/// pair. `region` is the country code from the `_YY` / `-YY` suffix,
/// or `None` when the tag is plain (`"ro"`, `"en"`).
fn parse_posix_locale_full(raw: &str) -> (Option<String>, Option<String>) {
    let trimmed = raw.trim().trim_matches('"');
    if trimmed.is_empty() {
        return (None, None);
    }
    let Some(head) = trimmed.split(['.', '@']).next() else {
        return (None, None);
    };
    let mut parts = head.split(['_', '-']);
    let lang = parts.next().map(|s| s.trim().to_ascii_lowercase());
    let region = parts.next().map(|s| s.trim().to_ascii_lowercase());

    let lang = lang.filter(|lc| {
        !lc.is_empty()
            && lc != "c"
            && lc != "posix"
            && (2..=3).contains(&lc.len())
            && lc.chars().all(|c| c.is_ascii_alphabetic())
    });
    let region = region.filter(|r| r.len() == 2 && r.chars().all(|c| c.is_ascii_alphabetic()));
    (lang, region)
}

// ─── Static tables ──────────────────────────────────────────────────

/// Country (ISO 3166-1 alpha-2, lowercase) → predominant language(s).
/// Curated to cover the languages in `fono_core::languages` plus a few
/// multilingual hot-spots; missing entries contribute no region signal.
const COUNTRY_LANGS: &[(&str, &[&str])] = &[
    ("ar", &["es"]),
    ("at", &["de"]),
    ("au", &["en"]),
    ("be", &["nl", "fr"]),
    ("bg", &["bg"]),
    ("bo", &["es"]),
    ("br", &["pt"]),
    ("ca", &["en", "fr"]),
    ("ch", &["de", "fr", "it"]),
    ("cl", &["es"]),
    ("cn", &["zh"]),
    ("co", &["es"]),
    ("cz", &["cs"]),
    ("de", &["de"]),
    ("dk", &["da"]),
    ("ec", &["es"]),
    ("eg", &["ar"]),
    ("es", &["es"]),
    ("fi", &["fi"]),
    ("fr", &["fr"]),
    ("gb", &["en"]),
    ("gr", &["el"]),
    ("hk", &["zh", "en"]),
    ("hu", &["hu"]),
    ("id", &["id"]),
    ("ie", &["en"]),
    ("il", &["he"]),
    ("in", &["hi", "en"]),
    ("ir", &["fa"]),
    ("it", &["it"]),
    ("jp", &["ja"]),
    ("kr", &["ko"]),
    ("lu", &["fr", "de"]),
    ("mx", &["es"]),
    ("nl", &["nl"]),
    ("no", &["nb"]),
    ("nz", &["en"]),
    ("pe", &["es"]),
    ("ph", &["en"]),
    ("pl", &["pl"]),
    ("pt", &["pt"]),
    ("ro", &["ro"]),
    ("ru", &["ru"]),
    ("sa", &["ar"]),
    ("se", &["sv"]),
    ("sg", &["en", "zh"]),
    ("sk", &["sk"]),
    ("th", &["th"]),
    ("tr", &["tr"]),
    ("tw", &["zh"]),
    ("ua", &["uk", "ru"]),
    ("uk", &["en"]),
    ("us", &["en"]),
    ("ve", &["es"]),
    ("vn", &["vi"]),
    ("za", &["en"]),
];

fn country_to_langs(country_code: &str) -> &'static [&'static str] {
    let cc = country_code.to_ascii_lowercase();
    COUNTRY_LANGS.iter().find(|(c, _)| *c == cc).map_or(&[][..], |(_, langs)| *langs)
}

/// X11/xkb layout code → predominant language. Layout variants in
/// parentheses (e.g. `"us(intl)"`) and `+`-suffix forms
/// (`"ro+std"`) are stripped before lookup.
#[cfg(any(target_os = "linux", test))]
const XKB_LAYOUT_LANGS: &[(&str, &str)] = &[
    ("us", "en"),
    ("gb", "en"),
    ("uk", "en"),
    ("ca", "en"),
    ("au", "en"),
    ("ie", "en"),
    ("nz", "en"),
    ("ro", "ro"),
    ("de", "de"),
    ("ch", "de"),
    ("at", "de"),
    ("fr", "fr"),
    ("be", "fr"),
    ("it", "it"),
    ("es", "es"),
    ("latam", "es"),
    ("pt", "pt"),
    ("br", "pt"),
    ("nl", "nl"),
    ("pl", "pl"),
    ("ru", "ru"),
    ("ua", "uk"),
    ("tr", "tr"),
    ("cn", "zh"),
    ("jp", "ja"),
    ("kr", "ko"),
    ("in", "hi"),
    ("ara", "ar"),
    ("il", "he"),
    ("cz", "cs"),
    ("sk", "sk"),
    ("hu", "hu"),
    ("gr", "el"),
    ("bg", "bg"),
    ("se", "sv"),
    ("no", "nb"),
    ("dk", "da"),
    ("fi", "fi"),
    ("th", "th"),
    ("vn", "vi"),
    ("id", "id"),
    ("ir", "fa"),
];

#[cfg(any(target_os = "linux", test))]
fn xkb_layout_to_lang(layout: &str) -> Option<&'static str> {
    // Strip variant parenthetical / `+`-suffix: `us(intl)` or
    // `ro+std` → `us` / `ro`.
    let base = layout.split(['(', '+']).next().unwrap_or(layout).trim().to_ascii_lowercase();
    if base.is_empty() {
        return None;
    }
    XKB_LAYOUT_LANGS.iter().find(|(k, _)| *k == base).map(|(_, v)| *v)
}

/// macOS `AppleEnabledInputSources` `KeyboardLayout Name` → language.
#[cfg(target_os = "macos")]
fn apple_kbd_name_to_lang(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let table: &[(&str, &str)] = &[
        ("u.s.", "en"),
        ("us", "en"),
        ("british", "en"),
        ("australian", "en"),
        ("canadian", "en"),
        ("irish", "en"),
        ("german", "de"),
        ("swiss german", "de"),
        ("austrian", "de"),
        ("french", "fr"),
        ("canadian french", "fr"),
        ("italian", "it"),
        ("spanish", "es"),
        ("spanish - iso", "es"),
        ("latin american", "es"),
        ("portuguese", "pt"),
        ("brazilian", "pt"),
        ("dutch", "nl"),
        ("polish", "pl"),
        ("russian", "ru"),
        ("ukrainian", "uk"),
        ("romanian", "ro"),
        ("czech", "cs"),
        ("slovak", "sk"),
        ("hungarian", "hu"),
        ("greek", "el"),
        ("bulgarian", "bg"),
        ("swedish", "sv"),
        ("norwegian", "nb"),
        ("danish", "da"),
        ("finnish", "fi"),
        ("turkish", "tr"),
        ("thai", "th"),
        ("vietnamese", "vi"),
        ("indonesian", "id"),
        ("persian", "fa"),
        ("arabic", "ar"),
        ("hebrew", "he"),
        ("japanese", "ja"),
        ("korean", "ko"),
        ("chinese", "zh"),
        ("pinyin", "zh"),
    ];
    table.iter().find(|(k, _)| n.contains(k)).map(|(_, v)| *v)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── parsers ────────────────────────────────────────────────

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

    #[test]
    fn parse_full_extracts_region() {
        let (l, r) = parse_posix_locale_full("ro_RO.UTF-8");
        assert_eq!(l.as_deref(), Some("ro"));
        assert_eq!(r.as_deref(), Some("ro"));

        let (l, r) = parse_posix_locale_full("en_US");
        assert_eq!(l.as_deref(), Some("en"));
        assert_eq!(r.as_deref(), Some("us"));

        let (l, r) = parse_posix_locale_full("en");
        assert_eq!(l.as_deref(), Some("en"));
        assert_eq!(r, None);

        let (l, r) = parse_posix_locale_full("pt-BR");
        assert_eq!(l.as_deref(), Some("pt"));
        assert_eq!(r.as_deref(), Some("br"));
    }

    #[test]
    fn country_lookup_covers_curated_languages() {
        assert_eq!(country_to_langs("RO"), &["ro"]);
        assert_eq!(country_to_langs("us"), &["en"]);
        assert_eq!(country_to_langs("ch"), &["de", "fr", "it"]);
        assert_eq!(country_to_langs("zz"), &[] as &[&str]);
    }

    #[test]
    fn xkb_lookup_handles_variants_and_plus_suffix() {
        assert_eq!(xkb_layout_to_lang("us"), Some("en"));
        assert_eq!(xkb_layout_to_lang("us(intl)"), Some("en"));
        assert_eq!(xkb_layout_to_lang("ro"), Some("ro"));
        assert_eq!(xkb_layout_to_lang("ro+std"), Some("ro"));
        assert_eq!(xkb_layout_to_lang("latam"), Some("es"));
        assert_eq!(xkb_layout_to_lang(""), None);
        assert_eq!(xkb_layout_to_lang("zz"), None);
    }

    // ─── locale.conf KV parser ──────────────────────────────────

    #[test]
    fn locale_kv_parses_quoted_and_unquoted_values() {
        let s = r#"
# system locale
LANG=en_US.UTF-8
LC_TIME="ro_RO.UTF-8"
LC_NUMERIC='ro_RO.UTF-8'
"#;
        let kv = parse_locale_kv(s);
        assert_eq!(
            kv,
            vec![
                ("LANG".to_string(), "en_US.UTF-8".to_string()),
                ("LC_TIME".to_string(), "ro_RO.UTF-8".to_string()),
                ("LC_NUMERIC".to_string(), "ro_RO.UTF-8".to_string()),
            ]
        );
    }

    #[test]
    fn locale_kv_handles_empty_and_garbage() {
        assert!(parse_locale_kv("").is_empty());
        assert!(parse_locale_kv("   \n\n  ").is_empty());
        assert!(parse_locale_kv("# only comments\n# more").is_empty());
        // No `=` ⇒ skip; empty key/value ⇒ skip. No panic.
        assert!(parse_locale_kv("no equals here\nALSO_NO_EQUALS").is_empty());
        assert!(parse_locale_kv("=value\nKEY=").is_empty());
    }

    // ─── localectl multi-line parser ────────────────────────────

    #[test]
    fn localectl_continuation_lines_picked_up() {
        // Simulates the canonical "LANG=en_US but LC_* are ro_RO"
        // case — the user's failing setup. The continuation lines
        // start with whitespace; after `trim` they surface as bare
        // `LC_*=…`.
        let fixture = "\
   System Locale: LANG=en_US.UTF-8
                  LC_MESSAGES=ro_RO.UTF-8
                  LC_TIME=ro_RO.UTF-8
                  LC_NUMERIC=ro_RO.UTF-8
       VC Keymap: us
      X11 Layout: us
";
        let mut acc = Accumulator::default();
        // Wire the parser only on Linux builds.
        #[cfg(target_os = "linux")]
        parse_localectl_block(fixture, &mut acc);
        #[cfg(not(target_os = "linux"))]
        {
            // Emulate the parser on non-Linux by reusing
            // push_locale directly so the test still has value.
            for line in fixture.lines() {
                let line = line.trim();
                let line = line.strip_prefix("System Locale:").unwrap_or(line).trim();
                if let Some(rest) = line.strip_prefix("LANG=") {
                    push_locale(&mut acc, rest, SignalKind::SystemLang, 2);
                } else if line.starts_with("LC_") {
                    if let Some((_, v)) = line.split_once('=') {
                        push_locale(&mut acc, v, SignalKind::FormatLocale, 2);
                    }
                }
            }
        }
        let out = acc.finalize();
        let ro = out.iter().find(|d| d.code == "ro").expect("ro detected");
        assert!(ro.reasons.contains(&SignalKind::FormatLocale));
        let en = out.iter().find(|d| d.code == "en").expect("en detected");
        assert!(en.reasons.contains(&SignalKind::SystemLang));
    }

    // ─── timezone tab lookup ────────────────────────────────────

    #[test]
    fn zone1970_lookup_finds_country_and_handles_missing() {
        let fixture = "\
# comment
RO\t+4426+02606\tEurope/Bucharest
US\t+340308-1181434\tAmerica/Los_Angeles\tPacific
CH,DE,LI\t+4723+00832\tEurope/Zurich\tSwiss time
";
        assert_eq!(lookup_zone_in_tab(fixture, "Europe/Bucharest").as_deref(), Some("RO"));
        // Shared zone: take the first country.
        assert_eq!(lookup_zone_in_tab(fixture, "Europe/Zurich").as_deref(), Some("CH"));
        // Missing zone ⇒ None, no panic.
        assert_eq!(lookup_zone_in_tab(fixture, "Mars/Olympus"), None);
        // Empty / comments-only ⇒ None.
        assert_eq!(lookup_zone_in_tab("", "Europe/Bucharest"), None);
        assert_eq!(lookup_zone_in_tab("# only comments", "Europe/Bucharest"), None);
    }

    #[test]
    fn extract_iana_zone_handles_common_symlink_targets() {
        assert_eq!(
            extract_iana_zone("/usr/share/zoneinfo/Europe/Bucharest").as_deref(),
            Some("Europe/Bucharest")
        );
        assert_eq!(
            extract_iana_zone("/var/db/timezone/zoneinfo/Europe/Bucharest").as_deref(),
            Some("Europe/Bucharest")
        );
        assert_eq!(extract_iana_zone("Europe/Bucharest").as_deref(), Some("Europe/Bucharest"));
        // No `zoneinfo/` segment ⇒ falls back to last 2 components.
        assert_eq!(extract_iana_zone("/etc/localtime").as_deref(), Some("etc/localtime"));
    }

    // ─── runtime keyboard parsers ───────────────────────────────

    #[test]
    fn setxkbmap_query_parses_multi_layout() {
        let fixture = "\
rules:      evdev
model:      pc105
layout:     us,ro
variant:    ,
options:    grp:alt_shift_toggle
";
        let layouts = parse_setxkbmap_layouts(fixture);
        assert_eq!(layouts, vec!["us".to_string(), "ro".to_string()]);
    }

    #[test]
    fn gsettings_input_sources_parser_extracts_xkb_codes() {
        let fixture = "[('xkb', 'us'), ('xkb', 'ro+std'), ('ibus', 'pinyin')]";
        let layouts = parse_gsettings_input_sources(fixture);
        assert_eq!(layouts, vec!["us".to_string(), "ro".to_string()]);
    }

    #[test]
    fn gsettings_empty_or_garbage_returns_empty() {
        assert!(parse_gsettings_input_sources("").is_empty());
        assert!(parse_gsettings_input_sources("garbage").is_empty());
        assert!(parse_gsettings_input_sources("[('xkb', '").is_empty());
    }

    #[test]
    fn kxkbrc_parses_layout_list() {
        let fixture = "[Layout]\nLayoutList=us,ro\nUse=true\n";
        let layouts = parse_kxkbrc_layouts(fixture);
        assert_eq!(layouts, vec!["us".to_string(), "ro".to_string()]);
        // Empty / missing key ⇒ no panic.
        assert!(parse_kxkbrc_layouts("").is_empty());
        assert!(parse_kxkbrc_layouts("[OtherSection]\nfoo=bar").is_empty());
    }

    // ─── dedup-by-kind ──────────────────────────────────────────

    #[test]
    fn dedup_collapses_same_kind_from_multiple_sources() {
        // Same `SystemLang` contribution at weight 2 from three
        // different sources (env, localectl, locale.conf) must
        // count once.
        let mut acc = Accumulator::default();
        acc.push("en", SignalKind::SystemLang, 2);
        acc.push("en", SignalKind::SystemLang, 2);
        acc.push("en", SignalKind::SystemLang, 2);
        let out = acc.finalize();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].score, 2, "same kind must not double-count");
        assert_eq!(out[0].reasons, vec![SignalKind::SystemLang]);
    }

    #[test]
    fn dedup_keeps_max_weight_across_sources() {
        // Three sources push the same `(en, SystemLang)` at
        // different weights; max wins.
        let mut acc = Accumulator::default();
        acc.push("en", SignalKind::SystemLang, 1);
        acc.push("en", SignalKind::SystemLang, 3);
        acc.push("en", SignalKind::SystemLang, 2);
        let out = acc.finalize();
        assert_eq!(out[0].score, 3);
    }

    #[test]
    fn different_kinds_for_same_code_sum() {
        let mut acc = Accumulator::default();
        acc.push("ro", SignalKind::FormatLocale, 2);
        acc.push("ro", SignalKind::Keyboard, 2);
        acc.push("ro", SignalKind::Timezone, 1);
        let out = acc.finalize();
        assert_eq!(out[0].code, "ro");
        assert_eq!(out[0].score, 5);
        assert_eq!(
            out[0].reasons,
            vec![SignalKind::FormatLocale, SignalKind::Keyboard, SignalKind::Timezone,]
        );
    }

    #[test]
    fn ranked_users_real_scenario_lang_en_us_ro_otherwise() {
        // Simulate the user's NimbleX box exactly:
        //   env LANG=en_US.UTF-8
        //   /etc/locale.conf LC_MESSAGES=LC_TIME=LC_NUMERIC=ro_RO
        //   /etc/localtime -> Europe/Bucharest (RO)
        //   setxkbmap -query layout: us,ro
        let mut acc = Accumulator::default();

        // env LANG
        push_locale(&mut acc, "en_US.UTF-8", SignalKind::SystemLang, 2);
        // locale.conf LANG (same kind, dedupes)
        push_locale(&mut acc, "en_US.UTF-8", SignalKind::SystemLang, 2);
        // locale.conf LC_* (FormatLocale)
        push_locale(&mut acc, "ro_RO.UTF-8", SignalKind::FormatLocale, 2);
        push_locale(&mut acc, "ro_RO.UTF-8", SignalKind::FormatLocale, 2);
        push_locale(&mut acc, "ro_RO.UTF-8", SignalKind::FormatLocale, 2);
        // timezone
        acc.push_country("ro", SignalKind::Timezone, 1);
        // keyboard us,ro (the runtime switch)
        acc.push("en", SignalKind::Keyboard, 2);
        acc.push("ro", SignalKind::Keyboard, 2);

        let out = acc.finalize();
        let ro = out.iter().find(|d| d.code == "ro").expect("ro detected");
        let en = out.iter().find(|d| d.code == "en").expect("en detected");
        // ro: FormatLocale 2 + Keyboard 2 + Timezone 1 = 5
        // en: SystemLang 2 + Keyboard 2 = 4
        assert_eq!(ro.score, 5);
        assert_eq!(en.score, 4);
        // Both meet the score ≥ 1 allow-list threshold.
        assert!(ro.score >= 1 && en.score >= 1);
        // Ranked: ro first.
        assert_eq!(out[0].code, "ro");
        assert_eq!(out[1].code, "en");
    }

    // ─── friendly banner ────────────────────────────────────────

    #[test]
    fn format_summary_uses_display_names_and_friendly_labels() {
        let detected = vec![
            DetectedLanguage {
                code: "ro".into(),
                score: 5,
                reasons: vec![SignalKind::FormatLocale, SignalKind::Keyboard, SignalKind::Timezone],
            },
            DetectedLanguage {
                code: "en".into(),
                score: 4,
                reasons: vec![SignalKind::SystemLang, SignalKind::Keyboard],
            },
        ];
        let s = format_detection_summary(&detected).unwrap();
        assert_eq!(
            s,
            "Romanian (ro), English (en) — detected from \
             formatting locale, keyboard layout, timezone, system locale"
        );
    }

    #[test]
    fn format_summary_none_when_empty() {
        assert_eq!(format_detection_summary(&[]), None);
    }

    #[test]
    fn format_summary_handles_no_reasons() {
        // Pathological: a code with no reasons. The function must
        // still render the language list rather than crashing or
        // emitting a dangling " — detected from " tail.
        let detected = vec![DetectedLanguage { code: "ro".into(), score: 0, reasons: vec![] }];
        assert_eq!(format_detection_summary(&detected).as_deref(), Some("Romanian (ro)"));
    }
}
