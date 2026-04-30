// SPDX-License-Identifier: GPL-3.0-only
//! TXT record helpers shared by the browser + advertiser.
//!
//! The Fono mDNS profile uses a small fixed set of TXT keys so peers
//! can be classified, labelled, and connected to without a side
//! channel:
//!
//! | key       | example          | meaning                                 |
//! |-----------|------------------|-----------------------------------------|
//! | `proto`   | `wyoming/1`      | protocol family + revision              |
//! | `version` | `0.4.0`          | server software version (diagnostic)    |
//! | `caps`    | `stt,llm`        | comma-separated capability tags         |
//! | `model`   | `whisper-small`  | primary model hint (Wyoming only)       |
//! | `auth`    | `token` / `none` | does the server require a bearer?       |
//! | `path`    | `/fono/v1`       | WebSocket path (Fono-native only)       |
//!
//! Unknown keys are silently ignored — peers from outside Fono (e.g.
//! a stock wyoming-faster-whisper that publishes only `name`) still
//! resolve cleanly, just with sparser metadata.

/// Canonical TXT keys understood by the browser. Listed once here so
/// the advertiser cannot accidentally publish a key the browser will
/// drop.
pub const KEY_PROTO: &str = "proto";
pub const KEY_VERSION: &str = "version";
pub const KEY_CAPS: &str = "caps";
pub const KEY_MODEL: &str = "model";
pub const KEY_AUTH: &str = "auth";
pub const KEY_PATH: &str = "path";

/// Split a comma-separated `caps` value into a normalised vec, with
/// whitespace trimmed and empty entries dropped. The order is
/// preserved so the tray menu can show a stable sequence.
#[must_use]
pub fn parse_caps(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Inverse of [`parse_caps`] — joins a slice for the TXT record.
#[must_use]
pub fn format_caps<S: AsRef<str>>(caps: &[S]) -> String {
    caps.iter()
        .map(|c| c.as_ref().trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

/// `auth` key normaliser: `Some(true)` for `"token"` /`"required"`,
/// `Some(false)` for `"none"`, `None` for unknown values.
#[must_use]
pub fn parse_auth(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "token" | "required" | "true" | "yes" => Some(true),
        "none" | "false" | "no" | "" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_round_trip() {
        let caps = vec!["stt", "llm", "history"];
        let raw = format_caps(&caps);
        assert_eq!(raw, "stt,llm,history");
        assert_eq!(parse_caps(&raw), caps);
    }

    #[test]
    fn caps_tolerates_whitespace_and_empty() {
        assert_eq!(parse_caps(" stt , , llm "), vec!["stt", "llm"]);
        assert!(parse_caps("").is_empty());
    }

    #[test]
    fn auth_parser_known() {
        assert_eq!(parse_auth("token"), Some(true));
        assert_eq!(parse_auth("REQUIRED"), Some(true));
        assert_eq!(parse_auth("none"), Some(false));
        assert_eq!(parse_auth(""), Some(false));
    }

    #[test]
    fn auth_parser_unknown_returns_none() {
        assert_eq!(parse_auth("oauth2"), None);
    }
}
