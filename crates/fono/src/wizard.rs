// SPDX-License-Identifier: GPL-3.0-only
//! First-run interactive wizard. Phase 8 Tasks 8.1 & 8.2.
//!
//! Offers two branches — local models vs. cloud APIs — each with ≥ 2 options.

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Input, Password, Select};
use fono_core::config::{
    Config, LlmBackend, LlmCloud, LlmLocal, Stt, SttBackend, SttCloud, SttLocal,
};
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

    let path_choice = Select::with_theme(&theme)
        .with_prompt("How would you like to run speech-to-text and cleanup?")
        .items(&[
            "Local models (private, offline, ~1.3 GB disk)",
            "Cloud APIs (needs internet, bring your own key)",
        ])
        .default(0)
        .interact()
        .context("prompt")?;

    let mut config = Config::default();
    let mut secrets = Secrets::default();

    if path_choice == 0 {
        configure_local(&theme, &mut config)?;
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
        let want = dialoguer::Confirm::with_theme(&theme)
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

fn configure_local(theme: &ColorfulTheme, config: &mut Config) -> Result<()> {
    let stt_options = &[
        "whisper small (multilingual, ~466 MB) — recommended",
        "whisper base  (multilingual, ~142 MB) — lighter",
        "whisper medium (multilingual, ~1.5 GB) — highest quality",
    ];
    let stt_idx = Select::with_theme(theme)
        .with_prompt("Pick a local speech-to-text model")
        .items(stt_options)
        .default(0)
        .interact()?;
    let stt_model = ["small", "base", "medium"][stt_idx];

    let llm_options = &[
        "Qwen2.5-1.5B-Instruct (Apache-2.0, multilingual) — recommended",
        "Qwen2.5-0.5B-Instruct (Apache-2.0, lighter)",
        "SmolLM2-1.7B-Instruct (Apache-2.0)",
        "Skip LLM cleanup (raw transcription only)",
    ];
    let llm_idx = Select::with_theme(theme)
        .with_prompt("Pick a local LLM for cleanup")
        .items(llm_options)
        .default(0)
        .interact()?;

    config.stt = Stt {
        backend: SttBackend::Local,
        local: SttLocal {
            model: stt_model.into(),
            ..Default::default()
        },
        cloud: None,
    };

    config.llm.backend = if llm_idx == 3 {
        LlmBackend::None
    } else {
        LlmBackend::Local
    };
    config.llm.enabled = llm_idx != 3;
    config.llm.local = match llm_idx {
        0 => LlmLocal {
            model: "qwen2.5-1.5b-instruct".into(),
            ..Default::default()
        },
        1 => LlmLocal {
            model: "qwen2.5-0.5b-instruct".into(),
            ..Default::default()
        },
        2 => LlmLocal {
            model: "smollm2-1.7b-instruct".into(),
            ..Default::default()
        },
        _ => LlmLocal::default(),
    };

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
        "Groq (whisper-large-v3, fastest) — recommended",
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
        0 => (SttBackend::Groq, "GROQ_API_KEY", "whisper-large-v3"),
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
    } else {
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
    }

    let _lang: String = Input::with_theme(theme)
        .with_prompt("Default language (BCP-47 code, or 'auto')")
        .default("auto".into())
        .interact_text()?;
    Ok(())
}
