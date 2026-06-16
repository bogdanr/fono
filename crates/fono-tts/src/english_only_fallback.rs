// SPDX-License-Identifier: GPL-3.0-only
//! Transparent local fallback for English-only cloud TTS backends
//! (feature `tts-local`).
//!
//! Some cloud voices only render intelligible English (Groq's Orpheus
//! `…-english`, the Speechmatics TTS preview, …). Feeding them text in
//! another language produces an English phonemization of foreign words —
//! gibberish, not speech in that language. The catalogue marks those
//! providers with [`fono_core::provider_catalog::TtsDefaults::english_only`].
//!
//! [`EnglishOnlyFallback`] wraps such a backend. On each utterance it
//! identifies the language of the **text being spoken** (constrained to the
//! user's configured `general.languages` for short-reply accuracy, falling
//! back to the caller's `lang` hint). English (or an inconclusive detection)
//! goes to the cloud backend unchanged — zero behaviour change on the common
//! path. A reliably non-English utterance is instead synthesised by the local
//! multilingual voice for that language, which is downloaded + cached on first
//! use. When the local engine cannot be made available (no catalogue voice,
//! download/load failure), the utterance is skipped with a single warning
//! rather than spoken as gibberish.
//!
//! There are **no new config knobs**: the behaviour is automatic and only
//! engages for backends the catalogue flags as English-only. When the
//! `tts-local` feature is not compiled in, this wrapper does not exist and the
//! cloud backend is used directly (the legacy behaviour).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::local_router::{base_lang, detect_base_lang, load_engine};
use crate::traits::{TextToSpeech, TtsAudio};

/// A [`TextToSpeech`] that routes non-English utterances away from an
/// English-only cloud backend to the local multilingual voice stack.
pub struct EnglishOnlyFallback {
    /// The English-only cloud backend. Used verbatim for English (or
    /// inconclusively-detected) text.
    primary: Arc<dyn TextToSpeech>,
    /// Where local voice assets are cached / downloaded.
    voices_dir: PathBuf,
    /// Optional mirror base-URL override (`[tts.local].base_url`).
    base_url: Option<String>,
    /// Configured `general.languages` as deduped base codes, used to
    /// constrain language identification for short replies.
    langs: Vec<String>,
    /// Local engines keyed by base language code; `Some(None)` records a
    /// permanent failure (missing catalogue voice / download / load) so we
    /// don't retry every utterance.
    local: Mutex<HashMap<String, Option<Arc<dyn TextToSpeech>>>>,
    /// Emit the "fallback unavailable" warning at most once per process.
    warned: AtomicBool,
}

impl EnglishOnlyFallback {
    /// Wrap `primary` (an English-only cloud backend). `languages` is the
    /// user's `general.languages`; `base_url` overrides the voice mirror.
    #[must_use]
    pub fn new(
        primary: Arc<dyn TextToSpeech>,
        voices_dir: impl Into<PathBuf>,
        base_url: Option<String>,
        languages: &[String],
    ) -> Self {
        let mut langs: Vec<String> = Vec::new();
        for l in languages {
            let b = base_lang(l);
            if !b.is_empty() && !langs.contains(&b) {
                langs.push(b);
            }
        }
        Self {
            primary,
            voices_dir: voices_dir.into(),
            base_url,
            langs,
            local: Mutex::new(HashMap::new()),
            warned: AtomicBool::new(false),
        }
    }

    /// The base language to route locally, or `None` to use the cloud
    /// backend. Identifies the text's language first (constrained to the
    /// configured languages), falling back to the caller's hint; returns
    /// `Some(base)` only for a reliably non-English result.
    fn route_language(&self, text: &str, lang: Option<&str>) -> Option<String> {
        let detected = detect_base_lang(text, &self.langs);
        let chosen =
            detected.or_else(|| lang.map(str::trim).filter(|l| !l.is_empty()).map(base_lang));
        match chosen {
            Some(b) if !b.is_empty() && b != "en" => Some(b),
            _ => None,
        }
    }

    /// Get (or lazily build + cache) the local engine for `base`.
    async fn local_engine_for(&self, base: &str) -> Option<Arc<dyn TextToSpeech>> {
        let mut cache = self.local.lock().await;
        if let Some(slot) = cache.get(base) {
            return slot.clone();
        }
        let engine = build_local_engine(base, &self.voices_dir, self.base_url.as_deref()).await;
        cache.insert(base.to_string(), engine.clone());
        engine
    }

    /// Log the "fallback unavailable, skipping" warning at most once.
    fn warn_skip_once(&self, base: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                target: "fono_tts::english_only_fallback",
                lang = base,
                primary = self.primary.name(),
                "configured TTS backend is English-only and no local fallback voice is \
                 available for this language; skipping the utterance instead of speaking \
                 gibberish. Build/enable the local TTS engine (tts-local) or switch to a \
                 multilingual TTS backend.",
            );
        }
    }
}

/// Resolve, download, and load the local voice for `base`. Returns `None`
/// (logged) when the catalogue has no voice, the download fails, or the
/// engine can't load — the caller then skips the utterance.
async fn build_local_engine(
    base: &str,
    voices_dir: &std::path::Path,
    base_url: Option<&str>,
) -> Option<Arc<dyn TextToSpeech>> {
    let voice = match crate::voices::for_language(base) {
        Ok(Some(v)) => v,
        Ok(None) => {
            tracing::warn!(
                target: "fono_tts::english_only_fallback",
                lang = base,
                "no local voice in the catalog for this language; cannot fall back",
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(
                target: "fono_tts::english_only_fallback",
                lang = base, error = %e,
                "voice catalog lookup failed",
            );
            return None;
        }
    };
    if let Err(e) = crate::voices::ensure_voice(&voice, voices_dir, base_url).await {
        tracing::warn!(
            target: "fono_tts::english_only_fallback",
            lang = base, voice = %voice.name, error = %format!("{e:#}"),
            "downloading the local fallback voice failed",
        );
        return None;
    }
    match load_engine(voices_dir, &voice) {
        Ok(engine) => {
            tracing::info!(
                target: "fono_tts::english_only_fallback",
                lang = base, voice = %voice.name,
                "local fallback voice ready for non-English speech",
            );
            Some(engine)
        }
        Err(e) => {
            tracing::warn!(
                target: "fono_tts::english_only_fallback",
                lang = base, voice = %voice.name, error = %format!("{e:#}"),
                "loading the local fallback voice failed",
            );
            None
        }
    }
}

#[async_trait]
impl TextToSpeech for EnglishOnlyFallback {
    async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        lang: Option<&str>,
    ) -> Result<TtsAudio> {
        if text.trim().is_empty() {
            return self.primary.synthesize(text, voice, lang).await;
        }
        let Some(base) = self.route_language(text, lang) else {
            // English or inconclusive — the cloud backend handles it.
            return self.primary.synthesize(text, voice, lang).await;
        };
        if let Some(engine) = self.local_engine_for(&base).await {
            tracing::debug!(
                target: "fono_tts::english_only_fallback",
                lang = %base, primary = self.primary.name(),
                "routing non-English utterance to the local voice",
            );
            engine.synthesize(text, None, None).await
        } else {
            self.warn_skip_once(&base);
            Ok(TtsAudio { pcm: Vec::new(), sample_rate: self.primary.native_sample_rate() })
        }
    }

    fn name(&self) -> &'static str {
        self.primary.name()
    }

    fn native_sample_rate(&self) -> u32 {
        self.primary.native_sample_rate()
    }

    async fn prewarm(&self) -> Result<()> {
        self.primary.prewarm().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial fake cloud backend that records whether it was called and
    /// returns a 1-sample buffer so we can tell it apart from the
    /// skip-with-empty-PCM path.
    struct FakePrimary {
        called: AtomicBool,
    }

    #[async_trait]
    impl TextToSpeech for FakePrimary {
        async fn synthesize(
            &self,
            _text: &str,
            _voice: Option<&str>,
            _lang: Option<&str>,
        ) -> Result<TtsAudio> {
            self.called.store(true, Ordering::Relaxed);
            Ok(TtsAudio { pcm: vec![0.5], sample_rate: 24_000 })
        }
        fn name(&self) -> &'static str {
            "fake-english-cloud"
        }
        fn native_sample_rate(&self) -> u32 {
            24_000
        }
    }

    fn langs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    fn fallback(languages: &[&str]) -> (Arc<FakePrimary>, EnglishOnlyFallback) {
        let primary = Arc::new(FakePrimary { called: AtomicBool::new(false) });
        // An unroutable mirror base URL guarantees the voice download fails,
        // so the non-English path deterministically takes the warn-and-skip
        // branch without depending on the network or on-disk assets.
        let wrapper = EnglishOnlyFallback::new(
            primary.clone(),
            std::env::temp_dir().join("fono-english-only-fallback-test"),
            Some("http://127.0.0.1:1/never".to_string()),
            &langs(languages),
        );
        (primary, wrapper)
    }

    #[test]
    fn route_language_keeps_english_on_cloud() {
        let (_p, w) = fallback(&["en", "ro"]);
        let text = "Good afternoon, today we will talk about how this program works.";
        assert!(w.route_language(text, None).is_none(), "English must stay on the cloud backend");
    }

    #[test]
    fn route_language_picks_romanian() {
        let (_p, w) = fallback(&["en", "ro"]);
        let text = "Bună ziua, astăzi vom vorbi despre cum funcționează acest program.";
        assert_eq!(w.route_language(text, None).as_deref(), Some("ro"));
    }

    #[test]
    fn route_language_uses_hint_when_detection_inconclusive() {
        // Single configured language → detection can't disambiguate; the
        // caller's hint (e.g. STT-detected language on the assistant path)
        // drives the decision.
        let (_p, w) = fallback(&["en"]);
        assert_eq!(w.route_language("ok", Some("ro")).as_deref(), Some("ro"));
        assert!(w.route_language("ok", Some("en")).is_none());
        assert!(w.route_language("ok", Some("en-US")).is_none());
        assert!(w.route_language("ok", None).is_none());
    }

    #[tokio::test]
    async fn english_text_goes_to_primary() {
        let (primary, w) = fallback(&["en", "ro"]);
        let audio = w
            .synthesize("This is a normal English sentence to synthesize.", None, None)
            .await
            .unwrap();
        assert!(primary.called.load(Ordering::Relaxed), "cloud backend must be used for English");
        assert_eq!(audio.pcm.len(), 1, "primary returns a 1-sample buffer");
    }

    #[tokio::test]
    async fn non_english_skips_when_local_unavailable() {
        let (primary, w) = fallback(&["en", "ro"]);
        let audio = w
            .synthesize("Bună ziua, astăzi vom vorbi despre acest program important.", None, None)
            .await
            .unwrap();
        assert!(
            !primary.called.load(Ordering::Relaxed),
            "English-only cloud backend must NOT be fed non-English text",
        );
        assert!(audio.pcm.is_empty(), "with no local engine the utterance is skipped (empty PCM)");
    }

    #[tokio::test]
    async fn empty_text_passes_through_to_primary() {
        let (primary, w) = fallback(&["en", "ro"]);
        let audio = w.synthesize("   ", None, None).await.unwrap();
        assert!(primary.called.load(Ordering::Relaxed));
        assert_eq!(audio.pcm.len(), 1);
    }
}
