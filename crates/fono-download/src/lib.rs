// SPDX-License-Identifier: GPL-3.0-only
//! HTTP downloader with Range-resume, SHA256 verification, and `indicatif`
//! progress UI. Phase 9 Task 9.5 (mirror override) + first-run downloads.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

/// Download `url` to `dest`, resuming if a partial file exists. Verifies the
/// final file against `expected_sha256` (hex lowercase, 64 chars). A hash
/// starting with `"0000..."` is treated as "unpinned" and only logged.
pub async fn download(url: &str, dest: &Path, expected_sha256: &str) -> Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match try_download(url, dest).await {
            Ok(()) => break,
            Err(e) if attempt < 3 => {
                warn!("download attempt {attempt} failed: {e}; retrying");
                tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
            }
            Err(e) => return Err(e),
        }
    }

    let actual = sha256_of(dest).await?;
    if expected_sha256.chars().all(|c| c == '0') {
        info!("downloaded {dest:?}: sha256={actual} (unpinned)");
    } else if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(anyhow!(
            "sha256 mismatch for {dest:?}: expected {expected_sha256}, got {actual}"
        ));
    } else {
        info!("downloaded {dest:?}: sha256 verified");
    }
    Ok(())
}

async fn try_download(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("fono/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let existing = tokio::fs::metadata(dest)
        .await
        .ok()
        .map(|m| m.len())
        .unwrap_or(0);
    let mut builder = client.get(url);
    if existing > 0 {
        builder = builder.header("Range", format!("bytes={existing}-"));
    }
    let resp = builder.send().await.context("GET failed")?;
    let status = resp.status();
    if !status.is_success() && status.as_u16() != 206 {
        return Err(anyhow!("HTTP {status} for {url}"));
    }

    let total = resp.content_length().map(|c| c + existing).unwrap_or(0);
    let pb = ProgressBar::new(total.max(1));
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .unwrap()
        .progress_chars("=> "),
    );
    if existing > 0 {
        pb.set_position(existing);
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(existing > 0)
        .write(true)
        .open(dest)
        .await?;
    if existing == 0 {
        file.set_len(0).await.ok();
    }

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("stream chunk")?;
        file.write_all(&bytes).await?;
        pb.inc(bytes.len() as u64);
    }
    file.flush().await?;
    pb.finish_and_clear();
    Ok(())
}

async fn sha256_of(path: &Path) -> Result<String> {
    use tokio::io::AsyncReadExt;
    let mut f = File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}
