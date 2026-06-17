// SPDX-License-Identifier: GPL-3.0-only
//! Networked voice-discovery probe.
//!
//! The only fallible/networked step of autodiscovery. Given a catalogue
//! [`VoiceDiscovery`] descriptor and an API key, it `GET`s the provider's
//! voice list (reusing the shared [`build_auth_get`] auth helper so the wire
//! shape matches key validation), then hands the JSON to the pure
//! [`map_discovered`] transform in `fono-core`. Returns a [`Palette`] or an
//! error — callers (the `fono voices discover` command) treat an error as
//! "leave the existing palette in place", so a failed probe never breaks
//! anything.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use fono_core::provider_catalog::{build_auth_get, VoiceDiscovery};
use fono_core::voice_discovery::{map_discovered, MAX_DISCOVERED_VOICES};
use fono_core::voice_palette::Palette;

/// Default discovery timeout for the explicit `fono voices discover` command
/// and the daemon-start background refresh.
pub const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Probe `descriptor.list_url` with `api_key` and return a bounded,
/// gender-balanced [`Palette`] (capped at [`MAX_DISCOVERED_VOICES`]).
///
/// Uses [`DEFAULT_DISCOVERY_TIMEOUT`]; callers that need a tighter bound (e.g.
/// the lazy `fono voices list` refresh) use [`discover_palette_capped`].
///
/// # Errors
/// Network failure, non-2xx status, unparseable JSON, or an empty mapped
/// result all yield an error; the caller keeps the previously active palette.
pub async fn discover_palette(descriptor: &VoiceDiscovery, api_key: &str) -> Result<Palette> {
    discover_palette_capped(descriptor, api_key, MAX_DISCOVERED_VOICES, DEFAULT_DISCOVERY_TIMEOUT)
        .await
}

/// As [`discover_palette`] but with an explicit cap and request timeout
/// (exposed for tests and the bounded lazy refresh).
///
/// # Errors
/// See [`discover_palette`].
pub async fn discover_palette_capped(
    descriptor: &VoiceDiscovery,
    api_key: &str,
    max: usize,
    timeout: Duration,
) -> Result<Palette> {
    let (url, headers) =
        build_auth_get(descriptor.list_url, descriptor.auth, api_key, descriptor.extra_headers);
    let client = reqwest::Client::new();
    let mut req = client.get(&url).timeout(timeout);
    for (h, v) in &headers {
        req = req.header(h, v);
    }
    let resp = req.send().await.with_context(|| format!("GET {url} for voice discovery"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_else(|_| "<unreadable body>".to_string());
        return Err(anyhow!("voice discovery returned {status}: {}", truncate(&body, 300)));
    }
    let body: serde_json::Value = resp.json().await.context("parse voice-list JSON")?;
    let palette = map_discovered(&body, descriptor, max);
    if palette.is_empty() {
        return Err(anyhow!("provider returned no usable voices for discovery"));
    }
    Ok(palette)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s[..max].to_string();
        out.push('…');
        out
    }
}
