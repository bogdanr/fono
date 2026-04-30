// SPDX-License-Identifier: GPL-3.0-only
//! First-run interactive wizard. Phase 8 Tasks 8.1 & 8.2.
//!
//! Tier-aware: probes hardware first and recommends local-vs-cloud based on
//! what the host can sustain — see `docs/plans/2026-04-25-fono-local-default-v1.md`.
//!
//! Roadmap-v2 R3.2 + R3.3: cloud keys are validated against the provider's
//! `/v1/models` endpoint before persisting (so the user catches a typo
//! immediately, not on the first dictation), and the top-level path picker
//! offers a "Mixed" option that asks for STT and LLM backends independently
//! (e.g. local STT + cloud LLM cleanup).
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
use dialoguer::{theme::ColorfulTheme, Confirm, MultiSelect, Select};
use fono_core::config::{
    Config, LlmBackend, LlmCloud, LlmLocal, Stt, SttBackend, SttCloud, SttLocal,
};
use fono_core::hwcheck::{Affordability, HardwareSnapshot, LocalTier};
use fono_core::locale::detect_os_languages;
use fono_core::{Paths, Secrets};
use fono_stt::registry::{ModelInfo, ModelRegistry, WHISPER_MODELS};
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
        PathChoice::Mixed => configure_mixed(&theme, &mut config, &mut secrets, &snap).await?,
    }

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
    if config.stt.backend == SttBackend::Local || config.llm.backend == LlmBackend::Local {
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

    println!(
        "\n  Configuration saved to: {}",
        paths.config_file().display()
    );
    println!(
        "  Default hotkeys: hold={}   toggle={}   cancel={}",
        config.hotkeys.hold, config.hotkeys.toggle, config.hotkeys.cancel
    );
    println!("  Run `fono` to start the daemon, or `fono doctor` to diagnose your setup.\n");
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathChoice {
    Local,
    Cloud,
    /// Mixed = pick STT and LLM backends independently. Lets users run
    /// e.g. local whisper for privacy + cloud Cerebras for fast cleanup,
    /// or cloud Groq STT + skip-LLM (raw output) on a slow-CPU machine.
    Mixed,
}

fn print_hw_summary(snap: &HardwareSnapshot, tier: LocalTier) {
    let ram_gb = snap.total_ram_bytes / (1024 * 1024 * 1024);
    let disk_gb = snap.free_disk_bytes / (1024 * 1024 * 1024);
    println!("  Detected hardware:");
    println!(
        "    cores : {} physical / {} logical",
        snap.physical_cores, snap.logical_cores
    );
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

    // R3.3 — three top-level options. Order tracks tier:
    // Comfortable+: local first (matches `tier.local_default()`).
    // Minimum:      cloud first (faster on the host hardware).
    let (items, default_idx, mapping): (&[&str; 3], usize, [PathChoice; 3]) =
        if tier.local_default() {
            (
                &[
                    "Local models (recommended for your machine — private, offline)",
                    "Mixed     (local speech-to-text + cloud cleanup, or vice-versa)",
                    "Cloud APIs (fast, needs internet, bring your own key)",
                ],
                0,
                [PathChoice::Local, PathChoice::Mixed, PathChoice::Cloud],
            )
        } else {
            (
                &[
                    "Cloud APIs (faster on your machine)",
                    "Mixed     (local speech-to-text + cloud cleanup, or vice-versa)",
                    "Local models (will work but slower — ~2 s per dictation)",
                ],
                0,
                [PathChoice::Cloud, PathChoice::Mixed, PathChoice::Local],
            )
        };

    let idx = Select::with_theme(theme)
        .with_prompt("How would you like to run speech-to-text and cleanup?")
        .items(items)
        .default(default_idx)
        .interact()
        .context("prompt")?;
    Ok(mapping[idx])
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
        local: SttLocal {
            model: stt_model.into(),
            ..Default::default()
        },
        cloud: None,
        wyoming: None,
        prompts: std::collections::HashMap::new(),
    };

    // Step 4 — Live dictation (interactive) mode.
    pick_interactive_mode(theme, config, snap, true, Some(stt_model));

    // Step 5 — Tier-aware LLM cleanup choice. Local LLM (llama.cpp) is wired and
    // ships in the default build; offer it alongside skip and cloud.
    let llm_options = vec![
        "Local LLM cleanup (qwen2.5, private, offline) — recommended",
        "Skip LLM cleanup (raw whisper output)",
        "Cloud LLM cleanup (Cerebras / Groq / OpenAI / Anthropic — needs key)",
    ];
    let llm_choice = Select::with_theme(theme)
        .with_prompt("Apply LLM cleanup (filler-removal, capitalization, punctuation)?")
        .items(&llm_options)
        .default(0)
        .interact()?;

    match llm_choice {
        0 => configure_local_llm(theme, config, tier)?,
        1 => {
            config.llm.backend = LlmBackend::None;
            config.llm.enabled = false;
            config.llm.local = LlmLocal::default();
        }
        _ => configure_cloud_llm(theme, config, secrets).await?,
    }
    Ok(())
}

async fn configure_cloud(
    theme: &ColorfulTheme,
    config: &mut Config,
    secrets: &mut Secrets,
    snap: &HardwareSnapshot,
) -> Result<()> {
    configure_cloud_stt(theme, config, secrets).await?;
    configure_cloud_llm(theme, config, secrets).await?;
    config.general.languages = pick_languages(theme)?;
    // Live dictation is always feasible for cloud STT.
    pick_interactive_mode(theme, config, snap, false, None);
    Ok(())
}

/// R3.3 -- Mixed path: ask STT and LLM independently, no coupling.
async fn configure_mixed(
    theme: &ColorfulTheme,
    config: &mut Config,
    secrets: &mut Secrets,
    snap: &HardwareSnapshot,
) -> Result<()> {
    let tier = snap.tier();
    println!("  Mixed mode — pick speech-to-text and LLM cleanup independently.\n");

    // ----- STT side -----
    let stt_options = &[
        "Local whisper.cpp (private, offline)",
        "Cloud STT  (Groq / OpenAI / Deepgram / …)",
    ];
    let stt_idx = Select::with_theme(theme)
        .with_prompt("Speech-to-text:")
        .items(stt_options)
        .default(usize::from(!tier.local_default()))
        .interact()?;

    let stt_is_local;
    let local_model_chosen: Option<String>;

    if stt_idx == 0 {
        stt_is_local = true;
        let english_only = pick_english_only(theme);
        if english_only {
            config.general.languages = vec!["en".to_string()];
        } else {
            config.general.languages = pick_languages(theme)?;
        }
        let stt_model = pick_local_stt_model(theme, english_only, &config.general.languages, snap)?;
        local_model_chosen = Some(stt_model.to_string());
        config.stt = Stt {
            backend: SttBackend::Local,
            local: SttLocal {
                model: stt_model.into(),
                ..Default::default()
            },
            cloud: None,
            wyoming: None,
            prompts: std::collections::HashMap::new(),
        };
    } else {
        stt_is_local = false;
        local_model_chosen = None;
        configure_cloud_stt(theme, config, secrets).await?;
        config.general.languages = pick_languages(theme)?;
    }

    // Interactive mode question (depends on STT backend + model).
    pick_interactive_mode(
        theme,
        config,
        snap,
        stt_is_local,
        local_model_chosen.as_deref(),
    );

    // ----- LLM side -----
    let llm_options = &[
        "Local LLM (qwen2.5, private, offline)",
        "Skip LLM cleanup (raw STT output)",
        "Cloud LLM (Cerebras / Groq / OpenAI / Anthropic)",
    ];
    let llm_idx = Select::with_theme(theme)
        .with_prompt("LLM cleanup:")
        .items(llm_options)
        .default(usize::from(!tier.local_default()))
        .interact()?;
    match llm_idx {
        0 => configure_local_llm(theme, config, tier)?,
        1 => {
            config.llm.backend = LlmBackend::None;
            config.llm.enabled = false;
        }
        _ => configure_cloud_llm(theme, config, secrets).await?,
    }

    Ok(())
}

// ─── Language scope ────────────────────────────────────────────────────────

/// Ask whether the user dictates only in English. This fast-path skips the
/// multi-language checkbox UI and allows the model picker to offer more
/// accurate `.en` variants. Default cursor is "Yes" because the majority of
/// first-time users are monolingual English speakers; OS locale `en_*`
/// reinforces the default but does not force it.
fn pick_english_only(theme: &ColorfulTheme) -> bool {
    let os_codes = fono_core::locale::detect_os_languages();
    let os_is_english = os_codes.iter().any(|c| c == "en");
    // If OS locale is non-English, default to "No" so bilingual users
    // immediately see the full checkbox picker.
    let default_yes = os_is_english || os_codes.is_empty();

    Confirm::with_theme(theme)
        .with_prompt(
            "Will you dictate only in English? \
             (English-only models are smaller and a bit more accurate)",
        )
        .default(default_yes)
        .interact()
        .unwrap_or(default_yes)
}

/// Languages-you-dictate-in picker. Plan v3 task 7. Builds a checkbox
/// list from a curated common-language set unioned with the OS-detected
/// locale, with `en` and any OS-detected entry pre-checked. Order in
/// the resulting `Vec` is cosmetic — Fono treats every entry as a peer.
/// Returning an empty `Vec` is allowed (collapses to unconstrained
/// auto-detect at runtime).
fn pick_languages(theme: &ColorfulTheme) -> Result<Vec<String>> {
    // Curated common dictation languages, BCP-47 alpha-2 + display name.
    // Order is presentation-only.
    let curated: Vec<(&str, &str)> = vec![
        ("en", "English"),
        ("es", "Spanish"),
        ("fr", "French"),
        ("de", "German"),
        ("it", "Italian"),
        ("pt", "Portuguese"),
        ("nl", "Dutch"),
        ("ro", "Romanian"),
        ("pl", "Polish"),
        ("ru", "Russian"),
        ("uk", "Ukrainian"),
        ("tr", "Turkish"),
        ("zh", "Chinese"),
        ("ja", "Japanese"),
        ("ko", "Korean"),
        ("hi", "Hindi"),
        ("ar", "Arabic"),
    ];
    let os_codes = detect_os_languages();

    // Build the candidate list: curated first, plus any OS code missing
    // from curated (appended with a "(detected)" label). Codes are
    // de-duplicated; `(label, code, default_checked)` triples drive the
    // MultiSelect.
    let mut entries: Vec<(String, String, bool)> = Vec::new();
    for (code, name) in &curated {
        let detected = os_codes.iter().any(|c| c == code);
        let label = if detected {
            format!("{name} ({code}) — detected from OS")
        } else {
            format!("{name} ({code})")
        };
        let checked = *code == "en" || detected;
        entries.push((label, (*code).to_string(), checked));
    }
    for code in &os_codes {
        if !curated.iter().any(|(c, _)| c == code) {
            entries.push((format!("{code} (detected from OS)"), code.clone(), true));
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

    let mut codes: Vec<String> = chosen.into_iter().map(|i| entries[i].1.clone()).collect();
    // Normalise via LanguageSelection so dedupe + lowercase rules apply
    // uniformly with the rest of the runtime.
    let normalised = fono_stt::LanguageSelection::from_config(&codes);
    codes = normalised.codes().to_vec();

    // If the user selected exactly one non-English language, silently add
    // English as a peer. Without it, a single-entry allow-list would cause
    // the rerun mechanism to force that language on any clip that Groq
    // auto-detects as something outside the list — including genuine English
    // speech — producing garbled output. English as a peer is harmless for
    // speakers who never use it (it will simply never be detected) and
    // essential for bilingual users.
    if codes.len() == 1 && codes[0] != "en" {
        codes.push("en".to_string());
        println!(
            "  ℹ  English added as a peer language — Fono works best with at\n\
             \x20     least two languages. You can remove it later by editing\n\
             \x20     general.languages in your config file."
        );
    }

    Ok(codes)
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
        model
            .wer_by_lang
            .iter()
            .find(|(l, _)| *l == "en")
            .map(|&(_, w)| w)
    } else {
        langs
            .iter()
            .filter_map(|lang| {
                model
                    .wer_by_lang
                    .iter()
                    .find(|(l, _)| l == lang)
                    .map(|&(_, w)| w)
            })
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
/// - models with `wizard_visible: false` (legacy variants kept for compat),
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
        .filter(|m| m.wizard_visible)
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
    let any_acceptable = candidates
        .iter()
        .any(|e| e.accuracy != AccuracyBucket::Inaccurate);
    let mut entries: Vec<ShortlistEntry> = if any_acceptable {
        candidates
            .into_iter()
            .filter(|e| e.accuracy != AccuracyBucket::Inaccurate)
            .collect()
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
        "base" | "base.en" => "Base (fast, good for clean speech)",
        "small" | "small.en" => "Small (balanced quality)",
        "large-v3-turbo" => "Turbo (best quality, needs more memory)",
        "medium" | "medium.en" => "Medium (legacy)",
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
        // smallest model in the correct language family (still in the
        // registry, even if marked wizard_visible=false for medium).
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
            let default_tag = if i == 0 { "  (recommended)" } else { "" };
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

// ─── Interactive (live) mode question ─────────────────────────────────────

/// Ask whether to enable live dictation (streaming overlay preview while
/// speaking). Recommendation matrix:
///
/// - **Cloud STT**: always recommend — the server handles the compute.
/// - **Local STT, accelerated host (Apple Silicon)**: recommend if the
///   model is `Comfortable` under the relaxed accel threshold.
/// - **Local STT, CPU-only host**: recommend only when the model is
///   `Comfortable` under the strict CPU threshold (typically tiny/base;
///   small only on >=12 cores; turbo never on CPU-only). Borderline →
///   default to off and explain that live mode may lag.
/// - **Local STT, model unknown**: fall back to hardware tier.
fn pick_interactive_mode(
    theme: &ColorfulTheme,
    config: &mut Config,
    snap: &HardwareSnapshot,
    stt_is_local: bool,
    local_model_name: Option<&str>,
) {
    let (recommend, reason): (bool, String) = if stt_is_local {
        local_model_name.and_then(ModelRegistry::get).map_or_else(
            || {
                (
                    snap.tier().local_default(),
                    "recommendation based on detected hardware".to_string(),
                )
            },
            |m| match snap.affords_model(m.min_ram_mb, m.approx_mb, m.realtime_factor_cpu_avx2) {
                Affordability::Comfortable => {
                    let reason = if snap.accelerated() {
                        "hardware-accelerated speech recognition keeps live preview snappy".to_string()
                    } else {
                        "this CPU should keep up with real-time transcription".to_string()
                    };
                    (true, reason)
                }
                Affordability::Borderline => {
                    let reason = if snap.accelerated() {
                        "this model is heavy even with hardware acceleration; batch dictation will feel snappier".to_string()
                    } else {
                        "on this CPU, live preview is not recommended".to_string()
                    };
                    (false, reason)
                }
                Affordability::Unsuitable => (
                    false,
                    "this hardware can't sustain live mode for the chosen model".to_string(),
                ),
            },
        )
    } else {
        (true, "transcription will do more API calls".to_string())
    };

    println!(
        "\n  Live dictation shows a real-time preview as you speak. It works best on\n  \
         fast machines or with cloud transcription; otherwise batch mode (full\n  \
         transcription appears when you release the hotkey) is smoother."
    );
    let enable = Confirm::with_theme(theme)
        .with_prompt(format!("Enable live dictation? — {reason}"))
        .default(recommend)
        .interact()
        .unwrap_or(recommend);

    config.interactive.enabled = enable;
}
// ─── LLM configuration helpers ─────────────────────────────────────────────

/// Tier-aware local LLM model picker. Sets `config.llm` to the chosen
/// `LlmBackend::Local` + matching `LlmLocal` defaults. The model file
/// is downloaded later by `ensure_models` once the wizard finishes.
fn configure_local_llm(theme: &ColorfulTheme, config: &mut Config, tier: LocalTier) -> Result<()> {
    let (items, models, default_idx) = match tier {
        LocalTier::HighEnd => (
            vec![
                "qwen2.5-3b-instruct  (~2.0 GB) — recommended for your machine",
                "qwen2.5-1.5b-instruct (~1.0 GB) — lighter",
                "qwen2.5-0.5b-instruct (~350 MB) — lightest",
            ],
            vec![
                "qwen2.5-3b-instruct",
                "qwen2.5-1.5b-instruct",
                "qwen2.5-0.5b-instruct",
            ],
            0usize,
        ),
        LocalTier::Recommended | LocalTier::Comfortable => (
            vec![
                "qwen2.5-1.5b-instruct (~1.0 GB) — recommended for your machine",
                "qwen2.5-0.5b-instruct (~350 MB) — lighter (faster, lower quality)",
                "qwen2.5-3b-instruct  (~2.0 GB) — slower but higher quality",
            ],
            vec![
                "qwen2.5-1.5b-instruct",
                "qwen2.5-0.5b-instruct",
                "qwen2.5-3b-instruct",
            ],
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
    config.llm.backend = LlmBackend::Local;
    config.llm.enabled = true;
    config.llm.local = LlmLocal {
        model: models[idx].into(),
        ..LlmLocal::default()
    };
    config.llm.cloud = None;
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
    let key = prompt_api_key_with_validation(theme, secrets, stt_key_name).await?;
    if let Some(k) = key {
        secrets.insert(stt_key_name, k);
    }

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
        "Cerebras (llama-3.3-70b, < 1s latency) — recommended",
        "Groq (llama-3.3-70b-versatile)",
        "OpenAI (gpt-4o-mini)",
        "Anthropic (claude-3-5-haiku)",
        "Skip LLM cleanup",
    ];
    let llm_idx = Select::with_theme(theme)
        .with_prompt("Pick a cloud LLM for cleanup")
        .items(llm_providers)
        .default(0)
        .interact()?;

    if llm_idx == 4 {
        config.llm.backend = LlmBackend::None;
        config.llm.enabled = false;
        return Ok(());
    }
    let (backend, key_name, model) = match llm_idx {
        0 => (LlmBackend::Cerebras, "CEREBRAS_API_KEY", "llama-3.3-70b"),
        1 => (LlmBackend::Groq, "GROQ_API_KEY", "llama-3.3-70b-versatile"),
        2 => (LlmBackend::OpenAI, "OPENAI_API_KEY", "gpt-4o-mini"),
        _ => (
            LlmBackend::Anthropic,
            "ANTHROPIC_API_KEY",
            "claude-3-5-haiku-latest",
        ),
    };
    if let Some(k) = prompt_api_key_with_validation(theme, secrets, key_name).await? {
        secrets.insert(key_name, k);
    }
    config.llm.backend = backend;
    config.llm.enabled = true;
    config.llm.cloud = Some(LlmCloud {
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
    print!(
        "  received {key_name} ({} chars); validating … ",
        new_key.chars().count()
    );
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
        "GROQ_API_KEY" => client
            .get("https://api.groq.com/openai/v1/models")
            .bearer_auth(key),
        "OPENAI_API_KEY" => client
            .get("https://api.openai.com/v1/models")
            .bearer_auth(key),
        "CEREBRAS_API_KEY" => client
            .get("https://api.cerebras.ai/v1/models")
            .bearer_auth(key),
        "ANTHROPIC_API_KEY" => client
            .get("https://api.anthropic.com/v1/models")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        "DEEPGRAM_API_KEY" => client
            .get("https://api.deepgram.com/v1/projects")
            .header("Authorization", format!("Token {key}")),
        "ASSEMBLYAI_API_KEY" => client
            .get("https://api.assemblyai.com/v2/transcript")
            .header("Authorization", key),
        "CARTESIA_API_KEY" => client
            .get("https://api.cartesia.ai/voices")
            .header("X-API-Key", key)
            .header("Cartesia-Version", "2024-06-10"),
        other => {
            // Unknown provider — skip validation.
            anyhow::bail!("no validation endpoint configured for {other}; key not validated")
        }
    };
    let resp = req
        .send()
        .await
        .with_context(|| format!("connect to {key_name} provider"))?;
    let status = resp.status();
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
    anyhow::ensure!(
        term.is_term(),
        "API key prompt requires an interactive terminal"
    );

    term.write_str(&format!(
        "? Paste your {key_name} (stored mode 0600, leave empty to skip) "
    ))?;
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
        LocalTier::Comfortable => 1500,
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

    let stt = match build_stt(
        &config.stt,
        &config.general,
        &secrets,
        &paths.whisper_models_dir(),
    ) {
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

    fn snap(cores: u32, ram_gb: u32, disk_gb: u32, avx2: bool) -> HardwareSnapshot {
        const GB: u64 = 1024 * 1024 * 1024;
        HardwareSnapshot {
            physical_cores: cores,
            logical_cores: cores * 2,
            total_ram_bytes: u64::from(ram_gb) * GB,
            available_ram_bytes: u64::from(ram_gb) * GB,
            free_disk_bytes: u64::from(disk_gb) * GB,
            cpu_features: CpuFeatures {
                avx2,
                avx512: false,
                fma: false,
                neon: false,
            },
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
    fn shortlist_hides_legacy_medium_models() {
        // Even on a beefy box that affords medium, the wizard never offers it
        // (wizard_visible=false). Turbo replaces it.
        let s = snap(16, 64, 500, true);
        assert!(!build_local_stt_shortlist(true, &["en".to_string()], &s)
            .iter()
            .any(|e| e.model.name == "medium.en"));
        assert!(!build_local_stt_shortlist(false, &["en".to_string()], &s)
            .iter()
            .any(|e| e.model.name == "medium"));
    }

    #[test]
    fn english_only_recommended_pick_is_small_en_on_high_end_cpu() {
        // With turbo not having a .en variant and the new realtime thresholds,
        // small.en is the highest-quality English-only model that's
        // Comfortable on a 12-core CPU-only machine.
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
            cpu_features: CpuFeatures {
                neon: true,
                ..Default::default()
            },
            ..snap(8, 16, 200, false)
        };
        let shortlist = build_local_stt_shortlist(false, &["en".to_string()], &s);
        assert!(!shortlist.is_empty());
        assert_eq!(shortlist[0].model.name, "large-v3-turbo");
        assert_eq!(shortlist[0].affordability, Affordability::Comfortable);
    }

    #[test]
    fn small_is_borderline_on_8_core_cpu_only() {
        // The user's 12th-gen Intel scenario: small lags in live mode.
        // Shortlist still includes small but flags it Borderline.
        let s = snap(8, 16, 200, true);
        let entry = build_local_stt_shortlist(true, &["en".to_string()], &s)
            .into_iter()
            .find(|e| e.model.name == "small.en")
            .expect("small.en should be in shortlist");
        assert_eq!(entry.affordability, Affordability::Borderline);
    }

    #[test]
    fn low_ram_machine_hides_large_models() {
        let s = HardwareSnapshot {
            physical_cores: 8,
            logical_cores: 16,
            total_ram_bytes: 2 * 1024 * 1024 * 1024,
            available_ram_bytes: 1024 * 1024 * 1024,
            free_disk_bytes: 200 * 1024 * 1024 * 1024,
            cpu_features: CpuFeatures {
                avx2: true,
                ..Default::default()
            },
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        let names: Vec<&str> = build_local_stt_shortlist(true, &["en".to_string()], &s)
            .iter()
            .map(|e| e.model.name)
            .collect();
        assert!(!names.contains(&"medium.en"), "medium.en should be hidden");
        assert!(names.contains(&"base.en"), "base.en should be in shortlist");
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
        assert_eq!(
            accuracy_for_langs(m, &["en".to_string()]),
            AccuracyBucket::Excellent
        );
    }

    #[test]
    fn accuracy_inaccurate_for_tiny_on_polish() {
        // tiny multilingual: pl=30% → Inaccurate
        let m = ModelRegistry::get("tiny").unwrap();
        assert_eq!(
            accuracy_for_langs(m, &["pl".to_string()]),
            AccuracyBucket::Inaccurate
        );
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
        assert_eq!(
            accuracy_for_langs(m, &["xx".to_string()]),
            AccuracyBucket::Unknown
        );
    }

    #[test]
    fn shortlist_drops_inaccurate_entries_when_alternatives_exist() {
        // For Polish on a high-end machine: tiny=30% (Inaccurate), base=22%
        // (Inaccurate), small=15% (Acceptable), turbo=10% (Good). Inaccurate
        // entries must be filtered out.
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
        // turbo should appear and rank highest by accuracy.
        assert!(shortlist.iter().any(|e| e.model.name == "large-v3-turbo"));
    }
}
