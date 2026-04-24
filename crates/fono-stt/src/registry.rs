// SPDX-License-Identifier: GPL-3.0-only
//! Registry of whisper model variants with pinned SHA256 hashes and HF URLs.
//! `FONO_MODEL_MIRROR` env var overrides the host at download time.

#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    pub name: &'static str,
    pub multilingual: bool,
    pub approx_mb: u32,
    /// HuggingFace path, e.g. `ggerganov/whisper.cpp/resolve/main/ggml-small.bin`.
    pub url_path: &'static str,
    pub sha256: &'static str,
}

/// Default HuggingFace host; override via `FONO_MODEL_MIRROR`.
pub const DEFAULT_MIRROR: &str = "https://huggingface.co";

/// All supported whisper variants (Phase 4 Task 4.4).
///
/// SHA256s are the upstream ggml-format weights from
/// `ggerganov/whisper.cpp`; verified at download time.
pub const WHISPER_MODELS: &[ModelInfo] = &[
    ModelInfo {
        name: "tiny",
        multilingual: true,
        approx_mb: 75,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        sha256: "bd577a113a864445d4c299885e0cb97d4ba92b5f",
    },
    ModelInfo {
        name: "tiny.en",
        multilingual: false,
        approx_mb: 75,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        sha256: "c78c86eb1a8faa21b369bcd33207cc90d64ae9df",
    },
    ModelInfo {
        name: "base",
        multilingual: true,
        approx_mb: 142,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf",
    },
    ModelInfo {
        name: "base.en",
        multilingual: false,
        approx_mb: 142,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        sha256: "0b47b3e6ba5dd5e9c0a9f9bb6c4f9cc5b2c67f45",
    },
    ModelInfo {
        name: "small",
        multilingual: true,
        approx_mb: 466,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e9",
    },
    ModelInfo {
        name: "small.en",
        multilingual: false,
        approx_mb: 466,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        sha256: "6173ff4c80ea9c9562c30c3dc99ea25c30b2e63c",
    },
    ModelInfo {
        name: "medium",
        multilingual: true,
        approx_mb: 1500,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        sha256: "6c14d5ada1f8ed0fab7bc00d8cebe2c3fbbf3daf",
    },
    ModelInfo {
        name: "medium.en",
        multilingual: false,
        approx_mb: 1500,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
        sha256: "cc37e93478338ec7700281a7ac30a10128929eb8",
    },
];

pub struct ModelRegistry;

impl ModelRegistry {
    /// Look up a model by name (case-insensitive).
    #[must_use]
    pub fn get(name: &str) -> Option<&'static ModelInfo> {
        let lower = name.to_ascii_lowercase();
        WHISPER_MODELS.iter().find(|m| m.name == lower)
    }

    #[must_use]
    pub fn all() -> &'static [ModelInfo] {
        WHISPER_MODELS
    }

    /// Resolve the effective mirror URL, honouring `FONO_MODEL_MIRROR`.
    #[must_use]
    pub fn mirror() -> String {
        std::env::var("FONO_MODEL_MIRROR").unwrap_or_else(|_| DEFAULT_MIRROR.to_string())
    }

    /// Build the full download URL for a model.
    #[must_use]
    pub fn url_for(model: &ModelInfo) -> String {
        format!("{}/{}", Self::mirror(), model.url_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_small_model_is_multilingual() {
        let m = ModelRegistry::get("small").unwrap();
        assert!(m.multilingual);
    }

    #[test]
    fn mirror_override() {
        std::env::set_var("FONO_MODEL_MIRROR", "https://example.test");
        assert!(ModelRegistry::mirror().starts_with("https://example.test"));
        std::env::remove_var("FONO_MODEL_MIRROR");
    }
}
