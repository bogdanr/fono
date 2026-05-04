// SPDX-License-Identifier: GPL-3.0-only
//! Pre-release helper: list the chat-class models each cloud
//! account has access to so the user can pick the right strings
//! for `default_cloud_model()`. Hits `/v1/models` (or the
//! provider-specific equivalent) using the keys in
//! `~/.config/fono/secrets.toml`.
//!
//! ```sh
//! cargo run --release --example list_cloud_models -p fono
//! ```

use anyhow::Result;
use fono_core::{Paths, Secrets};
use serde_json::Value;
use std::time::Duration;

const PROVIDERS: &[(&str, &str, &str)] = &[
    (
        "cerebras",
        "CEREBRAS_API_KEY",
        "https://api.cerebras.ai/v1/models",
    ),
    (
        "groq",
        "GROQ_API_KEY",
        "https://api.groq.com/openai/v1/models",
    ),
    (
        "openai",
        "OPENAI_API_KEY",
        "https://api.openai.com/v1/models",
    ),
];

#[tokio::main]
async fn main() -> Result<()> {
    let workspace_secrets = std::path::PathBuf::from("tests/secrets.toml");
    let secrets_path = if workspace_secrets.exists() {
        workspace_secrets
    } else {
        Paths::resolve()?.secrets_file()
    };
    let secrets = Secrets::load(&secrets_path).unwrap_or_default();
    println!("(secrets from {})\n", secrets_path.display());

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;

    for (label, key_env, url) in PROVIDERS {
        let Some(key) = secrets.resolve(key_env) else {
            println!("[SKIP] {label}: {key_env} not in secrets");
            continue;
        };
        println!("\n=== {label} ({url}) ===");
        let resp = client.get(*url).bearer_auth(&key).send().await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                println!("[FAIL] {label}: request error: {e}");
                continue;
            }
        };
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            println!("[FAIL] {label}: {status}: {body}");
            continue;
        }
        let parsed: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                println!("[FAIL] {label}: parse error: {e}; body: {body}");
                continue;
            }
        };
        let mut ids: Vec<String> = parsed
            .get("data")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        if ids.is_empty() {
            println!("(no `data[].id` entries — raw body follows)\n{body}");
        } else {
            println!("{} models:", ids.len());
            for id in ids {
                println!("  - {id}");
            }
        }
    }
    Ok(())
}
