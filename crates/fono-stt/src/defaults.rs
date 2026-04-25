// SPDX-License-Identifier: GPL-3.0-only
//! Provider-keyed default model strings used by the STT factory and
//! wizard.  Keeping them in one place avoids drift between the two.

/// Best-known default model identifier for a given cloud STT provider.
/// Returns a generic Whisper fallback when the provider is unknown.
#[must_use]
pub fn default_cloud_model(provider: &str) -> &'static str {
    match provider {
        // Groq's distilled turbo whisper has the best latency/quality tradeoff
        // currently available; see docs/plans/2026-04-25-fono-latency-v1.md.
        "groq" => "whisper-large-v3-turbo",
        "openai" => "whisper-1",
        "deepgram" => "nova-2",
        "assemblyai" => "best",
        "cartesia" => "sonic-transcribe",
        "azure" => "whisper",
        "google" => "default",
        _ => "whisper-large-v3",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_resolve() {
        assert_eq!(default_cloud_model("groq"), "whisper-large-v3-turbo");
        assert_eq!(default_cloud_model("openai"), "whisper-1");
    }

    #[test]
    fn unknown_falls_back() {
        assert_eq!(default_cloud_model("nope"), "whisper-large-v3");
    }
}
