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
//! to the token cap. llama.cpp's penalty sampler only sees tokens the
//! sampler has *accepted*, and the backends decode prefill through
//! `ctx.decode`, so only generated tokens are ever accepted — the penalty
//! discourages the model from repeating ITS OWN output without penalising
//! faithful reuse of prompt content. A modest `repeat = 1.3` over the
//! recent window breaks the loop while staying deterministic: greedy still
//! picks the argmax of the penalised logits.
//!
//! Accepting is llama.cpp's job, not the caller's — see [`sample_next`],
//! which every decode loop must go through. Accepting a token that was
//! just sampled feeds it to the sampler twice, and that is not a rounding
//! error: it silently disarmed the tool-call rails for an entire
//! measurement (see [`generation_sampler_with_grammar`]).
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

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::token_type::LlamaTokenAttr;
use tracing::{debug, warn};

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

/// The same sampler with a grammar link in front of greedy, holding the model
/// to `grammar` from the moment any of `triggers` matches what it has written.
///
/// Before a trigger matches, nothing is constrained at all — ordinary talking
/// is untouched, which is the whole point of the lazy form. After one matches,
/// only text the grammar allows can be sampled.
///
/// The second half of the return says whether the grammar actually went in.
/// Callers are expected to report it rather than report what they asked for:
/// a rejected grammar samples exactly like the setting being off, and a trace
/// that says `on` either way hides the only failure this code has.
///
/// The reply is never at risk: a grammar llama.cpp will not take costs the
/// constraint and nothing else.
///
/// # Why this reaches past the safe wrapper
///
/// `llama_cpp_2::LlamaSampler::grammar_lazy_patterns` is gated behind that
/// crate's `common` feature, which links `libcommon.a` — around 14 MB into a
/// binary with a 25 MiB budget. The grammar implementation itself is in
/// `libllama`, which is always linked; only the safe wrapper is out of reach.
/// So the raw entry point is called directly and the result is wrapped, with
/// the layout asserts below turning any upstream change into a compile error.
#[must_use]
pub fn generation_sampler_with_grammar(
    model: &LlamaModel,
    grammar: &str,
    triggers: &[String],
) -> (LlamaSampler, bool) {
    let Some(g) = grammar_sampler(model, grammar, triggers) else {
        warn!(
            "the tool-call grammar was rejected by llama.cpp and is not being applied; \
             commands are being written unconstrained, exactly as with the setting off"
        );
        return (generation_sampler(), false);
    };
    let chain = LlamaSampler::chain_simple([
        LlamaSampler::penalties(PENALTY_LAST_N, PENALTY_REPEAT, 0.0, 0.0),
        g,
        LlamaSampler::greedy(),
    ]);
    (chain, true)
}

/// Sample the next token, leaving the sampler's state exactly as llama.cpp
/// wants it.
///
/// **Every decode loop must go through this.** `llama_sampler_sample` accepts
/// the token it returns before handing it back (`llama-sampler.cpp`, the
/// `llama_sampler_accept(smpl, token)` immediately before its `return`), so a
/// caller that also calls `accept()` feeds the sampler every token twice.
///
/// That looked harmless for a year, and it was not. What it cost:
///
/// * **The tool-call rails never engaged.** A lazy grammar decides when to
///   start constraining by matching a pattern against the text it has been
///   given. Fed twice, that text reads `<<tooltool__callcall>>{"{"`, which
///   contains no opener anything would recognise, so the grammar sat waiting
///   for a trigger that could not arrive. Two `bench-actions` runs came back
///   byte-for-byte identical with the rails switched on and off, and a house
///   full of traces recorded commands the grammar forbids — while the trace
///   said `on`, because arming a sampler and it having any effect are two
///   different facts.
/// * **The repetition penalty saw half the history it was configured for**,
///   each entry twice over.
///
/// A token that was sampled somewhere else — the prompt cache samples the
/// first one from the restored state — is the one case a caller *must* hand
/// over itself, with [`adopt_sampled_token`].
pub fn sample_next(
    sampler: &mut LlamaSampler,
    ctx: &llama_cpp_2::context::LlamaContext<'_>,
    idx: i32,
) -> LlamaToken {
    sampler.sample(ctx, idx)
}

/// Tell the sampler about a token it did not sample itself.
///
/// The counterpart to [`sample_next`], and the only correct use of `accept`
/// in a decode loop: the prompt-cache path samples the first token from the
/// restored state with its own sampler, and the generation sampler has to be
/// told, or its penalty window and its grammar both start a token behind.
pub fn adopt_sampled_token(sampler: &mut LlamaSampler, token: LlamaToken) {
    sampler.accept(token);
}

/// How many of this vocabulary's tokens `sampler` is currently ruling out.
///
/// This is the only way to see a grammar working from outside llama.cpp: a
/// rule being enforced shows up as `-inf` on everything it forbids. Zero means
/// the sampler is not holding the model to anything right now — which, for a
/// lazy grammar that has been fed a complete command, is a defect and not a
/// state.
///
/// Costs one pass over the vocabulary, so it is for tests and for the one
/// end-of-generation check that reports whether the rails ever bit.
#[must_use]
pub fn ruled_out(model: &LlamaModel, sampler: &LlamaSampler) -> usize {
    let mut all = llama_cpp_2::token::data_array::LlamaTokenDataArray::from_iter(
        (0..model.n_vocab())
            .map(|i| llama_cpp_2::token::data::LlamaTokenData::new(LlamaToken(i), 0.0, 0.0)),
        false,
    );
    sampler.apply(&mut all);
    all.data.iter().filter(|d| d.logit() == f32::NEG_INFINITY).count()
}

/// Compile-time proof that the two `llama-cpp-2` handle types really are bare
/// pointers, which is what makes the raw-pointer route in [`grammar_sampler`]
/// sound. Both hold their pointer in a private field with no accessor, so
/// reaching it means a layout-checked copy; if a future release adds a field,
/// these asserts fail the build instead of letting a crash ship.
const LAYOUT_MODEL: () = {
    assert!(
        std::mem::size_of::<LlamaModel>()
            == std::mem::size_of::<*const llama_cpp_sys_2::llama_model>()
    );
    assert!(
        std::mem::align_of::<LlamaModel>()
            == std::mem::align_of::<*const llama_cpp_sys_2::llama_model>()
    );
};
const LAYOUT_SAMPLER: () = {
    assert!(
        std::mem::size_of::<LlamaSampler>()
            == std::mem::size_of::<*mut llama_cpp_sys_2::llama_sampler>()
    );
    assert!(
        std::mem::align_of::<LlamaSampler>()
            == std::mem::align_of::<*mut llama_cpp_sys_2::llama_sampler>()
    );
};

/// Build the lazy grammar link, or `None` if llama.cpp will not take it.
fn grammar_sampler(model: &LlamaModel, grammar: &str, triggers: &[String]) -> Option<LlamaSampler> {
    use std::ffi::CString;

    // Force the layout proofs above to be evaluated for this code path.
    let () = LAYOUT_MODEL;
    let () = LAYOUT_SAMPLER;

    if triggers.is_empty() {
        return None;
    }
    // Interior NULs cannot cross into C. Neither can occur in a grammar built
    // from a tool catalogue, but the check is free and the alternative is
    // undefined behaviour.
    let grammar_c = CString::new(grammar).ok()?;
    let root_c = CString::new("root").ok()?;
    let triggers_c: Vec<CString> =
        triggers.iter().map(|t| CString::new(t.as_str())).collect::<Result<_, _>>().ok()?;
    let mut patterns: Vec<*const std::ffi::c_char> =
        triggers_c.iter().map(|t| t.as_ptr()).collect();

    // SAFETY: `LlamaModel` is a single-field newtype over `NonNull<llama_model>`
    // with no public accessor, so the pointer is read through a copy whose
    // layout equality is asserted at compile time (see `LAYOUT_MODEL` above).
    // The read borrows `model`, so the pointer is valid for this call. Same
    // pattern, and same reasoning, as `BrainTap::install`.
    let model_ptr: *const llama_cpp_sys_2::llama_model = unsafe { std::mem::transmute_copy(model) };

    // SAFETY: `model_ptr` came from a live `&LlamaModel`. The grammar, root and
    // pattern pointers all outlive the call, which copies what it needs.
    let raw = unsafe {
        let vocab = llama_cpp_sys_2::llama_model_get_vocab(model_ptr);
        llama_cpp_sys_2::llama_sampler_init_grammar_lazy_patterns(
            vocab,
            grammar_c.as_ptr(),
            root_c.as_ptr(),
            patterns.as_mut_ptr(),
            patterns.len(),
            std::ptr::null(),
            0,
        )
    };
    if raw.is_null() {
        return None;
    }

    // SAFETY: `LlamaSampler` is likewise a single-field newtype over the raw
    // pointer and cannot be constructed from outside its crate; its layout
    // equality is asserted at compile time (see `LAYOUT_SAMPLER` above), so an
    // upstream change is a build failure rather than a crash. `raw` is a live
    // sampler this call now owns — the wrapper frees it, or the chain it joins.
    Some(unsafe { std::mem::transmute::<*mut llama_cpp_sys_2::llama_sampler, LlamaSampler>(raw) })
}

/// Model-agnostic stop predicate: `true` for any token the vocabulary
/// tags as a control token (turn markers, BOS/EOS, end-of-generation),
/// however it is spelled. See the module docs for the `gemma-4-e2b`
/// evidence behind attribute matching over literal-string matching.
#[must_use]
pub fn is_control_token(model: &LlamaModel, token: LlamaToken) -> bool {
    model.token_attr(token).contains(LlamaTokenAttr::Control)
}

/// Name, at debug level, the control token that just ended a turn.
///
/// "Stopped on a control token" is not enough to tell a real end of turn
/// from a structural marker the model emitted mid-answer, because a
/// vocabulary can tag a great many tokens `Control`: gemma-4-26B tags 16,
/// DeepSeek-V4-Flash tags 1,277, of which exactly two are begin/end of
/// sentence and the rest are padding, tool-call framing, and table and
/// bounding-box markup. Sampling any one of those cuts the reply short, and
/// without the spelling there is nothing in the record to say which fired.
///
/// Rendering asks for `special = true` on purpose: the reply path renders
/// control tokens as empty text so a marker can never reach the user, which
/// also makes them invisible in a log.
pub fn log_stop_token(model: &LlamaModel, token: LlamaToken, generated: u32) {
    let bytes = model.token_to_piece_bytes(token, 32, true, None).unwrap_or_default();
    debug!(
        token = token.0,
        spelling = %String::from_utf8_lossy(&bytes),
        eog = model.is_eog_token(token),
        generated,
        "generation stopped on a control token"
    );
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

/// Hand-rolled chat-template family a backend renders a turn with. The
/// fully general alternative — rendering the GGUF's embedded
/// `tokenizer.chat_template` through a Jinja engine — stays out: it needs
/// a template engine the binary does not carry, and most of the 54
/// families llama.cpp knows are not an open/close marker pair these two
/// renderers can express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateFamily {
    Gemma,
    ChatMl,
}

/// Template family to render `model_name` with, taken from whichever
/// markers [`turn_markers`] resolved — so a model that named its own
/// markers picks the renderer to match, and only an unresolved model
/// falls back to the family its file name suggests.
#[must_use]
pub fn template_family(model_name: &str) -> TemplateFamily {
    turn_markers(model_name).family()
}

/// Family a model's file name suggests, used only when the model itself
/// named no markers we can emit.
fn family_from_name(model_name: &str) -> TemplateFamily {
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

    /// Every spelling the hand-rolled renderer can emit, most specific
    /// first. `GEMMA_4` precedes `GEMMA` for readability only — the two
    /// spellings share no substring, so the order cannot change a match.
    const ALL: [Self; 3] = [Self::GEMMA_4, Self::GEMMA, Self::CHATML];

    /// Which renderer frames a turn with these markers.
    #[must_use]
    pub fn family(self) -> TemplateFamily {
        if self == Self::CHATML {
            TemplateFamily::ChatMl
        } else {
            TemplateFamily::Gemma
        }
    }
}

/// The turn markers `model`'s own embedded chat template names, or `None`
/// when that template names none the hand-rolled renderer can emit.
///
/// A candidate is accepted when the template mentions both of its markers
/// *and* the vocabulary registers each as a single control token. Both
/// halves are needed: the template alone can name a marker the vocabulary
/// spells differently, and the vocabulary alone cannot say which of
/// several registered markers frames a turn.
///
/// `None` for a model that embeds no template, and for the many families
/// the hand-rolled renderer cannot express — a DeepSeek template frames
/// roles as `<｜User｜>`, while Llama-3, Mistral and Command-R are not an
/// open/close pair at all.
///
/// This is the model's own answer, and [`resolve_turn_markers`] records it
/// so every later [`turn_markers`] call renders with it.
#[must_use]
pub fn turn_markers_from_template(model: &LlamaModel) -> Option<TurnMarkers> {
    let text = model.chat_template(None).ok()?;
    let text = text.to_str().ok()?;
    TurnMarkers::ALL.into_iter().find(|candidate| {
        [candidate.open, candidate.close]
            .into_iter()
            .all(|marker| text.contains(marker) && is_single_control_token(model, marker))
    })
}

/// Whether `marker` tokenizes to exactly one control token in `model`'s
/// vocabulary, i.e. whether emitting it prefills as the marker the model
/// was trained on rather than as prose.
#[must_use]
pub fn is_single_control_token(model: &LlamaModel, marker: &str) -> bool {
    let tokens = model.str_to_token(marker, AddBos::Never).unwrap_or_default();
    tokens.len() == 1 && is_control_token(model, tokens[0])
}

/// Markers recorded by [`resolve_turn_markers`], keyed by model file stem.
///
/// A file stem is what the prompt builders have to work with: they are pure
/// functions of the rendered text, called far below the loaded model, and
/// several run in tests with no model at all. Recording the answer once at
/// load keeps rendering a pure function of the name while still following
/// the model. Stems collide only if one process serves two different models
/// whose files are named the same, which the shared-weights cache already
/// treats as the same model.
fn recorded_markers() -> &'static Mutex<HashMap<String, TurnMarkers>> {
    static RECORDED: OnceLock<Mutex<HashMap<String, TurnMarkers>>> = OnceLock::new();
    RECORDED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Ask `model` how it frames a turn and record the answer for `model_name`,
/// so every later [`turn_markers`] call renders the model's own markers
/// rather than the ones its file name suggests. Returns what will be
/// rendered. Call once per load, before any prompt is built.
///
/// A model that names no pair the hand-rolled renderers can emit (most
/// families are not an open/close pair) records nothing and keeps the name
/// guess, which [`warn_on_template_vocab_mismatch`] reports.
pub fn resolve_turn_markers(model: &LlamaModel, model_name: &str) -> TurnMarkers {
    let Some(markers) = turn_markers_from_template(model) else {
        return turn_markers(model_name);
    };
    if let Ok(mut recorded) = recorded_markers().lock() {
        recorded.insert(model_name.to_string(), markers);
    }
    markers
}

/// How a turn is framed for `model_name`: what the model itself named at
/// load if [`resolve_turn_markers`] got an answer, otherwise the spelling
/// its file name suggests — Gemma 4 for a `gemma-4` name, classic Gemma for
/// any other Gemma, ChatML for everything else.
#[must_use]
pub fn turn_markers(model_name: &str) -> TurnMarkers {
    if let Ok(recorded) = recorded_markers().lock() {
        if let Some(markers) = recorded.get(model_name) {
            return *markers;
        }
    }
    let name = model_name.to_ascii_lowercase();
    match family_from_name(model_name) {
        TemplateFamily::Gemma if name.contains("gemma-4") => TurnMarkers::GEMMA_4,
        TemplateFamily::Gemma => TurnMarkers::GEMMA,
        TemplateFamily::ChatMl => TurnMarkers::CHATML,
    }
}

/// Load-time template/vocab tripwire. Call once per model load, after
/// [`resolve_turn_markers`], with the loaded model and its file stem.
///
/// Warns when:
/// - the model named no markers we can emit *and* its file name matches no
///   family either, so the ChatML renderer is a pure guess, or
/// - the markers that will actually be rendered do not tokenize to a single
///   control token in this vocabulary, so they prefill as prose.
///
/// Diagnostic only: never changes behaviour. The Control-attribute stop
/// in the decode loops keeps generation terminating correctly even when
/// this warning fires.
pub fn warn_on_template_vocab_mismatch(model: &LlamaModel, model_name: &str) {
    if turn_markers_from_template(model).is_none() && !is_recognized_model_name(model_name) {
        warn!(
            model = model_name,
            "the model's chat template names no turn markers Fono can emit and its name matches \
             no known family, so the ChatML template is a guess — verify the model's chat format \
             if output quality or turn termination looks wrong"
        );
    }
    // Validate the markers the prompt builders will ACTUALLY emit for this
    // model, which is whatever `resolve_turn_markers` settled on — so a model
    // that named its own markers goes silent here, and a guess that this
    // vocabulary spells differently still trips the wire.
    let rendered = turn_markers(model_name);
    for marker in [rendered.open, rendered.close] {
        if !is_single_control_token(model, marker) {
            let token_count = model.str_to_token(marker, AddBos::Never).unwrap_or_default().len();
            warn!(
                model = model_name,
                marker,
                token_count,
                "chat-template marker does not tokenize to a single control token in this \
                 model's vocabulary; the template will prefill it as plain text and the \
                 model's real turn markers are spelled differently. Generation still \
                 terminates via the control-token stop, but prompt fidelity is degraded"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real vocabulary, loaded weights-free, for the tests that have to cross
    /// into llama.cpp.
    ///
    /// `vocab_only` is what makes this affordable: compiling a grammar needs the
    /// token table and nothing else, so there is no tensor data to read and no
    /// gigabyte of resident weights. Any GGUF will do, including the tiny
    /// vocab-only files llama.cpp keeps for its own tests.
    fn vocab_model() -> LlamaModel {
        let path = std::env::var("FONO_TEST_VOCAB_GGUF").expect("FONO_TEST_VOCAB_GGUF");
        let params = llama_cpp_2::model::params::LlamaModelParams::default().with_vocab_only(true);
        LlamaModel::load_from_file(crate::llama_backend::backend(), &path, &params)
            .expect("loading the vocabulary")
    }

    /// The candidate list is searched in order, so a marker that is a
    /// substring of another candidate's would make the order decide the
    /// answer. Nothing in the code enforces that, so assert it here: adding
    /// a family whose markers nest inside an earlier one has to fail loudly
    /// rather than silently mis-frame every turn.
    #[test]
    fn no_candidate_marker_hides_inside_another() {
        for (i, a) in TurnMarkers::ALL.iter().enumerate() {
            for (j, b) in TurnMarkers::ALL.iter().enumerate() {
                if i == j {
                    continue;
                }
                for mine in [a.open, a.close] {
                    for theirs in [b.open, b.close] {
                        assert!(
                            !theirs.contains(mine),
                            "{mine:?} occurs inside {theirs:?}, so which family a template \
                             matches would depend on the search order"
                        );
                    }
                }
            }
        }
    }

    /// A model's embedded template is the only authority on how it frames a
    /// turn, and the whole point of reading it is that the answer does not
    /// come from the file name. Prove the round trip against a real
    /// vocabulary: whatever comes back must be a pair this vocabulary
    /// registers as control tokens.
    ///
    /// `None` is a legitimate answer — most families are not an open/close
    /// pair the hand-rolled renderer can emit — so this asserts the property
    /// only when a pair is found.
    ///
    /// ```text
    /// FONO_TEST_VOCAB_GGUF=/path/to/any.gguf \
    ///   nice -n 10 cargo test -p fono-core --features llama-local \
    ///   --lib llama_gen -- --ignored
    /// ```
    #[test]
    #[ignore = "needs a vocabulary via FONO_TEST_VOCAB_GGUF"]
    fn markers_read_from_a_template_are_registered_by_the_vocabulary() {
        let model = vocab_model();
        let Some(markers) = turn_markers_from_template(&model) else { return };
        for marker in [markers.open, markers.close] {
            assert!(
                is_single_control_token(&model, marker),
                "{marker:?} came back from the template but is not a single control token"
            );
        }
    }

    /// The one thing no other test can prove: llama.cpp accepts a grammar Fono
    /// generated, through a raw entry point reached past a feature gate.
    ///
    /// Every step of that route is invisible to the type system. The symbol
    /// might not be linked, the grammar text might be rejected, and the
    /// returned pointer might be wrapped wrongly — which would not show up
    /// until it is freed. This walks all three, ending with the drop.
    ///
    /// Needs a vocabulary, so it is skipped unless one is named:
    ///
    /// ```text
    /// FONO_TEST_VOCAB_GGUF=/path/to/any.gguf \
    ///   nice -n 10 cargo test -p fono-core --features llama-local \
    ///   --lib llama_gen -- --ignored
    /// ```
    #[test]
    #[ignore = "needs a vocabulary via FONO_TEST_VOCAB_GGUF"]
    fn llama_cpp_accepts_a_grammar_we_generated() {
        let model = vocab_model();
        let row = |name: &str, schema: serde_json::Value| crate::tool_catalog::ToolRow {
            source: "ha".into(),
            name: name.into(),
            description: String::new(),
            schema,
            schema_hash: String::new(),
            capability: crate::tool_catalog::Capability::Safe,
            verify_class: crate::tool_catalog::VerifyClass::None,
            readback_tool: None,
            available: true,
            enabled: true,
            user_touched: false,
            runs: 0,
            last_run: None,
        };
        let tools = [
            row(
                "HassTurnOn",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "area": {"type": "string"},
                        "name": {"type": "string"},
                        "domain": {"type": "array", "items": {"type": "string"}},
                    },
                }),
            ),
            // Two field names that cannot be used verbatim in a GBNF rule name.
            // The values a slot holds now live in a *named* rule, so the name
            // has to be one llama.cpp will take — and this is the only place
            // that genuinely knows what it will take.
            //
            // `device_class` earns its place here by having been the one that
            // got through: it holds nothing but letters and an underscore, so
            // it looks harmless, and llama.cpp takes no underscore in a rule
            // name. A stock Home Assistant publishes it, one rejected rule name
            // discards the whole grammar, and every command in a real house was
            // written unconstrained as a result.
            row(
                "odd_server_tool",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "who's there?": {"type": "string"},
                        "device_class": {"type": "string"},
                    },
                }),
            ),
        ];
        let mut slots = crate::tool_grammar::SlotValues::new();
        slots.set("ha", "area", vec!["Kitchen".into(), "Master bedroom".into()]);
        slots.set("ha", "name", vec![r#"Nick's "big" lamp"#.into()]);
        slots.set("ha", "domain", vec!["light".into(), crate::tool_grammar::ANY_KIND.into()]);
        slots.set("ha", "who's there?", vec!["Kitchen".into()]);
        slots.set("ha", "device_class", vec!["door".into(), "window".into()]);
        slots.require("ha", "domain");
        let grammar = crate::tool_grammar::build(&tools, &slots).expect("a grammar");

        let sampler = grammar_sampler(&model, &grammar, &crate::tool_grammar::trigger_patterns());
        assert!(
            sampler.is_some(),
            "llama.cpp rejected a grammar built from a real catalogue:\n{grammar}"
        );
        // Freeing it is where a wrongly-wrapped pointer would abort.
        drop(sampler);
    }

    /// Text llama.cpp cannot parse has to come back as `None`, not as a null
    /// pointer dressed up as a sampler. That is the difference between losing
    /// the constraint and losing the process.
    #[test]
    #[ignore = "needs a vocabulary via FONO_TEST_VOCAB_GGUF"]
    fn a_grammar_llama_cpp_cannot_parse_is_refused_not_wrapped() {
        let model = vocab_model();
        assert!(grammar_sampler(&model, "root ::= (((", &["x".to_string()]).is_none());
    }

    /// A rejected grammar has to leave sampling exactly as it is with the
    /// setting off, so a mistake in the constraint can never cost the user
    /// their reply — and it has to SAY it was rejected, because a trace that
    /// cannot tell `on` from `nothing happened` is how the rails managed to
    /// look enabled for a whole evening while a house filled up with invented
    /// area names.
    #[test]
    #[ignore = "needs a vocabulary via FONO_TEST_VOCAB_GGUF"]
    fn a_rejected_grammar_still_returns_a_working_sampler() {
        let model = vocab_model();
        let (sampler, armed) =
            generation_sampler_with_grammar(&model, "root ::= (((", &["x".to_string()]);
        assert!(!armed, "a grammar llama.cpp refused must not be reported as applied");
        // Building it and freeing it is the whole test: a wrongly-built chain
        // aborts on drop rather than returning.
        drop(sampler);
    }

    /// The test every other grammar test here was standing in for: that the
    /// rails, once armed, actually **stop** the area this house does not have.
    ///
    /// Everything before this proved construction — the symbol links, the text
    /// parses, the pointer frees cleanly, every opener is accepted. None of it
    /// proved a single token was ever masked, and a house full of traces then
    /// recorded commands that the grammar forbids being written anyway, with
    /// the trace saying `on`. Construction is not enforcement, and only this
    /// shape of test can tell them apart.
    ///
    /// Two halves, and both matter. Before an opener, nothing at all is ruled
    /// out — that is the lazy form keeping ordinary talking free. After one,
    /// the vocabulary is cut down, and an area the caller never supplied cannot
    /// be spelled while the one that was supplied can.
    #[test]
    #[ignore = "needs a vocabulary via FONO_TEST_VOCAB_GGUF"]
    fn the_rails_refuse_an_area_this_house_does_not_have() {
        let model = vocab_model();
        let tools = [crate::tool_catalog::ToolRow {
            source: "ha".into(),
            name: "HassTurnOn".into(),
            description: String::new(),
            schema: serde_json::json!({
                "type": "object",
                "properties": { "area": {"type": "string"}, "name": {"type": "string"} },
            }),
            schema_hash: String::new(),
            capability: crate::tool_catalog::Capability::Safe,
            verify_class: crate::tool_catalog::VerifyClass::None,
            readback_tool: None,
            available: true,
            enabled: true,
            user_touched: false,
            runs: 0,
            last_run: None,
        }];
        let mut slots = crate::tool_grammar::SlotValues::new();
        slots.set("ha", "area", vec!["Kitchen".into()]);
        let grammar = crate::tool_grammar::build(&tools, &slots).expect("a grammar");
        let mut sampler =
            grammar_sampler(&model, &grammar, &crate::tool_grammar::trigger_patterns())
                .expect("llama.cpp took the grammar");

        assert_eq!(
            ruled_out(&model, &sampler),
            0,
            "before any opener a lazy grammar must leave every token available"
        );

        let opener = r#"<tool_call>{"name": "HassTurnOn", "arguments": {"area": ""#;
        let written = model.str_to_token(opener, AddBos::Never).expect("tokenize the opener");
        for token in &written {
            adopt_sampled_token(&mut sampler, *token);
        }

        let after = ruled_out(&model, &sampler);
        assert!(
            after > 0,
            "the rails were armed and then held the model to nothing: not one of \
             {} tokens was ruled out after `{opener}`",
            model.n_vocab()
        );
        // And the one area that was supplied is still reachable, so the model
        // is being narrowed rather than cornered.
        assert!(
            after < model.n_vocab() as usize,
            "every token was ruled out — the model has nothing left to write"
        );

        // The second half is the bug itself, pinned. Handing the same tokens
        // over twice — which is what a decode loop does if it accepts a token
        // `sample()` already accepted — leaves the opener unreadable and the
        // rails waiting for a trigger that can never arrive. This is not a
        // hypothetical: it is what shipped, and what made an entire A/B
        // measurement come back byte-for-byte identical in both arms.
        let mut doubled =
            grammar_sampler(&model, &grammar, &crate::tool_grammar::trigger_patterns())
                .expect("llama.cpp took the grammar");
        for token in &written {
            adopt_sampled_token(&mut doubled, *token);
            adopt_sampled_token(&mut doubled, *token);
        }
        assert_eq!(
            ruled_out(&model, &doubled),
            0,
            "a token handed over twice must be understood as the disarming mistake it is"
        );
    }

    /// Every opener the reply parser will honour has to arm the rails.
    ///
    /// This is the test the original grammar work was missing, and the gap it
    /// left was total: the rails covered the tagged form only, so a model that
    /// answered with a fenced block or a bare object wrote its command in a
    /// place nothing was watching — while the trace still said `on`.
    #[test]
    #[ignore = "needs a vocabulary via FONO_TEST_VOCAB_GGUF"]
    fn every_accepted_opener_arms_the_rails() {
        let model = vocab_model();
        let patterns = crate::tool_grammar::trigger_patterns();
        assert!(
            grammar_sampler(&model, "root ::= \"{\" \"}\"", &patterns).is_some(),
            "llama.cpp must accept every trigger pattern: {patterns:?}"
        );
    }

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

    /// What the model itself named at load beats what its file name
    /// suggests, and it picks the renderer too — a Gemma model saved under
    /// a name carrying no `gemma` used to be framed as ChatML, which a
    /// Gemma vocabulary accepts as ordinary words, so nothing complained
    /// while every prompt was framed off-distribution.
    #[test]
    fn a_recorded_answer_beats_the_file_name() {
        let name = "renamed-by-the-user-Q4_K_M";
        assert_eq!(turn_markers(name), TurnMarkers::CHATML);
        assert_eq!(template_family(name), TemplateFamily::ChatMl);

        recorded_markers().lock().unwrap().insert(name.to_string(), TurnMarkers::GEMMA_4);

        assert_eq!(turn_markers(name), TurnMarkers::GEMMA_4);
        assert_eq!(template_family(name), TemplateFamily::Gemma);
    }
}
