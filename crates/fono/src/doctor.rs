// SPDX-License-Identifier: GPL-3.0-only
//! `fono doctor` — diagnostic report.

use std::fmt::Write;
use std::io::IsTerminal;
use std::sync::OnceLock;

use anyhow::Result;
use fono_core::hwcheck;
use fono_core::{Config, Paths, Secrets};

use crate::key_probe::KeyReachability;

/// Map of API-key env-var name → live reachability outcome, produced by
/// [`crate::key_probe::probe_keys`] before [`gather`] runs (the probes
/// are async; `gather` is synchronous so it can run under
/// `spawn_blocking`). An empty map means the caller skipped live probes
/// — provider rows then fall back to the presence-only "present" line.
pub type KeyProbes = std::collections::BTreeMap<String, KeyReachability>;

/// Whether to emit ANSI color escapes in the report. True iff stdout
/// is a TTY and `NO_COLOR` is unset. Cached on first call.
fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}

fn paint(code: &str, s: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
#[rustfmt::skip] fn ok(s: &str) -> String { paint("32", s) } // green
#[rustfmt::skip] fn bad(s: &str) -> String { paint("31;1", s) } // bold red
#[rustfmt::skip] fn warn(s: &str) -> String { paint("33", s) } // yellow
#[rustfmt::skip] fn dim(s: &str) -> String { paint("2", s) } // dim
#[rustfmt::skip] fn head(s: &str) -> String { paint("1;36", s) } // bold cyan
#[rustfmt::skip]
fn star(active: bool) -> String {
    if active { paint("1;36", "*") } else { " ".into() }
}

/// Severity of a single doctor check. `Info` is purely informational and
/// never affects [`DoctorReport::aggregate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Ok,
    Warn,
    Fail,
    Info,
}

/// One structured finding inside a [`DoctorSection`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct DoctorCheck {
    pub label: String,
    pub detail: String,
    pub severity: Severity,
}

/// A titled group of checks — mirrors one heading of the text report.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DoctorSection {
    pub title: String,
    pub checks: Vec<DoctorCheck>,
}

/// Structured doctor report. The web settings UI and IPC consume the
/// JSON shape; the CLI prints [`Self::text`], which is built in the same
/// pass so the two surfaces can never disagree.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DoctorReport {
    pub version: String,
    pub variant: String,
    /// Unix seconds when the checks ran.
    pub generated_at: u64,
    /// Worst check severity: any `Fail` ⇒ `Fail`, else any `Warn` ⇒
    /// `Warn`, else `Ok`. `Info` never counts.
    pub aggregate: Severity,
    pub sections: Vec<DoctorSection>,
    /// The human text report (ANSI-colored iff stdout is a TTY).
    #[serde(skip)]
    pub text: String,
}

impl DoctorReport {
    /// The text report with ANSI escapes removed — for non-TTY surfaces
    /// (IPC responses, logs).
    #[must_use]
    pub fn render_plain(&self) -> String {
        strip_ansi(&self.text)
    }
}

/// Worst severity across all checks (`Info` is ignored).
fn aggregate(sections: &[DoctorSection]) -> Severity {
    let mut worst = Severity::Ok;
    for c in sections.iter().flat_map(|s| &s.checks) {
        match c.severity {
            Severity::Fail => return Severity::Fail,
            Severity::Warn => worst = Severity::Warn,
            Severity::Ok | Severity::Info => {}
        }
    }
    worst
}

/// Remove ANSI SGR escapes (`ESC [ … m`) — the only kind this module emits.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c2 in chars.by_ref() {
                if c2 == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Accumulates [`DoctorSection`]s alongside the text report as [`gather`]
/// walks the checks. Purely additive — the text output is built exactly
/// as before; details are stored ANSI-stripped.
#[derive(Default)]
struct Collector {
    sections: Vec<DoctorSection>,
}

impl Collector {
    fn section(&mut self, title: &str) {
        self.sections.push(DoctorSection { title: title.to_string(), checks: Vec::new() });
    }
    fn push(&mut self, severity: Severity, label: &str, detail: &str) {
        if self.sections.is_empty() {
            self.section("General");
        }
        let checks = &mut self.sections.last_mut().expect("non-empty sections").checks;
        checks.push(DoctorCheck { label: label.to_string(), detail: strip_ansi(detail), severity });
    }
}

/// `fono doctor` — the full diagnostic report as text (colored when
/// stdout is a TTY).
///
/// The live API-key reachability probes are kicked off **first**, then
/// the synchronous [`gather`] pass runs concurrently on a blocking
/// thread. `gather` only needs the probe results for the provider
/// matrix, so it blocks for them lazily — meaning all the local
/// hardware / Vulkan / model / config checks are computed *while* the
/// network probes are still in flight. Net effect: doctor waits roughly
/// `max(local_checks, slowest_probe)` instead of their sum.
pub async fn report(paths: &Paths) -> Result<String> {
    let probe_paths = paths.clone();
    let probe_task = tokio::spawn(async move { probe_configured_keys(&probe_paths).await });
    let handle = tokio::runtime::Handle::current();
    let gather_paths = paths.clone();
    tokio::task::spawn_blocking(move || {
        gather(&gather_paths, || handle.block_on(probe_task).unwrap_or_default()).map(|r| r.text)
    })
    .await
    .map_err(|e| anyhow::anyhow!("doctor task panicked: {e}"))?
}

/// Live-probe every configured API key in parallel. Returns an empty
/// map on any load failure — doctor then degrades to presence-only key
/// reporting rather than failing outright.
pub async fn probe_configured_keys(paths: &Paths) -> KeyProbes {
    let secrets = Secrets::load(&paths.secrets_file()).unwrap_or_default();
    let envs: Vec<String> = crate::key_probe::all_key_envs()
        .into_iter()
        .filter(|e| secrets.resolve(e).is_some())
        .collect();
    if envs.is_empty() {
        return KeyProbes::new();
    }
    crate::key_probe::probe_keys(&envs, &secrets).await
}

/// Compute the `(severity, painted status)` for a provider's API-key
/// column, folding in the live reachability probe when one is
/// available. Falls back to the presence-only "present / missing" line
/// when `probes` has no entry for this key (probes skipped or the key
/// carries no catalogue endpoint).
fn key_status(
    needs_key: bool,
    key_env: &str,
    secrets: &Secrets,
    probes: &KeyProbes,
) -> (Severity, String) {
    use Severity as S;
    if !needs_key {
        return (S::Info, dim("no key needed"));
    }
    if secrets.resolve(key_env).is_none() {
        return (S::Info, dim(&format!("{key_env} missing")));
    }
    match probes.get(key_env) {
        Some(KeyReachability::Valid) => (S::Ok, ok(&format!("{key_env} works"))),
        Some(KeyReachability::Rejected(code)) => {
            (S::Fail, bad(&format!("{key_env} REJECTED (HTTP {code} — expired or invalid)")))
        }
        Some(KeyReachability::Unexpected(code)) => {
            (S::Warn, warn(&format!("{key_env} present (unverified: HTTP {code})")))
        }
        Some(KeyReachability::Unreachable(e)) => {
            (S::Warn, warn(&format!("{key_env} present (unreachable: {e})")))
        }
        // No probe was run (or no endpoint) — report presence only.
        Some(KeyReachability::NoProbe) | None => (S::Ok, ok(&format!("{key_env} present"))),
    }
}

/// Run every doctor check once, producing the structured report and the
/// text rendering in a single pass. `probes_source` yields the live
/// API-key reachability results (from [`probe_configured_keys`]); it is
/// invoked lazily, only once the provider matrix is reached, so callers
/// can run the network probes concurrently with this pass. Return an
/// empty map to skip live key reporting (presence-only).
#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
pub fn gather(paths: &Paths, probes_source: impl FnOnce() -> KeyProbes) -> Result<DoctorReport> {
    use Severity as S;
    let mut col = Collector::default();
    let mut out = String::new();
    let variant = crate::variant::VARIANT;
    writeln!(
        out,
        "{} v{} ({} variant — {})",
        head("Fono doctor —"),
        env!("CARGO_PKG_VERSION"),
        variant.label(),
        variant.description(),
    )?;
    writeln!(out)?;

    // ----------------------------------------------------------------
    // Hardware probe + tier (drives wizard recommendations + helps
    // diagnose "why is local STT slow on my machine?")
    // ----------------------------------------------------------------
    let mut snap = hwcheck::probe(&paths.cache_dir);
    // Upgrade `host_gpu` from the Vulkan probe (no-op on Apple Silicon).
    // See ADR 0028.
    if snap.host_gpu == hwcheck::HostGpu::None {
        snap.host_gpu = fono_core::vulkan_probe::probe().host_gpu_class();
    }
    let tier = snap.tier();
    // Recommendation is the largest multilingual model that this binary
    // can actually afford to load — walking the same affordability gate
    // the wizard uses, against the inference path actually available to
    // this build (the CPU variant cannot reach the host's Vulkan GPU).
    // Replaces the static `tier.default_whisper_model()` lookup so
    // `fono doctor` and the wizard never disagree on the recommendation.
    let inference_snap =
        snap.for_inference(matches!(crate::variant::VARIANT, crate::variant::Variant::Gpu,));
    let recommended_model = fono_stt::registry::ModelRegistry::pick_default_local(&inference_snap);
    let ram_gb = snap.total_ram_bytes / (1024 * 1024 * 1024);
    let disk_gb = snap.free_disk_bytes / (1024 * 1024 * 1024);
    let isa = if snap.cpu_features.avx2 {
        "AVX2"
    } else if snap.cpu_features.neon {
        "NEON"
    } else {
        "no-vec"
    };
    writeln!(out, "{}", head("Hardware:"))?;
    writeln!(
        out,
        "  cores : {} physical / {} logical  ({isa})",
        snap.physical_cores, snap.logical_cores
    )?;
    writeln!(
        out,
        "  ram   : {ram_gb} GB total · disk free : {disk_gb} GB · arch : {}/{}",
        snap.os, snap.arch
    )?;
    writeln!(out, "  local-tier : {} (recommends whisper-{})", tier.as_str(), recommended_model)?;
    col.section("Hardware");
    col.push(
        S::Info,
        "cores",
        &format!("{} physical / {} logical ({isa})", snap.physical_cores, snap.logical_cores),
    );
    col.push(
        S::Info,
        "memory / disk",
        &format!("{ram_gb} GB RAM · {disk_gb} GB free disk · {}/{}", snap.os, snap.arch),
    );
    col.push(
        S::Info,
        "local tier",
        &format!("{} (recommends whisper-{})", tier.as_str(), recommended_model),
    );
    if let Err(reason) = snap.suitability() {
        writeln!(out, "  {} {reason}", bad("unsuitable because:"))?;
        col.push(S::Fail, "suitability", &format!("unsuitable: {reason}"));
    } else {
        col.push(S::Ok, "suitability", "suitable for local inference");
    }
    writeln!(out)?;

    // Compute backends — what's compiled into this variant + what the
    // host's Vulkan loader reports. Slice 2 of
    // `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`.
    {
        use crate::variant::{Variant, VARIANT};
        use fono_core::vulkan_probe::probe;
        writeln!(out, "{}", head("Compute backends:"))?;
        writeln!(out, "  variant  : {} ({})", VARIANT.label(), VARIANT.description())?;
        let outcome = probe();
        writeln!(out, "  vulkan   : {}", outcome.summary_line())?;
        col.section("Compute backends");
        col.push(S::Info, "variant", &format!("{} ({})", VARIANT.label(), VARIANT.description()));
        col.push(S::Info, "vulkan", &outcome.summary_line());
        if matches!(VARIANT, Variant::Cpu) && outcome.is_usable() {
            col.push(
                S::Info,
                "hint",
                "this machine has Vulkan-capable GPU(s); the GPU release variant runs \
                 inference faster on this hardware",
            );
            writeln!(
                out,
                "  hint     : your machine has Vulkan-capable GPU(s); the GPU release \
                 variant runs inference faster on this hardware. Download \
                 `fono-gpu-vX.Y.Z-x86_64` from the Releases page (or upgrade in-place \
                 once `fono update --variant gpu` lands)."
            )?;
        }
        writeln!(out)?;
    }

    writeln!(out, "{}", head("Paths:"))?;
    writeln!(out, "  config : {}", paths.config_file().display())?;
    writeln!(out, "  data   : {}", paths.data_dir.display())?;
    writeln!(out, "  cache  : {}", paths.cache_dir.display())?;
    writeln!(out, "  state  : {}", paths.state_dir.display())?;
    writeln!(out)?;
    col.section("Paths");
    col.push(S::Info, "config", &paths.config_file().display().to_string());
    col.push(S::Info, "data", &paths.data_dir.display().to_string());
    col.push(S::Info, "cache", &paths.cache_dir.display().to_string());
    col.push(S::Info, "state", &paths.state_dir.display().to_string());

    let install_state = crate::install::doctor_state();
    writeln!(out, "{} {}", head("Install:"), install_state)?;
    writeln!(out)?;
    col.section("Install");
    col.push(S::Info, "state", &install_state);

    let config_exists = paths.config_file().exists();
    writeln!(
        out,
        "{} {}",
        head("Config :"),
        if config_exists { ok("present") } else { bad("MISSING (run `fono setup`)") }
    )?;
    col.section("Config");
    if config_exists {
        col.push(S::Ok, "config file", "present");
    } else {
        col.push(S::Fail, "config file", "missing (run `fono setup`)");
    }
    let cfg = if config_exists {
        match Config::load(&paths.config_file()) {
            Ok(c) => {
                writeln!(out, "  version        : {}", c.version)?;
                writeln!(out, "  stt.backend    : {:?}", c.stt.backend)?;
                writeln!(out, "  stt.local.model: {}", c.stt.local.model)?;
                writeln!(out, "  polish.backend    : {:?}", c.polish.backend)?;
                writeln!(out, "  polish.local.model: {}", c.polish.local.model)?;
                writeln!(
                    out,
                    "  hotkeys        : dictation={} assistant={} (short=toggle, long=hold)",
                    c.hotkeys.dictation, c.hotkeys.assistant,
                )?;
                col.push(
                    S::Info,
                    "stt backend",
                    &format!("{:?} (model {})", c.stt.backend, c.stt.local.model),
                );
                col.push(
                    S::Info,
                    "polish backend",
                    &format!("{:?} (model {})", c.polish.backend, c.polish.local.model),
                );
                col.push(
                    S::Info,
                    "hotkeys",
                    &format!(
                        "dictation={} assistant={} (short=toggle, long=hold)",
                        c.hotkeys.dictation, c.hotkeys.assistant
                    ),
                );
                Some(c)
            }
            Err(e) => {
                writeln!(out, "  {} {e}", bad("FAILED TO LOAD:"))?;
                col.push(S::Fail, "parse", &format!("failed to load: {e}"));
                None
            }
        }
    } else {
        None
    };
    writeln!(out)?;

    // Personal vocabulary (ADR 0037): entry count / parse status.
    {
        let vpath = paths.vocabulary_file();
        col.section("Vocabulary");
        let state = if vpath.exists() {
            match fono_core::correction::VocabularyFile::load(&vpath) {
                Ok(f) => match f.to_table() {
                    Ok(t) => {
                        col.push(S::Ok, "rules", &format!("{} rule(s)", t.len()));
                        ok(&format!("{} rule(s)", t.len()))
                    }
                    Err(e) => {
                        col.push(
                            S::Fail,
                            "rules",
                            &format!("invalid ({e}) — corrections disabled"),
                        );
                        bad(&format!("INVALID ({e}) — corrections disabled"))
                    }
                },
                Err(e) => {
                    col.push(S::Fail, "rules", &format!("failed to load: {e}"));
                    bad(&format!("FAILED TO LOAD: {e}"))
                }
            }
        } else {
            col.push(S::Info, "rules", "none (add with `fono vocabulary add <wrong> <right>`)");
            "none (add with `fono vocabulary add <wrong> <right>`)".to_string()
        };
        writeln!(out, "{} {}", head("Vocabulary:"), state)?;
        writeln!(out, "  file : {}", vpath.display())?;
    }
    writeln!(out)?;

    // ----------------------------------------------------------------
    // Backend factories — if the user picked a cloud backend, exercise
    // the factory so they see a clear "API key missing" or "feature
    // missing" message right here rather than having to start the
    // daemon and read the log.
    // ----------------------------------------------------------------
    let secrets = Secrets::load(&paths.secrets_file()).unwrap_or_default();
    if let Some(c) = cfg.as_ref() {
        writeln!(out, "{}", head("Backends:"))?;
        col.section("Backends");
        match fono_stt::build_stt(&c.stt, &c.general, &secrets, &paths.whisper_models_dir()) {
            Ok(s) => {
                writeln!(out, "  stt: {} {}", s.name(), ok("ready"))?;
                col.push(S::Ok, "stt", &format!("{} ready", s.name()));
            }
            Err(e) => {
                writeln!(out, "  stt: {} {e:#}", bad("FAIL —"))?;
                col.push(S::Fail, "stt", &format!("{e:#}"));
            }
        }
        match fono_polish::build_polish(&c.polish, &secrets, &paths.polish_models_dir()) {
            Ok(Some(l)) => {
                writeln!(out, "  polish: {} {}", l.name(), ok("ready"))?;
                col.push(S::Ok, "polish", &format!("{} ready", l.name()));
            }
            Ok(None) => {
                writeln!(out, "  polish: {}", dim("disabled (cleanup off)"))?;
                col.push(S::Info, "polish", "disabled (cleanup off)");
            }
            Err(e) => {
                writeln!(out, "  polish: {} {e:#}", bad("FAIL —"))?;
                col.push(S::Fail, "polish", &format!("{e:#}"));
            }
        }
        // `mut` is only touched by the `realtime`-gated arm below; allow
        // the unused-mut when that feature is compiled out.
        #[allow(unused_mut)]
        let mut assistant_realtime_only = false;
        match fono_assistant::build_assistant_handle(
            &c.assistant,
            &secrets,
            &paths.polish_models_dir(),
        ) {
            Ok(Some(fono_assistant::AssistantHandle::Staged(a))) => {
                writeln!(out, "  assistant: {} {} (staged)", a.name(), ok("ready"))?;
                col.push(S::Ok, "assistant", &format!("{} ready (staged)", a.name()));
            }
            #[cfg(feature = "realtime")]
            Ok(Some(fono_assistant::AssistantHandle::Realtime(a))) => {
                assistant_realtime_only = true;
                writeln!(
                    out,
                    "  assistant: {} {} (realtime speech-to-speech)",
                    a.name(),
                    ok("ready")
                )?;
                col.push(
                    S::Ok,
                    "assistant",
                    &format!("{} ready (realtime speech-to-speech)", a.name()),
                );
            }
            Ok(None) => {
                writeln!(out, "  assistant: {}", dim("disabled"))?;
                col.push(S::Info, "assistant", "disabled");
            }
            Err(e) => {
                writeln!(out, "  assistant: {} {e:#}", bad("FAIL —"))?;
                col.push(S::Fail, "assistant", &format!("{e:#}"));
            }
        }
        match fono_tts::build_tts(&c.tts, &secrets, &c.general.languages, &paths.voices_dir()) {
            Ok(Some(t)) => {
                writeln!(out, "  tts: {} {}", t.name(), ok("ready"))?;
                col.push(S::Ok, "tts", &format!("{} ready", t.name()));
            }
            Ok(None) => {
                writeln!(
                    out,
                    "  tts: {}",
                    dim("disabled (assistant replies shown as on-screen text)")
                )?;
                col.push(S::Info, "tts", "disabled (assistant replies shown as on-screen text)");
            }
            Err(e) => {
                writeln!(out, "  tts: {} {e:#}", bad("FAIL —"))?;
                col.push(S::Fail, "tts", &format!("{e:#}"));
            }
        }
        // Local LLM inference server (OpenAI + Ollama HTTP API; ADR 0036).
        // The served model tracks the active [assistant] backend, with a
        // same-provider text fallback when that backend is realtime.
        if c.server.llm.enabled {
            let scope = if c.server.llm.bind == "127.0.0.1" || c.server.llm.bind == "::1" {
                "loopback only"
            } else {
                "LAN-exposed"
            };
            let m = c.server.llm.model.trim();
            let override_model = (!m.is_empty()).then_some(m);
            let served = fono_assistant::server_assistant_model_name(&c.assistant, override_model);
            let served = if served.is_empty() { "fono".to_string() } else { served };
            // Proxy fast-lane (ADR 0036): OpenAI-compat cloud backends are
            // forwarded verbatim (full tool/vision/parameter fidelity);
            // everything else is adapted through the assistant trait.
            let proxyable = c.assistant.enabled
                && fono_assistant::chat_endpoint(&c.assistant.backend).is_some();
            let mode = if proxyable {
                "OpenAI surface proxied to the cloud provider (full tool/vision fidelity)"
            } else {
                "served via the local adapter"
            };
            writeln!(
                out,
                "  llm server: {} on {}:{} ({scope}); serving {served}; {mode}; OpenAI + Ollama API",
                ok("enabled"),
                c.server.llm.bind,
                c.server.llm.port,
            )?;
            col.push(
                S::Info,
                "llm server",
                &format!(
                    "enabled on {}:{} ({scope}); serving {served}; {mode}",
                    c.server.llm.bind, c.server.llm.port
                ),
            );
            if assistant_realtime_only {
                writeln!(
                    out,
                    "    {} the active assistant is realtime (speech-to-speech); the API \
                     auto-serves the same provider's text model ({served}) instead — set \
                     [server.llm].model to pin a different one.",
                    dim("note:"),
                )?;
            }
        } else {
            writeln!(
                out,
                "  llm server: {} (enable `[server.llm]` to serve local inference over HTTP)",
                dim("disabled"),
            )?;
            col.push(
                S::Info,
                "llm server",
                "disabled (enable `[server.llm]` to serve local inference over HTTP)",
            );
        }

        // Inbound API-key authentication for the exposed servers (ADR:
        // replaces the legacy single static token). Loopback callers are
        // always trusted; non-loopback callers need a valid key when auth
        // is on. Report per-server auth state, key counts, and the loud
        // "open relay to your paid cloud account" hazard when a server is
        // LAN-exposed with auth off.
        {
            let (active, inactive) = fono_core::api_keys::ApiKeyStore::open(&paths.api_keys_db())
                .map_or((0, 0), |s| {
                    let a = s.active_count().unwrap_or(0);
                    let total = s.list().map(|k| k.len() as i64).unwrap_or(0);
                    (a, (total - a).max(0))
                });
            for (label, enabled, bind, auth) in [
                ("llm", c.server.llm.enabled, c.server.llm.bind.as_str(), c.server.llm.auth),
                ("web", c.server.web.enabled, c.server.web.bind.as_str(), c.server.web.auth),
            ] {
                if !enabled {
                    continue;
                }
                let loopback = bind == "127.0.0.1" || bind == "::1";
                let auth_str = if auth { ok("on") } else { bad("off") };
                writeln!(
                    out,
                    "  {label} auth: {auth_str}; {active} active / {inactive} inactive key(s)"
                )?;
                if !loopback && !auth {
                    let msg = format!(
                        "[server.{label}] is LAN-exposed ({bind}) with authentication OFF — anyone \
                         on the network can drive it (and any paid cloud key it proxies). Set \
                         [server.{label}].auth = true."
                    );
                    writeln!(out, "    {}", bad(&msg))?;
                    col.push(S::Fail, &format!("{label} auth"), &msg);
                } else if !loopback && active == 0 {
                    let msg = format!(
                        "[server.{label}] is LAN-exposed ({bind}) with auth on but no API keys \
                         exist — all remote requests are rejected. Create one with \
                         `fono server keys create`."
                    );
                    writeln!(out, "    {}", bad(&msg))?;
                    col.push(S::Warn, &format!("{label} auth"), &msg);
                } else {
                    col.push(
                        S::Info,
                        &format!("{label} auth"),
                        &format!(
                            "auth {}; {active} active / {inactive} inactive key(s)",
                            if auth { "on" } else { "off" }
                        ),
                    );
                }
            }
        }
        writeln!(out)?;

        // ------------------------------------------------------------
        // Per-provider key + reachability matrix (provider-switching
        // plan task S18). One line per known backend with active marker
        // so users see at a glance which providers are ready to switch
        // to via `fono use stt …` / `fono use polish …`.
        // ------------------------------------------------------------
        // Resolve the live key-reachability probes now. Everything above
        // (hardware, Vulkan, model registry, config, backends) was
        // computed while the probes ran concurrently, so this blocks
        // only for whatever network time is left — usually none.
        let probes = probes_source();
        writeln!(out, "{}", head("Providers (STT):"))?;
        col.section("Providers (STT)");
        for b in fono_core::providers::all_stt_backends() {
            let active = b == c.stt.backend;
            let mark = star(active);
            let name = fono_core::providers::stt_backend_str(&b);
            let needs_key = fono_core::providers::stt_requires_key(&b);
            let key_env = fono_core::providers::stt_key_env(&b);
            let (sev, key_status) = key_status(needs_key, key_env, &secrets, &probes);
            let model = if needs_key {
                fono_stt::defaults::default_cloud_model(name).to_string()
            } else {
                c.stt.local.model.clone()
            };
            writeln!(out, "  {mark} {name:<14} model: {model:<32} {key_status}")?;
            col.push(
                sev,
                name,
                &format!("{}model: {model} · {key_status}", if active { "(active) " } else { "" }),
            );
        }
        writeln!(out)?;

        writeln!(out, "{}", head("Providers (LLM):"))?;
        col.section("Providers (LLM)");
        for b in fono_core::providers::all_polish_backends() {
            let active = b == c.polish.backend;
            let mark = star(active);
            let name = fono_core::providers::polish_backend_str(&b);
            let needs_key = fono_core::providers::polish_requires_key(&b);
            let key_env = fono_core::providers::polish_key_env(&b);
            let (sev, key_status) = key_status(needs_key, key_env, &secrets, &probes);
            let model = if matches!(b, fono_core::config::PolishBackend::None) {
                "—".to_string()
            } else if needs_key || matches!(b, fono_core::config::PolishBackend::Ollama) {
                fono_polish::defaults::default_cloud_model(name).to_string()
            } else {
                c.polish.local.model.clone()
            };
            writeln!(out, "  {mark} {name:<14} model: {model:<32} {key_status}")?;
            col.push(
                sev,
                name,
                &format!("{}model: {model} · {key_status}", if active { "(active) " } else { "" }),
            );
        }
        writeln!(out)?;

        writeln!(out, "{}", head("Providers (assistant):"))?;
        col.section("Providers (assistant)");
        for b in fono_core::providers::all_assistant_backends() {
            let active = b == c.assistant.backend;
            let mark = star(active);
            let name = fono_core::providers::assistant_backend_str(&b);
            let needs_key = fono_core::providers::assistant_requires_key(&b);
            let key_env = fono_core::providers::assistant_key_env(&b);
            let (sev, key_status) = key_status(needs_key, key_env, &secrets, &probes);
            writeln!(out, "  {mark} {name:<14} {key_status}")?;
            col.push(sev, name, &format!("{}{key_status}", if active { "(active) " } else { "" }));
        }
        writeln!(out)?;

        writeln!(out, "{}", head("Providers (TTS):"))?;
        col.section("Providers (TTS)");
        for b in fono_core::providers::all_tts_backends() {
            let active = b == c.tts.backend;
            let mark = star(active);
            let name = fono_core::providers::tts_backend_str(&b);
            let needs_key = fono_core::providers::tts_requires_key(&b);
            let key_env = fono_core::providers::tts_key_env(&b);
            // Cloud TTS backends fold in the live reachability probe;
            // the non-cloud arms (Wyoming/Local/None) stay informational.
            let mut sev = S::Info;
            let extra = match b {
                fono_core::config::TtsBackend::Wyoming => c
                    .tts
                    .wyoming
                    .as_ref()
                    .map(|w| format!("uri={}", w.uri))
                    .unwrap_or_else(|| dim("uri=(unset)")),
                fono_core::config::TtsBackend::OpenAI
                | fono_core::config::TtsBackend::Groq
                | fono_core::config::TtsBackend::OpenRouter
                | fono_core::config::TtsBackend::Cartesia
                | fono_core::config::TtsBackend::Deepgram
                | fono_core::config::TtsBackend::ElevenLabs
                | fono_core::config::TtsBackend::Speechmatics
                | fono_core::config::TtsBackend::Gemini => {
                    let (s, status) = key_status(needs_key, key_env, &secrets, &probes);
                    sev = s;
                    status
                }
                fono_core::config::TtsBackend::Local => {
                    if c.tts.local.voice.is_empty() {
                        dim("voice=(default)")
                    } else {
                        format!("voice={}", c.tts.local.voice)
                    }
                }
                fono_core::config::TtsBackend::None => dim("—"),
            };
            writeln!(out, "  {mark} {name:<14} {extra}")?;
            col.push(sev, name, &format!("{}{extra}", if active { "(active) " } else { "" }));
        }
        writeln!(out)?;
        writeln!(out, "(* = active. Switch with `fono use stt|polish|assistant|tts <backend>`.)")?;
        writeln!(out)?;
    }

    writeln!(out, "{}", head("Session:"))?;
    writeln!(
        out,
        "  XDG_SESSION_TYPE : {}",
        std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "(unset)".into())
    )?;
    writeln!(
        out,
        "  WAYLAND_DISPLAY  : {}",
        std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "(unset)".into())
    )?;
    writeln!(
        out,
        "  DISPLAY          : {}",
        std::env::var("DISPLAY").unwrap_or_else(|_| "(unset)".into())
    )?;
    writeln!(out)?;
    col.section("Session");
    for var in ["XDG_SESSION_TYPE", "WAYLAND_DISPLAY", "DISPLAY"] {
        col.push(S::Info, var, &std::env::var(var).unwrap_or_else(|_| "(unset)".into()));
    }

    let audio_stack = fono_audio::mute::detect();
    writeln!(out, "{} {audio_stack:?}", head("Audio stack :"))?;
    col.section("Audio");
    col.push(S::Info, "stack", &format!("{audio_stack:?}"));
    // Input device matrix: list every device the active stack
    // (PulseAudio / PipeWire via pactl, or cpal as fallback) reports,
    // marking whichever the OS currently considers default. Fono no
    // longer keeps an `[audio].input_device` override; microphone
    // selection is delegated to the OS layer (pavucontrol / GNOME /
    // KDE settings on Linux, Sound preferences on macOS / Windows).
    let devices = fono_audio::devices::list_input_devices();
    writeln!(out, "{}", head("Audio inputs:"))?;
    if devices.is_empty() {
        // Platform-appropriate recovery hint: the Linux advice
        // (install pactl/wpctl) is meaningless on macOS, where the
        // usual causes are no mic hardware (Mac Studio / mini) or a
        // denied Microphone permission in System Settings.
        let hint = if cfg!(target_os = "macos") {
            "(no input devices reported — connect a microphone, and check System Settings → Privacy & Security → Microphone if capture fails)"
        } else {
            "(no input devices reported — install `wireplumber` (wpctl) or `pulseaudio-utils` (pactl), or check that your microphone is plugged in)"
        };
        writeln!(out, "  {}", bad(hint))?;
        col.push(S::Fail, "input devices", hint);
    } else {
        for d in &devices {
            let mark = star(d.is_default);
            writeln!(out, "  {mark} {}", d.display_name)?;
        }
        let names: Vec<String> = devices
            .iter()
            .map(|d| format!("{}{}", if d.is_default { "* " } else { "" }, d.display_name))
            .collect();
        col.push(S::Ok, "input devices", &names.join(" · "));
        writeln!(
            out,
            "(* = system default. Change via the tray Microphone submenu, \
             pavucontrol, or your OS sound settings.)"
        )?;
    }
    let injector = fono_inject::inject::Injector::detect();
    writeln!(out, "{} {injector:?}", head("Injector    :"))?;
    col.section("Desktop integration");
    col.push(S::Info, "injector", &format!("{injector:?}"));
    // macOS: injection is gated by the Accessibility TCC grant, and
    // CGEventPost drops events *silently* when it's missing — so the
    // probe is the only honest signal (macOS port plan Task 9.3).
    if let Some(trusted) = fono_inject::accessibility_trusted() {
        if trusted {
            writeln!(out, "{} {}", head("Accessibility:"), ok("granted (fono can type for you)"))?;
            col.push(S::Ok, "accessibility", "granted (fono can type for you)");
        } else {
            writeln!(
                out,
                "{} {}\n  {}",
                head("Accessibility:"),
                warn("not granted — dictation falls back to the clipboard (paste with Cmd+V)"),
                dim(&format!(
                    "Flip the Fono toggle once under System Settings → Privacy & \
                     Security → Accessibility, or run:\n  open \"{}\"",
                    fono_inject::ACCESSIBILITY_SETTINGS_URL
                ))
            )?;
            col.push(
                S::Warn,
                "accessibility",
                "not granted — dictation falls back to the clipboard (paste with Cmd+V)",
            );
        }
    }
    // Focused-window probe — powers the per-app context rules (terminal
    // shell vocabulary, code-editor hints, private-window history
    // suppression). Shown on every platform. Over a non-interactive
    // session (e.g. headless SSH, or a locked screen) there is no
    // foreground window and this reads "none detected".
    let focus = fono_inject::detect_focus().unwrap_or_default();
    if let Some(class) = focus.window_class.as_deref() {
        let profile = fono_inject::ContextClassifier::classify(
            focus.window_class.as_deref(),
            focus.window_title.as_deref(),
        );
        let label = profile.as_ref().map_or_else(
            || dim("no per-app rule — base prompt"),
            |p| ok(&format!("{} profile", p.name)),
        );
        writeln!(out, "{} {} ({})", head("Focus       :"), class, label)?;
        col.push(S::Info, "focus", &format!("{class} ({label})"));
    } else {
        writeln!(out, "{} {}", head("Focus       :"), dim("none detected — no foreground window"))?;
        col.push(S::Info, "focus", "none detected — no foreground window");
    }
    // Clipboard fallback — fono copies the cleaned text here when no
    // key-injection backend works, so the dictation is never lost.
    let mut clip_tools = Vec::new();
    for t in ["wl-copy", "xclip", "xsel"] {
        if which_in_path(t) {
            clip_tools.push(t);
        }
    }
    if clip_tools.is_empty() {
        writeln!(out, "{} {}", head("Clipboard   :"), ok("native (arboard)"))?;
        col.push(S::Info, "clipboard", "native (arboard)");
    } else {
        writeln!(
            out,
            "{} {} {}",
            head("Clipboard   :"),
            clip_tools.join(", "),
            dim("(native arboard preferred)")
        )?;
        col.push(
            S::Info,
            "clipboard",
            &format!("{} (native arboard preferred)", clip_tools.join(", ")),
        );
    }
    // Probe for a clipboard manager. We check both the ICCCM
    // `CLIPBOARD_MANAGER` selection owner *and* the running process
    // list (clipit, parcellite, xfce4-clipman, klipper, copyq, gpaste,
    // greenclip, diodon, clipmenud, cliphist) because not every
    // manager implements the ICCCM handoff — clipit, for example, is
    // a polling manager that watches the CLIPBOARD selection on a
    // timer. This is an X11/Wayland concept only: `detect_clipboard_manager`
    // returns `None` on macOS and Windows (their OS clipboards have no
    // such handoff/manager ecosystem), so the whole probe — and its
    // X11-specific "typed via XTEST" guidance — is compiled on Linux only.
    #[cfg(target_os = "linux")]
    {
        use fono_inject::ClipboardManager;
        match fono_inject::detect_clipboard_manager() {
            ClipboardManager::Icccm => {
                writeln!(
                    out,
                    "{} {}",
                    head("Clip manager:"),
                    ok("present (owns CLIPBOARD_MANAGER selection)")
                )?;
                col.push(S::Ok, "clipboard manager", "present (owns CLIPBOARD_MANAGER selection)");
            }
            ClipboardManager::Polling(name) => {
                writeln!(
                    out,
                    "{} {}",
                    head("Clip manager:"),
                    ok(&format!("present ({name}, polling — no CLIPBOARD_MANAGER selection)"))
                )?;
                col.push(
                    S::Ok,
                    "clipboard manager",
                    &format!("present ({name}, polling — no CLIPBOARD_MANAGER selection)"),
                );
            }
            ClipboardManager::None => {
                writeln!(
                    out,
                    "{} {}\n  {}",
                    head("Clip manager:"),
                    dim("none detected"),
                    dim("Typing is unaffected — fono types directly via XTEST and never \
                     touches the clipboard. The optional `also_copy_to_clipboard` path \
                     keeps working while fono runs because the daemon holds one \
                     persistent arboard handle. Clipboard contents are lost when fono \
                     exits unless a manager (clipit / parcellite / xfce4-clipman / \
                     klipper / copyq / gpaste / greenclip) is running.")
                )?;
                col.push(S::Info, "clipboard manager", "none detected");
            }
        }
    }
    writeln!(
        out,
        "{} {} ({})",
        head("IPC socket  :"),
        paths.ipc_socket().display(),
        if paths.ipc_socket().exists() { ok("exists") } else { warn("absent") }
    )?;
    if paths.ipc_socket().exists() {
        col.push(S::Ok, "ipc socket", &format!("{} (exists)", paths.ipc_socket().display()));
    } else {
        col.push(
            S::Warn,
            "ipc socket",
            &format!("{} (absent — daemon not running)", paths.ipc_socket().display()),
        );
    }

    // Overlay backend probe. Uses `fono_overlay::backend::probe_selection`
    // which only reads env vars + walks the candidate list — it does
    // not actually connect to Wayland or open a window, so doctor stays
    // cheap and side-effect-free.
    {
        use fono_overlay::{BackendCapabilities, BackendId};
        let (id, reason) = fono_overlay::backend::probe_selection();
        let caps = match id {
            BackendId::WlrLayerShell
            | BackendId::X11OverrideRedirect
            | BackendId::MacPanel
            | BackendId::Win32LayeredToolWindow => BackendCapabilities {
                transparency: true,
                client_positioning: true,
                focus_passthrough: true,
                click_passthrough: true,
            },
            BackendId::Noop => BackendCapabilities {
                transparency: false,
                client_positioning: false,
                focus_passthrough: true,
                click_passthrough: true,
            },
        };
        writeln!(out, "{} {} ({}) — {reason}", head("Overlay     :"), id.as_str(), caps.summary())?;
        col.section("Overlay");
        col.push(S::Info, "backend", &format!("{} ({}) — {reason}", id.as_str(), caps.summary()));
        // Wayland session without layer-shell and without Xwayland
        // — we have no graphical overlay path. Tell the user how
        // to fix it.
        if matches!(id, BackendId::Noop)
            && std::env::var_os("WAYLAND_DISPLAY").is_some()
            && std::env::var_os("DISPLAY").is_none()
        {
            col.push(
                S::Warn,
                "hint",
                "Wayland session without Xwayland or layer-shell — install your distro's \
                 xwayland package to enable the overlay",
            );
            writeln!(
                out,
                "  {}",
                warn(
                    "hint: Wayland session without Xwayland or layer-shell. Install \
                     your distro's xwayland package (e.g. `sudo apt install xwayland`) \
                     to enable the overlay."
                )
            )?;
        }
    }

    // ----------------------------------------------------------------
    // Screen capture section
    // ----------------------------------------------------------------
    {
        use fono_core::screen_capture::{GrabberProbe, RungKind};
        let probe = GrabberProbe::detect();

        writeln!(out, "{}", head("Screen capture:"))?;
        writeln!(out, "  Session type : {}", probe.session_label())?;
        col.section("Screen capture");
        col.push(S::Info, "session type", probe.session_label());

        let auto_rungs = probe.auto_rungs();
        if auto_rungs.is_empty() {
            writeln!(
                out,
                "  Active (auto): {}",
                bad("none — install scrot (X11) or grim+slurp (Wayland)")
            )?;
            col.push(S::Fail, "auto capture", "none — install scrot (X11) or grim+slurp (Wayland)");
        } else {
            let auto_display: Vec<String> = auto_rungs
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let name = r.display_name();
                    if i == 0 {
                        format!("{} {}", ok(&format!("{name} ✓")), dim("(active)"))
                    } else if r == &RungKind::Portal || which_in_path(r.binary()) {
                        format!("{name} {}", ok("✓"))
                    } else {
                        format!("{name} {}", dim("[missing]"))
                    }
                })
                .collect();
            writeln!(out, "  Auto rungs   : {}", auto_display.join("  "))?;
            writeln!(out, "  Active (auto): {}", ok(auto_rungs[0].display_name()))?;
            col.push(
                S::Ok,
                "auto capture",
                &format!("{} — rungs: {}", auto_rungs[0].display_name(), auto_display.join("  ")),
            );
        }

        let sel_rungs = probe.select_rungs();
        if sel_rungs.is_empty() {
            writeln!(
                out,
                "  Active (sel.): {}",
                bad("none — install grim+slurp (Wayland) or scrot -s (X11)")
            )?;
            col.push(
                S::Fail,
                "select capture",
                "none — install grim+slurp (Wayland) or scrot -s (X11)",
            );
        } else {
            let sel_display: Vec<String> = sel_rungs
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let name = r.display_name();
                    if i == 0 {
                        format!("{} {}", ok(&format!("{name} ✓")), dim("(active)"))
                    } else if r == &RungKind::Portal || which_in_path(r.binary()) {
                        format!("{name} {}", ok("✓"))
                    } else {
                        format!("{name} {}", dim("[missing]"))
                    }
                })
                .collect();
            writeln!(out, "  Select rungs : {}", sel_display.join("  "))?;
            writeln!(out, "  Active (sel.): {}", ok(sel_rungs[0].display_name()))?;
            col.push(
                S::Ok,
                "select capture",
                &format!("{} — rungs: {}", sel_rungs[0].display_name(), sel_display.join("  ")),
            );
        }
        writeln!(out)?;
    }

    // ----------------------------------------------------------------
    // Wake word — always-on "hey fono" activation (Phase J of
    // plans/2026-06-23-wake-word-openwakeword-v2.md). Honest field
    // reporting: enabled?, the detector backend that WOULD run, each
    // phrase's target + license badge + cache state, the clean default
    // model's cache state, NonCommercial consent, and the loud Wyoming
    // client-direction privacy warning. Cheap + side-effect free: it only
    // reads config and stats files (no network, no daemon, no model load).
    // ----------------------------------------------------------------
    if let Some(c) = cfg.as_ref() {
        use fono_audio::wake_registry;
        use fono_core::config::{WakeTarget, WakeWyoming};
        let w = &c.wakeword;
        writeln!(out, "{}", head("Wake word:"))?;
        col.section("Wake word");
        if w.enabled {
            writeln!(out, "  enabled        : {}", ok("true"))?;
            col.push(S::Ok, "enabled", "true");

            // Which detector backend would actually run. Reuses the registry's
            // path resolution (no duplicated filename logic) and mirrors
            // `wake::build_detector` / `try_load_onnx`: ONNX only when the
            // feature is compiled AND every configured phrase's model files
            // resolve on disk; otherwise the energy stub.
            let any_phrases = !w.phrases.is_empty();
            let inputs = WakeBackendInputs {
                onnx_compiled: cfg!(feature = "wakeword-onnx"),
                graphs_present: wake_graphs_present(&paths.cache_dir),
                all_classifiers_present: any_phrases
                    && w.phrases
                        .iter()
                        .all(|p| wake_classifier_path(&p.model, &paths.cache_dir).exists()),
            };
            match wake_backend(&inputs) {
                WakeBackend::Onnx => {
                    writeln!(out, "  backend        : {} (openWakeWord ONNX)", ok("ONNX"))?;
                    col.push(S::Ok, "backend", "ONNX (openWakeWord)");
                }
                WakeBackend::EnergyStub => {
                    let why = if !inputs.onnx_compiled {
                        "wakeword-onnx feature not compiled into this build"
                    } else if !any_phrases {
                        "no phrases configured"
                    } else if !inputs.graphs_present {
                        "shared melspec/embedding graphs not yet downloaded"
                    } else {
                        "one or more phrase classifiers not yet downloaded"
                    };
                    writeln!(out, "  backend        : {} ({why})", warn("energy stub"))?;
                    col.push(S::Warn, "backend", &format!("energy stub ({why})"));
                }
            }
            if w.phrases.is_empty() {
                writeln!(out, "  phrases        : {}", warn("none configured"))?;
                col.push(S::Warn, "phrases", "none configured");
            } else {
                writeln!(out, "  phrases:")?;
                for p in &w.phrases {
                    let entry = wake_registry::get(&p.model);
                    let license = match entry {
                        Some(e) if e.is_noncommercial() => warn(e.license.spdx()),
                        Some(e) => dim(e.license.spdx()),
                        None => dim("custom/unknown"),
                    };
                    let cache_state = if wake_classifier_path(&p.model, &paths.cache_dir).exists() {
                        ok("cached")
                    } else {
                        dim("not downloaded")
                    };
                    let target = match p.target {
                        WakeTarget::Dictation => "dictation",
                        WakeTarget::Assistant => "assistant",
                    };
                    writeln!(
                        out,
                        "    {} {:<14} target={target:<10} sens={:.2}  {license}  {cache_state}",
                        star(false),
                        p.model,
                        p.sensitivity,
                    )?;
                    col.push(
                        S::Info,
                        &format!("phrase {}", p.model),
                        &format!(
                            "target={target} sens={:.2} · {license} · {cache_state}",
                            p.sensitivity
                        ),
                    );
                }
            }

            // Runtime default model (the phrase served when none is
            // configured — e.g. the auto-served Wyoming wake path). TEMPORARY:
            // points at the community `hey_jarvis` until the clean Apache
            // `hey_fono` artifact is trained + pinned.
            let default_id = wake_registry::DEFAULT_WAKE_MODEL;
            let default_cached = wake_registry::resolved_paths(default_id, &paths.cache_dir)
                .is_some_and(|r| r.classifier.exists());
            let default_badge = match wake_registry::get(default_id) {
                Some(e) if e.is_noncommercial() => warn(e.license.spdx()),
                Some(e) => dim(e.license.spdx()),
                None => dim("unknown"),
            };
            writeln!(
                out,
                "  default model  : {default_id}  {default_badge}  {}",
                if default_cached { ok("cached") } else { dim("not yet downloaded") }
            )?;
            col.push(
                S::Info,
                "default model",
                &format!(
                    "{default_id} · {default_badge} · {}",
                    if default_cached { "cached" } else { "not yet downloaded" }
                ),
            );

            // NonCommercial community models: no consent is stored — Fono
            // notifies the user at download time and proceeds. Flag if any
            // configured phrase resolves to a CC-BY-NC-SA model.
            let nc_any = w.phrases.iter().any(|p| {
                wake_registry::get(&p.model)
                    .is_some_and(wake_registry::WakeModelEntry::is_noncommercial)
            });
            if nc_any {
                writeln!(
                    out,
                    "  community model: {}",
                    warn(
                        "NonCommercial (CC-BY-NC-SA-4.0) — you are notified at download; \
                         non-commercial use only"
                    )
                )?;
                col.push(
                    S::Warn,
                    "community model",
                    "NonCommercial (CC-BY-NC-SA-4.0) — non-commercial use only",
                );
            }

            // Wyoming wake serving is automatic, mirroring STT/TTS: whenever
            // the LAN server is enabled and this build can do wake detection,
            // Fono advertises + serves its LOCAL detector (audio stays on the
            // machine). The opt-in CLIENT direction (`[wakeword].wyoming` with
            // a uri) is the only path that leaks idle mic audio off-box, so it
            // gets the loud shared warning.
            if cfg!(feature = "wakeword-onnx") {
                if c.server.wyoming.enabled {
                    writeln!(
                        out,
                        "  Wyoming        : {} — local wake Detection served automatically over \
                         the LAN server; audio stays on this machine",
                        ok("served")
                    )?;
                    col.push(
                        S::Info,
                        "wyoming",
                        "served — local wake detection over the LAN server; audio stays on this machine",
                    );
                } else {
                    writeln!(
                        out,
                        "  Wyoming        : {} (enable `[server.wyoming]` to share wake on the LAN)",
                        dim("available")
                    )?;
                    col.push(
                        S::Info,
                        "wyoming",
                        "available (enable `[server.wyoming]` to share wake on the LAN)",
                    );
                }
            } else {
                writeln!(
                    out,
                    "  Wyoming        : {}",
                    dim("unavailable (wakeword-onnx not compiled into this build)")
                )?;
                col.push(
                    S::Info,
                    "wyoming",
                    "unavailable (wakeword-onnx not compiled into this build)",
                );
            }
            if w.wyoming.as_ref().is_some_and(WakeWyoming::is_client) {
                writeln!(
                    out,
                    "  Wyoming client : {} (idle mic audio leaves the machine)",
                    bad("CLIENT — opt-in")
                )?;
                writeln!(out, "  {}", bad(WakeWyoming::CLIENT_PRIVACY_WARNING))?;
                col.push(
                    S::Fail,
                    "wyoming client",
                    &format!(
                        "CLIENT mode (opt-in) — idle mic audio leaves the machine. {}",
                        WakeWyoming::CLIENT_PRIVACY_WARNING
                    ),
                );
            }
        } else {
            writeln!(
                out,
                "  enabled        : {} {}",
                dim("false"),
                dim("(default — no idle mic stream is opened; enable with \
                     `[wakeword].enabled = true`)")
            )?;
            col.push(
                S::Info,
                "enabled",
                "false (default — no idle mic stream is opened; enable with \
                 `[wakeword].enabled = true`)",
            );
        }
        writeln!(out)?;
    }
    // ----------------------------------------------------------------
    // Speaker verification — local voice biometrics (Slice 3 of
    // plans/2026-07-17-speaker-verification-v1.md). Honest field
    // reporting: enabled?, the selected registry model (and whether it
    // resolves), the enrolled-speaker count, and the threshold source.
    // Loudly warns when enabled with zero enrolled speakers (nothing
    // can ever match) — cheap + side-effect free: reads config and the
    // 0600 speakers store, no network, no daemon, no model load.
    // ----------------------------------------------------------------
    if let Some(c) = cfg.as_ref() {
        use fono_core::config::SpeakerThreshold;
        let sp = &c.speaker;
        writeln!(out, "{}", head("Speaker verification:"))?;
        col.section("Speaker verification");
        if sp.enabled {
            writeln!(out, "  enabled        : {}", ok("true"))?;
            col.push(S::Ok, "enabled", "true");

            // Selected registry model — flag an unknown name loudly so a
            // typo in `[speaker].model` is obvious.
            if let Some(m) = fono_audio::speaker::model(&sp.model) {
                writeln!(out, "  model          : {} ({})", ok(&sp.model), m.description)?;
                col.push(S::Ok, "model", &format!("{} ({})", sp.model, m.description));
            } else {
                writeln!(
                    out,
                    "  model          : {} (not in the registry — check `[speaker].model`)",
                    bad(&sp.model)
                )?;
                col.push(
                    S::Fail,
                    "model",
                    &format!("{} not in the registry — check `[speaker].model`", sp.model),
                );
            }

            let threshold = match sp.threshold {
                SpeakerThreshold::Auto => "auto (from calibration + shipped cohort)".to_string(),
                SpeakerThreshold::Fixed(t) => format!("pinned at {t:.3}"),
            };
            writeln!(out, "  threshold      : {}", dim(&threshold))?;
            col.push(S::Info, "threshold", &threshold);
            writeln!(out, "  min speech     : {}", dim(&format!("{:.1}s", sp.min_speech_secs)))?;
            col.push(S::Info, "min speech", &format!("{:.1}s", sp.min_speech_secs));

            // Enrolled-speaker count from the 0600 store. Enabled with
            // zero enrolled speakers is a mis-configuration: no utterance
            // can ever match, so every gated action silently fails closed.
            match fono_core::speakers::SpeakerStore::open(&paths.speakers_db())
                .and_then(|s| s.speaker_count())
            {
                Ok(0) => {
                    writeln!(
                        out,
                        "  enrolled       : {} (verification is on but no one is enrolled — \
                         nothing can match)",
                        warn("0")
                    )?;
                    col.push(
                        S::Warn,
                        "enrolled",
                        "0 — verification is on but no one is enrolled; nothing can match",
                    );
                }
                Ok(n) => {
                    writeln!(out, "  enrolled       : {}", ok(&format!("{n} speaker(s)")))?;
                    col.push(S::Ok, "enrolled", &format!("{n} speaker(s)"));
                }
                Err(e) => {
                    writeln!(out, "  enrolled       : {}", warn(&format!("unavailable ({e})")))?;
                    col.push(S::Warn, "enrolled", &format!("unavailable ({e})"));
                }
            }
        } else {
            writeln!(
                out,
                "  enabled        : {} {}",
                dim("false"),
                dim("(default — no voice biometrics; enable with `[speaker].enabled = true`)")
            )?;
            col.push(
                S::Info,
                "enabled",
                "false (default — no voice biometrics; enable with `[speaker].enabled = true`)",
            );
        }
        writeln!(out)?;
    }
    // ----------------------------------------------------------------
    // Coding agents — MCP server status
    // ----------------------------------------------------------------
    if let Some(c) = cfg.as_ref() {
        writeln!(out, "{}", head("Coding agents (MCP server):"))?;
        col.section("Coding agents (MCP)");
        if c.mcp.enabled {
            writeln!(out, "  mcp.enabled          : {}", ok("true"))?;
            col.push(S::Ok, "mcp", "enabled");
        } else {
            writeln!(
                out,
                "  mcp.enabled          : {} (enable with `fono use mcp-server on`)",
                warn("false")
            )?;
            col.push(S::Info, "mcp", "disabled (enable with `fono use mcp-server on`)");
        }
        writeln!(out, "  transport            : stdio")?;
        writeln!(
            out,
            "  tools available      : fono.speak, fono.listen, fono.confirm, fono.screen"
        )?;
        writeln!(
            out,
            "  voice preset         : assets/agent-presets/voice.md (see docs/coding-agents.md)"
        )?;
        writeln!(out)?;
    }

    writeln!(out)?;
    writeln!(out, "{} ({}):", head("Log tail"), paths.log_file().display())?;
    col.section("Log");
    match tail_log(&paths.log_file(), 10) {
        Ok(lines) if lines.is_empty() => {
            writeln!(out, "  {}", dim("(log is empty)"))?;
            col.push(S::Info, "tail", "(log is empty)");
        }
        Ok(lines) => {
            for line in &lines {
                writeln!(out, "  {line}")?;
            }
            col.push(S::Info, "tail", &lines.join("\n"));
        }
        Err(e) => {
            writeln!(out, "  {} {e}", bad("(cannot read log:"))?;
            col.push(S::Info, "tail", &format!("cannot read log: {e}"));
        }
    }

    let generated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Ok(DoctorReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        variant: variant.label().to_string(),
        generated_at,
        aggregate: aggregate(&col.sections),
        sections: col.sections,
        text: out,
    })
}

/// The detector backend `fono doctor` reports the daemon *would* build for an
/// enabled `[wakeword]`. Pure mirror of `crate::wake::build_detector`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakeBackend {
    /// Real openWakeWord three-stage ONNX detector.
    Onnx,
    /// Energy/VAD fallback stub.
    EnergyStub,
}

/// Inputs to [`wake_backend`] — the conditions the daemon's `build_detector`
/// keys off, grouped to keep the decision pure and the signature readable.
struct WakeBackendInputs {
    /// The `wakeword-onnx` feature is compiled into this build.
    onnx_compiled: bool,
    /// The shared melspectrogram + embedding graphs are present on disk.
    graphs_present: bool,
    /// Every configured phrase's classifier file is present on disk (which
    /// also implies at least one phrase is configured).
    all_classifiers_present: bool,
}

/// Decide which wake detector backend would run, purely from the inputs the
/// daemon's `build_detector` keys off. ONNX requires the feature compiled, the
/// shared graphs present, and every classifier present (the last already
/// implies a phrase is configured); anything missing falls back to the energy
/// stub. Kept pure + tested so doctor and the real loader can never disagree.
fn wake_backend(i: &WakeBackendInputs) -> WakeBackend {
    if i.onnx_compiled && i.graphs_present && i.all_classifiers_present {
        WakeBackend::Onnx
    } else {
        WakeBackend::EnergyStub
    }
}

/// Resolve where a phrase's classifier file lives in the wake-word cache,
/// reusing the registry's path resolution for known model ids and falling back
/// to the `<id>.ort` convention the ONNX loader uses for custom phrases. No
/// duplicated filename literals — the registry stays the single source.
fn wake_classifier_path(model: &str, cache_dir: &std::path::Path) -> std::path::PathBuf {
    match fono_audio::wake_registry::resolved_paths(model, cache_dir) {
        Some(r) => r.classifier,
        None => fono_audio::wake_registry::wakeword_dir(cache_dir).join(format!("{model}.ort")),
    }
}

/// Whether the shared melspectrogram + embedding graphs are present on disk
/// (both required before ONNX can load). Uses the registry's resolved paths.
fn wake_graphs_present(cache_dir: &std::path::Path) -> bool {
    fono_audio::wake_registry::resolved_paths("hey_fono", cache_dir)
        .is_some_and(|r| r.melspec.exists() && r.embedding.exists())
}

/// Read up to the last `n` lines of `path`. Preserves embedded ANSI.
fn tail_log(path: &std::path::Path, n: usize) -> std::io::Result<Vec<String>> {
    let data = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = data.lines().collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].iter().map(|s| (*s).to_string()).collect())
}

/// `fono doctor -f`: stream the log file forever via `tail -f`.
/// ANSI escapes pass through to the terminal unchanged.
pub async fn follow_log(paths: &Paths) -> Result<()> {
    let path = paths.log_file();
    println!();
    println!("Following {} (Ctrl-C to stop):", path.display());
    let status = tokio::process::Command::new("tail")
        .arg("-n")
        .arg("0")
        .arg("-F")
        .arg(&path)
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("tail exited with {status}");
    }
    Ok(())
}

/// Best-effort PATH lookup; mirrors fono-inject's `which` so doctor
/// reports the same set of clipboard tools the real fallback will try.
fn which_in_path(tool: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|p| p.join(tool).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Aggregate is the worst non-Info severity: Fail beats Warn beats Ok,
    /// and Info never counts.
    #[test]
    fn aggregate_orders_severities() {
        let sec = |sev: Severity| DoctorSection {
            title: "t".into(),
            checks: vec![DoctorCheck { label: "l".into(), detail: "d".into(), severity: sev }],
        };
        assert_eq!(aggregate(&[]), Severity::Ok);
        assert_eq!(aggregate(&[sec(Severity::Info)]), Severity::Ok);
        assert_eq!(aggregate(&[sec(Severity::Ok), sec(Severity::Info)]), Severity::Ok);
        assert_eq!(aggregate(&[sec(Severity::Ok), sec(Severity::Warn)]), Severity::Warn);
        assert_eq!(
            aggregate(&[sec(Severity::Warn), sec(Severity::Fail), sec(Severity::Ok)]),
            Severity::Fail
        );
    }

    /// SGR escapes are removed; plain text passes through untouched.
    #[test]
    fn strip_ansi_removes_sgr_only() {
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\x1b[32mready\x1b[0m"), "ready");
        assert_eq!(strip_ansi("a \x1b[31;1mFAIL\x1b[0m b"), "a FAIL b");
    }

    /// The collector groups checks under the most recent section and
    /// strips ANSI from details.
    #[test]
    fn collector_groups_and_strips() {
        let mut col = Collector::default();
        col.section("First");
        col.push(Severity::Ok, "a", "\x1b[32mfine\x1b[0m");
        col.section("Second");
        col.push(Severity::Warn, "b", "meh");
        assert_eq!(col.sections.len(), 2);
        assert_eq!(col.sections[0].checks[0].detail, "fine");
        assert_eq!(col.sections[1].checks[0].label, "b");
        assert_eq!(col.sections[1].checks[0].severity, Severity::Warn);
    }

    /// Severity serializes lowercase — the wire contract the web UI parses.
    #[test]
    fn severity_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Severity::Ok).unwrap(), "\"ok\"");
        assert_eq!(serde_json::to_string(&Severity::Warn).unwrap(), "\"warn\"");
        assert_eq!(serde_json::to_string(&Severity::Fail).unwrap(), "\"fail\"");
        assert_eq!(serde_json::to_string(&Severity::Info).unwrap(), "\"info\"");
    }

    /// ONNX is reported only when every precondition holds; missing any one
    /// of {feature, graphs, classifiers} falls back to the stub — the exact
    /// gate `wake::build_detector` applies.
    #[test]
    fn wake_backend_requires_all_preconditions() {
        let decide = |onnx, graphs, classifiers| {
            wake_backend(&WakeBackendInputs {
                onnx_compiled: onnx,
                graphs_present: graphs,
                all_classifiers_present: classifiers,
            })
        };
        assert_eq!(decide(true, true, true), WakeBackend::Onnx);
        // Any single missing input drops to the stub.
        assert_eq!(decide(false, true, true), WakeBackend::EnergyStub);
        assert_eq!(decide(true, false, true), WakeBackend::EnergyStub);
        assert_eq!(decide(true, true, false), WakeBackend::EnergyStub);
        assert_eq!(decide(false, false, false), WakeBackend::EnergyStub);
    }

    /// A registry model resolves to its registered classifier basename
    /// (`<id>.ort`); an unknown id falls back to the same `<id>.ort`
    /// convention. Both land under the wake-word cache dir (no duplicated
    /// filename literals).
    #[test]
    fn wake_classifier_path_uses_registry_then_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = fono_audio::wake_registry::wakeword_dir(tmp.path());
        // Known default model: registry-resolved basename.
        assert_eq!(wake_classifier_path("hey_fono", tmp.path()), dir.join("hey_fono.ort"));
        // Known community model: `.ort` conversion on the mirror (the loader
        // contract is always `<id>.ort`).
        assert_eq!(wake_classifier_path("hey_jarvis", tmp.path()), dir.join("hey_jarvis.ort"));
        // Unknown / custom phrase: `<id>.ort` fallback.
        assert_eq!(
            wake_classifier_path("my_custom_phrase", tmp.path()),
            dir.join("my_custom_phrase.ort")
        );
    }

    /// Graphs are absent on a fresh cache and present only when both `.ort`
    /// files exist.
    #[test]
    fn wake_graphs_present_needs_both_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!wake_graphs_present(tmp.path()), "fresh cache has no graphs");
        let dir = fono_audio::wake_registry::wakeword_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("melspectrogram.ort"), b"x").unwrap();
        assert!(!wake_graphs_present(tmp.path()), "melspec alone is not enough");
        std::fs::write(dir.join("embedding.ort"), b"x").unwrap();
        assert!(wake_graphs_present(tmp.path()), "both graphs present");
    }
}
