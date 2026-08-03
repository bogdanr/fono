// SPDX-License-Identifier: GPL-3.0-only
//! Per-vendor knowledge of what a tool server's own answers mean.
//!
//! MCP says how to *call* a tool and whether the server raised an error. It
//! says nothing about what a tool was trying to do, so there is no
//! protocol-level way to tell "the light is on now" from "nothing happened".
//! Proving an action landed therefore needs a little knowledge of each
//! server's payloads, and this is the only place it is allowed to live.
//!
//! Two measurements shaped the interface.
//!
//! First, a server can answer cheerfully having done nothing at all: Home
//! Assistant returns an error-free result for a command naming an area it does
//! not have. So [`Vendor::admission`] exists, and "no error" is never
//! treated as proof. It also has to tell *nothing worked* from *some of it
//! worked*: an area-wide switch-on that started the air conditioning and left
//! the one lamp that was wanted untouched is neither a success nor a total
//! failure, and calling it either one misinforms the reply.
//!
//! Second, the obvious generic check — read the world before and after and see
//! whether anything changed — is unsound. Two readings of one real house three
//! seconds apart, with nothing happening, already differed: a soil-temperature
//! probe had drifted two tenths of a degree. A change detector would have
//! called that a successful light switch.
//!
//! [`Vendor::confirms`] avoids that trap by asking a different question. Not
//! *did anything change* but *is the world now as the user asked*, looking only
//! at the things the server itself claimed to have touched. Sensors drifting
//! elsewhere are irrelevant, only one extra read is needed rather than two, and
//! a command that asked for something already true is correctly confirmed
//! instead of being reported as having done nothing.
//!
//! Adding a vendor means writing one implementation here and one line in
//! [`for_server`]. Nothing outside this module knows any vendor's name.

use fono_assistant::ToolCall;

/// What a server's own error-free result admits about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Every target the command named was acted on.
    Worked,
    /// Nothing was touched — typically a name that matched no device.
    NothingWorked,
    /// Some targets were acted on and others were not, named here so the reply
    /// can say which. An area-wide command routinely lands here.
    PartlyWorked { failed: Vec<String> },
}

/// What a post-condition check concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The world is as the user asked. This is the only rung that proves it.
    Confirmed,
    /// The server accepted the command and the world disagrees.
    Contradicted,
}

/// What one family of tool servers' answers mean.
///
/// Every method may decline to answer. Declining is safe: the weaker rungs of
/// the ladder still apply, and Fono's wording drops to what they support.
pub trait Vendor: Send + Sync {
    /// Short identifier, for logs and traces.
    fn id(&self) -> &'static str;

    /// Is this result one of ours?
    ///
    /// Must be specific enough that another vendor's answer never matches, and
    /// is allowed to be wrong only in the direction of saying no.
    fn recognises(&self, result: &serde_json::Value) -> bool;

    /// What does an error-free result admit about what actually happened?
    ///
    /// `None` means this vendor cannot tell from the payload, which must not
    /// be read as success.
    fn admission(&self, _result: &str) -> Option<Admission> {
        None
    }

    /// Is running this tool a second time the same request as running it once?
    ///
    /// This is what decides whether a command that did not land may be handed
    /// back to the model for one more go. "Be on" and "be off" name a state the
    /// world should end in, so asking twice changes nothing; "two degrees
    /// warmer" names a change, and asking twice is four degrees.
    ///
    /// Defaults to false, which costs only the retry: a vendor that cannot tell
    /// gets the honest failure sentence instead of a second attempt, and that
    /// is the safe direction to be wrong in.
    fn repeatable(&self, _tool: &str) -> bool {
        false
    }

    /// Given a fresh reading of the world, is it as the user asked?
    ///
    /// `None` when this vendor cannot tell, which is not a failure.
    fn confirms(&self, _call: &ToolCall, _result: &str, _readback: &str) -> Option<Verdict> {
        None
    }

    /// Which individual things in the home this call actually reached, and
    /// whether each one landed.
    ///
    /// This is what lets Fono say "the office lamp has worked eleven times and
    /// the bedroom blind has never once" — a per-device history rather than a
    /// per-tool one. It has to be vendor knowledge and cannot be read off the
    /// arguments: one command naming an area reaches six devices the arguments
    /// never mention, and the reply is the only place their names appear.
    ///
    /// Empty by default, and empty is not "nothing worked" — it is "this server
    /// does not say", which is the truth for every server Fono has no specific
    /// knowledge of. Nothing is recorded in that case, rather than a row of
    /// zeroes that would read as failure.
    fn targets(&self, _result: &str) -> Vec<Target> {
        Vec::new()
    }

    /// What a fresh reading of the house says about the named devices.
    ///
    /// Weaker than [`Self::confirms`] on purpose, and the difference is the
    /// point. `confirms` needs to know what state was *asked for*, which Fono
    /// only knows for a tool that names an end state — two tool names, on this
    /// server. This one needs no such knowledge: it reports what the devices
    /// read, and lets the model be the one to notice that "off" was asked for
    /// and `on` came back.
    ///
    /// It deliberately does not judge. A reading cannot be turned into a verdict
    /// here, because this server's readback carries a device's `state` and none
    /// of its attributes: a lamp dimmed from full to a tenth reads `on` before
    /// and `on` after, so "nothing changed" and "changed exactly as asked" are
    /// the same two words. Reporting the second as the first would call a
    /// working command broken, which is the wrong direction to be wrong in.
    ///
    /// Empty by default, and empty means "this server does not say" — the same
    /// meaning it has for [`Self::targets`].
    fn readings(&self, _readback: &str, _names: &[String]) -> Vec<(String, String)> {
        Vec::new()
    }

    /// One sentence a model can act on, out of a refusal this server wrote for
    /// itself.
    ///
    /// A server that objects in its own debugging vocabulary is worse than
    /// useless to a small model. A real house refused a temperature set with
    /// 1400 characters of Python `repr`, of which the two facts that mattered —
    /// that the area held two devices of that kind, and what they were called —
    /// sat in the middle, exactly where shortening a long answer throws text
    /// away. The model read the wreckage and told the user the opposite of the
    /// truth: that the home had no such device at all.
    ///
    /// Everything in the sentence has to come out of the refusal itself.
    /// Nothing is inferred about the home, so this cannot invent a device.
    ///
    /// `None` means "not one of mine, or nothing better to say than the server
    /// already said", and the original text is used unchanged.
    fn refusal(&self, _text: &str) -> Option<String> {
        None
    }

    /// The tools that switch a device without setting a value on it.
    ///
    /// Needed for one correction and no other purpose. A tool whose only job is
    /// to set a value, called with no value, is not a request this server can
    /// carry out — and where the user asked for something to be switched on or
    /// off, these are the tools that do it. Naming them costs a clause and
    /// saves a turn.
    ///
    /// Empty by default: a server that does not say keeps the general wording,
    /// which tells the model what kind of tool to reach for without claiming to
    /// know its name.
    fn switches(&self) -> &'static [&'static str] {
        &[]
    }

    /// The kind of device a tool is about, when the tool's own name says.
    ///
    /// A temperature tool is about the heating whatever words the request used,
    /// so a request that reached for one is a request about a kind of device —
    /// which is exactly the fact missing when the same request is retried with a
    /// tool that switches anything at all.
    ///
    /// `None` for every tool whose name does not state it, which is most of
    /// them: guessing here would put a kind in a call the user never limited.
    fn kind_of(&self, _tool: &str) -> Option<&'static str> {
        None
    }

    /// Which argument of a tool holds an area, which holds a device, and which
    /// holds a kind of device.
    ///
    /// This is the only vendor knowledge the rails need, and it is deliberately
    /// three *field names* rather than a list of tools. A table of tool names
    /// would have to be corrected every time the server gained or renamed one —
    /// maintenance nobody signed up for — while a field name is part of the
    /// server's published interface and cannot move without breaking every
    /// other client too. A tool that has none of these fields is unaffected,
    /// which is why an unfamiliar server keeps exactly today's freedom.
    ///
    /// The default is empty, so a vendor that says nothing gets constraints
    /// derived from published schemas alone.
    fn slot_fields(&self) -> SlotFields {
        SlotFields::default()
    }

    /// Is this catalogue of tool names one of ours?
    ///
    /// Needed because the rails are built before any tool has run, so there is
    /// no result payload to recognise. Same one-sided rule as
    /// [`Self::recognises`]: it may be wrong only by saying no.
    fn recognises_catalogue(&self, _tools: &[&str]) -> bool {
        false
    }
}

/// One thing in the home a call reached, and whether it landed.
///
/// The name is whatever the server called it, passed through untouched. Fono
/// matches it against the device list it already learned from the same server,
/// so a name that does not match is dropped rather than invented as a new
/// device — a reply is evidence about the home, not a source of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    /// The server put this one under `success` rather than `failed`.
    pub landed: bool,
}

/// The argument names a server uses for the three things a house is made of.
///
/// `None` means "this server has no such field", and nothing is constrained
/// for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SlotFields {
    /// Holds an area name.
    pub place: Option<&'static str>,
    /// Holds something an area is itself inside — a storey, a wing. Same
    /// standing as [`Self::place`]: another way of narrowing to a device, and
    /// so another way of narrowing to the wrong one.
    pub wider_place: Option<&'static str>,
    /// Holds a device name.
    pub device: Option<&'static str>,
    /// Holds a kind of device.
    pub kind: Option<&'static str>,
    /// Holds a further narrowing within a kind — a class of device. Same
    /// standing as [`Self::place`] again: it can only ever cut the set of
    /// devices down, so beside a name that already picks one out it can only
    /// cut it to nothing. A real call asking for the volume of a named display
    /// carried `device_class: ["tv"]` and the house refused the lot.
    pub filter: Option<&'static str>,
}

/// Pick the implementation for whichever software produced a result.
///
/// Recognition is by the shape of the answer rather than by a name the server
/// gave us earlier, for two reasons. It needs nothing remembered, so an
/// existing installation does not quietly lose its failure detection until the
/// next time it reconnects. And a server that declines to name itself, or names
/// itself something new, is still understood.
///
/// Falls back to [`Unknown`], which claims nothing — an unrecognised server
/// still works, it simply cannot be proved to have worked, and Fono says so
/// rather than pretending otherwise.
pub fn for_result(result: &str) -> &'static dyn Vendor {
    const KNOWN: &[&dyn Vendor] = &[&HomeAssistant];
    let parsed = serde_json::from_str::<serde_json::Value>(result).ok();
    for v in KNOWN {
        if parsed.as_ref().is_some_and(|p| v.recognises(p)) {
            return *v;
        }
    }
    &Unknown
}

/// Pick the implementation for a server we only know by the tools it offers.
///
/// The rails have to be built before anything has run, so there is no result
/// payload to go on — only the catalogue. Falls back to [`Unknown`], which
/// claims no field names, so an unrecognised catalogue is constrained by its
/// own published schemas and nothing else.
pub fn for_catalogue(tools: &[&str]) -> &'static dyn Vendor {
    const KNOWN: &[&dyn Vendor] = &[&HomeAssistant];
    for v in KNOWN {
        if v.recognises_catalogue(tools) {
            return *v;
        }
    }
    &Unknown
}

/// Turn a server's own refusal into one sentence, if any vendor can read it.
///
/// Recognition is by the shape of the refusal, for the same reason
/// [`for_result`] recognises a result that way: nothing has to be remembered,
/// and a server that renames itself is still understood. An unreadable refusal
/// yields `None` and is passed on exactly as the server wrote it.
pub fn refusal(text: &str) -> Option<String> {
    const KNOWN: &[&dyn Vendor] = &[&HomeAssistant];
    KNOWN.iter().find_map(|v| v.refusal(text))
}

/// A server we have no specific knowledge of.
pub struct Unknown;

impl Vendor for Unknown {
    fn id(&self) -> &'static str {
        "unknown"
    }

    fn recognises(&self, _result: &serde_json::Value) -> bool {
        false
    }
}

/// Home Assistant, via its built-in MCP server.
pub struct HomeAssistant;

impl Vendor for HomeAssistant {
    fn id(&self) -> &'static str {
        "home-assistant"
    }

    /// Every intent result carries a `response_type`, and the ones worth
    /// judging carry a `data` object listing what was and was not touched.
    fn recognises(&self, result: &serde_json::Value) -> bool {
        result.get("response_type").is_some()
            && (result.pointer("/data/success").is_some()
                || result.pointer("/data/failed").is_some())
    }

    /// Home Assistant reports per-target outcomes inside an otherwise
    /// successful result: `{"data": {"success": [...], "failed": [...]}}`.
    ///
    /// An empty `success` where the field exists means no device was touched —
    /// which is exactly what happened when a Romanian command asked for an area
    /// named `bucătărie` in a house whose areas are all named in English.
    ///
    /// A non-empty `failed` beside a non-empty `success` is a different animal
    /// and used to be reported as the same thing. Asked to turn on the light in
    /// the office, the house switched on the air conditioning, failed on the
    /// one lamp that was wanted, and Fono told the model the command had simply
    /// not worked. Both halves are news, so both are carried.
    ///
    /// Deliberately one-sided: a payload without these fields yields `None`
    /// rather than a verdict. Guessing the other way would report working
    /// commands as broken.
    fn admission(&self, result: &str) -> Option<Admission> {
        let v: serde_json::Value = serde_json::from_str(result).ok()?;
        let data = v.get("data").unwrap_or(&v);
        let failed = data.get("failed").and_then(|f| f.as_array());
        let success = data.get("success").and_then(|s| s.as_array());
        if failed.is_none() && success.is_none() {
            return None;
        }
        // An area is a grouping, not a device: a result whose only success is
        // the area itself touched nothing.
        let switched = success.is_some_and(|a| a.iter().any(is_entity));
        let missed: Vec<String> = failed
            .map(|a| a.iter().filter(|e| is_entity(e)).filter_map(name_of).collect())
            .unwrap_or_default();
        Some(match (switched, missed.is_empty()) {
            (false, _) => Admission::NothingWorked,
            (true, true) => Admission::Worked,
            (true, false) => Admission::PartlyWorked { failed: missed },
        })
    }

    /// Home Assistant refuses a command it cannot aim by raising
    /// `MatchFailedError`, whose `repr` carries the reason it gave up, the whole
    /// state of every device it considered, and the filters it was matching
    /// against. Three of those things are worth a sentence: why it gave up,
    /// what the call aimed at, and — where the trouble is that several devices
    /// answer to it — what they are called.
    ///
    /// The device names come from the `friendly_name` of each state it listed,
    /// which is the same spelling a command has to use to reach one.
    fn refusal(&self, text: &str) -> Option<String> {
        let reason: String = text
            .split_once("MatchFailedReason.")?
            .1
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || *c == '_')
            .collect();
        if reason.is_empty() {
            return None;
        }
        let filters = text.split_once("MatchTargetsConstraints(").map_or("", |(_, t)| t);
        let aimed_at = [
            between(text, "MatchTargetsConstraints(name=", ",").map(|n| format!("the name {n}")),
            between(filters, "domains=[", "]").map(|k| format!("the kind {k}")),
            between(filters, "area_name=", ",").map(|a| format!("the area {a}")),
            between(filters, "floor_name=", ",").map(|f| format!("the floor {f}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let aimed_at =
            if aimed_at.is_empty() { "this call".to_string() } else { aimed_at.join(" with ") };
        // Every device it weighed up and could not choose between.
        let candidates: Vec<&str> = text
            .split("friendly_name=")
            .skip(1)
            .filter_map(|s| s.split(',').next())
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect();
        Some(if reason == "MULTIPLE_TARGETS" && candidates.len() > 1 {
            format!(
                "{aimed_at} matches more than one device: {}. It reaches one device at a time, \
                 so name the one that was meant and leave out the area.",
                candidates.join(", ")
            )
        } else {
            format!(
                "{aimed_at} did not pick out a device this home can act on ({}).",
                reason.to_lowercase().replace('_', " ")
            )
        })
    }

    /// The two intents [`desired_state`] knows, which is not a coincidence: a
    /// tool that names an end state is exactly a tool that needs no value.
    fn switches(&self) -> &'static [&'static str] {
        SWITCHES
    }

    /// Read off the intent's own name. Home Assistant names its intents after
    /// the thing they operate — `HassClimateSetTemperature`, `HassLightSet` —
    /// so the kind is stated rather than inferred, and an intent whose name
    /// says nothing about a kind says nothing here either.
    fn kind_of(&self, tool: &str) -> Option<&'static str> {
        KINDS.iter().find(|(intent, _)| tool.starts_with(intent)).map(|(_, kind)| *kind)
    }

    /// The same two intents, and for the same reason: they name a state the
    /// world should end in rather than a change to make, so asking twice is
    /// asking once. Everything else — brightness, position, a temperature step
    /// — is left alone, because a wrong guess here doubles a real-world effect.
    fn repeatable(&self, tool: &str) -> bool {
        desired_state(tool).is_some()
    }

    fn confirms(&self, call: &ToolCall, result: &str, readback: &str) -> Option<Verdict> {
        let want = desired_state(&call.name)?;
        let touched = claimed_entities(result);
        // Nothing was claimed, so there is nothing to look up. The weaker rung
        // has already dealt with that case; saying "contradicted" here would
        // report the same failure twice in different words.
        if touched.is_empty() {
            return None;
        }
        let states = observed_states(readback, &touched);
        if states.is_empty() {
            return None;
        }
        // Every device the server said it switched must actually be in the
        // asked-for state. One lamp left behind is a half-done command, and
        // the user needs to hear that rather than "done".
        //
        // A reading that says nothing either way is not evidence, so a set of
        // those yields no verdict at all rather than a confident one.
        let mut judged = states.iter().filter_map(|s| want.met_by(s)).peekable();
        judged.peek()?;
        Some(if judged.all(|met| met) { Verdict::Confirmed } else { Verdict::Contradicted })
    }

    /// Read off the same two lists [`Self::admission`] judges, one entry at a
    /// time instead of one verdict for the lot.
    ///
    /// Areas are skipped for the reason they are skipped everywhere else here: a
    /// area is a grouping with no state of its own, so recording a run against
    /// it would put a history on something that cannot have one.
    fn targets(&self, result: &str) -> Vec<Target> {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(result) else { return Vec::new() };
        let data = v.get("data").unwrap_or(&v);
        let mut out = Vec::new();
        for (field, landed) in [("success", true), ("failed", false)] {
            let Some(list) = data.get(field).and_then(|f| f.as_array()) else { continue };
            out.extend(
                list.iter()
                    .filter(|e| is_entity(e))
                    .filter_map(name_of)
                    .map(|name| Target { name, landed }),
            );
        }
        out
    }

    /// Read the same dump [`Self::confirms`] reads, and report it instead of
    /// judging it.
    fn readings(&self, readback: &str, names: &[String]) -> Vec<(String, String)> {
        observed(readback, names)
    }

    /// The words Home Assistant uses across its whole intent interface.
    ///
    /// They are part of its public API — every voice integration in existence
    /// sends these names — so they cannot change without breaking far more than
    /// Fono. That is the entire reason this is three field names and not a list
    /// of the couple of dozen tools a house currently exposes: a new release can
    /// add and rename tools freely and this stays correct, whereas a tool table
    /// would rot silently at every upgrade.
    fn slot_fields(&self) -> SlotFields {
        SlotFields {
            place: Some("area"),
            wider_place: Some("floor"),
            device: Some("name"),
            kind: Some("domain"),
            filter: Some("device_class"),
        }
    }

    /// Recognised by the intent-name prefix every one of its tools carries.
    ///
    /// One match is enough, and the prefix is specific enough that no other
    /// server would collide with it.
    fn recognises_catalogue(&self, tools: &[&str]) -> bool {
        tools.iter().any(|t| t.starts_with("Hass"))
    }
}

/// The text after `open`, up to the first `close` that follows it.
///
/// Home Assistant spells an absent filter `None`, which is not a value to
/// report, so it reads as nothing at all. Quotes come off: they are Python's
/// punctuation, not part of a room's name.
fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let rest = text.split_once(open)?.1;
    let found = rest.split_once(close).map_or(rest, |(head, _)| head);
    let found = found.trim().trim_matches('\'').trim();
    (!found.is_empty() && found != "None").then_some(found)
}

/// The intents that switch a device without setting anything on it.
const SWITCHES: &[&str] = &["HassTurnOn", "HassTurnOff"];

/// The intents whose names state which kind of device they act on, and the
/// `domain` value that names that kind.
///
/// Matched on the leading word so a house running a newer Home Assistant, with
/// intents these lines have never seen, is still read correctly — every climate
/// intent begins `HassClimate`. The two volume intents are named outright
/// because they do not follow that pattern.
const KINDS: &[(&str, &str)] = &[
    ("HassClimate", "climate"),
    ("HassLight", "light"),
    ("HassMedia", "media_player"),
    ("HassSetVolume", "media_player"),
    ("HassVacuum", "vacuum"),
];

/// The state a tool is asking for, when its name says.
///
/// Only the plain on/off intents are covered. Brightness, colour and position
/// are deliberately absent: a wrong guess about what "set" meant would be a
/// confident false verdict, which is worse than no verdict.
fn desired_state(tool: &str) -> Option<Wanted> {
    match tool {
        "HassTurnOn" => Some(Wanted::On),
        "HassTurnOff" => Some(Wanted::Off),
        _ => None,
    }
}

/// Where a switching intent asked the world to end up.
///
/// A reading is compared by meaning rather than by equality with the words
/// `on` and `off`, because only a light spells it that way. A blind that is on
/// reports `open`, an air conditioner reports the mode it is running in, a
/// media player reports `playing`, a vacuum `cleaning`. Comparing those
/// against the literal `on` reports a call that worked as a call that failed —
/// and the model then "corrects" it by undoing what the user asked for, which
/// is how a request to open a blind ended up closing it.
///
/// Off has a short, closed list of spellings across Home Assistant's domains;
/// on has as many spellings as there are things a device can be busy doing. So
/// the list below is the off side, and everything else counts as on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Wanted {
    On,
    Off,
}

impl Wanted {
    /// Whether a device reporting `state` is where this intent wanted it, or
    /// `None` when the reading is no evidence either way.
    fn met_by(self, state: &str) -> Option<bool> {
        let state = state.trim().trim_matches('\'').to_ascii_lowercase();
        let resting = match state.as_str() {
            // Nobody has reported, or the device is beyond reach.
            "" | "unavailable" | "unknown" => return None,
            // Still on its way. The server took the command and the world has
            // not caught up — a roller takes seconds, and the reading happens
            // in milliseconds. Judge it by where it is heading, which is what
            // the reading is evidence of.
            "opening" => Self::On,
            "closing" | "returning" => Self::Off,
            "off" | "closed" | "standby" | "docked" => Self::Off,
            _ => Self::On,
        };
        Some(resting == self)
    }
}

/// A device, as opposed to an area — which is a grouping with no state of its
/// own, and so neither evidence that anything happened nor a thing to read.
///
/// Anything that does not call itself an area counts, rather than only what
/// calls itself an entity: a target whose kind the server left out is still a
/// thing that was or was not switched, and dropping it would quietly turn a
/// half-done command back into a clean success.
fn is_entity(target: &serde_json::Value) -> bool {
    target.get("type").and_then(|t| t.as_str()) != Some("area")
}

fn name_of(target: &serde_json::Value) -> Option<String> {
    target.get("name").and_then(|n| n.as_str()).map(str::to_string)
}

/// The devices the server itself said it switched.
fn claimed_entities(result: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(result) else { return Vec::new() };
    let data = v.get("data").unwrap_or(&v);
    let Some(list) = data.get("success").and_then(|s| s.as_array()) else { return Vec::new() };
    list.iter().filter(|e| is_entity(e)).filter_map(name_of).collect()
}

/// Look up those devices in a fresh reading of the house.
///
/// `GetLiveContext` returns a block per device, keyed by the same name the
/// result used:
///
/// ```text
/// - names: Hall lamp
///   domain: light
///   state: 'on'
/// ```
///
/// Two kinds of silence are skipped rather than counted as wrong, because a
/// thing that cannot tell us about itself is missing evidence, not evidence of
/// failure: a device the reading does not mention, and one that is offline. The
/// second is not hypothetical — a real kitchen turned out to contain a lamp
/// Home Assistant happily reports switching while it sits `unavailable`, so
/// counting that as a contradiction would call a working command broken every
/// single time.
fn observed_states(readback: &str, wanted: &[String]) -> Vec<String> {
    observed(readback, wanted).into_iter().map(|(_, state)| state).collect()
}

/// Every named device the reading mentions, with the state it is in.
fn observed(readback: &str, wanted: &[String]) -> Vec<(String, String)> {
    // `GetLiveContext` hands the dump back as a JSON string under `result`,
    // where the newlines are escaped. Read through that when it is there: the
    // block-per-device parsing below needs real line breaks, and without this
    // every lookup silently finds nothing — which would read as "no evidence"
    // and quietly disable the whole check.
    let unwrapped = serde_json::from_str::<serde_json::Value>(readback)
        .ok()
        .and_then(|v| v.get("result").and_then(|r| r.as_str()).map(str::to_string));
    let text = unwrapped.as_deref().unwrap_or(readback);

    let mut out = Vec::new();
    for block in text.split("- names: ").skip(1) {
        let (name, rest) = block.split_once('\n').unwrap_or((block, ""));
        if !wanted.iter().any(|w| w == name.trim()) {
            continue;
        }
        if let Some(state) = rest.lines().find_map(|l| l.trim().strip_prefix("state:")) {
            let state = state.trim().trim_matches('\'');
            if state == "unavailable" || state == "unknown" {
                continue;
            }
            out.push((name.trim().to_string(), state.to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Payload shapes copied from a real house, with the device names changed.
    const SWITCHED_A_LAMP: &str = r#"{"speech": {}, "response_type": "action_done", "data": {"success": [{"name": "Hall", "type": "area"}, {"name": "Hall lamp", "type": "entity"}], "failed": []}}"#;
    const TOUCHED_NOTHING: &str =
        r#"{"response_type": "action_done", "data": {"success": [], "failed": []}}"#;
    /// A real office: the climate came on, the light did not.
    const HALF_DONE_ROOM: &str = r#"{"speech": {}, "response_type": "action_done", "data": {"success": [{"name": "Office", "type": "area"}, {"name": "Office air conditioner", "type": "entity"}], "failed": [{"name": "Office TV Light", "type": "entity"}]}}"#;
    /// A real refusal, shortened in the middle of each device's attributes:
    /// asked to set a temperature in an area holding both a thermostat and an
    /// air conditioner, the house could not tell which was meant.
    const TWO_OF_A_KIND: &str = "Error calling tool: <MatchFailedError \
         result=MatchTargetsResult(is_match=False, \
         no_match_reason=<MatchFailedReason.MULTIPLE_TARGETS: 12>, \
         states=[<state climate.office_thermostat=off; min_temp=0.0, current_temperature=24, \
         friendly_name=Office thermostat, supported_features=401 @ \
         2026-08-02T14:37:13.080064+03:00>, <state climate.office_air_conditioner=off; \
         drlc_status_level=-1, friendly_name=Office air conditioner, supported_features=441 @ \
         2026-08-03T13:58:02.279993+03:00>], no_match_name=None, areas=[], floors=[]), \
         constraints=MatchTargetsConstraints(name=None, area_name='Office', floor_name=None, \
         domains=['climate'], device_classes=None, single_target=True), preferences=None>";

    fn call(name: &str) -> ToolCall {
        ToolCall { id: "1".into(), name: name.into(), arguments: "{}".into() }
    }

    #[test]
    fn a_server_we_do_not_know_claims_nothing() {
        let v = for_result(r#"{"status":"queued","id":7}"#);
        assert_eq!(v.id(), "unknown");
        assert_eq!(v.admission(SWITCHED_A_LAMP), None, "no opinion, not a verdict");
        assert!(v.readings("anything", &["Hall lamp".into()]).is_empty(), "cannot read it");
        assert_eq!(v.confirms(&call("HassTurnOn"), SWITCHED_A_LAMP, ""), None);
        // Prose, or nothing at all, is nobody's payload.
        assert_eq!(for_result("Done.").id(), "unknown");
        assert_eq!(for_result("").id(), "unknown");
    }

    /// Recognition comes from the answer itself, not from a name the server
    /// gave us earlier. An installation that predates this check therefore
    /// keeps working without having to reconnect first — otherwise it would
    /// silently lose its failure detection, which is the exact bug this whole
    /// module exists to prevent.
    #[test]
    fn the_house_is_recognised_by_its_answer() {
        assert_eq!(for_result(SWITCHED_A_LAMP).id(), "home-assistant");
        assert_eq!(for_result(TOUCHED_NOTHING).id(), "home-assistant");
    }

    /// The first payload is the one that caused a complaint: it *worked* — a
    /// lamp came on — yet its trailing `"failed": []` made keyword matching
    /// log it as a failure.
    #[test]
    fn a_working_command_and_an_empty_one_are_told_apart() {
        let ha = HomeAssistant;
        assert_eq!(ha.admission(SWITCHED_A_LAMP), Some(Admission::Worked));
        assert_eq!(ha.admission(TOUCHED_NOTHING), Some(Admission::NothingWorked));
        assert_eq!(
            ha.admission(r#"{"data": {"success": [], "failed": [{"name": "x"}]}}"#),
            Some(Admission::NothingWorked)
        );
        // Nothing recognisable in it, so no verdict either way.
        assert_eq!(ha.admission("Done."), None);
        assert_eq!(ha.admission(r#"{"speech": {"plain": {"speech": "ok"}}}"#), None);
    }

    /// A per-device history needs the names out of the *reply*, not the
    /// arguments: the half-done office command named an area, and both device
    /// names it actually reached appear nowhere else. The area itself is not a
    /// device and must not collect a history of its own.
    #[test]
    fn each_thing_the_house_touched_is_named_separately() {
        let ha = HomeAssistant;
        assert_eq!(
            ha.targets(HALF_DONE_ROOM),
            vec![
                Target { name: "Office air conditioner".into(), landed: true },
                Target { name: "Office TV Light".into(), landed: false },
            ],
            "the area is skipped, and the one that failed is kept with its verdict"
        );
        assert_eq!(ha.targets(TOUCHED_NOTHING), vec![], "nothing was reached");
        // A server we do not know says nothing rather than guessing, so no
        // device anywhere gets a run recorded against it.
        assert_eq!(for_result(r#"{"ok":true}"#).targets(HALF_DONE_ROOM), vec![]);
        assert_eq!(ha.targets("Done."), vec![]);
    }

    /// The office payload that started this: asked for the light, the house
    /// switched on the air conditioning and failed on the lamp. Reporting that
    /// as "did not work" is as wrong as reporting it as done, and the names of
    /// the devices that were missed are the part the reply needs.
    #[test]
    fn a_half_done_command_is_neither_a_success_nor_a_failure() {
        let ha = HomeAssistant;
        let Some(Admission::PartlyWorked { failed }) = ha.admission(HALF_DONE_ROOM) else {
            panic!("an area with successes and failures worked in part");
        };
        assert_eq!(failed, vec!["Office TV Light".to_string()]);
    }

    /// The area itself always comes back as a success, so counting it as a
    /// device would report a command that matched nothing as half-done.
    #[test]
    fn a_room_on_its_own_is_not_a_device_that_was_switched() {
        let ha = HomeAssistant;
        let only_the_area = r#"{"response_type": "action_done", "data": {"success": [{"name": "Hall", "type": "area"}], "failed": []}}"#;
        assert_eq!(ha.admission(only_the_area), Some(Admission::NothingWorked));
    }

    /// A switch-on that partly failed may be asked for again — the lamps that
    /// obeyed are already in the state asked for, so a repeat cannot double
    /// anything. A relative change may not, because twice is twice as much.
    #[test]
    fn only_a_command_naming_an_end_state_may_be_asked_for_twice() {
        let ha = HomeAssistant;
        assert!(ha.repeatable("HassTurnOn"), "being on twice is being on");
        assert!(ha.repeatable("HassTurnOff"));
        assert!(!ha.repeatable("HassLightSet"), "a brightness step must not be doubled");
        assert!(!ha.repeatable("HassClimateSetTemperature"));
        // A server we do not recognise gets no second attempt: it is the safe
        // direction to be wrong in.
        assert!(!Unknown.repeatable("HassTurnOn"));
    }

    /// The point of the whole rung: the server said it switched a lamp, and
    /// the house is asked whether that is true.
    #[test]
    fn the_house_can_agree_or_disagree_with_the_server() {
        let ha = HomeAssistant;
        let lit = "- names: Hall lamp\n  domain: light\n  state: 'on'\n  areas: Hall\n";
        let dark = "- names: Hall lamp\n  domain: light\n  state: 'off'\n  areas: Hall\n";

        assert_eq!(
            ha.confirms(&call("HassTurnOn"), SWITCHED_A_LAMP, lit),
            Some(Verdict::Confirmed)
        );
        assert_eq!(
            ha.confirms(&call("HassTurnOn"), SWITCHED_A_LAMP, dark),
            Some(Verdict::Contradicted),
            "the server said it switched a lamp that is still off"
        );
        assert_eq!(
            ha.confirms(&call("HassTurnOff"), SWITCHED_A_LAMP, dark),
            Some(Verdict::Confirmed)
        );
    }

    /// Only a light spells being on as the word `on`. This is the bug that
    /// made Fono close a blind it had just opened: the server switched the
    /// cover, the cover read `open`, Fono told the model the call had failed,
    /// and the model obligingly sent the opposite one and reported success.
    #[test]
    fn a_device_that_is_on_does_not_have_to_say_on() {
        let ha = HomeAssistant;
        let claimed =
            r#"{"data": {"success": [{"name": "Hall lamp", "type": "entity"}], "failed": []}}"#;
        let reads = |state: &str| format!("- names: Hall lamp\n  state: '{state}'\n");

        // On, in the words of a blind, an air conditioner, a media player and
        // a vacuum cleaner.
        for state in ["on", "open", "cool", "dry", "playing", "idle", "cleaning"] {
            assert_eq!(
                ha.confirms(&call("HassTurnOn"), claimed, &reads(state)),
                Some(Verdict::Confirmed),
                "{state} is a device doing something"
            );
        }
        for state in ["off", "closed", "standby", "docked"] {
            assert_eq!(
                ha.confirms(&call("HassTurnOff"), claimed, &reads(state)),
                Some(Verdict::Confirmed),
                "{state} is a device at rest"
            );
            assert_eq!(
                ha.confirms(&call("HassTurnOn"), claimed, &reads(state)),
                Some(Verdict::Contradicted)
            );
        }
        // A roller takes seconds to travel and the reading happens in
        // milliseconds, so it is caught mid-way far more often than not.
        // Where it is heading is what the reading is evidence of.
        assert_eq!(
            ha.confirms(&call("HassTurnOn"), claimed, &reads("opening")),
            Some(Verdict::Confirmed)
        );
        assert_eq!(
            ha.confirms(&call("HassTurnOff"), claimed, &reads("closing")),
            Some(Verdict::Confirmed)
        );
        assert_eq!(
            ha.confirms(&call("HassTurnOff"), claimed, &reads("returning")),
            Some(Verdict::Confirmed),
            "a vacuum on its way to the dock is being switched off"
        );
    }

    /// Sensor readings drift on their own — two readings of one real house
    /// three seconds apart differed by two tenths of a degree with nothing
    /// happening. Looking only at what the server claimed to touch, and only
    /// at whether it is in the asked-for state, makes that irrelevant.
    #[test]
    fn a_drifting_sensor_cannot_fake_a_successful_switch() {
        let ha = HomeAssistant;
        let house = "- names: Soil temperature\n  domain: sensor\n  state: '25.5'\n\
                     - names: Hall lamp\n  domain: light\n  state: 'off'\n";
        assert_eq!(
            ha.confirms(&call("HassTurnOn"), SWITCHED_A_LAMP, house),
            Some(Verdict::Contradicted),
            "the lamp is off; a sensor moving elsewhere is not evidence"
        );
    }

    /// Three ways to have no opinion, none of which may be reported as a
    /// failure: a tool whose intent we cannot infer, a result claiming
    /// nothing, and a device the reading does not mention.
    #[test]
    fn what_cannot_be_judged_is_left_alone() {
        let ha = HomeAssistant;
        let lit = "- names: Hall lamp\n  domain: light\n  state: 'on'\n";
        assert_eq!(
            ha.confirms(&call("HassLightSet"), SWITCHED_A_LAMP, lit),
            None,
            "brightness intent is not guessed at"
        );
        // No verdict, and still something to say: what the lamp reads is
        // reported even where it cannot be judged.
        assert_eq!(
            ha.readings(lit, &["Hall lamp".to_string()]),
            vec![("Hall lamp".to_string(), "on".to_string())]
        );
        assert_eq!(ha.confirms(&call("HassTurnOn"), TOUCHED_NOTHING, lit), None);
        assert_eq!(
            ha.confirms(&call("HassTurnOn"), SWITCHED_A_LAMP, "- names: Other\n  state: 'on'\n"),
            None,
            "a device the house did not mention is missing evidence, not failure"
        );
        assert!(
            ha.readings(lit, &["Other".to_string()]).is_empty(),
            "a device the house did not mention reads as nothing"
        );
    }

    /// A device can be listed as switched and be offline, which a real kitchen
    /// demonstrated on the first attempt. It has no state to disagree with, so
    /// it must not drag a working command down with it.
    #[test]
    fn a_device_that_is_offline_does_not_condemn_the_rest() {
        let ha = HomeAssistant;
        let house = "- names: Hall lamp\n  domain: light\n  state: 'on'\n\
                     - names: Broken lamp\n  domain: light\n  state: 'unavailable'\n";
        let claimed = r#"{"response_type": "action_done", "data": {"success": [{"name": "Hall lamp", "type": "entity"}, {"name": "Broken lamp", "type": "entity"}], "failed": []}}"#;
        assert_eq!(
            ha.confirms(&call("HassTurnOn"), claimed, house),
            Some(Verdict::Confirmed),
            "one lamp is on and the other cannot say; that is not a contradiction"
        );
    }

    /// A refusal the house wrote for its own debugging is no use to a model:
    /// the two facts that matter sit in the middle of 1400 characters, which is
    /// exactly the part a shortener throws away. One sentence carries them.
    #[test]
    fn a_refusal_is_read_back_as_one_sentence() {
        let ha = HomeAssistant;
        let said = ha.refusal(TWO_OF_A_KIND).expect("the house said why");
        assert_eq!(
            said,
            "the kind climate with the area Office matches more than one device: Office \
             thermostat, Office air conditioner. It reaches one device at a time, so name the one \
             that was meant and leave out the area."
        );
        // Both names a command could use to reach one of them, and nothing else
        // invented alongside them.
        assert!(said.contains("Office air conditioner"));
        assert!(!said.contains("supported_features"), "none of the debugging goes to the model");
        assert!(said.len() < TWO_OF_A_KIND.len() / 3, "shorter than what it replaces");
    }

    /// Every other way the house can fail to aim a command gets a sentence too,
    /// naming what was aimed at and why it did not land. Read off the refusal
    /// alone, so no device is ever invented.
    #[test]
    fn a_refusal_that_matched_nothing_says_what_it_looked_for() {
        let ha = HomeAssistant;
        let missed = "<MatchFailedError result=MatchTargetsResult(is_match=False, \
                      no_match_reason=<MatchFailedReason.NAME: 1>, states=[], no_match_name='Lamp'), \
                      constraints=MatchTargetsConstraints(name='Lamp', area_name=None, \
                      floor_name=None, domains=['light'])>";
        assert_eq!(
            ha.refusal(missed).as_deref(),
            Some(
                "the name Lamp with the kind light did not pick out a device this home can \
                 act on (name)."
            )
        );
    }

    /// One-sided, like every other reading here: anything that is not one of
    /// ours goes to the model exactly as the server wrote it.
    #[test]
    fn an_unreadable_refusal_is_left_alone() {
        assert_eq!(refusal("Received invalid slot info"), None);
        assert_eq!(refusal(""), None);
        assert_eq!(refusal(r#"{"error":"upstream timeout"}"#), None);
        assert_eq!(Unknown.refusal(TWO_OF_A_KIND), None, "not this server's to explain");
        // Read by whoever can, without being told which server answered.
        assert!(refusal(TWO_OF_A_KIND).is_some());
    }

    /// The tools named as switches must be the ones whose end state is known,
    /// or a correction would send the model to a tool that needs a value after
    /// all — the very mistake it is there to undo.
    #[test]
    fn the_switches_are_the_tools_that_need_no_value() {
        let ha = HomeAssistant;
        assert!(!ha.switches().is_empty());
        for tool in ha.switches() {
            assert!(desired_state(tool).is_some(), "{tool} names an end state");
        }
        assert!(Unknown.switches().is_empty(), "a server we do not know names no tool");
    }

    /// A tool that sets one kind of thing says so in its name, and that is the
    /// fact a plain switch-on loses. Read off the name only — a tool whose name
    /// says nothing gets no kind put in its mouth.
    #[test]
    fn a_tool_that_names_a_kind_of_device_is_read_as_being_about_it() {
        let ha = HomeAssistant;
        assert_eq!(ha.kind_of("HassClimateSetTemperature"), Some("climate"));
        assert_eq!(ha.kind_of("HassLightSet"), Some("light"));
        assert_eq!(ha.kind_of("HassSetVolume"), Some("media_player"));
        for switch in ha.switches() {
            assert_eq!(ha.kind_of(switch), None, "{switch} switches anything at all");
        }
        assert_eq!(ha.kind_of("HassGetState"), None);
        assert_eq!(Unknown.kind_of("HassClimateSetTemperature"), None);
    }

    /// The house sends its state as an escaped JSON string, not as loose text.
    /// Failing to read through that wrapper would find no device anywhere,
    /// which looks exactly like "no evidence" — so the check would turn itself
    /// off and nobody would notice.
    #[test]
    fn the_reading_is_understood_however_the_house_wraps_it() {
        let ha = HomeAssistant;
        let dump = "Live Context:\n- names: Hall lamp\n  domain: light\n  state: 'on'\n";
        let wrapped = serde_json::json!({"result": dump}).to_string();
        assert_eq!(
            ha.confirms(&call("HassTurnOn"), SWITCHED_A_LAMP, &wrapped),
            Some(Verdict::Confirmed)
        );
        assert_eq!(
            ha.confirms(&call("HassTurnOn"), SWITCHED_A_LAMP, dump),
            Some(Verdict::Confirmed),
            "and plain text still works"
        );
    }
}
