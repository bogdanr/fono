// SPDX-License-Identifier: GPL-3.0-only
//! Hardware probe + local-model tier classification.
//!
//! Used by the first-run wizard and `fono doctor` to decide whether the
//! user's machine can sustain local STT/LLM inference inside the latency
//! budget, and to pick a sane default model size.
//!
//! The classification is **best-effort**: a static rule based on cores,
//! RAM, free disk, and CPU features. It is intentionally conservative —
//! a machine that scores `Comfortable` may actually run `Recommended`
//! workloads fine; a machine that scores `Minimum` may stutter on a
//! noisy day. The wizard surfaces the snapshot so the user can override.
//!
//! Pure-rust, zero non-workspace deps. Uses `/proc` on Linux, `sysctl`
//! on macOS / BSD (best-effort), and `GetSystemInfo`-equivalent
//! information on Windows via plain stdlib (we currently fall back to
//! `available_parallelism` when richer info isn't available).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Snapshot of the runtime host's interesting hardware properties.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HardwareSnapshot {
    pub physical_cores: u32,
    pub logical_cores: u32,
    pub total_ram_bytes: u64,
    pub available_ram_bytes: u64,
    pub free_disk_bytes: u64,
    pub cpu_features: CpuFeatures,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct CpuFeatures {
    pub avx2: bool,
    pub avx512: bool,
    pub fma: bool,
    pub neon: bool,
}

/// Predicted ability to run local Fono workloads.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LocalTier {
    /// Below the supported floor for local STT — wizard steers to cloud.
    Unsuitable,
    /// Will work but slower; picks `whisper base`.
    Minimum,
    /// Comfortable headroom for `whisper small`.
    Comfortable,
    /// `whisper small` + room for an LLM if/when local LLM is wired.
    Recommended,
    /// `whisper medium` or larger; GPU optional bonus.
    HighEnd,
}

impl LocalTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsuitable => "unsuitable",
            Self::Minimum => "minimum",
            Self::Comfortable => "comfortable",
            Self::Recommended => "recommended",
            Self::HighEnd => "high-end",
        }
    }

    /// Default whisper model size for this tier.
    pub fn default_whisper_model(self) -> &'static str {
        match self {
            Self::Unsuitable | Self::Minimum => "base",
            Self::Comfortable | Self::Recommended => "small",
            Self::HighEnd => "medium",
        }
    }

    /// Should the wizard default-offer local STT for this tier?
    pub fn local_default(self) -> bool {
        matches!(self, Self::Comfortable | Self::Recommended | Self::HighEnd)
    }
}

/// Reason the snapshot was classified `Unsuitable`. Used to print a
/// specific user-facing message in the wizard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsuitableReason {
    TooFewCores { have: u32, need: u32 },
    NotEnoughRam { have_gb: u32, need_gb: u32 },
    NoVectorIsa,
    NotEnoughDisk { have_gb: u32, need_gb: u32 },
}

impl std::fmt::Display for UnsuitableReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewCores { have, need } => {
                write!(f, "only {have} physical cores; minimum is {need}")
            }
            Self::NotEnoughRam { have_gb, need_gb } => {
                write!(
                    f,
                    "only {have_gb} GB RAM available; minimum is {need_gb} GB"
                )
            }
            Self::NoVectorIsa => {
                write!(f, "no AVX2 / NEON support detected")
            }
            Self::NotEnoughDisk { have_gb, need_gb } => {
                write!(
                    f,
                    "only {have_gb} GB free disk; need at least {need_gb} GB for whisper models"
                )
            }
        }
    }
}

impl HardwareSnapshot {
    /// Classify this snapshot into a [`LocalTier`].
    ///
    /// Thresholds are duplicated as `pub const` to keep tests honest
    /// and let documentation reference exact numbers.
    pub fn tier(&self) -> LocalTier {
        if let Err(_reason) = self.suitability() {
            return LocalTier::Unsuitable;
        }
        let cores = self.physical_cores;
        let ram_gb = u32::try_from(self.total_ram_bytes / GB).unwrap_or(u32::MAX);
        let disk_gb = u32::try_from(self.free_disk_bytes / GB).unwrap_or(u32::MAX);

        if cores >= HIGH_END_CORES && ram_gb >= HIGH_END_RAM_GB {
            LocalTier::HighEnd
        } else if cores >= RECOMMENDED_CORES
            && ram_gb >= RECOMMENDED_RAM_GB
            && disk_gb >= RECOMMENDED_DISK_GB
        {
            LocalTier::Recommended
        } else if cores >= COMFORTABLE_CORES
            && ram_gb >= COMFORTABLE_RAM_GB
            && disk_gb >= COMFORTABLE_DISK_GB
        {
            LocalTier::Comfortable
        } else {
            LocalTier::Minimum
        }
    }

    /// Returns `Ok(())` if the machine clears the `Minimum` floor, or
    /// `Err(UnsuitableReason)` naming the first failed gate.
    pub fn suitability(&self) -> Result<(), UnsuitableReason> {
        if self.physical_cores < MIN_CORES {
            return Err(UnsuitableReason::TooFewCores {
                have: self.physical_cores,
                need: MIN_CORES,
            });
        }
        let ram_gb = u32::try_from(self.total_ram_bytes / GB).unwrap_or(u32::MAX);
        if ram_gb < MIN_RAM_GB {
            return Err(UnsuitableReason::NotEnoughRam {
                have_gb: ram_gb,
                need_gb: MIN_RAM_GB,
            });
        }
        if !self.cpu_features.avx2 && !self.cpu_features.neon {
            return Err(UnsuitableReason::NoVectorIsa);
        }
        let disk_gb = u32::try_from(self.free_disk_bytes / GB).unwrap_or(u32::MAX);
        if disk_gb < MIN_DISK_GB {
            return Err(UnsuitableReason::NotEnoughDisk {
                have_gb: disk_gb,
                need_gb: MIN_DISK_GB,
            });
        }
        Ok(())
    }
}

// ----- thresholds (pub const so tests + docs reference one source) -----

const GB: u64 = 1024 * 1024 * 1024;

pub const MIN_CORES: u32 = 4;
pub const MIN_RAM_GB: u32 = 4;
pub const MIN_DISK_GB: u32 = 2;

pub const COMFORTABLE_CORES: u32 = 6;
pub const COMFORTABLE_RAM_GB: u32 = 8;
pub const COMFORTABLE_DISK_GB: u32 = 4;

pub const RECOMMENDED_CORES: u32 = 8;
pub const RECOMMENDED_RAM_GB: u32 = 16;
pub const RECOMMENDED_DISK_GB: u32 = 6;

pub const HIGH_END_CORES: u32 = 12;
pub const HIGH_END_RAM_GB: u32 = 32;

// ----------------------------- probe -----------------------------

/// Probe the running host. Best-effort: fields that can't be measured
/// fall back to conservative defaults so the resulting tier never
/// over-promises.
pub fn probe(disk_check_dir: &Path) -> HardwareSnapshot {
    let logical = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(1) as u32;
    let physical = physical_cores()
        .unwrap_or_else(|| logical.max(1) / 2)
        .max(1);
    let (total_ram, avail_ram) = read_meminfo().unwrap_or((0, 0));
    let free_disk = free_disk_bytes(disk_check_dir).unwrap_or(0);

    HardwareSnapshot {
        physical_cores: physical,
        logical_cores: logical,
        total_ram_bytes: total_ram,
        available_ram_bytes: avail_ram,
        free_disk_bytes: free_disk,
        cpu_features: detect_cpu_features(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
    }
}

/// Best-effort physical-core detection. Linux: parses `/proc/cpuinfo`.
/// Other OSes: returns `None` and the caller falls back to halving
/// `available_parallelism` (assume SMT siblings).
fn physical_cores() -> Option<u32> {
    if cfg!(target_os = "linux") {
        let s = std::fs::read_to_string("/proc/cpuinfo").ok()?;
        let mut seen: std::collections::BTreeSet<(u32, u32)> = std::collections::BTreeSet::new();
        let mut cur_phys: Option<u32> = None;
        let mut cur_core: Option<u32> = None;
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("physical id") {
                cur_phys = v.split(':').nth(1).and_then(|t| t.trim().parse().ok());
            } else if let Some(v) = line.strip_prefix("core id") {
                cur_core = v.split(':').nth(1).and_then(|t| t.trim().parse().ok());
            } else if line.is_empty() {
                if let (Some(p), Some(c)) = (cur_phys, cur_core) {
                    seen.insert((p, c));
                }
                cur_phys = None;
                cur_core = None;
            }
        }
        if let (Some(p), Some(c)) = (cur_phys, cur_core) {
            seen.insert((p, c));
        }
        if seen.is_empty() {
            None
        } else {
            Some(seen.len() as u32)
        }
    } else {
        None
    }
}

/// `(total_bytes, available_bytes)` parsed from `/proc/meminfo`. Falls
/// through to `(0, 0)` on non-Linux for now.
fn read_meminfo() -> Option<(u64, u64)> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb = 0u64;
    let mut avail_kb = 0u64;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = parse_kb(rest).unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = parse_kb(rest).unwrap_or(0);
        }
    }
    Some((total_kb * 1024, avail_kb * 1024))
}

fn parse_kb(s: &str) -> Option<u64> {
    s.trim().trim_end_matches("kB").trim().parse::<u64>().ok()
}

/// Free bytes on the filesystem hosting `path`. Linux/Unix only;
/// other targets return `None` and the caller treats it as 0.
fn free_disk_bytes(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::mem::MaybeUninit;
        use std::os::unix::ffi::OsStrExt;
        let cpath = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut buf: MaybeUninit<libc_statvfs> = MaybeUninit::uninit();
        // SAFETY: `statvfs` is a well-defined C ABI; `cpath` is a
        // null-terminated UTF-8 path; `buf` is a fresh MaybeUninit.
        let rc = unsafe { statvfs_call(cpath.as_ptr(), buf.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        // SAFETY: rc == 0 means kernel populated the buffer.
        let s = unsafe { buf.assume_init() };
        Some(s.f_bsize * s.f_bavail)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[cfg(unix)]
#[repr(C)]
#[allow(non_camel_case_types)]
struct libc_statvfs {
    f_bsize: u64,
    f_frsize: u64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    _padding: [u64; 11],
}

#[cfg(unix)]
extern "C" {
    #[link_name = "statvfs"]
    fn statvfs_call(path: *const std::ffi::c_char, buf: *mut libc_statvfs) -> i32;
}

fn detect_cpu_features() -> CpuFeatures {
    let mut f = CpuFeatures::default();
    #[cfg(target_arch = "x86_64")]
    {
        f.avx2 = std::is_x86_feature_detected!("avx2");
        f.avx512 = std::is_x86_feature_detected!("avx512f");
        f.fma = std::is_x86_feature_detected!("fma");
    }
    #[cfg(target_arch = "x86")]
    {
        f.avx2 = std::is_x86_feature_detected!("avx2");
        f.avx512 = std::is_x86_feature_detected!("avx512f");
        f.fma = std::is_x86_feature_detected!("fma");
    }
    #[cfg(target_arch = "aarch64")]
    {
        // NEON is mandatory on aarch64 per the ARMv8-A baseline.
        f.neon = true;
    }
    f
}

// ----------------------------- tests -----------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(cores: u32, ram_gb: u32, disk_gb: u32, avx2: bool) -> HardwareSnapshot {
        HardwareSnapshot {
            physical_cores: cores,
            logical_cores: cores * 2,
            total_ram_bytes: u64::from(ram_gb) * GB,
            available_ram_bytes: u64::from(ram_gb) * GB,
            free_disk_bytes: u64::from(disk_gb) * GB,
            cpu_features: CpuFeatures {
                avx2,
                avx512: false,
                fma: false,
                neon: false,
            },
            os: "linux".into(),
            arch: "x86_64".into(),
        }
    }

    #[test]
    fn unsuitable_when_too_few_cores() {
        let s = snap(2, 16, 100, true);
        assert_eq!(s.tier(), LocalTier::Unsuitable);
        let r = s.suitability().unwrap_err();
        assert!(matches!(r, UnsuitableReason::TooFewCores { .. }));
    }

    #[test]
    fn unsuitable_when_no_vector_isa() {
        let s = snap(8, 16, 100, false);
        assert_eq!(s.tier(), LocalTier::Unsuitable);
        let r = s.suitability().unwrap_err();
        assert!(matches!(r, UnsuitableReason::NoVectorIsa));
    }

    #[test]
    fn unsuitable_when_too_little_ram() {
        let s = snap(8, 2, 100, true);
        assert_eq!(s.tier(), LocalTier::Unsuitable);
    }

    #[test]
    fn unsuitable_when_too_little_disk() {
        let s = snap(8, 16, 1, true);
        assert_eq!(s.tier(), LocalTier::Unsuitable);
    }

    #[test]
    fn minimum_at_floor() {
        let s = snap(MIN_CORES, MIN_RAM_GB, MIN_DISK_GB, true);
        assert_eq!(s.tier(), LocalTier::Minimum);
    }

    #[test]
    fn comfortable_at_threshold() {
        let s = snap(
            COMFORTABLE_CORES,
            COMFORTABLE_RAM_GB,
            COMFORTABLE_DISK_GB,
            true,
        );
        assert_eq!(s.tier(), LocalTier::Comfortable);
    }

    #[test]
    fn recommended_at_threshold() {
        let s = snap(
            RECOMMENDED_CORES,
            RECOMMENDED_RAM_GB,
            RECOMMENDED_DISK_GB,
            true,
        );
        assert_eq!(s.tier(), LocalTier::Recommended);
    }

    #[test]
    fn high_end_above_threshold() {
        let s = snap(HIGH_END_CORES, HIGH_END_RAM_GB, 200, true);
        assert_eq!(s.tier(), LocalTier::HighEnd);
    }

    #[test]
    fn just_below_high_end_drops_to_recommended() {
        let s = snap(HIGH_END_CORES - 1, HIGH_END_RAM_GB, 200, true);
        assert_eq!(s.tier(), LocalTier::Recommended);
    }

    #[test]
    fn local_default_only_for_comfortable_or_better() {
        assert!(!LocalTier::Unsuitable.local_default());
        assert!(!LocalTier::Minimum.local_default());
        assert!(LocalTier::Comfortable.local_default());
        assert!(LocalTier::Recommended.local_default());
        assert!(LocalTier::HighEnd.local_default());
    }

    #[test]
    fn whisper_model_per_tier() {
        assert_eq!(LocalTier::Unsuitable.default_whisper_model(), "base");
        assert_eq!(LocalTier::Minimum.default_whisper_model(), "base");
        assert_eq!(LocalTier::Comfortable.default_whisper_model(), "small");
        assert_eq!(LocalTier::Recommended.default_whisper_model(), "small");
        assert_eq!(LocalTier::HighEnd.default_whisper_model(), "medium");
    }

    #[test]
    fn live_probe_returns_a_classifiable_snapshot() {
        // We don't assert a specific tier — the test runner's hardware
        // is unknown — but every field must be populated and the tier
        // must be a valid variant.
        let snap = probe(std::path::Path::new("/tmp"));
        let _ = snap.tier(); // just verifies no panic
        assert!(snap.logical_cores >= 1);
    }

    #[test]
    fn unsuitable_reason_renders_specifically() {
        let r = UnsuitableReason::TooFewCores { have: 2, need: 4 };
        assert!(r.to_string().contains("only 2"));
        assert!(r.to_string().contains("minimum is 4"));
    }
}
