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

/// Predicted feasibility of running a specific whisper model on this hardware.
///
/// Returned by [`HardwareSnapshot::affords_model`]. The three buckets let the
/// wizard filter, warn, or hide models without exposing raw numbers to users.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affordability {
    /// Fits in RAM and the CPU can keep up with real-time transcription.
    /// Safe to offer as a default in the wizard.
    Comfortable,
    /// Fits in RAM but the effective real-time factor is below the live-mode
    /// threshold on this machine — batch dictation will be smooth but live
    /// preview may lag. Offered with a warning suffix in the wizard.
    Borderline,
    /// Insufficient available RAM or free disk space — the model cannot load
    /// without swapping. Hidden from the wizard shortlist; a footer explains
    /// why it was excluded.
    Unsuitable,
}

/// Minimum effective batch real-time factor required for comfortable
/// live-mode transcription on a CPU-only machine. Live mode adds
/// roughly 2–4× compute amplification on top of batch decoding
/// (overlapping windows, look-ahead context), so a model that decodes
/// at 4× batch realtime is *only just* fast enough for streaming.
///
/// Empirically: small (rf=4) on an 8-physical-core 12th-gen Intel
/// (no NPU) lags noticeably; the same model on a 12-core Zen 4 keeps
/// up. Threshold is calibrated to match that observation.
pub const LIVE_REALTIME_MIN_CPU: f32 = 6.0;

/// Minimum effective batch real-time factor on machines with hardware
/// acceleration (Apple Silicon Metal/CoreML today; future: CUDA,
/// Vulkan, Intel NPU). Whisper.cpp's accelerated path uses far less
/// CPU per audio-second, so streaming overhead is much smaller and a
/// model needs only ~1.5× batch realtime to feel snappy live.
pub const LIVE_REALTIME_MIN_ACCEL: f32 = 1.5;

/// Number of physical cores on the AVX2 reference machine used for the
/// `realtime_factor_cpu_avx2` benchmark in the model registry.
pub const REFERENCE_CORES: f32 = 8.0;

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

    /// Predict whether this machine can run a model with the given parameters.
    ///
    /// This is a pure function over the hardware snapshot — no I/O. The wizard
    /// calls it for each candidate model; `fono-stt`'s `ModelInfo` fields map
    /// directly to the three parameters so the caller avoids a circular dep
    /// (`fono-core` ← `fono-stt`):
    ///
    /// ```ignore
    /// let aff = snap.affords_model(
    ///     model.min_ram_mb,
    ///     model.approx_mb,
    ///     model.realtime_factor_cpu_avx2,
    /// );
    /// ```
    ///
    /// # Parameters
    /// - `min_ram_mb`: minimum available RAM (MiB) the model needs.
    /// - `approx_mb`: on-disk size (MiB); needs 2× headroom on free disk.
    /// - `realtime_factor_cpu_avx2`: audio-seconds per wall-second on the
    ///   8-core AVX2 reference machine ([`REFERENCE_CORES`]).
    #[must_use]
    pub fn affords_model(
        &self,
        min_ram_mb: u32,
        approx_mb: u32,
        realtime_factor_cpu_avx2: f32,
    ) -> Affordability {
        let avail_ram_mb = (self.available_ram_bytes / (1024 * 1024)) as u32;
        let free_disk_mb = (self.free_disk_bytes / (1024 * 1024)) as u32;

        // Cannot load without swapping or without disk headroom.
        if avail_ram_mb < min_ram_mb || free_disk_mb < approx_mb * 2 {
            return Affordability::Unsuitable;
        }

        // Scale the reference realtime factor by this machine's CPU capability.
        let core_scale = (self.physical_cores as f32 / REFERENCE_CORES).clamp(0.25, 2.0);
        let isa_scale = if self.cpu_features.avx2 || self.cpu_features.neon {
            1.0_f32
        } else {
            0.5 // non-vectorised path is roughly 2× slower
        };
        let effective_rf = realtime_factor_cpu_avx2 * core_scale * isa_scale;

        // Apple Silicon (and future GPU/NPU paths) decode much closer to
        // batch realtime under streaming load — use the relaxed threshold.
        let threshold = if self.accelerated() {
            LIVE_REALTIME_MIN_ACCEL
        } else {
            LIVE_REALTIME_MIN_CPU
        };

        if effective_rf < threshold {
            Affordability::Borderline
        } else {
            Affordability::Comfortable
        }
    }

    /// Whether whisper.cpp can use a hardware accelerator on this host.
    ///
    /// Currently true only on Apple Silicon (`macos` + `aarch64`), where
    /// whisper.cpp ships with Metal/CoreML kernels enabled by default.
    /// Linux/Windows GPU builds (CUDA / Vulkan) are not detected because
    /// our default release build is CPU-only; users with custom GPU
    /// builds can override the wizard's recommendation explicitly.
    #[must_use]
    pub fn accelerated(&self) -> bool {
        self.os == "macos" && self.arch == "aarch64"
    }

    /// One-line, human-readable description of the speech-recognition
    /// acceleration available on this host.
    ///
    /// The wizard prints this under the cores/ram lines so users see at a
    /// glance what hardware will be doing the speech work, and roughly how
    /// much it helps live (streaming) transcription.
    ///
    /// The expected-impact figures are rough — they reflect the ratio of
    /// the strict CPU realtime threshold to the relaxed accel threshold
    /// (`LIVE_REALTIME_MIN_CPU / LIVE_REALTIME_MIN_ACCEL` ≈ 4×) and the
    /// observation that whisper.cpp's Metal path often runs the encoder
    /// 3–5× faster than AVX2 on the same machine.
    #[must_use]
    pub fn acceleration_summary(&self) -> String {
        if self.os == "macos" && self.arch == "aarch64" {
            "Apple Silicon (Metal + CoreML) — ~3–5× faster, live mode OK".to_string()
        } else if self.cpu_features.avx512 {
            "CPU only (AVX-512) — solid for batch dictation; live mode works for tiny / base"
                .to_string()
        } else if self.cpu_features.avx2 && self.cpu_features.fma {
            "CPU only (AVX2 + FMA) — fine for batch dictation; live mode best with tiny".to_string()
        } else if self.cpu_features.avx2 {
            "CPU only (AVX2) — fine for batch dictation; live mode best with tiny".to_string()
        } else if self.cpu_features.neon {
            "CPU only (NEON) — fine for batch dictation; live mode not recommended".to_string()
        } else {
            "CPU only (no vector extensions) — local models will be very slow, cloud STT recommended".to_string()
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

    // ── affords_model tests ──────────────────────────────────────────────

    /// Realistic small.en: min_ram=1000 MiB, approx=466 MiB, rf=4.0
    const SMALL_EN: (u32, u32, f32) = (1_000, 466, 4.0);
    /// Realistic large-v3-turbo: min_ram=3400 MiB, approx=1620 MiB, rf=2.5
    const TURBO: (u32, u32, f32) = (3_400, 1_620, 2.5);
    /// Realistic tiny.en: min_ram=250 MiB, approx=75 MiB, rf=20
    const TINY_EN: (u32, u32, f32) = (250, 75, 20.0);

    fn affords(s: &HardwareSnapshot, (min_ram, approx, rf): (u32, u32, f32)) -> Affordability {
        s.affords_model(min_ram, approx, rf)
    }

    #[test]
    fn affords_tiny_comfortable_on_8_core_avx2() {
        // tiny rf=20 × 1.0 × 1.0 = 20 ≥ 6.0 (CPU threshold) → Comfortable
        let s = snap(8, 16, 100, true);
        assert_eq!(affords(&s, TINY_EN), Affordability::Comfortable);
    }

    #[test]
    fn affords_small_borderline_on_8_core_cpu_only() {
        // small rf=4.0 × 1.0 × 1.0 = 4.0 < 6.0 (CPU threshold) → Borderline.
        // Matches the user's 12th-gen Intel observation: small lags in live
        // mode without hardware acceleration.
        let s = snap(8, 16, 100, true);
        assert_eq!(affords(&s, SMALL_EN), Affordability::Borderline);
    }

    #[test]
    fn affords_small_comfortable_on_12_core_cpu_only() {
        // 12 cores: small rf=4.0 × 1.5 × 1.0 = 6.0 ≥ 6.0 → Comfortable
        let s = snap(12, 32, 200, true);
        assert_eq!(affords(&s, SMALL_EN), Affordability::Comfortable);
    }

    #[test]
    fn affords_turbo_borderline_on_cpu_only() {
        // turbo rf=2.5 × 1.5 × 1.0 = 3.75 < 6.0 → Borderline even on 12 cores.
        let s = snap(12, 32, 200, true);
        assert_eq!(affords(&s, TURBO), Affordability::Borderline);
    }

    #[test]
    fn affords_small_comfortable_on_apple_silicon() {
        // Apple Silicon: relaxed threshold (1.5). small rf=4.0 ≥ 1.5 → Comfortable
        let s = HardwareSnapshot {
            os: "macos".into(),
            arch: "aarch64".into(),
            cpu_features: CpuFeatures {
                neon: true,
                ..Default::default()
            },
            ..snap(8, 16, 100, false)
        };
        assert!(s.accelerated());
        assert_eq!(affords(&s, SMALL_EN), Affordability::Comfortable);
        assert_eq!(affords(&s, TURBO), Affordability::Comfortable);
    }

    #[test]
    fn affords_unsuitable_when_not_enough_ram() {
        let s = HardwareSnapshot {
            physical_cores: 8,
            logical_cores: 16,
            total_ram_bytes: GB * 8,
            available_ram_bytes: 512 * 1024 * 1024,
            free_disk_bytes: GB * 100,
            cpu_features: CpuFeatures {
                avx2: true,
                ..Default::default()
            },
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        assert_eq!(affords(&s, SMALL_EN), Affordability::Unsuitable);
    }

    #[test]
    fn affords_unsuitable_when_not_enough_disk() {
        // small.en needs 466 MB × 2 = 932 MB — only 800 MB free disk
        let s = HardwareSnapshot {
            physical_cores: 8,
            logical_cores: 16,
            total_ram_bytes: GB * 16,
            available_ram_bytes: GB * 8,
            free_disk_bytes: 800 * 1024 * 1024,
            cpu_features: CpuFeatures {
                avx2: true,
                ..Default::default()
            },
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        assert_eq!(affords(&s, SMALL_EN), Affordability::Unsuitable);
    }

    #[test]
    fn accelerated_only_on_apple_silicon() {
        let mac_arm = HardwareSnapshot {
            os: "macos".into(),
            arch: "aarch64".into(),
            ..snap(8, 16, 100, false)
        };
        assert!(mac_arm.accelerated());

        let mac_intel = HardwareSnapshot {
            os: "macos".into(),
            arch: "x86_64".into(),
            ..snap(8, 16, 100, true)
        };
        assert!(!mac_intel.accelerated());

        let linux = HardwareSnapshot {
            os: "linux".into(),
            arch: "x86_64".into(),
            ..snap(8, 16, 100, true)
        };
        assert!(!linux.accelerated());
    }

    #[test]
    fn acceleration_summary_apple_silicon_mentions_metal() {
        let mac_arm = HardwareSnapshot {
            os: "macos".into(),
            arch: "aarch64".into(),
            cpu_features: CpuFeatures {
                neon: true,
                ..Default::default()
            },
            ..snap(8, 16, 100, false)
        };
        let s = mac_arm.acceleration_summary();
        assert!(s.contains("Apple Silicon"));
        assert!(s.contains("Metal"));
    }

    #[test]
    fn acceleration_summary_avx2_says_cpu_only() {
        let s = snap(8, 16, 100, true).acceleration_summary();
        assert!(s.starts_with("CPU only"));
        assert!(s.contains("AVX2"));
    }

    #[test]
    fn acceleration_summary_no_vector_isa_warns() {
        let s = snap(8, 16, 100, false).acceleration_summary();
        assert!(s.contains("no vector"));
        assert!(s.contains("cloud"));
    }
}
