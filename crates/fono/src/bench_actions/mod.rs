// SPDX-License-Identifier: GPL-3.0-only
//! Measure whether spoken commands actually work, against the home they will
//! be used in.
//!
//! Three decisions shape everything here.
//!
//! **It runs against the real house, not a simulator.** A simulator would be
//! repeatable and would measure the wrong thing: the difficulty in routing a
//! command comes from the real catalogue size, the real device names, two
//! lamps in different areas called almost the same, a name that mentions a
//! area the device is not in. Recreate the house and you recreate only the
//! parts you already thought of, which are the parts that already work.
//!
//! **It goes through the production turn.** The utterance arrives as text, and
//! from there it is [`crate::assistant::run_assistant_turn`] and
//! [`crate::actions`] with nothing swapped out — the same prompt, the same area
//! hint, the same schema check, the same retry ladder, the same readback. A
//! harness that posted its own request to the model would grade the model;
//! this grades what the user will actually experience.
//!
//! **The fixtures name no device.** They state a requirement — "any light",
//! "an area with a light and something else switchable in it" — and it is
//! resolved against whatever house the suite is pointed at. That is what makes
//! them safe to commit, and it also makes them somebody else's benchmark: a
//! stranger clones the repo and runs it on their home with no configuration at
//! all. Results are split so the shareable half carries verdicts and timings
//! and never a device name; the half that names things stays on disk.
//!
//! Feature-gated and off by default, so the shipped binary carries none of it.

pub mod fixture;
pub mod house;
pub mod language;
pub mod runner;
pub mod turn;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fono_assistant::mcp_client::McpEndpoint;
use fono_core::config::Config;
use fono_core::paths::Paths;
use fono_core::Secrets;

use fixture::{Manifest, Verdict};
use runner::{RunOptions, RunOutcome};
use turn::TurnDriver;

/// How long a device call may take. Matches the assistant's own tool timeout
/// so a call that would fail in conversation fails here too.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Everything the subcommand was asked to do.
///
/// The flags are independent switches on a command line, not states of one
/// machine: tracing while dry-running while filtering to one case is a
/// perfectly ordinary request. Folding them into enums would invent
/// combinations that cannot happen and hide the ones that can.
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Fixture files to run. Defaults to the committed suite.
    pub fixtures: Vec<PathBuf>,
    pub languages: Vec<String>,
    pub repeats: u32,
    pub dry_run: bool,
    pub quiet_hours: bool,
    pub only: Option<String>,
    /// Use a different assistant backend than the configured one, without
    /// touching the user's configuration — comparing five models must not
    /// leave them on the fifth.
    pub backend: Option<String>,
    pub model: Option<String>,
    /// List every entity the house reports and stop.
    ///
    /// Needed to write a fixture that names a real device. The committed
    /// suite never names one — it states requirements — but a local fixture
    /// aimed at one particular lamp has to be written against the name the
    /// house actually uses, and guessing it wastes a run per guess.
    pub show_house: bool,
    /// Write a Chrome trace per turn into the run directory.
    ///
    /// Sets `FONO_ASSISTANT_TRACE` for the process, so the production tracer
    /// does the work — a trace of a benchmark turn is a trace of a real turn.
    /// Off by default because **a trace file is a full transcript**: the
    /// system prompt, every tool schema, and the name of every device in the
    /// home (see [`fono_core::turn_trace::transcript_enabled`]).
    pub trace: bool,
    /// Where to write the results. Defaults to a timestamped directory under
    /// the state directory, which is outside the repository.
    pub out: Option<PathBuf>,
}

/// Run the suite and write both halves of the report.
pub async fn run(mut config: Config, paths: &Paths, args: &Args) -> Result<()> {
    if let Some(b) = &args.backend {
        config.assistant.backend = fono_core::providers::parse_llm_backend(b)
            .with_context(|| format!("`{b}` is not an assistant backend Fono knows"))?;
    }
    if let Some(m) = &args.model {
        set_model(&mut config, m);
    }
    if !config.assistant.tools.enabled || config.assistant.tools.mcp.is_empty() {
        anyhow::bail!(
            "no tool servers are switched on — there is nothing to command. Connect one with \
             `fono tools add` first."
        );
    }

    let secrets = Secrets::load(&paths.secrets_file()).unwrap_or_default();

    // Before the model is loaded: listing the house needs no assistant, and
    // making someone wait for weights to load to read a device list would be
    // an odd way to spend thirty seconds.
    if args.show_house {
        return show_house(&config, &secrets).await;
    }

    let driver = TurnDriver::new(config.clone(), paths.clone(), &secrets)?;
    if !driver.can_run_actions() {
        anyhow::bail!(
            "the {} assistant cannot invoke tools, so every command would fail for the same \
             reason and the numbers would say nothing about routing",
            driver.backend_name()
        );
    }

    let fixtures =
        if args.fixtures.is_empty() { default_fixtures()? } else { args.fixtures.clone() };

    // The run directory has to exist before the first turn, not after the
    // last: traces are written by the turn itself, so there must be somewhere
    // for them to land.
    let dir = args
        .out
        .clone()
        .unwrap_or_else(|| paths.state_dir.join("bench").join("actions").join(stamp()));
    if !args.dry_run {
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        if args.trace {
            // Handing the production tracer a directory is the whole
            // integration — no second trace format, and the waterfall has the
            // `actions` lane in it already.
            std::env::set_var("FONO_ASSISTANT_TRACE", dir.join("traces"));
            println!(
                "Tracing on — {} (a trace is a full transcript)",
                dir.join("traces").display()
            );
        }
    }

    let opts = RunOptions {
        languages: args.languages.clone(),
        repeats: args.repeats,
        dry_run: args.dry_run,
        quiet_hours: args.quiet_hours,
        only: args.only.clone(),
    };

    let mut all = RunOutcome { safe: Vec::new(), detail: Vec::new() };
    // Read the steady part of the prompt before the first case, the way the
    // daemon reads it at startup. A user never pays for that read on a command,
    // so a run that pays for it charges the first cases with time nobody spends.
    if !args.dry_run {
        let started = std::time::Instant::now();
        match driver.warm().await {
            Ok(()) => println!("Prompt read and kept in {} ms", started.elapsed().as_millis()),
            Err(e) => println!("Prompt not kept ({e:#}) — the first cases will be slower"),
        }
    }
    for path in &fixtures {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read fixture {}", path.display()))?;
        let manifest: Manifest =
            toml::from_str(&text).with_context(|| format!("parse fixture {}", path.display()))?;
        let Some(ep) = endpoint_for(&config, &secrets, &manifest.server) else {
            let known: Vec<&str> =
                config.assistant.tools.mcp.iter().map(|s| s.name.as_str()).collect();
            let known = if known.is_empty() { "none".to_string() } else { known.join(", ") };
            println!(
                "\n{} — skipped: no `{}` server configured (available: {})",
                path.display(),
                manifest.server,
                known
            );
            continue;
        };
        // Count what will actually run, not what the file holds, or `--only`
        // prints a header that contradicts the results underneath it.
        let selected = manifest
            .cases
            .iter()
            .filter(|c| args.only.as_ref().is_none_or(|f| c.id.contains(f.as_str())))
            .count();
        if selected == 0 {
            println!("\n{} — no case matches --only", path.display());
            continue;
        }
        println!("\n{} — {selected} cases", path.display());
        let out = runner::run(&manifest, &driver, &ep, &opts).await?;
        all.safe.extend(out.safe);
        all.detail.extend(out.detail);
    }

    if args.dry_run {
        println!("\nDry run — nothing was touched.");
        return Ok(());
    }

    write_reports(&dir, &all, &driver, &config)?;
    print_summary(&all);
    warn_if_unconstrained(&dir);
    println!("\nFull results in {}", dir.display());
    Ok(())
}

/// Say so, loudly, if the rails were rejected during the run.
///
/// A grammar llama.cpp refuses samples exactly as no grammar does, so a run
/// with a broken grammar looks like a run of a worse model. That happened: one
/// whole 22-command measurement was written unconstrained because a field named
/// `device_class` put an underscore in a rule label, and the numbers were within
/// a whisker of being recorded as a regression caused by an unrelated change.
/// The generation itself stamps the outcome rather than the request, so the
/// evidence is already on disk — this only refuses to let it go unread.
fn warn_if_unconstrained(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir.join("traces")) else { return };
    let rejected = entries
        .flatten()
        .filter(|e| {
            std::fs::read_to_string(e.path()).is_ok_and(|t| t.contains(r#""grammar": "rejected""#))
        })
        .count();
    if rejected > 0 {
        println!(
            "\n!! {rejected} traces say the tool-call grammar was rejected, so those commands \
             were written with nothing holding them to this house. Treat these numbers as \
             measuring an unconstrained model, not this change."
        );
    }
}

/// Print every entity the house reports, grouped by area.
///
/// Exists so a local fixture can be written against the names the house
/// actually uses. Deliberately not part of a run: it prints device names,
/// which is precisely what the shareable report is designed never to contain.
async fn show_house(config: &Config, secrets: &Secrets) -> Result<()> {
    for server in &config.assistant.tools.mcp {
        let Some(ep) = endpoint_for(config, secrets, &server.name) else { continue };
        let house = match house::House::read(&ep).await {
            Ok(h) => h,
            Err(e) => {
                println!("{}: could not be read — {e}", server.name);
                continue;
            }
        };
        println!("\n{} — {} entities", server.name, house.entities.len());
        for area in house.areas() {
            println!("\n  {area}");
            for e in house.in_area(&area) {
                let dim = if e.level().is_some() { "  adjustable" } else { "" };
                let safe = if e.safe_to_target() { "" } else { "  [never targeted]" };
                // An outage is the commonest reason a run comes back full of
                // skips, and it is invisible in a list that only prints states.
                let out = if e.is_available() { "" } else { "  [unreachable]" };
                // Aliases are shown because a fixture may name any of them,
                // and a bilingual house is where that matters.
                let also = if e.aliases.is_empty() {
                    String::new()
                } else {
                    format!("  (also {})", e.aliases.join(", "))
                };
                println!(
                    "    {:<40} {:<14} {}{dim}{also}{safe}{out}",
                    e.name,
                    e.domain,
                    e.state.as_deref().unwrap_or("-")
                );
            }
        }
        // An entity in no area is still commandable by name, and a fixture may
        // well want one, so it must not be invisible here.
        let orphans: Vec<_> = house.entities.iter().filter(|e| e.areas.is_empty()).collect();
        if !orphans.is_empty() {
            println!("\n  (no area)");
            for e in orphans {
                println!(
                    "    {:<40} {:<14} {}",
                    e.name,
                    e.domain,
                    e.state.as_deref().unwrap_or("-")
                );
            }
        }
    }
    Ok(())
}

/// Point the chosen backend at a different model.
///
/// Applied to the in-memory copy only. Which field to set depends on the
/// backend, because a local model is a file and a cloud model is a name.
fn set_model(config: &mut Config, model: &str) {
    if config.assistant.backend == fono_core::config::LlmBackend::Local {
        config.assistant.local.model = model.to_string();
    } else {
        config.assistant.cloud.model = model.to_string();
    }
}

/// Where the committed fixtures live.
const FIXTURE_DIR: &str = "tests/fixtures/bench_actions";

/// Where fixtures that name real devices live.
///
/// Ignored by git. The committed suite states requirements and so is portable;
/// a fixture that reproduces one exact command on one exact lamp is about one
/// house and belongs here, where it cannot be published by accident.
const LOCAL_FIXTURE_DIR: &str = "tests/fixtures/bench_actions/local";

/// Every fixture to run when none was named: the committed suite, plus any
/// local ones.
fn default_fixtures() -> Result<Vec<PathBuf>> {
    let mut out = toml_files(Path::new(FIXTURE_DIR))?;
    // Absent is the normal case, so a missing local directory is not an error.
    out.extend(toml_files(Path::new(LOCAL_FIXTURE_DIR)).unwrap_or_default());
    if out.is_empty() {
        anyhow::bail!("no fixtures found in {FIXTURE_DIR}");
    }
    Ok(out)
}

/// Sorted `.toml` files directly inside a directory.
fn toml_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "toml"))
        .collect();
    out.sort();
    Ok(out)
}

/// Resolve one configured server against the name a fixture asks for.
///
/// Matched loosely on purpose. A fixture is committed and shared; the server
/// name is whatever the user typed into their own config, and `HomeAssistant`,
/// `Home Assistant` and `home-assistant` are all the same server to a human.
/// Comparing the exact strings would make every committed fixture fail on
/// every machine but the one it was written on.
fn endpoint_for(config: &Config, secrets: &Secrets, name: &str) -> Option<McpEndpoint> {
    let want = squash(name);
    let s = config.assistant.tools.mcp.iter().find(|s| squash(&s.name) == want)?;
    Some(McpEndpoint {
        url: s.sse_url(),
        token: secrets.resolve(&s.token_ref()),
        timeout: CALL_TIMEOUT,
    })
}

/// Lowercase and drop everything that is not a letter or digit.
fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("{secs}")
}

/// Write the two halves separately.
///
/// The split is the whole privacy design: `summary.json` carries verdicts,
/// timings and case ids and could be published without a second thought, and
/// it is the file a regression comparison reads — so comparing two runs works
/// on a machine that has never seen the house. `detail.json` carries the
/// device names, the literal arguments and the replies, and never leaves.
fn write_reports(dir: &Path, out: &RunOutcome, driver: &TurnDriver, config: &Config) -> Result<()> {
    let summary = serde_json::json!({
        "backend": driver.backend_name(),
        "model": model_name(config),
        "overall": runner::summarise(&out.safe),
        "by_language": runner::group_by(&out.safe, |r| r.language.clone()),
        "by_class": runner::group_by(&out.safe, |r| format!("{:?}", r.class)),
        "cases": out.safe,
    });
    std::fs::write(dir.join("summary.json"), serde_json::to_string_pretty(&summary)?)
        .context("write summary.json")?;
    std::fs::write(dir.join("detail.json"), serde_json::to_string_pretty(&out.detail)?)
        .context("write detail.json")?;
    Ok(())
}

fn model_name(config: &Config) -> String {
    if config.assistant.backend == fono_core::config::LlmBackend::Local {
        config.assistant.local.model.clone()
    } else {
        config.assistant.cloud.model.clone()
    }
}

/// Print the rates so they can be read as a story.
///
/// Three numbers, not one. `routed` is the model's own judgement; `first try`
/// and `in the end` nest, and the gap between them is exactly what Fono's
/// recovery machinery is worth. A single "success rate" hides both the gap
/// and the case that routed perfectly and still failed at the server.
fn print_summary(out: &RunOutcome) {
    let s = runner::summarise(&out.safe);
    println!("\n  ran {}   skipped {}", s.n, s.skipped);
    println!("  routed right       {:.0}%", s.routing_rate * 100.0);
    println!("  worked first try   {:.0}%", s.first_try_rate * 100.0);
    println!("  worked in the end  {:.0}%   (+{} recovered)", s.final_rate * 100.0, s.recovered);
    println!("  failed {}   drifted {}", s.failed, s.drifted);
    println!("  median {} ms   slowest tenth {} ms", s.p50_ms, s.p95_ms);

    let by_lang = runner::group_by(&out.safe, |r| r.language.clone());
    if by_lang.len() > 1 {
        println!("\n  by language");
        for (lang, g) in &by_lang {
            println!(
                "    {lang}   routed {:.0}%   final {:.0}%   median {} ms",
                g.routing_rate * 100.0,
                g.final_rate * 100.0,
                g.p50_ms
            );
        }
    }

    // Failures grouped by class, because six spread across six classes is a
    // weak model and six in one class is a broken rung — and the fix differs.
    let by_class = runner::group_by(&out.safe, |r| format!("{:?}", r.class));
    let broken: Vec<_> = by_class.iter().filter(|(_, g)| g.failed > 0).collect();
    if !broken.is_empty() {
        println!("\n  where it went wrong");
        for (class, g) in broken {
            println!("    {class}   {} of {}", g.failed, g.n);
        }
    }

    for d in out.detail.iter().filter(|d| !d.notes.is_empty()) {
        println!("\n  {} [{}]", d.id, d.language);
        println!("    said: {}", d.said);
        for n in &d.notes {
            println!("    - {n}");
        }
    }

    let bad = out.safe.iter().filter(|r| r.verdict == Verdict::Failed).count();
    if bad > 0 {
        println!("\n  {bad} case(s) failed.");
    }
}

#[cfg(test)]
mod tests {
    use super::squash;

    /// The whole point of committed fixtures: the same fixture has to find
    /// the server whatever the user called it. This exact mismatch —
    /// `HomeAssistant` in a real config against `Home Assistant` in the
    /// fixture — aborted the first live run.
    #[test]
    fn server_names_match_however_they_are_written() {
        for name in ["HomeAssistant", "Home Assistant", "home-assistant", "home_assistant"] {
            assert_eq!(squash(name), squash("Home Assistant"), "{name}");
        }
    }

    #[test]
    fn different_servers_still_differ() {
        assert_ne!(squash("Home Assistant"), squash("Notes"));
    }
}
