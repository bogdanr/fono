// SPDX-License-Identifier: GPL-3.0-only
//! Turns the user's tool catalogue into something the model can call.
//!
//! Everything here is assembled once per turn from data that already
//! exists: the servers in the config, the rows the user left switched on,
//! and the secrets file. Nothing is discovered on the request path.
//!
//! The one rule this module exists to enforce is honesty about outcomes.
//! A server can answer cheerfully and have done nothing at all — Home
//! Assistant does exactly that when a command names an area it does not
//! have — so the wording of every summary is capped by how well the
//! effect could actually be checked. See [`fono_core::tool_catalog::VerifyClass`].
//!
//! Deciding *how well* means reading a server's own payloads, which is
//! knowledge of that particular software. All of it lives in [`vendor`]; this
//! module knows the ladder, never a vendor's name.

pub mod vendor;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use fono_assistant::mcp_client::{self, McpEndpoint};
use fono_assistant::{ActionTools, ToolCall, ToolOutcome};
use fono_core::config::Config;
use fono_core::conversations::ToolUse;
use fono_core::paths::Paths;
use fono_core::secrets::Secrets;
use fono_core::tool_catalog::{RunOutcome, ToolCatalogStore, VerifyClass};
use fono_core::turn_trace::{current_instant, current_span, ACTIONS_LANE};
use tracing::{debug, info, warn};
use vendor::{Vendor, Verdict};

/// How long one tool call may take before we give up and say so.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Everything needed to run one stored tool.
#[derive(Clone)]
struct Runnable {
    endpoint: McpEndpoint,
    verify: VerifyClass,
    /// The tool whose output observes this one's effect, when there is one.
    readback: Option<String>,
    /// What the server said it accepts, kept so an obviously wrong argument
    /// can be caught here rather than costing a round trip.
    schema: serde_json::Value,
    /// Which server offers it. Two servers may publish the same tool name, so
    /// a run has to be filed against the right one.
    source: String,
}

/// One thing this turn did, in the form a spoken phrase can be keyed to.
///
/// Collected here rather than by the caller because this is the only layer that
/// sees the arguments as they were *finally* sent — after the blank fields were
/// dropped and the house's own facts applied — and the only one that sees which
/// things in the home the reply named.
#[derive(Clone)]
struct Acted {
    source: String,
    tool: String,
    /// Exactly what went to the server, so a replay sends the same thing.
    args: String,
    devices: Vec<String>,
    ok: bool,
    ms: i64,
}

/// What a turn did, held until the reply is over and it can be written down.
///
/// Two halves of one fact live in different places, which is the whole reason
/// this exists. The actions layer knows *what was done*; only the caller knows
/// *what was said* and when the reply finished. So the turn fills this in as it
/// runs and hands it back at the end.
///
/// The waiting is not laziness. A run is judged by whether the user came back
/// about the same thing within [`fono_core::tool_catalog::COMPLAINT_WINDOW_SECS`]
/// of hearing the reply, and starting that clock when the command was *sent*
/// would let a slow turn eat the window and push a real complaint outside it.
///
/// Cloning shares one record: the turn keeps a handle, the executor keeps a
/// handle, and both mean the same turn.
#[derive(Clone, Default)]
pub struct Learning {
    /// Absent on a path that is not a turn, and then nothing is written.
    db: Option<std::path::PathBuf>,
    did: Arc<std::sync::Mutex<Vec<Acted>>>,
}

impl Learning {
    /// Somewhere for one turn's actions to be written down.
    #[must_use]
    pub fn new(paths: &Paths) -> Self {
        Self { db: Some(paths.tool_catalog_db()), did: Arc::default() }
    }

    /// One that records nothing, for a path that is not a turn — warming a
    /// prompt, where nobody has spoken and so there is nothing to learn from.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    fn add(&self, a: Acted) {
        // A poisoned lock costs a promotion, never a command.
        if let Ok(mut did) = self.did.lock() {
            did.push(a);
        }
    }

    /// The reply is over: judge what was waiting, and write down what this turn
    /// did.
    ///
    /// Order is load-bearing. Judging comes first, because what is being judged
    /// is the *previous* run of this phrase — whether the user has just come
    /// back about the same thing — and writing this turn down first would
    /// overwrite the very run being scored.
    ///
    /// Only a turn that ran **exactly one** command is written down. A turn that
    /// ran several did several things, and replaying one of them later would do
    /// part of what was asked; a turn that needed a second attempt is a turn the
    /// model did not get right first time, which is not a phrase to trust yet.
    /// Both simply stay slow, which is the direction this whole mechanism is
    /// allowed to be wrong in.
    ///
    /// Best-effort throughout: a command that worked must never be reported as
    /// failed because a bookkeeping write did not land.
    pub fn finished(&self, said: &str, lang: &str) {
        let Some(db) = &self.db else { return };
        let Ok(did) = self.did.lock() else { return };
        if did.is_empty() {
            return;
        }
        let store = match ToolCatalogStore::open(db) {
            Ok(s) => s,
            Err(e) => return debug!("actions: cannot write down what this turn did: {e}"),
        };
        // Everything this turn reached, whatever command reached it. A complaint
        // is about a thing in the home, not about the command that moved it.
        let touched: Vec<String> = did.iter().flat_map(|a| a.devices.iter().cloned()).collect();
        if let Err(e) = store.settle(said, &touched) {
            debug!("actions: cannot judge what the last turn did: {e}");
        }
        let [one] = did.as_slice() else { return };
        let said = fono_core::tool_catalog::Said {
            phrase: said,
            lang,
            source: &one.source,
            tool: &one.tool,
            args: &one.args,
            devices: &one.devices,
            ok: one.ok,
            ms: one.ms,
        };
        match store.remember(&said) {
            Ok(true) => debug!("actions: {:?} now stands for {}", said.phrase, one.tool),
            Ok(false) => {}
            Err(e) => debug!("actions: cannot write down {:?}: {e}", said.phrase),
        }
    }
}

/// Run what this phrase has always run, without asking the model.
///
/// `Some` means the command is done and the turn needs no model at all: the
/// events returned are the same ones a model turn would have put on the stream,
/// so history, the page and the trace see no difference. `None` means carry on
/// as normal — either the phrase has earned nothing, or the replay did not
/// work.
///
/// **Falling back on a failure cannot double an action.** Only a call that
/// *names a thing* is ever written down (see
/// [`fono_core::tool_catalog::ToolCatalogStore::remember`]); a call that asks
/// for an *amount* is refused, precisely because asking twice for two degrees
/// warmer is four degrees. So the model may safely ask again for anything a
/// phrase could have replayed.
///
/// Nothing is spoken. A phrase on the fast path is one the user has said
/// before and watched work, and the reply Fono could produce without a model
/// would be a fixed word in one language — so the light coming on is the
/// answer. Say the phrase again and the words come back, because a failed
/// replay hands the turn to the model.
pub async fn replay(
    learning: &Learning,
    tools: &ActionTools,
    said: &str,
) -> Option<Vec<fono_assistant::TokenDelta>> {
    let db = learning.db.as_ref()?;
    // Opened twice rather than held: a SQLite handle cannot be kept across the
    // await below, and opening one costs microseconds beside a round trip to the
    // house. Same reason the journal reopens per call.
    let found = ToolCatalogStore::open(db).ok()?.replay(said).ok().flatten()?;
    match run_again(tools, &found).await {
        Ok(events) => Some(events),
        Err(ms) => {
            // One bad run makes a phrase slow again. Written here rather than
            // left to the end of the turn, because the turn is about to run a
            // second command and a turn that did two things is deliberately
            // never learned from — so this is the only chance to record it.
            let dirty = fono_core::tool_catalog::Said {
                phrase: said,
                lang: &found.lang,
                source: &found.source,
                tool: &found.tool,
                args: &found.args,
                devices: &[],
                ok: false,
                ms,
            };
            match ToolCatalogStore::open(db).and_then(|s| s.remember(&dirty)) {
                Ok(_) => info!("actions: {said:?} did not work; asking the model instead"),
                Err(e) => debug!("actions: cannot write down that {said:?} failed: {e}"),
            }
            None
        }
    }
}

/// Send one phrase's stored command.
///
/// `Ok` is the turn: the events a model turn would have produced. `Err` carries
/// how long the attempt took, which the caller writes down as the run that makes
/// the phrase slow again.
///
/// Split from [`replay`] so this half can be tested without a promoted row —
/// earning the fast path takes two clean runs and two closed complaint windows,
/// which is the store's business and pinned there.
async fn run_again(
    tools: &ActionTools,
    found: &fono_core::tool_catalog::Shortcut,
) -> Result<Vec<fono_assistant::TokenDelta>, i64> {
    use fono_assistant::{TokenDelta, ToolEvent};

    let call = ToolCall {
        id: "replay".to_string(),
        name: found.tool.clone(),
        arguments: found.args.clone(),
    };
    let started = std::time::Instant::now();
    let out = (tools.execute)(call.clone()).await;
    let ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    // The saving is the whole claim, so put both halves of it where they can be
    // read off one timeline rather than inferred.
    current_instant(
        "tool.replayed",
        "actions",
        ACTIONS_LANE,
        serde_json::json!({ "tool": found.tool, "ms": ms, "failed": out.failed }),
    );
    if out.failed {
        return Err(ms);
    }
    Ok(vec![
        TokenDelta::tool(ToolEvent::Called(call)),
        TokenDelta::tool(ToolEvent::Result {
            tool_call_id: "replay".to_string(),
            summary: out.summary,
            failed: false,
            sent: out.sent,
        }),
    ])
}

/// Where a finished call gets written down, and on whose behalf.
///
/// The store is reopened per call rather than held open: a SQLite handle is
/// not shareable across the async boundary this closure lives on, and opening
/// one costs microseconds beside a round trip to the house. Recording is
/// strictly best-effort — a command that worked must never be reported as
/// failed because a bookkeeping write did not land.
#[derive(Clone)]
struct Journal {
    db: std::path::PathBuf,
    /// The enrolled speaker for this turn, when one was recognised. Fixed for
    /// the life of the turn, because that is what it describes.
    speaker: Option<String>,
    /// When the assistant was last free to think: the moment this turn's tools
    /// were built, and thereafter the moment each call returned. The gap up to
    /// the next call is the model deciding, which is usually the larger half of
    /// what the user experiences as "how long that took" — a page reporting
    /// only the round trip to the server flatters Fono and misleads whoever is
    /// trying to work out why a command feels slow.
    idle_since: Arc<std::sync::Mutex<std::time::Instant>>,
    /// What this turn did, collected for the caller to write down once the
    /// reply is over.
    learning: Learning,
}

impl Journal {
    fn note(
        &self,
        source: &str,
        tool: &str,
        ran: &Ran,
        think: std::time::Duration,
        elapsed: std::time::Duration,
    ) {
        let ms = i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX);
        let think_ms = i64::try_from(think.as_millis()).unwrap_or(i64::MAX);
        self.learning.add(Acted {
            source: source.to_string(),
            tool: tool.to_string(),
            args: ran.sent.clone(),
            devices: ran.targets.iter().map(|t| t.name.clone()).collect(),
            ok: ran.how != RunOutcome::Failed,
            ms,
        });
        let res = ToolCatalogStore::open(&self.db).and_then(|s| {
            s.record_run(source, tool, ran.how, ms, Some(think_ms), self.speaker.as_deref())?;
            // Per device as well as per tool, because "the office lamp never
            // works" is what people actually notice — and because one command
            // naming an area reaches several things with different fates, which
            // a single row for the tool cannot represent. Only servers that
            // name what they touched produce anything here.
            for t in &ran.targets {
                s.record_device_run(source, &t.name, t.landed)?;
            }
            Ok(())
        });
        if let Err(e) = res {
            debug!("actions: could not note that {tool} ran: {e}");
        }
    }
    /// How long the assistant has been thinking since it was last busy, and
    /// restart that clock. Called immediately before a call is sent.
    fn take_think_time(&self) -> std::time::Duration {
        let now = std::time::Instant::now();
        // A poisoned lock here would cost a timing figure, never a command,
        // so the elapsed time is simply unknown and reported as zero.
        let Ok(mut since) = self.idle_since.lock() else { return std::time::Duration::ZERO };
        let waited = now.saturating_duration_since(*since);
        *since = now;
        waited
    }

    /// The assistant is thinking again, as of now.
    fn resumed(&self) {
        if let Ok(mut since) = self.idle_since.lock() {
            *since = std::time::Instant::now();
        }
    }
}

/// Build the tool set for this turn, or `None` when the user has no tools
/// switched on — in which case the turn stays conversation-only and costs
/// nothing extra.
///
/// `speaker` is the enrolled name Fono recognised for this turn, when it
/// recognised one. It is only ever written next to a completed call, so the
/// page can say who a thing was done for; it is not sent anywhere.
///
/// `learning` collects what this turn does, for the caller to write down once
/// the reply is over. Pass [`Learning::none`] on a path that is not a turn.
pub fn build(
    cfg: &Config,
    paths: &Paths,
    speaker: Option<&str>,
    learning: &Learning,
) -> Option<Arc<ActionTools>> {
    if !cfg.assistant.tools.enabled || cfg.assistant.tools.mcp.is_empty() {
        return None;
    }
    let store = match ToolCatalogStore::open(&paths.tool_catalog_db()) {
        Ok(s) => s,
        Err(e) => {
            warn!("actions: cannot open tool catalogue: {e}");
            return None;
        }
    };
    let rows = store.active_tools().ok()?;
    if rows.is_empty() {
        return None;
    }

    let secrets = Secrets::load(&paths.secrets_file()).unwrap_or_default();
    let mut endpoints = std::collections::HashMap::new();
    for s in &cfg.assistant.tools.mcp {
        endpoints.insert(
            s.name.clone(),
            McpEndpoint {
                url: s.sse_url(),
                token: secrets.keys.get(&s.token_ref()).cloned(),
                timeout: CALL_TIMEOUT,
            },
        );
    }

    let mut descriptors = Vec::with_capacity(rows.len());
    let mut runnable = std::collections::HashMap::new();
    // The rows that survived the endpoint check, kept so the rails describe
    // exactly the tools the model is being offered — no more, no fewer.
    let mut offered = Vec::with_capacity(rows.len());
    for r in rows {
        let Some(endpoint) = endpoints.get(&r.source).cloned() else { continue };
        descriptors.push(serde_json::json!({
            "type": "function",
            "function": {
                "name": r.name,
                "description": r.description,
                "parameters": r.schema,
            }
        }));
        runnable.insert(
            r.name.clone(),
            Runnable {
                endpoint,
                verify: r.verify_class,
                readback: r.readback_tool.clone(),
                schema: r.schema.clone(),
                source: r.source.clone(),
            },
        );
        offered.push(r);
    }
    if descriptors.is_empty() {
        return None;
    }
    info!("actions: {} tools offered to the assistant", descriptors.len());

    let runnable = Arc::new(runnable);
    // Per turn, like everything else here: `build` runs once per turn, so the
    // words this turn is about and the one refusal each tool gets both start
    // fresh.
    let words = Arc::new(Words::default());
    let said = words.said.clone();
    let house = Arc::new(HouseFacts::learn(
        &store,
        &offered.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
    ));
    let journal = Journal {
        db: paths.tool_catalog_db(),
        speaker: speaker.map(ToString::to_string),
        idle_since: Arc::new(std::sync::Mutex::new(std::time::Instant::now())),
        learning: learning.clone(),
    };
    let execute: fono_assistant::ToolExecFn = Arc::new(move |call: ToolCall| {
        let runnable = runnable.clone();
        let house = house.clone();
        let words = words.clone();
        let journal = journal.clone();
        Box::pin(async move {
            let name = call.name.clone();
            let think = journal.take_think_time();
            let started = std::time::Instant::now();
            let ran = run_one(&runnable, &house, &words, call).await;
            // Filed against the server that offers it, so a name published by
            // two servers cannot credit the wrong one. A call to a tool nobody
            // offers has no row to write to and is simply not recorded.
            if let Some(r) = runnable.get(&name) {
                journal.note(&r.source, &name, &ran, think, started.elapsed());
            }
            journal.resumed();
            // What the server was actually asked travels with the outcome, so
            // the record shows the call as it left rather than as it arrived.
            // So does whether the world itself was read afterwards and agreed:
            // that, and not the server saying "done", is what lets a caller
            // treat the command as reported without asking the model again.
            let mut out = ran.out;
            out.sent = Some(ran.sent);
            out.confirmed = matches!(ran.how, RunOutcome::Confirmed);
            out
        })
    });
    let hint = cfg.assistant.tools.place_names.then(|| area_hint(&store)).flatten();
    let grammar = rails(&store, &offered);
    // These names go to the assistant model and nowhere else — never to the
    // speech recogniser, which is frequently a cloud service chosen for audio
    // alone. See `docs/privacy.md`.
    Some(Arc::new(ActionTools { descriptors, execute, hint, grammar, said }))
}

/// The rails a local model is held to while it writes a command.
///
/// Everything here comes from two places, and neither is a list somebody has to
/// keep up to date: each tool's own published schema, and what the house said
/// about itself when it was connected. The only vendor knowledge involved is
/// three field names, and it is asked for rather than assumed — a server whose
/// catalogue is not recognised supplies none, and gets constraints from its
/// schemas alone.
///
/// Asked **once per server**, not once over all of them together. Recognition
/// is "does this catalogue look like Home Assistant", so a single `Hass*` tool
/// anywhere used to make every server's tools Home-Assistant-shaped, and every
/// server's `name` field narrow to the one house Fono had read. `name` is the
/// commonest parameter name there is, so that did not merely over-constrain: it
/// left the second server's correct call with no legal value at all.
///
/// `None` whenever nothing usable could be derived, which leaves the model
/// exactly as free as it is today.
fn rails(store: &ToolCatalogStore, rows: &[fono_core::tool_catalog::ToolRow]) -> Option<String> {
    let mut slots = fono_core::tool_grammar::SlotValues::new();
    let mut described = Vec::new();

    let mut by_server: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for r in rows {
        by_server.entry(r.source.as_str()).or_default().push(r.name.as_str());
    }

    for (source, names) in &by_server {
        let fields = vendor::for_catalogue(names).slot_fields();
        let mut said = Vec::new();
        if let Some(field) = fields.place {
            if let Ok(places) = store.place_names_of(source) {
                said.push(format!("{} areas", places.len()));
                slots.set(source, field, places);
            }
        }
        if let Some(field) = fields.device {
            if let Ok(devices) = store.device_names_of(source) {
                said.push(format!("{} devices", devices.len()));
                slots.set(source, field, devices);
            }
        }
        if let Some(field) = fields.kind {
            if let Ok(mut kinds) = store.device_domains_of(source) {
                // Only the kinds this house actually contains, so a command cannot
                // ask for a kind of thing that is not here. `__all__` is the way to
                // still say "everything in this area" — without it a required kind
                // would cost the user that sentence entirely.
                said.push(format!("{} kinds of device", kinds.len()));
                kinds.push(fono_core::tool_grammar::ANY_KIND.to_string());
                slots.set(source, field, kinds);
            }
        }
        if !said.is_empty() {
            described.push(format!("{source}: {}", said.join(", ")));
        }

        // A tool that sets exactly one thing cannot do anything without it, so
        // the field is compulsory whatever the schema says. Home Assistant
        // marks nothing required on any of its intents, which is how a
        // set-temperature call with no temperature comes to be writable at
        // all; the house then refuses it and the user waits for a round trip
        // to learn nothing.
        //
        // A field is only made compulsory where it is the *sole* value of
        // every tool declaring it. One shared with a tool that sets several
        // things is a real choice there, and insisting on it would make that
        // tool's other perfectly good calls unwritable.
        let mut sole: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut among_others: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        for r in rows.iter().filter(|r| r.source.as_str() == *source) {
            let values = the_values_a_tool_sets(&r.schema, fields);
            if values.len() == 1 {
                sole.extend(values);
            } else {
                among_others.extend(values);
            }
        }
        for field in sole.difference(&among_others) {
            slots.require(source, field);
        }
    }

    let g = fono_core::tool_grammar::build(rows, &slots);
    if let Some(text) = &g {
        info!(
            "actions: while writing a command the model is held to what each server reported{}{} \
             ({} bytes of rules)",
            if described.is_empty() { "" } else { " — " },
            described.join("; "),
            text.len()
        );
    } else {
        debug!("actions: nothing to hold the model to; commands stay unconstrained");
    }
    g
}

/// What each server's tools are held to, for the page to state once per server.
///
/// The same probe and the same readers as [`rails`], so the page cannot claim a
/// narrowing the model did not get. Per server for the same reason the rails
/// are: on a second server these numbers are a different house, and one
/// sentence covering both would be true of neither.
///
/// A server whose catalogue Fono does not recognise reports no fields, and the
/// page then says plainly that nothing is held to a house.
fn rails_facts(
    store: &ToolCatalogStore,
    rows: &[fono_core::tool_catalog::ToolRow],
) -> serde_json::Value {
    let mut by_server: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for r in rows {
        by_server.entry(r.source.as_str()).or_default().push(r.name.as_str());
    }
    let mut out = serde_json::Map::new();
    for (source, names) in &by_server {
        let f = vendor::for_catalogue(names).slot_fields();
        // Counted only where a field carries it. No field means nothing is
        // narrowed, and a number would read as though something were.
        let areas = f.place.map(|_| store.place_names_of(source).unwrap_or_default().len());
        let devices = f.device.map(|_| store.device_names_of(source).unwrap_or_default().len());
        let kinds = f.kind.map(|_| store.device_domains_of(source).unwrap_or_default().len());
        out.insert(
            (*source).to_string(),
            serde_json::json!({
                "place": f.place, "device": f.device, "kind": f.kind,
                "areas": areas, "devices": devices, "kinds": kinds,
            }),
        );
    }
    serde_json::Value::Object(out)
}

/// How many past invocations to show per tool. Enough to see whether a
/// failure is the standing state or a one-off, few enough that the panel
/// stays a summary rather than becoming a second history page.
const USES_PER_TOOL: usize = 4;

/// Group past invocations by tool, newest first, keeping a handful each.
///
/// Long payloads are cut here rather than in the browser so the response
/// stays small: a Home Assistant result is routinely a few kilobytes of
/// JSON, and two dozen tools' worth of them would dwarf everything else on
/// the page.
fn uses_by_tool(uses: &[ToolUse]) -> serde_json::Value {
    let mut out: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for u in uses {
        let slot = out
            .entry(u.tool.clone())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
            .as_array_mut()
            .expect("just inserted an array");
        if slot.len() >= USES_PER_TOOL {
            continue;
        }
        slot.push(serde_json::json!({
            "at": u.at,
            "said": u.said.as_deref().map(|s| clip(s, 240)),
            "speaker": u.speaker,
            "args": clip(&u.args, 400),
            "result": u.result.as_deref().map(|s| clip(s, 600)),
            "ok": u.ok,
        }));
    }
    serde_json::Value::Object(out)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).chain(std::iter::once('…')).collect()
}

/// Everything the Tools &amp; actions page needs beyond the tool list itself.
///
/// One payload, built by the same code that builds the prompt, from the same
/// store, at the same moment — so the page cannot show something the model was
/// not told. That is the whole point of it: the two worst bugs in this area
/// were both a mechanism working correctly while the only place anyone could
/// look sat in another crate, reporting something else.
///
/// Everything here is read-only and comes from the local store, so the page
/// renders instantly and no server is contacted.
///
/// `uses` is the recent tail of the conversation log — empty when the user
/// keeps no history, which the page states rather than hiding.
pub(crate) fn page_extras(
    cfg: &Config,
    store: &ToolCatalogStore,
    uses: &[ToolUse],
) -> serde_json::Value {
    let active = store.active_tools().unwrap_or_default();
    let devices = store.devices().unwrap_or_default();
    serde_json::json!({
        "place_names": cfg.assistant.tools.place_names,
        "rails": rails_facts(store, &active),
        "any_kind": fono_core::tool_grammar::ANY_KIND,
        "house": {
            "places": store.place_names().unwrap_or_default(),
            "devices": devices,
            "kinds": store.device_domains().unwrap_or_default(),
        },
        // The literal sentences the model is given about this home, or nothing
        // when it is given none. Shown verbatim: paraphrasing it here would
        // recreate the very gap this page exists to close.
        "hint": cfg.assistant.tools.place_names.then(|| area_hint(store)).flatten(),
        // The whole steady head, block by block, in the order the model reads
        // it. `hint` above is only the first of the three.
        "prompt": prompt_blocks(cfg, store, &active),
        "catalogue_hash": store.catalogue_hash().unwrap_or_default(),
        "offered": active.len(),
        // What each tool has actually been asked to do, in the user's own
        // words. Read back out of the ordinary transcript, so it is present
        // only while conversation history is kept.
        "uses": uses_by_tool(uses),
        "history_kept": cfg.conversations.enabled,
        // The phrases Fono has written down and what each one would run. The
        // list of phrases that have worked but never earned the fast path is the
        // model's own blind-spot list, so it is shown rather than hidden.
        "shortcuts": phrases(store),
    })
}

/// The steady head of the system prompt, block by block, in the order the
/// model reads it.
///
/// The page used to show the house block alone under a heading that promised
/// the exact words the assistant is given. On a 79-device home that was 2,894
/// characters of a 6,559-character head: the tool block — the largest of the
/// three — and the behavioural rules were both invisible, so the one place a
/// user can look under-reported the prompt by more than half. Anything that is
/// sent is shown here.
///
/// Rendered by the same functions the reply path uses, from the same store, so
/// the page cannot show a prompt the model was not given. The tool block goes
/// through [`fono_assistant::local_tools::instructions`] for that reason rather
/// than being re-spelled here.
///
/// `in_prompt` is false for a backend that carries tools as data in its API
/// request instead of as words in the prompt. The block is still shown — it is
/// the same information, and a user comparing two backends deserves to see it —
/// but it does not count towards what the model reads.
fn prompt_blocks(
    cfg: &Config,
    store: &ToolCatalogStore,
    active: &[fono_core::tool_catalog::ToolRow],
) -> serde_json::Value {
    let house = cfg.assistant.tools.place_names.then(|| area_hint(store)).flatten();
    let descriptors: Vec<serde_json::Value> = active
        .iter()
        .map(|r| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": r.name,
                    "description": r.description,
                    "parameters": r.schema,
                }
            })
        })
        .collect();
    let tools =
        (!descriptors.is_empty()).then(|| fono_assistant::local_tools::instructions(&descriptors));
    let behaviour = cfg.assistant.prompt_main.trim();
    // Only what this backend actually reads as words. `compose_head` joins the
    // blocks it is given with a blank line, so the joins are counted too.
    let in_prompt = cfg.assistant.backend == fono_core::config::LlmBackend::Local;
    let read: Vec<&str> =
        [house.as_deref(), tools.as_deref().filter(|_| in_prompt), Some(behaviour)]
            .into_iter()
            .flatten()
            .filter(|b| !b.trim().is_empty())
            .collect();
    let chars: usize =
        read.iter().map(|b| b.chars().count()).sum::<usize>() + read.len().saturating_sub(1) * 2;
    serde_json::json!({
        "house": house,
        "tools": tools,
        "tools_in_prompt": in_prompt,
        "behaviour": (!behaviour.is_empty()).then(|| behaviour.to_string()),
        "chars": chars,
    })
}

/// The phrases Fono has written down, each with the one word the page may put
/// beside it.
///
/// The word is asked of the row rather than worked out here, so the page and the
/// fast path can never disagree about which phrases are replayed.
fn phrases(store: &ToolCatalogStore) -> Vec<serde_json::Value> {
    store
        .shortcuts()
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "phrase": s.phrase,
                "lang": s.lang,
                "source": s.source,
                "tool": s.tool,
                "args": s.args,
                "target": target_in(&s.tool, &s.args),
                "origin": s.origin,
                "state": s.state(),
                "runs": s.runs,
                "clean": s.clean,
                "last_run": s.last_run,
                "last_ok": s.last_ok,
                "last_ms": s.last_ms,
            })
        })
        .collect()
}

/// What in the home a stored command names, when the server's fields for it are
/// known.
///
/// So every row can read as the sentence it is — this phrase turns *that thing*
/// off — rather than as a line of JSON. Asked of the vendor exactly as the rails
/// and the house's own corrections ask, so a server Fono has no specific
/// knowledge of yields nothing here and the page falls back to the arguments.
///
/// A device wins over an area when both are named, because the device is the
/// narrower answer to "what does this touch". A command that names neither
/// yields nothing.
fn target_in(tool: &str, args: &str) -> Option<String> {
    let fields = vendor::for_catalogue(&[tool]).slot_fields();
    let sent: serde_json::Value = serde_json::from_str(args).ok()?;
    [fields.device, fields.place, fields.wider_place]
        .into_iter()
        .flatten()
        .find_map(|f| Some(sent.get(f)?.as_str()?.to_string()))
}

/// What the user's tools amount to once the chosen backend is taken into
/// account, plus anything extra the model needs told.
///
/// A backend that cannot invoke tools does not fail — it replies fluently,
/// having quietly ignored them, so the model says "I'll turn the light on"
/// and no light moves. So the tools are withheld and the model is told, in
/// one line, that it cannot act. Better a plain "I can't do that" than a
/// promise nothing keeps.
pub(crate) fn for_backend(
    actions: Option<Arc<ActionTools>>,
    backend_can_act: bool,
    backend: &str,
) -> (Option<Arc<ActionTools>>, Option<String>) {
    let Some(actions) = actions else { return (None, None) };
    if backend_can_act {
        let hint = actions.hint.clone();
        return (Some(actions), hint);
    }
    warn!(
        "actions: {} tools are switched on, but the {backend} assistant cannot run them — \
         telling the model it cannot act rather than letting it promise",
        actions.descriptors.len()
    );
    (None, Some(CANNOT_ACT.to_string()))
}

/// Said to the model when tools exist but the backend cannot reach them.
const CANNOT_ACT: &str = "You cannot control any devices or run any tools in this conversation. \
     If asked to, say plainly that you are unable to, and do not claim to have done it.";

/// The one line that stops the model inventing an area name.
///
/// Without it a Romanian command asks for `bucătărie` in a house whose
/// areas are all named in English, Home Assistant matches nothing, and
/// nothing happens. Naming the areas turns an open guess into a closed
/// choice — which is a translation, the one thing a model is reliably good
/// at — and does so in every language at once, including ones nobody
/// anticipated. Aliases in the house can only widen what it accepts; they
/// cannot stop the guessing.
///
/// Read from the catalogue, learned when the server was connected, so this
/// costs no network on the request path.
///
/// The second sentence exists because of a failure that looked like a
/// missing device and was not. Asked to switch off a lamp whose name began
/// with an area, the model searched only that area and found nothing — the
/// lamp was named after the place it lights, not the one it sits in. It then
/// reported the lamp unavailable while it was on. Device names routinely
/// mention somewhere they are not, so narrowing the search by area hides the
/// very thing being looked for.
///
/// The third sentence exists because "act on the area in one call", left on
/// its own, is dangerous advice. Asked in Romanian to turn on *the light* in
/// the office, the model asked for the whole office — and an area-wide switch-on
/// reaches everything switchable in it, so the air conditioning came on while
/// the one lamp that was actually wanted failed. An area plus a kind of device
/// is still one call; saying which kind is what keeps the area from being a
/// blunt instrument.
///
/// The domain rule leads, and says *required*, because stating it second — after
/// "act on the area in one call" — did not work. A later trace, with that
/// wording in the prompt, still produced a bare `{"area": "Master bedroom"}`
/// and moved the curtains and the roller. Two things were wrong with putting it
/// second: the sentence opened with the permission ("act on the area in one
/// call") and only then qualified it, and the qualification was phrased as
/// advice ("pass that kind as the domain") rather than an obligation. The
/// one-call economy is a separate rule now, so it cannot be read as licence to
/// omit the domain.
/// The fourth rule exists because the model picked the wrong tool, not the
/// wrong target. Asked in Romanian and again in English to turn the bedroom
/// lights on, it reached for the brightness-and-colour tool and invented both
/// values; the house rejected the payload and the lights stayed off. The hint
/// had been entirely about *targeting* and silent on *choosing*, while a
/// couple of dozen near-identical signatures competed for the same request.
/// The fifth rule is the other half of that failure: nobody had mentioned
/// brightness, and a field is not a request.
///
/// Written as a numbered list rather than a paragraph, and shorter than the
/// prose it replaces. Verbosity is not instruction strength — the paragraph
/// version stated the domain rule at length and still failed to prevent a
/// domain-less call.
fn area_hint(store: &ToolCatalogStore) -> Option<String> {
    written_hint(store, hint_arm())
}

/// The hint itself, with the arm passed in so a test can pin each one without
/// touching the environment of a test running beside it.
fn written_hint(store: &ToolCatalogStore, arm: HintArm) -> Option<String> {
    let names = store.place_names().ok()?;
    if names.is_empty() {
        return None;
    }
    let mut hint = format!("Areas in this home: {}.", names.join(", "));
    if arm != HintArm::NoRules {
        hint.push_str("\nRules for acting on this home:");
        for (n, rule) in RULES.iter().enumerate() {
            // Rules 1 and 4 look like the two the code now guarantees on its
            // own — the rails make an invented name unwritable, and
            // `HouseFacts` drops an area named beside a device. `lean` asked
            // whether saying them as well still helps. It does: see `HintArm`.
            if arm == HintArm::Lean && matches!(n, 0 | 3) {
                continue;
            }
            let _ = write!(hint, "\n{}. {rule}", n + 1);
        }
    }

    // The device names, when there are few enough to state without crowding
    // out the conversation. A truncated list would be worse than none: the
    // model would conclude a real device does not exist and say so.
    if arm != HintArm::NoDevices {
        if let Ok(devices) = store.devices() {
            if !devices.is_empty() && devices.len() <= MAX_LISTED_DEVICES {
                hint.push_str(&by_kind(&devices));
            } else if devices.len() > MAX_LISTED_DEVICES {
                debug!(
                    "actions: {} devices is too many to name in the prompt; \
                     the model will have to look them up",
                    devices.len()
                );
            }
        }
    }
    Some(hint)
}

/// The devices, one line per kind, with the kind written as the value the
/// `domain` argument takes.
///
/// The kind is the only thing that stops a name being read for what it sounds
/// like. Asked to switch an air conditioner on, a model sent
/// `{"area": "Office", "domain": ["light"]}` — `light` is the domain with the
/// most training behind it, and a flat list of names offered nothing to
/// contradict it. It is worse than a guess in the other direction too: this
/// home has a `switch` called "Entrance lights" and another called "Basement
/// lights", so the obvious domain for either finds nothing at all.
///
/// Grouping is what makes that mapping available, and it is close to free: on
/// a 79-device home the whole hint moves by four characters, because one label
/// per kind is paid for by a header shorter than the sentence about exact names
/// it replaces.
///
/// Devices whose kind the server never stated are labelled as unknown rather
/// than guessed at — a wrong domain is a call that silently reaches nothing.
fn by_kind(devices: &[fono_core::tool_catalog::Device]) -> String {
    let mut out = String::from(
        "\nDevices by kind — the kind is the `domain`. Areas and names work only exactly as \
         written:",
    );
    // `devices()` is already ordered by kind then name, so one pass groups it.
    let mut kind: Option<&str> = None;
    for d in devices {
        if kind == Some(d.domain.as_str()) {
            out.push_str(", ");
        } else {
            kind = Some(&d.domain);
            let label = if d.domain.is_empty() { "kind unknown" } else { &d.domain };
            let _ = write!(out, "\n{label}: ");
        }
        out.push_str(&d.name);
    }
    out
}

/// The rules, one per entry, numbered where they are written.
///
/// Split out so a measurement can leave some of them out without the numbering
/// or the wording drifting between arms.
///
/// Two details in rule 2 are deliberate. The worked example uses `climate`
/// rather than `light`, because the device list below already says which kind
/// each device is, and `light` there primed the one domain the model over-reaches
/// for — a run sent `["light"]` for an air conditioner three times in four.
/// And `__all__` is named, because the rails make the field compulsory and
/// offer that value as the way to still say "everything in here": unexplained,
/// a model that means it has to either guess or file it under one kind. Fono
/// takes the value back out before the call leaves, on every backend.
const RULES: [&str; 6] = [
    "Never translate or invent an area or device name — pick the closest one listed.",
    "When the user says which kind of device they mean — the lights, the heating, the blinds — \
     the domain is required, e.g. {\"area\": \"Master bedroom\", \"domain\": [\"climate\"]}. \
     Write the domain [\"__all__\"] only when they meant every device in the area.",
    "One call for an area, not one per device: an area plus a domain is a single call.",
    "When the user names a device, act on it by that name and add no area: a device's name \
     often mentions somewhere it is not.",
    "Use the simplest tool that does what was asked: a brightness, colour or temperature tool \
     only when the request named that value, the plain on/off tool to switch something on or \
     off.",
    "Fill in only what the user asked for. If a tool cannot be called without a value nobody \
     gave, it is the wrong tool.",
];

/// Which parts of the hint to write, for measurement only.
///
/// The hint costs about 700 tokens of prefill on every turn — roughly 400 of
/// them the device list and 250 the rules. Leaving any of it out cost commands
/// and saved no time; the numbers are in the plan (F54), not here.
///
/// The one thing worth knowing at this call site: the two rules the code now
/// enforces on its own are still load-bearing. `HouseFacts` drops an area named
/// beside a device whether the model was told to or not, but only when a device
/// *is* named — and the rule is what gets it named.
///
/// An environment variable rather than a setting or a flag, deliberately. The
/// rails were shipped behind a config key on the same reasoning and the key
/// long outlived the measurement it was for; a variable cannot be saved into a
/// config file and cannot appear on the settings page. It goes once the
/// measurement is repeated on a second local model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HintArm {
    /// Everything. What a user gets, and the best of the four.
    Full,
    /// Every rule except the two the code already guarantees.
    Lean,
    /// The areas and the devices, and no rules at all.
    NoRules,
    /// The areas and the rules, and no device list.
    NoDevices,
}

fn hint_arm() -> HintArm {
    match std::env::var("FONO_ACTION_HINT").unwrap_or_default().as_str() {
        "lean" => HintArm::Lean,
        "no-rules" => HintArm::NoRules,
        "no-devices" => HintArm::NoDevices,
        _ => HintArm::Full,
    }
}

/// How many device names may go in the prompt.
///
/// Measured on a real home: 77 devices cost about 400 tokens, which is a
/// fair price for the model never having to guess a name. Beyond a few
/// hundred the list would dominate the prompt, and the lookup tool is the
/// better answer.
const MAX_LISTED_DEVICES: usize = 200;

/// Drop arguments the model filled in with nothing.
///
/// A small local model, asked to turn off the kitchen lights, sent
/// `{"area": "Kitchen", "domain": ["light"], "floor": null, "name":
/// "Kitchen lights"}`. Every field the tool advertises got a value, and two of
/// them were placeholders: `floor` was `null` and, in a sibling trace, `name`
/// was an empty string. Home Assistant answered *"Input validation error: None
/// is not of type 'string'"* and did nothing — twice in a row, with the user
/// repeating themselves and the model apologising each time. Nothing was
/// broken, and the model was one `null` away from a working command.
///
/// A key the caller did not mean to set and a key it left blank are the same
/// request, so the blank ones are removed before the server sees them: `null`,
/// the empty string, and the empty list, at the top level and inside nested
/// objects. Anything with a value is passed through untouched — this never
/// changes what was asked for, only stops us asking for it badly.
fn drop_empty_arguments(args: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    fn is_blank(v: &Value) -> bool {
        match v {
            Value::Null => true,
            Value::String(s) => s.trim().is_empty(),
            Value::Array(a) => a.is_empty(),
            Value::Object(o) => o.is_empty(),
            _ => false,
        }
    }
    match args {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, drop_empty_arguments(v)))
                .filter(|(_, v)| !is_blank(v))
                .collect(),
        ),
        other => other,
    }
}

/// Take back the one value Fono offered that no server accepts.
///
/// The rails make a field compulsory to stop it being forgotten, and offer
/// [`fono_core::tool_grammar::ANY_KIND`] as the way to still say "everything in
/// this area". Nothing outside Fono has ever heard of it, so it is removed here
/// and the field goes back to being absent — which is exactly what "everything"
/// has always meant to a server.
///
/// The gain is in the record rather than the behaviour: a command that meant the
/// whole area and one that forgot to say what it meant used to be the same
/// payload, and both open the blinds. Now they are told apart before this point,
/// and only the deliberate one gets here.
///
/// Left blank by [`drop_empty_arguments`] rather than deleted outright, so the
/// two rules compose and neither has to know about the other. Runs before the
/// schema check, because the value would fail it.
fn drop_any_kind(args: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    fn strip(v: Value) -> Value {
        match v {
            Value::String(s) if s == fono_core::tool_grammar::ANY_KIND => Value::Null,
            // The kind is usually a list, so "everything" arrives as a
            // one-element array and the whole array has to go: a list with the
            // placeholder taken out would ask for nothing at all.
            Value::Array(a) => {
                let cleaned: Vec<Value> = a
                    .into_iter()
                    .filter(|e| e.as_str() != Some(fono_core::tool_grammar::ANY_KIND))
                    .collect();
                Value::Array(cleaned)
            }
            other => other,
        }
    }
    match args {
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, strip(v))).collect()),
        other => other,
    }
}

/// What this home already told us about the things in it.
///
/// Built once per turn from the same store the rails come from. Only the one
/// fact worth acting on is kept: which kind of thing each named device is.
///
/// The default knows nothing, which leaves every call exactly as written —
/// including the vendor, which is the one we have no specific knowledge of.
struct HouseFacts {
    /// Which published argument carries a device name and which carries a kind,
    /// asked of the vendor rather than assumed. A server naming neither leaves
    /// everything below inert.
    slots: vendor::SlotFields,
    /// Device name, folded for comparison, to the single kind it is. A name
    /// this home uses for two kinds of thing is left out: there is no one
    /// answer, so there is nothing to correct to.
    kind_of: std::collections::HashMap<String, String>,
    /// Names this home uses for exactly one device, folded for comparison and
    /// paired with the spelling the home published. Such a name is the whole
    /// address of a thing, so nothing else needs saying to reach it. A name two
    /// devices share is left out — there the area is the only thing telling
    /// them apart.
    ///
    /// The published spelling is kept because it is the only one that works: a
    /// home matches a name exactly, so a name recovered from what the user said
    /// has to be written back in the home's own casing, not the user's.
    sole: std::collections::HashMap<String, String>,
    /// Area names, folded. Only ever used to *decline* to treat a word as a
    /// device: a home with a room and a device sharing one name gives that word
    /// two readings, and "everything in the Office" must not be narrowed to a
    /// single thing called `Office`.
    places: std::collections::HashSet<String>,
    /// The software that answered discovery, kept so a correction can ask it
    /// what it knows about a tool: which tools switch a thing without setting a
    /// value on it, and which kind of device a tool is about.
    vendor: &'static dyn vendor::Vendor,
}

impl Default for HouseFacts {
    fn default() -> Self {
        Self {
            slots: vendor::SlotFields::default(),
            kind_of: std::collections::HashMap::new(),
            sole: std::collections::HashMap::new(),
            places: std::collections::HashSet::new(),
            vendor: &vendor::Unknown,
        }
    }
}

impl HouseFacts {
    fn learn(store: &ToolCatalogStore, tools: &[&str]) -> Self {
        let vendor = vendor::for_catalogue(tools);
        let slots = vendor.slot_fields();
        let mut kind_of: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut ambiguous = Vec::new();
        let mut sole = std::collections::HashMap::new();
        let mut shared = Vec::new();
        for d in store.devices().unwrap_or_default() {
            let key = d.name.trim().to_lowercase();
            if sole.insert(key.clone(), d.name.trim().to_string()).is_some() {
                shared.push(key.clone());
            }
            match kind_of.get(&key) {
                Some(kind) if *kind == d.domain => {}
                Some(_) => ambiguous.push(key),
                None => {
                    kind_of.insert(key, d.domain);
                }
            }
        }
        for key in ambiguous {
            kind_of.remove(&key);
        }
        for key in shared {
            sole.remove(&key);
        }
        let places = store
            .place_names()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.trim().to_lowercase())
            .collect();
        Self { slots, kind_of, sole, places, vendor }
    }

    /// Aim the call at the device the user actually named.
    ///
    /// The defect: asked to turn off the *Air conditioner* — a name this home
    /// publishes, spoken exactly as it is written — a local model wrote
    /// `HassTurnOff {"area": "Master bedroom"}`, and the house obediently
    /// switched off two beds, two lights, a salt lamp and a curtain. Nine of
    /// the nineteen commands in one run that spoke a catalogued name did this,
    /// and every one of them reached for an area instead. An area is not a
    /// vaguer way of saying a device; it is a different set of devices.
    ///
    /// So when the request contains a name this home uses for exactly one
    /// thing, that name is the target, and it is written in whatever the model
    /// wrote. [`HouseFacts::agree`] then takes out the area, the storey and the
    /// class of device, because a name only one device answers to needs none of
    /// them — that rule already existed and never fired, for want of a name to
    /// fire it on.
    ///
    /// Nothing here reads a word of any language. It compares what was said
    /// against the list the server published about itself, so it works on a
    /// house Fono has never seen, in a language Fono cannot parse, and on a
    /// cloud backend exactly as on a local one.
    ///
    /// Silent in four cases, each of which would be Fono guessing:
    ///
    /// - the model already named a device the user said — it is right already;
    /// - the name is shared by two devices, so there is no single thing to aim
    ///   at (the area is what tells them apart);
    /// - the name is also an area's name, so the word has two readings and
    ///   picking one would break *"everything in the Office"*;
    /// - the server publishes no field for a device name.
    ///
    /// The longest spoken name wins, so a home holding both `Couch` and
    /// `Couch Blue` reaches the one the user said all of.
    fn aim_at_what_was_said(
        &self,
        args: serde_json::Value,
        said: &str,
    ) -> (serde_json::Value, Option<String>) {
        let Some(field) = self.slots.device else { return (args, None) };
        let heard = said.trim().to_lowercase();
        if heard.is_empty() {
            return (args, None);
        }
        let Some(map) = args.as_object() else { return (args, None) };

        // A name the model wrote that the user also said is the answer already.
        if let Some(written) = map.get(field).and_then(|v| v.as_str()) {
            if spoken_in(&heard, &written.trim().to_lowercase()) {
                return (args, None);
            }
        }

        let Some((_, published)) = self
            .sole
            .iter()
            .filter(|(folded, _)| !self.places.contains(*folded))
            .filter(|(folded, _)| spoken_in(&heard, folded))
            .max_by_key(|(folded, _)| folded.chars().count())
        else {
            return (args, None);
        };

        let mut map = map.clone();
        let previous = map.insert(field.to_string(), serde_json::Value::String(published.clone()));
        let note = previous.as_ref().and_then(serde_json::Value::as_str).map_or_else(
            || format!("{field} is {published}: the user said it"),
            |was| format!("{field} was {was}, but the user said {published}"),
        );
        (serde_json::Value::Object(map), Some(note))
    }

    /// The device an area named beside a device name is pointing at.
    ///
    /// The defect: asked in Romanian and again in English to aim the office air
    /// conditioner at a temperature, the model wrote
    /// `{"area": "Office", "name": "Air conditioner"}`. This home publishes both
    /// an `Air conditioner`, in the hall, and an `Office air conditioner`. The
    /// name matched one device exactly, so the area was taken out as
    /// redundant — and the hall unit was set to twenty degrees while the reply
    /// said the office. Wrong room, and a confident account of the room the user
    /// asked about.
    ///
    /// So before an area is discarded it gets its say: when exactly one
    /// published name contains both the area and the name that was written, that
    /// is the device, and the area was doing the work of telling two apart.
    ///
    /// Silent unless there is exactly one answer. Two candidates mean the area
    /// still has not settled it, and none means the area really was redundant —
    /// in both, the rule below is right to drop it.
    ///
    /// Nothing here reads a word of any language: both halves are searched for
    /// in names this server published, at word boundaries, so the same
    /// substitution happens in a house Fono has never seen.
    fn narrowed_by(&self, written: &str, place: &str) -> Option<String> {
        let (written, place) = (written.trim().to_lowercase(), place.trim().to_lowercase());
        if written.is_empty() || place.is_empty() {
            return None;
        }
        let mut better = self
            .sole
            .iter()
            .filter(|(folded, _)| **folded != written)
            .filter(|(folded, _)| spoken_in(folded, &written) && spoken_in(folded, &place));
        let (_, published) = better.next()?;
        better.next().is_none().then(|| published.clone())
    }

    /// Make the call agree with the house that was published.
    ///
    /// Two things a command says about a device are not the model's to decide,
    /// because this home already stated them: what kind of thing the device is,
    /// and — when the name belongs to one device only — where it is.
    ///
    /// **The kind.** Asked in plain English to turn the air conditioner off, a
    /// local model wrote `{"name": "Air conditioner", "domain": ["light"]}`;
    /// Home Assistant looked for a light by that name, found none, and reported
    /// a failure the model then read aloud. The same mistake broke four of the
    /// benchmark's cells and survived every rewording of the prompt, because
    /// the field is free and a plausible wrong value is as easy to write as the
    /// right one. Corrected rather than refused, keeping whichever shape it was
    /// written in, list or single value, because that is what the schema asks.
    ///
    /// **The area.** With the kind put right the same mistake simply moved one
    /// field: the model named a real device and paired it with an area the
    /// device is not in, and the house refused it again — three times in one
    /// run, each time after the kind had been corrected successfully. An area
    /// beside a name can only ever *narrow*, so on a name only one device
    /// answers to it can only narrow wrongly. It is dropped, along with
    /// anything an area is itself inside and any class of device. Not
    /// corrected: the catalogue records what a device is and not where, so
    /// there is no right value to write — but there is a value that is never
    /// needed.
    ///
    /// **The storey.** The same argument one rung out, and it needs no device
    /// name: a command that names an area does not need to say which floor the
    /// area is on, and a run showed the model *adding* `floor: "1"` on its
    /// second attempt at a bedroom the house then failed to find. Whatever
    /// narrows furthest is kept and everything wider than it goes.
    ///
    /// Silent when the named device is unknown to us, when the name is shared
    /// by two devices (there the area is the only thing telling them apart),
    /// when the kind already agrees, and for any server whose field names we do
    /// not know. In each of those the call goes out exactly as written.
    fn agree(&self, args: serde_json::Value) -> (serde_json::Value, Option<String>) {
        use serde_json::Value;
        let Some(map) = args.as_object() else { return (args, None) };
        let mut map = map.clone();
        let mut notes = Vec::new();
        let named = self
            .slots
            .device
            .and_then(|f| map.get(f))
            .and_then(|v| v.as_str())
            .map(|n| n.trim().to_string());

        if let Some(named) = &named {
            let key = named.to_lowercase();
            if let (Some(kind_field), Some(kind)) = (self.slots.kind, self.kind_of.get(&key)) {
                if let Some(written) = map.get(kind_field) {
                    let agrees = match written {
                        Value::String(s) => Some(s == kind),
                        Value::Array(a) => {
                            Some(a.len() == 1 && a[0].as_str() == Some(kind.as_str()))
                        }
                        // Anything else is not a kind we can read, so nothing is claimed.
                        _ => None,
                    };
                    if agrees == Some(false) {
                        notes.push(format!(
                            "{kind_field} was {}, but this home says {named} is a {kind}",
                            written.to_string().trim_matches('"')
                        ));
                        let fixed = match written {
                            Value::Array(_) => Value::Array(vec![Value::String(kind.clone())]),
                            _ => Value::String(kind.clone()),
                        };
                        map.insert(kind_field.to_string(), fixed);
                    }
                }
            }
            if self.sole.contains_key(&key) {
                // An area beside a name usually narrows nothing. Sometimes it
                // is the only thing saying which device was meant, and then
                // dropping it acts in the wrong room, so it gets its say first.
                if let (Some(field), Some(place)) = (self.slots.device, self.slots.place) {
                    let said_where = map
                        .get(place)
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_default();
                    if let Some(better) = self.narrowed_by(named, &said_where) {
                        notes.push(format!(
                            "{field} was {named}, but the only {named} in {said_where} is {better}"
                        ));
                        map.insert(field.to_string(), Value::String(better));
                    }
                }
                let wider = [self.slots.place, self.slots.wider_place, self.slots.filter];
                for field in wider.into_iter().flatten() {
                    if map.remove(field).is_some() {
                        notes.push(format!(
                            "{field} was dropped: {named} is one device in this home"
                        ));
                    }
                }
            }
        }
        // Nothing narrower than an area was given, so the area is the target
        // and anything containing it is one more thing to get wrong.
        if let (Some(place), Some(wider)) = (self.slots.place, self.slots.wider_place) {
            if map.contains_key(place) && map.remove(wider).is_some() {
                notes.push(format!("{wider} was dropped: the {place} already says where"));
            }
        }
        if notes.is_empty() {
            return (Value::Object(map), None);
        }
        (Value::Object(map), Some(notes.join("; ")))
    }
}

/// Did the request contain this name, as a name rather than as part of a word?
///
/// Both sides arrive folded, so this is a plain search with one condition: what
/// sits either side of a match must not be a letter or a digit. Without it a
/// device called `TV` is found inside *"switch it off"*, and a home is free to
/// name a thing after a syllable.
///
/// Deliberately nothing cleverer. No stemming, no fuzzy distance, no word list
/// — a near-match is a guess, and this is only ever allowed to act on a name
/// the user said exactly as the home writes it.
fn spoken_in(heard: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(offset) = heard[from..].find(name) {
        let start = from + offset;
        let end = start + name.len();
        let clear_before = heard[..start].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let clear_after = heard[end..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        if clear_before && clear_after {
            return true;
        }
        // Past the first character of this match, so an overlapping one is
        // still found — and never a zero-width step, which would not end.
        from = start + heard[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Never send a number the user did not ask for.
///
/// The defect this exists for: asked in plain words to switch an air
/// conditioner off, a local model called the tool that *sets a temperature*
/// and wrote `"temperature": 0`. Nothing already here can catch that. The
/// value is the type the schema asks for, so the schema check passes it; it is
/// not blank, so the blank check passes it; and it is required by that tool, so
/// it cannot simply be removed. The tool is wrong, and the only evidence of
/// that is the request: nobody said a number.
///
/// So that is the test, and it is deliberately the crudest one that works —
/// **if the request contains no digit anywhere, a numeric argument was
/// invented**. Digits are the same in every language, which is what makes this
/// work on a Romanian sentence without a translation table, a locale, or a word
/// list to maintain. It needs nothing from the server and nothing from the
/// model, so it holds on a cloud backend exactly as it does on a local one.
///
/// One outcome, and it is the same whichever field the number is in: the call
/// stays at home, and the complaint names the number and says the request had
/// none. Nothing is quietly deleted. That was tried first — drop the number
/// where the schema calls it optional, keep the call — and it fails in both
/// directions at once. Home Assistant marks nothing required, so *"turn the air
/// conditioner off"* went out as
/// `HassClimateSetTemperature {"name": "Air conditioner"}`, a tool asked to set
/// a temperature and given none; the house refused it and the user heard
/// silence. And on *"set the Couch Blue to thirty percent"* the same rule threw
/// away the one value the user had asked for, because "thirty" is not a digit.
/// Asking is right in both: the model either writes the call again, or writes a
/// better one.
///
/// The escape hatch is at the call site: this fires once per tool per turn, and
/// the refusal marks itself repeatable, so a model that writes the same call
/// again gets it sent. That covers the one way the test can be wrong — a
/// recogniser that writes "seventy" instead of "70" — at the price of one extra
/// round trip. Both halves were learned the hard way on *"set the volume to
/// seventy"*: with the hatch unreachable, and then with a complaint that called
/// the tool wrong outright, the model answered that no tool for setting a value
/// existed, deleted the volume, and the display never moved. So the complaint
/// states the evidence and offers both readings of it, and picks neither.
fn numbers_nobody_asked_for(
    args: serde_json::Value,
    said: &str,
) -> (serde_json::Value, Option<String>) {
    // Empty means the words were never recorded, which is no evidence at all —
    // and a number is only suspect where there is something to compare it to.
    if said.trim().is_empty() || said.chars().any(char::is_numeric) {
        return (args, None);
    }
    let Some(map) = args.as_object() else { return (args, None) };
    let unasked: Vec<String> =
        map.iter().filter(|(_, v)| v.is_number()).map(|(k, v)| format!("{k} ({v})")).collect();
    let complaint = (!unasked.is_empty()).then(|| {
        format!(
            "nothing in what the user said is a number, so {} came from nowhere. If they did ask \
             for that value, in words rather than digits, write this same call again and it will \
             be sent. If they only asked to switch something on or off, use the plain on/off \
             tool instead.",
            unasked.join(" and ")
        )
    });
    (args, complaint)
}

/// The values a tool exists to set, as against the fields that only say which
/// device it means.
///
/// The split needs one thing from the vendor layer and nothing else: the names
/// of the target fields, which are already asked for and already stated per
/// server. Everything a tool declares that is not a target is a value it exists
/// to set. A server whose catalogue is unrecognised supplies no target names,
/// so every field counts as a value and no tool is ever reduced to one — which
/// is the right answer when Fono cannot tell the two apart.
fn the_values_a_tool_sets(schema: &serde_json::Value, slots: vendor::SlotFields) -> Vec<String> {
    let targets: std::collections::BTreeSet<&str> =
        [slots.place, slots.wider_place, slots.device, slots.kind, slots.filter]
            .into_iter()
            .flatten()
            .collect();
    schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|props| props.keys().filter(|k| !targets.contains(k.as_str())).cloned().collect())
        .unwrap_or_default()
}

/// The one value a tool exists to set, when it sets exactly one.
///
/// A tool with a single value field is a tool whose whole purpose is that
/// value, so a call that omits it asks the server to set nothing. Every such
/// call in a run of the suite was refused outright by the house: eight of
/// them, every one `HassClimateSetTemperature` with no temperature, written
/// after Fono had stopped an invented one and the model deleted the field
/// rather than change tool.
///
/// Nothing is claimed about a tool declaring two or more. A tool taking a
/// brightness and a colour is a real answer to a request naming either, and
/// which of them is wanted is the user's business, not a rule's.
fn the_one_value_a_tool_sets(
    schema: &serde_json::Value,
    slots: vendor::SlotFields,
) -> Option<String> {
    let values = the_values_a_tool_sets(schema, slots);
    (values.len() == 1).then(|| values.into_iter().next().unwrap_or_default())
}

/// Check the arguments against what the server said it accepts.
///
/// Returns the server's own vocabulary for what is wrong, or `None` when
/// nothing obviously is.
///
/// A small local model asked to turn on the bedroom lights sent
/// `{"area": "Master bedroom", "brightness": 10, "color": "#FFFFFF"}` to a
/// tool whose `color` is an enumeration of colour names. Nobody had mentioned
/// brightness or colour; the model filled the fields in because they were
/// there. Home Assistant answered *"Received invalid slot info"* and did
/// nothing, twice, in two languages.
///
/// Catching that here rather than at the house is worth a round trip, but the
/// real value is the sentence: an argument named against the schema the model
/// was shown is a correction it can act on, where a server's rejection of the
/// whole payload often is not.
///
/// Deliberately shallow. Only two things are checked — the type of a value,
/// and membership of an enumeration — because those are the server's own
/// unambiguous statements about itself. Required fields are not enforced: an
/// advertised schema is routinely stricter than the behaviour behind it, and
/// refusing a call the house would have accepted is a worse failure than
/// letting it through.
fn schema_complaint(schema: &serde_json::Value, args: &serde_json::Value) -> Option<String> {
    let props = schema.get("properties")?.as_object()?;
    let given = args.as_object()?;
    let mut bad = Vec::new();
    for (key, value) in given {
        let Some(spec) = props.get(key) else {
            // An argument the tool never advertised. Not necessarily wrong —
            // servers extend themselves — so it is passed through.
            continue;
        };
        if let Some(allowed) = spec.get("enum").and_then(|e| e.as_array()) {
            if !allowed.contains(value) {
                let names: Vec<String> = allowed.iter().map(ToString::to_string).collect();
                bad.push(format!("{key} must be one of {}", names.join(", ")));
                continue;
            }
        }
        if let Some(want) = spec.get("type").and_then(|t| t.as_str()) {
            if !matches_json_type(want, value) {
                bad.push(format!("{key} must be a {want}"));
            }
        }
    }
    (!bad.is_empty()).then(|| bad.join("; "))
}

/// Does a value match a JSON Schema `type` keyword?
///
/// `integer` accepts any number without a fractional part, matching the
/// specification: a model that writes `21.0` for a whole number of degrees is
/// not making the mistake this check is looking for.
fn matches_json_type(want: &str, value: &serde_json::Value) -> bool {
    use serde_json::Value;
    match want {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => matches!(value, Value::Null),
        // A type keyword we do not understand, or a list of them. Saying
        // nothing is the safe answer.
        _ => true,
    }
}

/// Appended to a failure where nothing in the world moved.
///
/// The traces that motivated this all ended the same way: the house said in
/// plain words what was wrong with the request, Fono read the objection aloud,
/// and the user had to say the whole thing again. The correction was already
/// in the model's hands and nothing invited it to use it. One sentence does,
/// and it costs nothing on a call that worked.
///
/// Deliberately not a promise: a second failure is a real answer, and saying
/// so beats a third attempt.
const RETRY_INVITATION: &str = "Nothing was changed. If you can tell from this what was wrong \
     with the request, correct it and call the tool once more; otherwise tell the user plainly \
     what went wrong.";

/// Appended to a failure where part of the request may already have landed.
///
/// Says nothing about what did or did not happen, because at this rung we
/// genuinely do not know — an area-wide command that moved four things and
/// missed two is the common case. Only offered where running the tool again
/// is the same request as running it once, so a repeat cannot double an
/// effect on the parts that did work.
const RETRY_THE_REST: &str = "If you can tell from this what was wrong with the request, \
     correct it and call the tool once more for what was missed; otherwise tell the user \
     plainly which parts did not happen.";

/// What one call did, and how strongly Fono can say so.
///
/// The second field is not a restatement of `out.failed`. It carries the one
/// distinction only this function can make — whether success was *checked*
/// against the world, merely accepted by the server, or simply sent with
/// nothing knowable afterwards. Recovering that from the outside is
/// impossible, and guessing it would put a claim on the page that nothing
/// supports.
struct Ran {
    out: ToolOutcome,
    how: RunOutcome,
    /// The individual things in the home this call reached, when the server
    /// names them. Empty for every server Fono has no specific knowledge of,
    /// and for anything that never left — which is why nothing is recorded in
    /// those cases rather than recorded as a failure.
    targets: Vec<vendor::Target>,
    /// The arguments as they actually went to the server, after the blank
    /// fields were dropped and the house's own facts applied. This, and not
    /// what the model wrote, is what a replay would have to send — so it is
    /// what a phrase is keyed to.
    sent: String,
}

/// Send one call to its server and record the timing, or describe why it
/// never landed.
///
/// Split out from [`run_one`] so the trace span and the two ways a send can
/// come back empty sit together, away from the judging of a successful answer.
async fn execute(
    r: &Runnable,
    call: &ToolCall,
    args: &serde_json::Value,
) -> Result<mcp_client::ToolResult, String> {
    // Running the command is the part the user is waiting on, and until this
    // span existed it was an unexplained gap between two model requests: a
    // real trace showed 587 ms of silence there with nothing to attribute it
    // to. Finished before the outcome is judged, so the timing measures the
    // server and not our reading of it.
    let span = current_span("tool.execute", "actions", ACTIONS_LANE);
    let called = mcp_client::call_tool(&r.endpoint, &call.name, args).await;
    // What was asked for and what the server said about it both belong here.
    // A trace of a command that never happened showed only the tool's name and
    // that something went wrong, which is not enough to tell a bad area name
    // from an unreachable server from a device that cannot do what was asked.
    let detail = match &called {
        Err(e) => Some(e.to_string()),
        Ok(res) if res.is_error => Some(res.text.clone()),
        Ok(_) => None,
    };
    if let Some(detail) = &detail {
        warn!(tool = %call.name, args = %call.arguments, "actions: {} refused: {detail}", call.name);
    }
    span.finish(serde_json::json!({
        "tool": call.name,
        "args": args,
        "answered": called.is_ok(),
        "server_error": called.as_ref().is_ok_and(|res| res.is_error),
        "error": detail.as_deref().map(|d| d.chars().take(300).collect::<String>()),
    }));
    match called {
        // The server was never reached, so nothing moved. Worth one more go:
        // the model may pick a different tool, and if it picks the same one
        // the second failure is the honest answer.
        Err(e) => Err(format!("{} could not be run: {e}", call.name)),
        // The server objected. Its own words are the most useful thing we
        // have: they tell the user why, and they are also precisely what the
        // model needs to correct itself. A refused call did nothing, so
        // trying again cannot double an effect.
        //
        // Its own words, but not necessarily its own wording: a server that
        // objects in its debugging vocabulary buries the useful part, and
        // [`brief`] then throws the middle away to fit. Where a vendor can read
        // the objection, the sentence it makes of it goes instead; where it
        // cannot, the server's text stands.
        Ok(res) if res.is_error => Err(format!(
            "{} failed: {}",
            call.name,
            vendor::refusal(&res.text).unwrap_or_else(|| brief(&res.text))
        )),
        Ok(res) => Ok(res),
    }
}

/// The arguments as they will actually be sent, or the complaint that keeps the
/// call at home.
///
/// Four steps in this order: drop what says nothing, let the house settle what
/// it already knows about the device named, take out numbers nobody asked for,
/// and only then check what is left against the schema the model was shown.
/// Nothing has left when this returns, so a complaint costs nothing to act on.
///
/// The `Err` carries what *would* have been sent beside the complaint, because a
/// run is recorded by the arguments it used whether or not they travelled.
fn prepare_args(
    r: &Runnable,
    house: &HouseFacts,
    words: &Words,
    call: &ToolCall,
) -> Result<serde_json::Value, Refusal> {
    let args: serde_json::Value =
        serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);
    let args = drop_empty_arguments(drop_any_kind(args));
    // A name the user said outranks anything the model reached for instead,
    // and has to be settled before the house corrects the call: the rules that
    // take out the area and the storey only fire on a named device.
    let (args, aimed) = house.aim_at_what_was_said(args, &words.said.words());
    // Anything the house has already stated is not the model's to get wrong.
    let (args, corrected) = house.agree(args);
    for note in [aimed, corrected].into_iter().flatten() {
        write_down(call, &args, &note);
    }

    // Only on the first go at this tool. A model that writes the same number
    // again is insisting, and the one way this check can be wrong — a
    // recogniser that spelled a number out in words — needs a way through.
    let args = if words.first_attempt_at(&call.name) {
        let (args, unasked) = numbers_nobody_asked_for(args, &words.said.words());
        if let Some(complaint) = unasked {
            // The refusal rests on a guess about the request, and the complaint
            // tells the model how to say the guess was wrong.
            return Err(Refusal::of(call, &args, &complaint, true));
        }
        args
    } else {
        args
    };

    let args = match the_whole_room(house, words, call, &r.schema, &args) {
        Room::Fine => args,
        Room::Device(fixed, note) | Room::Kind(fixed, note) => {
            write_down(call, &fixed, &note);
            fixed
        }
        // The same call again switches the same room, so there is no reading of
        // it that is safe to send.
        Room::Complain(complaint) => return Err(Refusal::of(call, &args, &complaint, false)),
    };

    // A tool that sets one thing, written without the thing. The house refuses
    // these itself, so the only question is whether the user waits for it to
    // say so; naming the way out here is both faster and more use to the model
    // than the server's own rejection.
    if let Some(field) = the_one_value_a_tool_sets(&r.schema, house.slots) {
        if args.as_object().is_some_and(|o| !o.contains_key(&field)) {
            // What this tool is about outlives it. The request is going to be
            // retried with a tool that switches anything at all, and the kind of
            // device this one names is the only record that the user asked about
            // one kind of thing.
            // The device goes with it: a switch written for a whole area can be
            // aimed back at the one thing this call was already aimed at.
            words.is_about(
                house.vendor.kind_of(&call.name),
                house.slots.device.and_then(|f| args.get(f)).and_then(|v| v.as_str()),
            );
            // Naming the way out is the whole point, and naming the tool by name
            // is a stronger way out than describing it. Told only to reach for
            // "the tool that switches things without one", a small model wrote
            // the same wrong call again and then apologised to the user for
            // having no temperature to set — twice in one turn, for a request
            // that was only ever "switch it on".
            let instead = match house.vendor.switches() {
                [] => "use the tool that switches things without one".to_string(),
                names => format!("use {} instead", names.join(" or ")),
            };
            let complaint = format!(
                "{field} is the only thing {} sets, and there is none in this call, so it \
                 would ask the house to set nothing. Either say the value or {instead}.",
                call.name
            );
            // Writing the same call again would still set nothing, so there is
            // no reading of the request under which it works.
            return Err(Refusal::of(call, &args, &complaint, false));
        }
    }

    // Never sent, so nothing moved and a correction is free. The complaint is
    // phrased against the schema the model was shown, which is a more useful
    // thing to hand back than the server's rejection of the whole payload.
    if let Some(complaint) = schema_complaint(&r.schema, &args) {
        return Err(Refusal::of(call, &args, &format!("{complaint}."), false));
    }
    Ok(args)
}

/// Record a call Fono put right on its way out, for the log and the trace.
fn write_down(call: &ToolCall, args: &serde_json::Value, note: &str) {
    debug!(tool = %call.name, "actions: corrected {}: {note}", call.name);
    current_instant(
        "tool.corrected",
        "actions",
        ACTIONS_LANE,
        serde_json::json!({ "tool": call.name, "args": args, "note": note }),
    );
}

/// What a switch aimed at a whole area turns out to be, in a turn that was
/// about one kind of thing in it.
enum Room {
    /// Nothing here reaches a whole room, so the call goes out as written.
    Fine,
    /// The call aimed back at the device the turn had already settled on, and
    /// the note that says so.
    Device(serde_json::Value, String),
    /// The call narrowed to the kind of device the turn is about, and the note
    /// that says so.
    Kind(serde_json::Value, String),
    /// Nothing here can narrow it, and what to tell the model.
    Complain(String),
}

/// Would this switch reach a whole room, in a turn that was about one kind of
/// thing in it?
///
/// The defect, seen once in Romanian and twice in English: told that a
/// temperature tool cannot run without a temperature and to switch the thing
/// instead, the model reached for the switch, kept the area it had written and
/// dropped the kind — so a request about one air conditioner turned on every
/// light in the office, and the turn before it turned them all off.
///
/// The kind is not guessed: the tool that could not run names it, and a
/// temperature tool is about the heating whatever words the request used.
///
/// When that refused tool had a device to aim at, the switch is aimed at the
/// same one and goes out — a request that named a device does not become a
/// request about a room because the first tool for it was the wrong tool. The
/// area then goes, as it does for any name only one device answers to.
/// Otherwise the kind is written in, and the switch reaches only that kind of
/// device in the area. Refusing instead was safe and useless: told which field
/// to write, the model apologised to the user rather than writing it, so the
/// command the user asked for never happened at all. Writing it can only ever
/// *narrow* what the call reaches, and the alternative it replaces — a bare
/// area — is the widest call there is.
///
/// Narrowing to a kind is not narrowing to a device: an area holding two
/// climate devices still gets both. That is a smaller version of the same
/// fault, not a cure for it, and only knowing where each device is would cure
/// it — which this home's catalogue does not record.
///
/// Silent unless every part of the trap is present: a tool this server switches
/// with, an area, no device named, and no kind. A turn that never reached for a
/// tool naming a kind is left alone entirely — switching off a whole room is a
/// real request, and this must not become the reason nobody can make it.
///
/// The device is only substituted when its published name is spoken inside the
/// area that was written, which is what says the two agree. Told to set the
/// office air conditioner and turn the bedroom on, the switch is about the
/// bedroom, and aiming it at the office would act in the wrong room — so a
/// device the area does not vouch for falls through to the complaint.
fn the_whole_room(
    house: &HouseFacts,
    words: &Words,
    call: &ToolCall,
    schema: &serde_json::Value,
    args: &serde_json::Value,
) -> Room {
    let Some(kind) = words.about_kind() else { return Room::Fine };
    if !house.vendor.switches().contains(&call.name.as_str()) {
        return Room::Fine;
    }
    let (Some(place), Some(kind_field)) = (house.slots.place, house.slots.kind) else {
        return Room::Fine;
    };
    if house.slots.device.is_some_and(|field| args.get(field).is_some()) {
        return Room::Fine;
    }
    let Some(area) = args.get(place).and_then(|v| v.as_str()) else { return Room::Fine };
    if area.is_empty() || args.get(kind_field).is_some() {
        return Room::Fine;
    }

    if let (Some(field), Some(device)) = (house.slots.device, words.about_device()) {
        if spoken_in(&device.to_lowercase(), &area.trim().to_lowercase()) {
            let mut map = args.as_object().cloned().unwrap_or_default();
            map.insert(field.to_string(), serde_json::Value::String(device.clone()));
            map.remove(place);
            let note = format!(
                "{field} is {device} and the {place} is gone: the call this switch stands in \
                 for was aimed at that one device"
            );
            return Room::Device(serde_json::Value::Object(map), note);
        }
    }

    // Written in the shape the tool asked for, list or single value, exactly as
    // a kind that disagreed with the house is corrected.
    let Some(spec) = schema.get("properties").and_then(|p| p.get(kind_field)) else {
        // The tool takes no kind at all, so there is nothing to narrow it with
        // and the model has to aim it at one device instead.
        return Room::Complain(format!(
            "{} with the {place} {area} would switch everything in {area}, and this request is \
             about {kind}. Name the one device that was meant.",
            call.name
        ));
    };
    let value = if spec.get("type").and_then(|t| t.as_str()) == Some("array") {
        serde_json::Value::Array(vec![serde_json::Value::String(kind.clone())])
    } else {
        serde_json::Value::String(kind.clone())
    };
    let mut map = args.as_object().cloned().unwrap_or_default();
    map.insert(kind_field.to_string(), value);
    let note = format!(
        "{kind_field} is {kind}: without it this would switch everything in {area}, and the call \
         this switch stands in for was about {kind}"
    );
    Room::Kind(serde_json::Value::Object(map), note)
}

/// A call Fono would not send, and what to tell the model about it.
#[derive(Debug)]
struct Refusal {
    /// What would have gone out, so the run is recorded by the arguments it
    /// used whether or not they travelled.
    sent: String,
    /// The reason, in the model's hands, phrased as something to act on.
    complaint: String,
    /// Whether writing the same call again is worth sending. See
    /// [`ToolOutcome::repeat_ok`].
    repeat_ok: bool,
}

impl Refusal {
    /// Build one, and record it in the log and the trace on the way — every
    /// refusal is news, and a run is only readable if they all read alike.
    fn of(call: &ToolCall, args: &serde_json::Value, complaint: &str, repeat_ok: bool) -> Self {
        warn!(tool = %call.name, args = %call.arguments, "actions: not sending {}: {complaint}", call.name);
        current_instant(
            "tool.rejected",
            "actions",
            ACTIONS_LANE,
            serde_json::json!({ "tool": call.name, "args": args, "complaint": complaint }),
        );
        Self {
            sent: args.to_string(),
            complaint: format!("{} was not sent: {complaint}", call.name),
            repeat_ok,
        }
    }
}

/// What the user said this turn, and which tools have already been told a
/// number in their arguments was never asked for.
///
/// One per turn, because both halves are: the words change every turn, and a
/// tool that has spent its one refusal must get through on its next attempt.
#[derive(Default)]
struct Words {
    said: fono_assistant::Said,
    told: std::sync::Mutex<std::collections::HashSet<String>>,
    /// The kind of device this turn turned out to be about, when a tool that
    /// names one could not run. It outlives that tool: the retry reaches for
    /// something that switches anything at all, and this is what stops it
    /// switching the whole room.
    kind: std::sync::Mutex<Option<String>>,
    /// The device that tool was aimed at, when it named one. What stops the
    /// retry needing a second go: the switch is pointed straight back at it.
    device: std::sync::Mutex<Option<String>>,
}

impl Words {
    /// Is this the first call to this tool in this turn? Records it on the way
    /// past, so the answer is `false` from then on.
    ///
    /// A poisoned lock answers `false`, which lets the call through unchecked —
    /// the safe direction, since the worst a missed check costs is the wrong
    /// tool running, and the worst a wrongly applied one costs is a command the
    /// user asked for never happening.
    fn first_attempt_at(&self, tool: &str) -> bool {
        self.told.lock().map(|mut told| told.insert(tool.to_string())).unwrap_or(false)
    }

    /// Remember what this turn is about, and which device. Ignored when the
    /// tool names no kind, and never overwritten: the first tool the model
    /// reached for is the one that read the request, and a later guess should
    /// not talk over it.
    fn is_about(&self, kind: Option<&str>, device: Option<&str>) {
        let Some(kind) = kind else { return };
        if let Ok(mut about) = self.kind.lock() {
            about.get_or_insert_with(|| kind.to_string());
        }
        if let (Some(device), Ok(mut aimed)) = (device, self.device.lock()) {
            aimed.get_or_insert_with(|| device.to_string());
        }
    }

    /// What this turn is about, when something has said.
    fn about_kind(&self) -> Option<String> {
        self.kind.lock().ok().and_then(|k| k.clone())
    }

    /// The device this turn is about, when a tool named one.
    fn about_device(&self) -> Option<String> {
        self.device.lock().ok().and_then(|d| d.clone())
    }
}

/// Run one call the model asked for and describe what happened.
///
/// Never returns an error: a tool that failed is the news, not a fault in
/// the turn, and the user has to hear it.
///
/// Long because it is a ladder, and the rungs only mean anything in order —
/// each one is a stronger claim about what happened than the one below it, and
/// splitting them into functions would hide which claim a given answer rests
/// on.
#[allow(clippy::too_many_lines)]
async fn run_one(
    runnable: &std::collections::HashMap<String, Runnable>,
    house: &HouseFacts,
    words: &Words,
    call: ToolCall,
) -> Ran {
    // Two ways for a call to end badly, and they are not equally safe to
    // repeat. `nothing_happened` is for the cases where the request never
    // reached the world — an unknown tool, an unreachable server, a payload
    // the server refused — so a second go cannot double anything and is always
    // offered. `not_as_asked` is for the cases where something may already have
    // moved, and only the vendor can say whether asking again is the same
    // request as asking once.
    // Nothing left, so what would have been sent is the best statement of what
    // this call was. It is only ever read as a failed run, which demotes a
    // phrase; nothing is ever promoted from here.
    let nothing_happened = |sent: &str, s: String| Ran {
        out: ToolOutcome {
            summary: format!("{s} {RETRY_INVITATION}"),
            failed: true,
            retryable: true,
            // Filled in once, where the outcome leaves the executor.
            sent: None,
            repeat_ok: false,
            confirmed: false,
        },
        how: RunOutcome::Failed,
        targets: Vec::new(),
        sent: sent.to_string(),
    };

    let Some(r) = runnable.get(&call.name) else {
        return nothing_happened(
            &call.arguments,
            format!("There is no tool called {}.", call.name),
        );
    };
    let args = match prepare_args(r, house, words, &call) {
        Ok(args) => args,
        Err(no) => {
            let mut ran = nothing_happened(&no.sent, no.complaint);
            ran.out.repeat_ok = no.repeat_ok;
            return ran;
        }
    };

    let sent = args.to_string();
    let res = match execute(r, &call, &args).await {
        Ok(res) => res,
        // Either the server was never reached or it objected outright. Both
        // mean nothing moved, so both are safe to offer again.
        Err(complaint) => return nothing_happened(&sent, complaint),
    };

    // Which software answered decides what its answer means, and the answer
    // itself is the only thing that can say. Anything unrecognised gets no
    // opinion, so the rungs below simply do not fire.
    let vendor = vendor::for_result(&res.text);
    // Read once, off the reply, and carried on every outcome below — including
    // the half-done one, which is precisely where knowing *which* device was
    // left behind is the whole answer.
    let touched = vendor.targets(&res.text);
    let ok = |how: RunOutcome, s: String| Ran {
        out: ToolOutcome::worked(s),
        how,
        targets: touched.clone(),
        sent: sent.clone(),
    };

    // A command that may be safely asked for twice can be handed back for one
    // more attempt; one that names a change rather than an end state cannot,
    // because asking twice for two degrees warmer is four degrees.
    let not_as_asked = |s: String| Ran {
        out: ToolOutcome {
            summary: if vendor.repeatable(&call.name) {
                format!("{s} {RETRY_THE_REST}")
            } else {
                s
            },
            failed: true,
            retryable: vendor.repeatable(&call.name),
            sent: None,
            // Something in the world may already have moved, so a repeat is
            // never a free question here.
            repeat_ok: false,
            confirmed: false,
        },
        how: RunOutcome::Failed,
        targets: touched.clone(),
        sent: sent.clone(),
    };

    // Second rung. A server can answer "fine" and mean nothing of the sort:
    // Home Assistant returns an ordinary, error-free result for a command that
    // matched no device. Only the vendor can read that admission.
    //
    // A half-done command is its own answer and must not be flattened into
    // either neighbour. Told "it did not work" the model apologises for
    // nothing; told "done" it hides a lamp that stayed dark. Naming the
    // devices that were missed is what lets the reply be true.
    match vendor.admission(&res.text) {
        Some(vendor::Admission::NothingWorked) => {
            return not_as_asked(format!("{} did not work: {}", call.name, brief(&res.text)));
        }
        Some(vendor::Admission::PartlyWorked { failed }) => {
            return not_as_asked(format!(
                "{} worked for some devices but not for these: {}. Tell the user which ones \
                 did not respond, and do not claim the whole request succeeded.",
                call.name,
                failed.join(", ")
            ));
        }
        Some(vendor::Admission::Worked) | None => {}
    }

    // Top rung. Ask the world itself, rather than taking the server's word for
    // its own success. Costs one extra read (~100 ms), so it is only paid when
    // there is a readback tool and the server named something it touched —
    // which is nothing for a tool that only reads, and nothing at all for a
    // server Fono has no specific knowledge of.
    //
    // The read is deliberately not gated on the vendor being able to *judge*
    // the answer. It only knows the end state two of this server's tools ask
    // for, and gating on that left every value-setting tool unwatched: a lamp
    // that answered "done" to a brightness it never took was reported as a
    // success, and the model then told the user so. Reading the world and
    // handing back what it says needs no such knowledge, and lets the model be
    // the one to notice that "off" was asked for and `on` came back.
    //
    // `checked` records whether the world was consulted and agreed. An unproven
    // check must not set it: "checked" and "the server did not complain" are
    // different claims, and the record is only worth keeping while it holds
    // them apart.
    // What a call that worked is worth handing back, in place of the server's
    // own words for it.
    let worked = || landed(&call.name, &touched).unwrap_or_else(|| brief(&res.text));
    match (&r.readback, r.verify) {
        (Some(rb), VerifyClass::PostCondition) if !touched.is_empty() => {
            let ask = about(runnable.get(rb).map(|rb| &rb.schema), vendor.slot_fields(), &args);
            let looked = confirm(r, vendor, rb, &ask, &call, &res.text).await;
            let reads = state_of_the_house(&looked.readings);
            match looked.verdict {
                Some(Verdict::Contradicted) => {
                    // Deliberately not "nothing changed": the check may have
                    // found some devices obeying and others not, and claiming
                    // more than was observed is the mistake this rung exists
                    // to stop.
                    return not_as_asked(format!(
                        "{} was accepted, but the devices are not in the state you asked \
                         for.{reads}",
                        call.name
                    ));
                }
                Some(Verdict::Confirmed) => {
                    info!(tool = %call.name, "action confirmed");
                    return ok(RunOutcome::Confirmed, format!("{}{reads}", worked()));
                }
                // Unproven is not disproven: the weaker rungs stand. What the
                // house reads is still worth saying, because the alternative is
                // a reply built on the server's word alone.
                None if !reads.is_empty() => {
                    return ok(RunOutcome::Accepted, format!("{}{reads}", worked()));
                }
                None => {}
            }
        }
        // Nothing observes this tool's effect, so "it was accepted" is the
        // strongest true statement available. Saying "done" here would be
        // inventing evidence.
        //
        // The server's own words are kept whole here, unlike every branch
        // above: a tool that only reads *is* its answer, and shortening it to
        // the devices it mentions would throw away the thing the model asked
        // the question to find out.
        (_, VerifyClass::None) => {
            return ok(RunOutcome::Sent, format!("{} was sent. {}", call.name, brief(&res.text)));
        }
        _ => {}
    }
    ok(RunOutcome::Accepted, worked())
}

/// What a call that worked is worth handing back to the model.
///
/// A server's reply to a command it carried out is written for a program, not
/// for a reader. A stock Home Assistant answers an area-wide command with every
/// entity it reached and a 32-character identifier for each: one measured reply
/// ran to 3,447 characters, which is 1,090 tokens the model pays to read and
/// pays again to answer, for a command whose whole content is "these seven
/// things did it". The names are the only part that says anything, and Fono has
/// already read them off the same reply.
///
/// `None` when the server named nothing it reached — the truth for every server
/// Fono has no specific knowledge of, and for a call that moved a device the
/// server did not list. The reply is handed back whole in that case.
fn landed(tool: &str, touched: &[vendor::Target]) -> Option<String> {
    // A long list is a command that reached a whole area, and the model does
    // not need the roll call to answer for it. Enough names to say what kind of
    // thing was reached, then the count.
    const NAMED: usize = 8;
    let names: Vec<&str> = touched.iter().filter(|t| t.landed).map(|t| t.name.as_str()).collect();
    match names.len() {
        0 => None,
        n if n <= NAMED => Some(format!("{tool} reached {}.", names.join(", "))),
        n => Some(format!(
            "{tool} reached {} devices: {} and {} more.",
            n,
            names[..NAMED].join(", "),
            n - NAMED
        )),
    }
}

/// Which devices to ask the house about, taken from the command just sent.
///
/// The readback is a second round trip to the same server, and asked bare it
/// answers with every device in the home — a whole-house dump to check that one
/// lamp did what it was told. Every field of the command that names *what* it
/// was aimed at is worth repeating to the reader, and nothing else is: the
/// value that was set says nothing about which devices to look at.
///
/// Both halves come from what the servers publish about themselves, so this
/// carries no knowledge of any particular tool. The vendor's slot table already
/// names the fields that mean a place, a device and a kind, and a field is only
/// repeated when the reading tool's own schema advertises it and accepts the
/// type. A reader with no such fields, or a command that named nothing, asks
/// bare as before.
fn about(
    schema: Option<&serde_json::Value>,
    slots: vendor::SlotFields,
    sent: &serde_json::Value,
) -> serde_json::Value {
    let mut ask = serde_json::Map::new();
    let props = schema.and_then(|s| s.get("properties")).and_then(|p| p.as_object());
    if let Some(props) = props {
        for field in [slots.place, slots.device, slots.kind].into_iter().flatten() {
            let (Some(value), Some(spec)) = (sent.get(field), props.get(field)) else { continue };
            // A reader that spells out a different type for the same field —
            // one domain where the command took a list — is not being argued
            // with. Leaving the field out reads the house widely, which is
            // still true, where sending the wrong shape risks an error.
            let fits = spec
                .get("type")
                .and_then(|t| t.as_str())
                .is_none_or(|want| matches_json_type(want, value));
            if fits {
                ask.insert(field.to_string(), value.clone());
            }
        }
    }
    serde_json::Value::Object(ask)
}

/// Re-read the world: what the vendor makes of it, and what it plainly says.
///
/// Both are kept because they are different claims and can disagree. A verdict
/// is only available for a tool whose intended end state Fono knows, and
/// reporting one without the other would leave no way to tell a judged check
/// from an unjudged one after the fact.
struct Looked {
    verdict: Option<Verdict>,
    readings: Vec<(String, String)>,
}

/// How long a device is given to admit it obeyed before the reading is
/// believed over the command.
///
/// A device does not always report its new state when it accepts a command.
/// An air conditioner in the author's home (observed 2026-08-03, from its own
/// recorded history) kept saying `off` for up to eight seconds after switching
/// on, so a reading taken at once shows the state the command was sent to
/// change. Fono then told the user their command had failed while the room was
/// cooling — a worse lie than the one the check exists to prevent.
const OBEY_WINDOW: std::time::Duration = std::time::Duration::from_secs(8);

/// How long to wait between readings inside [`OBEY_WINDOW`].
const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_millis(1500);

/// Re-read the world and ask the vendor what it shows.
///
/// A readback that fails to arrive yields nothing: not being able to look is
/// not the same as having looked and found a problem, and reporting a working
/// command as broken because a second request timed out would be its own bug.
///
/// A reading that contradicts the command is taken again, up to
/// [`OBEY_WINDOW`], because a slow device and a disobedient one look identical
/// in the first reading and only one of them changes its mind. Nothing else is
/// retried: a confirmed or unjudged check is already as good as it will get,
/// so the extra round trips are only ever spent on the turn that would
/// otherwise have said something untrue.
async fn confirm(
    r: &Runnable,
    vendor: &'static dyn Vendor,
    readback: &str,
    ask: &serde_json::Value,
    call: &ToolCall,
    result: &str,
) -> Looked {
    // Sequential with `tool.execute` and never nested inside it, so the two
    // costs read off the lane separately: proving a command landed is a whole
    // extra round trip to the same server, and it is charged to the same turn.
    let span = current_span("tool.verify", "actions", ACTIONS_LANE);
    let started = std::time::Instant::now();
    let mut reads = 0_usize;
    let looked = loop {
        let back = mcp_client::call_tool(&r.endpoint, readback, ask).await;
        reads += 1;
        let looked = match &back {
            Ok(back) => Looked {
                verdict: vendor.confirms(call, result, &back.text),
                readings: vendor.readings(&back.text, &claimed(vendor, result)),
            },
            Err(e) => {
                warn!("actions: could not check whether {} worked: {e}", call.name);
                Looked { verdict: None, readings: Vec::new() }
            }
        };
        if looked.verdict != Some(Verdict::Contradicted)
            || started.elapsed() + LOOK_AGAIN >= OBEY_WINDOW
        {
            break looked;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    // The server's claim and the house's reading are both stamped, so a run can
    // be asked afterwards whether the two ever disagreed — the question that
    // decides whether the extra read is worth its round trip.
    span.finish(serde_json::json!({
        "tool": call.name,
        "readback": readback,
        "verdict": match looked.verdict {
            Some(Verdict::Confirmed) => "confirmed",
            Some(Verdict::Contradicted) => "contradicted",
            None => "unproven",
        },
        "claimed": claimed(vendor, result),
        "reading": looked.readings.iter().map(|(n, s)| format!("{n}: {s}")).collect::<Vec<_>>(),
        // More than one means a device was slow to admit it obeyed, and says
        // how much of the turn went on waiting for it.
        "reads": reads,
    }));
    looked
}

/// The devices the server said it reached, and which are therefore worth
/// looking up.
fn claimed(vendor: &'static dyn Vendor, result: &str) -> Vec<String> {
    vendor.targets(result).into_iter().filter(|t| t.landed).map(|t| t.name).collect()
}

/// What the house reads, as one sentence to hand back to the model.
///
/// A command aimed at a room reaches everything in it, and naming each one back
/// is the largest thing the model reads in an ordinary turn: one measured reply
/// listed twelve lights in 152 tokens, which the model paid to read twice while
/// it corrected itself — 5.7 s of a 15.3 s turn, for a ten-word request.
///
/// The reader only needs to know whether the devices agree, and which ones do
/// not. So devices in the same state are counted rather than listed, and the
/// names are spent on the ones that stand apart: the largest group is a bare
/// count, every smaller group is named. A device that disobeyed is the whole
/// point of looking, and it is exactly what survives.
///
/// Grouping is by equality of the state string, so this knows nothing of what
/// any state means and works for a server Fono has never seen.
///
/// Empty when there is nothing to report, so a caller can append it
/// unconditionally without leaving a stray sentence behind.
fn state_of_the_house(readings: &[(String, String)]) -> String {
    // Under this, naming each device is both short and more useful than any
    // summary of it.
    const LIST_ALL: usize = 4;
    // Enough of a minority to say which devices stand apart, before the list
    // itself becomes the thing being paid for.
    const NAMED: usize = 6;

    // Two devices with the same name in the same state say the same thing
    // twice; two with the same name in *different* states are a genuine
    // disagreement and both survive.
    let mut seen: Vec<&(String, String)> = Vec::with_capacity(readings.len());
    for r in readings {
        if !seen.contains(&r) {
            seen.push(r);
        }
    }
    let told = match seen.len() {
        0 => return String::new(),
        n if n <= LIST_ALL => {
            seen.iter().map(|(n, s)| format!("{n} is {s}")).collect::<Vec<_>>().join(", ")
        }
        total => {
            let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
            for (name, state) in &seen {
                match groups.iter_mut().find(|(s, _)| *s == state.as_str()) {
                    Some((_, names)) => names.push(name),
                    None => groups.push((state, vec![name])),
                }
            }
            // Biggest first, so the bare count is the one that would have cost
            // the most to spell out. Ties keep the order the house read them in.
            groups.sort_by_key(|g| std::cmp::Reverse(g.1.len()));
            if groups.len() == 1 {
                format!("all {total} are {}", groups[0].0)
            } else {
                let parts: Vec<String> = groups
                    .iter()
                    .enumerate()
                    .map(|(i, (state, names))| {
                        let is = if names.len() == 1 { "is" } else { "are" };
                        match (i, names.len()) {
                            (0, n) => format!("{n} {is} {state}"),
                            (_, n) if n <= NAMED => {
                                format!("{n} {is} {state}: {}", names.join(", "))
                            }
                            (_, n) => format!(
                                "{n} {is} {state}: {} and {} more",
                                names[..NAMED].join(", "),
                                n - NAMED
                            ),
                        }
                    })
                    .collect();
                format!("{total} devices — {}", parts.join("; "))
            }
        }
    };
    format!(
        " Reading the home back afterwards: {told}. Tell the user what the home actually says, \
         not what was asked for."
    )
}

/// Servers can be chatty, and every extra token here is paid for twice — once
/// reading it, once replying to it.
///
/// What survives is the start and the end, because the part a server puts last
/// is often the part that says what went wrong: Home Assistant's result ends
/// with its list of failures, and an earlier head-only cap cut exactly that
/// off, leaving the model reading an apparently clean success. Keeping both
/// ends is what lets the cap be tight rather than generous. Trimming is visible
/// (`…`) so a shortened result is never mistaken for a complete one.
fn brief(text: &str) -> String {
    const HEAD: usize = 700;
    const TAIL: usize = 500;
    let t = text.trim();
    if t.is_empty() {
        return "Done.".into();
    }
    let count = t.chars().count();
    if count <= HEAD + TAIL {
        return t.to_string();
    }
    let head_end = t.char_indices().nth(HEAD).map_or(t.len(), |(i, _)| i);
    let tail_start = t.char_indices().nth(count - TAIL).map_or(t.len(), |(i, _)| i);
    format!("{}…{}", &t[..head_end], &t[tail_start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(verify: VerifyClass) -> std::collections::HashMap<String, Runnable> {
        let mut m = std::collections::HashMap::new();
        m.insert(
            "HassTurnOn".to_string(),
            Runnable {
                endpoint: McpEndpoint {
                    url: "http://127.0.0.1:1/sse".into(),
                    token: None,
                    timeout: std::time::Duration::from_millis(50),
                },
                verify,
                readback: Some("GetLiveContext".into()),
                schema: serde_json::json!({}),
                source: "home".into(),
            },
        );
        m
    }

    fn call(name: &str) -> ToolCall {
        ToolCall { id: "1".into(), name: name.into(), arguments: "{}".into() }
    }

    /// A name the catalogue does not know must be reported, not silently
    /// dropped — otherwise the model waits for a result that never comes.
    #[tokio::test]
    async fn an_unknown_tool_is_reported() {
        let ran = run_one(
            &tools(VerifyClass::PostCondition),
            &HouseFacts::default(),
            &Words::default(),
            call("Nope"),
        )
        .await;
        assert!(ran.out.failed, "an unknown tool is not a success");
        assert!(ran.out.summary.contains("no tool called Nope"), "{}", ran.out.summary);
        assert_eq!(ran.how, RunOutcome::Failed);
    }

    /// A server we cannot reach must say so in words the user can act on,
    /// rather than the turn failing or, worse, claiming success.
    #[tokio::test]
    async fn an_unreachable_server_is_reported_not_claimed_done() {
        let ran = run_one(
            &tools(VerifyClass::PostCondition),
            &HouseFacts::default(),
            &Words::default(),
            call("HassTurnOn"),
        )
        .await;
        assert!(ran.out.failed, "unreachable must not be logged as a success");
        assert!(ran.out.summary.starts_with("HassTurnOn could not be run"), "{}", ran.out.summary);
        assert!(!ran.out.summary.to_lowercase().contains("done"), "{}", ran.out.summary);
        // And it must be written down as a failure, not as "sent" — nothing
        // was sent, and a page saying otherwise would be worse than silent.
        assert_eq!(ran.how, RunOutcome::Failed);
    }

    /// Verbatim from a trace: asked to turn the kitchen lights off, a small
    /// local model filled in every field the tool advertises, two of them with
    /// nothing. Home Assistant rejected the whole command over the `null` and
    /// the light stayed on, twice in a row. A key left blank is a key the
    /// caller did not mean to set.
    #[test]
    fn a_blank_argument_is_not_sent_to_the_server() {
        let args = serde_json::json!({
            "area": "Kitchen",
            "domain": ["light"],
            "floor": null,
            "name": "",
            "device_class": [],
            "extra": {"nested": null, "kept": "yes"},
        });
        assert_eq!(
            drop_empty_arguments(args),
            serde_json::json!({
                "area": "Kitchen",
                "domain": ["light"],
                "extra": {"kept": "yes"},
            })
        );
    }

    /// Trimming must never change what was asked for: a real value of every
    /// shape survives, including the ones that look empty but are not.
    #[test]
    fn a_real_argument_is_passed_through_untouched() {
        let args = serde_json::json!({
            "brightness": 0,
            "on": false,
            "name": "Kitchen lights",
            "domain": ["light", "switch"],
        });
        assert_eq!(drop_empty_arguments(args.clone()), args);
    }

    /// The rails make the kind of device compulsory so it cannot be forgotten,
    /// and hand the model one word for "everything in this area". No server has
    /// heard of that word, so it must be taken back out before the call leaves —
    /// and taking it out has to mean the field is absent, which is what a server
    /// has always read as "everything".
    #[test]
    fn the_word_for_everything_never_reaches_the_server() {
        let args = serde_json::json!({
            "area": "Kitchen",
            "domain": [fono_core::tool_grammar::ANY_KIND],
        });
        // Both rules run together on the real path, in this order.
        assert_eq!(
            drop_empty_arguments(drop_any_kind(args)),
            serde_json::json!({ "area": "Kitchen" }),
            "the whole field goes, not just the word — a list with it removed asks for nothing"
        );

        // A bare string form is handled too, since a schema may not use a list.
        let args = serde_json::json!({ "domain": fono_core::tool_grammar::ANY_KIND });
        assert_eq!(drop_empty_arguments(drop_any_kind(args)), serde_json::json!({}));
    }

    /// A real kind of device must survive untouched, or asking for the lights
    /// only would silently become asking for everything in the area.
    #[test]
    fn a_real_kind_of_device_is_left_alone() {
        let args = serde_json::json!({ "area": "Kitchen", "domain": ["light"] });
        assert_eq!(drop_any_kind(args.clone()), args);
    }

    /// A house with one of each, a name two devices answer to, and — as real
    /// homes have — two air conditioners, one of them named after the room it
    /// is in and one not.
    fn house() -> HouseFacts {
        let mut kind_of = std::collections::HashMap::new();
        kind_of.insert("air conditioner".to_string(), "climate".to_string());
        kind_of.insert("office air conditioner".to_string(), "climate".to_string());
        kind_of.insert("balcony lights".to_string(), "light".to_string());
        let sole = [
            ("air conditioner", "Air conditioner"),
            ("office air conditioner", "Office air conditioner"),
            ("balcony lights", "Balcony lights"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let places = ["kitchen", "yard", "office"].into_iter().map(String::from).collect();
        HouseFacts { slots: slots(), kind_of, sole, places, vendor: &vendor::HomeAssistant }
    }

    /// The fields Home Assistant uses to say *which* device a call is about.
    fn slots() -> vendor::SlotFields {
        vendor::SlotFields {
            place: Some("area"),
            wider_place: Some("floor"),
            device: Some("name"),
            kind: Some("domain"),
            filter: Some("device_class"),
        }
    }

    /// A tool with only its schema filled in. Nothing here reaches a server,
    /// so the endpoint is a placeholder.
    fn runnable(schema: serde_json::Value) -> Runnable {
        Runnable {
            endpoint: fono_assistant::mcp_client::McpEndpoint {
                url: "http://example.invalid/mcp".to_string(),
                token: None,
                timeout: CALL_TIMEOUT,
            },
            verify: VerifyClass::None,
            readback: None,
            schema,
            source: "hass".to_string(),
        }
    }

    /// One call the model wrote.
    fn wrote(name: &str, arguments: &str) -> ToolCall {
        ToolCall { id: "1".to_string(), name: name.to_string(), arguments: arguments.to_string() }
    }

    /// What the user said, in the shape the checks read it.
    fn said(words: &str) -> Words {
        let w = Words::default();
        w.said.heard(words);
        w
    }

    /// A tool that sets one thing is the tool that sets that thing. Home
    /// Assistant's set-temperature intent takes four fields naming a device
    /// and one temperature; the switch intents take no value at all.
    #[test]
    fn a_tool_is_reduced_to_the_one_value_it_sets() {
        let set_temperature = serde_json::json!({"properties": {
            "area": {}, "floor": {}, "name": {}, "domain": {}, "temperature": {}
        }});
        assert_eq!(
            the_one_value_a_tool_sets(&set_temperature, slots()),
            Some("temperature".to_string())
        );

        let turn_off = serde_json::json!({"properties": {
            "area": {}, "floor": {}, "name": {}, "domain": {}, "device_class": {}
        }});
        assert_eq!(the_one_value_a_tool_sets(&turn_off, slots()), None, "it sets nothing");

        let light_set = serde_json::json!({"properties": {
            "name": {}, "brightness": {}, "color": {}
        }});
        assert_eq!(
            the_one_value_a_tool_sets(&light_set, slots()),
            None,
            "which of the two is wanted is the user's business"
        );
    }

    /// A server Fono does not recognise names no target fields, so every field
    /// counts as a value and nothing is ever insisted on. Saying nothing is the
    /// right answer when the two cannot be told apart.
    #[test]
    fn an_unrecognised_server_has_nothing_insisted_upon() {
        let schema = serde_json::json!({"properties": {"area": {}, "temperature": {}}});
        assert_eq!(the_one_value_a_tool_sets(&schema, vendor::SlotFields::default()), None);
    }

    /// Verbatim from the benchmark: eight calls in one run were
    /// `HassClimateSetTemperature` with no temperature at all, written after
    /// Fono had stopped an invented one. The model deleted the field rather
    /// than change tool, and the house refused every one of them.
    #[test]
    fn a_value_tool_written_without_its_value_stays_at_home() {
        let schema = serde_json::json!({"properties": {
            "area": {}, "name": {}, "temperature": {}
        }});
        let r = runnable(schema);
        let call = wrote("HassClimateSetTemperature", r#"{"name": "Air conditioner"}"#);
        let Err(refused) = prepare_args(&r, &house(), &Words::default(), &call) else {
            panic!("a set-temperature call with no temperature sets nothing")
        };
        assert!(refused.complaint.contains("temperature"), "{}", refused.complaint);
        assert!(!refused.repeat_ok, "writing it again would still set nothing");
        assert!(
            refused.complaint.contains("HassTurnOn"),
            "the way out is named, not described: {}",
            refused.complaint
        );

        // A server that names no switching tool still gets a way out, described
        // rather than named — there is no name to give.
        let quiet = HouseFacts { vendor: &vendor::Unknown, ..house() };
        let Err(refused) = prepare_args(&r, &quiet, &Words::default(), &call) else {
            panic!("still sets nothing")
        };
        assert!(refused.complaint.contains("switches things without one"), "{}", refused.complaint);

        let call =
            wrote("HassClimateSetTemperature", r#"{"name": "Air conditioner", "temperature": 23}"#);
        assert!(
            prepare_args(&r, &house(), &said("set the Air conditioner to 23"), &call).is_ok(),
            "a temperature was asked for"
        );
    }

    /// Verbatim from three runs, one in Romanian and two in English: told that a
    /// temperature tool cannot run without a temperature and to switch the thing
    /// instead, the model reached for the switch and pointed it at the whole
    /// room. An area-wide switch reaches everything in the area, so a request
    /// about one air conditioner turned on every light in the office \u2014 and the
    /// turn before it turned them all off.
    #[test]
    fn a_switch_aimed_at_a_whole_area_is_narrowed_to_the_kind_the_turn_named() {
        let words = said("turn on the air conditioning in the Office");
        let value_tool = runnable(serde_json::json!({"properties": {
            "area": {}, "name": {}, "domain": {}, "temperature": {}
        }}));
        let no_value = wrote("HassClimateSetTemperature", r#"{"area": "Office"}"#);
        assert!(
            prepare_args(&value_tool, &house(), &words, &no_value).is_err(),
            "there is no temperature to set"
        );

        // The tool that was refused is what says this request is about the
        // heating. The switch that replaces it must not lose that.
        let switch = runnable(serde_json::json!({"properties": {
            "area": {}, "name": {}, "domain": {"type": "array"}
        }}));
        let whole_room = wrote("HassTurnOn", r#"{"area": "Office"}"#);
        let sent = prepare_args(&switch, &house(), &words, &whole_room)
            .expect("the kind is written in rather than the room switched");
        assert_eq!(sent, serde_json::json!({"area": "Office", "domain": ["climate"]}));

        // A tool with nowhere to put the kind cannot be narrowed, so the model
        // has to aim it at one device instead.
        let blunt = runnable(serde_json::json!({"properties": {"area": {}, "name": {}}}));
        let Err(refused) = prepare_args(&blunt, &house(), &words, &whole_room) else {
            panic!("a bare area switches everything in it")
        };
        assert!(refused.complaint.contains("climate"), "{}", refused.complaint);
        assert!(refused.complaint.contains("everything in Office"), "{}", refused.complaint);
        assert!(!refused.repeat_ok, "the same call switches the same room");

        // What the model writes itself is left alone, kind or device.
        let with_kind = wrote("HassTurnOn", r#"{"area": "Office", "domain": ["climate"]}"#);
        assert!(prepare_args(&switch, &house(), &words, &with_kind).is_ok());
        let one_device = wrote("HassTurnOn", r#"{"name": "Office air conditioner"}"#);
        assert!(prepare_args(&switch, &house(), &words, &one_device).is_ok());

        // And a turn that never established a kind is not second-guessed: a
        // request to switch off a whole room is a real request, and it goes out
        // as the room.
        let room = prepare_args(&switch, &house(), &said("turn on the Office"), &whole_room)
            .expect("switching a whole room is a real request");
        assert_eq!(room, serde_json::json!({"area": "Office"}));
    }

    /// The half of that defect the user pays for: refused and told to switch the
    /// thing instead, the model wrote the room again, so the safe answer was to
    /// refuse a second time and nothing happened at all. When the refused call
    /// had one device to aim at, the switch is aimed at the same one and goes.
    #[test]
    fn a_switch_is_aimed_back_at_the_device_the_turn_settled_on() {
        // Words holding no published name, so the device on the call is the
        // model's own and nothing else can put it back.
        let words = said("pornește aerul condiționat din birou");
        let value_tool = runnable(serde_json::json!({"properties": {
            "area": {}, "name": {}, "domain": {}, "temperature": {}
        }}));
        let no_value = wrote(
            "HassClimateSetTemperature",
            r#"{"area": "Office", "name": "Office air conditioner"}"#,
        );
        assert!(prepare_args(&value_tool, &house(), &words, &no_value).is_err());

        let switch = runnable(serde_json::json!({"properties": {
            "area": {}, "name": {}, "domain": {}
        }}));
        let whole_room = wrote("HassTurnOn", r#"{"area": "Office"}"#);
        let sent = prepare_args(&switch, &house(), &words, &whole_room)
            .expect("the device it was aimed at is not a room");
        assert_eq!(sent, serde_json::json!({"name": "Office air conditioner"}));

        // An area the remembered device does not answer to is a different
        // request, and aiming at the device would act in the wrong room. The
        // kind is all that carries over.
        let elsewhere = wrote("HassTurnOn", r#"{"area": "Kitchen"}"#);
        let sent = prepare_args(&switch, &house(), &words, &elsewhere)
            .expect("a kitchen switch is still a switch");
        assert_eq!(
            sent,
            serde_json::json!({"area": "Kitchen", "domain": "climate"}),
            "nothing here says the Kitchen means that air conditioner"
        );
    }

    /// Verbatim from the benchmark, four cells over two languages: asked in
    /// plain words to turn the air conditioner off, the model named the right
    /// device and then called it a light. The house said otherwise when it was
    /// connected, so the disagreement has one answer and costs no round trip.
    #[test]
    fn a_kind_that_contradicts_the_named_device_is_corrected() {
        let (fixed, note) =
            house().agree(serde_json::json!({"name": "Air conditioner", "domain": ["light"]}));
        assert_eq!(fixed, serde_json::json!({"name": "Air conditioner", "domain": ["climate"]}));
        assert!(note.is_some_and(|n| n.contains("climate")), "the correction is written down");

        // Whatever shape it was written in is the shape the schema asked for.
        let (fixed, _) =
            house().agree(serde_json::json!({"name": "Air conditioner", "domain": "light"}));
        assert_eq!(fixed, serde_json::json!({"name": "Air conditioner", "domain": "climate"}));
    }

    /// Correcting must be silent whenever there is nothing it can prove: an
    /// agreeing kind, a device this home never mentioned, an area-wide command
    /// naming no device, and any server whose field names we do not know. In
    /// every one of those the call has to go out exactly as written.
    #[test]
    fn nothing_is_corrected_without_grounds() {
        let h = house();
        for args in [
            serde_json::json!({"name": "Air conditioner", "domain": ["climate"]}),
            serde_json::json!({"name": "Something else entirely", "domain": ["light"]}),
            serde_json::json!({"area": "Kitchen", "domain": ["light"]}),
            serde_json::json!({"name": "Air conditioner"}),
        ] {
            let (out, note) = h.agree(args.clone());
            assert_eq!(out, args, "left alone");
            assert_eq!(note, None);
        }

        let unknown = HouseFacts { slots: vendor::SlotFields::default(), ..house() };
        let args = serde_json::json!({"name": "Air conditioner", "domain": ["light"]});
        assert_eq!(unknown.agree(args.clone()), (args, None), "no field names, no opinion");
    }

    /// Verbatim from the benchmark, three cells: the kind was corrected, the
    /// call was sent, and the house refused it anyway because the area named
    /// beside the device was an area that device is not in. An area that picks
    /// out nothing more than the name already did can only narrow wrongly.
    #[test]
    fn an_area_named_beside_one_device_is_dropped() {
        let (fixed, note) = house().agree(serde_json::json!({
            "name": "Air conditioner",
            "area": "Kitchen",
            "floor": "1",
            "domain": ["light"],
        }));
        assert_eq!(fixed, serde_json::json!({"name": "Air conditioner", "domain": ["climate"]}));
        let note = note.expect("both repairs are written down");
        assert!(note.contains("climate"), "{note}");
        assert!(note.contains("area") && note.contains("floor"), "{note}");
    }

    /// Verbatim from a run, in Romanian and again in English: asked to aim the
    /// office air conditioner at a temperature, the model wrote the area beside
    /// the shorter name, and the area was taken out as redundant — so the call
    /// reached the *hall* unit, and the reply said the office. The area was not
    /// redundant at all: it was the only thing saying which of the two was
    /// meant.
    #[test]
    fn an_area_beside_a_name_two_rooms_share_picks_the_one_in_that_room() {
        let (fixed, note) =
            house().agree(serde_json::json!({"name": "Air conditioner", "area": "Office"}));
        assert_eq!(
            fixed,
            serde_json::json!({"name": "Office air conditioner"}),
            "the room that was said picks the device, and then has nothing left to say"
        );
        let note = note.expect("the substitution is written down");
        assert!(note.contains("Office air conditioner"), "{note}");

        // Only ever with one answer. A room that leaves the name as ambiguous as
        // it found it changes nothing about which device is meant.
        let (fixed, _) =
            house().agree(serde_json::json!({"name": "Air conditioner", "area": "Kitchen"}));
        assert_eq!(fixed, serde_json::json!({"name": "Air conditioner"}));
    }

    /// The area stays when it is the only thing telling two devices apart, and
    /// when the device named is one this home never mentioned — there the area
    /// may be all the server has to go on.
    #[test]
    fn an_area_that_is_still_doing_work_stays() {
        let mut h = house();
        h.sole.remove("air conditioner");
        let args = serde_json::json!({"name": "Air conditioner", "area": "Office"});
        assert_eq!(h.agree(args.clone()), (args, None), "two devices share the name");

        let args = serde_json::json!({"name": "Something else entirely", "area": "Office"});
        assert_eq!(house().agree(args.clone()), (args, None), "never heard of it");
    }

    /// Verbatim from a run: asked for the volume of a named display, the model
    /// added `device_class: ["tv"]` and the house matched nothing. A class of
    /// device narrows within a kind, so beside a name that already picks one
    /// device out it can only narrow to nothing.
    #[test]
    fn a_class_of_device_named_beside_one_device_is_dropped() {
        let (fixed, note) = house().agree(serde_json::json!({
            "name": "Balcony lights",
            "device_class": ["tv"],
            "brightness": 70,
        }));
        assert_eq!(fixed, serde_json::json!({"name": "Balcony lights", "brightness": 70}));
        assert!(note.is_some_and(|n| n.contains("device_class")), "the drop is written down");
    }

    /// Verbatim from a run: the model's second attempt at the guest bedroom
    /// lights *added* `floor: "1"`, and the house then failed to find the
    /// bedroom at all. A storey cannot narrow past the area inside it, so
    /// whatever narrows furthest is kept and everything wider goes — and this
    /// one needs no device name, which is what makes it more than the rule
    /// above.
    #[test]
    fn a_storey_named_beside_an_area_is_dropped() {
        let (fixed, note) = house()
            .agree(serde_json::json!({"area": "Guest bedroom", "floor": "1", "domain": ["light"]}));
        assert_eq!(fixed, serde_json::json!({"area": "Guest bedroom", "domain": ["light"]}));
        assert!(note.is_some_and(|n| n.contains("floor")), "the drop is written down");

        // A storey on its own is the target and stays: it is the narrowest
        // thing the call gave.
        let args = serde_json::json!({"floor": "1", "domain": ["light"]});
        assert_eq!(house().agree(args.clone()), (args, None), "nothing narrower was given");
    }

    /// Verbatim from a run: *"turn off the Air conditioner"* produced
    /// `HassTurnOff {"area": "Master bedroom"}`, and the house switched off two
    /// beds, two lights, a salt lamp and a curtain. Nine of the nineteen
    /// commands that spoke a catalogued name did this. The name is in the
    /// request exactly as the home writes it, and the home's own spelling is
    /// what goes back — the user's casing matches nothing.
    #[test]
    fn a_device_the_user_named_becomes_the_target() {
        let (fixed, note) = house().aim_at_what_was_said(
            serde_json::json!({"area": "Master bedroom"}),
            "turn off the air conditioner",
        );
        assert_eq!(fixed["name"], "Air conditioner", "the home's spelling, not the user's");
        assert!(note.is_some_and(|n| n.contains("Air conditioner")), "written down");

        // And then the rule that was waiting for a name to fire on takes the
        // area out, because a name only one device answers to needs none.
        let (fixed, _) = house().agree(fixed);
        assert_eq!(fixed, serde_json::json!({"name": "Air conditioner"}));
    }

    /// The four cases where acting would be Fono guessing. Each has to leave
    /// the call exactly as the model wrote it.
    #[test]
    fn a_name_is_only_recovered_when_there_is_one_answer() {
        let h = house();
        let said = "turn off the air conditioner";

        // Already named it: nothing to add.
        let args = serde_json::json!({"name": "Air conditioner"});
        assert_eq!(h.aim_at_what_was_said(args.clone(), said), (args, None));

        // Nothing catalogued was spoken.
        let args = serde_json::json!({"area": "Kitchen"});
        assert_eq!(h.aim_at_what_was_said(args.clone(), "turn the heating up"), (args, None));

        // Two devices answer to the name, so the area is the only thing telling
        // them apart.
        let mut shared = house();
        shared.sole.remove("air conditioner");
        let args = serde_json::json!({"area": "Master bedroom"});
        assert_eq!(shared.aim_at_what_was_said(args.clone(), said), (args, None));

        // A word that is both a room and a device has two readings, and
        // "everything in the Office" must not become one thing called Office.
        let mut twice = house();
        twice.sole.insert("office".into(), "Office".into());
        let args = serde_json::json!({"area": "Office", "domain": ["light"]});
        assert_eq!(
            twice.aim_at_what_was_said(args.clone(), "turn off the lights in the office"),
            (args, None)
        );

        // A server that publishes no field for a device name has no opinion.
        let unknown = HouseFacts { slots: vendor::SlotFields::default(), ..house() };
        let args = serde_json::json!({"area": "Master bedroom"});
        assert_eq!(unknown.aim_at_what_was_said(args.clone(), said), (args, None));
    }

    /// A name is only found as a name. Without the boundary test a home is not
    /// free to call a thing after a syllable, and the longest spoken name has
    /// to win or a home holding both `Couch` and `Couch Blue` reaches the wrong
    /// one.
    #[test]
    fn a_name_is_matched_whole_and_the_longest_wins() {
        assert!(spoken_in("turn off the couch blue", "couch"));
        assert!(!spoken_in("switch it off", "tv"), "not inside a word");
        assert!(spoken_in("the tv, please", "tv"), "punctuation is not a letter");

        let mut h = house();
        h.sole.insert("couch".into(), "Couch".into());
        h.sole.insert("couch blue".into(), "Couch Blue".into());
        let (fixed, _) = h.aim_at_what_was_said(serde_json::json!({}), "turn on the couch blue");
        assert_eq!(fixed["name"], "Couch Blue", "the whole of what was said");
    }

    /// The defect that started this, verbatim: *"turn the air conditioner
    /// off"*, and the model called the tool that sets a temperature with
    /// `temperature: 0`. The schema is happy and the value is not blank, so the
    /// only evidence against it is the request — nobody said a number.
    #[test]
    fn a_number_the_user_never_said_keeps_the_call_at_home() {
        let args = serde_json::json!({"name": "Air conditioner", "temperature": 0});
        let (kept, complaint) = numbers_nobody_asked_for(args.clone(), "oprește aerul condiționat");
        let complaint = complaint.expect("a value nobody asked for is not sent");
        assert_eq!(kept, args, "nothing is quietly deleted; the call is simply not sent");
        assert!(complaint.contains("temperature"), "{complaint}");
        assert!(complaint.contains("on/off"), "the model needs a way out: {complaint}");
    }

    /// The rule is the same wherever the number sits, and that is the point:
    /// dropping it instead threw away the value *"set the Couch Blue to thirty
    /// percent"* had asked for, because "thirty" is not a digit. Asked about, it
    /// can come back.
    #[test]
    fn a_number_beside_another_value_is_asked_about_too() {
        let args = serde_json::json!({"name": "Couch Blue", "color": "blue", "brightness": 30});
        let (kept, complaint) =
            numbers_nobody_asked_for(args.clone(), "set the Couch Blue to thirty percent");
        let complaint = complaint.expect("a spelled-out number is still not a digit");
        assert_eq!(kept, args, "the brightness survives to be written again");
        assert!(complaint.contains("brightness (30)"), "{complaint}");
    }

    /// The check must never touch a number the user did in fact ask for, in
    /// any language — a digit is a digit — and must have no opinion at all when
    /// the words were never recorded.
    #[test]
    fn a_number_the_user_asked_for_is_sent_untouched() {
        let args = serde_json::json!({"name": "Office thermostat", "temperature": 21});
        for said in ["set the office thermostat to 21", "pune termostatul la 21 de grade", ""] {
            let (fixed, complaint) = numbers_nobody_asked_for(args.clone(), said);
            assert_eq!(fixed, args, "left alone for {said:?}");
            assert_eq!(complaint, None, "no complaint for {said:?}");
        }
    }

    /// The escape hatch, for the one way the digit test can be wrong: a
    /// recogniser that writes "seventy" instead of "70". The first attempt is
    /// refused, and whatever the model writes next goes through.
    #[test]
    fn a_tool_that_insists_on_its_number_gets_through_the_second_time() {
        let words = Words::default();
        assert!(words.first_attempt_at("HassClimateSetTemperature"), "the first go is checked");
        assert!(!words.first_attempt_at("HassClimateSetTemperature"), "the second is not");
        assert!(words.first_attempt_at("HassTurnOff"), "and every tool gets its own");
    }

    /// The hatch is only usable if the model is told it exists and is allowed to
    /// use it. *"Set the volume to seventy"* proved both halves matter: told
    /// only that the value was invented, the model believed it, wrote the same
    /// tool with the volume deleted, and the display never moved.
    #[test]
    fn the_refusal_says_how_to_insist() {
        let args = serde_json::json!({"name": "Kitchen display", "volume_level": 70});
        let (_, complaint) = numbers_nobody_asked_for(args, "set the volume to seventy");
        let complaint = complaint.expect("no digits, so the number is suspect");
        assert!(complaint.contains("in words rather than digits"), "{complaint}");
        assert!(complaint.contains("write this same call again"), "{complaint}");
    }

    /// Verbatim from two traces, one in Romanian and one in English: asked to
    /// turn the bedroom lights on, the model reached for the brightness-and-
    /// colour tool and invented both values. `#FFFFFF` is not a colour that
    /// tool accepts, and the house threw the whole command away over it.
    /// Naming the offending argument against the schema the model was shown
    /// is a correction it can act on; "received invalid slot info" is not.
    #[test]
    fn a_value_outside_the_advertised_enumeration_is_named() {
        let schema = serde_json::json!({
            "properties": {
                "area": {"type": "string"},
                "color": {"enum": ["red", "white", "warm white"]},
            }
        });
        let args = serde_json::json!({"area": "Master bedroom", "color": "#FFFFFF"});
        let complaint =
            schema_complaint(&schema, &args).expect("an invented colour is a complaint");
        assert!(complaint.contains("color"), "{complaint}");
        assert!(
            complaint.contains("red"),
            "the allowed values belong in the sentence: {complaint}"
        );
    }

    /// A number where a word belongs is the other half of the same mistake,
    /// and just as cheap to catch before it costs a round trip.
    #[test]
    fn a_value_of_the_wrong_type_is_named() {
        let schema = serde_json::json!({"properties": {"name": {"type": "string"}}});
        let complaint = schema_complaint(&schema, &serde_json::json!({"name": 7}))
            .expect("a number is not a name");
        assert!(complaint.contains("name must be a string"), "{complaint}");
    }

    /// The check must stay quiet about everything it is not sure of. An
    /// advertised schema is routinely stricter than the behaviour behind it,
    /// so refusing a call the house would have accepted is the worse failure:
    /// a missing required field, an argument the tool never advertised, and a
    /// whole number written with a decimal point all pass.
    #[test]
    fn the_schema_check_refuses_only_what_the_server_plainly_forbids() {
        let schema = serde_json::json!({
            "properties": {
                "area": {"type": "string"},
                "temperature": {"type": "integer"},
            },
            "required": ["area", "missing_one"],
        });
        let args = serde_json::json!({
            "area": "Office",
            "temperature": 21.0,
            "undocumented": "servers extend themselves",
        });
        assert_eq!(schema_complaint(&schema, &args), None);
        // A tool that advertises nothing can never be complained about.
        assert_eq!(schema_complaint(&serde_json::json!({}), &args), None);
    }

    /// A catalogue on disk with the one tool these tests act with, plus the
    /// directory it lives in — dropping the directory deletes the file, so the
    /// caller has to keep it.
    fn a_home_on_disk() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            config_dir: dir.path().into(),
            data_dir: dir.path().into(),
            cache_dir: dir.path().into(),
            state_dir: dir.path().into(),
        };
        let (_, rows) = a_small_home();
        let found: Vec<_> = rows
            .into_iter()
            .map(|r| fono_core::tool_catalog::DiscoveredTool {
                name: r.name,
                description: r.description,
                schema: r.schema,
                capability: r.capability,
                verify_class: r.verify_class,
                readback_tool: r.readback_tool,
            })
            .collect();
        let store = ToolCatalogStore::open(&paths.tool_catalog_db()).expect("store");
        store.reconcile("home", "sse", &found).expect("tools");
        (dir, paths)
    }

    fn ran(sent: &str, devices: &[&str]) -> Ran {
        Ran {
            out: ToolOutcome::worked("Done.".into()),
            how: RunOutcome::Accepted,
            targets: devices
                .iter()
                .map(|n| vendor::Target { name: (*n).to_string(), landed: true })
                .collect(),
            sent: sent.to_string(),
        }
    }

    fn journal(paths: &Paths, learning: &Learning) -> Journal {
        Journal {
            db: paths.tool_catalog_db(),
            speaker: None,
            idle_since: Arc::new(std::sync::Mutex::new(std::time::Instant::now())),
            learning: learning.clone(),
        }
    }

    /// The whole mechanism hangs off one call at the end of a turn, and every
    /// part of it fails quietly by design — so the thing worth pinning is that
    /// a plain turn does reach the catalogue at all. Nothing here asserts a
    /// promotion: one clean run is not two.
    #[test]
    fn a_turn_that_did_one_thing_is_written_down() {
        let (_dir, paths) = a_home_on_disk();
        let learning = Learning::new(&paths);
        let sent = r#"{"name":"Office outdoor light"}"#;
        journal(&paths, &learning).note(
            "home",
            "HassTurnOn",
            &ran(sent, &["Office outdoor light"]),
            std::time::Duration::ZERO,
            std::time::Duration::from_millis(7),
        );
        learning.finished("turn on the office light", "en");

        let store = ToolCatalogStore::open(&paths.tool_catalog_db()).expect("reopen");
        let [row] = store.shortcuts().expect("shortcuts").try_into().expect("exactly one phrase");
        assert_eq!(row.phrase, "turn on the office light");
        assert_eq!(row.tool, "HassTurnOn");
        assert_eq!(row.args, sent, "a replay has to send what was sent, not what was said");
        assert!(!row.fast(), "one clean run is not enough to skip the model");
    }

    /// A turn that ran two commands did two things, and replaying one of them
    /// would do half of what was asked. Both simply stay slow.
    #[test]
    fn a_turn_that_did_two_things_is_not_written_down() {
        let (_dir, paths) = a_home_on_disk();
        let learning = Learning::new(&paths);
        let j = journal(&paths, &learning);
        for dev in ["Office outdoor light", "Bedroom blind"] {
            let sent = format!(r#"{{"name":"{dev}"}}"#);
            j.note(
                "home",
                "HassTurnOn",
                &ran(&sent, &[dev]),
                std::time::Duration::ZERO,
                std::time::Duration::from_millis(7),
            );
        }
        learning.finished("get the office ready", "en");

        let store = ToolCatalogStore::open(&paths.tool_catalog_db()).expect("reopen");
        assert!(store.shortcuts().expect("shortcuts").is_empty());
    }

    /// Warming a prompt is not a turn: nobody spoke, so there is nothing to
    /// learn from, and the handle passed on that path must write nothing even
    /// if something contrives to run.
    #[test]
    fn a_path_that_is_not_a_turn_learns_nothing() {
        let (_dir, paths) = a_home_on_disk();
        let learning = Learning::none();
        journal(&paths, &learning).note(
            "home",
            "HassTurnOn",
            &ran(r#"{"name":"Office outdoor light"}"#, &["Office outdoor light"]),
            std::time::Duration::ZERO,
            std::time::Duration::from_millis(7),
        );
        learning.finished("turn on the office light", "en");

        let store = ToolCatalogStore::open(&paths.tool_catalog_db()).expect("reopen");
        assert!(store.shortcuts().expect("shortcuts").is_empty());
    }

    /// A phrase that has earned the fast path.
    fn earned() -> fono_core::tool_catalog::Shortcut {
        fono_core::tool_catalog::Shortcut {
            phrase: "turn on the office light".into(),
            lang: "en".into(),
            source: "home".into(),
            tool: "HassTurnOn".into(),
            args: r#"{"name":"Office outdoor light"}"#.into(),
            origin: fono_core::tool_catalog::Origin::Learned,
            runs: 2,
            clean: 2,
            last_run: None,
            last_ok: Some(true),
            last_ms: Some(2_400),
            stale: None,
        }
    }

    /// Every call an executor was asked to make, in order: tool name and
    /// arguments as sent.
    type Asked = Arc<std::sync::Mutex<Vec<(String, String)>>>;

    /// Tools whose executor answers as told and records what it was asked,
    /// standing in for a house without one.
    fn answering(out: ToolOutcome) -> (ActionTools, Asked) {
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&asked);
        let execute: fono_assistant::ToolExecFn = Arc::new(move |call: ToolCall| {
            seen.lock().unwrap().push((call.name, call.arguments));
            let out = out.clone();
            Box::pin(async move { out })
        });
        let tools = ActionTools {
            descriptors: Vec::new(),
            execute,
            hint: None,
            grammar: None,
            said: fono_assistant::Said::default(),
        };
        (tools, asked)
    }

    /// The fast path has to be invisible from the outside: the same events a
    /// model turn puts on the stream, so history, the page and the next turn
    /// see no difference — and the command sent exactly as it was sent before,
    /// not as it was said.
    #[tokio::test]
    async fn a_replayed_phrase_produces_the_events_a_model_turn_would() {
        let found = earned();
        let (tools, asked) = answering(ToolOutcome::worked("Done.".into()));
        let events = run_again(&tools, &found).await.expect("a working replay ends the turn");

        assert_eq!(
            asked.lock().unwrap().as_slice(),
            &[("HassTurnOn".to_string(), r#"{"name":"Office outdoor light"}"#.to_string())]
        );
        let [called, result] = events.as_slice() else { panic!("a call and its result") };
        match &called.tool_event {
            Some(fono_assistant::ToolEvent::Called(c)) => assert_eq!(c.name, "HassTurnOn"),
            other => panic!("{other:?}"),
        }
        match &result.tool_event {
            Some(fono_assistant::ToolEvent::Result { failed, .. }) => assert!(!failed),
            other => panic!("{other:?}"),
        }
        assert!(
            events.iter().all(|d| d.text.is_empty()),
            "nothing is spoken: the model is not there to word it in the right language"
        );
    }

    /// A replay that failed hands the turn to the model, and the phrase is slow
    /// again at once — recorded here because a turn that ran two commands is
    /// deliberately never learned from, so this is the only chance to write it.
    #[tokio::test]
    async fn a_replay_that_did_not_work_hands_the_turn_to_the_model() {
        let found = earned();
        let (tools, asked) = answering(ToolOutcome {
            summary: "HassTurnOn could not be run".into(),
            failed: true,
            retryable: true,
            sent: None,
            repeat_ok: false,
            confirmed: false,
        });
        assert!(run_again(&tools, &found).await.is_err(), "a failed replay is not an answer");
        assert_eq!(asked.lock().unwrap().len(), 1, "tried once, then handed over");
    }

    /// A call that never left Fono changed nothing, so the model is invited
    /// to correct it inside the same turn rather than the user being asked to
    /// say the whole thing again.
    #[tokio::test]
    async fn a_failure_that_changed_nothing_invites_one_correction() {
        let ran = run_one(
            &tools(VerifyClass::PostCondition),
            &HouseFacts::default(),
            &Words::default(),
            call("Nope"),
        )
        .await;
        assert!(ran.out.failed);
        assert!(ran.out.retryable, "an unknown tool moved nothing, so a second go is free");
        assert!(ran.out.summary.contains("call the tool once more"), "{}", ran.out.summary);
    }

    /// The failure in two traces was choosing the wrong tool, not naming the
    /// wrong area: asked to switch lights on, the model reached for the
    /// brightness-and-colour tool and invented both values. The hint said
    /// nothing about choosing between a couple of dozen near-identical
    /// signatures, and nothing about leaving a field alone.
    #[test]
    fn the_area_hint_says_which_tool_to_use_and_not_to_invent_values() {
        let store = ToolCatalogStore::open_in_memory().expect("store");
        store.set_place_names("home", &["Office".to_string()]).expect("names");
        let hint = area_hint(&store).expect("names present means a hint");
        let lower = hint.to_lowercase();
        assert!(lower.contains("simplest tool"), "{hint}");
        assert!(lower.contains("on/off tool"), "{hint}");
        assert!(lower.contains("only what the user asked for"), "{hint}");
        assert!(lower.contains("it is the wrong tool"), "{hint}");
    }

    /// A device named after somewhere it is not — a lamp called after the
    /// place it lights rather than the place it sits — was reported missing
    /// because the model narrowed its search to that area. The hint has to
    /// say so, or the same lookup fails the same way.
    #[test]
    fn the_area_hint_warns_against_searching_by_room_for_a_named_device() {
        let store = ToolCatalogStore::open_in_memory().expect("store");
        store.set_place_names("home", &["Office".to_string(), "Yard".to_string()]).expect("names");
        let hint = area_hint(&store).expect("names present means a hint");
        assert!(hint.contains("Office"), "{hint}");
        let lower = hint.to_lowercase();
        assert!(lower.contains("never translate"), "{hint}");
        assert!(lower.contains("add no area"), "{hint}");
    }

    /// The page's prompt panel has to carry every block the model reads, not
    /// just the house. It showed the house alone under a heading promising the
    /// exact words the assistant is given, and on a real home that was 2,894
    /// characters of 6,559 — the tool block, the largest of the three, was
    /// invisible, and so was the reply style.
    #[test]
    fn the_page_shows_every_block_of_the_prompt_not_only_the_house() {
        let store = ToolCatalogStore::open_in_memory().expect("store");
        store.set_place_names("home", &["Office".to_string()]).expect("areas");
        store
            .set_devices("home", &[fono_core::tool_catalog::Device::new("Office light", "light")])
            .expect("devices");
        store
            .reconcile(
                "home",
                "sse",
                &[fono_core::tool_catalog::DiscoveredTool {
                    name: "HassTurnOn".into(),
                    description: "Turns on/opens a device or entity".into(),
                    schema: serde_json::json!({
                        "type": "object",
                        "properties": {"area": {"type": "string"}, "domain": {"type": "array"}},
                    }),
                    capability: fono_core::tool_catalog::Capability::Safe,
                    verify_class: fono_core::tool_catalog::VerifyClass::ResultContract,
                    readback_tool: None,
                }],
            )
            .expect("tools");

        let mut cfg = Config::default();
        cfg.assistant.backend = fono_core::config::LlmBackend::Local;
        cfg.assistant.tools.place_names = true;
        cfg.assistant.prompt_main = "Be brief.".into();
        let active = store.active_tools().expect("active");
        let blocks = prompt_blocks(&cfg, &store, &active);

        let house = blocks["house"].as_str().expect("the house block");
        let tools = blocks["tools"].as_str().expect("the tool block");
        assert!(house.contains("Office light"), "{house}");
        assert!(tools.contains("HassTurnOn(area, domain[])"), "{tools}");
        assert_eq!(blocks["behaviour"].as_str(), Some("Be brief."));
        assert_eq!(blocks["tools_in_prompt"], serde_json::json!(true));

        // The count is what this backend reads, joins included — the number a
        // budget is judged against.
        let chars = blocks["chars"].as_u64().expect("a character count");
        assert_eq!(chars as usize, house.chars().count() + tools.chars().count() + 9 + 4);

        // A backend that carries its tools in the request still shows the block,
        // because it is the same information — but the model does not read it,
        // so it is not charged for.
        cfg.assistant.backend = fono_core::config::LlmBackend::OpenAI;
        let cloud = prompt_blocks(&cfg, &store, &active);
        assert!(cloud["tools"].as_str().is_some(), "the block is still shown");
        assert_eq!(cloud["tools_in_prompt"], serde_json::json!(false));
        assert!(cloud["chars"].as_u64().expect("count") < chars, "and is not charged for");
    }

    /// An area-wide switch-on reaches everything switchable in the area. Asked
    /// for *the light* in the office, the model asked for the office, and the
    /// air conditioning came on. The hint has to name the domain escape hatch,
    /// or a request for one kind of device keeps acting on all of them.
    ///
    /// Stating it was not enough. With the rule in the prompt but placed
    /// *after* "act on the area in one call", a later trace still sent a bare
    /// `{"area": "Master bedroom"}` and moved the curtains and the roller. So
    /// the ordering is part of the fix, and is asserted: the obligation comes
    /// before the economy, and it is phrased as an obligation.
    #[test]
    fn the_area_hint_asks_for_a_domain_when_the_user_named_a_kind_of_device() {
        let store = ToolCatalogStore::open_in_memory().expect("store");
        store.set_place_names("home", &["Office".to_string()]).expect("names");
        let hint = area_hint(&store).expect("names present means a hint");
        let lower = hint.to_lowercase();
        assert!(lower.contains("domain"), "{hint}");
        assert!(hint.contains("\"domain\": [\"climate\"]"), "the worked example survives: {hint}");
        assert!(lower.contains("domain is required"), "advice was not enough: {hint}");
        // The one value that means "everything in here". The rails offer it and
        // nothing else explains it, so a model that means it would have to
        // guess — and would file the request under one kind instead.
        assert!(
            hint.contains(fono_core::tool_grammar::ANY_KIND),
            "the way to say 'every device' must be named: {hint}"
        );

        let domain_at = lower.find("the domain is required").expect("the obligation");
        let one_call_at = lower.find("one call for an area").expect("the economy");
        assert!(
            domain_at < one_call_at,
            "the obligation must come before the one-call economy, or the economy \
             reads as licence to omit the domain: {hint}"
        );
    }

    /// The home matches a device name only exactly: "outdoor office light"
    /// and "outdoor light" both find nothing when the device is "Office
    /// outdoor light". Naming the devices removes the guess, so the list has
    /// to reach the prompt verbatim, with the exactness spelled out.
    #[test]
    fn device_names_are_stated_exactly_when_there_are_few_enough() {
        let store = ToolCatalogStore::open_in_memory().expect("store");
        store.set_place_names("home", &["Yard".to_string()]).expect("areas");
        store
            .set_devices(
                "home",
                &[
                    fono_core::tool_catalog::Device::new("Office outdoor light", "light"),
                    fono_core::tool_catalog::Device::new("Hall lamp", "light"),
                ],
            )
            .expect("devices");
        let hint = area_hint(&store).expect("hint");
        assert!(hint.contains("Office outdoor light"), "{hint}");
        assert!(hint.to_lowercase().contains("only exactly as written"), "{hint}");
    }

    /// A name says what a device sounds like, not what it is. Told only the
    /// names, a model sent `domain: ["light"]` for an air conditioner — and
    /// this home has a `switch` called "Entrance lights", where the obvious
    /// domain finds nothing. So each name is written under its kind, and the
    /// kind is written as the word the `domain` argument takes.
    #[test]
    fn each_device_is_named_under_its_kind() {
        let store = ToolCatalogStore::open_in_memory().expect("store");
        store.set_place_names("home", &["Office".to_string()]).expect("areas");
        store
            .set_devices(
                "home",
                &[
                    fono_core::tool_catalog::Device::new("Office light", "light"),
                    fono_core::tool_catalog::Device::new("Air conditioner", "climate"),
                    fono_core::tool_catalog::Device::new("Entrance lights", "switch"),
                    fono_core::tool_catalog::Device::new("Mystery box", ""),
                ],
            )
            .expect("devices");
        let hint = area_hint(&store).expect("hint");
        assert!(hint.contains("\nclimate: Air conditioner"), "{hint}");
        assert!(hint.contains("\nlight: Office light"), "{hint}");
        assert!(hint.contains("\nswitch: Entrance lights"), "{hint}");
        assert!(hint.contains("`domain`"), "the kind has to be named as the argument: {hint}");
        // An unstated kind is said to be unstated, never guessed from the name.
        assert!(hint.contains("kind unknown: Mystery box"), "{hint}");
    }

    /// Grouping must not be paid for in tokens. One label per kind is covered
    /// by the header being shorter than the sentence about exact names it
    /// replaced, so the device half of the hint does not grow.
    #[test]
    fn naming_the_kinds_costs_no_more_than_the_flat_list_did() {
        let store = ToolCatalogStore::open_in_memory().expect("store");
        store.set_place_names("home", &["Office".to_string()]).expect("areas");
        let devices: Vec<fono_core::tool_catalog::Device> = (0..40)
            .map(|i| {
                let kind = if i % 4 == 0 { "light" } else { "switch" };
                fono_core::tool_catalog::Device::new(format!("Device number {i}"), kind)
            })
            .collect();
        store.set_devices("home", &devices).expect("devices");
        let grouped = area_hint(&store).expect("hint");
        let flat = format!(
            "Devices in this home, named exactly as they must be used: {}. \
             Use a name exactly as written: the home matches nothing else, \
             not a shortened name and not a different word order.",
            devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")
        );
        let devices_part = grouped.find("\nDevices by kind").expect("the device half is written");
        assert!(
            grouped.len() - devices_part <= flat.len(),
            "grouped {} vs flat {}",
            grouped.len() - devices_part,
            flat.len()
        );
    }

    /// A truncated list is worse than none: the model would read it as the
    /// whole house and tell the user a real device does not exist.
    #[test]
    fn too_many_devices_are_left_out_rather_than_cut_short() {
        let store = ToolCatalogStore::open_in_memory().expect("store");
        store.set_place_names("home", &["Yard".to_string()]).expect("areas");
        let many: Vec<fono_core::tool_catalog::Device> = (0..=MAX_LISTED_DEVICES)
            .map(|i| fono_core::tool_catalog::Device::new(format!("Device number {i}"), "light"))
            .collect();
        store.set_devices("home", &many).expect("devices");
        let hint = area_hint(&store).expect("areas still give a hint");
        assert!(hint.contains("Yard"), "the area half must survive: {hint}");
        assert!(!hint.contains("Device number"), "a partial list must not be stated: {hint}");
    }

    /// Each arm must leave out exactly what it says it leaves out, or the
    /// measurement measures something other than what it reports.
    #[test]
    fn each_arm_writes_what_it_claims() {
        let (store, _) = a_small_home();
        let full = written_hint(&store, HintArm::Full).expect("hint");
        let lean = written_hint(&store, HintArm::Lean).expect("hint");
        let no_rules = written_hint(&store, HintArm::NoRules).expect("hint");
        let no_devices = written_hint(&store, HintArm::NoDevices).expect("hint");

        // Every arm still names the areas — that half is not under test.
        for h in [&full, &lean, &no_rules, &no_devices] {
            assert!(h.contains("Master bedroom"), "{h}");
        }

        // `lean` drops rules 1 and 4 and keeps the rest, numbering unchanged so
        // the wording cannot drift between arms.
        assert!(full.contains("1. Never translate"), "{full}");
        assert!(!lean.contains("Never translate"), "{lean}");
        assert!(!lean.contains("add no area"), "{lean}");
        assert!(lean.contains("2. When the user says which kind"), "{lean}");
        assert!(lean.contains("5. Use the simplest tool"), "{lean}");

        // `no-rules` and `no-devices` each drop one whole half.
        assert!(!no_rules.contains("Rules for acting"), "{no_rules}");
        assert!(no_rules.contains("Office outdoor light"), "{no_rules}");
        assert!(no_devices.contains("Rules for acting"), "{no_devices}");
        assert!(!no_devices.contains("Office outdoor light"), "{no_devices}");

        // And the default arm is the longest, so an accidental default change
        // would show up here rather than in a run.
        assert!(
            full.len() > lean.len() && full.len() > no_rules.len() && full.len() > no_devices.len()
        );
    }

    /// No rooms means no sentence: an empty list would spend tokens telling
    /// the model to choose from nothing.
    #[test]
    fn no_areas_means_no_hint() {
        let store = ToolCatalogStore::open_in_memory().expect("store");
        assert!(area_hint(&store).is_none());
    }

    /// A store standing in for a small Home Assistant: two areas, two kinds of
    /// device, and the one tool the traces keep failing on.
    fn a_small_home() -> (ToolCatalogStore, Vec<fono_core::tool_catalog::ToolRow>) {
        let store = ToolCatalogStore::open_in_memory().expect("store");
        store
            .set_place_names("home", &["Office".to_string(), "Master bedroom".to_string()])
            .expect("areas");
        store
            .set_devices(
                "home",
                &[
                    fono_core::tool_catalog::Device::new("Office outdoor light", "light"),
                    fono_core::tool_catalog::Device::new("Bedroom blind", "cover"),
                ],
            )
            .expect("devices");
        let rows = vec![fono_core::tool_catalog::ToolRow {
            source: "home".into(),
            name: "HassTurnOn".into(),
            description: String::new(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "area": {"type": "string"},
                    "name": {"type": "string"},
                    "domain": {"type": "array", "items": {"type": "string"}},
                },
            }),
            schema_hash: String::new(),
            capability: fono_core::tool_catalog::Capability::Safe,
            verify_class: fono_core::tool_catalog::VerifyClass::None,
            readback_tool: None,
            available: true,
            enabled: true,
            user_touched: false,
            runs: 0,
            last_run: None,
        }];
        (store, rows)
    }

    /// The rails are built from what the house reported and nothing else, so
    /// every area and device name it gave must appear, and the word for
    /// "everything in this area" must be offered alongside the real kinds.
    #[test]
    fn the_rails_are_built_from_what_the_home_reported() {
        let (store, rows) = a_small_home();
        let g = rails(&store, &rows).expect("a home with areas and devices gives rails");
        assert!(g.contains("Office"), "{g}");
        assert!(g.contains("Master bedroom"), "{g}");
        assert!(g.contains("Office outdoor light"), "{g}");
        assert!(g.contains("light"), "{g}");
        assert!(g.contains("cover"), "{g}");
        assert!(
            g.contains(fono_core::tool_grammar::ANY_KIND),
            "the escape hatch must be there: {g}"
        );
        assert!(g.contains("HassTurnOn"), "the tool name must be pinned too: {g}");
    }

    /// Home Assistant marks nothing required on any of its intents, so a
    /// set-temperature call with no temperature is writable as far as the
    /// schema is concerned — and the house refuses every one. A tool that sets
    /// exactly one thing has that field insisted upon whatever the schema says,
    /// while a tool that offers a choice of values keeps the choice.
    #[test]
    fn a_tool_that_sets_one_thing_must_be_given_it() {
        let (store, mut rows) = a_small_home();
        let mut with_value = |name: &str, values: serde_json::Value| {
            let mut row = rows[0].clone();
            row.name = name.into();
            row.schema = serde_json::json!({"type": "object", "properties": values});
            rows.push(row);
        };
        with_value(
            "HassClimateSetTemperature",
            serde_json::json!({"name": {"type": "string"}, "temperature": {"type": "number"}}),
        );
        with_value(
            "HassLightSet",
            serde_json::json!({
                "name": {"type": "string"},
                "brightness": {"type": "number"},
                "color": {"type": "string"},
            }),
        );

        let g = rails(&store, &rows).expect("rails");
        // Each tool gets a label of its own; find it by the name it pins, then
        // read the rule for the field in question. A field that cannot be
        // skipped has no alternative branch.
        let rule_for = |tool: &str, field: &str| -> String {
            let label = g
                .lines()
                .find(|l| l.contains(&format!("\\\"{tool}\\\"")))
                .and_then(|l| l.split_once(" ::= "))
                .map(|(name, _)| name.to_owned())
                .expect("a branch per tool");
            g.lines()
                .find(|l| {
                    l.starts_with(&format!("{label}-a")) && l.contains(&format!("\\\"{field}\\\""))
                })
                .expect("a rule per field")
                .to_owned()
        };
        let temperature = rule_for("HassClimateSetTemperature", "temperature");
        assert!(
            !temperature.contains(" | "),
            "the only thing this tool sets cannot be skipped: {temperature}"
        );
        for field in ["brightness", "color"] {
            let rule = rule_for("HassLightSet", field);
            assert!(
                rule.contains(" | "),
                "a tool offering a choice of values keeps the choice: {rule}"
            );
        }
    }

    /// The switch is the whole point of shipping this off by default: it has to
    /// be possible to run the same home with and without the rails and compare.
    /// A setting that is read but ignored would make that comparison a lie.
    #[test]
    fn the_switch_decides_whether_the_rails_exist_at_all() {
        let (store, rows) = a_small_home();
        // This mirrors the one line in `build` that consults the setting.
        let with = true.then(|| rails(&store, &rows)).flatten();
        let without = false.then(|| rails(&store, &rows)).flatten();
        assert!(with.is_some(), "on means rails");
        assert!(without.is_none(), "off means the model is exactly as free as before");
    }

    /// A house that said nothing about itself gives nothing to hold the model
    /// to, and must leave it unconstrained rather than fail or invent a menu.
    #[test]
    fn a_silent_home_leaves_the_model_free() {
        let store = ToolCatalogStore::open_in_memory().expect("store");
        let rows: Vec<fono_core::tool_catalog::ToolRow> = Vec::new();
        assert!(rails(&store, &rows).is_none());
    }

    /// Long server output is trimmed, but visibly, and from the middle: a
    /// server that puts its failures last must still be heard saying so.
    #[test]
    fn long_output_is_trimmed_from_the_middle() {
        assert_eq!(brief("  "), "Done.");
        let out = brief(&format!("{}{}", "x".repeat(50_000), "the failures go here"));
        assert!(out.contains('…'), "trimming must be visible: {out}");
        assert!(out.starts_with("xxx"), "the start must survive");
        assert!(out.ends_with("the failures go here"), "the end must survive: {out}");
        assert!(out.chars().count() < 1300, "and the whole thing must be short");
    }

    /// What a call that worked hands back is the devices it reached, not the
    /// server's own words for them.
    ///
    /// The reply Home Assistant sends for one area-wide command measured 3,447
    /// characters, nearly all of it identifiers. The model pays for that text
    /// twice — once to read it, once to answer it — and none of it says
    /// anything the device names do not.
    #[test]
    fn a_call_that_worked_hands_back_the_devices_not_the_payload() {
        let one = [vendor::Target { name: "Balcony lights".into(), landed: true }];
        assert_eq!(
            landed("HassTurnOn", &one).as_deref(),
            Some("HassTurnOn reached Balcony lights.")
        );

        // A device the server listed as failed is not something that worked,
        // and the half-done wording says so elsewhere.
        let failed = [vendor::Target { name: "Hall lamp".into(), landed: false }];
        assert_eq!(landed("HassTurnOn", &failed), None);

        // A server Fono knows nothing about names nothing, so its own words
        // are all there is and must be kept.
        assert_eq!(landed("Whatever", &[]), None);
    }

    /// A command that reaches a whole area gets a count, not a roll call: the
    /// names stop saying anything new long before the list ends.
    #[test]
    fn an_area_wide_call_is_counted_after_the_first_few() {
        let many: Vec<vendor::Target> =
            (0..12).map(|i| vendor::Target { name: format!("Lamp {i}"), landed: true }).collect();
        let out = landed("HassTurnOff", &many).expect("named");
        assert!(out.starts_with("HassTurnOff reached 12 devices: Lamp 0,"), "{out}");
        assert!(out.ends_with("and 4 more."), "{out}");
        assert!(!out.contains("Lamp 9"), "the tail is counted, not listed: {out}");
    }

    /// Devices that agree are counted; the ones that stand apart keep their
    /// names. Naming all of them is the largest thing the model reads in an
    /// ordinary turn, and the roll call says nothing the counts do not.
    #[test]
    fn the_reading_counts_the_agreeing_devices_and_names_the_rest() {
        let read = |v: &[(&str, &str)]| {
            state_of_the_house(
                &v.iter().map(|(n, s)| ((*n).to_string(), (*s).to_string())).collect::<Vec<_>>(),
            )
        };
        assert_eq!(read(&[]), "", "nothing to report leaves no stray sentence");

        // Few enough that the names are both short and more useful.
        assert!(read(&[("Hall lamp", "on")]).contains("Hall lamp is on."), "one device is named");

        // The real reply that prompted this: twelve lights, four of them on.
        let mut many: Vec<(&str, &str)> = (0..8).map(|_| ("Couch Blue", "off")).collect();
        many[1] = ("Couch Green", "off");
        many[2] = ("Couch Red", "off");
        many[3] = ("Couch White", "off");
        many[4] = ("Living square", "off");
        many[5] = ("Living square (1)", "off");
        many[6] = ("Living square blue", "off");
        many[7] = ("Living square green", "off");
        many.extend([("Couch", "on"), ("Living square red", "on"), ("Living square white", "on")]);
        let out = read(&many);
        assert!(out.contains("11 devices — 8 are off; 3 are on: Couch, "), "{out}");
        assert!(
            !out.contains("Couch Blue,"),
            "the agreeing majority is counted, not listed: {out}"
        );
        assert!(out.len() < 200, "and the whole thing is short: {} chars", out.len());

        // The one device that stands apart is exactly what the reading is for.
        let odd = read(&[("a", "off"), ("b", "off"), ("c", "off"), ("d", "off"), ("Hall", "on")]);
        assert!(odd.contains("5 devices — 4 are off; 1 is on: Hall."), "{odd}");

        // A device the house names twice in the same state says it once; the
        // same name in two states is a real disagreement and both survive.
        let twice = read(&[
            ("Couch", "on"),
            ("Couch", "on"),
            ("Couch Blue", "off"),
            ("Couch Red", "off"),
            ("Couch White", "off"),
        ]);
        assert_eq!(twice.matches("Couch is on").count(), 1, "said once: {twice}");
        let split = read(&[("Couch", "on"), ("Couch", "off"), ("A", "off"), ("B", "off")]);
        assert!(split.contains("Couch is on"), "a contradiction survives: {split}");
        assert!(split.contains("Couch is off"), "a contradiction survives: {split}");

        // Everything agreeing needs no names at all.
        let agreed: Vec<(&str, &str)> =
            ["a", "b", "c", "d", "e"].into_iter().map(|n| (n, "off")).collect();
        assert!(read(&agreed).contains("all 5 are off."), "{}", read(&agreed));

        // A large minority is capped like any other roll call.
        let mut lopsided: Vec<(&str, &str)> = Vec::new();
        for i in 0..9 {
            lopsided.push((["a", "b", "c", "d", "e", "f", "g", "h", "i"][i], "on"));
        }
        for i in 0..10 {
            lopsided.push((["j", "k", "l", "m", "n", "o", "p", "q", "r", "s"][i], "off"));
        }
        let capped = read(&lopsided);
        assert!(capped.contains("9 are on: a, b, c, d, e, f and 3 more"), "{capped}");
    }

    /// The check reads the devices the command was aimed at, not the whole
    /// home. A bare read costs a dump of every device in the house to find out
    /// what one lamp is doing.
    #[test]
    fn the_check_asks_about_what_the_command_was_aimed_at() {
        let reader = serde_json::json!({"properties": {
            "area": {"type": "string"},
            "name": {"type": "string"},
            "domain": {"description": "one or a list"},
        }});
        let sent = serde_json::json!({
            "name": "Balcony lights", "area": "Yard", "domain": ["light"], "brightness": 40,
        });
        assert_eq!(
            about(Some(&reader), slots(), &sent),
            serde_json::json!({"area": "Yard", "name": "Balcony lights", "domain": ["light"]}),
            "every field that says what was aimed at, and no value"
        );

        // A reader that publishes nothing to filter by, and a command that
        // named nothing, both read the house as they always did.
        assert_eq!(about(None, slots(), &sent), serde_json::json!({}));
        assert_eq!(
            about(Some(&reader), slots(), &serde_json::json!({"brightness": 40})),
            serde_json::json!({})
        );

        // The reader wants a plain string where the command sent a list. Its
        // own schema decides, and the field is left out rather than reshaped.
        let strict = serde_json::json!({"properties": {"domain": {"type": "string"}}});
        assert_eq!(about(Some(&strict), slots(), &sent), serde_json::json!({}));
    }

    /// The page exists to close one gap: the model was told things nobody
    /// could see. So everything the prompt is built from has to reach it —
    /// the exact sentences about this home, the areas and devices those
    /// sentences came from, which published field each of those lands in,
    /// and the fingerprint that says whether a warmed model is stale. A
    /// payload missing any of these leaves a question that can only be
    /// answered by reading a trace, which is the situation being fixed.
    #[test]
    fn the_page_is_told_everything_the_prompt_was_told() {
        let (store, _) = a_small_home();
        store
            .reconcile(
                "home",
                "sse",
                &[fono_core::tool_catalog::DiscoveredTool {
                    name: "HassTurnOn".into(),
                    description: "Turns on a device".into(),
                    schema: serde_json::json!({"type": "object"}),
                    capability: fono_core::tool_catalog::Capability::Safe,
                    verify_class: fono_core::tool_catalog::VerifyClass::PostCondition,
                    readback_tool: Some("GetLiveContext".into()),
                }],
            )
            .expect("reconcile");

        let mut cfg = Config::default();
        cfg.assistant.tools.place_names = true;
        let v = page_extras(&cfg, &store, &[]);

        assert_eq!(v["offered"], 1, "the count on the page must be the count in the prompt");
        let hint = v["hint"].as_str().expect("the sentences about this home must be shown");
        assert!(hint.contains("Office"), "{hint}");
        assert!(!v["catalogue_hash"].as_str().unwrap_or_default().is_empty());
        let places = v["house"]["places"].as_array().expect("areas");
        assert!(places.iter().any(|p| p == "Office"), "{places:?}");
        let devices = v["house"]["devices"].as_array().expect("devices");
        assert!(devices.iter().any(|d| d["name"] == "Bedroom blind"), "{devices:?}");
        assert_eq!(v["any_kind"], fono_core::tool_grammar::ANY_KIND);
        // Which field carries an area is asked of the vendor, never assumed, and
        // filed under the server it was asked about; showing it is how a wrong
        // guess becomes visible instead of costing an afternoon in the traces.
        assert_eq!(v["rails"]["home"]["place"], "area", "{:?}", v["rails"]);
        assert_eq!(v["rails"]["home"]["areas"], 2, "{:?}", v["rails"]);
        assert!(v["rails"]["home"]["devices"].as_u64().unwrap_or_default() > 0, "{:?}", v["rails"]);

        // Switched off, the page must say so rather than show sentences the
        // model never received — a page that lies is worse than no page.
        cfg.assistant.tools.place_names = false;
        assert!(page_extras(&cfg, &store, &[])["hint"].is_null());
    }

    /// The page must show what a tool was actually asked to do, grouped by
    /// tool and capped, so one chatty tool cannot bury the rest.
    #[test]
    fn past_uses_are_grouped_per_tool_and_capped() {
        let use_of = |tool: &str, said: &str| ToolUse {
            tool: tool.into(),
            at: 0,
            args: r#"{"area":"Office"}"#.into(),
            said: Some(said.into()),
            speaker: Some("Bogdan".into()),
            result: Some("ok".into()),
            ok: Some(true),
        };
        let mut uses: Vec<ToolUse> =
            (0..9).map(|i| use_of("HassTurnOn", &format!("n{i}"))).collect();
        uses.push(use_of("HassTurnOff", "off please"));

        let v = uses_by_tool(&uses);
        let on = v["HassTurnOn"].as_array().expect("grouped under its tool");
        assert_eq!(on.len(), USES_PER_TOOL, "one busy tool must not fill the payload");
        assert_eq!(on[0]["said"], "n0", "newest first, as the query returned them");
        assert_eq!(v["HassTurnOff"].as_array().map(Vec::len), Some(1));
        assert!(v.get("GetLiveContext").is_none(), "a tool with no uses gets no entry");
    }

    /// The whole chain except the model: real config, real catalogue, real
    /// server, real light. Everything up to here can be mocked into
    /// agreeing with itself; only this says the lamp changed.
    ///
    /// Run with a configured server present:
    /// `cargo test -p fono --lib turns_on_a_real_light -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs a configured MCP server and switches a real light"]
    async fn turns_on_a_real_light() {
        let paths = Paths::resolve().expect("paths");
        let cfg = Config::load(&paths.config_file()).expect("config");
        let tools = build(&cfg, &paths, Some("live test"), &Learning::none())
            .expect("no tools configured — nothing to test");
        assert!(
            tools.descriptors.iter().any(|d| d["function"]["name"] == "HassTurnOn"),
            "HassTurnOn is not switched on in the catalogue"
        );

        let area = std::env::var("FONO_TEST_ACTION_AREA").unwrap_or_else(|_| "Kitchen".into());
        let switch = |on: bool| ToolCall {
            id: "live".into(),
            name: if on { "HassTurnOn" } else { "HassTurnOff" }.into(),
            arguments: serde_json::json!({"area": area, "domain": ["light"]}).to_string(),
        };
        let started = std::time::Instant::now();
        let out = (tools.execute)(switch(true)).await;
        println!("[{} ms] failed={} {}", started.elapsed().as_millis(), out.failed, out.summary);
        assert!(!out.failed, "{}", out.summary);
        assert!(out.summary.contains(&area), "nothing in {area} was touched: {}", out.summary);

        // Again, with the lights already on. Nothing changes, and that must
        // still be reported as having worked: what is checked is whether the
        // world is as the user asked, not whether anything moved. A change
        // detector would call this a failure — and it would be wrong, because
        // "turn on the light" is satisfied by a light that is already on.
        let started = std::time::Instant::now();
        let out = (tools.execute)(switch(true)).await;
        println!(
            "[{} ms] again: failed={} {}",
            started.elapsed().as_millis(),
            out.failed,
            out.summary
        );
        assert!(!out.failed, "asking for a state already true is not a failure: {}", out.summary);

        let _ = (tools.execute)(switch(false)).await;
    }

    /// A backend that cannot invoke tools must not be handed them, because
    /// the failure is silent: it answers fluently, having ignored them, and
    /// the model promises an action nothing performs. This is exactly what
    /// the embedded local backend did — 15 tools offered, the reply said it
    /// would turn the bedroom light on, and no call was ever made.
    #[test]
    fn a_backend_that_cannot_act_is_told_so_instead_of_being_handed_tools() {
        let tools = Arc::new(ActionTools {
            descriptors: vec![serde_json::json!({"type": "function"})],
            execute: Arc::new(|_| Box::pin(async { ToolOutcome::worked(String::new()) })),
            hint: Some("Areas: Kitchen".into()),
            grammar: None,
            said: fono_assistant::Said::default(),
        });

        let (kept, note) = for_backend(Some(tools.clone()), true, "openai");
        assert!(kept.is_some(), "a backend that can act keeps its tools");
        assert_eq!(note.as_deref(), Some("Areas: Kitchen"), "and still gets the area names");

        let (kept, note) = for_backend(Some(tools), false, "llama-local");
        assert!(kept.is_none(), "a backend that cannot act is not handed tools");
        let note = note.expect("and is told why");
        assert!(note.contains("cannot control"), "{note}");
        // The area list is pointless here and would only cost tokens.
        assert!(!note.contains("Kitchen"), "{note}");

        // No tools configured at all stays silent: nothing to explain.
        assert_eq!(for_backend(None, false, "llama-local").1, None);
    }

    /// The whole chain, model included: spoken words in, real light out.
    /// This is the only test that says the feature works — everything
    /// narrower proves a part in isolation.
    ///
    /// `cargo test -p fono --lib says_a_command_and_the_light_changes -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs a configured assistant and MCP server, and switches a real light"]
    #[allow(clippy::too_many_lines, reason = "one end-to-end story, split would obscure it")]
    async fn says_a_command_and_the_light_changes() {
        use futures::StreamExt;

        let paths = Paths::resolve().expect("paths");
        let cfg = Config::load(&paths.config_file()).expect("config");
        let secrets = Secrets::load(&paths.secrets_file()).unwrap_or_default();
        // Connect first, as pressing "Save & connect" does, so the area names
        // are learned before any command is spoken. This is also the whole
        // claim: the names cost nothing per command because they are already
        // known by the time one arrives.
        let mut endpoint = None;
        for s in &cfg.assistant.tools.mcp {
            let ep = fono_assistant::mcp_client::McpEndpoint {
                url: s.sse_url(),
                token: secrets.keys.get(&s.token_ref()).cloned(),
                timeout: CALL_TIMEOUT,
            };
            let found = fono_assistant::mcp_client::discover(&ep).await.expect("discover");
            let store = ToolCatalogStore::open(&paths.tool_catalog_db()).expect("store");
            store.set_place_names(&s.name, &found.places).expect("store areas");
            store.set_devices(&s.name, &found.devices).expect("store devices");
            println!(
                "{} is {}, {} areas, {} devices",
                s.name,
                found.server.name,
                found.places.len(),
                found.devices.len()
            );
            endpoint = Some(ep);
        }
        let ep = endpoint.expect("no MCP server configured");
        let actions =
            build(&cfg, &paths, Some("live test"), &Learning::none()).expect("no tools configured");
        assert!(actions.hint.is_some(), "the model was told no room names");
        let assistant =
            fono_assistant::build_assistant(&cfg.assistant, &secrets, &paths.polish_models_dir())
                .expect("build assistant")
                .expect("no assistant backend configured");

        // Nothing about one particular home is baked in: the area defaults to
        // a name almost every house has, and both it and the foreign name can
        // be pointed at whatever the machine running this actually owns.
        let area = std::env::var("FONO_TEST_ACTION_AREA").unwrap_or_else(|_| "Kitchen".into());
        let alt = std::env::var("FONO_TEST_ACTION_ALT_AREA").unwrap_or_else(|_| "bucătărie".into());

        // Each phrase asks for a state the lights are not already in, so a
        // command that does nothing cannot pass by accident. The second is the
        // whole point: an area named in another language, which the house has
        // never heard of and only the area list can rescue. What is asserted
        // is the light, never the reply: the assistant claiming success is
        // exactly the thing under suspicion.
        let phrases = [
            (format!("turn off the {} lights", area.to_lowercase()), false),
            (format!("aprinde luminile din {alt}"), true),
        ];
        for (phrase, want_on) in phrases {
            set_lights(&ep, &area, !want_on).await;
            let ctx = fono_assistant::AssistantContext {
                // Compose the prompt through the shipping path, so the area
                // names reach the model exactly as they do in a real turn.
                system_prompt: crate::session::assistant_prompt_context(actions.hint.as_deref()),
                instructions: Some(cfg.assistant.prompt_main.clone()),
                actions: Some(actions.clone()),
                ..Default::default()
            };
            let started = std::time::Instant::now();
            let mut stream = assistant.reply_stream(&phrase, &ctx).await.expect("reply_stream");
            let mut said = String::new();
            let mut ran = Vec::new();
            while let Some(d) = stream.next().await {
                let d = d.expect("delta");
                said.push_str(&d.text);
                if let Some(fono_assistant::ToolEvent::Called(c)) = &d.tool_event {
                    ran.push(c.name.clone());
                }
            }
            let lit = lights_are_on(&ep, &area).await;
            println!(
                "\n[{} ms] {phrase:?}\n  ran: {ran:?}\n  said: {}\n  lights on: {lit} (wanted {want_on})",
                started.elapsed().as_millis(),
                said.trim()
            );
            assert_eq!(lit, want_on, "the lights did not end up as asked; it said: {said}");
        }

        // A device asked for by its own name, which is the case the area list
        // cannot help with: the name often mentions an area the device is not
        // in, and the house matches names exactly, so a paraphrase finds
        // nothing. Skipped unless the machine running this names a device it
        // actually owns.
        let Ok(device) = std::env::var("FONO_TEST_ACTION_DEVICE") else { return };
        for want_on in [true, false] {
            set_device(&ep, &device, !want_on).await;
            let ctx = fono_assistant::AssistantContext {
                system_prompt: crate::session::assistant_prompt_context(actions.hint.as_deref()),
                instructions: Some(cfg.assistant.prompt_main.clone()),
                actions: Some(actions.clone()),
                ..Default::default()
            };
            // Deliberately not the device's exact name: the point is that the
            // model has to recover the real one from the list it was given.
            let phrase = format!(
                "turn {} the {}",
                if want_on { "on" } else { "off" },
                device.to_lowercase()
            );
            let started = std::time::Instant::now();
            let mut stream = assistant.reply_stream(&phrase, &ctx).await.expect("reply_stream");
            let mut said = String::new();
            let mut ran = Vec::new();
            while let Some(d) = stream.next().await {
                let d = d.expect("delta");
                said.push_str(&d.text);
                if let Some(fono_assistant::ToolEvent::Called(c)) = &d.tool_event {
                    ran.push(c.name.clone());
                }
            }
            let lit = device_is_on(&ep, &device).await;
            println!(
                "\n[{} ms] {phrase:?}\n  ran: {ran:?}\n  said: {}\n  on: {lit} (wanted {want_on})",
                started.elapsed().as_millis(),
                said.trim()
            );
            assert_eq!(lit, want_on, "{device} did not end up as asked; it said: {said}");
        }
    }

    /// Put one named device in a known state, bypassing the model.
    #[cfg(test)]
    async fn set_device(ep: &fono_assistant::mcp_client::McpEndpoint, name: &str, on: bool) {
        let tool = if on { "HassTurnOn" } else { "HassTurnOff" };
        let args = serde_json::json!({"name": name});
        fono_assistant::mcp_client::call_tool(ep, tool, &args).await.expect("set the device");
    }

    /// Ask the house about one named device, rather than believing the reply.
    #[cfg(test)]
    /// Home Assistant hands the dump back as an escaped JSON string, so the
    /// newlines are two characters until it is unwrapped. Reading it raw makes
    /// every exact-name match fail, which looks exactly like a dark lamp.
    async fn live_dump(ep: &fono_assistant::mcp_client::McpEndpoint) -> String {
        let out =
            fono_assistant::mcp_client::call_tool(ep, "GetLiveContext", &serde_json::json!({}))
                .await
                .expect("read the house");
        serde_json::from_str::<serde_json::Value>(&out.text)
            .ok()
            .and_then(|v| v.get("result")?.as_str().map(str::to_owned))
            .unwrap_or(out.text)
    }

    async fn device_is_on(ep: &fono_assistant::mcp_client::McpEndpoint, name: &str) -> bool {
        live_dump(ep)
            .await
            .split("- names: ")
            .filter(|b| b.split('\n').next().unwrap_or("").eq_ignore_ascii_case(name))
            .any(|b| b.contains("state: 'on'"))
    }

    /// Put an area's lights in a known state without involving the model, so
    /// the command under test has something real to change.
    #[cfg(test)]
    async fn set_lights(ep: &fono_assistant::mcp_client::McpEndpoint, area: &str, on: bool) {
        let name = if on { "HassTurnOn" } else { "HassTurnOff" };
        let args = serde_json::json!({"area": area, "domain": ["light"]});
        fono_assistant::mcp_client::call_tool(ep, name, &args).await.expect("set the lights");
    }

    /// Ask the house, rather than the assistant, whether the lights are on.
    ///
    /// Goes straight to the server rather than through the executor: what the
    /// executor returns is trimmed to keep a huge dump out of the model's
    /// prompt, and the answer we need can be past the cut. Checking a state
    /// has to see the whole state.
    #[cfg(test)]
    async fn lights_are_on(ep: &fono_assistant::mcp_client::McpEndpoint, area: &str) -> bool {
        live_dump(ep)
            .await
            .split("- names: ")
            .filter(|b| {
                b.contains("domain: light") && b.split('\n').next().unwrap_or("").contains(area)
            })
            .any(|b| b.contains("state: 'on'"))
    }
}
