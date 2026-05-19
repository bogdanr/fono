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

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use fono_assistant::{AssistantContext, ConversationHistory};
use fono_core::config::{
    Assistant as AssistantCfg, AssistantBackend, AssistantCloud, General, Stt as SttCfg,
    SttBackend, SttCloud, Tts as TtsCfg, TtsBackend, TtsCloud,
};
use fono_core::{Paths, Secrets};
use futures::stream::StreamExt;

const TEST_PROMPT: &str = "Say hello in five words.";

/// Returns `true` when an error string looks like a 429 / rate-limit /
/// queue-exceeded response from any cloud provider.
fn is_429(msg: &str) -> bool {
    msg.contains("429") || msg.contains("queue_exceeded") || msg.contains("too_many_requests")
}

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
    let secrets =
        secrets_path.as_ref().map(|p| Secrets::load(p).unwrap_or_default()).unwrap_or_default();

    println!("Fono assistant smoke test ({} mode)\n", if ci_mode { "ci" } else { "local" });
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
            println!("[SKIP] assistant/{label}: {} not in secrets/env — skipping", p.key_env);
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

        // Retry up to 3 times with backoff on 429 / queue-full. If
        // all attempts are 429 the provider is overloaded — log SKIP
        // rather than FAIL so a transient Cerebras / Groq outage
        // doesn't block the release gate.
        let retry_delays: &[u64] = &[5, 15, 30]; // seconds
        let mut last_err: Option<anyhow::Error> = None;
        let mut overloaded = false;
        for (attempt, &delay) in std::iter::once(&0u64).chain(retry_delays.iter()).enumerate() {
            if delay > 0 {
                println!("       [{label}] 429 on attempt {attempt}, retrying in {delay}s…");
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
            match exercise_assistant(&cfg, &secrets).await {
                Ok(reply_chars) => {
                    println!("[ OK ] assistant/{label} ({}): replied {reply_chars} chars", p.model);
                    last_err = None;
                    overloaded = false;
                    break;
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    if is_429(&msg) {
                        overloaded = true;
                        last_err = Some(e);
                        // try again (or exhaust retries)
                    } else {
                        println!("[FAIL] assistant/{label} ({}): {e:#}", p.model);
                        failures.push(format!("assistant/{label}: {e:#}"));
                        last_err = None;
                        overloaded = false;
                        break;
                    }
                }
            }
        }
        if overloaded {
            let msg = last_err.map(|e| format!("{e:#}")).unwrap_or_default();
            println!(
                "[SKIP] assistant/{label} ({}): provider overloaded after all retries — {msg}",
                p.model
            );
            skipped.push(format!("assistant/{label} (429 overloaded)"));
        }
    }

    // ── Groq end-to-end: STT → LLM → TTS ───────────────────────────
    // Drives the same three Groq endpoints the daemon would hit in a
    // dictate-then-speak round trip, against a real PCM fixture. Runs
    // in both local and CI modes — Groq's key is one of the two CI
    // GitHub Secrets. The Orpheus default-voice regression that
    // landed in v0.8.0 was invisible to the LLM-only smoke; this
    // catches that whole class of TTS-side breakage.
    if secrets.resolve("GROQ_API_KEY").is_some() {
        ran += 1;
        match exercise_groq_e2e(&secrets).await {
            Ok(()) => println!("[ OK ] e2e/groq: stt → polish → tts completed"),
            Err(e) => {
                let msg = format!("{e:#}");
                if is_429(&msg) {
                    println!("[SKIP] e2e/groq: provider overloaded — {msg}");
                    skipped.push("e2e/groq (429 overloaded)".into());
                } else {
                    println!("[FAIL] e2e/groq: {e:#}");
                    failures.push(format!("e2e/groq: {e:#}"));
                }
            }
        }
    } else {
        println!("[SKIP] e2e/groq: GROQ_API_KEY not in secrets/env — skipping");
        skipped.push("e2e/groq".into());
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
        println!("All {ran} active backend(s) passed; {} skipped.", skipped.len());
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
    let stream =
        tokio::time::timeout(Duration::from_secs(30), assistant.reply_stream(TEST_PROMPT, &ctx))
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
        if trimmed.len() > 80 { format!("{}…", &trimmed[..80]) } else { trimmed.to_string() }
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
    println!("       synth ok ({ms} ms): {} samples @ {} Hz", audio.pcm.len(), audio.sample_rate);
    Ok(audio.pcm.len())
}

/// Groq end-to-end smoke: real fixture WAV → Groq Whisper STT →
/// Groq gpt-oss assistant → Groq Orpheus TTS. Each stage is timed
/// and required to produce non-empty output; a single 30 s timeout
/// per stage keeps a flapping provider from hanging the whole run.
// Three back-to-back HTTP exchanges (STT / chat / TTS) with their
// timeouts and pretty-printers; splitting into per-stage helpers
// would obscure the linear narrative more than it shortens.
#[allow(clippy::too_many_lines)]
async fn exercise_groq_e2e(secrets: &Secrets) -> Result<()> {
    // 1. STT — Groq Whisper on a real public-domain English clip.
    //    The equivalence harness already pins the fixture path and
    //    license; we just read raw PCM and shove it at the cloud.
    let fixture = Path::new("tests/fixtures/equivalence/en-conversational.wav");
    let (pcm, sample_rate) = read_wav_mono_f32(fixture)
        .map_err(|e| anyhow!("read fixture {}: {e:#}", fixture.display()))?;
    let stt_cfg = SttCfg {
        backend: SttBackend::Groq,
        cloud: Some(SttCloud {
            provider: "groq".into(),
            api_key_ref: "GROQ_API_KEY".into(),
            model: "whisper-large-v3-turbo".into(),
        }),
        ..SttCfg::default()
    };
    let general = General::default();
    // build_stt only reads this for SttBackend::Local — pass a path
    // that doesn't need to exist so we don't depend on $XDG_CACHE_HOME.
    let unused_models_dir = std::path::PathBuf::from(".");
    let stt = fono_stt::build_stt(&stt_cfg, &general, secrets, &unused_models_dir)
        .map_err(|e| anyhow!("build_stt: {e:#}"))?;
    let started = Instant::now();
    let transcription = tokio::time::timeout(
        Duration::from_secs(30),
        stt.transcribe(&pcm, sample_rate, Some("en")),
    )
    .await
    .map_err(|_| anyhow!("stt transcribe timed out"))?
    .map_err(|e| anyhow!("stt transcribe: {e:#}"))?;
    let stt_ms = started.elapsed().as_millis();
    let user_text = transcription.text.trim().to_string();
    if user_text.is_empty() {
        return Err(anyhow!("stt returned empty transcript"));
    }
    println!(
        "       stt ({stt_ms} ms, {} Hz, {} samples): {:?}",
        sample_rate,
        pcm.len(),
        trunc(&user_text, 80)
    );

    // 2. LLM/Assistant — feed the transcript through Groq's chat
    //    completion and stream-collect the reply.
    let llm_cfg = AssistantCfg {
        enabled: true,
        backend: AssistantBackend::Groq,
        cloud: Some(AssistantCloud {
            provider: "groq".into(),
            api_key_ref: "GROQ_API_KEY".into(),
            model: "openai/gpt-oss-120b".into(),
        }),
        ..AssistantCfg::default()
    };
    let assistant = fono_assistant::build_assistant(&llm_cfg, secrets)
        .map_err(|e| anyhow!("build_assistant: {e:#}"))?
        .ok_or_else(|| anyhow!("build_assistant returned None"))?;
    let ctx = AssistantContext {
        system_prompt: "You are a brief smoke-test assistant. Reply in one short sentence.".into(),
        language: None,
        history: ConversationHistory::default().snapshot(),
    };
    let started = Instant::now();
    let mut stream =
        tokio::time::timeout(Duration::from_secs(30), assistant.reply_stream(&user_text, &ctx))
            .await
            .map_err(|_| anyhow!("polish reply_stream open timed out"))?
            .map_err(|e| anyhow!("polish reply_stream open: {e:#}"))?;
    let mut reply = String::new();
    let drain = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(item) = stream.next().await {
            match item {
                Ok(d) => reply.push_str(&d.text),
                Err(e) => return Err::<(), anyhow::Error>(anyhow!("polish delta: {e:#}")),
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| anyhow!("polish reply_stream drain timed out"))?;
    drain?;
    let llm_ms = started.elapsed().as_millis();
    let reply_trim = reply.trim().to_string();
    if reply_trim.is_empty() {
        return Err(anyhow!("polish returned empty reply"));
    }
    println!("       polish ({llm_ms} ms): {:?}", trunc(&reply_trim, 80));

    // 3. TTS — Groq Orpheus on the (truncated) reply. Truncate to
    //    keep the synthesis cheap; the goal is to prove the wire
    //    shape works, not to produce minutes of audio.
    let tts_cfg = TtsCfg {
        backend: TtsBackend::Groq,
        cloud: Some(TtsCloud {
            provider: "groq".into(),
            api_key_ref: "GROQ_API_KEY".into(),
            model: "canopylabs/orpheus-v1-english".into(),
        }),
        ..TtsCfg::default()
    };
    let tts = fono_tts::build_tts(&tts_cfg, secrets)
        .map_err(|e| anyhow!("build_tts: {e:#}"))?
        .ok_or_else(|| anyhow!("build_tts returned None"))?;
    let to_speak: String = reply_trim.chars().take(200).collect();
    let started = Instant::now();
    let audio =
        tokio::time::timeout(Duration::from_secs(30), tts.synthesize(&to_speak, None, None))
            .await
            .map_err(|_| anyhow!("tts synthesize timed out"))?
            .map_err(|e| anyhow!("tts synthesize: {e:#}"))?;
    let tts_ms = started.elapsed().as_millis();
    if audio.pcm.is_empty() {
        return Err(anyhow!("tts returned empty PCM"));
    }
    println!("       tts ({tts_ms} ms): {} samples @ {} Hz", audio.pcm.len(), audio.sample_rate);
    Ok(())
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Minimal 16-bit-PCM mono/stereo WAV reader. Duplicates the helper
/// in `crates/fono/src/cli.rs` (kept private there) so this example
/// stays self-contained. Returns `(samples in [-1.0, 1.0], rate)`.
fn read_wav_mono_f32(path: &Path) -> Result<(Vec<f32>, u32)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(anyhow!("not a RIFF/WAVE file: {}", path.display()));
    }
    let mut i = 12;
    let mut fmt_chans: u16 = 1;
    let mut fmt_rate: u32 = 16_000;
    let mut fmt_bps: u16 = 16;
    let mut data_off = 0;
    let mut data_len = 0;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let sz =
            u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        let body = i + 8;
        if id == b"fmt " {
            fmt_chans = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]);
            fmt_rate = u32::from_le_bytes([
                bytes[body + 4],
                bytes[body + 5],
                bytes[body + 6],
                bytes[body + 7],
            ]);
            fmt_bps = u16::from_le_bytes([bytes[body + 14], bytes[body + 15]]);
        } else if id == b"data" {
            data_off = body;
            data_len = sz;
            break;
        }
        i = body + sz;
    }
    if data_off == 0 {
        return Err(anyhow!("no `data` chunk in {}", path.display()));
    }
    if fmt_bps != 16 {
        return Err(anyhow!("only 16-bit PCM supported (got {fmt_bps}-bit)"));
    }
    let body = &bytes[data_off..data_off + data_len];
    let frames = body.len() / 2 / fmt_chans as usize;
    let mut out = Vec::with_capacity(frames);
    for f_i in 0..frames {
        let mut sum = 0f32;
        for c in 0..fmt_chans {
            let off = (f_i * fmt_chans as usize + c as usize) * 2;
            let s = i16::from_le_bytes([body[off], body[off + 1]]);
            sum += f32::from(s) / f32::from(i16::MAX);
        }
        out.push(sum / f32::from(fmt_chans));
    }
    Ok((out, fmt_rate))
}
