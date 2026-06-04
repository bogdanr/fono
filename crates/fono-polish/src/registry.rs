// SPDX-License-Identifier: GPL-3.0-only
//! LLM model registry for the local (Apache-2.0-only) defaults.
//!
//! Per ADR 0004 we ship only Apache-2.0-licensed defaults. Llama / Gemma are
//! excluded from the default list.

#[derive(Debug, Clone, Copy)]
pub struct PolishModelInfo {
    pub name: &'static str,
    pub multilingual: bool,
    pub approx_mb: u32,
    pub url_path: &'static str,
    pub sha256: &'static str,
    pub license: &'static str,
}

/// HuggingFace GGUF weights pinned for the first-run downloader.
///
/// NOTE: SHA256s below are pinned from the Qwen3.5 GGUF releases at the
/// time of registry updates; `fono models verify` re-checks them.
pub const POLISH_MODELS: &[PolishModelInfo] = &[
    PolishModelInfo {
        name: "qwen3.5-0.8b",
        multilingual: true,
        approx_mb: 528,
        url_path: "lmstudio-community/Qwen3.5-0.8B-GGUF/resolve/main/Qwen3.5-0.8B-Q4_K_M.gguf",
        sha256: "f5b14da98939b60bbe1019a964eba656407e1e0b64f1fe3003ff6d650e93bfec",
        license: "Apache-2.0",
    },
    PolishModelInfo {
        name: "qwen3.5-2b",
        multilingual: true,
        approx_mb: 1_270,
        url_path: "lmstudio-community/Qwen3.5-2B-GGUF/resolve/main/Qwen3.5-2B-Q4_K_M.gguf",
        sha256: "0bfe35afc9f05b7fac3fa04925e051ac7939a42a8a17ea11afc99701bea826cc",
        license: "Apache-2.0",
    },
];

pub struct PolishRegistry;

impl PolishRegistry {
    #[must_use]
    pub fn get(name: &str) -> Option<&'static PolishModelInfo> {
        let lower = name.to_ascii_lowercase();
        POLISH_MODELS.iter().find(|m| m.name == lower)
    }

    #[must_use]
    pub fn all() -> &'static [PolishModelInfo] {
        POLISH_MODELS
    }

    #[must_use]
    pub fn mirror() -> String {
        std::env::var("FONO_MODEL_MIRROR").unwrap_or_else(|_| "https://huggingface.co".to_string())
    }

    #[must_use]
    pub fn url_for(m: &PolishModelInfo) -> String {
        format!("{}/{}", Self::mirror(), m.url_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qwen3_5_models_are_apache_multilingual() {
        let default = PolishRegistry::get("qwen3.5-0.8b").unwrap();
        assert_eq!(default.license, "Apache-2.0");
        assert!(default.multilingual);
        assert_eq!(
            default.sha256,
            "f5b14da98939b60bbe1019a964eba656407e1e0b64f1fe3003ff6d650e93bfec"
        );

        let high = PolishRegistry::get("qwen3.5-2b").unwrap();
        assert_eq!(high.license, "Apache-2.0");
        assert!(high.multilingual);
        assert_eq!(high.approx_mb, 1_270);
        assert_eq!(high.sha256, "0bfe35afc9f05b7fac3fa04925e051ac7939a42a8a17ea11afc99701bea826cc");
    }
}
