// SPDX-License-Identifier: GPL-3.0-only
//! Error types for `fono-core`.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse TOML at {path:?}: {source}")]
    TomlParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to serialize TOML: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("secrets file {path:?} is world- or group-readable; chmod 600 it before continuing")]
    SecretsPermissions { path: PathBuf },

    #[error("config version {found} is newer than supported ({supported})")]
    ConfigVersionTooNew { found: u32, supported: u32 },

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
