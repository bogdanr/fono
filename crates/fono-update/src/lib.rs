// SPDX-License-Identifier: GPL-3.0-only
//! Self-update for the `fono` binary.
//!
//! Polls the GitHub Releases API for the latest tag, compares it to
//! `CARGO_PKG_VERSION`, and on user confirmation downloads the matching
//! prebuilt binary, verifies it, and atomically replaces the running
//! executable.
//!
//! ## Asset naming
//!
//! Releases publish a single uncompressed binary per arch named
//! `fono-<tag>-<arch>` (e.g. `fono-v0.2.0-x86_64`), per
//! `.github/workflows/release.yml` and the `install` script on the site
//! branch. Distro-packaged assets (`.txz` / `.deb` / `.pkg.tar.zst`) are
//! ignored — see [`is_package_managed`].
//!
//! ## Privacy
//!
//! The check is a single unauthenticated HTTPS GET to
//! `api.github.com/repos/bogdanr/fono/releases/...`. No identifiers are
//! sent beyond the `User-Agent: fono/<version>` header. Disable via the
//! `FONO_NO_UPDATE_CHECK=1` env var or `[update] auto_check = false`
//! in `config.toml`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

pub const REPO: &str = "bogdanr/fono";
const API_LATEST: &str = "https://api.github.com/repos/bogdanr/fono/releases/latest";
const API_LIST: &str = "https://api.github.com/repos/bogdanr/fono/releases?per_page=10";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// Release channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Prerelease,
}

impl Default for Channel {
    fn default() -> Self {
        Self::Stable
    }
}

impl Channel {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "stable" => Some(Self::Stable),
            "prerelease" | "pre" | "beta" => Some(Self::Prerelease),
            _ => None,
        }
    }
}

/// Resolved metadata for the latest release matching the running binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    /// Tag as published, e.g. `v0.3.0`.
    pub tag: String,
    /// Parsed semver (tag with the leading `v` stripped).
    pub version: String,
    /// Asset filename, e.g. `fono-v0.3.0-x86_64`.
    pub asset_name: String,
    /// Direct download URL.
    pub asset_url: String,
    /// Asset size in bytes (informational; verified during download).
    pub asset_size: u64,
    /// HTML URL of the release page (for "release notes" links).
    pub html_url: String,
    /// Release notes (Markdown body), best-effort.
    pub notes: String,
    /// Release was flagged prerelease on GitHub.
    pub prerelease: bool,
    /// URL of the asset's `.sha256` sidecar, when one was published
    /// alongside the release. `None` for legacy releases (v0.1.x /
    /// v0.2.x) that predate per-asset sidecars.
    #[serde(default)]
    pub sha256_url: Option<String>,
    /// Pre-fetched sidecar digest (lowercase hex). Populated by
    /// `fetch_latest` when the sidecar is small enough to ride on the
    /// metadata request; otherwise resolved at apply time.
    #[serde(default)]
    pub expected_sha256: Option<String>,
}

impl UpdateInfo {
    /// True when this Available update represents a *cross-variant
    /// switch only* (the asset is for a different release variant of
    /// the same version, e.g. `fono-v0.5.0-x86_64` →
    /// `fono-gpu-v0.5.0-x86_64`). Compared against the running
    /// binary's version (with or without leading `v`).
    #[must_use]
    pub fn is_variant_switch_only(&self, current_version: &str) -> bool {
        self.version == strip_v(current_version)
    }
}

/// Cached/computed status of the most recent update check.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateStatus {
    UpToDate { current: String },
    Available { current: String, info: UpdateInfo },
    CheckFailed { current: String, error: String },
}

impl UpdateStatus {
    pub fn current(&self) -> &str {
        match self {
            Self::UpToDate { current }
            | Self::Available { current, .. }
            | Self::CheckFailed { current, .. } => current,
        }
    }

    pub fn available(&self) -> Option<&UpdateInfo> {
        if let Self::Available { info, .. } = self {
            Some(info)
        } else {
            None
        }
    }
}

/// Current binary version, parsed from `CARGO_PKG_VERSION` of the
/// embedding crate (the `fono` bin). Callers pass it explicitly so this
/// crate stays decoupled from the bin's `env!`.
pub fn current_version_str() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// `true` if `remote` is strictly newer than `local`. Both are
/// SemVer-ish; tags may carry a leading `v` which is stripped.
pub fn is_newer(remote: &str, local: &str) -> bool {
    let r = strip_v(remote);
    let l = strip_v(local);
    match (semver::Version::parse(r), semver::Version::parse(l)) {
        (Ok(rv), Ok(lv)) => rv > lv,
        // Fallback: lexical compare. Avoids reporting false-positives on
        // weird tags by requiring strict `>` (different + greater).
        _ => r > l,
    }
}

fn strip_v(s: &str) -> &str {
    s.strip_prefix('v').unwrap_or(s)
}

/// Asset-name prefix for the CPU-only build.
pub const CPU_ASSET_PREFIX: &str = "fono";
/// Asset-name prefix for the Vulkan-enabled GPU build.
pub const GPU_ASSET_PREFIX: &str = "fono-gpu";

/// Asset name expected for the running build, given an explicit prefix.
/// Returns `None` on platforms / arches we don't publish a binary for.
///
/// The prefix is one of `CPU_ASSET_PREFIX` or `GPU_ASSET_PREFIX`. Use
/// [`desired_asset_prefix`] to pick automatically based on the host's
/// Vulkan capability.
pub fn asset_name_for(tag: &str, prefix: &str) -> Option<String> {
    let arch = current_arch()?;
    // Linux-only today; the install script enforces the same.
    if !cfg!(target_os = "linux") {
        return None;
    }
    Some(format!("{prefix}-{tag}-{arch}"))
}

/// Pick the right release-asset prefix for this host: GPU-enabled
/// (`fono-gpu`) when Vulkan is available, CPU-only (`fono`) otherwise.
///
/// Per slice 3 of `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`, the
/// update flow is automatic — `fono update` always fetches the variant
/// that matches the host's hardware. If the user is currently on the
/// CPU build but has a GPU + libvulkan installed, the next update
/// switches them to the GPU build. If they later move to a GPU-less
/// machine, the next update switches back to CPU. No explicit flag,
/// no wizard prompt; one decision in one place.
///
/// Cost: ~50–300 ms on Mesa, ~10 ms when libvulkan is absent. Callers
/// that invoke this multiple times in one process should cache the
/// result.
#[must_use]
pub fn desired_asset_prefix() -> &'static str {
    if fono_core::vulkan_probe::probe().is_usable() {
        GPU_ASSET_PREFIX
    } else {
        CPU_ASSET_PREFIX
    }
}

fn current_arch() -> Option<&'static str> {
    Some(match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return None,
    })
}

// ---------------------------------------------------------------------
// GitHub release fetch
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("fono/", env!("CARGO_PKG_VERSION")))
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("build reqwest client")
}

fn download_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("fono/", env!("CARGO_PKG_VERSION")))
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .context("build reqwest download client")
}

/// Fetch the most recent release on the requested channel that ships an
/// asset for the running platform/arch.
/// Resolve the latest release matching the given channel and asset
/// prefix. Pass [`desired_asset_prefix()`](desired_asset_prefix) for
/// auto-pick-by-host-capability behaviour, or one of
/// [`CPU_ASSET_PREFIX`] / [`GPU_ASSET_PREFIX`] to force a specific
/// variant.
pub async fn fetch_latest(channel: Channel, asset_prefix: &str) -> Result<GhReleaseChoice> {
    let client = http_client()?;
    let releases: Vec<GhRelease> = match channel {
        Channel::Stable => {
            let r = client.get(API_LATEST).send().await.context("GET latest")?;
            if !r.status().is_success() {
                anyhow::bail!("github api returned HTTP {}", r.status());
            }
            vec![r.json::<GhRelease>().await.context("parse latest")?]
        }
        Channel::Prerelease => {
            let r = client.get(API_LIST).send().await.context("GET releases")?;
            if !r.status().is_success() {
                anyhow::bail!("github api returned HTTP {}", r.status());
            }
            r.json::<Vec<GhRelease>>().await.context("parse releases")?
        }
    };
    pick_release(&releases, channel, asset_prefix)
}

#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct GhReleaseChoice {
    pub tag: String,
    pub html_url: String,
    pub notes: String,
    pub prerelease: bool,
    pub asset_name: String,
    pub asset_url: String,
    pub asset_size: u64,
    pub sha256_url: Option<String>,
    pub expected_sha256: Option<String>,
}

fn pick_release(
    releases: &[GhRelease],
    channel: Channel,
    asset_prefix: &str,
) -> Result<GhReleaseChoice> {
    for r in releases {
        if r.draft {
            continue;
        }
        if matches!(channel, Channel::Stable) && r.prerelease {
            continue;
        }
        let Some(want) = asset_name_for(&r.tag_name, asset_prefix) else {
            anyhow::bail!(
                "no published binary for {}/{}; install via your package manager",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
        };
        if let Some(asset) = r.assets.iter().find(|a| a.name == want) {
            // Look for a sibling `<asset>.sha256` published in the
            // same release. Wave 2 Thread B — supply-chain hardening:
            // when the sidecar is present, `apply_update` requires the
            // streamed digest to match it.
            let sidecar_name = format!("{}.sha256", asset.name);
            let sha256_url = r
                .assets
                .iter()
                .find(|a| a.name == sidecar_name)
                .map(|a| a.browser_download_url.clone());
            return Ok(GhReleaseChoice {
                tag: r.tag_name.clone(),
                html_url: r.html_url.clone(),
                notes: r.body.clone().unwrap_or_default(),
                prerelease: r.prerelease,
                asset_name: asset.name.clone(),
                asset_url: asset.browser_download_url.clone(),
                asset_size: asset.size,
                sha256_url,
                // Resolved lazily in apply_update; pre-fetching here
                // would couple `fetch_latest` to the download client.
                expected_sha256: None,
            });
        }
    }
    Err(anyhow!(
        "no matching release asset found on the {channel:?} channel"
    ))
}

/// Compare the latest release against `current_version` and return a
/// classified [`UpdateStatus`]. Honours `FONO_NO_UPDATE_CHECK=1`.
///
/// `current_asset_prefix` is the prefix of *this running binary*
/// (`CPU_ASSET_PREFIX` for the default `fono` build, `GPU_ASSET_PREFIX`
/// for `fono-gpu`). The check internally computes the *desired* prefix
/// via [`desired_asset_prefix()`](desired_asset_prefix) (Vulkan-probe
/// based) and treats a prefix mismatch as an available update even when
/// the version hasn't changed — that's how the auto-switch from CPU to
/// GPU lands when a user's hardware gains a usable GPU.
pub async fn check(
    current_version: &str,
    current_asset_prefix: &str,
    channel: Channel,
) -> UpdateStatus {
    if std::env::var_os("FONO_NO_UPDATE_CHECK").is_some_and(|v| v == "1") {
        return UpdateStatus::UpToDate {
            current: current_version.to_string(),
        };
    }
    let desired_prefix = desired_asset_prefix();
    match fetch_latest(channel, desired_prefix).await {
        Ok(choice) => {
            let remote = strip_v(&choice.tag).to_string();
            // A prefix mismatch is "an update is available" even at the
            // same version — that's how the host-capability change
            // (e.g. plugged in an eGPU) propagates to the user without
            // waiting for a new release. But **never** offer a
            // downgrade: if `remote` is strictly older than `current`
            // (e.g. v0.6.0 was published as a Draft on GitHub so the
            // releases API still returns v0.5.0 as `latest`, while
            // the user is already running v0.6.0), the variant-switch
            // path would otherwise surface "Update to v0.5.0" — a
            // bogus downgrade.
            let variant_changed = desired_prefix != current_asset_prefix;
            let remote_is_newer = is_newer(&choice.tag, current_version);
            let remote_is_older = is_newer(current_version, &choice.tag);
            let allow_variant_switch = variant_changed && !remote_is_older;
            if remote_is_newer || allow_variant_switch {
                UpdateStatus::Available {
                    current: current_version.to_string(),
                    info: UpdateInfo {
                        tag: choice.tag,
                        version: remote,
                        asset_name: choice.asset_name,
                        asset_url: choice.asset_url,
                        asset_size: choice.asset_size,
                        html_url: choice.html_url,
                        notes: choice.notes,
                        prerelease: choice.prerelease,
                        sha256_url: choice.sha256_url,
                        expected_sha256: choice.expected_sha256,
                    },
                }
            } else {
                UpdateStatus::UpToDate {
                    current: current_version.to_string(),
                }
            }
        }
        Err(e) => UpdateStatus::CheckFailed {
            current: current_version.to_string(),
            error: format!("{e:#}"),
        },
    }
}

// ---------------------------------------------------------------------
// Cache (last-known check on disk)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedCheck {
    /// Unix seconds since epoch.
    pub checked_at: u64,
    pub status: UpdateStatus,
}

/// Read the persisted cache, if any. Best-effort; errors return `None`.
pub fn load_cache(path: &Path) -> Option<CachedCheck> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Persist a check. Best-effort.
pub fn save_cache(path: &Path, status: &UpdateStatus) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = CachedCheck {
        checked_at: now,
        status: status.clone(),
    };
    let Ok(json) = serde_json::to_string_pretty(&entry) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, json);
}

// ---------------------------------------------------------------------
// Package-manager detection
// ---------------------------------------------------------------------

/// Heuristic: the running binary lives in a directory typically owned
/// by the system package manager. In that case `apply_update` should
/// refuse to overwrite — `pacman` / `dpkg` / `installpkg` track that
/// file and a self-replace would fight them.
pub fn is_package_managed(exe: &Path) -> bool {
    let s = exe.to_string_lossy();
    // Distro-owned bin dirs. `/usr/local/bin` is left writable for
    // self-update because the install script defaults to it, mirroring
    // `install:13` semantics.
    s.starts_with("/usr/bin/") || s.starts_with("/bin/") || s.starts_with("/usr/sbin/")
}

// ---------------------------------------------------------------------
// Apply (download + verify + atomic swap)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ApplyOpts {
    /// Skip the rename-into-place step. Useful for `--dry-run`.
    pub dry_run: bool,
    /// Override the target binary path. `None` → `current_exe()`.
    pub target_override: Option<PathBuf>,
}

/// Outcome of a successful [`apply_update`] call.
#[derive(Debug, Clone)]
pub struct ApplyOutcome {
    pub installed_at: PathBuf,
    pub backup_at: Option<PathBuf>,
    pub bytes: u64,
    pub sha256: String,
}

/// Download the asset described by `info` and atomically replace the
/// running executable with it. Returns the new path and the path to a
/// `.bak` of the previous binary (which the caller may keep for
/// rollback or remove on first successful re-exec).
///
/// Rejects package-manager-owned installs (see [`is_package_managed`]).
#[allow(clippy::too_many_lines)]
pub async fn apply_update(info: &UpdateInfo, opts: ApplyOpts) -> Result<ApplyOutcome> {
    let target: PathBuf = if let Some(p) = opts.target_override.as_ref() {
        p.clone()
    } else {
        std::env::current_exe().context("resolve current_exe")?
    };
    let target = std::fs::canonicalize(&target).unwrap_or(target);

    if is_package_managed(&target) {
        anyhow::bail!(
            "{} is owned by the system package manager; \
             update via your distro's package manager instead of `fono update`",
            target.display()
        );
    }

    let dir = target
        .parent()
        .ok_or_else(|| anyhow!("target {} has no parent dir", target.display()))?;

    // Writability check up-front so we fail before downloading.
    let probe = tempfile::Builder::new()
        .prefix(".fono-update-probe-")
        .tempfile_in(dir);
    if let Err(e) = probe {
        anyhow::bail!(
            "cannot write to {} ({e}); try `sudo fono update`",
            dir.display()
        );
    }
    drop(probe);

    // Download to a sibling temp file so the final rename is atomic
    // (same filesystem). Verifies HTTPS, content-length and SHA-256
    // along the way.
    let mut tmp = tempfile::Builder::new()
        .prefix(".fono-update-")
        .tempfile_in(dir)
        .with_context(|| format!("create temp file in {}", dir.display()))?;

    if !info.asset_url.starts_with("https://") {
        anyhow::bail!("refusing non-HTTPS asset URL: {}", info.asset_url);
    }

    let (bytes, sha) = stream_download(&info.asset_url, tmp.as_file_mut()).await?;
    if info.asset_size > 0 && bytes != info.asset_size {
        anyhow::bail!(
            "downloaded {} bytes, GitHub announced {}",
            bytes,
            info.asset_size
        );
    }

    // Wave 2 Thread B — verify against the published `.sha256` sidecar
    // when one is available. Fail-closed on mismatch (the temp file is
    // dropped without renaming, so the running binary stays intact).
    // Fail-warn-and-proceed when no sidecar was published, for
    // back-compat with v0.1.x / v0.2.x releases that predate the
    // sidecar publication step in release.yml.
    let expected = if let Some(hex) = info.expected_sha256.as_deref() {
        Some(hex.to_ascii_lowercase())
    } else if let Some(url) = info.sha256_url.as_deref() {
        match fetch_sidecar(url, &info.asset_name).await {
            Ok(hex) => Some(hex),
            Err(e) => {
                anyhow::bail!(
                    "failed to fetch .sha256 sidecar from {url}: {e:#}; \
                     refusing to apply unverified binary"
                );
            }
        }
    } else {
        None
    };
    if let Some(want) = expected.as_deref() {
        if !sha.eq_ignore_ascii_case(want) {
            anyhow::bail!(
                "sha256 mismatch for {}: downloaded {} but sidecar published {}; \
                 refusing to apply (running binary unchanged)",
                info.asset_name,
                sha,
                want
            );
        }
    } else {
        tracing::warn!(
            tag = info.tag.as_str(),
            "no .sha256 sidecar published for {}; trusting Content-Length + TLS only",
            info.tag
        );
    }

    // Set 0755 before swapping so the new binary is immediately
    // executable when the rename lands.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod 0755 {}", tmp.path().display()))?;
    }

    if opts.dry_run {
        return Ok(ApplyOutcome {
            installed_at: target,
            backup_at: None,
            bytes,
            sha256: sha,
        });
    }

    // Keep a `.bak` of the previous binary so the caller can roll back
    // if the new build fails to start. Best-effort: if the rename fails
    // (e.g. /usr/local/bin owned by root), surface a clear error.
    let backup = target.with_extension("bak");
    let _ = std::fs::remove_file(&backup);
    if let Err(e) = std::fs::rename(&target, &backup) {
        anyhow::bail!(
            "cannot rename {} -> {} ({e}); try `sudo fono update`",
            target.display(),
            backup.display()
        );
    }

    // Persist the temp file into the final path. `persist` does
    // `rename(tmp, target)` — atomic on the same filesystem because we
    // created the temp in the same dir.
    tmp.persist(&target)
        .map_err(|e| anyhow!("persist into {}: {}", target.display(), e.error))?;

    Ok(ApplyOutcome {
        installed_at: target,
        backup_at: Some(backup),
        bytes,
        sha256: sha,
    })
}

async fn stream_download(url: &str, out: &mut std::fs::File) -> Result<(u64, String)> {
    use futures::StreamExt;
    use sha2::{Digest, Sha256};
    use std::io::Write;

    let client = download_client()?;
    let resp = client.get(url).send().await.context("GET asset")?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} fetching {}", resp.status(), url);
    }

    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("stream chunk")?;
        hasher.update(&bytes);
        out.write_all(&bytes).context("write temp file")?;
        total += bytes.len() as u64;
    }
    out.flush().ok();
    out.sync_all().ok();
    Ok((total, hex::encode(hasher.finalize())))
}

/// Fetch the `.sha256` sidecar from GitHub and parse it.
async fn fetch_sidecar(url: &str, expected_filename: &str) -> Result<String> {
    if !url.starts_with("https://") {
        anyhow::bail!("refusing non-HTTPS sidecar URL: {url}");
    }
    let client = download_client()?;
    let resp = client.get(url).send().await.context("GET sidecar")?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} fetching {url}", resp.status());
    }
    let body = resp.text().await.context("read sidecar body")?;
    parse_sha256_sidecar(&body, expected_filename)
}

/// Parse a `.sha256` sidecar body and extract the digest matching
/// `expected_filename`.
///
/// Tolerates the four canonical sidecar shapes:
///
/// * `<hex>\n` (sha256sum bare digest, no filename column)
/// * `<hex>  <name>\n` (sha256sum default "text mode")
/// * `<hex> *<name>\n` (sha256sum "binary mode")
/// * Multi-entry sidecars — picks the row whose filename matches
///   `expected_filename`, otherwise falls back to the first valid row.
///
/// Trailing whitespace and blank lines are skipped. Rejects too-short
/// or non-hex digests.
pub fn parse_sha256_sidecar(body: &str, expected_filename: &str) -> Result<String> {
    let mut first_valid: Option<String> = None;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Split on first whitespace run.
        let mut parts = line.splitn(2, char::is_whitespace);
        let Some(hex) = parts.next() else {
            continue;
        };
        if !is_valid_sha256_hex(hex) {
            continue;
        }
        let rest = parts.next().unwrap_or("").trim();
        // Strip the optional `*` binary-mode prefix.
        let name = rest.strip_prefix('*').unwrap_or(rest);
        let hex_lc = hex.to_ascii_lowercase();
        if name.is_empty() || name == expected_filename {
            return Ok(hex_lc);
        }
        if first_valid.is_none() {
            first_valid = Some(hex_lc);
        }
    }
    first_valid.ok_or_else(|| anyhow!("no valid sha256 digest in sidecar body"))
}

fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Replace the running process with the binary at `target`, preserving
/// the original argv. On Unix this uses `execv` so the PID is
/// preserved. Never returns on success.
///
/// **The caller MUST pass the path of the just-installed binary
/// (`ApplyOutcome::installed_at`), not rely on `std::env::current_exe()`.**
/// `apply_update` does a `rename(old → old.bak); rename(tmp → old)`
/// dance that leaves the running process's inode at `old.bak`. On
/// Linux, `/proc/self/exe` (and therefore `current_exe()`) resolves
/// to the inode the kernel tracks for the running process — i.e. to
/// `old.bak` after the rename. Exec'ing that path re-runs the OLD
/// binary, defeating the update. Always pass the post-update target
/// path explicitly.
#[cfg(unix)]
pub fn restart_in_place(target: &Path) -> Result<std::convert::Infallible> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let exe_c = CString::new(target.as_os_str().as_bytes()).context("exe path NUL")?;
    let args: Vec<CString> = std::env::args_os()
        .filter_map(|a| CString::new(a.as_bytes()).ok())
        .collect();
    let mut argv: Vec<*const libc::c_char> = args.iter().map(|c| c.as_ptr()).collect();
    argv.push(std::ptr::null());

    // SAFETY: argv terminated with NULL; CStrings live until execv either
    // succeeds (process image replaced) or fails and we return immediately.
    unsafe {
        libc::execv(exe_c.as_ptr(), argv.as_ptr());
    }
    Err(anyhow!(
        "execv {} returned: {}",
        target.display(),
        std::io::Error::last_os_error()
    ))
}

#[cfg(not(unix))]
pub fn restart_in_place(_target: &Path) -> Result<std::convert::Infallible> {
    anyhow::bail!("in-place restart not supported on this platform");
}

#[cfg(unix)]
mod libc {
    pub use std::ffi::c_char;
    extern "C" {
        pub fn execv(path: *const c_char, argv: *const *const c_char) -> i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_detects_minor_bump() {
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(is_newer("0.2.1", "v0.2.0"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("v0.1.9", "0.2.0"));
    }

    /// Regression: when the GitHub releases API serves an OLDER
    /// release as "latest" (e.g. v0.6.0 was tagged but only published
    /// as a Draft, so the API still returns v0.5.0), a CPU-variant
    /// binary running on a Vulkan-capable host must NOT trip the
    /// variant-switch branch into surfacing a downgrade prompt.
    /// The combination is: remote=v0.5.0, current=0.6.0,
    /// variant_changed=true → must yield UpToDate, not Available.
    #[test]
    fn variant_switch_never_offers_downgrade() {
        let remote = "v0.5.0";
        let current = "0.6.0";
        let variant_changed = true;
        let remote_is_newer = is_newer(remote, current);
        let remote_is_older = is_newer(current, remote);
        let allow_variant_switch = variant_changed && !remote_is_older;
        let available = remote_is_newer || allow_variant_switch;
        assert!(!available, "must not surface a downgrade prompt");
    }

    /// Same-version variant switch is the legitimate case the
    /// downgrade guard must NOT block: remote and current at the same
    /// version, but the desired prefix differs (CPU host gained a
    /// usable GPU). Should yield Available so the user can switch
    /// variants in place.
    #[test]
    fn variant_switch_at_same_version_offers_update() {
        let remote = "v0.6.0";
        let current = "0.6.0";
        let variant_changed = true;
        let remote_is_newer = is_newer(remote, current);
        let remote_is_older = is_newer(current, remote);
        let allow_variant_switch = variant_changed && !remote_is_older;
        let available = remote_is_newer || allow_variant_switch;
        assert!(available, "same-version variant switch must still fire");
    }

    #[test]
    fn channel_parse() {
        assert_eq!(Channel::parse("stable"), Some(Channel::Stable));
        assert_eq!(Channel::parse("Pre"), Some(Channel::Prerelease));
        assert_eq!(Channel::parse("nightly"), None);
    }

    #[test]
    fn asset_name_includes_arch_on_linux() {
        if cfg!(target_os = "linux") {
            let cpu = asset_name_for("v1.2.3", CPU_ASSET_PREFIX).unwrap();
            assert!(cpu.starts_with("fono-v1.2.3-"));
            let gpu = asset_name_for("v1.2.3", GPU_ASSET_PREFIX).unwrap();
            assert!(gpu.starts_with("fono-gpu-v1.2.3-"));
        }
    }

    #[test]
    fn pkg_managed_paths() {
        assert!(is_package_managed(Path::new("/usr/bin/fono")));
        assert!(is_package_managed(Path::new("/bin/fono")));
        assert!(!is_package_managed(Path::new("/usr/local/bin/fono")));
        assert!(!is_package_managed(Path::new("/home/u/.cargo/bin/fono")));
    }

    #[test]
    fn cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("update.json");
        let st = UpdateStatus::UpToDate {
            current: "0.2.0".into(),
        };
        save_cache(&p, &st);
        let loaded = load_cache(&p).unwrap();
        assert_eq!(loaded.status.current(), "0.2.0");
    }

    #[test]
    fn parse_sidecar_bare_digest() {
        let hex = "a".repeat(64);
        let body = format!("{hex}\n");
        let got = parse_sha256_sidecar(&body, "fono-v1-x86_64").expect("parse");
        assert_eq!(got, hex);
    }

    #[test]
    fn parse_sidecar_text_mode() {
        let hex = "b".repeat(64);
        let body = format!("{hex}  fono-v1-x86_64\n");
        let got = parse_sha256_sidecar(&body, "fono-v1-x86_64").expect("parse");
        assert_eq!(got, hex);
    }

    #[test]
    fn parse_sidecar_binary_mode() {
        let hex = "c".repeat(64);
        let body = format!("{hex} *fono-v1-x86_64\n");
        let got = parse_sha256_sidecar(&body, "fono-v1-x86_64").expect("parse");
        assert_eq!(got, hex);
    }

    #[test]
    fn parse_sidecar_multi_entry_picks_matching_filename() {
        let want = "d".repeat(64);
        let other = "e".repeat(64);
        let body = format!("{other}  fono-v1-aarch64\n{want}  fono-v1-x86_64\n");
        let got = parse_sha256_sidecar(&body, "fono-v1-x86_64").expect("parse");
        assert_eq!(got, want);
    }

    #[test]
    fn parse_sidecar_rejects_short_or_non_hex() {
        // Too short.
        assert!(parse_sha256_sidecar("deadbeef\n", "x").is_err());
        // Non-hex character.
        let bad = format!("{}{}\n", "z".repeat(63), "a");
        assert!(parse_sha256_sidecar(&bad, "x").is_err());
        // Empty body.
        assert!(parse_sha256_sidecar("\n\n\n", "x").is_err());
    }

    #[test]
    fn parse_sidecar_uppercase_normalised_to_lowercase() {
        let hex = "A".repeat(64);
        let body = format!("{hex}  fono-v1-x86_64\n");
        let got = parse_sha256_sidecar(&body, "fono-v1-x86_64").expect("parse");
        assert_eq!(got, hex.to_ascii_lowercase());
    }
}
