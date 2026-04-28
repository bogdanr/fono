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

use anyhow::{Context, Result};
use dialoguer::console::{Key, Term};
use dialoguer::{theme::ColorfulTheme, Confirm, MultiSelect, Select};
use fono_core::config::{
    Config, LlmBackend, LlmCloud, LlmLocal, Stt, SttBackend, SttCloud, SttLocal,
};
use fono_core::hwcheck::{self, HardwareSnapshot, LocalTier};
use fono_core::locale::detect_os_languages;
use fono_core::{Paths, Secrets};
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
    let snap = hwcheck::probe(&paths.cache_dir);
    let tier = snap.tier();
    print_hw_summary(&snap, tier);

    let path_choice = pick_path(&theme, tier, &snap)?;

    let mut config = Config::default();

    match path_choice {
        PathChoice::Local => configure_local(&theme, &mut config, &mut secrets, tier).await?,
        PathChoice::Cloud => configure_cloud(&theme, &mut config, &mut secrets).await?,
        PathChoice::Mixed => configure_mixed(&theme, &mut config, &mut secrets, tier).await?,
    }

    config.save(&paths.config_file())?;
    if !secrets.keys.is_empty() {
        secrets.save(&paths.secrets_file())?;
    }

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
    let isa = if snap.cpu_features.avx2 {
        "AVX2"
    } else if snap.cpu_features.neon {
        "NEON"
    } else {
        "no-vec"
    };
    println!("  Detected hardware:");
    println!(
        "    cores : {} physical / {} logical  ({})",
        snap.physical_cores, snap.logical_cores, isa
    );
    println!(
        "    ram   : {ram_gb} GB total · disk free : {disk_gb} GB · arch : {}/{}",
        snap.os, snap.arch
    );
    let blurb = match tier {
        LocalTier::Unsuitable => "  Local-model tier: UNSUITABLE — recommended path is cloud APIs.",
        LocalTier::Minimum => {
            "  Local-model tier: MINIMUM — local will work but expect ~2 s per dictation."
        }
        LocalTier::Comfortable => {
            "  Local-model tier: COMFORTABLE — local STT recommended (whisper-small)."
        }
        LocalTier::Recommended => {
            "  Local-model tier: RECOMMENDED — local STT runs fast (whisper-small, ~1 s)."
        }
        LocalTier::HighEnd => "  Local-model tier: HIGH-END — local STT/LLM both viable.",
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
                    "Mixed     (local STT + cloud LLM, or vice-versa)",
                    "Cloud APIs (fast, needs internet, bring your own key)",
                ],
                0,
                [PathChoice::Local, PathChoice::Mixed, PathChoice::Cloud],
            )
        } else {
            (
                &[
                    "Cloud APIs (faster on your machine)",
                    "Mixed     (local STT + cloud LLM, or vice-versa)",
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
    tier: LocalTier,
) -> Result<()> {
    let stt_model = pick_local_stt_model(theme, tier)?;
    config.stt = Stt {
        backend: SttBackend::Local,
        local: SttLocal {
            model: stt_model.into(),
            ..Default::default()
        },
        cloud: None,
    };

    // Tier-aware LLM cleanup choice. Local LLM (llama.cpp) is wired and
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
) -> Result<()> {
    configure_cloud_stt(theme, config, secrets).await?;
    configure_cloud_llm(theme, config, secrets).await?;

    config.general.languages = pick_languages(theme)?;
    Ok(())
}

/// R3.3 -- Mixed path: ask STT and LLM independently, no coupling.
async fn configure_mixed(
    theme: &ColorfulTheme,
    config: &mut Config,
    secrets: &mut Secrets,
    tier: LocalTier,
) -> Result<()> {
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
    if stt_idx == 0 {
        let stt_model = pick_local_stt_model(theme, tier)?;
        config.stt = Stt {
            backend: SttBackend::Local,
            local: SttLocal {
                model: stt_model.into(),
                ..Default::default()
            },
            cloud: None,
        };
    } else {
        configure_cloud_stt(theme, config, secrets).await?;
    }

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

    let langs = pick_languages(theme)?;
    config.general.languages = langs;
    Ok(())
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
            entries.push((
                format!("{code} (detected from OS)"),
                code.clone(),
                true,
            ));
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

    let mut codes: Vec<String> = chosen
        .into_iter()
        .map(|i| entries[i].1.clone())
        .collect();
    // Normalise via LanguageSelection so dedupe + lowercase rules apply
    // uniformly with the rest of the runtime.
    let normalised = fono_stt::LanguageSelection::from_config(&codes);
    codes = normalised.codes().to_vec();
    Ok(codes)
}

fn pick_local_stt_model(theme: &ColorfulTheme, tier: LocalTier) -> Result<&'static str> {
    let (items, models, default_idx) = match tier {
        LocalTier::HighEnd => (
            vec![
                "whisper medium (multilingual, ~1.5 GB) — recommended for your machine",
                "whisper small  (multilingual, ~466 MB) — lighter",
                "whisper base   (multilingual, ~142 MB) — lightest",
            ],
            vec!["medium", "small", "base"],
            0usize,
        ),
        LocalTier::Recommended | LocalTier::Comfortable => (
            vec![
                "whisper small (multilingual, ~466 MB) — recommended for your machine",
                "whisper base  (multilingual, ~142 MB) — lighter (faster, lower accuracy)",
            ],
            vec!["small", "base"],
            0usize,
        ),
        LocalTier::Minimum | LocalTier::Unsuitable => (
            vec![
                "whisper base (multilingual, ~142 MB) — recommended for your machine",
                "whisper small (multilingual, ~466 MB) — slower but more accurate",
            ],
            vec!["base", "small"],
            0usize,
        ),
    };
    let stt_idx = Select::with_theme(theme)
        .with_prompt("Pick a local speech-to-text model")
        .items(&items)
        .default(default_idx)
        .interact()?;
    Ok(models[stt_idx])
}

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
    // Streaming pseudo-stream is opt-in: prompt only when the user
    // picked Groq AND has interactive enabled. Other cloud providers
    // don't have a pseudo-stream backend yet (Slice B1 ships Groq
    // first; OpenAI realtime lands in a follow-up).
    let streaming = if matches!(stt_backend, SttBackend::Groq) && config.interactive.enabled {
        dialoguer::Confirm::with_theme(theme)
            .with_prompt(
                "Enable Groq streaming dictation? \
                 (~25% extra cost vs batch — see docs/providers.md)",
            )
            .default(false)
            .interact()
            .unwrap_or(false)
    } else {
        false
    };
    config.stt.cloud = Some(SttCloud {
        provider: stt_key_name.trim_end_matches("_API_KEY").to_lowercase(),
        api_key_ref: stt_key_name.into(),
        model: stt_default_model.into(),
        streaming,
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
