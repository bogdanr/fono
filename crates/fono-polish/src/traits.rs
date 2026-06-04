// SPDX-License-Identifier: GPL-3.0-only
//! `TextFormatter` trait — cleanup a raw STT string into polished text.

use anyhow::Result;
use async_trait::async_trait;
use whatlang::{Detector, Lang};

#[derive(Debug, Clone, Default)]
pub struct FormatContext {
    pub main_prompt: String,
    pub advanced_prompt: String,
    pub dictionary: Vec<String>,
    pub rule_suffix: Option<String>,
    pub app_class: Option<String>,
    pub app_title: Option<String>,
    /// Best-effort per-utterance language code reported by the STT
    /// backend (e.g. `"ro"`). When present, cleanup treats it as the
    /// source-language anchor for same-language editing: the formatter
    /// may polish text, but must not translate it.
    pub language: Option<String>,
    /// The user's candidate language set (BCP-47 codes, e.g.
    /// `["ro", "en"]`), auto-populated from OS-locale signals in
    /// `general.languages`. Used when the STT backend did not provide a
    /// concrete source language so the cleanup model can infer the
    /// transcript's language from a bounded set and keep its output in
    /// that language.
    pub candidate_languages: Vec<String>,
}

impl FormatContext {
    /// Build the system prompt to send to the LLM.
    #[must_use]
    pub fn system_prompt(&self) -> String {
        let mut s = String::new();
        if !self.main_prompt.is_empty() {
            s.push_str(&self.main_prompt);
            s.push_str("\n\n");
        }
        if !self.advanced_prompt.is_empty() {
            s.push_str(&self.advanced_prompt);
            s.push_str("\n\n");
        }
        if !self.dictionary.is_empty() {
            s.push_str("Personal dictionary (preserve spelling exactly): ");
            s.push_str(&self.dictionary.join(", "));
            s.push_str("\n\n");
        }
        if let Some(sfx) = &self.rule_suffix {
            s.push_str(sfx);
            s.push_str("\n\n");
        }
        if let Some(directive) = self.language_directive() {
            s.push_str(&directive);
            s.push_str("\n\n");
        }
        s.trim_end().to_string()
    }

    /// Build the language directive appended to the system prompt.
    ///
    /// When STT provides a concrete language, cleanup treats that code
    /// as the source language and frames the task as same-language
    /// copy-editing rather than language detection. Candidate-language
    /// detection is only used when the source language is unknown.
    fn language_directive(&self) -> Option<String> {
        if let Some(source) = self.language.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let name = fono_core::languages::display_name(source);
            return Some(format!(
                "SOURCE_LANGUAGE: {name} ({source}).\n\nThis is a same-language transcription cleanup \
task, not a translation task. Return the cleaned transcript in SOURCE_LANGUAGE only. Preserve \
SOURCE_LANGUAGE even when the transcript mentions other languages, products, commands, names, or \
technical terms. Allowed edits: punctuation, capitalization, diacritics/orthography restoration, \
filler removal, and obvious STT repairs. Forbidden edits: translation, paraphrase, summarization, \
adding facts, or changing the language. If you cannot clean the transcript while preserving \
SOURCE_LANGUAGE, return the original transcript unchanged."
            ));
        }

        if self.candidate_languages.is_empty() {
            return None;
        }
        let names: Vec<&str> = self
            .candidate_languages
            .iter()
            .map(|c| fono_core::languages::display_name(c))
            .collect();
        Some(format!(
            "This transcript is in one of these languages: {}. Detect which one, then perform \
same-language transcription cleanup in that detected language. Return the cleaned transcript \
entirely in the detected source language, and restore its correct orthography — including \
diacritics when that language uses them (for example, Romanian uses ă, â, î, ș, ț). Do not \
translate between these languages or into any other language. If detection is uncertain, return \
the original transcript unchanged.",
            names.join(", ")
        ))
    }
}

/// Wrap the raw transcript in unambiguous fenced delimiters so chat-trained
/// models — cloud or local — cannot mistake the user turn for a
/// conversational message addressed to them. The matching `<<<` / `>>>`
/// markers are referenced by `default_prompt_main` and must stay in sync
/// with it. Applied identically by every `TextFormatter` impl. See
/// `plans/2026-04-28-polish-cleanup-clarification-refusal-fix-v1.md` Task 2.
#[must_use]
pub fn user_prompt(raw: &str) -> String {
    format!(
        "Transcript to clean (return ONLY the cleaned text, no quotes, no commentary):\n<<<\n{raw}\n>>>"
    )
}

/// Heuristic: does `out` look like a chat-style refusal / clarification
/// reply rather than a cleaned transcript? Triggered by the bug where
/// chat-trained LLMs — cloud (Cerebras / Groq Llama-3.3-70B, gpt-4o-mini,
/// Claude Haiku, …) **or** local (llama.cpp Qwen / SmolLM / …) —
/// sometimes respond with *"It seems like you're describing a situation,
/// but the details are incomplete. Could you provide the full text
/// you're referring to…"* on short captures. Applied uniformly by every
/// `TextFormatter` impl; the failure mode is a property of chat
/// fine-tuning, not of any specific provider.
///
/// Returns `true` only when the text begins with one of a small set of
/// telltale openers AND contains a corroborating clarification fragment,
/// to keep false positives low for legitimate transcripts that happen to
/// start with similar words.
#[must_use]
pub fn looks_like_clarification(out: &str) -> bool {
    const OPENERS: &[&str] = &[
        "it seems like you",
        "it looks like you",
        "it sounds like you",
        "it appears that you",
        "could you provide",
        "could you please provide",
        "could you clarify",
        "can you provide",
        "can you clarify",
        "please provide",
        "please clarify",
        "i'm not sure what",
        "i am not sure what",
        "i don't have enough",
        "i do not have enough",
        "i'm sorry, but",
        "i am sorry, but",
        "i need more",
        "to clarify",
    ];

    const TELLS: &[&str] = &[
        "the full text",
        "more context",
        "more information",
        "more details",
        "details are incomplete",
        "what you're referring to",
        "what you are referring to",
        "what you mean",
        "the text you",
        "to assist you",
        "to better understand",
        // Note: "please provide", "please clarify", "could you provide" are intentionally
        // omitted here — they appear in OPENERS already and would create self-referential
        // matches on sentences like "Please provide the report by Friday."
    ];

    let trimmed = out.trim_start_matches(|c: char| !c.is_alphanumeric());
    let lower = trimmed.to_ascii_lowercase();

    let opener_hit = OPENERS.iter().any(|p| lower.starts_with(p));
    if !opener_hit {
        return false;
    }
    TELLS.iter().any(|t| lower.contains(t))
}

/// Heuristic: did a cleanup model change the transcript's language instead of
/// returning the same transcript cleaned in-place?
///
/// The prompt already says "do not translate", but small local instruction
/// models can still produce a translated paraphrase. This guard is intentionally
/// conservative and language-agnostic: it only fires for sufficiently long text
/// when Fono has a source language from the STT backend or reliable text
/// detection, and the cleanup output is reliably detected as another language.
/// Short or ambiguous text is accepted.
#[must_use]
pub fn looks_like_translated_cleanup(raw: &str, out: &str, ctx: &FormatContext) -> bool {
    if !has_enough_text_for_language_guard(raw) || !has_enough_text_for_language_guard(out) {
        return false;
    }

    let candidate_langs = candidate_whatlangs(&ctx.candidate_languages);
    let Some(source_lang) = expected_source_lang(raw, ctx, &candidate_langs) else { return false };
    let Some(output_lang) =
        detect_lang_unconstrained(out).or_else(|| detect_lang_allowed(out, &candidate_langs))
    else {
        return false;
    };

    source_lang != output_lang
}

fn expected_source_lang(raw: &str, ctx: &FormatContext, candidate_langs: &[Lang]) -> Option<Lang> {
    // STT detects language from the audio, while text-only language ID sees the
    // recogniser's imperfect transcript (often diacritic-stripped or garbled).
    // Prefer the audio-derived hint when present; fall back to text detection
    // for backends/configurations that do not provide one.
    ctx.language
        .as_deref()
        .and_then(whatlang_for_code)
        .or_else(|| detect_lang_allowed(raw, candidate_langs))
        .or_else(|| detect_lang_unconstrained(raw))
}

fn detect_lang_allowed(text: &str, allowed: &[Lang]) -> Option<Lang> {
    if allowed.len() < 2 {
        return None;
    }
    let info = Detector::with_allowlist(allowed.to_vec()).detect(text)?;
    reliable_lang(&info).then_some(info.lang())
}

fn detect_lang_unconstrained(text: &str) -> Option<Lang> {
    let info = Detector::new().detect(text)?;
    reliable_lang(&info).then_some(info.lang())
}

fn reliable_lang(info: &whatlang::Info) -> bool {
    info.confidence() >= 0.65
}

fn candidate_whatlangs(codes: &[String]) -> Vec<Lang> {
    let mut out = Vec::new();
    for code in codes {
        if let Some(lang) = whatlang_for_code(code) {
            if !out.contains(&lang) {
                out.push(lang);
            }
        }
    }
    out
}

fn has_enough_text_for_language_guard(text: &str) -> bool {
    let alpha_chars = text.chars().filter(|c| c.is_alphabetic()).count();
    let words = text.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).count();
    let non_ascii_alpha_chars = text.chars().filter(|c| c.is_alphabetic() && !c.is_ascii()).count();

    (alpha_chars >= 24 && words >= 4) || non_ascii_alpha_chars >= 8
}

#[allow(clippy::too_many_lines)]
fn whatlang_for_code(code: &str) -> Option<Lang> {
    let base = lang_base(code).to_ascii_lowercase();
    if let Some(lang) = Lang::from_code(&base) {
        return Some(lang);
    }
    Some(match base.as_str() {
        "af" => Lang::Afr,
        "ak" => Lang::Aka,
        "am" => Lang::Amh,
        "ar" => Lang::Ara,
        "az" => Lang::Aze,
        "be" => Lang::Bel,
        "bg" => Lang::Bul,
        "bn" => Lang::Ben,
        "ca" => Lang::Cat,
        "cs" => Lang::Ces,
        "da" => Lang::Dan,
        "de" => Lang::Deu,
        "el" => Lang::Ell,
        "en" => Lang::Eng,
        "eo" => Lang::Epo,
        "es" => Lang::Spa,
        "et" => Lang::Est,
        "fa" => Lang::Pes,
        "fi" => Lang::Fin,
        "fr" => Lang::Fra,
        "gu" => Lang::Guj,
        "he" | "iw" => Lang::Heb,
        "hi" => Lang::Hin,
        "hr" => Lang::Hrv,
        "hu" => Lang::Hun,
        "hy" => Lang::Hye,
        "id" => Lang::Ind,
        "it" => Lang::Ita,
        "ja" => Lang::Jpn,
        "jv" | "jw" => Lang::Jav,
        "ka" => Lang::Kat,
        "km" => Lang::Khm,
        "kn" => Lang::Kan,
        "ko" => Lang::Kor,
        "la" => Lang::Lat,
        "lt" => Lang::Lit,
        "lv" => Lang::Lav,
        "mk" => Lang::Mkd,
        "ml" => Lang::Mal,
        "mo" | "ro" => Lang::Ron,
        "mr" => Lang::Mar,
        "my" => Lang::Mya,
        "nb" | "no" => Lang::Nob,
        "ne" => Lang::Nep,
        "nl" => Lang::Nld,
        "or" => Lang::Ori,
        "pa" => Lang::Pan,
        "pl" => Lang::Pol,
        "pt" => Lang::Por,
        "ru" => Lang::Rus,
        "si" => Lang::Sin,
        "sk" => Lang::Slk,
        "sl" => Lang::Slv,
        "sn" => Lang::Sna,
        "sr" => Lang::Srp,
        "sv" => Lang::Swe,
        "ta" => Lang::Tam,
        "te" => Lang::Tel,
        "th" => Lang::Tha,
        "tk" => Lang::Tuk,
        "tl" => Lang::Tgl,
        "tr" => Lang::Tur,
        "uk" => Lang::Ukr,
        "ur" => Lang::Urd,
        "uz" => Lang::Uzb,
        "vi" => Lang::Vie,
        "yi" => Lang::Yid,
        "zh" => Lang::Cmn,
        "zu" => Lang::Zul,
        _ => return None,
    })
}

fn lang_base(lang: &str) -> &str {
    let trimmed = lang.trim();
    let cut = trimmed.find(['-', '_']).unwrap_or(trimmed.len());
    &trimmed[..cut]
}

#[async_trait]
pub trait TextFormatter: Send + Sync {
    async fn format(&self, raw: &str, ctx: &FormatContext) -> Result<String>;
    fn name(&self) -> &'static str;

    /// Optional best-effort warmup. See `SpeechToText::prewarm`. Latency
    /// plan L3 / L10.
    async fn prewarm(&self) -> Result<()> {
        Ok(())
    }

    /// True for backends that run entirely on the local machine
    /// (llama.cpp, future Ollama). Cloud backends (OpenAI-compat,
    /// Anthropic, Groq, Cerebras, OpenRouter) leave this at the
    /// `false` default.
    ///
    /// Used by the orchestrator to decide whether the post-release
    /// "polishing" overlay should run the synthetic animation:
    /// multi-second local cleanup benefits from active feedback,
    /// sub-second cloud cleanup would just flash.
    fn is_local(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_prompt_wraps_raw_with_fences() {
        let p = user_prompt("hello world");
        assert!(p.contains("<<<\nhello world\n>>>"));
        assert!(p.starts_with("Transcript to clean"));
    }

    #[test]
    fn user_prompt_preserves_payload_verbatim() {
        let raw = "  weird   spacing\nand\ttabs ";
        let p = user_prompt(raw);
        assert!(p.contains(raw), "payload must round-trip unchanged");
    }

    #[test]
    fn detects_exact_bug_report_reply() {
        let s = "It seems like you're describing a situation, but the details are \
                 incomplete. Could you provide the full text you're referring to, so \
                 I can better understand and assist you?";
        assert!(looks_like_clarification(s));
    }

    #[test]
    fn detects_paraphrased_clarifications() {
        let cases = [
            "Could you please provide more context so I can help?",
            "I'm not sure what you mean — could you clarify?",
            "Please provide the full text you would like cleaned up.",
            "It looks like you might be missing some details. Could you provide more information?",
            "I don't have enough information to assist you.",
        ];
        for c in cases {
            assert!(looks_like_clarification(c), "should flag: {c}");
        }
    }

    #[test]
    fn does_not_flag_legitimate_transcripts() {
        let cases = [
            "It seems like the meeting is at three.",
            "Could you grab the milk on your way home?",
            "Please provide the report by Friday.",
            "I'm not sure if I'll make it tonight.",
            "It looks like rain.",
            "okay",
            "send it",
            "yes",
            "",
            "The details are incomplete on the form he sent over.",
        ];
        for c in cases {
            assert!(!looks_like_clarification(c), "should NOT flag legitimate transcript: {c}");
        }
    }

    #[test]
    fn detector_ignores_leading_whitespace_and_punctuation() {
        let s = "\n  \"It seems like you're missing context. Could you provide more details?\"";
        assert!(looks_like_clarification(s));
    }

    #[test]
    fn rejects_cleanup_when_romanian_hint_translates_to_english() {
        let ctx = FormatContext {
            language: Some("ro".into()),
            candidate_languages: vec!["ro".into(), "en".into()],
            ..Default::default()
        };
        let raw = "O să dictesc ceva în limba română ca să văd că nu se traduce din greșeala în limba ingleza.";
        let out =
            "I will speak in Romanian as I see that it is not translated correctly into English.";
        assert!(looks_like_translated_cleanup(raw, out, &ctx));
    }

    #[test]
    fn rejects_cleanup_when_diacriticless_hint_translates_to_english() {
        let ctx = FormatContext {
            language: Some("ro".into()),
            candidate_languages: vec!["ro".into(), "en".into()],
            ..Default::default()
        };
        let raw = "o sa facem un test sa vedem daca sta face din limba romanana, limba inglesa";
        let out = "Let's make a test to see if it's in Romanian, English.";
        assert!(looks_like_translated_cleanup(raw, out, &ctx));
    }

    #[test]
    fn rejects_cleanup_when_spanish_hint_translates_to_english() {
        let ctx = FormatContext {
            language: Some("es".into()),
            candidate_languages: vec!["es".into(), "en".into()],
            ..Default::default()
        };
        let raw = "Voy a dictar una frase en español para comprobar que no se traduzca al inglés.";
        let out =
            "I will dictate a sentence in Spanish to check that it is not translated into English.";
        assert!(looks_like_translated_cleanup(raw, out, &ctx));
    }

    #[test]
    fn rejects_cleanup_when_japanese_hint_translates_to_english() {
        let ctx = FormatContext {
            language: Some("ja".into()),
            candidate_languages: vec!["ja".into(), "en".into()],
            ..Default::default()
        };
        let raw = "今日は日本語で文章を音声入力して、英語に翻訳されないことを確認します。";
        let out =
            "Today I will dictate a sentence in Japanese and confirm it is not translated into English.";
        assert!(looks_like_translated_cleanup(raw, out, &ctx));
    }

    #[test]
    fn accepts_cleanup_kept_in_source_language() {
        let ctx = FormatContext {
            language: Some("es".into()),
            candidate_languages: vec!["es".into(), "en".into()],
            ..Default::default()
        };
        let raw = "Voy a dictar una frase en espanol para comprobar que no se traduzca.";
        let out = "Voy a dictar una frase en español para comprobar que no se traduzca.";
        assert!(!looks_like_translated_cleanup(raw, out, &ctx));
    }

    #[test]
    fn accepts_english_cleanup() {
        let ctx = FormatContext {
            language: Some("en".into()),
            candidate_languages: vec!["ro".into(), "en".into()],
            ..Default::default()
        };
        let raw = "I will dictate something in English and see whether it cleans correctly";
        let out = "I will dictate something in English and see whether it cleans correctly.";
        assert!(!looks_like_translated_cleanup(raw, out, &ctx));
    }

    #[test]
    fn accepts_source_language_that_mentions_another_language() {
        let ctx = FormatContext {
            language: Some("ro".into()),
            candidate_languages: vec!["ro".into(), "en".into()],
            ..Default::default()
        };
        let raw = "Vreau să văd dacă limba română nu devine limba engleză.";
        let out = "Vreau să văd dacă limba română nu devine limba engleză.";
        assert!(!looks_like_translated_cleanup(raw, out, &ctx));
    }

    #[test]
    fn language_guard_accepts_short_or_ambiguous_text() {
        let ctx = FormatContext {
            language: Some("es".into()),
            candidate_languages: vec!["es".into(), "en".into()],
            ..Default::default()
        };
        assert!(!looks_like_translated_cleanup("hola", "hello", &ctx));
    }

    #[test]
    fn whatlang_mapping_covers_curated_language_codes() {
        for (code, _) in fono_core::languages::CURATED_LANGUAGES {
            assert!(whatlang_for_code(code).is_some(), "missing whatlang mapping for {code}");
        }
    }
    #[test]
    fn directive_names_candidates_and_mentions_diacritics() {
        let ctx = FormatContext {
            candidate_languages: vec!["ro".into(), "en".into()],
            ..Default::default()
        };
        let sp = ctx.system_prompt();
        assert!(sp.contains("Romanian"), "directive must name Romanian: {sp}");
        assert!(sp.contains("English"), "directive must name English: {sp}");
        assert!(sp.contains("diacritics"), "directive must mention diacritics: {sp}");
        assert!(sp.contains("ă, â, î, ș, ț"), "directive must list Romanian diacritics: {sp}");
        assert!(sp.contains("Do not translate"), "directive must forbid translation: {sp}");
    }

    #[test]
    fn source_language_contract_applies_even_without_candidates() {
        let ctx = FormatContext { language: Some("ro".into()), ..Default::default() };
        let sp = ctx.system_prompt();
        assert!(sp.contains("SOURCE_LANGUAGE: Romanian (ro)."), "source contract missing: {sp}");
        assert!(
            sp.contains("same-language transcription cleanup task"),
            "source contract must frame cleanup as same-language editing: {sp}"
        );
        assert!(
            sp.contains("not a translation task"),
            "source contract must forbid translation: {sp}"
        );
        assert!(
            sp.contains("return the original transcript unchanged"),
            "source contract must define safe fallback: {sp}"
        );
        assert!(
            !sp.contains("Detect which one"),
            "known source language must not ask for detection"
        );
        assert!(!sp.contains("It is most likely"), "known source language must not be a soft hint");
    }

    #[test]
    fn source_language_contract_overrides_candidate_detection() {
        let ctx = FormatContext {
            candidate_languages: vec!["ro".into(), "en".into()],
            language: Some("ro".into()),
            ..Default::default()
        };
        let sp = ctx.system_prompt();
        assert!(sp.contains("SOURCE_LANGUAGE: Romanian (ro)."), "source contract missing: {sp}");
        assert!(sp.contains("Preserve SOURCE_LANGUAGE"), "source language must be preserved: {sp}");
        assert!(
            !sp.contains("Detect which one"),
            "known source language must not ask for detection: {sp}"
        );
        assert!(
            !sp.contains("It is most likely"),
            "known source language must not be a soft hint: {sp}"
        );
    }

    #[test]
    fn candidate_detection_used_only_when_source_language_unknown() {
        let ctx = FormatContext {
            candidate_languages: vec!["ro".into(), "en".into()],
            ..Default::default()
        };
        let sp = ctx.system_prompt();
        assert!(sp.contains("This transcript is in one of"), "candidate directive missing: {sp}");
        assert!(
            sp.contains("Detect which one"),
            "unknown source language should ask for detection: {sp}"
        );
        assert!(!sp.contains("SOURCE_LANGUAGE:"), "no source contract without STT language: {sp}");
        assert!(!sp.contains("It is most likely"), "no soft hint when language is None: {sp}");
    }
}
