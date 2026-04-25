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
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password, Select};
use fono_core::config::{
    Config, LlmBackend, LlmCloud, LlmLocal, Stt, SttBackend, SttCloud, SttLocal,
};
use fono_core::hwcheck::{self, HardwareSnapshot, LocalTier};
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

    // If the user chose local STT, download the model now (silently —
    // it'll also be re-checked on every daemon start). Failures are
    // non-fatal: the daemon will retry on next launch.
    if config.stt.backend == SttBackend::Local {
        if let Err(e) = crate::models::ensure_models(paths, &config).await {
            eprintln!("  (model download failed: {e:#} — the daemon will retry on next start)");
        }
    }

    println!(
        "\n  Configuration saved to: {}",
        paths.config_file().display()
    );
    println!(
        "  Default hotkeys: hold={}   toggle={}   paste-last={}",
        config.hotkeys.hold, config.hotkeys.toggle, config.hotkeys.paste_last
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

    // For v0.1 the LLM-cleanup local path defers to v0.2. Offer cloud
    // LLM cleanup OR skip — local LLM (LlamaLocal) is not yet wired.
    let llm_options = vec![
        "Skip LLM cleanup (recommended for now — raw whisper output)",
        "Cloud LLM cleanup (Cerebras / Groq / OpenAI / Anthropic — needs key)",
    ];
    let llm_choice = Select::with_theme(theme)
        .with_prompt("Apply LLM cleanup (filler-removal, capitalization, punctuation)?")
        .items(&llm_options)
        .default(0)
        .interact()?;

    if llm_choice == 0 {
        config.llm.backend = LlmBackend::None;
        config.llm.enabled = false;
        config.llm.local = LlmLocal::default();
    } else {
        configure_cloud_llm(theme, config, secrets).await?;
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

    let _lang: String = Input::with_theme(theme)
        .with_prompt("Default language (BCP-47 code, or 'auto')")
        .default("auto".into())
        .interact_text()?;
    Ok(())
}

/// R3.3 — Mixed path: ask STT and LLM independently, no coupling.
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
        "Skip LLM cleanup (raw STT output)",
        "Cloud LLM (Cerebras / Groq / OpenAI / Anthropic)",
    ];
    let llm_idx = Select::with_theme(theme)
        .with_prompt("LLM cleanup:")
        .items(llm_options)
        .default(0)
        .interact()?;
    if llm_idx == 0 {
        config.llm.backend = LlmBackend::None;
        config.llm.enabled = false;
    } else {
        configure_cloud_llm(theme, config, secrets).await?;
    }

    let _lang: String = Input::with_theme(theme)
        .with_prompt("Default language (BCP-47 code, or 'auto')")
        .default("auto".into())
        .interact_text()?;
    Ok(())
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

    config.stt.backend = stt_backend;
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
    print!("  validating {key_name} … ");
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
fn prompt_api_key_force(theme: &ColorfulTheme, key_name: &str) -> Result<Option<String>> {
    let k: String = Password::with_theme(theme)
        .with_prompt(format!(
            "Paste your {key_name} (stored mode 0600, leave empty to skip)"
        ))
        .allow_empty_password(true)
        .interact()?;
    if k.is_empty() {
        Ok(None)
    } else {
        Ok(Some(k))
    }
}
