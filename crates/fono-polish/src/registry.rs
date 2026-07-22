// SPDX-License-Identifier: GPL-3.0-only
//! Local LLM model registry, shared by both LLM roles (polish/F7 cleanup and
//! assistant/F8 chat).

#[derive(Debug, Clone, Copy)]
pub struct LocalLlmModelInfo {
    pub name: &'static str,
    pub display_name: &'static str,
    pub multilingual: bool,
    pub default_eligible: bool,
    pub approx_mb: u32,
    pub url_path: &'static str,
    pub sha256: &'static str,
    pub license: &'static str,
}

/// HuggingFace GGUF weights pinned for the first-run downloader.
///
/// NOTE: SHA256s below are pinned from the upstream GGUF/LFS metadata;
/// `fono models verify` re-checks them.
pub const LOCAL_LLM_MODELS: &[LocalLlmModelInfo] = &[
    LocalLlmModelInfo {
        name: "gemma-4-e2b",
        display_name: "Gemma 4 E2B Instruct Q4_0",
        multilingual: true,
        default_eligible: true,
        approx_mb: 3_195,
        url_path: "google/gemma-4-E2B-it-qat-q4_0-gguf/resolve/main/gemma-4-E2B_q4_0-it.gguf",
        sha256: "3646b4c147cd235a44d91df1546d3b7d8e29b547dbe4e1f80856419aa455e6fd",
        license: "Apache-2.0",
    },
    LocalLlmModelInfo {
        name: "qwen3.5-0.8b",
        display_name: "Qwen3.5 0.8B Q4_K_M",
        multilingual: true,
        default_eligible: false,
        approx_mb: 528,
        url_path: "lmstudio-community/Qwen3.5-0.8B-GGUF/resolve/main/Qwen3.5-0.8B-Q4_K_M.gguf",
        sha256: "f5b14da98939b60bbe1019a964eba656407e1e0b64f1fe3003ff6d650e93bfec",
        license: "Apache-2.0",
    },
    LocalLlmModelInfo {
        name: "qwen3.5-2b",
        display_name: "Qwen3.5 2B Q4_K_M",
        multilingual: true,
        default_eligible: false,
        approx_mb: 1_270,
        url_path: "lmstudio-community/Qwen3.5-2B-GGUF/resolve/main/Qwen3.5-2B-Q4_K_M.gguf",
        sha256: "0bfe35afc9f05b7fac3fa04925e051ac7939a42a8a17ea11afc99701bea826cc",
        license: "Apache-2.0",
    },
    LocalLlmModelInfo {
        name: "gemma-4-26b-a4b-it-asym",
        display_name: "Gemma 4 26B-A4B Instruct (asym 2-bit, streamed)",
        multilingual: true,
        default_eligible: false,
        approx_mb: 9_602,
        url_path: "bogdan-radulescu/gemma-4-26B-A4B-it-asym-GGUF/resolve/main/gemma-4-26B-A4B-it-asym.gguf",
        sha256: "88cca0d55b441627f2c9cb05b5a4752d6bf78b28377ddb4ea0b81675334d8404",
        license: "Apache-2.0",
    },
    LocalLlmModelInfo {
        name: "qwen3.6-35b-a3b-asym",
        display_name: "Qwen3.6 35B-A3B (asym 2-bit, streamed)",
        multilingual: true,
        default_eligible: false,
        approx_mb: 11_737,
        url_path: "bogdan-radulescu/qwen3.6-35B-A3B-asym-GGUF/resolve/main/qwen3.6-35b-a3b-asym.gguf",
        sha256: "d5d34aba11845c8a6fee4a8007c49989769fa1bc9418a1ad22dbd13faef8a41c",
        license: "Apache-2.0",
    },
];

pub struct LocalLlmRegistry;

impl LocalLlmRegistry {
    #[must_use]
    pub fn get(name: &str) -> Option<&'static LocalLlmModelInfo> {
        let normalized = Self::normalize(name);
        // Registry `name`s are lowercase, suffix-free by convention. We match
        // against a normalized form of the caller's value (lowercased, with a
        // trailing `-gguf` stripped) so the HuggingFace repo name a user
        // naturally copies — e.g. `gemma-4-26B-A4B-it-asym-GGUF` — resolves to
        // the same entry as the canonical `gemma-4-26b-a4b-it-asym`.
        LOCAL_LLM_MODELS.iter().find(|m| m.name == normalized)
    }

    /// Normalize a user-supplied model name to the registry's canonical form:
    /// ASCII-lowercased, with a single trailing `-gguf` (the HuggingFace repo
    /// suffix) removed. Pure string transform; does not consult the registry.
    #[must_use]
    pub fn normalize(name: &str) -> String {
        let lower = name.to_ascii_lowercase();
        lower.strip_suffix("-gguf").unwrap_or(&lower).to_string()
    }

    /// On-disk filename stem for a user-configured model name. A
    /// registry-known name resolves to its canonical (lowercase, suffix-free)
    /// `name` so the downloader (which writes `<info.name>.gguf`) and the
    /// loader (which reads this stem) always agree — regardless of the case or
    /// `-GGUF` suffix the user typed. An unknown name passes through verbatim,
    /// preserving support for manually-placed GGUFs.
    #[must_use]
    pub fn resolve_filename_stem(name: &str) -> String {
        Self::get(name).map_or_else(|| name.to_string(), |m| m.name.to_string())
    }

    #[must_use]
    pub fn all() -> &'static [LocalLlmModelInfo] {
        LOCAL_LLM_MODELS
    }

    #[must_use]
    pub fn mirror() -> String {
        std::env::var("FONO_MODEL_MIRROR").unwrap_or_else(|_| "https://huggingface.co".to_string())
    }

    #[must_use]
    pub fn url_for(m: &LocalLlmModelInfo) -> String {
        format!("{}/{}", Self::mirror(), m.url_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemma_e2b_is_apache_multilingual_and_default_eligible() {
        let gemma = LocalLlmRegistry::get("gemma-4-e2b").unwrap();
        assert_eq!(gemma.display_name, "Gemma 4 E2B Instruct Q4_0");
        assert_eq!(gemma.license, "Apache-2.0");
        assert!(gemma.multilingual);
        assert!(gemma.default_eligible);
        assert_eq!(gemma.approx_mb, 3_195);
        assert_eq!(
            gemma.url_path,
            "google/gemma-4-E2B-it-qat-q4_0-gguf/resolve/main/gemma-4-E2B_q4_0-it.gguf"
        );
        assert_eq!(
            gemma.sha256,
            "3646b4c147cd235a44d91df1546d3b7d8e29b547dbe4e1f80856419aa455e6fd"
        );
    }

    #[test]
    fn qwen3_5_models_are_apache_multilingual_manual_options() {
        let default = LocalLlmRegistry::get("qwen3.5-0.8b").unwrap();
        assert_eq!(default.license, "Apache-2.0");
        assert!(default.multilingual);
        assert!(!default.default_eligible);
        assert_eq!(
            default.sha256,
            "f5b14da98939b60bbe1019a964eba656407e1e0b64f1fe3003ff6d650e93bfec"
        );

        let high = LocalLlmRegistry::get("qwen3.5-2b").unwrap();
        assert_eq!(high.license, "Apache-2.0");
        assert!(high.multilingual);
        assert!(!high.default_eligible);
        assert_eq!(high.approx_mb, 1_270);
        assert_eq!(high.sha256, "0bfe35afc9f05b7fac3fa04925e051ac7939a42a8a17ea11afc99701bea826cc");
    }

    #[test]
    fn asym_moes_registered_and_not_default_eligible() {
        for name in ["gemma-4-26b-a4b-it-asym", "qwen3.6-35b-a3b-asym"] {
            let m =
                LocalLlmRegistry::get(name).unwrap_or_else(|| panic!("registry missing {name}"));
            assert!(!m.default_eligible, "{name} must not be a first-run default");
            assert_eq!(m.license, "Apache-2.0");
            assert!(m.url_path.starts_with("bogdan-radulescu/"), "{name} url_path");
            assert_eq!(m.sha256.len(), 64, "{name} sha256 must be a full digest");
        }
        // Names are lowercase; get() normalizes its input (lowercase + strip a
        // trailing `-gguf`) so mixed-case and the HuggingFace repo name both
        // resolve to the same (lowercase) on-disk filename stem.
        assert!(LocalLlmRegistry::get("Gemma-4-26B-A4B-it-asym").is_some());
        assert!(LocalLlmRegistry::get("gemma-4-26B-A4B-it-asym-GGUF").is_some());
    }

    #[test]
    fn resolve_filename_stem_canonicalizes_known_and_passes_through_unknown() {
        // Known names (any case / repo suffix) collapse to the canonical stem,
        // so the downloader dest and the loader path always agree.
        assert_eq!(
            LocalLlmRegistry::resolve_filename_stem("gemma-4-26B-A4B-it-asym-GGUF"),
            "gemma-4-26b-a4b-it-asym"
        );
        assert_eq!(
            LocalLlmRegistry::resolve_filename_stem("Gemma-4-26b-A4B-it-asym"),
            "gemma-4-26b-a4b-it-asym"
        );
        // Unknown names pass through verbatim (manually-placed GGUFs).
        assert_eq!(LocalLlmRegistry::resolve_filename_stem("my-custom-model"), "my-custom-model");
    }
}
