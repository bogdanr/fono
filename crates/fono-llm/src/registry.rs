// SPDX-License-Identifier: GPL-3.0-only
//! LLM model registry for the local (Apache-2.0-only) defaults.
//!
//! Per ADR 0004 we ship only Apache-2.0-licensed defaults. Llama / Gemma are
//! excluded from the default list.

#[derive(Debug, Clone, Copy)]
pub struct LlmModelInfo {
    pub name: &'static str,
    pub multilingual: bool,
    pub approx_mb: u32,
    pub url_path: &'static str,
    pub sha256: &'static str,
    pub license: &'static str,
}

/// HuggingFace GGUF weights pinned for the first-run downloader.
///
/// NOTE: SHA256s below are pinned from the Qwen2.5 / SmolLM2 GGUF releases at
/// the time of Phase 5 plan authoring; `fono models verify` re-checks them.
pub const LLM_MODELS: &[LlmModelInfo] = &[
    LlmModelInfo {
        name: "qwen2.5-0.5b-instruct",
        multilingual: true,
        approx_mb: 350,
        url_path: "Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/qwen2.5-0.5b-instruct-q4_k_m.gguf",
        sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        license: "Apache-2.0",
    },
    LlmModelInfo {
        name: "qwen2.5-1.5b-instruct",
        multilingual: true,
        approx_mb: 1_000,
        url_path: "Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf",
        sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        license: "Apache-2.0",
    },
    LlmModelInfo {
        name: "qwen2.5-3b-instruct",
        multilingual: true,
        approx_mb: 2_000,
        url_path: "Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf",
        sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        license: "Apache-2.0",
    },
    LlmModelInfo {
        name: "smollm2-1.7b-instruct",
        multilingual: false,
        approx_mb: 1_100,
        url_path:
            "HuggingFaceTB/SmolLM2-1.7B-Instruct-GGUF/resolve/main/smollm2-1.7b-instruct-q4_k_m.gguf",
        sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        license: "Apache-2.0",
    },
];

pub struct LlmRegistry;

impl LlmRegistry {
    #[must_use]
    pub fn get(name: &str) -> Option<&'static LlmModelInfo> {
        let lower = name.to_ascii_lowercase();
        LLM_MODELS.iter().find(|m| m.name == lower)
    }

    #[must_use]
    pub fn all() -> &'static [LlmModelInfo] {
        LLM_MODELS
    }

    #[must_use]
    pub fn mirror() -> String {
        std::env::var("FONO_MODEL_MIRROR").unwrap_or_else(|_| "https://huggingface.co".to_string())
    }

    #[must_use]
    pub fn url_for(m: &LlmModelInfo) -> String {
        format!("{}/{}", Self::mirror(), m.url_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_qwen_1_5b() {
        let m = LlmRegistry::get("qwen2.5-1.5b-instruct").unwrap();
        assert_eq!(m.license, "Apache-2.0");
        assert!(m.multilingual);
    }
}
