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
/// The SHA-256 pins are currently the "unpinned" sentinel
/// (64 zeros) because upstream `ggerganov/whisper.cpp` does not publish a
/// canonical SHA-256 manifest and the previous placeholders were in fact
/// SHA-1 strings of unknown provenance. Downloads succeed and the
/// computed SHA-256 is logged at `info`; a follow-up change will pin the
/// real values once an authoritative manifest exists.
pub const UNPINNED: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub const WHISPER_MODELS: &[ModelInfo] = &[
    ModelInfo {
        name: "tiny",
        multilingual: true,
        approx_mb: 75,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        sha256: UNPINNED,
    },
    ModelInfo {
        name: "tiny.en",
        multilingual: false,
        approx_mb: 75,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        sha256: UNPINNED,
    },
    ModelInfo {
        name: "base",
        multilingual: true,
        approx_mb: 142,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        sha256: UNPINNED,
    },
    ModelInfo {
        name: "base.en",
        multilingual: false,
        approx_mb: 142,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        sha256: UNPINNED,
    },
    ModelInfo {
        name: "small",
        multilingual: true,
        approx_mb: 466,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        sha256: UNPINNED,
    },
    ModelInfo {
        name: "small.en",
        multilingual: false,
        approx_mb: 466,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        sha256: UNPINNED,
    },
    ModelInfo {
        name: "medium",
        multilingual: true,
        approx_mb: 1500,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        sha256: UNPINNED,
    },
    ModelInfo {
        name: "medium.en",
        multilingual: false,
        approx_mb: 1500,
        url_path: "ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
        sha256: UNPINNED,
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
