// SPDX-License-Identifier: GPL-3.0-only
//! `TextFormatter` trait — cleanup a raw STT string into polished text.

use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone, Default)]
pub struct FormatContext {
    pub main_prompt: String,
    pub advanced_prompt: String,
    pub dictionary: Vec<String>,
    pub rule_suffix: Option<String>,
    pub app_class: Option<String>,
    pub app_title: Option<String>,
    /// Best-effort per-utterance language code reported by the STT
    /// backend (e.g. `"ro"`). Engine-dependent and often `None` — a
    /// soft hint only, never load-bearing. See
    /// `plans/2026-05-29-romanian-dictation-polish-reconstruction-v2.md`.
    pub language: Option<String>,
    /// The user's candidate language set (BCP-47 codes, e.g.
    /// `["ro", "en"]`), auto-populated from OS-locale signals in
    /// `general.languages`. Always available regardless of STT
    /// engine; the reliable signal the cleanup directive leans on so
    /// the LLM can detect the transcript's language, keep its output
    /// in that language, and restore diacritics.
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

    /// Build the candidate-language directive appended to the system
    /// prompt. Returns `None` when no candidate set is configured (so
    /// the prompt stays byte-identical to today's on exotic systems
    /// where locale detection yielded nothing).
    ///
    /// When the set is non-empty, instructs the model to detect which
    /// of the candidate languages the transcript is in, keep its
    /// output in that language, and restore its orthography — every
    /// diacritic included. When the soft `language` hint is present
    /// and its code is one of the candidates, an "It is most likely
    /// <Name>." sentence is appended to bias the choice. See
    /// `plans/2026-05-29-romanian-dictation-polish-reconstruction-v2.md`.
    fn language_directive(&self) -> Option<String> {
        if self.candidate_languages.is_empty() {
            return None;
        }
        let names: Vec<&str> = self
            .candidate_languages
            .iter()
            .map(|c| fono_core::languages::display_name(c))
            .collect();
        let mut directive = format!(
            "This transcript is in one of these languages: {}. Detect which one, keep your \
output entirely in that language, and restore its correct orthography — including all \
diacritics (for example, Romanian uses ă, â, î, ș, ț). Do not translate between these \
languages.",
            names.join(", ")
        );
        if let Some(hint) = &self.language {
            if self.candidate_languages.iter().any(|c| c == hint) {
                use std::fmt::Write as _;
                let _ = write!(
                    directive,
                    " It is most likely {}.",
                    fono_core::languages::display_name(hint)
                );
            }
        }
        Some(directive)
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
    fn directive_absent_when_candidate_set_empty() {
        let ctx = FormatContext { language: Some("ro".into()), ..Default::default() };
        let sp = ctx.system_prompt();
        assert!(!sp.contains("This transcript is in one of"), "no directive without candidates");
        assert!(!sp.contains("It is most likely"), "no soft-hint sentence without candidates");
    }

    #[test]
    fn soft_hint_sentence_only_when_hint_in_candidate_set() {
        // Hint inside the set ⇒ the "most likely" sentence appears.
        let in_set = FormatContext {
            candidate_languages: vec!["ro".into(), "en".into()],
            language: Some("ro".into()),
            ..Default::default()
        };
        let sp = in_set.system_prompt();
        assert!(sp.contains("It is most likely Romanian."), "soft hint should appear: {sp}");

        // Hint outside the set ⇒ no soft-hint sentence, directive still present.
        let out_of_set = FormatContext {
            candidate_languages: vec!["ro".into(), "en".into()],
            language: Some("fr".into()),
            ..Default::default()
        };
        let sp = out_of_set.system_prompt();
        assert!(sp.contains("This transcript is in one of"), "directive still present: {sp}");
        assert!(!sp.contains("It is most likely"), "no soft hint for out-of-set code: {sp}");

        // No hint at all ⇒ directive present, no soft-hint sentence.
        let no_hint = FormatContext {
            candidate_languages: vec!["ro".into(), "en".into()],
            ..Default::default()
        };
        let sp = no_hint.system_prompt();
        assert!(!sp.contains("It is most likely"), "no soft hint when language is None: {sp}");
    }
}
