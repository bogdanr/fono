// SPDX-License-Identifier: GPL-3.0-only
//! Tool calling for the embedded llama.cpp backend.
//!
//! The cloud backends get tool calling for free: they send OpenAI-style
//! descriptors and the server hands back a parsed `tool_calls` array. The
//! embedded backend has neither. It hand-rolls the chat markers rather than
//! rendering the GGUF's own Jinja template — that is what makes the pinned
//! prompt-cache prefix possible — and a model's trained tool syntax lives
//! *in* that template. Bypassing the template loses tools with it.
//!
//! So we ask in the prompt and parse the answer out of the text. For Gemma
//! that is not a downgrade: Gemma has no tool tokens at all, and its own
//! template does exactly this. For families that do have tokens (Qwen's
//! `<tool_call>` tags, most notably) we ask for the syntax they were trained
//! on, which is why that is the shape we request.
//!
//! The parser is deliberately more tolerant than the instructions: models
//! wander into fenced code blocks or drop the wrapper and emit bare JSON, and
//! a reply that was *meant* to switch a light must not be read out loud as
//! prose because of a missing tag.

use serde_json::Value;

/// The wrapper we ask for, and the one Qwen/Hermes-family models already emit.
///
/// Public because a backend can write it *for* the model. On the one correction
/// a failed command is allowed, ending the prompt with this leaves the model
/// mid-command, so continuing the sentence means writing a command and there is
/// no prose branch to take instead. Asking in words did not work: the invitation
/// to correct itself was declined every time in favour of an apology.
pub const OPEN: &str = "<tool_call>";
const CLOSE: &str = "</tool_call>";

/// Openers a reply may legitimately begin with when it is a tool call.
/// Used to decide whether a partly-generated reply is still possibly a call.
///
/// `<` is deliberately broad. Models prefix calls with markers of their own —
/// `gemma-4-26b` emits a `<|channel>thought` preamble before a perfectly good
/// call — and the two mistakes are not equally cheap. Holding a reply that
/// turns out to be prose costs a moment of streaming; releasing one that turns
/// out to be a call means the light stays off and the machinery is read aloud.
/// A spoken reply never opens with an angle bracket, so nothing conversational
/// is delayed by this.
const OPENERS: [&str; 3] = ["<", "{", "```"];

/// The steady head of the system prompt: the caller's context, the tool block,
/// then how to behave.
///
/// The one place the tool block is rendered. The reply path and the cache
/// warm-up must produce the same bytes or the pinned checkpoint can never be
/// restored, and two renderings that had to agree by convention have drifted
/// twice before — each time costing a local model tens of seconds re-reading a
/// device list that had not changed. Ordering rationale lives on
/// [`crate::compose_head`].
#[must_use]
pub fn head_with_tools(
    context: &str,
    descriptors: &[Value],
    instructions_suffix: Option<&str>,
) -> String {
    let tools = (!descriptors.is_empty()).then(|| instructions(descriptors));
    crate::traits::compose_head(context, tools.as_deref(), instructions_suffix)
}

/// Renders the tool block appended to the system prompt.
///
/// Kept terse on purpose. Every line here is prefilled on the request path of
/// every turn, and on CPU prefill is the dominant cost — so the schema is
/// summarised to names and types rather than pasted as JSON.
#[must_use]
pub fn instructions(descriptors: &[Value]) -> String {
    let mut s = String::from(
        "You can operate the user's devices by calling a tool.\n\
         To call one, first say in one short sentence, in the user's own language, what you \
         are doing, then immediately write EXACTLY this:\n",
    );
    s.push_str(OPEN);
    s.push_str("{\"name\": \"ToolName\", \"arguments\": {\"key\": \"value\"}}");
    s.push_str(CLOSE);
    s.push_str(
        "\nOtherwise reply normally. Never write a tool call as prose, and never say you have \
         done something unless a tool result says so.\n\nTools:\n",
    );
    for d in descriptors {
        let f = d.get("function").unwrap_or(d);
        let Some(name) = f.get("name").and_then(Value::as_str) else { continue };
        s.push_str("- ");
        s.push_str(name);
        s.push('(');
        let props =
            f.get("parameters").and_then(|p| p.get("properties")).and_then(Value::as_object);
        if let Some(props) = props {
            let mut first = true;
            for (k, v) in props {
                if !first {
                    s.push_str(", ");
                }
                first = false;
                s.push_str(k);
                if v.get("type").and_then(Value::as_str) == Some("array") {
                    s.push_str("[]");
                }
            }
        }
        s.push(')');
        if let Some(desc) = f.get("description").and_then(Value::as_str) {
            let desc = desc.trim();
            if !desc.is_empty() {
                s.push_str(": ");
                s.push_str(desc.lines().next().unwrap_or(desc));
            }
        }
        s.push('\n');
    }
    s
}

/// Spells a tool call the way [`instructions`] asks for it, for replaying a
/// completed exchange back to the model on a later turn.
///
/// Rendering it here rather than at the call site keeps the asked-for shape and
/// the replayed shape in one file. A model that reads back a call in a syntax
/// it was never told to produce learns the wrong lesson about what a call looks
/// like, and the two spellings have no reason to agree unless they are written
/// next to each other.
#[must_use]
pub fn render_call(name: &str, arguments: &str) -> String {
    let args = arguments.trim();
    let args = if args.is_empty() { "{}" } else { args };
    format!("{OPEN}{{\"name\": \"{name}\", \"arguments\": {args}}}{CLOSE}")
}

/// Labels a tool's answer for the model.
///
/// The single definition used both to continue the current turn and to replay
/// the exchange on later turns. These were separate strings once; that is
/// precisely the kind of drift that silently costs a cache match, because the
/// prompt continued mid-turn must remain a prefix of the prompt built next
/// turn.
#[must_use]
pub fn render_result(summary: &str) -> String {
    format!("Tool result: {}", summary.trim())
}

/// Whether `text` — a reply generated so far — could still turn out to be a
/// tool call.
///
/// This is what lets the backend keep streaming. Tokens are held back only
/// while the answer is ambiguous; the moment the reply is plainly prose it is
/// released and streams as normal. Without it every turn would have to be
/// buffered whole, and a conversational answer would lose its head start
/// purely because the user happens to own a lamp.
#[must_use]
pub fn could_be_call(text: &str) -> bool {
    let t = text.trim_start();
    if t.is_empty() {
        return true;
    }
    OPENERS.iter().any(|o| t.starts_with(o) || o.starts_with(t))
}

/// Split a partly-generated reply into what is safe to speak now and what must
/// be held back because it may be the start of a tool call.
///
/// [`could_be_call`] only judges a reply *as a whole*, which is right when the
/// model does as it is told and emits a call and nothing else. It is wrong the
/// moment a call arrives *after* prose — and that is exactly what Fono now asks
/// for on a correction: say which devices did not respond, then try again. A
/// trace of that turn shows the consequence in full. The model said "I will try
/// turning on those specific lights now for you" and then emitted two perfectly
/// good calls; because the reply had already been released as prose, the calls
/// were streamed to the speaker instead of the house. The user heard raw JSON
/// read aloud, the lights stayed off, and — worse — the spoken text went into
/// the conversation as something the assistant had said, so the next turn
/// believed a command could be carried out by describing it.
///
/// The split is deliberately narrow: text is held only when its tail is the
/// opening tag, complete or half-arrived. Prose containing an angle bracket
/// ("5 < 3") is not a prefix of the tag and streams untouched, and a single
/// held `<` is released by the very next token if it turns out to be nothing.
#[must_use]
pub fn split_speakable(text: &str) -> (&str, &str) {
    if let Some(at) = text.find(OPEN) {
        return text.split_at(at);
    }
    // Longest tail that is still an unfinished opening tag. The tail may be
    // the whole of what has arrived: a model that writes the tag one token at
    // a time starts with a bare `<`, and releasing that releases the rest of
    // the tag behind it, one harmless-looking fragment after another, until a
    // whole command has been read aloud instead of run.
    let max = (OPEN.len() - 1).min(text.len());
    (1..=max)
        .rev()
        .find(|&i| text.is_char_boundary(text.len() - i) && text.ends_with(&OPEN[..i]))
        .map_or((text, ""), |i| text.split_at(text.len() - i))
}

/// Pulls the tool call out of a finished reply, as `(name, arguments_json)`.
///
/// `None` means the reply was prose after all — the caller must then say it,
/// not swallow it.
#[must_use]
pub fn parse_call(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    // Scan for the wrapper rather than requiring it first: models prefix calls
    // with channel or thinking markers, and a call announced late is still a
    // call. Anything before the opener is the model talking to itself and is
    // dropped, which also keeps those markers out of the user's ears.
    let mut body = t;
    if let Some(at) = t.find(OPEN) {
        let rest = &t[at + OPEN.len()..];
        body = rest.split(CLOSE).next().unwrap_or(rest);
    } else if let Some(rest) = t.strip_prefix("```") {
        // ```json\n{...}\n```
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        body = rest.split("```").next().unwrap_or(rest);
    }
    let v: Value = serde_json::from_str(body.trim()).ok()?;
    // Some models nest it under the wrapper name they were shown.
    let v = v.get("tool_call").unwrap_or(&v);
    let name = v.get("name").and_then(Value::as_str)?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let args = v.get("arguments").or_else(|| v.get("parameters"));
    let args = match args {
        // Some emit the arguments as a JSON *string* rather than an object.
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "{}".to_string(),
    };
    Some((name, args))
}

/// Is this the same request as the one that just failed?
///
/// A second attempt that writes the first attempt again, word for word, is not
/// an attempt. It is a second wait for the same refusal, and the user pays for
/// it in silence.
///
/// It is the common case rather than a corner: of thirteen commands that tried
/// twice in one benchmark run, six repeated themselves exactly, and none of the
/// thirteen was rescued by trying again. A model handed the same refusal it has
/// already read has nothing new to go on, so it writes the same thing.
///
/// Compared as JSON where both sides parse, so the same request spelled with
/// its fields in another order, or with different spacing, still counts as the
/// same request. Where either side is not JSON, the text is compared as it
/// stands — being wrong here may only cost one extra attempt, never a command.
///
/// Blank fields are ignored, and that is not tidiness. The question here is
/// whether the *request* is the same, not whether the typing is, and a field
/// holding nothing asks for nothing: it is dropped before the call leaves, so
/// two calls differing only there are one request. A real turn slipped through
/// this gap — the model wrote the refused call a second time with one empty
/// field added, the comparison called them different, and the user waited out
/// the identical refusal twice.
#[must_use]
pub fn same_request(a: &str, b: &str) -> bool {
    match (serde_json::from_str::<Value>(a), serde_json::from_str::<Value>(b)) {
        (Ok(x), Ok(y)) => without_blanks(x) == without_blanks(y),
        _ => a.trim() == b.trim(),
    }
}

/// The same request with everything that says nothing taken out.
fn without_blanks(v: Value) -> Value {
    fn says_nothing(v: &Value) -> bool {
        match v {
            Value::Null => true,
            Value::String(s) => s.trim().is_empty(),
            Value::Array(a) => a.is_empty(),
            Value::Object(o) => o.is_empty(),
            _ => false,
        }
    }
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, without_blanks(v)))
                .filter(|(_, v)| !says_nothing(v))
                .collect(),
        ),
        other => other,
    }
}

/// Drops a model's own channel or thinking header from the front of a reply.
///
/// `gemma-4-26b` opens even a one-line answer with `<|channel>thought
/// <channel|>`, and spoken aloud that is noise the user has to listen through.
/// Only a *closing* marker counts — `|>` or `</think>` — because those end a
/// header rather than starting one, so ordinary prose containing an angle
/// bracket is left alone.
#[must_use]
pub fn strip_preamble(text: &str) -> &str {
    let cut = ["</think>", "|>"].iter().filter_map(|m| text.rfind(m).map(|i| i + m.len())).max();
    cut.map_or(text, |i| text[i..].trim_start())
}

/// Whether the reply so far could still be an unfinished channel header.
///
/// Used while streaming, where the header arrives a token at a time. It only
/// ever holds text that opens with `<` and has not yet closed, and gives up
/// after a short run so a stray bracket cannot swallow a whole answer.
#[must_use]
pub fn maybe_preamble(sofar: &str) -> bool {
    let t = sofar.trim_start();
    t.starts_with('<') && t.len() < 64 && strip_preamble(t) == t
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tools() -> Vec<Value> {
        vec![json!({
            "type": "function",
            "function": {
                "name": "HassTurnOn",
                "description": "Turns on a device.\nSecond line ignored.",
                "parameters": {"type": "object", "properties": {
                    "area": {"type": "string"},
                    "domain": {"type": "array", "items": {"type": "string"}}
                }}
            }
        })]
    }

    /// Verbatim from `gemma-4-26b`: it opened even a one-line confirmation
    /// with its own channel header, which the user then heard read aloud.
    #[test]
    fn a_channel_header_is_never_spoken() {
        assert_eq!(strip_preamble("<|channel>thought\n<channel|>Lights are on."), "Lights are on.");
        assert_eq!(strip_preamble("<think>hm</think>\nDone."), "Done.");
        // Ordinary prose is left exactly as it is.
        assert_eq!(strip_preamble("The lights are on."), "The lights are on.");
        assert_eq!(strip_preamble("5 > 3 and 2 < 4."), "5 > 3 and 2 < 4.");
    }

    /// While streaming, the header arrives a token at a time; holding must
    /// stop the moment it is plainly prose, or the answer never starts.
    #[test]
    fn holding_stops_as_soon_as_it_is_prose() {
        assert!(maybe_preamble("<"));
        assert!(maybe_preamble("<|chan"));
        assert!(!maybe_preamble("<|channel>thought\n<channel|>"));
        assert!(!maybe_preamble("Lights"));
    }

    #[test]
    fn the_tool_block_names_each_tool_and_its_arguments() {
        let s = instructions(&tools());
        assert!(s.contains("HassTurnOn(area, domain[])"), "{s}");
        assert!(s.contains("Turns on a device."), "{s}");
        // Only the first line of a description is worth the prefill.
        assert!(!s.contains("Second line"), "{s}");
    }

    /// The shape we ask for.
    #[test]
    fn reads_the_wrapper_we_asked_for() {
        let (n, a) = parse_call("<tool_call>{\"name\": \"HassTurnOn\", \"arguments\": {\"area\": \"Kitchen\"}}</tool_call>").unwrap();
        assert_eq!(n, "HassTurnOn");
        assert_eq!(a, "{\"area\":\"Kitchen\"}");
    }

    /// The shapes models actually produce when they drift. Each of these was
    /// a light that would otherwise have been read out loud instead of switched.
    #[test]
    fn reads_the_shapes_models_drift_into() {
        for raw in [
            "```json\n{\"name\": \"HassTurnOn\", \"arguments\": {\"area\": \"Kitchen\"}}\n```",
            "{\"name\": \"HassTurnOn\", \"arguments\": {\"area\": \"Kitchen\"}}",
            "{\"tool_call\": {\"name\": \"HassTurnOn\", \"parameters\": {\"area\": \"Kitchen\"}}}",
            // Arguments as a string, not an object.
            "{\"name\": \"HassTurnOn\", \"arguments\": \"{\\\"area\\\": \\\"Kitchen\\\"}\"}",
            // Verbatim from gemma-4-26b: a thinking-channel preamble in front
            // of an otherwise perfect call. Requiring the wrapper first read
            // this as prose and left the light off.
            "<|channel>thought\n<channel|><tool_call>{\"name\": \"HassTurnOn\", \"arguments\": {\"area\": \"Kitchen\"}}</tool_call>",
        ] {
            let (n, a) = parse_call(raw).unwrap_or_else(|| panic!("did not parse: {raw}"));
            assert_eq!(n, "HassTurnOn", "{raw}");
            assert!(a.contains("Kitchen"), "{raw} -> {a}");
        }
    }

    #[test]
    fn prose_is_not_mistaken_for_a_call() {
        assert!(parse_call("I will turn on the light in the master bedroom.").is_none());
        assert!(parse_call("<tool_call>not json</tool_call>").is_none());
        assert!(parse_call("{\"arguments\": {}}").is_none());
    }

    /// A call that arrives *after* prose must still be run, not read out.
    ///
    /// This is the shape a correction takes — name what failed, then try again —
    /// and getting it wrong meant a user heard two well-formed calls recited as
    /// JSON while the lights stayed off.
    #[test]
    fn a_call_after_prose_is_held_back_and_the_prose_is_not() {
        let (speak, hold) = split_speakable("Trying again.\n\n<tool_call>{\"name\": \"X\"}");
        assert_eq!(speak, "Trying again.\n\n");
        assert_eq!(hold, "<tool_call>{\"name\": \"X\"}");
    }

    /// The tag arrives a token at a time, so a partial tail must be held too —
    /// otherwise the first fragment escapes and the rest is spoken after it.
    #[test]
    fn a_half_arrived_tag_is_held_until_it_is_settled() {
        for tail in ["<", "<to", "<tool_cal"] {
            let text = format!("Trying again. {tail}");
            let (speak, hold) = split_speakable(&text);
            assert_eq!(speak, "Trying again. ", "tail {tail:?}");
            assert_eq!(hold, tail, "tail {tail:?}");
        }
    }

    /// Prose that merely contains an angle bracket is not a call and must not
    /// be delayed — held text is released the moment it cannot be the tag.
    #[test]
    fn an_angle_bracket_in_prose_streams_untouched() {
        let (speak, hold) = split_speakable("Anything under 5 < 3 is wrong.");
        assert_eq!(speak, "Anything under 5 < 3 is wrong.");
        assert!(hold.is_empty());
    }

    /// Feeding the split the way the backend does — one fragment at a time,
    /// keeping only what it says to keep — must leave the whole command in
    /// hand and nothing of it spoken. The tag can start a fragment on its own,
    /// and a bare `<` used to be released for being all there was so far,
    /// which let the rest of the command follow it out one piece at a time and
    /// be read aloud as JSON while the house did nothing.
    #[test]
    fn a_command_written_one_character_at_a_time_is_never_spoken() {
        let reply = "Sting lumina.\n<tool_call>{\"name\": \"HassTurnOff\"}</tool_call>";
        let (mut buf, mut spoken) = (String::new(), String::new());
        for ch in reply.chars() {
            buf.push(ch);
            let (speak, hold) = split_speakable(&buf);
            spoken.push_str(speak);
            buf = hold.to_string();
        }
        assert_eq!(spoken, "Sting lumina.\n");
        assert_eq!(buf, "<tool_call>{\"name\": \"HassTurnOff\"}</tool_call>");
        assert!(parse_call(&buf).is_some(), "the held text is still a readable command");
    }

    /// Multi-byte text must never be split mid-character; Fono is spoken to in
    /// Romanian, and a panic here would take the whole turn down.
    #[test]
    fn splitting_never_lands_inside_a_character() {
        for text in ["Am aprins lumina în birou.", "Gata — încerc din nou. <", "«»<t"] {
            let (speak, hold) = split_speakable(text);
            assert_eq!(format!("{speak}{hold}"), text);
        }
    }

    /// Holding back only while ambiguous is what keeps ordinary conversation
    /// streaming for anyone who owns a lamp.
    #[test]
    fn prose_is_released_as_soon_as_it_is_recognisable() {
        assert!(could_be_call(""));
        assert!(could_be_call("<to"));
        assert!(could_be_call("<tool_call>{\"nam"));
        assert!(could_be_call("{\"na"));
        assert!(could_be_call("``"));
        // A model's own preamble must not look like prose.
        assert!(could_be_call("<|channel>thought"));
        assert!(!could_be_call("I"));
        assert!(!could_be_call("Sure,"));
        assert!(!could_be_call("The kitchen light is on."));
    }

    /// Verbatim from a benchmark run: the second attempt was the first attempt.
    /// The same request written another way is still the same request, and two
    /// genuinely different requests must still be tried.
    #[test]
    fn the_same_request_is_recognised_however_it_is_spelled() {
        let first = r##"{"area":"Living room","brightness":10,"color":"#4285F4"}"##;
        assert!(same_request(first, first));
        let reordered = r##"{ "color": "#4285F4", "brightness": 10, "area": "Living room" }"##;
        assert!(same_request(first, reordered));
        assert!(!same_request(first, r#"{"area":"Living room","brightness":30}"#));
        // Neither side is JSON: compared as it stands, whitespace aside.
        assert!(same_request("not json", " not json "));
        assert!(!same_request("not json", "something else"));
    }

    /// From a real turn: the model wrote the refused call again with one empty
    /// field added, and the comparison called that a fresh attempt. The user
    /// waited out the identical refusal twice. A field holding nothing asks for
    /// nothing, so it cannot be what makes two requests different.
    #[test]
    fn a_field_holding_nothing_does_not_make_a_new_request() {
        let asked = r#"{"area":"Office"}"#;
        assert!(same_request(asked, r#"{"area":"Office","floor":""}"#));
        assert!(same_request(asked, r#"{"area":"Office","name":null,"domain":[]}"#));
        // Emptiness cuts no further than that: a field that says something
        // still tells two requests apart.
        assert!(!same_request(asked, r#"{"area":"Office","domain":["climate"]}"#));
        assert!(!same_request(asked, r#"{"area":"Kitchen"}"#));
    }
}
