// SPDX-License-Identifier: GPL-3.0-only
//! XDG-compliant path resolver for Fono.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Name used as the leaf directory under each XDG root.
pub const APP_NAME: &str = "fono";

/// Absolute path of the IPC socket used by the headless system service
/// (`packaging/assets/fono.service` — runs as user `fono` with
/// `XDG_STATE_HOME=/var/lib`). CLI clients try this path first so a
/// root or fono-group user shell can drive the system-installed daemon
/// without needing `XDG_STATE_HOME` overrides.
pub const SYSTEM_IPC_SOCKET: &str = "/var/lib/fono/fono.sock";

/// Resolved absolute paths for every file Fono touches.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl Paths {
    /// Resolve from environment, falling back to HOME-based defaults.
    pub fn resolve() -> Result<Self> {
        let home = home_dir()?;
        Ok(Self {
            config_dir: xdg_root("XDG_CONFIG_HOME", &home.join(".config")).join(APP_NAME),
            data_dir: xdg_root("XDG_DATA_HOME", &home.join(".local/share")).join(APP_NAME),
            cache_dir: xdg_root("XDG_CACHE_HOME", &home.join(".cache")).join(APP_NAME),
            state_dir: xdg_root("XDG_STATE_HOME", &home.join(".local/state")).join(APP_NAME),
        })
    }

    /// Create every directory if it does not exist.
    pub fn ensure(&self) -> Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.state_dir,
            &self.whisper_models_dir(),
            &self.llm_models_dir(),
            &self.sherpa_models_dir(),
        ] {
            std::fs::create_dir_all(dir).map_err(|source| Error::Io {
                path: dir.clone(),
                source,
            })?;
        }
        Ok(())
    }

    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    #[must_use]
    pub fn secrets_file(&self) -> PathBuf {
        self.config_dir.join("secrets.toml")
    }

    #[must_use]
    pub fn history_db(&self) -> PathBuf {
        self.data_dir.join("history.sqlite")
    }

    #[must_use]
    pub fn notes_db(&self) -> PathBuf {
        self.data_dir.join("notes.sqlite")
    }

    #[must_use]
    pub fn whisper_models_dir(&self) -> PathBuf {
        self.cache_dir.join("models").join("whisper")
    }

    #[must_use]
    pub fn llm_models_dir(&self) -> PathBuf {
        self.cache_dir.join("models").join("llm")
    }

    #[must_use]
    pub fn sherpa_models_dir(&self) -> PathBuf {
        self.cache_dir.join("models").join("sherpa")
    }

    #[must_use]
    pub fn ipc_socket(&self) -> PathBuf {
        self.state_dir.join("fono.sock")
    }

    /// Ordered list of socket paths a CLI client should attempt when
    /// dialling a running daemon. The system-service socket
    /// (`/var/lib/fono/fono.sock`) is tried first so a root shell or
    /// `fono`-group member can drive the headless system unit without
    /// `XDG_STATE_HOME` gymnastics; the per-user XDG socket
    /// ([`Self::ipc_socket`]) is the fallback for standalone /
    /// `systemctl --user` deployments. Deduped when the resolved user
    /// path matches [`SYSTEM_IPC_SOCKET`] (e.g. the daemon itself
    /// running as user `fono` with the same XDG layout).
    #[must_use]
    pub fn client_ipc_socket_candidates(&self) -> Vec<PathBuf> {
        let user = self.ipc_socket();
        let system = PathBuf::from(SYSTEM_IPC_SOCKET);
        if user == system {
            vec![user]
        } else {
            vec![system, user]
        }
    }

    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.state_dir.join("fono.log")
    }

    #[must_use]
    pub fn pid_file(&self) -> PathBuf {
        self.state_dir.join("fono.pid")
    }

    /// Construct `Paths` rooted under a specific directory (for tests and
    /// `HOME=/tmp/fresh-user` integration runs).
    #[must_use]
    pub fn rooted_at(root: &Path) -> Self {
        Self {
            config_dir: root.join("config").join(APP_NAME),
            data_dir: root.join("data").join(APP_NAME),
            cache_dir: root.join("cache").join(APP_NAME),
            state_dir: root.join("state").join(APP_NAME),
        }
    }
}

fn xdg_root(var: &str, fallback: &Path) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => fallback.to_path_buf(),
    }
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| Error::Other("HOME environment variable not set".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_at_produces_expected_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::rooted_at(tmp.path());
        assert!(p.config_file().ends_with("config/fono/config.toml"));
        assert!(p.history_db().ends_with("data/fono/history.sqlite"));
        assert!(p.ipc_socket().ends_with("state/fono/fono.sock"));
        p.ensure().unwrap();
        assert!(p.whisper_models_dir().is_dir());
    }
}
