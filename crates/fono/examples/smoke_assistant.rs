// SPDX-License-Identifier: GPL-3.0-only
//! Pre-release smoke test for the cloud assistant backends.
//! Builds each backend through the same factory the daemon uses,
//! runs `prewarm()` (hits the provider's `/v1/models` so a wrong
//! key fails here) and then a tiny `reply_stream` ("Say hello in
//! five words.") to confirm the chat endpoint works end-to-end.
//!
//! Two scopes:
//!
//! * **Local** (default) — exercises all four cloud assistants
//!   (Anthropic, Cerebras, Groq, OpenAI) plus the OpenAI TTS
//!   `/v1/audio/speech` endpoint. Run from the workspace root
//!   so `tests/secrets.toml` is found:
//!
//!   ```sh
//!   cargo run --release --example smoke_assistant -p fono
//!   ```
//!
//! * **CI** (`--ci` flag) — exercises Groq + Cerebras only and
//!   skips the OpenAI TTS test. These are the providers whose
//!   API keys are stored as GitHub Actions secrets and exposed
//!   to the workflow. Anthropic and OpenAI are local-only.
//!
//!   ```sh
//!   cargo run --release --example smoke_assistant -p fono -- --ci
//!   ```
//!
//! Secrets are loaded from `tests/secrets.toml` if present,
//! otherwise from `~/.config/fono/secrets.toml`, otherwise from
//! the process environment (CI workflow exposes them via env).
//! `Secrets::resolve` falls back to env transparently so the same
//! binary works in all three contexts.
//!
//! Exit code 0 if every configured backend works; non-zero if any
//! backend with a present API key fails. Backends without a key
//! are skipped with a SKIP marker (not a failure).

use std::time::{Duration, Instant};

use anyhow::Result;
use fono_assistant::{AssistantContext, ConversationHistory};
use fono_core::config::{
    Assistant as AssistantCfg, AssistantBackend, AssistantCloud, Tts as TtsCfg, TtsBackend,
    TtsCloud,
};
use fono_core::{Paths, Secrets};
use futures::stream::StreamExt;

const TEST_PROMPT: &str = "Say hello in five words.";

struct Provider {
    backend: AssistantBackend,
    label: &'static str,
    key_env: &'static str,
    model: &'static str,
    /// Run this provider in `--ci` mode? CI's GitHub Secrets
    /// store keys for Groq + Cerebras only; Anthropic + OpenAI
    /// are local-only.
    ci: bool,
}

fn providers() -> Vec<Provider> {
    vec![
        Provider {
            backend: AssistantBackend::Anthropic,
            label: "anthropic",
            key_env: "ANTHROPIC_API_KEY",
            model: "claude-haiku-4-5-20251001",
            ci: false,
        },
        Provider {
            backend: AssistantBackend::Cerebras,
            label: "cerebras",
            key_env: "CEREBRAS_API_KEY",
            // Mirrors the user-facing default in
            // fono-assistant/src/factory.rs:73.
            model: "qwen-3-235b-a22b-instruct-2507",
            ci: true,
        },
        Provider {
            backend: AssistantBackend::Groq,
            label: "groq",
            key_env: "GROQ_API_KEY",
            model: "openai/gpt-oss-120b",
            ci: true,
        },
        Provider {
            backend: AssistantBackend::OpenAI,
            label: "openai",
            key_env: "OPENAI_API_KEY",
            model: "gpt-5.4-mini",
            ci: false,
        },
    ]
}

#[tokio::main]
// Flat smoke orchestrator: secret discovery, the assistant provider
// loop, the OpenAI TTS check, and the summary block all live here so
// the test reads top-to-bottom. Splitting into helpers would obscure
// more than it would shorten.
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    let ci_mode = std::env::args().any(|a| a == "--ci");
    // Prefer the workspace-local `tests/secrets.toml` if present
    // (the fixture file the user maintains explicitly for local
    // pre-release smoke runs); fall back to the daemon's runtime
    // path under `~/.config/fono/secrets.toml`. In CI neither file
    // exists; `Secrets::resolve` then transparently reads the env
    // vars exposed by the GitHub Actions `secrets:` block.
    let workspace_secrets = std::path::PathBuf::from("tests/secrets.toml");
    let secrets_path = if workspace_secrets.exists() {
        Some(workspace_secrets)
    } else {
        let p = Paths::resolve()?.secrets_file();
        p.exists().then_some(p)
    };
    let secrets = secrets_path
        .as_ref()
        .map(|p| Secrets::load(p).unwrap_or_default())
        .unwrap_or_default();

    println!(
        "Fono assistant smoke test ({} mode)\n",
        if ci_mode { "ci" } else { "local" }
    );
    if let Some(p) = secrets_path.as_ref() {
        println!("secrets file: {}", p.display());
    } else {
        println!("secrets file: (none — reading from env)");
    }
    println!();

    let mut failures: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut ran = 0_usize;

    // ── Assistant backends ─────────────────────────────────────────
    for p in providers() {
        if ci_mode && !p.ci {
            continue;
        }
        let label = p.label;
        if secrets.resolve(p.key_env).is_none() {
            println!(
                "[SKIP] assistant/{label}: {} not in secrets/env — skipping",
                p.key_env
            );
            skipped.push(format!("assistant/{label}"));
            continue;
        }
        ran += 1;

        let cfg = AssistantCfg {
            enabled: true,
            backend: p.backend.clone(),
            cloud: Some(AssistantCloud {
                provider: p.label.to_string(),
                api_key_ref: p.key_env.to_string(),
                model: p.model.to_string(),
            }),
            ..AssistantCfg::default()
        };

        match exercise_assistant(&cfg, &secrets).await {
            Ok(reply_chars) => {
                println!(
                    "[ OK ] assistant/{label} ({}): replied {reply_chars} chars",
                    p.model
                );
            }
            Err(e) => {
                println!("[FAIL] assistant/{label} ({}): {e:#}", p.model);
                failures.push(format!("assistant/{label}: {e:#}"));
            }
        }
    }

    // ── TTS (OpenAI) — local only; CI doesn't carry OPENAI_API_KEY ─
    if !ci_mode {
        if secrets.resolve("OPENAI_API_KEY").is_some() {
            ran += 1;
            let cfg = TtsCfg {
                backend: TtsBackend::OpenAI,
                voice: "alloy".into(),
                cloud: Some(TtsCloud {
                    provider: "openai".into(),
                    api_key_ref: "OPENAI_API_KEY".into(),
                    model: "tts-1".into(),
                }),
                ..TtsCfg::default()
            };
            match exercise_tts(&cfg, &secrets).await {
                Ok(samples) => {
                    println!("[ OK ] tts/openai (tts-1, alloy): synthesised {samples} samples");
                }
                Err(e) => {
                    println!("[FAIL] tts/openai: {e:#}");
                    failures.push(format!("tts/openai: {e:#}"));
                }
            }
        } else {
            println!("[SKIP] tts/openai: OPENAI_API_KEY not in secrets/env — skipping");
            skipped.push("tts/openai".into());
        }
    }

    // ── Summary ────────────────────────────────────────────────────
    println!();
    println!("───────────────────────────────────────────");
    if failures.is_empty() {
        println!(
            "All {ran} active backend(s) passed; {} skipped.",
            skipped.len()
        );
        Ok(())
    } else {
        println!("FAILURES ({}):", failures.len());
        for f in &failures {
            println!("  - {f}");
        }
        std::process::exit(1);
    }
}

async fn exercise_assistant(cfg: &AssistantCfg, secrets: &Secrets) -> Result<usize> {
    let assistant = fono_assistant::build_assistant(cfg, secrets)?
        .ok_or_else(|| anyhow::anyhow!("build_assistant returned None"))?;
    // Prewarm — for cloud backends this hits /v1/models so a wrong
    // key fails here with a clear status.
    let warm_started = Instant::now();
    if let Err(e) = assistant.prewarm().await {
        return Err(anyhow::anyhow!("prewarm: {e:#}"));
    }
    let warm_ms = warm_started.elapsed().as_millis();
    println!("       prewarm ok ({warm_ms} ms)");

    // Reply stream — short prompt, collect deltas. 30 s timeout.
    let ctx = AssistantContext {
        system_prompt: "You are a smoke-test fixture. Reply in 5 words exactly.".into(),
        language: None,
        history: ConversationHistory::default().snapshot(),
    };
    let stream_started = Instant::now();
    let stream = tokio::time::timeout(
        Duration::from_secs(30),
        assistant.reply_stream(TEST_PROMPT, &ctx),
    )
    .await
    .map_err(|_| anyhow::anyhow!("reply_stream open timed out"))??;

    let mut full = String::new();
    let mut deltas = stream;
    let drain = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(item) = deltas.next().await {
            match item {
                Ok(d) => full.push_str(&d.text),
                Err(e) => return Err::<(), anyhow::Error>(anyhow::anyhow!("delta error: {e:#}")),
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("reply_stream drain timed out"))?;
    drain?;

    let stream_ms = stream_started.elapsed().as_millis();
    let trimmed = full.trim();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!("empty reply (no deltas)"));
    }
    println!(
        "       reply ({stream_ms} ms): {:?}",
        if trimmed.len() > 80 {
            format!("{}…", &trimmed[..80])
        } else {
            trimmed.to_string()
        }
    );
    Ok(trimmed.len())
}

async fn exercise_tts(cfg: &TtsCfg, secrets: &Secrets) -> Result<usize> {
    let tts = fono_tts::build_tts(cfg, secrets)?
        .ok_or_else(|| anyhow::anyhow!("build_tts returned None"))?;
    let started = Instant::now();
    let audio = tokio::time::timeout(
        Duration::from_secs(15),
        tts.synthesize("Hello from Fono.", None, None),
    )
    .await
    .map_err(|_| anyhow::anyhow!("synthesize timed out"))??;
    let ms = started.elapsed().as_millis();
    if audio.pcm.is_empty() {
        return Err(anyhow::anyhow!("empty PCM"));
    }
    println!(
        "       synth ok ({ms} ms): {} samples @ {} Hz",
        audio.pcm.len(),
        audio.sample_rate
    );
    Ok(audio.pcm.len())
}
