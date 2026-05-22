# Cartesia per-language native-voice cache

## Objective

Each language the user listed in `general.languages` (and each language STT detects at runtime) gets its **own native Cartesia voice**, looked up once via `GET /voices?language=<code>&limit=1` and cached for the process lifetime. A Romanian utterance plays through a Romanian voice; an English utterance through the catalogue's English voice; a Hindi utterance through a Hindi voice — all from the same daemon, picked per-sentence based on STT detection. No new config knobs.

## Implementation Plan

- [ ] Task 1. Replace the entire body of `crates/fono-tts/src/cartesia.rs` with the file content in **Appendix A** below. Key changes vs. today: `preferred_language: String` + `OnceCell<(String, String)>` are replaced by `user_languages: Vec<String>` + `Mutex<HashMap<String, Option<String>>>`; `resolve_voice` becomes `resolve_for_lang(lang: Option<&str>)`; `prewarm` iterates *every* non-English user language; nine new unit tests cover positive/negative caching, English-on-multilingual, pinned-voice, and normalisation helpers.

- [ ] Task 2. Update `crates/fono-tts/src/factory.rs:185-201` (`build_cartesia`) to pass the raw slice directly — no more `pick_preferred_language` indirection:

  ```rust
  #[cfg(feature = "cartesia")]
  fn build_cartesia(
      cfg: &Tts,
      secrets: &Secrets,
      languages: &[String],
  ) -> Result<Arc<dyn TextToSpeech>> {
      let (key_ref, model_override, voice_override) = resolve_cloud(cfg, &TtsBackend::Cartesia);
      let key = resolve_key(&key_ref, &TtsBackend::Cartesia, secrets)?;
      let voice = resolve_voice(cfg, voice_override);
      Ok(Arc::new(crate::cartesia::CartesiaTts::new(key, model_override, voice, languages)))
  }
  ```

- [ ] Task 3. Update `docs/providers.md:381-395` — the "Language-aware voice selection" paragraph. Replace the single-language framing with: "Each non-English code in `general.languages` (and each language STT detects at runtime) gets its own native voice, fetched lazily and cached per-language for the process lifetime. A Romanian utterance plays through a Romanian voice; the same multilingual user's English utterance plays through the catalogue's English voice; both sound native."

- [ ] Task 4. Pre-commit gate:
  - `cargo fmt --all`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --tests --lib`

  All must exit 0. Per `AGENTS.md`, run all three before pushing.

- [ ] Task 5. Commit with sign-off. Suggested message:

  ```
  fono-tts(cartesia): per-language native-voice cache

  Replace the single-preferred-language voice slot with a per-language
  HashMap cache keyed on the lower-cased BCP-47 alpha-2 code. Every
  non-English language the user configured (plus any language STT
  detects at runtime) gets its own native voice, fetched once via
  GET /voices?language=<code>&limit=1 and cached for the process
  lifetime. A multilingual user dictating in Romanian now hears a
  Romanian voice; the same user dictating in English hears the
  catalogue's English voice — both native, not a single voice forced
  to bilingual duty.
  ```

## Verification Criteria

- A user with `general.languages = ["en", "ro"]` who dictates in Romanian hears `(romanian_voice, language="ro")` on the wire; same user dictating in English hears `(english_fallback_voice, language="en")`. Both pay one `/voices` lookup ever per language across the daemon's lifetime.
- A user with `tts.voice` pinned hears the pinned voice for every utterance regardless of language; wire `language` echoes the STT-detected code.
- A user with no `general.languages` and no `tts.voice` hears the English catalogue voice with `language = "en"` — no `/voices` HTTP fired at all.
- All 14+ Cartesia unit tests pass; clippy clean; fmt clean.
- The `language_not_supported` retry path still trips and self-heals on the rare case the model rejects a voice's catalogue-advertised language.

## Potential Risks and Mitigations

1. **`/voices` API latency on first-use of an unexpected language.** Prewarm already covers every language in `general.languages`. The cold-cache cost only hits languages STT *detects* that the user didn't configure — rare, and the cost is one parallel HTTP roundtrip before the synth POST.
   Mitigation: keep prewarm; document the rare cold-path case in the file's module doc (already done in Appendix A).

2. **Cache grows unbounded if STT keeps detecting new languages.** Real-world cap is ~100 entries (every BCP-47 alpha-2 code) — well under any memory concern.
   Mitigation: none needed.

3. **Negative cache traps a transient failure.** A one-off network blip during the first lookup permanently routes that language to English for this process.
   Mitigation: acceptable — `language_not_supported` retry still catches mid-session model rejections; users restart the daemon rarely enough that a re-attempt on next launch is fine. If this proves annoying, add a TTL to negative entries (out of scope).

## Alternative Approaches

1. **Detect text language inside `cartesia.rs` via `whatlang` or `lingua-rs`.** Removes the dependency on STT plumbing but adds a Rust dep and language-detection accuracy concerns on short text. Rejected: STT detection is free, ground-truthed by the user's voice, and already plumbed.

2. **Bake a static BCP-47 → voice-UUID table.** No `/voices` HTTP at all, predictable behaviour. Rejected: requires manual upkeep when Cartesia rotates voices and locks us into specific voice personas the user can't override.

3. **Fetch *all* voices in one call at startup.** Single HTTP up front, no cold cache. Rejected: response is large, most languages never used, and the lazy pattern is one extra HTTP per language ever — negligible.

---

## Appendix A — Full content for `crates/fono-tts/src/cartesia.rs`

```rust
// SPDX-License-Identifier: GPL-3.0-only
//! Cartesia `/tts/bytes` client.
//!
//! Wire shape:
//!   POST `https://api.cartesia.ai/tts/bytes`
//!   header: `X-Api-Key: <key>`
//!   header: `Cartesia-Version: <YYYY-MM-DD>` (required; the API
//!           rejects requests without it with HTTP 400)
//!   body: `{ "model_id": "sonic-3.5", "transcript": <text>,
//!           "voice": { "mode": "id", "id": <voice_id> },
//!           "output_format": { "container": "raw",
//!                              "encoding": "pcm_s16le",
//!                              "sample_rate": 24000 },
//!           "language": "en" }`
//!   response: raw int16 LE mono PCM at 24 kHz.
//!
//! ## Per-language voice routing
//!
//! Each language the user listed in `general.languages` gets its own
//! native voice, looked up lazily via `GET /voices?language=<code>
//! &limit=1` the first time we need to synthesise in that language.
//! The result — `Some(voice_id)` or `None` (negative cache) — lives
//! in [`tokio::sync::Mutex<HashMap>`] for the process lifetime, so
//! every subsequent synth pays no extra HTTP.
//!
//! At synth time the caller passes the language detected by STT (see
//! `fono/src/assistant.rs`). We honour that hint: a Romanian utterance
//! gets a Romanian voice with `language = "ro"` on the wire (native
//! quality); an English utterance from the same multilingual user
//! gets a separate, English voice with `language = "en"` (also
//! native — Cartesia's catalogue has dedicated English voices that
//! sound better than a Romanian voice forced to read English).
//!
//! Resolution order: explicit `tts.voice` config pin wins over
//! everything; else the per-call `lang` hint drives the per-language
//! lookup; else the first non-English entry in `general.languages`;
//! else the English fallback voice from the catalogue. Any failure
//! (network, auth, no voices for that language, or model rejection
//! via the `language_not_supported` retry path) silently falls back
//! to the English fallback voice — TTS never errors out *because* of
//! voice routing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_core::provider_catalog;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::traits::{TextToSpeech, TtsAudio};

const NATIVE_RATE: u32 = 24_000;
const ENDPOINT: &str = "https://api.cartesia.ai/tts/bytes";
const VOICES_ENDPOINT: &str = "https://api.cartesia.ai/voices";
/// Pinned Cartesia API version. The server requires this header in
/// `YYYY-MM-DD` form on every request; without it the API responds
/// with HTTP 400 (`Cartesia-Version header is required …`). We pin
/// a known-good date rather than tracking `latest` so wire-shape
/// changes never break Fono silently. Matches the date the wizard's
/// key-validation probe sends.
const API_VERSION: &str = "2026-03-01";

pub struct CartesiaTts {
    api_key: String,
    model: String,
    /// English / fallback voice id. Comes from the provider catalogue
    /// (`Sonic English Female`) unless the user pinned one explicitly.
    fallback_voice_id: String,
    /// Lower-case BCP-47 alpha-2 codes the user configured, minus
    /// English / locale-tagged English (e.g. `["ro", "no"]` for a
    /// user who has `languages = ["en", "ro", "no"]`). Drives prewarm
    /// and provides the default language when the caller doesn't pass
    /// a per-call `lang` hint.
    user_languages: Vec<String>,
    /// True when the user pinned `tts.voice` explicitly — the factory
    /// promotes that into `fallback_voice_id` and we then skip
    /// language-based lookup entirely (user override always wins).
    voice_pinned: bool,
    /// `lang_code -> Some(voice_id)` (positive cache) or `None`
    /// (negative cache; lookup failed once, don't retry this
    /// session). Populated lazily by [`Self::resolve_for_lang`] or
    /// eagerly by [`Self::prewarm`].
    voice_cache: Mutex<HashMap<String, Option<String>>>,
    /// Set the first time sonic-3.5 returns `language_not_supported`
    /// for our resolved language. Subsequent synth calls short-circuit
    /// to the English fallback so we never re-pay the failed
    /// round-trip latency for the rest of the process lifetime.
    sonic_rejected_language: AtomicBool,
    client: reqwest::Client,
}

impl CartesiaTts {
    /// Build a client using the catalogue defaults for model / voice.
    ///
    /// `languages` is the full `general.languages` slice. The
    /// constructor normalises it: lowercases, strips locale tails
    /// (`pt-BR` → `pt`), drops English / empty entries, and dedupes.
    #[must_use]
    pub fn new(
        api_key: impl Into<String>,
        model_override: Option<String>,
        voice_override: Option<String>,
        languages: &[String],
    ) -> Self {
        let entry = provider_catalog::find("cartesia")
            .and_then(|p| p.tts.as_ref())
            .expect("cartesia catalogue entry must exist with a TTS capability");
        let voice_pinned = voice_override.is_some();
        Self {
            api_key: api_key.into(),
            model: model_override.unwrap_or_else(|| entry.model.to_string()),
            fallback_voice_id: voice_override.unwrap_or_else(|| entry.default_voice.to_string()),
            user_languages: normalise_languages(languages),
            voice_pinned,
            voice_cache: Mutex::new(HashMap::new()),
            sonic_rejected_language: AtomicBool::new(false),
            client: crate::openai_compat::warm_client(),
        }
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub fn fallback_voice_id(&self) -> &str {
        &self.fallback_voice_id
    }

    #[must_use]
    pub fn user_languages(&self) -> &[String] {
        &self.user_languages
    }

    #[must_use]
    pub const fn endpoint(&self) -> &'static str {
        ENDPOINT
    }

    #[must_use]
    pub fn build_request_body(
        &self,
        text: &str,
        voice_id: &str,
        language: &str,
    ) -> serde_json::Value {
        serde_json::to_value(SynthesizeReq {
            model_id: &self.model,
            transcript: text,
            voice: VoiceRef { mode: "id", id: voice_id },
            output_format: OutputFormat {
                container: "raw",
                encoding: "pcm_s16le",
                sample_rate: NATIVE_RATE,
            },
            language,
        })
        .expect("serialising static-shape Cartesia request must not fail")
    }

    async fn resolve_for_lang(&self, lang: Option<&str>) -> (String, String) {
        if self.voice_pinned {
            let wire = effective_lang(lang, &self.user_languages);
            let wire = if wire.is_empty() { "en".to_string() } else { wire };
            return (self.fallback_voice_id.clone(), wire);
        }
        if self.sonic_rejected_language.load(Ordering::Relaxed) {
            return (self.fallback_voice_id.clone(), "en".to_string());
        }
        let target = effective_lang(lang, &self.user_languages);
        if target.is_empty() || target == "en" {
            return (self.fallback_voice_id.clone(), "en".to_string());
        }
        {
            let cache = self.voice_cache.lock().await;
            if let Some(slot) = cache.get(&target) {
                return match slot {
                    Some(id) => (id.clone(), target),
                    None => (self.fallback_voice_id.clone(), "en".to_string()),
                };
            }
        }
        let fetched = match self.fetch_voice_for_language(&target).await {
            Ok(Some(id)) => {
                tracing::debug!(
                    target: "fono::tts::cartesia",
                    language = %target,
                    voice_id = %id,
                    "resolved Cartesia voice for language"
                );
                Some(id)
            }
            Ok(None) => {
                tracing::warn!(
                    target: "fono::tts::cartesia",
                    language = %target,
                    "Cartesia returned no public voices for language; falling back to English"
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    target: "fono::tts::cartesia",
                    language = %target,
                    error = %e,
                    "Cartesia voice lookup failed; falling back to English"
                );
                None
            }
        };
        let mut cache = self.voice_cache.lock().await;
        cache.insert(target.clone(), fetched.clone());
        match fetched {
            Some(id) => (id, target),
            None => (self.fallback_voice_id.clone(), "en".to_string()),
        }
    }

    async fn fetch_voice_for_language(&self, lang: &str) -> Result<Option<String>> {
        let resp = self
            .client
            .get(VOICES_ENDPOINT)
            .header("X-Api-Key", &self.api_key)
            .header("Cartesia-Version", API_VERSION)
            .query(&[("language", lang), ("limit", "1")])
            .send()
            .await
            .context("GET cartesia /voices")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(anyhow!("cartesia /voices returned {status}: {}", truncate(&body, 200)));
        }
        let parsed: VoicesPage = resp.json().await.context("decoding cartesia /voices JSON")?;
        Ok(parsed.data.into_iter().next().map(|v| v.id))
    }

    async fn post_synthesize(
        &self,
        text: &str,
        voice_id: &str,
        language: &str,
    ) -> Result<TtsAudio> {
        let body = self.build_request_body(text, voice_id, language);
        let resp = self
            .client
            .post(ENDPOINT)
            .header("X-Api-Key", &self.api_key)
            .header("Cartesia-Version", API_VERSION)
            .json(&body)
            .send()
            .await
            .context("posting to cartesia /tts/bytes")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(anyhow!("cartesia TTS returned {status}: {}", truncate(&body, 400)));
        }
        let bytes = resp.bytes().await.context("reading cartesia TTS response body")?;
        let pcm = pcm_i16_le_to_f32(&bytes);
        Ok(TtsAudio { pcm, sample_rate: NATIVE_RATE })
    }
}

fn normalise_languages(input: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for code in input {
        let lower = code.trim().to_lowercase();
        if lower.is_empty() {
            continue;
        }
        let bare = lower.split(['-', '_']).next().unwrap_or("").to_string();
        if bare.is_empty() || bare == "en" {
            continue;
        }
        if !out.contains(&bare) {
            out.push(bare);
        }
    }
    out
}

fn effective_lang(lang: Option<&str>, user_languages: &[String]) -> String {
    if let Some(raw) = lang {
        let lower = raw.trim().to_lowercase();
        if !lower.is_empty() {
            let bare = lower.split(['-', '_']).next().unwrap_or("").to_string();
            if !bare.is_empty() {
                return bare;
            }
        }
    }
    user_languages.first().cloned().unwrap_or_default()
}

#[derive(Serialize)]
struct SynthesizeReq<'a> {
    model_id: &'a str,
    transcript: &'a str,
    voice: VoiceRef<'a>,
    output_format: OutputFormat,
    language: &'a str,
}

#[derive(Serialize)]
struct VoiceRef<'a> {
    mode: &'a str,
    id: &'a str,
}

#[derive(Serialize)]
struct OutputFormat {
    container: &'static str,
    encoding: &'static str,
    sample_rate: u32,
}

#[derive(Deserialize)]
struct VoicesPage {
    data: Vec<VoiceSummary>,
}

#[derive(Deserialize)]
struct VoiceSummary {
    id: String,
}

#[async_trait]
impl TextToSpeech for CartesiaTts {
    fn name(&self) -> &'static str {
        "cartesia"
    }

    fn native_sample_rate(&self) -> u32 {
        NATIVE_RATE
    }

    async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        lang: Option<&str>,
    ) -> Result<TtsAudio> {
        if text.is_empty() {
            return Ok(TtsAudio { pcm: Vec::new(), sample_rate: NATIVE_RATE });
        }
        let (voice_id, language) = if let Some(v) = voice {
            let wire = effective_lang(lang, &self.user_languages);
            let wire = if wire.is_empty() { "en".to_string() } else { wire };
            (v.to_string(), wire)
        } else {
            self.resolve_for_lang(lang).await
        };
        match self.post_synthesize(text, &voice_id, &language).await {
            Ok(audio) => Ok(audio),
            Err(e) => {
                let already_english = voice_id == self.fallback_voice_id && language == "en";
                if !already_english && is_language_not_supported(&e) {
                    tracing::warn!(
                        target: "fono::tts::cartesia",
                        language = %language,
                        "Cartesia model rejected language; falling back to English for this session"
                    );
                    self.sonic_rejected_language.store(true, Ordering::Relaxed);
                    self.post_synthesize(text, &self.fallback_voice_id, "en").await
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn prewarm(&self) -> Result<()> {
        if !self.voice_pinned {
            for lang in &self.user_languages {
                let _ = self.resolve_for_lang(Some(lang)).await;
            }
        }
        Ok(())
    }
}

fn is_language_not_supported(err: &anyhow::Error) -> bool {
    let s = format!("{err:#}");
    s.contains("language_not_supported")
}

fn pcm_i16_le_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32767.0)
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s[..max].to_string();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cartesia_client_uses_catalogue_defaults() {
        let c = CartesiaTts::new("ck-test", None, None, &[]);
        assert_eq!(c.model(), "sonic-3.5");
        assert_eq!(c.fallback_voice_id(), "a0e99841-438c-4a64-b679-ae501e7d6091");
        assert!(c.user_languages().is_empty());
        assert_eq!(c.endpoint(), "https://api.cartesia.ai/tts/bytes");
        assert_eq!(c.native_sample_rate(), NATIVE_RATE);
    }

    #[test]
    fn request_body_shape_matches_spec() {
        let c = CartesiaTts::new("ck-test", None, None, &[]);
        let body =
            c.build_request_body("hello world", "a0e99841-438c-4a64-b679-ae501e7d6091", "en");
        assert_eq!(body["model_id"], "sonic-3.5");
        assert_eq!(body["transcript"], "hello world");
        assert_eq!(body["voice"]["mode"], "id");
        assert_eq!(body["voice"]["id"], "a0e99841-438c-4a64-b679-ae501e7d6091");
        assert_eq!(body["output_format"]["container"], "raw");
        assert_eq!(body["output_format"]["encoding"], "pcm_s16le");
        assert_eq!(body["output_format"]["sample_rate"], 24_000);
        assert_eq!(body["language"], "en");
    }

    #[tokio::test]
    async fn empty_text_returns_empty_audio_without_request() {
        let c = CartesiaTts::new("ck-test", None, None, &[]);
        let audio = c.synthesize("", None, None).await.unwrap();
        assert!(audio.pcm.is_empty());
        assert_eq!(audio.sample_rate, NATIVE_RATE);
    }

    #[tokio::test]
    async fn resolve_english_skips_network() {
        let c = CartesiaTts::new("ck-test", None, None, &[]);
        let (voice, lang) = c.resolve_for_lang(Some("en")).await;
        assert_eq!(voice, "a0e99841-438c-4a64-b679-ae501e7d6091");
        assert_eq!(lang, "en");
        let (voice2, lang2) = c.resolve_for_lang(None).await;
        assert_eq!(voice2, "a0e99841-438c-4a64-b679-ae501e7d6091");
        assert_eq!(lang2, "en");
    }

    #[tokio::test]
    async fn pinned_voice_skips_language_lookup() {
        let c =
            CartesiaTts::new("ck-test", None, Some("custom-uuid".to_string()), &["ro".to_string()]);
        let (voice, lang) = c.resolve_for_lang(Some("ro")).await;
        assert_eq!(voice, "custom-uuid");
        assert_eq!(lang, "ro");
    }

    #[tokio::test]
    async fn resolve_short_circuits_after_model_rejection() {
        let c = CartesiaTts::new("ck-test", None, None, &["ro".to_string()]);
        c.sonic_rejected_language.store(true, Ordering::Relaxed);
        let (voice, lang) = c.resolve_for_lang(Some("ro")).await;
        assert_eq!(voice, "a0e99841-438c-4a64-b679-ae501e7d6091");
        assert_eq!(lang, "en");
    }

    #[tokio::test]
    async fn negative_cache_short_circuits_subsequent_lookups() {
        let c = CartesiaTts::new("ck-test", None, None, &["xx".to_string()]);
        c.voice_cache.lock().await.insert("xx".to_string(), None);
        let (voice, lang) = c.resolve_for_lang(Some("xx")).await;
        assert_eq!(voice, "a0e99841-438c-4a64-b679-ae501e7d6091");
        assert_eq!(lang, "en");
    }

    #[tokio::test]
    async fn positive_cache_returns_cached_voice() {
        let c = CartesiaTts::new("ck-test", None, None, &["ro".to_string()]);
        c.voice_cache
            .lock()
            .await
            .insert("ro".to_string(), Some("ro-voice-uuid".to_string()));
        let (voice, lang) = c.resolve_for_lang(Some("ro")).await;
        assert_eq!(voice, "ro-voice-uuid");
        assert_eq!(lang, "ro");
    }

    #[tokio::test]
    async fn english_call_on_multilingual_user_uses_english_voice() {
        let c = CartesiaTts::new("ck-test", None, None, &["en".to_string(), "ro".to_string()]);
        c.voice_cache
            .lock()
            .await
            .insert("ro".to_string(), Some("ro-voice-uuid".to_string()));
        let (voice, lang) = c.resolve_for_lang(Some("en")).await;
        assert_eq!(voice, "a0e99841-438c-4a64-b679-ae501e7d6091");
        assert_eq!(lang, "en");
    }

    #[tokio::test]
    async fn none_lang_uses_first_user_language() {
        let c = CartesiaTts::new("ck-test", None, None, &["en".to_string(), "ro".to_string()]);
        c.voice_cache
            .lock()
            .await
            .insert("ro".to_string(), Some("ro-voice-uuid".to_string()));
        let (voice, lang) = c.resolve_for_lang(None).await;
        assert_eq!(voice, "ro-voice-uuid");
        assert_eq!(lang, "ro");
    }

    #[test]
    fn detects_language_not_supported_error() {
        let err = anyhow!(
            "cartesia TTS returned 400 Bad Request: \
             {{\"error_code\":\"language_not_supported\",\
             \"message\":\"The language is not supported by this model.\"}}"
        );
        assert!(is_language_not_supported(&err));
    }

    #[test]
    fn ignores_unrelated_errors() {
        let err = anyhow!("cartesia TTS returned 500 Internal Server Error: oops");
        assert!(!is_language_not_supported(&err));
    }

    #[test]
    fn normalise_drops_english_and_dedupes() {
        let langs = vec![
            "en".to_string(),
            "EN".to_string(),
            "en-GB".to_string(),
            "ro".to_string(),
            "RO".to_string(),
            "pt-BR".to_string(),
            "".to_string(),
            "ro".to_string(),
        ];
        assert_eq!(normalise_languages(&langs), vec!["ro".to_string(), "pt".to_string()]);
    }

    #[test]
    fn normalise_empty_returns_empty() {
        assert!(normalise_languages(&[]).is_empty());
    }

    #[test]
    fn effective_prefers_call_hint() {
        let users = vec!["ro".to_string()];
        assert_eq!(effective_lang(Some("ja"), &users), "ja");
        assert_eq!(effective_lang(Some("pt-BR"), &users), "pt");
        assert_eq!(effective_lang(Some("EN"), &users), "en");
    }

    #[test]
    fn effective_falls_back_to_first_user_language() {
        let users = vec!["ro".to_string(), "no".to_string()];
        assert_eq!(effective_lang(None, &users), "ro");
        assert_eq!(effective_lang(Some(""), &users), "ro");
    }

    #[test]
    fn effective_empty_when_no_signal() {
        assert_eq!(effective_lang(None, &[]), "");
    }
}
```
