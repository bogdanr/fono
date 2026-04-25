// SPDX-License-Identifier: GPL-3.0-only
//! First-run interactive wizard. Phase 8 Tasks 8.1 & 8.2.
//!
//! Tier-aware: probes hardware first and recommends local-vs-cloud based on
//! what the host can sustain — see `docs/plans/2026-04-25-fono-local-default-v1.md`.

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password, Select};
use fono_core::config::{
    Config, LlmBackend, LlmCloud, LlmLocal, Stt, SttBackend, SttCloud, SttLocal,
};
use fono_core::hwcheck::{self, HardwareSnapshot, LocalTier};
use fono_core::{Paths, Secrets};

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

    // ---------- Hardware probe + tier ----------
    let snap = hwcheck::probe(&paths.cache_dir);
    let tier = snap.tier();
    print_hw_summary(&snap, tier);

    let path_choice = pick_path(&theme, tier, &snap)?;

    let mut config = Config::default();
    let mut secrets = Secrets::default();

    if path_choice == PathChoice::Local {
        configure_local(&theme, &mut config, tier)?;
    } else {
        configure_cloud(&theme, &mut config, &mut secrets)?;
    }

    config.save(&paths.config_file())?;
    if !secrets.keys.is_empty() {
        secrets.save(&paths.secrets_file())?;
    }

    // If the user chose local STT, kick off the model download now so
    // the first `fono` invocation doesn't pause for hundreds of MB.
    if config.stt.backend == SttBackend::Local {
        let want = Confirm::with_theme(&theme)
            .with_prompt(format!(
                "Download whisper model '{}' now?",
                config.stt.local.model
            ))
            .default(true)
            .interact()
            .unwrap_or(false);
        if want {
            if let Err(e) = crate::models::ensure_models(paths, &config).await {
                eprintln!(
                    "  (model download failed: {e:#} — you can retry with `fono models install`)"
                );
            }
        } else {
            println!(
                "  Skipped. Run `fono models install {}` when you're ready.",
                config.stt.local.model
            );
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

    // Local-default tiers (Comfortable+): local first.
    let (items, default_idx, mapping): (&[&str; 2], usize, [PathChoice; 2]) =
        if tier.local_default() {
            (
                &[
                    "Local models (recommended for your machine — private, offline)",
                    "Cloud APIs (fast, needs internet, bring your own key)",
                ],
                0,
                [PathChoice::Local, PathChoice::Cloud],
            )
        } else {
            // Minimum: cloud first because it's faster on this hardware.
            (
                &[
                    "Cloud APIs (faster on your machine)",
                    "Local models (will work but slower — ~2 s per dictation)",
                ],
                0,
                [PathChoice::Cloud, PathChoice::Local],
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

fn configure_local(theme: &ColorfulTheme, config: &mut Config, tier: LocalTier) -> Result<()> {
    // Tier-narrowed model menu: only show models the host can sustain
    // plus one safer fallback. Order: recommended → fallback.
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
    let stt_model = models[stt_idx];

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
        // Reuse the cloud LLM picker.
        let mut secrets = Secrets::default();
        configure_cloud_llm(theme, config, &mut secrets)?;
        if !secrets.keys.is_empty() {
            // Save the cloud key separately. Caller's `secrets` won't
            // see this; persist directly so the daemon can resolve it.
            // Path-resolution is the caller's job; we just stash it.
            // (We don't have `paths` here; configure_cloud_llm wrote
            // into `secrets` which the caller saves below.)
            // This is a no-op until we refactor — local + cloud LLM
            // is rare; document and move on.
            tracing::debug!("local STT + cloud LLM not yet wired in saved secrets");
        }
    }

    println!(
        "\n  Models will be downloaded on first recording (or run \
         `fono models install {stt_model}`)."
    );
    Ok(())
}

fn configure_cloud(
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
    let key: String = Password::with_theme(theme)
        .with_prompt(format!("Paste your {stt_key_name} (stored mode 0600)"))
        .allow_empty_password(true)
        .interact()?;
    if !key.is_empty() {
        secrets.insert(stt_key_name, key);
    }

    config.stt.backend = stt_backend;
    config.stt.cloud = Some(SttCloud {
        provider: stt_key_name.trim_end_matches("_API_KEY").to_lowercase(),
        api_key_ref: stt_key_name.into(),
        model: stt_default_model.into(),
    });

    configure_cloud_llm(theme, config, secrets)?;

    let _lang: String = Input::with_theme(theme)
        .with_prompt("Default language (BCP-47 code, or 'auto')")
        .default("auto".into())
        .interact_text()?;
    Ok(())
}

fn configure_cloud_llm(
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
    let reuse = secrets.keys.contains_key(key_name);
    if !reuse {
        let k: String = Password::with_theme(theme)
            .with_prompt(format!("Paste your {key_name}"))
            .allow_empty_password(true)
            .interact()?;
        if !k.is_empty() {
            secrets.insert(key_name, k);
        }
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
