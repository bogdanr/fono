// SPDX-License-Identifier: GPL-3.0-only
//! Language-aware router over per-language Piper voices (feature `tts-local`).
//!
//! [`PiperLocal`] wraps exactly one voice. A bilingual user (e.g.
//! `languages = ["en", "ro"]`) needs the *Romanian* voice for a Romanian reply
//! and the *English* voice for an English one — synthesising Romanian text
//! through the English voice produces the wrong phonemes and an English accent.
//!
//! [`LocalRouter`] is the [`TextToSpeech`] the daemon actually holds. It keys a
//! lazily-populated map of [`PiperLocal`] engines by catalog voice and, on each
//! `synthesize`, picks the voice for the utterance's language (the best-effort
//! `lang` hint the caller threads through from STT detection). Engines load on
//! first use per language and are cached for the process lifetime.
//!
//! An explicit `[tts.local].voice` pin disables routing: every utterance uses
//! the pinned voice, matching the Cartesia client's "user pinned a voice"
//! semantics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use fono_core::turn_trace::{current_instant, current_span};
use serde_json::json;
use whatlang::{Detector, Lang};

use crate::kokoro::KokoroLocal;
use crate::piper::{PiperConfig, PiperLocal};
use crate::traits::{TextToSpeech, TtsAudio};
use crate::voices::Voice;

/// A [`TextToSpeech`] that routes each utterance to the cached Piper voice
/// matching its language, loading voices on demand.
pub struct LocalRouter {
    voices_dir: PathBuf,
    /// Loaded engines keyed by catalog voice `name`. Populated lazily; the
    /// `default_voice` engine is loaded eagerly at construction. Engines are
    /// trait objects so Piper and Kokoro voices coexist in one map (ADR 0033:
    /// Kokoro for English, Piper for the rest).
    cache: Mutex<HashMap<String, Arc<dyn TextToSpeech>>>,
    /// Voice used when the utterance language has no dedicated catalog voice,
    /// when no `lang` hint is supplied, or when a voice pin is in effect: the
    /// user's primary language voice, or the explicit `[tts.local].voice`.
    default_voice: Voice,
    /// When set, every utterance uses `default_voice` regardless of `lang`
    /// (an explicit `[tts.local].voice` pin).
    pinned: bool,
    /// Engine the user pinned via `[tts.local].engine` (`"piper"` /
    /// `"kokoro"`), or `None` for Auto. When set, per-language routing and
    /// explicit per-call voice overrides are constrained to this engine's
    /// catalog entries, so a Kokoro-pinned user never gets routed to a Piper
    /// voice (and vice versa).
    engine_filter: Option<String>,
    /// The user's configured `general.languages` as base codes (deduped,
    /// e.g. `["en", "ro"]`). When a caller supplies no `lang` hint, text is
    /// language-identified against *this* allowlist so detection on short
    /// replies stays accurate.
    langs: Vec<String>,
    /// Native PCM rate of `default_voice`, for the playback warmup hint.
    native_rate: u32,
}

impl LocalRouter {
    /// Build a router. Eagerly loads `default_voice` (so a missing primary
    /// voice surfaces the same actionable error the single-voice path gave,
    /// and the playback layer gets a real sample-rate hint); other languages
    /// load on first use.
    pub fn new(
        voices_dir: impl Into<PathBuf>,
        default_voice: Voice,
        pinned: bool,
        languages: &[String],
        engine_filter: Option<String>,
    ) -> Result<Self> {
        let voices_dir = voices_dir.into();
        let engine = load_engine(&voices_dir, &default_voice)?;
        let native_rate = engine.native_sample_rate();
        let mut cache: HashMap<String, Arc<dyn TextToSpeech>> = HashMap::new();
        cache.insert(default_voice.name.clone(), engine);
        let langs = dedup_base_langs(languages);
        Ok(Self {
            voices_dir,
            cache: Mutex::new(cache),
            default_voice,
            pinned,
            langs,
            native_rate,
            engine_filter,
        })
    }

    /// Resolve which catalog voice should speak `text`.
    ///
    /// The voice must match the language of the **text being spoken**, which is
    /// not always the caller's `lang` hint: on the assistant path that hint is
    /// the language the STT engine detected for the *user's speech*, but the
    /// LLM reply can be in a different language (ask an English question, get a
    /// Romanian answer). So we identify the language from the text itself first
    /// — constrained to the configured `langs` for accuracy — and fall back to
    /// the caller's hint only when detection is inconclusive (e.g. a reply too
    /// short to fingerprint), then to the default voice.
    fn voice_for(&self, text: &str, voice: Option<&str>, lang: Option<&str>) -> Voice {
        if self.pinned {
            return self.default_voice.clone();
        }
        // An explicit per-call voice name (the resolver's chosen palette
        // voice, e.g. "am_michael") overrides language routing when it
        // names a real catalog voice. An empty or unknown name falls
        // through to the language-based selection below.
        if let Some(name) = voice.map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(v) = resolve_explicit_voice(Some(name))
                .filter(|v| self.engine_filter.as_deref().is_none_or(|e| v.engine == e))
            {
                current_instant(
                    "tts.voice_select",
                    "assistant.tts",
                    "tts",
                    json!({ "explicit": name, "voice": v.name, "engine": v.engine }),
                );
                tracing::debug!(
                    target: "fono_tts::local_router",
                    explicit = name,
                    engine = %v.engine,
                    voice = %v.name,
                    "local TTS explicit voice override",
                );
                return v;
            }
            tracing::debug!(
                target: "fono_tts::local_router",
                explicit = name,
                "explicit voice not in catalog — falling back to language routing",
            );
        }
        let detected = detect_base_lang(text, &self.langs);
        let chosen = detected
            .clone()
            .or_else(|| lang.map(str::trim).filter(|l| !l.is_empty()).map(str::to_string));
        let voice = resolve_voice_for_lang_engine(
            &self.default_voice,
            self.pinned,
            chosen.as_deref(),
            self.engine_filter.as_deref(),
        );
        current_instant(
            "tts.voice_select",
            "assistant.tts",
            "tts",
            json!({
                "hint": lang.unwrap_or(""),
                "detected": detected.as_deref().unwrap_or(""),
                "chosen_lang": chosen.as_deref().unwrap_or(""),
                "voice": voice.name,
                "pinned": self.pinned,
            }),
        );
        tracing::debug!(
            target: "fono_tts::local_router",
            hint = lang.unwrap_or(""),
            detected = detected.as_deref().unwrap_or(""),
            chosen_lang = chosen.as_deref().unwrap_or(""),
            engine = %voice.engine,
            voice = %voice.name,
            "local TTS voice selection",
        );
        voice
    }

    /// Get the cached engine for `voice`, loading + caching it on first use.
    /// The lock is never held across the (synchronous) load, so concurrent
    /// first-uses of two languages don't serialise; a rare double-load is
    /// resolved by keeping whichever insert wins.
    fn engine_for(&self, voice: &Voice) -> Result<Arc<dyn TextToSpeech>> {
        let span = current_span("tts.engine_for", "assistant.tts", "tts");
        if let Some(e) = self.cache.lock().expect("router cache mutex poisoned").get(&voice.name) {
            span.finish(json!({ "voice": voice.name, "cache_hit": true }));
            return Ok(Arc::clone(e));
        }
        let engine = load_engine(&self.voices_dir, voice)?;
        let mut cache = self.cache.lock().expect("router cache mutex poisoned");
        span.finish(json!({ "voice": voice.name, "cache_hit": false, "engine": voice.engine }));
        Ok(Arc::clone(cache.entry(voice.name.clone()).or_insert(engine)))
    }
}

/// Normalise a BCP-47-ish language tag to the catalog's base code: lowercase,
/// dropping any region/script subtag (`en-US` → `en`, `pt_BR` → `pt`).
#[must_use]
pub fn base_lang(lang: &str) -> String {
    let cut = lang.find(['-', '_']).unwrap_or(lang.len());
    lang[..cut].to_ascii_lowercase()
}

/// Normalise + dedup a list of configured language tags to base codes,
/// preserving order (the first entry is the user's primary language).
fn dedup_base_langs(languages: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in languages {
        let b = base_lang(l);
        if !b.is_empty() && !out.contains(&b) {
            out.push(b);
        }
    }
    out
}

/// Map an ISO 639-1 base code to the corresponding `whatlang` language, for
/// the subset of catalog languages `whatlang`'s trigram model can identify.
/// Languages it cannot detect (e.g. `eu`, `sq`, `cy`, `kk`) return `None` and
/// simply do not participate in text detection — those users fall back to the
/// default voice when no `lang` hint is supplied.
fn whatlang_for_base(base: &str) -> Option<Lang> {
    Some(match base {
        "ar" => Lang::Ara,
        "bg" => Lang::Bul,
        "ca" => Lang::Cat,
        "cs" => Lang::Ces,
        "da" => Lang::Dan,
        "de" => Lang::Deu,
        "en" => Lang::Eng,
        "es" => Lang::Spa,
        "fa" => Lang::Pes,
        "fi" => Lang::Fin,
        "fr" => Lang::Fra,
        "hi" => Lang::Hin,
        "hu" => Lang::Hun,
        "it" => Lang::Ita,
        "ka" => Lang::Kat,
        "lv" => Lang::Lav,
        "nl" => Lang::Nld,
        "no" => Lang::Nob,
        "pl" => Lang::Pol,
        "pt" => Lang::Por,
        "ro" => Lang::Ron,
        "ru" => Lang::Rus,
        "sk" => Lang::Slk,
        "sl" => Lang::Slv,
        "sr" => Lang::Srp,
        "sv" => Lang::Swe,
        "tr" => Lang::Tur,
        "uk" => Lang::Ukr,
        "ur" => Lang::Urd,
        "vi" => Lang::Vie,
        "zh" => Lang::Cmn,
        _ => return None,
    })
}

/// Identify the base language of `text`, constrained to the `allowed` base
/// codes. Returns `None` when there is nothing to disambiguate (fewer than two
/// detectable candidates), when detection is unreliable (typically very short
/// text), or when the winning language is not in `allowed` — in every such
/// case the caller keeps the default voice. Constraining the detector to the
/// user's configured languages is what makes single-sentence replies route
/// correctly.
#[must_use]
pub fn detect_base_lang(text: &str, allowed: &[String]) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    // (base_code, whatlang Lang) for the detectable configured languages.
    let candidates: Vec<(String, Lang)> =
        allowed.iter().filter_map(|b| whatlang_for_base(b).map(|l| (b.clone(), l))).collect();
    // With zero or one detectable candidate there is nothing to choose between
    // — the default voice already covers it.
    if candidates.len() < 2 {
        return None;
    }
    let allowlist: Vec<Lang> = candidates.iter().map(|(_, l)| *l).collect();
    let info = Detector::with_allowlist(allowlist).detect(text)?;
    if !info.is_reliable() {
        return None;
    }
    candidates.into_iter().find(|(_, l)| *l == info.lang()).map(|(b, _)| b)
}

/// Pure voice-selection policy (unit-testable without a loaded runtime): a
/// pin or an absent/unmatched hint yields `default_voice`; otherwise the first
/// catalog voice for the hint's base language, falling back to the default.
#[must_use]
pub fn resolve_voice_for_lang(default_voice: &Voice, pinned: bool, lang: Option<&str>) -> Voice {
    resolve_voice_for_lang_engine(default_voice, pinned, lang, None)
}

/// Engine-aware variant of [`resolve_voice_for_lang`]: when `engine` is
/// `Some("piper"/"kokoro")`, the per-language lookup is constrained to that
/// engine's catalog entries (falling back to the default voice when the
/// engine has no voice for the language). `None` is the Auto policy.
#[must_use]
pub fn resolve_voice_for_lang_engine(
    default_voice: &Voice,
    pinned: bool,
    lang: Option<&str>,
    engine: Option<&str>,
) -> Voice {
    if pinned {
        return default_voice.clone();
    }
    let Some(lang) = lang.map(base_lang).filter(|s| !s.is_empty()) else {
        return default_voice.clone();
    };
    if lang == base_lang(&default_voice.language) {
        return default_voice.clone();
    }
    let found = engine.map_or_else(
        || crate::voices::for_language(&lang).ok().flatten(),
        |e| crate::voices::for_language_engine(&lang, e).ok().flatten(),
    );
    found.unwrap_or_else(|| default_voice.clone())
}

/// Resolve an explicit per-call voice name (the resolver's chosen
/// palette voice, e.g. `"am_michael"`) to its catalog [`Voice`]. Empty
/// or unknown names yield `None`, so the caller falls back to language
/// routing. Pure (catalog lookup only) and unit-testable.
#[must_use]
pub fn resolve_explicit_voice(voice: Option<&str>) -> Option<Voice> {
    let name = voice.map(str::trim).filter(|s| !s.is_empty())?;
    crate::voices::by_name(name).ok().flatten()
}

/// Load the engine for a voice from its cached assets under `voices_dir`,
/// dispatching on `voice.engine`: Kokoro voices (English) use a shared `.ort`
/// model plus a per-voice style pack; Piper voices use a `.ort` model plus a
/// `.onnx.json` config sidecar. A missing asset yields an actionable error
/// (the daemon downloads voices at startup; see `ensure_local_tts`).
///
/// Public so out-of-crate tooling (e.g. `fono-bench`'s TTS backend benchmark)
/// can construct a single Piper/Kokoro voice **directly**, bypassing
/// [`LocalRouter`]'s language auto-routing, to measure exactly the intended
/// backend.
pub fn load_engine(voices_dir: &Path, voice: &Voice) -> Result<Arc<dyn TextToSpeech>> {
    let model_path = voices_dir.join(&voice.model.file);
    if !model_path.is_file() {
        return Err(not_downloaded(voice, voices_dir));
    }
    // Per-voice espeak-ng data is materialised under a stable subdir so it is
    // written once and reused across runs and across voices.
    let espeak_dir = voices_dir.join("espeak");
    // Kokoro is the English engine; every other engine string is Piper
    // (ADR 0033). Dispatch on the catalog's `engine` field.
    if voice.engine == "kokoro" {
        let style = voice.style.as_ref().ok_or_else(|| {
            anyhow!("kokoro voice {:?} has no style asset in the catalog", voice.name)
        })?;
        let style_path = voices_dir.join(&style.file);
        if !style_path.is_file() {
            return Err(not_downloaded(voice, voices_dir));
        }
        // The catalog declares the espeak accent for Kokoro (no sidecar);
        // default to en-us so a malformed entry still phonemizes English.
        let accent = voice.espeak_voice.clone().unwrap_or_else(|| "en-us".to_string());
        let engine = KokoroLocal::load(&model_path, &style_path, accent, espeak_dir)?;
        return Ok(Arc::new(engine));
    }
    let config = voice.config.as_ref().ok_or_else(|| {
        anyhow!("piper voice {:?} has no config (.onnx.json) asset in the catalog", voice.name)
    })?;
    let config_path = voices_dir.join(&config.file);
    if !config_path.is_file() {
        return Err(not_downloaded(voice, voices_dir));
    }
    let cfg_bytes = std::fs::read(&config_path)
        .map_err(|e| anyhow!("read voice config {}: {e}", config_path.display()))?;
    let piper_cfg = PiperConfig::from_json(&cfg_bytes)?;
    let engine = PiperLocal::load(&model_path, piper_cfg, espeak_dir)?;
    Ok(Arc::new(engine))
}

/// Actionable error for a voice whose assets are not yet on disk.
fn not_downloaded(voice: &Voice, voices_dir: &Path) -> anyhow::Error {
    anyhow!(
        "local voice {:?} is not downloaded yet (expected {} and its companion asset under {}); \
         it is fetched automatically at daemon startup — restart the daemon or check the \
         logs / network",
        voice.name,
        voice.model.file,
        voices_dir.display()
    )
}

#[async_trait]
impl TextToSpeech for LocalRouter {
    async fn synthesize(
        &self,
        text: &str,
        voice_override: Option<&str>,
        lang: Option<&str>,
    ) -> Result<TtsAudio> {
        if text.trim().is_empty() {
            return Ok(TtsAudio { pcm: Vec::new(), sample_rate: self.native_rate });
        }
        let route_span = current_span("tts.local_router", "assistant.tts", "tts");
        let voice = self.voice_for(text, voice_override, lang);
        let engine = self.engine_for(&voice)?;
        // The chosen engine owns the voice; its own `synthesize` does the
        // phonemize + ONNX inference (off the async runtime via spawn_blocking).
        let audio = engine.synthesize(text, None, None).await?;
        route_span.finish(json!({
            "voice": voice.name,
            "engine": voice.engine,
            "chars": text.chars().count(),
            "sample_rate": audio.sample_rate,
            "samples": audio.pcm.len(),
        }));
        Ok(audio)
    }

    fn name(&self) -> &'static str {
        // The router dispatches to whichever engine (Piper or Kokoro)
        // owns the chosen voice, so a "piper-"prefixed name would be a
        // misnomer for Kokoro synthesis. The per-call engine is logged
        // separately on the "local TTS voice selection" line.
        "local"
    }

    fn native_sample_rate(&self) -> u32 {
        self.native_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(name: &str, language: &str) -> Voice {
        // serde_json keeps the test fixture compact and exercises the real
        // Deserialize path the catalog uses.
        serde_json::from_value(serde_json::json!({
            "name": name,
            "engine": "piper",
            "language": language,
            "ort_version": "1.24.2",
            "release_tag": "ort-1.24.2",
            "model": { "file": format!("{name}.ort"), "sha256": "0".repeat(64), "size": 1 },
            "config": { "file": format!("{name}.onnx.json"), "sha256": "0".repeat(64), "size": 1 }
        }))
        .unwrap()
    }

    #[test]
    fn base_lang_strips_region_and_lowercases() {
        assert_eq!(base_lang("en"), "en");
        assert_eq!(base_lang("en-US"), "en");
        assert_eq!(base_lang("pt_BR"), "pt");
        assert_eq!(base_lang("RO"), "ro");
        assert_eq!(base_lang(""), "");
    }

    #[test]
    fn pin_always_returns_default_voice() {
        let def = voice("en_US-amy-medium", "en");
        // Even with a Romanian hint, a pinned voice wins.
        let got = resolve_voice_for_lang(&def, true, Some("ro"));
        assert_eq!(got.name, "en_US-amy-medium");
    }

    #[test]
    fn absent_hint_returns_default_voice() {
        let def = voice("en_US-amy-medium", "en");
        assert_eq!(resolve_voice_for_lang(&def, false, None).name, "en_US-amy-medium");
        assert_eq!(resolve_voice_for_lang(&def, false, Some("  ")).name, "en_US-amy-medium");
    }

    #[test]
    fn matching_default_language_keeps_default_voice() {
        // A user whose default is en_GB-alan but whose reply is tagged "en"
        // (or "en-US") must keep their chosen English voice, not be re-routed
        // to whichever English voice the catalog lists first.
        let def = voice("en_GB-alan-medium", "en");
        assert_eq!(resolve_voice_for_lang(&def, false, Some("en")).name, "en_GB-alan-medium");
        assert_eq!(resolve_voice_for_lang(&def, false, Some("en-US")).name, "en_GB-alan-medium");
    }

    #[test]
    fn romanian_hint_routes_to_romanian_catalog_voice() {
        let def = voice("en_US-amy-medium", "en");
        let got = resolve_voice_for_lang(&def, false, Some("ro"));
        assert_eq!(got.language, "ro", "ro hint must route to a Romanian catalog voice");
        assert_eq!(got.name, "ro_RO-mihai-medium");
    }

    #[test]
    fn unknown_language_falls_back_to_default() {
        let def = voice("en_US-amy-medium", "en");
        // "xx" is not in the catalog; keep the default rather than erroring.
        assert_eq!(resolve_voice_for_lang(&def, false, Some("xx")).name, "en_US-amy-medium");
    }

    fn langs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn dedup_base_langs_normalises_and_dedups_preserving_order() {
        assert_eq!(dedup_base_langs(&langs(&["en-US", "ro_RO", "en"])), vec!["en", "ro"]);
        assert_eq!(dedup_base_langs(&langs(&["RO", ""])), vec!["ro"]);
        assert!(dedup_base_langs(&[]).is_empty());
    }

    #[test]
    fn detect_routes_romanian_text_to_ro() {
        // A real Romanian sentence against an en/ro user must detect `ro`,
        // which is the whole point: the MCP path supplies no lang hint.
        let allowed = langs(&["en", "ro"]);
        let text = "Bună ziua, astăzi vom vorbi despre cum funcționează acest program.";
        assert_eq!(detect_base_lang(text, &allowed).as_deref(), Some("ro"));
    }

    #[test]
    fn detect_routes_english_text_to_en() {
        let allowed = langs(&["en", "ro"]);
        let text = "Good afternoon, today we will talk about how this program works.";
        assert_eq!(detect_base_lang(text, &allowed).as_deref(), Some("en"));
    }

    #[test]
    fn detect_returns_none_without_two_detectable_candidates() {
        // Single configured language: nothing to disambiguate, keep default.
        assert!(detect_base_lang("orice text aici", &langs(&["ro"])).is_none());
        // `eu` (Basque) is not in whatlang's model, so en+eu collapses to one
        // detectable candidate -> no detection.
        assert!(detect_base_lang("hello there friend", &langs(&["en", "eu"])).is_none());
    }

    #[test]
    fn detect_returns_none_for_empty_text() {
        assert!(detect_base_lang("   ", &langs(&["en", "ro"])).is_none());
    }

    #[test]
    fn whatlang_for_base_maps_known_and_skips_unknown() {
        assert_eq!(whatlang_for_base("ro"), Some(Lang::Ron));
        assert_eq!(whatlang_for_base("en"), Some(Lang::Eng));
        assert_eq!(whatlang_for_base("zh"), Some(Lang::Cmn));
        assert!(whatlang_for_base("eu").is_none());
        assert!(whatlang_for_base("xx").is_none());
    }

    #[test]
    fn explicit_voice_resolves_known_catalog_name() {
        // A real Kokoro male voice resolves to its catalog entry.
        let v = resolve_explicit_voice(Some("am_michael")).expect("am_michael is in the catalog");
        assert_eq!(v.name, "am_michael");
        assert_eq!(v.engine, "kokoro");
    }

    #[test]
    fn explicit_voice_unknown_or_empty_is_none() {
        assert!(resolve_explicit_voice(Some("definitely-not-a-voice")).is_none());
        assert!(resolve_explicit_voice(Some("   ")).is_none());
        assert!(resolve_explicit_voice(None).is_none());
    }

    #[test]
    fn engine_pin_constrains_language_routing_to_that_engine() {
        // Default is a Romanian Piper voice. Under a Piper pin an English hint
        // stays within Piper (a Piper English voice), never crossing to Kokoro.
        let def = voice("ro_RO-mihai-medium", "ro");
        let got = resolve_voice_for_lang_engine(&def, false, Some("en"), Some("piper"));
        assert_eq!(got.engine, "piper", "piper pin must resolve a piper voice");

        // Under Auto (no pin) the same English hint routes to the catalog's
        // English (Kokoro) voice per the ADR 0033 policy.
        let auto = resolve_voice_for_lang_engine(&def, false, Some("en"), None);
        assert_eq!(auto.engine, "kokoro", "auto policy routes English to Kokoro");
    }

    #[test]
    fn kokoro_pin_routes_english_within_kokoro() {
        // A Kokoro-pinned English default keeps routing English within Kokoro.
        let def = voice("af_heart", "en");
        let got = resolve_voice_for_lang_engine(&def, false, Some("en"), Some("kokoro"));
        assert_eq!(got.name, "af_heart");
    }
}
