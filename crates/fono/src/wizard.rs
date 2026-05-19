// SPDX-License-Identifier: GPL-3.0-only
//! First-run interactive wizard. Phase 8 Tasks 8.1 & 8.2.
//!
//! Tier-aware: probes hardware first and recommends local-vs-cloud based on
//! what the host can sustain — see `docs/plans/2026-04-25-fono-local-default-v1.md`.
//!
//! Roadmap-v2 R3.2 + R3.3: cloud keys are validated against the provider's
//! `/v1/models` endpoint before persisting (so the user catches a typo
//! immediately, not on the first dictation), and the top-level path picker
//! offers a "Mixed" option that asks for STT and polish backends independently
//! (e.g. local STT + cloud polish).
//!
//! Wizard local-model plan: `plans/2026-04-28-wizard-local-model-selection-v1.md`.
//! The model picker is now data-driven from `WHISPER_MODELS`:
//!
//! 1. **Language scope first** — "English only?" before the model picker so
//!    `.en` variants are only shown to mono-lingual English users.
//! 2. **Hardware-aware shortlist** — `HardwareSnapshot::affords_model` gates
//!    each candidate; Unsuitable models are hidden, Borderline models appear
//!    with a friendly warning. The shortlist is capped to **3 entries** so
//!    new users aren't overwhelmed.
//! 3. **Friendly accuracy labels** — each item shows a quality bucket
//!    (Excellent / Good / Acceptable / Inaccurate) computed from the
//!    model's worst WER across the user's selected languages. Raw
//!    percentages are intentionally not surfaced.
//! 4. **Interactive mode question** — after the STT model is chosen, the
//!    wizard asks whether to enable live dictation, with a recommendation
//!    that factors in hardware acceleration (Apple Silicon Metal/CoreML)
//!    in addition to RAM/cores. On CPU-only Intel/AMD, live mode is only
//!    recommended for small or smaller models on machines that comfortably
//!    clear the streaming threshold.

use anyhow::{Context, Result};
use dialoguer::console::{Key, Term};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, MultiSelect, Select};
use fono_core::config::{
    AssistantBackend, AssistantCloud, Config, PolishBackend, PolishCloud, PolishLocal, Stt,
    SttBackend, SttCloud, SttLocal, TtsBackend, TtsCloud, TtsWyoming,
};
use fono_core::hwcheck::{Affordability, HardwareSnapshot, LocalTier};
use fono_core::locale::detect_user_languages_ranked;
use fono_core::provider_catalog::{CloudProvider, WebSearchSupport, CLOUD_PROVIDERS};
use fono_core::providers::{
    configured_tts_backends, parse_assistant_backend, parse_polish_backend, parse_stt_backend,
    parse_tts_backend,
};
use fono_core::{Paths, Secrets};
use fono_stt::registry::{ModelInfo, WHISPER_MODELS};
use std::time::Duration;

pub async fn run(paths: &Paths) -> Result<()> {
    println!(
        r"
  ┌──────────────────────────────────────────────────────────┐
  │  Fono — lightweight native voice dictation               │
  │  First-run setup                                         │
  └──────────────────────────────────────────────────────────┘
"
    );

    let theme = ColorfulTheme::default();

    // Load existing secrets so we can offer 'keep existing key' prompts
    // when the user re-runs the wizard with a key already on disk.
    let mut secrets = if paths.secrets_file().exists() {
        Secrets::load(&paths.secrets_file()).unwrap_or_default()
    } else {
        Secrets::default()
    };

    // ---------- Hardware probe + tier ----------
    let snap = fono_core::hwcheck::probe(&paths.cache_dir);
    let tier = snap.tier();
    print_hw_summary(&snap, tier);

    let path_choice = pick_path(&theme, tier, &snap)?;

    let mut config = Config::default();

    match path_choice {
        PathChoice::Local => configure_local(&theme, &mut config, &mut secrets, &snap).await?,
        PathChoice::Cloud => configure_cloud(&theme, &mut config, &mut secrets, &snap).await?,
        PathChoice::Customize => {
            configure_customize(&theme, &mut config, &mut secrets, &snap).await?;
        }
    }

    // Voice assistant — opt-in step. Independent of the dictation
    // pipeline above (the assistant uses its own backend selection
    // and a TTS layer that doesn't exist on the dictation path).
    configure_assistant(&theme, &mut config, &mut secrets).await?;

    config.save(&paths.config_file())?;
    if !secrets.keys.is_empty() {
        secrets.save(&paths.secrets_file())?;
    }

    // (Microphone picker removed — Fono now follows the OS default
    // unconditionally; users override via pavucontrol / GNOME / KDE
    // sound settings or the tray Microphone submenu at runtime.)

    // If the user chose any local backend (STT or LLM), download the
    // model(s) now (silently — re-checked on every daemon start).
    // Failures are non-fatal: the daemon will retry on next launch.
    if config.stt.backend == SttBackend::Local || config.polish.backend == PolishBackend::Local {
        if let Err(e) = crate::models::ensure_models(paths, &config).await {
            eprintln!("  (model download failed: {e:#} — the daemon will retry on next start)");
        }
    }
    if config.stt.backend == SttBackend::Local {
        // R3.1 — in-wizard latency probe. Run a 3-second synthetic clip
        // through the just-installed whisper to confirm the host can
        // sustain its tier budget. Surfaces "first dictation will be
        // slow" *before* the user presses the hotkey for the first time.
        probe_local_latency(paths, &config, tier).await;
    }

    println!("\n  Configuration saved to: {}", paths.config_file().display());
    println!(
        "  Hotkeys: {} (dictation), {} (assistant), {} (cancel) — \
         tap to toggle, hold for push-to-talk.",
        config.hotkeys.dictation, config.hotkeys.assistant, config.hotkeys.cancel,
    );
    println!("  Run `fono` to start the daemon, or `fono doctor` to diagnose your setup.\n");

    // If a daemon is already running, hot-reload it so the changes
    // take effect without a manual restart. Best-effort: the daemon
    // may not be running (first-run) or the IPC socket may not be
    // accepting connections; both surface as a friendly hint, not
    // an error.
    match fono_ipc::request_any(&paths.client_ipc_socket_candidates(), &fono_ipc::Request::Reload)
        .await
    {
        Ok(fono_ipc::Response::Ok | fono_ipc::Response::Discovered(_)) => {
            println!("  Daemon reloaded — new settings are live.");
        }
        Ok(fono_ipc::Response::Text(t)) => println!("  Daemon: {t}"),
        Ok(fono_ipc::Response::Error(e)) => {
            println!("  Daemon reload error: {e} — run `fono` to restart.");
        }
        Err(_) => {
            println!("  Daemon: not running (start it with `fono`).");
        }
    }
    Ok(())
}

// ─── Catalogue-driven helpers (Phase B, issues #9/#11) ────────────────────

/// Outcome of the primary-cloud-provider picker.
#[derive(Debug, Clone, Copy)]
enum PrimaryPick {
    /// User picked a catalogued provider — wizard walks its
    /// capability defaults from the catalogue.
    Catalogued(&'static CloudProvider),
    /// User picked the "Customize per capability (advanced)" entry —
    /// caller falls through to the [`configure_customize`] flow.
    Customize,
}

/// A catalogue provider is a viable *primary* pick if it offers LLM
/// cleanup (the substrate every assistant + dictation flow needs) AND
/// its factory wiring is complete. The Gemini LLM + assistant clients
/// are not yet implemented (see `fono-polish::factory` / `fono-assistant::factory`)
/// so Gemini is excluded — surfacing it would let the wizard pick a
/// configuration that fails at runtime.
fn is_primary_candidate(entry: &CloudProvider) -> bool {
    if entry.polish.is_none() {
        return false;
    }
    if parse_polish_backend(entry.id).is_none() {
        return false;
    }
    if entry.id == "gemini" {
        return false;
    }
    true
}

/// The catalogue advertises an assistant for several providers; only
/// those with a wired factory should appear in the assistant picker.
fn is_assistant_wired(entry: &CloudProvider) -> bool {
    entry.assistant.is_some() && parse_assistant_backend(entry.id).is_some() && entry.id != "gemini"
}

/// Catalogue entries with a wired assistant chat factory.
fn assistant_candidates() -> Vec<&'static CloudProvider> {
    CLOUD_PROVIDERS.iter().filter(|p| is_assistant_wired(p)).collect()
}

/// Header labels for the primary-cloud-provider picker's capability
/// columns. Order matches the per-row capability tuple emitted by
/// [`primary_capabilities`].
const PRIMARY_CAP_HEADERS: [&str; 6] = ["STT", "LLM", "Assistant", "TTS", "Vision", "Search"];

/// Per-capability column widths for the primary picker table. All
/// capability columns share a single uniform width = widest header
/// label (`Assistant`) + 2 trailing spaces, so the table reads as a
/// regular grid rather than a ragged set of columns sized to each
/// abbreviation. Width is always > 1, i.e. wide enough for the
/// single-character `✓`/`·` glyph.
fn primary_cap_widths() -> [usize; 6] {
    let max = PRIMARY_CAP_HEADERS.iter().map(|h| h.chars().count()).max().unwrap_or(0) + 2;
    [max; 6]
}

/// Six-tuple of capability-presence flags for a catalogue entry, in
/// the order rendered by [`primary_header`] / [`primary_row`].
/// Vision and Search are derived from runtime catalogue state
/// (`multimodal_model.is_some()` and `web_search != None`) rather
/// than from the static `badges` array, so a single catalogue edit
/// keeps the wizard, the assistant builder, and request-tool
/// injection in lockstep.
fn primary_capabilities(entry: &CloudProvider) -> [bool; 6] {
    let assistant = entry.assistant;
    let vision = assistant.and_then(|a| a.multimodal_model).is_some();
    let search = matches!(
        assistant.map(|a| a.web_search),
        Some(WebSearchSupport::NativeTool(_) | WebSearchSupport::Always)
    );
    [
        entry.stt.is_some(),
        entry.polish.is_some(),
        assistant.is_some(),
        entry.tts.is_some(),
        vision,
        search,
    ]
}

/// Aligned header row for the primary picker table. Printed once
/// **above** the `dialoguer::Select` so it scrolls with the prompt
/// rather than appearing as a non-selectable cursor item.
fn primary_header(provider_col_width: usize) -> String {
    use std::fmt::Write as _;
    let widths = primary_cap_widths();
    let mut s = format!("{:<w$}", "Provider", w = provider_col_width);
    for (i, h) in PRIMARY_CAP_HEADERS.iter().enumerate() {
        let _ = write!(s, "{:<w$}", h, w = widths[i]);
    }
    s.trim_end().to_string()
}

/// One aligned table row for the primary picker. Columns:
/// provider name (padded to `provider_col_width`), then one `✓`/`·`
/// glyph per capability in `primary_capabilities` order, each padded
/// to the header label width + 2 spaces.
fn primary_row(entry: &CloudProvider, provider_col_width: usize) -> String {
    use std::fmt::Write as _;
    let widths = primary_cap_widths();
    let caps = primary_capabilities(entry);
    let mut s = format!("{:<w$}", entry.display_name, w = provider_col_width);
    for (i, present) in caps.iter().enumerate() {
        let glyph = if *present { "✓" } else { "·" };
        let _ = write!(s, "{:<w$}", glyph, w = widths[i]);
    }
    s.trim_end().to_string()
}

/// Pre-seed the primary picker default cursor:
/// 1. If the existing config's polish backend matches a catalogue entry
///    that's still a primary candidate, prefer that.
/// 2. Else if any primary candidate has its key already in
///    `secrets.toml`, pick the first such candidate (catalogue order).
/// 3. Else default to OpenAI (broadest coverage).
fn default_primary_for_seed(
    candidates: &[&'static CloudProvider],
    cfg: &Config,
    secrets: &Secrets,
) -> usize {
    // 1. Match existing polish backend.
    let polish_id = fono_core::providers::polish_backend_str(&cfg.polish.backend);
    if let Some(i) = candidates.iter().position(|p| p.id == polish_id) {
        return i;
    }
    // 2. Match existing STT backend (cloud user with STT but no LLM yet).
    let stt_id = fono_core::providers::stt_backend_str(&cfg.stt.backend);
    if let Some(i) = candidates.iter().position(|p| p.id == stt_id) {
        return i;
    }
    // 3. First candidate with a key already in secrets.toml.
    if let Some(i) = candidates.iter().position(|p| secrets.has_in_file(p.key_env)) {
        return i;
    }
    // 4. OpenAI as the broadest-coverage fallback.
    candidates.iter().position(|p| p.id == "openai").unwrap_or(0)
}

/// Render the primary-cloud-provider picker as an aligned table. The
/// header row is printed once via `println!` so it scrolls with the
/// prompt; the `dialoguer::Select` carries only data rows + the
/// Customize escape hatch. See [`PrimaryPick`].
fn pick_primary_cloud_provider(
    theme: &ColorfulTheme,
    secrets: &Secrets,
    cfg: &Config,
) -> Result<PrimaryPick> {
    let candidates: Vec<&'static CloudProvider> =
        CLOUD_PROVIDERS.iter().filter(|p| is_primary_candidate(p)).collect();
    let provider_col_width = candidates
        .iter()
        .map(|p| p.display_name.chars().count())
        .max()
        .unwrap_or(0)
        .max("Customize".len())
        + 2;
    let default = default_primary_for_seed(&candidates, cfg, secrets);

    println!(
        "  Pick a primary cloud provider. One key, one walk — Fono fills in every\n  \
         capability that provider covers (STT · LLM · Assistant · TTS).\n"
    );
    println!("  {}", primary_header(provider_col_width));

    let mut rows: Vec<String> =
        candidates.iter().map(|p| primary_row(p, provider_col_width)).collect();
    rows.push(format!(
        "{:width$}Pick a backend per capability",
        "Customize",
        width = provider_col_width
    ));

    let idx = Select::with_theme(theme)
        .with_prompt("Primary cloud provider")
        .items(&rows)
        .default(default)
        .interact()
        .context("prompt")?;
    if idx == candidates.len() {
        return Ok(PrimaryPick::Customize);
    }
    Ok(PrimaryPick::Catalogued(candidates[idx]))
}

/// Phase D1 — pure helper that walks a catalogue entry and fills
/// every capability the entry offers into `config`. Mirrors the
/// outcome of [`configure_cloud`] + [`configure_assistant`]'s
/// collapsed-Confirm path: STT, LLM, TTS, and Assistant chat all
/// land on the primary provider when the entry supplies them.
///
/// Pure — no I/O, no secrets touched. Callers handle the key prompt
/// (or reuse) via [`seed_primary_secret`] / [`prompt_or_reuse_key`].
///
/// Skips capabilities the entry doesn't carry (e.g. Anthropic has no
/// `stt`) and capabilities the runtime hasn't wired yet (Gemini
/// chat). The assistant chat is only filled when
/// [`is_assistant_wired`] returns true for the entry — same rule the
/// wizard uses.
pub fn apply_primary_provider(config: &mut Config, entry: &CloudProvider) {
    if let Some(stt_def) = &entry.stt {
        if let Some(backend) = parse_stt_backend(entry.id) {
            config.stt = Stt {
                backend,
                local: SttLocal::default(),
                cloud: Some(SttCloud {
                    provider: entry.id.into(),
                    api_key_ref: entry.key_env.into(),
                    model: stt_def.model.into(),
                }),
                wyoming: None,
                prompts: std::collections::HashMap::new(),
            };
        }
    }
    if let Some(polish_def) = &entry.polish {
        if let Some(backend) = parse_polish_backend(entry.id) {
            config.polish.enabled = true;
            config.polish.backend = backend;
            config.polish.cloud = Some(PolishCloud {
                provider: entry.id.into(),
                api_key_ref: entry.key_env.into(),
                model: polish_def.model.into(),
            });
        }
    }
    if let Some(tts_def) = &entry.tts {
        if let Some(backend) = parse_tts_backend(entry.id) {
            config.tts.backend = backend;
            config.tts.cloud = Some(TtsCloud {
                provider: entry.id.into(),
                api_key_ref: entry.key_env.into(),
                model: tts_def.model.into(),
            });
            config.tts.voice = tts_def.default_voice.into();
        }
    }
    if let Some(adef) = entry.assistant {
        if is_assistant_wired(entry) {
            if let Some(backend) = parse_assistant_backend(entry.id) {
                config.assistant.enabled = true;
                config.assistant.backend = backend;
                config.assistant.cloud = Some(AssistantCloud {
                    provider: entry.id.into(),
                    api_key_ref: entry.key_env.into(),
                    model: adef.text_model.into(),
                });
            }
        }
    }
}

/// Phase D1 — pair to [`apply_primary_provider`]. Inserts a mocked
/// API key into `secrets` if the entry's `key_env` is not already
/// present, then returns whether a fresh insert happened
/// (`true`) or the existing key was reused (`false`). The boolean is
/// what the wizard surfaces as a `"reusing …"` notice in the real
/// flow.
///
/// Pure — no prompting, no validation. Production paths still go
/// through [`prompt_or_reuse_key`]; this helper exists for
/// integration-test setup where the prompt would block on stdin.
pub fn seed_primary_secret(secrets: &mut Secrets, entry: &CloudProvider, key: &str) -> bool {
    if secrets.has_in_file(entry.key_env) {
        return false;
    }
    secrets.insert(entry.key_env, key);
    true
}

/// Phase D1 — pure TTS-secondary helper. Sets `config.tts` to the
/// catalogue entry's defaults, leaving everything else alone. Used
/// by tests that exercise the "primary doesn't offer TTS, user picks
/// a secondary" scenario (e.g. Anthropic LLM + Cartesia TTS).
pub fn apply_secondary_tts(config: &mut Config, entry: &CloudProvider) {
    let Some(tts_def) = &entry.tts else { return };
    let Some(backend) = parse_tts_backend(entry.id) else {
        return;
    };
    config.tts.backend = backend;
    config.tts.cloud = Some(TtsCloud {
        provider: entry.id.into(),
        api_key_ref: entry.key_env.into(),
        model: tts_def.model.into(),
    });
    config.tts.voice = tts_def.default_voice.into();
}

/// Phase B5 — central key-reuse helper. If `secrets.toml` already
/// carries `key_env`, print one `"  reusing …"` line and return
/// without prompting. Otherwise prompt for a fresh key (with
/// validation) and print the provider's console URL.
async fn prompt_or_reuse_key(
    theme: &ColorfulTheme,
    secrets: &mut Secrets,
    key_env: &str,
    display_name: &str,
    console_url: &str,
) -> Result<()> {
    if secrets.has_in_file(key_env) {
        println!("  reusing {key_env} from secrets.toml for {display_name}");
        return Ok(());
    }
    if !console_url.is_empty() {
        println!("  Get one at {console_url}");
    }
    if let Some(k) = prompt_api_key_with_validation(theme, secrets, key_env).await? {
        secrets.insert(key_env, k);
    }
    Ok(())
}

/// Look up the catalogue entry whose `key_env` matches `key_env`.
/// Used by the Customize flow to recover the human-readable display
/// name + console URL from a bare env-var name.
fn catalogue_by_key_env(key_env: &str) -> Option<&'static CloudProvider> {
    CLOUD_PROVIDERS.iter().find(|p| p.key_env == key_env)
}

/// Convenience accessor for the Customize flow: given a `*_API_KEY`
/// string, returns (display_name, console_url), falling back to the
/// env-var name and an empty URL if the catalogue doesn't know it.
fn catalogue_meta_for_key(key_env: &str) -> (&'static str, &'static str) {
    catalogue_by_key_env(key_env)
        .map(|p| (p.display_name, p.console_url))
        .unwrap_or(("(unknown)", ""))
}

/// Catalogue entry matching the configured polish backend (used by the
/// assistant fast-path to determine whether the primary covers chat).
fn catalogue_for_llm_backend(b: &PolishBackend) -> Option<&'static CloudProvider> {
    let id = fono_core::providers::polish_backend_str(b);
    fono_core::provider_catalog::find(id)
}

/// Short display label for the currently-selected TTS backend, used
/// inside the assistant collapsed-Confirm prompt.
fn tts_short_label(b: &TtsBackend) -> &'static str {
    match b {
        TtsBackend::OpenAI => "OpenAI",
        TtsBackend::Groq => "Groq",
        TtsBackend::OpenRouter => "OpenRouter (OpenAI Mini TTS)",
        TtsBackend::Cartesia => "Cartesia",
        TtsBackend::Deepgram => "Deepgram",
        TtsBackend::Wyoming => "Wyoming",
        TtsBackend::None => "no",
    }
}

/// Human-readable chat-model label for the assistant picker table.
/// Falls back to the raw catalogue `text_model` string when an entry
/// is not in the hand-curated table — so a new catalogue entry never
/// produces an empty cell, only a less-pretty one.
fn humanize_chat_model(entry: &CloudProvider) -> String {
    let pretty = match entry.id {
        "openai" => Some("GPT-5.4 mini"),
        "anthropic" => Some("Claude Haiku 4.5"),
        "groq" => Some("GPT-OSS 120B"),
        "cerebras" => Some("Qwen 3 235B"),
        "gemini" => Some("Gemini 1.5 Flash"),
        "openrouter" => Some("Claude Haiku 4.5 (via OpenRouter)"),
        _ => None,
    };
    pretty
        .map(ToString::to_string)
        .or_else(|| entry.assistant.map(|a| a.text_model.to_string()))
        .unwrap_or_default()
}

/// Phase F7 — assistant TTS picker. Built from
/// [`configured_tts_backends`] so providers whose key is already in
/// `secrets.toml` lead. Falls through to a Wyoming URI prompt when
/// the user picks Wyoming, or to [`prompt_or_reuse_key`] for any
/// cloud provider whose key isn't yet stored.
enum TtsPickerAction {
    Cloud(&'static CloudProvider),
    Wyoming,
    Skip,
}

async fn pick_tts_for_assistant(
    theme: &ColorfulTheme,
    config: &mut Config,
    secrets: &mut Secrets,
) -> Result<()> {
    let has_wyoming = config.tts.wyoming.as_ref().is_some_and(|w| !w.uri.is_empty());
    let backends = configured_tts_backends(secrets, &TtsBackend::None, has_wyoming);

    let mut labels: Vec<String> = Vec::new();
    let mut actions: Vec<TtsPickerAction> = Vec::new();
    for b in backends {
        match b {
            TtsBackend::None => {}
            TtsBackend::Wyoming => {
                labels.push("Wyoming TTS server (LAN piper)".into());
                actions.push(TtsPickerAction::Wyoming);
            }
            _ => {
                let id = fono_core::providers::tts_backend_str(&b);
                let Some(entry) = fono_core::provider_catalog::find(id) else {
                    continue;
                };
                let has_key = secrets.has_in_file(entry.key_env);
                let key_part = if has_key { "key already set" } else { "will ask for key" };
                let extra = match entry.id {
                    "groq" => " — fastest",
                    "cartesia" => " — best quality",
                    "openrouter" => " — OpenAI Mini TTS / multilingual",
                    _ => "",
                };
                labels.push(format!("{} TTS (cloud, {key_part}){extra}", entry.display_name));
                actions.push(TtsPickerAction::Cloud(entry));
            }
        }
    }
    labels.push("Skip — text-only assistant (no audio reply)".into());
    actions.push(TtsPickerAction::Skip);

    let idx = Select::with_theme(theme)
        .with_prompt("Pick a TTS backend (assistant audio replies)")
        .items(&labels)
        .default(0)
        .interact()?;
    match &actions[idx] {
        TtsPickerAction::Skip => {
            config.tts.backend = TtsBackend::None;
        }
        TtsPickerAction::Wyoming => {
            let default_uri = config
                .tts
                .wyoming
                .as_ref()
                .map(|w| w.uri.clone())
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| fono_tts::defaults::DEFAULT_WYOMING_URI.into());
            let uri: String = Input::with_theme(theme)
                .with_prompt("Wyoming TTS server URI")
                .default(default_uri)
                .interact_text()
                .unwrap_or_else(|_| fono_tts::defaults::DEFAULT_WYOMING_URI.into());
            config.tts.backend = TtsBackend::Wyoming;
            config.tts.wyoming = Some(TtsWyoming { uri, ..TtsWyoming::default() });
        }
        TtsPickerAction::Cloud(entry) => {
            prompt_or_reuse_key(
                theme,
                secrets,
                entry.key_env,
                entry.display_name,
                entry.console_url,
            )
            .await?;
            let tdef = entry.tts.expect("filtered to TTS-capable entries");
            let backend = parse_tts_backend(entry.id).context("catalogue TTS id should parse")?;
            config.tts.backend = backend;
            config.tts.cloud = Some(TtsCloud {
                provider: entry.id.into(),
                api_key_ref: entry.key_env.into(),
                model: tdef.model.into(),
            });
            // Deepgram encodes the voice in the model id, so the
            // catalogue exposes an empty default_voice; suppressing
            // the voice prompt naturally falls out of writing the
            // catalogue value.
            config.tts.voice = tdef.default_voice.into();
        }
    }
    Ok(())
}

/// Optional final step — set up the voice assistant (toggle by default
/// → STT → chat → TTS → speakers). Skips entirely if the user declines.
/// All cloud key prompts reuse keys already present in `secrets` so a
/// re-run is non-destructive.
///
/// Phase B3 rewrite (issues #9/#11): the assistant chat backend is
/// chosen from the capability catalogue rather than a hard-coded
/// `match` block, and the TTS picker (F7) is built from
/// `configured_tts_backends` so providers whose key is already in
/// `secrets.toml` lead. When the user's primary cloud provider
/// already covers both assistant chat and a TTS backend (set by
/// `configure_cloud`), the prompt collapses to a single Confirm.
///
/// v0.8.1 (pre-release UX polish): the prior `MultiSelect` that
/// asked the user to opt into vision and web-search extras has been
/// removed. For the providers that surface either capability the
/// multimodal model is identical to the text model (zero cost) and
/// server-side web search is the kind of thing the user who sets up
/// an assistant in the first place almost certainly wants. Both
/// flags default to `true` in [`fono_core::config::Assistant`] and
/// can be flipped off in `config.toml` (`[assistant].prefer_vision
/// = false` / `prefer_web_search = false`) or via a future tray
/// submenu.
//
/// Build the "Extras:" info line shown in the assistant fast path.
/// Reports the *enabled* extras — i.e. catalogue support **and**
/// the corresponding `[assistant].prefer_*` flag — so the line
/// reflects runtime behaviour rather than catalogue capabilities.
/// Returns `None` when nothing is enabled so the caller can skip
/// rendering an empty line.
fn assistant_extras_summary(
    entry: &CloudProvider,
    assistant_cfg: &fono_core::config::Assistant,
) -> Option<String> {
    let adef = entry.assistant?;
    let mut parts: Vec<String> = Vec::new();
    if assistant_cfg.prefer_vision && adef.multimodal_model.is_some() {
        parts.push("vision (multimodal model)".to_string());
    }
    if assistant_cfg.prefer_web_search {
        match adef.web_search {
            WebSearchSupport::NativeTool(id) => {
                parts.push(format!("web search ({id})"));
            }
            WebSearchSupport::Always => {
                parts.push("web search (always grounded)".to_string());
            }
            WebSearchSupport::None => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

#[allow(clippy::too_many_lines)]
async fn configure_assistant(
    theme: &ColorfulTheme,
    config: &mut Config,
    secrets: &mut Secrets,
) -> Result<()> {
    println!();
    println!("  ── Voice assistant (optional) ──────────────────────────");
    println!(
        "  Press {} to ask a question and hear the answer through your speakers.",
        config.hotkeys.assistant
    );
    println!("  Independent of dictation cleanup — different model, different key.");

    // Fast path — when the primary cloud provider (inferred from the
    // currently-selected polish backend) covers assistant chat, auto-enable
    // the assistant (no Confirm). The user already committed to a cloud
    // key that supports it in `configure_cloud`; one more prompt was
    // redundant. Two sub-cases:
    //   1. TTS already set     → enable + 2 info lines + extras prompt.
    //   2. TTS not yet set     → enable + first info line, fall through
    //                            to `pick_tts_for_assistant` so the user
    //                            still gets `Skip — text-only assistant`
    //                            as an explicit escape hatch.
    let primary = catalogue_for_llm_backend(&config.polish.backend);
    let tts_already_set = !matches!(config.tts.backend, TtsBackend::None);
    if let Some(entry) = primary {
        if let Some(adef) = entry.assistant {
            if is_assistant_wired(entry) {
                let backend = parse_assistant_backend(entry.id)
                    .context("catalogue assistant id should parse")?;
                config.assistant.enabled = true;
                config.assistant.backend = backend;
                config.assistant.cloud = Some(AssistantCloud {
                    provider: entry.id.into(),
                    api_key_ref: entry.key_env.into(),
                    model: adef.text_model.into(),
                });
                // Key was prompted/reused in `configure_cloud`.
                let tts_label = if tts_already_set {
                    tts_short_label(&config.tts.backend).to_string()
                } else {
                    "no TTS yet".to_string()
                };
                println!(
                    "  Voice assistant enabled — {} chat + {} TTS.",
                    entry.display_name, tts_label
                );
                if let Some(extras) = assistant_extras_summary(entry, &config.assistant) {
                    println!("  Extras: {extras}.");
                }
                if tts_already_set {
                    println!(
                        "  Press {} to ask a question and hear the reply.",
                        config.hotkeys.assistant
                    );
                }
                // Extras (vision + web search) are default-on in
                // [`fono_core::config::Assistant`] — see
                // `assistant_extras_summary` above. The factory
                // re-checks the catalogue at startup so providers
                // without a multimodal model or native search tool
                // gracefully degrade.
                if !tts_already_set {
                    pick_tts_for_assistant(theme, config, secrets).await?;
                }
                return Ok(());
            }
        }
    }

    // Local-LLM users: no catalogue primary matches the current LLM
    // backend. Keep the Confirm so they can decline.
    let want = Confirm::with_theme(theme)
        .with_prompt("Enable the voice assistant?")
        .default(false)
        .interact()
        .unwrap_or(false);
    if !want {
        config.assistant.enabled = false;
        config.assistant.backend = AssistantBackend::None;
        // Do NOT clobber `tts.backend` — a returning user may have an
        // existing TTS backend they still want for future opt-in.
        return Ok(());
    }

    // ── Assistant chat backend (catalogue-driven) ─────────────────
    let candidates = assistant_candidates();
    // Order: providers with key already in `secrets.toml` first; among
    // each subgroup keep catalogue order so OpenAI/Anthropic/etc. lead.
    let (with_key, without_key): (Vec<_>, Vec<_>) =
        candidates.into_iter().partition(|p| secrets.has_in_file(p.key_env));
    let ordered: Vec<&'static CloudProvider> =
        with_key.iter().chain(without_key.iter()).copied().collect();

    // Render as an aligned three-column table (Provider | Model |
    // Key). The header is printed once via `println!` so it scrolls
    // with the prompt; the `Select` items carry only the rows + the
    // Skip escape hatch.
    let (rows, header) = assistant_picker_rows(&ordered, secrets);
    println!();
    println!("  {header}");
    let mut labels = rows;
    labels.push("Skip — disable assistant".into());
    let chat_idx = Select::with_theme(theme)
        .with_prompt("Pick an assistant chat backend")
        .items(&labels)
        .default(0)
        .interact()?;
    if chat_idx == ordered.len() {
        config.assistant.enabled = false;
        config.assistant.backend = AssistantBackend::None;
        return Ok(());
    }
    let entry = ordered[chat_idx];
    let adef = entry.assistant.expect("candidate has assistant");
    prompt_or_reuse_key(theme, secrets, entry.key_env, entry.display_name, entry.console_url)
        .await?;
    let backend =
        parse_assistant_backend(entry.id).context("catalogue assistant id should parse")?;
    config.assistant.enabled = true;
    config.assistant.backend = backend;
    config.assistant.cloud = Some(AssistantCloud {
        provider: entry.id.into(),
        api_key_ref: entry.key_env.into(),
        model: adef.text_model.into(),
    });

    // Extras (vision + web search) default on at the config layer
    // — see `Assistant::default()`. No prompt here.

    // ── TTS auto-pick / picker ────────────────────────────────────
    // If the user's assistant key also covers TTS (e.g. they picked
    // OpenAI for chat and OpenAI also offers TTS), reuse the same
    // provider/key for the spoken reply — no second prompt. Falls
    // through to the explicit TTS picker only when the assistant
    // provider has no TTS capability AND configure_cloud didn't set
    // one earlier.
    if !tts_already_set {
        if entry.tts.is_some() && parse_tts_backend(entry.id).is_some() {
            apply_secondary_tts(config, entry);
            println!(
                "  TTS: {} (same key as the assistant — no extra prompt).",
                entry.display_name
            );
        } else {
            pick_tts_for_assistant(theme, config, secrets).await?;
        }
    }
    Ok(())
}

/// Build the rendered rows + header for the slow-path assistant chat
/// picker. Columns: provider, human-readable model name, key status.
/// Widths are computed from the longest entry so future catalogue
/// additions stay aligned. Factored out for unit testing.
fn assistant_picker_rows(
    ordered: &[&'static CloudProvider],
    secrets: &Secrets,
) -> (Vec<String>, String) {
    use std::fmt::Write as _;

    const PROVIDER_HDR: &str = "Provider";
    const MODEL_HDR: &str = "Model";
    const KEY_HDR: &str = "Key";

    let provider_w = ordered
        .iter()
        .map(|p| p.display_name.chars().count())
        .max()
        .unwrap_or(0)
        .max(PROVIDER_HDR.len())
        + 2;
    let model_w = ordered
        .iter()
        .map(|p| humanize_chat_model(p).chars().count())
        .max()
        .unwrap_or(0)
        .max(MODEL_HDR.len())
        + 2;

    let mut header = format!("{PROVIDER_HDR:<provider_w$}{MODEL_HDR:<model_w$}{KEY_HDR}");
    header = header.trim_end().to_string();

    let mut rows = Vec::with_capacity(ordered.len());
    for p in ordered {
        let model = humanize_chat_model(p);
        let key = if secrets.has_in_file(p.key_env) { "set" } else { "missing" };
        let display = p.display_name;
        let mut row = String::new();
        let _ = write!(&mut row, "{display:<provider_w$}{model:<model_w$}{key}",);
        rows.push(row.trim_end().to_string());
    }
    (rows, header)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathChoice {
    Local,
    Cloud,
    /// Customize = pick STT and polish backends independently. Lets users
    /// run e.g. local whisper for privacy + cloud Cerebras for fast
    /// cleanup, or cloud Groq STT + skip-LLM (raw output) on a slow-CPU
    /// machine.
    Customize,
}

fn print_hw_summary(snap: &HardwareSnapshot, tier: LocalTier) {
    let ram_gb = snap.total_ram_bytes / (1024 * 1024 * 1024);
    let disk_gb = snap.free_disk_bytes / (1024 * 1024 * 1024);
    println!("  Detected hardware:");
    println!("    cores : {} physical / {} logical", snap.physical_cores, snap.logical_cores);
    println!(
        "    ram   : {ram_gb} GB total · disk free : {disk_gb} GB · platform : {}/{}",
        snap.os, snap.arch
    );
    println!("    accel : {}", snap.acceleration_summary());
    let blurb = match tier {
        LocalTier::Unsuitable => "  Local models look unsuitable for this machine — the wizard will default to cloud APIs.",
        LocalTier::Minimum => {
            "  This machine is on the lower end for local models — expect ~2 s per dictation."
        }
        LocalTier::Comfortable => {
            "  This machine handles local models well."
        }
        LocalTier::Recommended => {
            "  This machine runs local models smoothly."
        }
        LocalTier::HighEnd => "  Plenty of headroom for local models on this machine.",
    };
    println!("{blurb}\n");
}

fn pick_path(
    theme: &ColorfulTheme,
    tier: LocalTier,
    snap: &HardwareSnapshot,
) -> Result<PathChoice> {
    // Unsuitable: gate local behind explicit confirmation, default cloud.
    if tier == LocalTier::Unsuitable {
        if let Err(reason) = snap.suitability() {
            println!(
                "  Local models are below the supported floor on this machine: {reason}.\n  \
                 The wizard will default to cloud APIs."
            );
        }
        let want_local = Confirm::with_theme(theme)
            .with_prompt(
                "I understand my hardware is below the supported floor — show local anyway?",
            )
            .default(false)
            .interact()
            .unwrap_or(false);
        if want_local {
            return Ok(PathChoice::Local);
        }
        return Ok(PathChoice::Cloud);
    }

    // R3.3 — three top-level options in a fixed order, rendered as a
    // two-column table. Order is independent of hardware tier; only
    // the default cursor position tracks `tier.local_default()`.
    let mapping = [PathChoice::Local, PathChoice::Cloud, PathChoice::Customize];
    let rows = pick_path_rows();
    let default_idx = usize::from(!tier.local_default());

    let idx = Select::with_theme(theme)
        .with_prompt("Where should Fono run speech-to-text and polish?")
        .items(&rows)
        .default(default_idx)
        .interact()
        .context("prompt")?;
    Ok(mapping[idx])
}

/// Rows shown by `pick_path`'s two-column Select. Each row is
/// `"<name padded>  <description>"` where the pad width is computed
/// from the longest name + 2 spaces so future variants (e.g. `Hybrid`)
/// don't break alignment. Factored out for unit testing.
fn pick_path_rows() -> [String; 3] {
    let entries: [(&str, &str); 3] = [
        ("Local", "Private, offline, runs on this machine"),
        ("Cloud", "Fast, accurate, needs an API key"),
        ("Customize", "Pick a backend per capability"),
    ];
    let width = entries.iter().map(|(name, _)| name.len()).max().unwrap_or(0) + 2;
    [
        format!("{:<width$}{}", entries[0].0, entries[0].1, width = width),
        format!("{:<width$}{}", entries[1].0, entries[1].1, width = width),
        format!("{:<width$}{}", entries[2].0, entries[2].1, width = width),
    ]
}

async fn configure_local(
    theme: &ColorfulTheme,
    config: &mut Config,
    secrets: &mut Secrets,
    snap: &HardwareSnapshot,
) -> Result<()> {
    let tier = snap.tier();

    // Step 1 — English-only or multilingual?
    let english_only = pick_english_only(theme);

    // Step 2 — Language selection
    if english_only {
        config.general.languages = vec!["en".to_string()];
    } else {
        config.general.languages = pick_languages(theme)?;
    }

    // Step 3 — Pick a local STT model (language- and hardware-aware).
    let stt_model = pick_local_stt_model(theme, english_only, &config.general.languages, snap)?;
    config.stt = Stt {
        backend: SttBackend::Local,
        local: SttLocal { model: stt_model.into(), ..Default::default() },
        cloud: None,
        wyoming: None,
        prompts: std::collections::HashMap::new(),
    };

    // Live dictation stays off by default; users opt in later from
    // the tray's "Live dictation" toggle (no wizard prompt — the
    // question was confusing for first-run users).

    // Step 4 — polish choice. Default is **Skip**: dictation is
    // valuable on its own without an LLM rewrite step, and the user
    // can opt in later via `fono settings`. Cloud comes second
    // (cheap, fast, no model download). Local comes last and is only
    // marked "recommended" when the host has real LLM acceleration —
    // CPU-only inference on a 1.5 GB Qwen model is a frustrating
    // first-run experience.
    let polish_options = build_polish_options(snap);
    let llm_choice = Select::with_theme(theme)
        .with_prompt("Apply polish (filler-removal, capitalization, punctuation)?")
        .items(&polish_options)
        .default(0)
        .interact()?;

    match llm_choice {
        // Order matches `build_polish_options`: 0=Skip, 1=Cloud, 2=Local.
        0 => {
            config.polish.backend = PolishBackend::None;
            config.polish.enabled = false;
            config.polish.local = PolishLocal::default();
        }
        1 => configure_cloud_llm(theme, config, secrets).await?,
        _ => configure_local_llm(theme, config, tier)?,
    }
    Ok(())
}

/// Build the LLM-cleanup-choice menu items in the standard order
/// (Skip, Cloud, Local) with the "— recommended" suffix attached
/// only to the entry the wizard actively wants the user to pick.
/// Local gets the recommendation only when the host has hardware
/// acceleration that makes local inference comfortable; otherwise
/// Cloud picks up the suffix. Skip never carries the suffix —
/// it's the safe default but not "the wizard's pick".
fn build_polish_options(snap: &HardwareSnapshot) -> Vec<String> {
    let local_accelerated = host_has_llm_acceleration(snap);
    let local_label = if local_accelerated {
        "Local polish (qwen2.5, private, offline) — recommended"
    } else {
        "Local polish (qwen2.5, private, offline) — slow without GPU/Apple Silicon"
    };
    let cloud_label = if local_accelerated {
        "Cloud polish (Cerebras / Groq / OpenAI / Anthropic — needs key)"
    } else {
        "Cloud polish (Cerebras / Groq / OpenAI / Anthropic — needs key) — recommended"
    };
    vec![
        "Skip polish (raw whisper output)".to_string(),
        cloud_label.to_string(),
        local_label.to_string(),
    ]
}

/// Whether this host has the kind of acceleration that makes local
/// polish comfortable enough to recommend to a first-time user.
/// Today: Apple Silicon (Metal/CoreML) or a Vulkan-capable GPU.
/// CUDA / ROCm / NPU detection lands when those backends are wired.
fn host_has_llm_acceleration(snap: &HardwareSnapshot) -> bool {
    if snap.accelerated() {
        return true;
    }
    fono_core::vulkan_probe::probe().is_usable()
}

/// Cloud-path wizard. Phase B2 rewrite (issue #9): a single primary
/// provider picker walks the capability catalogue and configures
/// every capability that primary covers from one key entry. Users
/// who want per-capability granularity pick the
/// "Customize per capability (advanced)" entry to fall through to
/// the [`configure_customize`] flow.
async fn configure_cloud(
    theme: &ColorfulTheme,
    config: &mut Config,
    secrets: &mut Secrets,
    snap: &HardwareSnapshot,
) -> Result<()> {
    let pick = pick_primary_cloud_provider(theme, secrets, config)?;
    let entry = match pick {
        PrimaryPick::Customize => {
            return configure_customize(theme, config, secrets, snap).await;
        }
        PrimaryPick::Catalogued(e) => e,
    };

    // Single key entry (or reuse) for the primary provider.
    prompt_or_reuse_key(theme, secrets, entry.key_env, entry.display_name, entry.console_url)
        .await?;

    // Walk capabilities ----------------------------------------------
    if let Some(stt_def) = &entry.stt {
        let backend = parse_stt_backend(entry.id).context("catalogue STT id should parse")?;
        config.stt = Stt {
            backend,
            local: SttLocal::default(),
            cloud: Some(SttCloud {
                provider: entry.id.into(),
                api_key_ref: entry.key_env.into(),
                model: stt_def.model.into(),
            }),
            wyoming: None,
            prompts: std::collections::HashMap::new(),
        };
    } else {
        offer_secondary_stt(theme, config, secrets).await?;
    }

    if let Some(polish_def) = &entry.polish {
        let backend = parse_polish_backend(entry.id).context("catalogue LLM id should parse")?;
        config.polish.enabled = true;
        config.polish.backend = backend;
        config.polish.cloud = Some(PolishCloud {
            provider: entry.id.into(),
            api_key_ref: entry.key_env.into(),
            model: polish_def.model.into(),
        });
    }

    if let Some(tts_def) = &entry.tts {
        let backend = parse_tts_backend(entry.id).context("catalogue TTS id should parse")?;
        config.tts.backend = backend;
        config.tts.cloud = Some(TtsCloud {
            provider: entry.id.into(),
            api_key_ref: entry.key_env.into(),
            model: tts_def.model.into(),
        });
        config.tts.voice = tts_def.default_voice.into();
    }
    // Assistant chat is configured by `configure_assistant` (called
    // unconditionally from `run`), which inspects `config.polish.backend`
    // and the now-set TTS state to collapse to a single Confirm when
    // the primary covers both.

    // Language picker (interactive-mode prompt removed — tray toggle handles it).
    config.general.languages = pick_languages(theme)?;
    Ok(())
}

/// Secondary STT picker used when the primary cloud provider doesn't
/// offer transcription (e.g. Anthropic, Cerebras). Lists every
/// catalogue STT-capable provider, key-already-present first, plus a
/// "Skip" entry that falls back to local Whisper.
async fn offer_secondary_stt(
    theme: &ColorfulTheme,
    config: &mut Config,
    secrets: &mut Secrets,
) -> Result<()> {
    let mut keyed: Vec<&'static CloudProvider> = Vec::new();
    let mut unkeyed: Vec<&'static CloudProvider> = Vec::new();
    for p in CLOUD_PROVIDERS {
        if p.stt.is_none() || parse_stt_backend(p.id).is_none() {
            continue;
        }
        if secrets.has_in_file(p.key_env) {
            keyed.push(p);
        } else {
            unkeyed.push(p);
        }
    }
    let ordered: Vec<&'static CloudProvider> =
        keyed.iter().chain(unkeyed.iter()).copied().collect();
    let mut labels: Vec<String> = Vec::new();
    for p in &ordered {
        let key_part =
            if secrets.has_in_file(p.key_env) { "key already set" } else { "will ask for key" };
        let model = p.stt.expect("filtered").model;
        labels.push(format!("{} STT ({key_part}) — {}", p.display_name, model));
    }
    labels.push("Skip — fall back to local Whisper".into());
    let default = if keyed.is_empty() { labels.len() - 1 } else { 0 };
    let idx = Select::with_theme(theme)
        .with_prompt("Add speech-to-text from another provider?")
        .items(&labels)
        .default(default)
        .interact()?;
    if idx == ordered.len() {
        // Skip — leave stt as default (Local). The daemon will download
        // the default model on first run.
        return Ok(());
    }
    let entry = ordered[idx];
    prompt_or_reuse_key(theme, secrets, entry.key_env, entry.display_name, entry.console_url)
        .await?;
    let backend = parse_stt_backend(entry.id).expect("filtered");
    config.stt = Stt {
        backend,
        local: SttLocal::default(),
        cloud: Some(SttCloud {
            provider: entry.id.into(),
            api_key_ref: entry.key_env.into(),
            model: entry.stt.expect("filtered").model.into(),
        }),
        wyoming: None,
        prompts: std::collections::HashMap::new(),
    };
    Ok(())
}

/// R3.3 -- Customize path: ask STT and LLM independently, no coupling.
async fn configure_customize(
    theme: &ColorfulTheme,
    config: &mut Config,
    secrets: &mut Secrets,
    snap: &HardwareSnapshot,
) -> Result<()> {
    let tier = snap.tier();
    println!("  Mixed mode — pick speech-to-text and polish independently.\n");

    // ----- STT side -----
    let stt_options =
        &["Local whisper.cpp (private, offline)", "Cloud STT  (Groq / OpenAI / Deepgram / …)"];
    let stt_idx = Select::with_theme(theme)
        .with_prompt("Speech-to-text:")
        .items(stt_options)
        .default(usize::from(!tier.local_default()))
        .interact()?;

    if stt_idx == 0 {
        let english_only = pick_english_only(theme);
        if english_only {
            config.general.languages = vec!["en".to_string()];
        } else {
            config.general.languages = pick_languages(theme)?;
        }
        let stt_model = pick_local_stt_model(theme, english_only, &config.general.languages, snap)?;
        config.stt = Stt {
            backend: SttBackend::Local,
            local: SttLocal { model: stt_model.into(), ..Default::default() },
            cloud: None,
            wyoming: None,
            prompts: std::collections::HashMap::new(),
        };
    } else {
        configure_cloud_stt(theme, config, secrets).await?;
        config.general.languages = pick_languages(theme)?;
    }

    // Live dictation stays off by default; tray's "Live dictation"
    // toggle is the editing surface post-install.

    // ----- LLM side -----
    // Standard ordering (Skip, Cloud, Local) with hardware-aware
    // recommendation marker — see `build_polish_options` for the policy.
    let polish_options = build_polish_options(snap);
    let llm_idx = Select::with_theme(theme)
        .with_prompt("polish:")
        .items(&polish_options)
        .default(0)
        .interact()?;
    match llm_idx {
        0 => {
            config.polish.backend = PolishBackend::None;
            config.polish.enabled = false;
        }
        1 => configure_cloud_llm(theme, config, secrets).await?,
        _ => configure_local_llm(theme, config, tier)?,
    }

    Ok(())
}

// ─── Language scope ────────────────────────────────────────────────────────

/// Ask whether the user dictates only in English. This fast-path skips
/// the multi-language checkbox UI and allows the model picker to offer
/// more accurate `.en` variants. Renders as an arrow-key `Select`
/// (No / Yes) defaulting to **No** — the safer choice that opens the
/// full language picker; any user who really only dictates English
/// can flip the cursor in one keypress.
fn pick_english_only(theme: &ColorfulTheme) -> bool {
    let idx = Select::with_theme(theme)
        .with_prompt(
            "Will you dictate only in English? \
             (English-only models are smaller and a bit more accurate)",
        )
        .items(&["No", "Yes"])
        .default(0)
        .interact()
        .unwrap_or(0);
    idx == 1
}

/// Languages-you-dictate-in picker. Plan v3 task 7. Wizard skips the
/// picker entirely when ≥ 1 languages are reliably detected from the
/// OS; the tray's Languages submenu remains the primary editing
/// surface post-install. Only the zero-detection case falls back to
/// the manual MultiSelect from a curated common-language set with
/// `en` pre-checked. Returning an empty `Vec` is allowed (collapses
/// to unconstrained auto-detect at runtime).
fn pick_languages(theme: &ColorfulTheme) -> Result<Vec<String>> {
    // Single canonical curated list shared with the tray — picking
    // "English" here writes the exact same `general.languages = ["en"]`
    // that the tray's checkbox writes.
    let curated: &[(&str, &str)] = fono_core::languages::CURATED_LANGUAGES;
    // Ranked OS detection: collects signals from POSIX env, system
    // locale, formatting locales, keyboard layout, timezone, and
    // platform-native APIs. Any code with score ≥ 1 is considered a
    // real hint — the wizard skips the picker outright when the OS
    // surfaces at least one such code.
    let ranked = detect_user_languages_ranked();
    let detected_codes: Vec<String> = ranked.iter().map(|d| d.code.clone()).collect();

    if !detected_codes.is_empty() {
        let codes = finalise_detected_languages(&detected_codes);
        println!(
            "  Languages detected from your OS locale: {} — change later from the tray menu.",
            codes.join(", ")
        );
        return Ok(codes);
    }

    let os_codes: Vec<String> = detected_codes;
    let reasons_of = |code: &str| -> Option<String> {
        ranked
            .iter()
            .find(|d| d.code == code)
            .map(|d| d.reasons.iter().map(|k| k.label()).collect::<Vec<_>>().join(", "))
    };

    // Build the candidate list: curated first, plus any OS code missing
    // from curated (appended with a "(detected)" label). Codes are
    // de-duplicated; `(label, code, default_checked)` triples drive the
    // MultiSelect.
    let mut entries: Vec<(String, String, bool)> = Vec::new();
    for (code, name) in curated {
        let reasons = reasons_of(code);
        let label = reasons.as_deref().map_or_else(
            || format!("{name} ({code})"),
            |r| format!("{name} ({code}) — detected ({r})"),
        );
        // Pre-check every detected language plus English as a sensible
        // baseline (Fono's rerun logic works better with at least two
        // peers — see the bottom of this function).
        let checked = *code == "en" || reasons.is_some();
        entries.push((label, (*code).to_string(), checked));
    }
    for code in &os_codes {
        if !curated.iter().any(|(c, _)| c == code) {
            let reasons = reasons_of(code).unwrap_or_default();
            let label = if reasons.is_empty() {
                format!("{code} (detected)")
            } else {
                format!("{code} — detected ({reasons})")
            };
            entries.push((label, code.clone(), true));
        }
    }

    println!(
        "  Languages you dictate in (Fono treats every selection as an equal peer — \
         no primary). Press Space to toggle, Enter to confirm."
    );
    let labels: Vec<&str> = entries.iter().map(|(l, _, _)| l.as_str()).collect();
    let defaults: Vec<bool> = entries.iter().map(|(_, _, d)| *d).collect();
    let chosen = MultiSelect::with_theme(theme)
        .with_prompt("Languages")
        .items(&labels)
        .defaults(&defaults)
        .interact()?;

    let codes: Vec<String> = chosen.into_iter().map(|i| entries[i].1.clone()).collect();
    Ok(finalise_detected_languages(&codes))
}

/// Pure helper: applies the shared normalisation + bilingual-English
/// peer safety net used by both the OS-detected fast-path and the
/// manual MultiSelect fallback. Factored out for unit testability.
fn finalise_detected_languages(input: &[String]) -> Vec<String> {
    // Normalise via LanguageSelection so dedupe + lowercase rules apply
    // uniformly with the rest of the runtime.
    let normalised = fono_stt::LanguageSelection::from_config(input);
    let mut codes: Vec<String> = normalised.codes().to_vec();
    // If only one non-English language is present, silently add
    // English as a peer. Without it, a single-entry allow-list would
    // cause the rerun mechanism to force that language on any clip
    // that Groq auto-detects as something outside the list —
    // including genuine English speech — producing garbled output.
    // English as a peer is harmless for speakers who never use it
    // (it will simply never be detected) and essential for bilingual
    // users.
    if codes.len() == 1 && codes[0] != "en" {
        codes.push("en".to_string());
    }
    codes
}

// ─── Local STT model picker ────────────────────────────────────────────────

/// Friendly accuracy bucket derived from the model's worst WER across the
/// user's selected languages. Surfaces quality without showing raw
/// percentages or technical jargon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccuracyBucket {
    Excellent,
    Good,
    Acceptable,
    /// Worst-case WER above 15% on at least one selected language.
    Inaccurate,
    /// No published benchmark for any of the user's languages.
    Unknown,
}

impl AccuracyBucket {
    fn label(self) -> &'static str {
        match self {
            Self::Excellent => "Excellent",
            Self::Good => "Good",
            Self::Acceptable => "Acceptable",
            Self::Inaccurate => "Inaccurate",
            Self::Unknown => "Untested",
        }
    }
}

/// Compute the accuracy bucket for a model given the user's selected
/// languages. Returns `Unknown` if none of the languages have published
/// WER data; otherwise returns the bucket corresponding to the *worst*
/// (highest) WER — the model is only as accurate as its weakest selected
/// language.
fn accuracy_for_langs(model: &ModelInfo, langs: &[String]) -> AccuracyBucket {
    let langs: &[String] = if langs.is_empty() {
        // English-only path passes an empty list before language selection;
        // fall back to English so we still produce a meaningful bucket.
        &[]
    } else {
        langs
    };
    let worst = if langs.is_empty() {
        model.wer_by_lang.iter().find(|(l, _)| *l == "en").map(|&(_, w)| w)
    } else {
        langs
            .iter()
            .filter_map(|lang| model.wer_by_lang.iter().find(|(l, _)| l == lang).map(|&(_, w)| w))
            .fold(None, |acc, w| Some(acc.map_or(w, |a: f32| a.max(w))))
    };
    match worst {
        None => AccuracyBucket::Unknown,
        Some(w) if w <= 6.0 => AccuracyBucket::Excellent,
        Some(w) if w <= 10.0 => AccuracyBucket::Good,
        Some(w) if w <= 15.0 => AccuracyBucket::Acceptable,
        Some(_) => AccuracyBucket::Inaccurate,
    }
}

/// A candidate model for the wizard shortlist.
pub struct ShortlistEntry {
    pub model: &'static ModelInfo,
    pub affordability: Affordability,
    pub accuracy: AccuracyBucket,
}

/// Maximum number of model choices shown in the wizard. New users get
/// overwhelmed by long lists; three covers "fastest / balanced / best".
pub const SHORTLIST_MAX: usize = 3;

/// Build an ordered shortlist of whisper models for this hardware + language
/// scope. Excluded:
///
/// - models the hardware cannot load at all (`Affordability::Unsuitable`),
/// - models whose accuracy is `Inaccurate` for the selected languages
///   (worst WER > 15%) — unless every remaining candidate is also
///   Inaccurate, in which case we keep them as a fallback.
///
/// Within the shortlist, entries are sorted by:
///
/// 1. Affordability (Comfortable before Borderline);
/// 2. Accuracy (Excellent → Good → Acceptable);
/// 3. Largest-first (better quality first).
///
/// Capped at [`SHORTLIST_MAX`] entries so the picker stays uncluttered.
///
/// This is a pure function — no I/O, no TTY. Unit-testable directly.
pub fn build_local_stt_shortlist(
    english_only: bool,
    langs: &[String],
    snap: &HardwareSnapshot,
) -> Vec<ShortlistEntry> {
    let candidates: Vec<ShortlistEntry> = WHISPER_MODELS
        .iter()
        .filter(|m| m.multilingual != english_only)
        .filter_map(|m| {
            let aff = snap.affords_model(m.min_ram_mb, m.approx_mb, m.realtime_factor_cpu_avx2);
            if aff == Affordability::Unsuitable {
                None
            } else {
                Some(ShortlistEntry {
                    model: m,
                    affordability: aff,
                    accuracy: accuracy_for_langs(m, langs),
                })
            }
        })
        .collect();

    // Drop "Inaccurate" entries unless every candidate is Inaccurate
    // (keeps the wizard usable even on language combinations where no
    // model meets the 15% threshold).
    let any_acceptable = candidates.iter().any(|e| e.accuracy != AccuracyBucket::Inaccurate);
    let mut entries: Vec<ShortlistEntry> = if any_acceptable {
        candidates.into_iter().filter(|e| e.accuracy != AccuracyBucket::Inaccurate).collect()
    } else {
        candidates
    };

    entries.sort_by(|a, b| {
        let aff_order = |aff: &Affordability| match aff {
            Affordability::Comfortable => 0,
            Affordability::Borderline => 1,
            Affordability::Unsuitable => 2,
        };
        let acc_order = |acc: &AccuracyBucket| match acc {
            AccuracyBucket::Excellent => 0,
            AccuracyBucket::Good => 1,
            AccuracyBucket::Acceptable => 2,
            AccuracyBucket::Unknown => 3,
            AccuracyBucket::Inaccurate => 4,
        };
        aff_order(&a.affordability)
            .cmp(&aff_order(&b.affordability))
            .then_with(|| acc_order(&a.accuracy).cmp(&acc_order(&b.accuracy)))
            .then(b.model.approx_mb.cmp(&a.model.approx_mb))
    });

    entries.truncate(SHORTLIST_MAX);
    entries
}

/// Format the size field for display: `"~466 MB"` or `"~1.6 GB"`.
fn size_label(approx_mb: u32) -> String {
    if approx_mb >= 1_000 {
        format!("~{:.1} GB", approx_mb as f32 / 1_000.0)
    } else {
        format!("~{approx_mb} MB")
    }
}

/// Friendly model display name. Hides internal whisper variant names
/// (`large-v3-turbo`, `small.en`) behind a more approachable label that
/// fits the way most users think about model size.
fn friendly_model_label(model: &ModelInfo) -> &'static str {
    match model.name {
        "tiny" | "tiny.en" => "Tiny (fastest, lowest quality)",
        "small" | "small.en" => "Small (balanced quality)",
        "large-v3-turbo" => "Turbo (best quality, needs more memory)",
        other => other,
    }
}

/// Data-driven local STT model picker. Replaces the hard-coded tier match.
///
/// Shows at most [`SHORTLIST_MAX`] models matching the language scope
/// (`.en` for English-only, multilingual otherwise). Borderline models
/// appear with a friendly "may lag in live mode" suffix. The default
/// cursor points at the highest-ranked entry (Comfortable + best
/// accuracy first).
fn pick_local_stt_model(
    theme: &ColorfulTheme,
    english_only: bool,
    langs: &[String],
    snap: &HardwareSnapshot,
) -> Result<&'static str> {
    let shortlist = build_local_stt_shortlist(english_only, langs, snap);

    if shortlist.is_empty() {
        // Edge case: every visible model is Unsuitable. Fall back to the
        // smallest model in the correct language family.
        let fallback = WHISPER_MODELS
            .iter()
            .filter(|m| m.multilingual != english_only)
            .min_by_key(|m| m.approx_mb);
        if let Some(m) = fallback {
            eprintln!(
                "  No model fits comfortably on your machine — falling back to '{}' ({}).",
                friendly_model_label(m),
                size_label(m.approx_mb)
            );
            return Ok(m.name);
        }
        anyhow::bail!("no local speech-to-text models available for the selected languages");
    }

    // Single-option fast path: when the shortlist collapses to one
    // entry (typically a low-RAM machine where only `tiny` fits, or
    // English-only with one acceptable accuracy bucket), don't make
    // the user press Enter on a list of one. Auto-pick and announce.
    if shortlist.len() == 1 {
        let entry = &shortlist[0];
        let label = friendly_model_label(entry.model);
        let size = size_label(entry.model.approx_mb);
        let acc = entry.accuracy.label();
        eprintln!(
            "  Picking '{label}', {size} — accuracy: {acc} \
             (only model that fits this machine + language selection)."
        );
        return Ok(entry.model.name);
    }

    // Build the Select items with friendly labels.
    let items: Vec<String> = shortlist
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let label = friendly_model_label(entry.model);
            let size = size_label(entry.model.approx_mb);
            let acc = entry.accuracy.label();
            let warn = match entry.affordability {
                Affordability::Comfortable => "",
                Affordability::Borderline => " — may lag in live mode on this machine",
                Affordability::Unsuitable => unreachable!("filtered above"),
            };
            // Only tag the top entry as (recommended) when it is
            // genuinely Comfortable; a Borderline entry at position 0
            // already carries a "may lag" suffix, so adding
            // "(recommended)" alongside it would contradict itself.
            let default_tag = if i == 0 && matches!(entry.affordability, Affordability::Comfortable)
            {
                "  (recommended)"
            } else {
                ""
            };
            format!("{label}, {size} — accuracy: {acc}{warn}{default_tag}")
        })
        .collect();

    println!(
        "  Pick a speech-to-text model. Smaller = faster; larger = more accurate.\n  \
         Accuracy ratings reflect typical real-world dictation across your selected languages.\n"
    );

    let idx = Select::with_theme(theme)
        .with_prompt("Pick a speech-to-text model")
        .items(&items)
        .default(0)
        .interact()?;

    Ok(shortlist[idx].model.name)
}

// ─── LLM configuration helpers ─────────────────────────────────────────────

/// Tier-aware local LLM model picker. Sets `config.polish` to the chosen
/// `PolishBackend::Local` + matching `PolishLocal` defaults. The model file
/// is downloaded later by `ensure_models` once the wizard finishes.
fn configure_local_llm(theme: &ColorfulTheme, config: &mut Config, tier: LocalTier) -> Result<()> {
    let (items, models, default_idx) = match tier {
        LocalTier::HighEnd => (
            vec![
                "qwen2.5-3b-instruct  (~2.0 GB) — recommended for your machine",
                "qwen2.5-1.5b-instruct (~1.0 GB) — lighter",
                "qwen2.5-0.5b-instruct (~350 MB) — lightest",
            ],
            vec!["qwen2.5-3b-instruct", "qwen2.5-1.5b-instruct", "qwen2.5-0.5b-instruct"],
            0usize,
        ),
        LocalTier::Recommended | LocalTier::Comfortable => (
            vec![
                "qwen2.5-1.5b-instruct (~1.0 GB) — recommended for your machine",
                "qwen2.5-0.5b-instruct (~350 MB) — lighter (faster, lower quality)",
                "qwen2.5-3b-instruct  (~2.0 GB) — slower but higher quality",
            ],
            vec!["qwen2.5-1.5b-instruct", "qwen2.5-0.5b-instruct", "qwen2.5-3b-instruct"],
            0usize,
        ),
        LocalTier::Minimum | LocalTier::Unsuitable => (
            vec![
                "qwen2.5-0.5b-instruct (~350 MB) — recommended for your machine",
                "qwen2.5-1.5b-instruct (~1.0 GB) — slower but higher quality",
            ],
            vec!["qwen2.5-0.5b-instruct", "qwen2.5-1.5b-instruct"],
            0usize,
        ),
    };
    let idx = Select::with_theme(theme)
        .with_prompt("Pick a local LLM model")
        .items(&items)
        .default(default_idx)
        .interact()?;
    config.polish.backend = PolishBackend::Local;
    config.polish.enabled = true;
    config.polish.local = PolishLocal { model: models[idx].into(), ..PolishLocal::default() };
    config.polish.cloud = None;
    Ok(())
}

async fn configure_cloud_stt(
    theme: &ColorfulTheme,
    config: &mut Config,
    secrets: &mut Secrets,
) -> Result<()> {
    let stt_providers = &[
        "Groq (whisper-large-v3-turbo, fastest) — recommended",
        "OpenAI (whisper-1)",
        "Deepgram (streaming)",
        "Cartesia",
        "AssemblyAI",
    ];
    let stt_idx = Select::with_theme(theme)
        .with_prompt("Pick a cloud speech-to-text provider")
        .items(stt_providers)
        .default(0)
        .interact()?;
    let (stt_backend, stt_key_name, stt_default_model) = match stt_idx {
        0 => (SttBackend::Groq, "GROQ_API_KEY", "whisper-large-v3-turbo"),
        1 => (SttBackend::OpenAI, "OPENAI_API_KEY", "whisper-1"),
        2 => (SttBackend::Deepgram, "DEEPGRAM_API_KEY", "nova-2"),
        3 => (SttBackend::Cartesia, "CARTESIA_API_KEY", "sonic-transcribe"),
        _ => (SttBackend::AssemblyAI, "ASSEMBLYAI_API_KEY", "best"),
    };
    // Phase B5: every cloud key prompt routes through prompt_or_reuse_key
    // so re-runs print one "reusing…" line instead of re-asking.
    let (display, console) = catalogue_meta_for_key(stt_key_name);
    prompt_or_reuse_key(theme, secrets, stt_key_name, display, console).await?;

    config.stt.backend = stt_backend.clone();
    // Streaming for cloud Groq is auto-on whenever the user enabled
    // live mode (`[interactive].enabled = true`); there is no separate
    // per-backend opt-in. Wizard removed the third question in v0.3.5
    // (plan `2026-04-29-streaming-config-collapse-v1.md`).
    config.stt.cloud = Some(SttCloud {
        provider: stt_key_name.trim_end_matches("_API_KEY").to_lowercase(),
        api_key_ref: stt_key_name.into(),
        model: stt_default_model.into(),
    });
    Ok(())
}

async fn configure_cloud_llm(
    theme: &ColorfulTheme,
    config: &mut Config,
    secrets: &mut Secrets,
) -> Result<()> {
    let llm_providers = &[
        "Cerebras (llama3.1-8b, < 1s latency) — recommended",
        "Groq (openai/gpt-oss-20b)",
        "OpenAI (gpt-5.4-nano)",
        "Anthropic (claude-haiku-4-5)",
        "Skip polish",
    ];
    let llm_idx = Select::with_theme(theme)
        .with_prompt("Pick a cloud LLM for cleanup")
        .items(llm_providers)
        .default(0)
        .interact()?;

    if llm_idx == 4 {
        config.polish.backend = PolishBackend::None;
        config.polish.enabled = false;
        return Ok(());
    }
    let (backend, key_name, model) = match llm_idx {
        0 => (PolishBackend::Cerebras, "CEREBRAS_API_KEY", "llama3.1-8b"),
        1 => (PolishBackend::Groq, "GROQ_API_KEY", "openai/gpt-oss-20b"),
        2 => (PolishBackend::OpenAI, "OPENAI_API_KEY", "gpt-5.4-nano"),
        _ => (PolishBackend::Anthropic, "ANTHROPIC_API_KEY", "claude-haiku-4-5-20251001"),
    };
    let (display, console) = catalogue_meta_for_key(key_name);
    prompt_or_reuse_key(theme, secrets, key_name, display, console).await?;
    config.polish.backend = backend;
    config.polish.enabled = true;
    config.polish.cloud = Some(PolishCloud {
        provider: key_name.trim_end_matches("_API_KEY").to_lowercase(),
        api_key_ref: key_name.into(),
        model: model.into(),
    });
    Ok(())
}

// ─── API key helpers ────────────────────────────────────────────────────────

/// R3.2 — wraps `prompt_api_key` with a reachability probe. If the user
/// types a new key, validate it against the provider before persisting.
/// Persists invalid keys only on explicit confirmation (so the user can
/// continue offline / behind a proxy / on a flaky network).
async fn prompt_api_key_with_validation(
    theme: &ColorfulTheme,
    secrets: &Secrets,
    key_name: &str,
) -> Result<Option<String>> {
    let Some(new_key) = prompt_api_key(theme, secrets, key_name)? else {
        return Ok(None);
    };
    print!("  received {key_name} ({} chars); validating … ", new_key.chars().count());
    let _ = std::io::Write::flush(&mut std::io::stdout());
    match validate_cloud_key(key_name, &new_key).await {
        Ok(()) => {
            println!("OK");
            Ok(Some(new_key))
        }
        Err(e) => {
            println!("FAILED ({e:#})");
            let save_anyway = Confirm::with_theme(theme)
                .with_prompt("Save this key anyway? (offline / behind proxy / try later)")
                .default(false)
                .interact()
                .unwrap_or(false);
            if save_anyway {
                Ok(Some(new_key))
            } else {
                Ok(None)
            }
        }
    }
}

/// Probe the provider's `/v1/models` (or equivalent authed endpoint)
/// with a 5 s timeout and assert a 2xx status. Returns `Err` on auth
/// failure, network failure, or non-2xx response.
async fn validate_cloud_key(key_name: &str, key: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .user_agent("fono-wizard/0.1")
        .build()
        .context("build http client")?;
    let req = match key_name {
        "GROQ_API_KEY" => client.get("https://api.groq.com/openai/v1/models").bearer_auth(key),
        "OPENAI_API_KEY" => client.get("https://api.openai.com/v1/models").bearer_auth(key),
        "CEREBRAS_API_KEY" => client.get("https://api.cerebras.ai/v1/models").bearer_auth(key),
        "OPENROUTER_API_KEY" => client
            // OpenRouter's auth-check endpoint: returns 200 with the
            // key's tier/credit metadata when authenticated, 401 when
            // the key is invalid. Preferred over `/v1/models` because
            // that route is unauthenticated on OpenRouter (it lists
            // every available model regardless of key validity), so it
            // can't actually verify the key.
            //
            // Attribution headers are attached here too so the first
            // request a fresh user makes against OpenRouter already
            // creates Fono's public app page (per
            // <https://openrouter.ai/docs/app-attribution>: "without
            // [HTTP-Referer], no app page will be created").
            .get("https://openrouter.ai/api/v1/auth/key")
            .bearer_auth(key)
            .header("HTTP-Referer", fono_core::openrouter_attribution::REFERER)
            .header("X-OpenRouter-Title", fono_core::openrouter_attribution::TITLE)
            .header("X-OpenRouter-Categories", fono_core::openrouter_attribution::CATEGORIES),
        "GEMINI_API_KEY" => client
            // Gemini authenticates via `?key=` query parameter rather
            // than a bearer header; the models list returns 200 for
            // valid keys and 400/403 for invalid ones.
            .get(format!("https://generativelanguage.googleapis.com/v1beta/models?key={key}")),
        "ANTHROPIC_API_KEY" => client
            .get("https://api.anthropic.com/v1/models")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        "DEEPGRAM_API_KEY" => client
            .get("https://api.deepgram.com/v1/projects")
            .header("Authorization", format!("Token {key}")),
        "ASSEMBLYAI_API_KEY" => {
            client.get("https://api.assemblyai.com/v2/transcript").header("Authorization", key)
        }
        "CARTESIA_API_KEY" => client
            .get("https://api.cartesia.ai/voices")
            .header("X-API-Key", key)
            .header("Cartesia-Version", "2024-06-10"),
        other => {
            // Unknown provider — skip validation.
            anyhow::bail!("no validation endpoint configured for {other}; key not validated")
        }
    };
    let resp = req.send().await.with_context(|| format!("connect to {key_name} provider"))?;
    let status = resp.status();
    let request_id = fono_http::provider_request_id(resp.headers())
        .map(str::to_owned)
        .unwrap_or_else(|| "<none>".to_string());
    // Drain & discard the body so the connection returns to the pool.
    let _ = resp.bytes().await;
    tracing::debug!(
        target: "fono.http",
        stage = "wizard",
        provider = key_name,
        status = status.as_u16(),
        request_id = %request_id,
        "wizard key validation"
    );
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!("{status} (key rejected)");
    }
    if !status.is_success() {
        anyhow::bail!("{status} (provider returned non-success)");
    }
    Ok(())
}

/// Prompt for an API key. If one already exists in `secrets`, ask whether
/// to keep it (default = yes). Returns `Some(new_key)` only when the user
/// types something new; `None` means "keep existing or leave unset".
fn prompt_api_key(
    theme: &ColorfulTheme,
    secrets: &Secrets,
    key_name: &str,
) -> Result<Option<String>> {
    if secrets.keys.contains_key(key_name) {
        let keep = Confirm::with_theme(theme)
            .with_prompt(format!("Existing {key_name} found — keep it?"))
            .default(true)
            .interact()
            .unwrap_or(true);
        if keep {
            return Ok(None);
        }
    }
    prompt_api_key_force(theme, key_name)
}

/// Always prompt for an API key. Empty input -> `None` (key left unset).
fn prompt_api_key_force(_theme: &ColorfulTheme, key_name: &str) -> Result<Option<String>> {
    let k = prompt_masked_api_key(key_name)?;
    if k.is_empty() {
        Ok(None)
    } else {
        Ok(Some(k))
    }
}

/// Read an API key with masked live feedback.
///
/// `dialoguer::Password` intentionally echoes nothing while typing/pasting,
/// which makes large pasted provider keys feel like they did not land. This
/// keeps the secret hidden but prints one `*` per accepted character so the
/// user gets immediate confirmation that paste/input is being captured.
fn prompt_masked_api_key(key_name: &str) -> Result<String> {
    let term = Term::stderr();
    anyhow::ensure!(term.is_term(), "API key prompt requires an interactive terminal");

    term.write_str(&format!("? Paste your {key_name} (stored mode 0600, leave empty to skip) "))?;
    term.flush()?;

    let mut key = String::new();
    loop {
        match term.read_key()? {
            Key::Enter => {
                term.write_line("")?;
                break;
            }
            Key::CtrlC | Key::Escape => anyhow::bail!("setup cancelled while entering {key_name}"),
            Key::Backspace => {
                if key.pop().is_some() {
                    term.clear_chars(1)?;
                    term.flush()?;
                }
            }
            Key::Char(ch) if !ch.is_control() => {
                key.push(ch);
                term.write_str("*")?;
                term.flush()?;
            }
            _ => {}
        }
    }

    Ok(key)
}

// ─── Local STT latency probe ───────────────────────────────────────────────

/// Tier-specific p50 budget for transcribing a 3-second clip with the
/// recommended whisper model on that tier. Numbers come from the latency
/// budget table in `docs/plans/2026-04-25-fono-latency-v1.md`. The probe
/// uses these as soft thresholds: exceeding them prints a warning, not a
/// hard fail, because real-world variance can be wide on first run.
fn tier_latency_budget_ms(tier: LocalTier) -> u128 {
    match tier {
        LocalTier::HighEnd => 600,
        LocalTier::Recommended => 1000,
        LocalTier::Comfortable => 2000,
        LocalTier::Minimum => 2500,
        LocalTier::Unsuitable => 4000,
    }
}

/// Synthesize 3 seconds of 16 kHz mono PCM with low-amplitude pink-ish
/// noise plus a 220 Hz tone. Whisper's encoder still does a full
/// log-mel + transformer forward pass on this, so the wall time is
/// representative of "real" first-dictation latency without needing to
/// vendor an audio fixture in the binary.
fn synthetic_3s_pcm() -> Vec<f32> {
    let sr = 16_000usize;
    let n = sr * 3;
    let mut out = Vec::with_capacity(n);
    let mut state: u32 = 0x1234_5678;
    for i in 0..n {
        // xorshift PRNG → low-amp white noise
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let noise = ((state as i32 as f32) / (i32::MAX as f32)) * 0.05;
        // 220 Hz tone, ~0.1 amp
        #[allow(clippy::cast_precision_loss)]
        let t = i as f32 / sr as f32;
        let tone = (t * 220.0 * std::f32::consts::TAU).sin() * 0.1;
        out.push(noise + tone);
    }
    out
}

/// Run a single 3-second STT pass against the configured local backend
/// and report wall time relative to `tier`'s budget. Errors are
/// non-fatal — this is purely advisory.
async fn probe_local_latency(paths: &fono_core::Paths, config: &Config, tier: LocalTier) {
    use fono_stt::factory::build_stt;
    use std::time::Instant;

    let secrets = if paths.secrets_file().exists() {
        Secrets::load(&paths.secrets_file()).unwrap_or_default()
    } else {
        Secrets::default()
    };

    let stt = match build_stt(&config.stt, &config.general, &secrets, &paths.whisper_models_dir()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  (latency probe skipped: {e:#})");
            return;
        }
    };

    println!("\n  Running latency probe on a 3-second synthetic clip…");
    let pcm = synthetic_3s_pcm();
    let start = Instant::now();
    let result = stt.transcribe(&pcm, 16_000, Some("en")).await;
    let elapsed_ms = start.elapsed().as_millis();
    let budget_ms = tier_latency_budget_ms(tier);

    match result {
        Ok(_) if elapsed_ms <= budget_ms => {
            println!(
                "  ✓ Probe transcribed 3s of audio in {elapsed_ms} ms (budget {budget_ms} ms for {} tier).",
                tier.as_str()
            );
        }
        Ok(_) => {
            eprintln!(
                "  ⚠ Probe took {elapsed_ms} ms — slower than the {budget_ms} ms budget for the {} tier.",
                tier.as_str()
            );
            eprintln!("    First dictation may feel slow. Options:");
            eprintln!("      • Switch to a smaller model: `fono use stt local` then edit `[stt.local].model`");
            eprintln!(
                "      • Switch to fast cloud STT: `fono use stt groq`  (requires GROQ_API_KEY)"
            );
            eprintln!(
                "      • Check load: a busy CPU during setup inflates this number — re-run later."
            );
        }
        Err(e) => {
            eprintln!("  ⚠ Probe failed: {e:#}");
            eprintln!("    The model loaded but inference errored — daemon may need a different model size.");
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use fono_core::hwcheck::{CpuFeatures, HardwareSnapshot};
    use fono_core::provider_catalog::find;
    use fono_stt::registry::ModelRegistry;

    #[test]
    fn humanize_chat_model_known_providers() {
        // Spot-check the hand-curated table: every catalogue entry
        // with assistant defaults should map to a friendly label, not
        // the raw model id.
        for id in ["openai", "anthropic", "groq", "cerebras", "openrouter"] {
            let entry = find(id).expect("catalogue entry");
            let pretty = humanize_chat_model(entry);
            let raw = entry.assistant.expect("entry has assistant defaults").text_model;
            assert!(
                pretty != raw,
                "{id}: humanize_chat_model fell through to raw model id ({raw})"
            );
            assert!(!pretty.is_empty(), "{id}: humanize returned empty");
        }
    }

    #[test]
    fn assistant_picker_rows_align_columns() {
        // Build with two entries of different widths and confirm the
        // header + every row share the same starting column for the
        // `Model` and `Key` cells.
        let mut secrets = Secrets::default();
        secrets.insert("OPENAI_API_KEY", "stub");
        let openai = find("openai").expect("openai");
        let anthropic = find("anthropic").expect("anthropic");
        let ordered = [openai, anthropic];
        let (rows, header) = assistant_picker_rows(&ordered, &secrets);
        assert_eq!(rows.len(), 2);
        let model_col = header.find("Model").expect("header has Model column");
        // First row uses padding-aware lookup: the `Model` cell must
        // start at the same column index as the header's `Model`.
        let row0_model_start =
            rows[0].find(&humanize_chat_model(openai)).expect("row 0 contains model");
        assert_eq!(row0_model_start, model_col);
        let row1_model_start =
            rows[1].find(&humanize_chat_model(anthropic)).expect("row 1 contains model");
        assert_eq!(row1_model_start, model_col);
        // Key status reflects secrets.toml presence.
        assert!(rows[0].ends_with("set"));
        assert!(rows[1].ends_with("missing"));
    }

    #[test]
    fn comfortable_tier_budget_is_two_seconds() {
        // Regression guard for the 1500 -> 2000 ms bump (user feedback
        // 2026-05-14): the Comfortable tier was rejecting first-run
        // probes on borderline hardware too aggressively.
        assert_eq!(tier_latency_budget_ms(LocalTier::Comfortable), 2000);
    }

    #[test]
    fn primary_capabilities_match_catalogue() {
        // OpenAI: full house except web search (catalogue advertises
        // `WebSearchSupport::None` until the OpenAI client migrates to
        // the Responses API).
        assert_eq!(
            primary_capabilities(find("openai").expect("openai entry")),
            [true, true, true, true, true, false],
        );
        // Groq: STT + LLM + Assistant + TTS, no Vision, no Search.
        // (Vision dropped after the Maverick 404 fix — Groq has no
        // hosted vision model Fono is willing to default to today.)
        assert_eq!(
            primary_capabilities(find("groq").expect("groq entry")),
            [true, true, true, true, false, false],
        );
        // Anthropic: LLM + Assistant + Vision + Search, no STT/TTS.
        assert_eq!(
            primary_capabilities(find("anthropic").expect("anthropic entry")),
            [false, true, true, false, true, true],
        );
        // Cerebras: LLM + Assistant only.
        assert_eq!(
            primary_capabilities(find("cerebras").expect("cerebras entry")),
            [false, true, true, false, false, false],
        );
        // OpenRouter: STT (Whisper Turbo) + LLM + Assistant + TTS
        // (OpenAI Mini TTS, swapped in from Kokoro 2026-05-14), no
        // Vision/Search yet. STT was added in 2026-05-14 — OpenRouter
        // proxies OpenAI-compatible /v1/audio/transcriptions to Groq's
        // Whisper Turbo when the model id is `openai/whisper-large-v3-turbo`.
        let openrouter = primary_capabilities(find("openrouter").expect("openrouter entry"));
        assert_eq!(openrouter[..2], [true, true]);
        assert!(openrouter[2], "OpenRouter exposes an assistant chat");
        assert!(!openrouter[4], "no multimodal model wired for OpenRouter");
        assert!(!openrouter[5], "no native web-search tool for OpenRouter");
    }

    #[test]
    fn assistant_extras_summary_matches_catalogue() {
        use fono_core::config::Assistant;
        // Default config: prefer_vision = true, prefer_web_search = false.
        // OpenAI now advertises web_search = None too (Responses-API
        // migration pending), so the line should mention vision only.
        let defaults = Assistant::default();
        let s = assistant_extras_summary(find("openai").unwrap(), &defaults)
            .expect("openai has vision extra");
        assert!(s.contains("vision"), "{s}");
        assert!(!s.contains("web search"), "{s}");
        // Anthropic, defaults: vision only (web_search disabled by default).
        let s = assistant_extras_summary(find("anthropic").unwrap(), &defaults)
            .expect("anthropic has vision extra");
        assert!(s.contains("vision"), "{s}");
        assert!(!s.contains("web search"), "{s}");
        // Anthropic with prefer_web_search opted in: surfaces the
        // Messages-API tool id.
        let opted_in = Assistant { prefer_web_search: true, ..Assistant::default() };
        let s = assistant_extras_summary(find("anthropic").unwrap(), &opted_in)
            .expect("anthropic has extras when web_search opted in");
        assert!(s.contains("vision"), "{s}");
        assert!(s.contains("web_search_20250305"), "{s}");
        // Even with both flags on, OpenAI advertises None today so the
        // line shows vision only.
        let s = assistant_extras_summary(find("openai").unwrap(), &opted_in)
            .expect("openai has vision extra");
        assert!(s.contains("vision"), "{s}");
        assert!(!s.contains("web search"), "{s}");
        // Groq: no multimodal model and web_search = None — the
        // Extras line is absent entirely. Regression guard for the
        // Maverick 404 fix; Groq's catalogue advertised a nonexistent
        // multimodal model (`llama-4-maverick-17b-128e-instruct`)
        // until that was removed.
        assert!(
            assistant_extras_summary(find("groq").unwrap(), &defaults).is_none(),
            "groq must have no extras line after the Maverick removal"
        );
        // Even with prefer_web_search opted in, Groq still has no
        // multimodal model and no native search tool — line stays
        // absent.
        assert!(assistant_extras_summary(find("groq").unwrap(), &opted_in).is_none());
        // Cerebras / OpenRouter: no multimodal, no native search.
        assert!(assistant_extras_summary(find("cerebras").unwrap(), &defaults).is_none());
        assert!(assistant_extras_summary(find("openrouter").unwrap(), &defaults).is_none());
        // STT-only providers have no assistant entry.
        assert!(assistant_extras_summary(find("assemblyai").unwrap(), &defaults).is_none());
    }

    fn snap(cores: u32, ram_gb: u32, disk_gb: u32, avx2: bool) -> HardwareSnapshot {
        const GB: u64 = 1024 * 1024 * 1024;
        HardwareSnapshot {
            physical_cores: cores,
            logical_cores: cores * 2,
            total_ram_bytes: u64::from(ram_gb) * GB,
            available_ram_bytes: u64::from(ram_gb) * GB,
            free_disk_bytes: u64::from(disk_gb) * GB,
            cpu_features: CpuFeatures { avx2, avx512: false, fma: false, neon: false },
            os: "linux".into(),
            arch: "x86_64".into(),
        }
    }

    // ── build_local_stt_shortlist ────────────────────────────────────────

    #[test]
    fn shortlist_english_only_excludes_multilingual() {
        let s = snap(12, 32, 200, true);
        let shortlist = build_local_stt_shortlist(true, &["en".to_string()], &s);
        for entry in &shortlist {
            assert!(
                !entry.model.multilingual,
                "english_only shortlist must not contain multilingual model '{}'",
                entry.model.name
            );
        }
    }

    #[test]
    fn shortlist_multilingual_excludes_en_only() {
        let s = snap(12, 32, 200, true);
        let shortlist = build_local_stt_shortlist(false, &["en".to_string(), "fr".to_string()], &s);
        for entry in &shortlist {
            assert!(
                entry.model.multilingual,
                "multilingual shortlist must not contain .en model '{}'",
                entry.model.name
            );
        }
    }

    #[test]
    fn shortlist_capped_at_three_entries() {
        // Big machine: many models qualify, but we never show more than 3.
        let s = snap(16, 64, 500, true);
        let shortlist = build_local_stt_shortlist(false, &["en".to_string()], &s);
        assert!(
            shortlist.len() <= SHORTLIST_MAX,
            "shortlist len {} exceeds cap {}",
            shortlist.len(),
            SHORTLIST_MAX
        );
    }

    #[test]
    fn english_only_recommended_pick_is_small_en_on_12_core_cpu() {
        // With threshold 3.0: small.en (rf=3.0) on 12-core CPU =
        // 3.0 × sqrt(12/8) ≈ 3.67 ≥ 3.0 → Comfortable; outranks base.en.
        let s = snap(12, 32, 200, true);
        let shortlist = build_local_stt_shortlist(true, &["en".to_string()], &s);
        assert!(!shortlist.is_empty());
        assert_eq!(shortlist[0].model.name, "small.en");
        assert_eq!(shortlist[0].affordability, Affordability::Comfortable);
    }

    #[test]
    fn multilingual_recommended_pick_is_turbo_on_apple_silicon() {
        // Apple Silicon: relaxed live threshold lets turbo become Comfortable.
        let s = HardwareSnapshot {
            os: "macos".into(),
            arch: "aarch64".into(),
            cpu_features: CpuFeatures { neon: true, ..Default::default() },
            ..snap(8, 16, 200, false)
        };
        let shortlist = build_local_stt_shortlist(false, &["en".to_string()], &s);
        assert!(!shortlist.is_empty());
        assert_eq!(shortlist[0].model.name, "large-v3-turbo");
        assert_eq!(shortlist[0].affordability, Affordability::Comfortable);
    }

    #[test]
    fn small_is_comfortable_on_8_core_cpu_only() {
        // With threshold 3.0: small.en (rf=3.0) on 8-core CPU =
        // 3.0 × 1.0 = 3.0 → Comfortable. Matches Phase 0 measurement
        // on ultra7-258v (3.13× batch RTF).
        let s = snap(8, 16, 200, true);
        let entry = build_local_stt_shortlist(true, &["en".to_string()], &s)
            .into_iter()
            .find(|e| e.model.name == "small.en")
            .expect("small.en should be in shortlist");
        assert_eq!(entry.affordability, Affordability::Comfortable);
    }

    #[test]
    fn low_ram_machine_hides_large_models() {
        let s = HardwareSnapshot {
            physical_cores: 8,
            logical_cores: 16,
            total_ram_bytes: 2 * 1024 * 1024 * 1024,
            available_ram_bytes: 1024 * 1024 * 1024,
            free_disk_bytes: 200 * 1024 * 1024 * 1024,
            cpu_features: CpuFeatures { avx2: true, ..Default::default() },
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        let has_tiny_en = build_local_stt_shortlist(true, &["en".to_string()], &s)
            .iter()
            .any(|e| e.model.name == "tiny.en");
        assert!(has_tiny_en, "tiny.en should be in shortlist on a 1 GiB-available machine");
    }

    #[test]
    fn comfortable_first_in_shortlist() {
        let s = snap(6, 16, 200, true);
        let shortlist = build_local_stt_shortlist(true, &["en".to_string()], &s);
        let mut seen_borderline = false;
        for entry in &shortlist {
            if entry.affordability == Affordability::Borderline {
                seen_borderline = true;
            }
            if seen_borderline {
                assert_ne!(
                    entry.affordability,
                    Affordability::Comfortable,
                    "Comfortable entry '{}' found after a Borderline entry",
                    entry.model.name
                );
            }
        }
    }

    // ── accuracy_for_langs ───────────────────────────────────────────────

    #[test]
    fn accuracy_excellent_for_small_en_on_english() {
        let m = ModelRegistry::get("small.en").unwrap();
        assert_eq!(accuracy_for_langs(m, &["en".to_string()]), AccuracyBucket::Excellent);
    }

    #[test]
    fn accuracy_inaccurate_for_tiny_on_polish() {
        // tiny multilingual: pl=30% → Inaccurate
        let m = ModelRegistry::get("tiny").unwrap();
        assert_eq!(accuracy_for_langs(m, &["pl".to_string()]), AccuracyBucket::Inaccurate);
    }

    #[test]
    fn accuracy_uses_worst_language() {
        // small: en=6 (Excellent), pl=15 (Acceptable). Combined → Acceptable.
        let m = ModelRegistry::get("small").unwrap();
        assert_eq!(
            accuracy_for_langs(m, &["en".to_string(), "pl".to_string()]),
            AccuracyBucket::Acceptable
        );
    }

    #[test]
    fn accuracy_unknown_for_unbenchmarked_language() {
        let m = ModelRegistry::get("small").unwrap();
        assert_eq!(accuracy_for_langs(m, &["xx".to_string()]), AccuracyBucket::Unknown);
    }

    #[test]
    fn shortlist_drops_inaccurate_entries_when_alternatives_exist() {
        // For Polish: tiny=30% (Inaccurate), base=22% (Inaccurate),
        // small=15% (Acceptable), turbo=10% (Good). Inaccurate entries
        // must be filtered out. On CPU-only 16-core, turbo is Unsuitable
        // (rf=0.6 × sqrt(2) ≈ 0.85 < 1.0 batch floor) so the highest
        // remaining is small.
        let s = snap(16, 64, 500, true);
        let shortlist = build_local_stt_shortlist(false, &["pl".to_string()], &s);
        for entry in &shortlist {
            assert_ne!(
                entry.accuracy,
                AccuracyBucket::Inaccurate,
                "Inaccurate entry '{}' should have been filtered (alternatives exist)",
                entry.model.name
            );
        }
        assert!(shortlist.iter().any(|e| e.model.name == "small"));
    }

    // ── Phase B6: pre-seed defaults from existing config ─────────────────

    fn primary_candidates_vec() -> Vec<&'static CloudProvider> {
        CLOUD_PROVIDERS.iter().filter(|p| is_primary_candidate(p)).collect()
    }

    #[test]
    fn seed_prefers_existing_llm_backend() {
        // A config with `[polish].backend = "cerebras"` must default the
        // primary picker to Cerebras, not OpenAI.
        let mut cfg = Config::default();
        cfg.polish.backend = PolishBackend::Cerebras;
        let secrets = Secrets::default();
        let candidates = primary_candidates_vec();
        let idx = default_primary_for_seed(&candidates, &cfg, &secrets);
        assert_eq!(candidates[idx].id, "cerebras");
    }

    #[test]
    fn seed_prefers_existing_stt_when_local_llm() {
        // STT=groq + LLM=local (local LLM not a primary candidate) →
        // should fall through to STT backend "groq".
        let mut cfg = Config::default();
        cfg.stt.backend = SttBackend::Groq;
        cfg.polish.backend = PolishBackend::Local;
        let secrets = Secrets::default();
        let candidates = primary_candidates_vec();
        let idx = default_primary_for_seed(&candidates, &cfg, &secrets);
        assert_eq!(candidates[idx].id, "groq");
    }

    #[test]
    fn seed_falls_back_to_secrets_then_openai() {
        let cfg = Config::default(); // STT=Local, LLM=Local — neither matches.
        let mut secrets = Secrets::default();
        secrets.insert("ANTHROPIC_API_KEY", "sk-test");
        let candidates = primary_candidates_vec();
        let idx = default_primary_for_seed(&candidates, &cfg, &secrets);
        assert_eq!(candidates[idx].id, "anthropic");

        // Without any key: defaults to OpenAI (broadest coverage).
        let empty = Secrets::default();
        let idx = default_primary_for_seed(&candidates, &cfg, &empty);
        assert_eq!(candidates[idx].id, "openai");
    }

    #[test]
    fn seed_round_trip_preserves_wyoming_tts() {
        // The regression guard from B6: a config with Groq STT +
        // Cerebras LLM + Wyoming TTS must survive the wizard helpers
        // (seed + tts_short_label) without flipping the TTS backend.
        let mut cfg = Config::default();
        cfg.stt.backend = SttBackend::Groq;
        cfg.polish.backend = PolishBackend::Cerebras;
        cfg.tts.backend = TtsBackend::Wyoming;
        cfg.tts.wyoming =
            Some(TtsWyoming { uri: "tcp://piper.lan:10200".into(), ..TtsWyoming::default() });
        let secrets = Secrets::default();
        let candidates = primary_candidates_vec();
        let idx = default_primary_for_seed(&candidates, &cfg, &secrets);
        // Cerebras is the LLM → seed default lands there.
        assert_eq!(candidates[idx].id, "cerebras");
        // The seed step never mutates `cfg.tts`.
        assert_eq!(cfg.tts.backend, TtsBackend::Wyoming);
        // tts_short_label round-trips Wyoming as a distinct label.
        assert_eq!(tts_short_label(&cfg.tts.backend), "Wyoming");
    }

    #[test]
    fn primary_picker_renders_aligned_table() {
        // Pin the column-aligned shape of the primary picker against
        // a known catalogue subset (issues #9/#11, v0.8.1 polish).
        // Provider column width = longest display_name ("OpenRouter")
        // + 2 = 12; capability columns = header label width + 2.
        let candidates: Vec<&'static CloudProvider> = primary_candidates_vec();
        let provider_col_width = candidates
            .iter()
            .map(|p| p.display_name.chars().count())
            .max()
            .unwrap_or(0)
            .max("Customize".len())
            + 2;
        assert_eq!(provider_col_width, 12);

        let header = primary_header(provider_col_width);
        assert_eq!(
            header,
            "Provider    STT        LLM        Assistant  TTS        Vision     Search"
        );

        // Rows for the five primary candidates in catalogue order.
        let rows: Vec<String> =
            candidates.iter().map(|p| primary_row(p, provider_col_width)).collect();
        assert_eq!(
            rows,
            vec![
                "OpenAI      ✓          ✓          ✓          ✓          ✓          ·".to_string(),
                "Groq        ✓          ✓          ✓          ✓          ·          ·".to_string(),
                "Anthropic   ·          ✓          ✓          ·          ✓          ✓".to_string(),
                "Cerebras    ·          ✓          ✓          ·          ·          ·".to_string(),
                "OpenRouter  ✓          ✓          ✓          ✓          ·          ·".to_string(),
            ]
        );

        // Customize escape hatch: provider column padded to the same
        // width, then a free-form description spanning the capability
        // columns.
        let customize = format!(
            "{:width$}Pick a backend per capability",
            "Customize",
            width = provider_col_width
        );
        assert_eq!(customize, "Customize   Pick a backend per capability");
    }

    #[test]
    fn primary_candidates_exclude_gemini_and_stt_only() {
        let ids: Vec<&str> = primary_candidates_vec().iter().map(|p| p.id).collect();
        assert!(!ids.contains(&"gemini"), "Gemini must be excluded (factory unwired)");
        assert!(!ids.contains(&"cartesia"), "Cartesia is STT-only → secondary, not primary");
        assert!(!ids.contains(&"deepgram"), "Deepgram is STT-only → secondary, not primary");
        assert!(!ids.contains(&"assemblyai"), "AssemblyAI is STT-only → secondary, not primary");
        // The five LLM-capable providers DO appear:
        for must in ["openai", "groq", "anthropic", "cerebras", "openrouter"] {
            assert!(ids.contains(&must), "{must} must be a primary candidate");
        }
    }

    /// Phase B Task B4. Pins the two-column layout of `pick_path`'s
    /// rows: name padded to longest-name + 2 spaces, then description.
    /// The fixed order is `Local`, `Cloud`, `Customize`.
    #[test]
    fn pick_path_rows_render_two_column_table() {
        let rows = pick_path_rows();
        assert_eq!(
            rows,
            [
                "Local      Private, offline, runs on this machine".to_string(),
                "Cloud      Fast, accurate, needs an API key".to_string(),
                "Customize  Pick a backend per capability".to_string(),
            ]
        );
        // Padding width derived from the longest name ("Customize") + 2.
        let expected_width = "Customize".len() + 2;
        for row in &rows {
            // The first `expected_width` chars contain the padded name;
            // char `expected_width - 1` is always a space (the second
            // of the two trailing pad spaces).
            assert_eq!(
                row.as_bytes()[expected_width - 1],
                b' ',
                "row {row:?} should have pad-space at column {}",
                expected_width - 1
            );
        }
    }

    /// Phase C Task C7. Pre-seed two detected language codes and
    /// assert the pure helper returns them (normalised) without
    /// invoking `dialoguer`. The wizard's OS-detected fast-path is
    /// exercised by feeding the helper directly.
    #[test]
    fn finalise_detected_languages_preserves_multi_detect() {
        let input = vec!["en".to_string(), "es".to_string()];
        let out = finalise_detected_languages(&input);
        assert!(out.contains(&"en".to_string()));
        assert!(out.contains(&"es".to_string()));
        assert_eq!(out.len(), 2, "no extra peer when ≥ 2 already present");
    }

    /// Phase C Task C7 (cont.). Single non-English detection auto-adds
    /// English as a peer per the bilingual safety net.
    #[test]
    fn finalise_detected_languages_adds_english_peer_for_single_nonenglish() {
        let input = vec!["es".to_string()];
        let out = finalise_detected_languages(&input);
        assert_eq!(out.len(), 2);
        assert!(out.contains(&"es".to_string()));
        assert!(out.contains(&"en".to_string()));
    }
}
