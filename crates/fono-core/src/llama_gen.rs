// SPDX-License-Identifier: GPL-3.0-only
//! Shared generation policy for the embedded llama.cpp backends.
//!
//! Both embedded-LLM paths — `fono-polish` (F7 cleanup) and
//! `fono-assistant` (F8 chat / `fono summarize`) — decode with the SAME
//! sampler and stop rules defined here. History shows why this must be
//! one definition: the polish backend fixed the Gemma verbatim-repetition
//! loop (repetition penalty) and the `gemma-4-e2b` dead-stop-token bug
//! (Control-attribute stop) in 2026-05, but the assistant kept its own
//! copy of the decode loop and shipped without either fix — observed as a
//! refusal sentence repeated to the 384-token cap (~13 s) on
//! `fono summarize`. Any future decoding fix lands here, once, for both.
//!
//! ## The two rules
//!
//! **Sampler** ([`generation_sampler`]): greedy decoding with a repetition
//! penalty over *generated tokens only*. Cleanup/summary output closely
//! mirrors the prompt — exactly the condition where pure greedy decoding
//! degenerates into an infinite verbatim loop: once the model reproduces
//! the (near-echo) input, the highest-probability continuation is to
//! reproduce it AGAIN, so it never emits its end-of-turn token and runs
//! to the token cap. llama.cpp's penalty sampler only sees tokens passed
//! to `sampler.accept()`, and the backends accept ONLY generated tokens
//! (prefill goes through `ctx.decode`), so the penalty discourages the
//! model from repeating ITS OWN output without penalising faithful reuse
//! of prompt content. A modest `repeat = 1.3` over the recent window
//! breaks the loop while staying deterministic: greedy still picks the
//! argmax of the penalised logits.
//!
//! **Stop predicate** ([`is_control_token`]): stop the moment the model
//! samples ANY token tagged `LlamaTokenAttr::Control`, regardless of how
//! that marker is spelled in this model's vocabulary. This is deliberately
//! model-agnostic instead of matching literal strings: `gemma-4-e2b`'s
//! turn markers are NOT the standard `<start_of_turn>` / `<end_of_turn>`
//! — they tokenize as `<|turn>` (id 105, control, NOT eog) and `<turn|>`
//! (id 106, control + eog). Literal `single_token("<end_of_turn>")`
//! lookups return `None` on that vocab (the literals tokenize as plain
//! text), so every string-based stop check is dead code, and
//! `token_to_piece(special = false)` renders the real control tokens as
//! empty text so textual scans can't see them either. The `Control`
//! attribute catches all of these (105, 106, eos, bos) while letting
//! ordinary newline tokens through.
//!
//! The textual [`STOP_MARKERS`] scan remains as belt-and-braces for
//! models that spell turn markers as plain text, with
//! [`safe_stream_end`] holding back any partially-streamed marker.
//!
//! [`warn_on_template_vocab_mismatch`] is the load-time tripwire for the
//! next model switch: it warns prominently when the hand-rolled template
//! a backend selected emits markers the loaded vocabulary does not treat
//! as control tokens (the `gemma-4-e2b` anomaly stayed invisible until
//! someone debugged a 13-second loop).

use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::token_type::LlamaTokenAttr;
use tracing::warn;

/// Window of recent generated tokens the repetition penalty considers.
pub const PENALTY_LAST_N: i32 = 128;
/// Multiplicative repetition penalty. Modest by design: strong enough to
/// break a verbatim self-repetition loop, weak enough to keep greedy
/// decoding faithful (it must not stop the model from legitimately
/// reusing words from the prompt).
pub const PENALTY_REPEAT: f32 = 1.3;

/// Stop-marker spellings shared by the supported template families, used
/// for the textual belt-and-braces scan. Union of the Gemma and ChatML
/// (plus common EOG) spellings.
pub const STOP_MARKERS: &[&str] = &[
    "<end_of_turn>",
    "<start_of_turn>",
    "<|im_end|>",
    "<|end|>",
    "<|eot_id|>",
    "<|endoftext|>",
    "</s>",
];

/// The shared sampler: repetition penalty (generated tokens only — the
/// caller must `accept()` exactly the generated tokens) feeding greedy.
/// Deterministic. See the module docs for why bare greedy is not enough.
#[must_use]
pub fn generation_sampler() -> LlamaSampler {
    LlamaSampler::chain_simple([
        LlamaSampler::penalties(PENALTY_LAST_N, PENALTY_REPEAT, 0.0, 0.0),
        LlamaSampler::greedy(),
    ])
}

/// Model-agnostic stop predicate: `true` for any token the vocabulary
/// tags as a control token (turn markers, BOS/EOS, end-of-generation),
/// however it is spelled. See the module docs for the `gemma-4-e2b`
/// evidence behind attribute matching over literal-string matching.
#[must_use]
pub fn is_control_token(model: &LlamaModel, token: LlamaToken) -> bool {
    model.token_attr(token).contains(LlamaTokenAttr::Control)
}

/// Byte offset and spelling of the earliest [`STOP_MARKERS`] occurrence
/// in `text`, or `None`. Catches template markers that round-trip as
/// plain text instead of registered control tokens.
#[must_use]
pub fn first_stop_marker(text: &str) -> Option<(usize, &'static str)> {
    STOP_MARKERS
        .iter()
        .filter_map(|marker| text.find(marker).map(|idx| (idx, *marker)))
        .min_by_key(|(idx, _)| *idx)
}

/// Byte offset up to which `text` can be streamed without risking
/// emitting a partial stop marker. Holds back the longest suffix of
/// `text` that is also a non-empty prefix of any [`STOP_MARKERS`] entry,
/// so a marker split across several token pieces (e.g. `<end` then
/// `_of_turn>`) is never partially surfaced to the consumer.
#[must_use]
pub fn safe_stream_end(text: &str) -> usize {
    let keep = STOP_MARKERS
        .iter()
        .filter_map(|marker| longest_marker_prefix_suffix(text, marker))
        .max()
        .unwrap_or(0);
    text.len().saturating_sub(keep)
}

/// Length of the longest suffix of `text` that is a proper non-empty
/// prefix of `marker` (on a char boundary). `None` when there is no
/// overlap.
fn longest_marker_prefix_suffix(text: &str, marker: &str) -> Option<usize> {
    let max = text.len().min(marker.len().saturating_sub(1));
    (1..=max)
        .rev()
        .find(|&len| text.is_char_boundary(text.len() - len) && text.ends_with(&marker[..len]))
}

/// Hand-rolled chat-template family a backend selects for a local GGUF,
/// keyed off the model file name. The fully general alternative —
/// rendering via the GGUF's embedded `tokenizer.chat_template` metadata —
/// is deferred: the prompt-state cache's textual prefix/suffix split and
/// pinned-base invariants are built on these hand-rolled templates, so
/// adopting embedded templates needs its own design pass to preserve
/// cacheability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateFamily {
    Gemma,
    ChatMl,
}

/// Template family for `model_name` (a GGUF file stem). Mirrors the
/// dispatch both backends use: Gemma-named models get the Gemma turn
/// markers, everything else falls through to ChatML.
#[must_use]
pub fn template_family(model_name: &str) -> TemplateFamily {
    if model_name.to_ascii_lowercase().contains("gemma") {
        TemplateFamily::Gemma
    } else {
        TemplateFamily::ChatMl
    }
}

/// Whether `model_name` matches a family the hand-rolled templates were
/// actually written for. Anything else still *works* (ChatML fallback)
/// but deserves a load-time warning — see
/// [`warn_on_template_vocab_mismatch`].
#[must_use]
pub fn is_recognized_model_name(model_name: &str) -> bool {
    let name = model_name.to_ascii_lowercase();
    name.contains("gemma") || name.contains("qwen")
}

/// The open/close turn markers a model's chat template uses. The hand-rolled
/// templates frame every turn as `{open}{role}\n{content}{close}\n`, so these
/// two strings are the only thing that varies between otherwise
/// structurally-identical vocabularies.
///
/// Most Gemma builds and all ChatML builds spell their markers the obvious
/// way, but the `gemma-4` line ships NON-standard markers — `<|turn>` (id
/// 105, control) opens and `<turn|>` (id 106, control + eog) closes. Emitting
/// the literal `<start_of_turn>` / `<end_of_turn>` against that vocabulary
/// tokenizes as 7 plain-text pieces instead of one control token, degrading
/// prompt fidelity (the anomaly behind [`warn_on_template_vocab_mismatch`]).
/// Selecting the spelling per model here is the whole fix: rendering stays
/// deterministic and append-only, so the prompt-state cache is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnMarkers {
    /// Opens a turn, immediately followed by the role word (e.g. `user`).
    pub open: &'static str,
    /// Closes a turn.
    pub close: &'static str,
}

impl TurnMarkers {
    /// Standard Gemma 1/2/3 markers.
    pub const GEMMA: Self = Self { open: "<start_of_turn>", close: "<end_of_turn>" };
    /// Gemma 4 ships non-standard markers registered as control tokens.
    pub const GEMMA_4: Self = Self { open: "<|turn>", close: "<turn|>" };
    /// ChatML / Qwen / SmolLM markers.
    pub const CHATML: Self = Self { open: "<|im_start|>", close: "<|im_end|>" };
}

/// The turn markers `model_name` actually registers as control tokens. Keyed
/// off the same name dispatch as [`template_family`]; the only special case is
/// the `gemma-4` line, whose real markers differ from the rest of the Gemma
/// family. Any unrecognized model falls through to its family default and is
/// surfaced by [`warn_on_template_vocab_mismatch`] if the spelling is wrong.
#[must_use]
pub fn turn_markers(model_name: &str) -> TurnMarkers {
    let name = model_name.to_ascii_lowercase();
    match template_family(model_name) {
        TemplateFamily::Gemma if name.contains("gemma-4") => TurnMarkers::GEMMA_4,
        TemplateFamily::Gemma => TurnMarkers::GEMMA,
        TemplateFamily::ChatMl => TurnMarkers::CHATML,
    }
}

/// Load-time template/vocab tripwire. Call once per model load (e.g.
/// from `ensure_loaded`) with the loaded model and its file stem.
///
/// Warns when:
/// - the markers the selected template family will emit do not tokenize
///   to a single control token in this vocabulary (the template text
///   will prefill as plain prose and the model's real turn markers are
///   spelled differently — the `gemma-4-e2b` anomaly), or
/// - the model name matches no recognized family and the backend is
///   falling through to the ChatML default.
///
/// Diagnostic only: never changes behaviour. The Control-attribute stop
/// in the decode loops keeps generation terminating correctly even when
/// this warning fires.
pub fn warn_on_template_vocab_mismatch(model: &LlamaModel, model_name: &str) {
    if !is_recognized_model_name(model_name) {
        warn!(
            model = model_name,
            "model name matches no known template family; defaulting to the ChatML template — \
             verify the model's chat format and extend the template dispatch if output quality \
             or turn termination looks wrong"
        );
    }
    // Validate the markers the prompt builders will ACTUALLY emit for this
    // model (via `turn_markers`), not the family's nominal spelling — so the
    // `gemma-4` line goes silent once its real `<|turn>`/`<turn|>` markers are
    // emitted, and any future mis-spelling still trips the wire.
    let TurnMarkers { open, close } = turn_markers(model_name);
    for marker in [open, close] {
        let tokens = model.str_to_token(marker, AddBos::Never).unwrap_or_default();
        let single_control = tokens.len() == 1 && is_control_token(model, tokens[0]);
        if !single_control {
            warn!(
                model = model_name,
                marker,
                token_count = tokens.len(),
                "chat-template marker does not tokenize to a single control token in this \
                 model's vocabulary; the template will prefill it as plain text and the \
                 model's real turn markers are spelled differently (gemma-4-e2b ships \
                 `<|turn>`/`<turn|>`). Generation still terminates via the control-token \
                 stop, but prompt fidelity may be degraded"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_stop_marker_finds_earliest() {
        assert_eq!(first_stop_marker("clean text"), None);
        let s = "Sentence.<start_of_turn>model";
        assert_eq!(first_stop_marker(s), Some(("Sentence.".len(), "<start_of_turn>")));
        let s2 = "a<|im_end|>b<end_of_turn>";
        assert_eq!(first_stop_marker(s2), Some((1, "<|im_end|>")));
    }

    #[test]
    fn safe_stream_end_holds_back_partial_markers() {
        // A complete word with no marker overlap streams fully.
        assert_eq!(safe_stream_end("hello"), "hello".len());
        // A trailing partial marker is held back.
        let s = "hello <end_of_tu";
        assert_eq!(safe_stream_end(s), "hello ".len());
        // `<` alone is a 1-byte prefix of several markers.
        assert_eq!(safe_stream_end("abc<"), 3);
    }

    #[test]
    fn safe_stream_end_respects_char_boundaries() {
        // Multibyte text with no marker overlap streams fully and never
        // panics on a non-boundary slice.
        let s = "ăîșț";
        assert_eq!(safe_stream_end(s), s.len());
    }

    #[test]
    fn template_family_dispatch() {
        assert_eq!(template_family("gemma-4-e2b"), TemplateFamily::Gemma);
        assert_eq!(template_family("GEMMA-X"), TemplateFamily::Gemma);
        assert_eq!(template_family("qwen3.5-0.8b"), TemplateFamily::ChatMl);
        assert_eq!(template_family("mystery-model"), TemplateFamily::ChatMl);
    }

    #[test]
    fn recognized_model_names() {
        assert!(is_recognized_model_name("gemma-4-e2b"));
        assert!(is_recognized_model_name("qwen3.5-0.8b"));
        assert!(!is_recognized_model_name("llama-3.1-8b"));
    }

    #[test]
    fn turn_markers_selects_per_family() {
        // The gemma-4 line ships non-standard control-token spellings.
        assert_eq!(turn_markers("gemma-4-e2b-it-Q4_K_M"), TurnMarkers::GEMMA_4);
        // Older Gemma builds keep the classic markers.
        assert_eq!(turn_markers("gemma-2-2b-it"), TurnMarkers::GEMMA);
        // Everything else is ChatML.
        assert_eq!(turn_markers("qwen3.5-0.8b"), TurnMarkers::CHATML);
        assert_eq!(turn_markers("mystery-model"), TurnMarkers::CHATML);
    }
}
