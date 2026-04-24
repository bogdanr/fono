// SPDX-License-Identifier: GPL-3.0-only
//! Secrets loader for API keys. Stored separately from [`crate::Config`] in
//! `~/.config/fono/secrets.toml` at mode 0600.
//!
//! Keys are read by *reference* (`api_key_ref = "GROQ_API_KEY"`). The
//! reference is resolved as:
//! 1. If the key exists in `secrets.toml`'s `[keys]` table, use it.
//! 2. Otherwise, read the environment variable of that name.
//!
//! This lets CI and container users avoid on-disk secrets entirely.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::atomic_write;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Secrets {
    #[serde(default)]
    pub keys: HashMap<String, String>,
}

impl Secrets {
    /// Load from disk. If missing, returns empty secrets.
    ///
    /// On Unix, refuses to read the file if it is group- or world-readable.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::metadata(path) {
            Ok(md) => {
                check_mode(path, &md)?;
                let raw = std::fs::read_to_string(path).map_err(|source| Error::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                let secrets: Self = toml::from_str(&raw).map_err(|source| Error::TomlParse {
                    path: path.to_path_buf(),
                    source,
                })?;
                Ok(secrets)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Atomic write at mode 0600.
    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = toml::to_string_pretty(self)?;
        atomic_write(path, raw.as_bytes(), 0o600)
    }

    /// Look up `name` first in the in-memory table, falling back to the
    /// process environment. Returns `None` if neither has it.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<String> {
        if let Some(v) = self.keys.get(name) {
            return Some(v.clone());
        }
        std::env::var(name).ok()
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.keys.insert(name.into(), value.into());
    }
}

#[cfg(unix)]
fn check_mode(path: &Path, md: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    // Reject any group/other bits.
    if md.mode() & 0o077 != 0 {
        return Err(Error::SecretsPermissions {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_mode(_: &Path, _: &std::fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secrets.toml");
        let mut s = Secrets::default();
        s.insert("GROQ_API_KEY", "sk-test-123");
        s.save(&path).unwrap();
        let loaded = Secrets::load(&path).unwrap();
        assert_eq!(
            loaded.resolve("GROQ_API_KEY").as_deref(),
            Some("sk-test-123")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secrets.toml");
        std::fs::write(&path, "[keys]\nX = \"y\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            Secrets::load(&path),
            Err(Error::SecretsPermissions { .. })
        ));
    }

    #[test]
    fn env_fallback() {
        let s = Secrets::default();
        // SAFETY: unique env var name.
        std::env::set_var("FONO_TEST_KEY_XYZ", "env-val");
        assert_eq!(s.resolve("FONO_TEST_KEY_XYZ").as_deref(), Some("env-val"));
        std::env::remove_var("FONO_TEST_KEY_XYZ");
    }
}
