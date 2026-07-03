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
    /// Three-class GPU summary derived from the Vulkan probe. See
    /// [`HostGpu`] and ADR 0028. Defaults to [`HostGpu::None`] for
    /// snapshots that pre-date the field (serde `default`).
    #[serde(default)]
    pub host_gpu: HostGpu,
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

/// Three-class host-GPU summary, derived from the Vulkan probe's
/// `VkPhysicalDeviceType` + `shaderFloat16` features. The classes are a
/// gross approximation chosen because they are the smallest set that
/// reproduces the calibration-matrix speedups (1x / 1.3x / 2x / 4x) on
/// the calibration hosts without a maintained PCI table. See ADR 0028.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HostGpu {
    /// No usable GPU (no Vulkan loader, software rasteriser, or a
    /// legacy iGPU that lacks `shaderFloat16`).
    #[default]
    None,
    /// fp16-capable integrated GPU **without** `VK_KHR_cooperative_matrix`.
    /// Empirically ~1.2-2x CPU on the calibration hosts (Kaby Lake-R
    /// UHD 620 at the bottom, Alder Lake Iris Xe at the top); the
    /// `1.3x` multiplier is a conservative split-the-difference value
    /// that prevents the wizard from over-promising on legacy iGPUs
    /// while keeping modern Iris Xe-class hosts in the shortlist via
    /// CPU horsepower.
    Integrated,
    /// fp16-capable integrated GPU **with** `VK_KHR_cooperative_matrix`
    /// (Lunar Lake / Arc / Battlemage / Apple Silicon / RDNA3+ APUs).
    /// Empirically ~3-4x CPU on the calibration hosts; the extension's
    /// presence is what unlocks whisper.cpp's ggml-vulkan tensor matmul
    /// kernel.
    IntegratedTensor,
    /// Discrete GPU. Empirically ~4x CPU on the calibration hosts.
    Discrete,
}

impl HostGpu {
    /// Multiplier applied to the CPU-AVX2 batch-RTF anchor in
    /// [`HardwareSnapshot::affords_model`]. Internal only -- do **not**
    /// surface to users.
    #[must_use]
    pub fn multiplier(self) -> f32 {
        match self {
            Self::None => 1.0,
            Self::Integrated => 1.3,
            Self::IntegratedTensor => 2.0,
            Self::Discrete => 4.0,
        }
    }
}

/// Predicted ability to run local Fono workloads.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LocalTier {
    /// Below the supported floor for local STT — wizard steers to cloud.
    Unsuitable,
    /// Will work but slower; picks `whisper tiny`.
    Minimum,
    /// Comfortable headroom for `whisper small`.
    Comfortable,
    /// `whisper small` + room for an LLM if/when local LLM is wired.
    Recommended,
    /// `whisper large-v3-turbo` for max quality; GPU optional bonus.
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

    /// Default whisper model size for this tier. `base` was dropped
    /// from the registry on 2026-05-19 (ADR 0026); minimum-tier hosts
    /// now fall back to `tiny`, and the high-end tier shoots for
    /// `large-v3-turbo` directly.
    pub fn default_whisper_model(self) -> &'static str {
        match self {
            Self::Unsuitable | Self::Minimum => "tiny",
            Self::Comfortable | Self::Recommended => "small",
            Self::HighEnd => "large-v3-turbo",
        }
    }

    /// Should the wizard default-offer local STT for this tier?
    pub fn local_default(self) -> bool {
        matches!(self, Self::Comfortable | Self::Recommended | Self::HighEnd)
    }
}

/// Floor on effective batch RTF below which a model is considered
/// unsuitable for the host: it cannot keep up with real-time audio in
/// the wizard's data-driven walk. Raised from 1.0 to 2.0 on
/// 2026-05-25 (plan `2026-05-25-wizard-selection-heuristics-refresh-v5`)
/// to match the auto-select walk's gate
/// (`docs/bench/calibration/summary/auto-select.html:279, 368`).
pub const BATCH_REALTIME_MIN: f32 = 2.0;

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
                write!(f, "only {have_gb} GB RAM available; minimum is {need_gb} GB")
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
    /// This is a pure function over the hardware snapshot -- no I/O. The wizard
    /// calls it for each candidate model; `fono-stt`'s `ModelInfo` fields map
    /// directly to the three parameters so the caller avoids a circular dep
    /// (`fono-core` <- `fono-stt`):
    ///
    /// ```ignore
    /// let ok = snap.affords_model(
    ///     model.min_ram_mb,
    ///     model.approx_mb,
    ///     model.realtime_factor_cpu_avx2,
    /// );
    /// ```
    ///
    /// Returns `true` when the host has enough RAM + disk headroom AND
    /// the effective batch RTF (CPU AVX2 anchor scaled by cores, ISA,
    /// and the [`HostGpu`] multiplier) clears [`BATCH_REALTIME_MIN`].
    ///
    /// # Parameters
    /// - `min_ram_mb`: minimum available RAM (MiB) the model needs.
    /// - `approx_mb`: on-disk size (MiB); needs 2x headroom on free disk.
    /// - `realtime_factor_cpu_avx2`: audio-seconds per wall-second on the
    ///   8-core AVX2 reference machine ([`REFERENCE_CORES`]).
    #[must_use]
    pub fn affords_model(
        &self,
        min_ram_mb: u32,
        approx_mb: u32,
        realtime_factor_cpu_avx2: f32,
    ) -> bool {
        let avail_ram_mb = (self.available_ram_bytes / (1024 * 1024)) as u32;
        let free_disk_mb = (self.free_disk_bytes / (1024 * 1024)) as u32;

        // Cannot load without swapping or without disk headroom.
        if avail_ram_mb < min_ram_mb || free_disk_mb < approx_mb * 2 {
            return false;
        }

        // Whisper.cpp is memory-bandwidth-bound past ~6 threads, so
        // doubling cores past the 8-core reference yields ~sqrt, not
        // linear, throughput. Cap at 1.6 to avoid 32-core fantasy.
        let cores = self.physical_cores as f32;
        let core_scale = if cores <= REFERENCE_CORES {
            (cores / REFERENCE_CORES).max(0.25)
        } else {
            (cores / REFERENCE_CORES).sqrt().min(1.6)
        };
        let isa_scale = if self.cpu_features.avx2 || self.cpu_features.neon {
            1.0_f32
        } else {
            0.5 // non-vectorised path is roughly 2x slower
        };
        let effective_rf =
            realtime_factor_cpu_avx2 * core_scale * isa_scale * self.host_gpu.multiplier();
        effective_rf >= BATCH_REALTIME_MIN
    }

    /// One-line, qualitative description of the speech-recognition
    /// acceleration available on this host. Reflects the host's
    /// [`HostGpu`] class; **no numeric multiplier is exposed** (the
    /// 1x / 1.3x / 2x / 4x multipliers are internal scaling factors,
    /// not a promise to the user). See ADR 0028.
    ///
    /// The wizard prints this under the cores/ram lines so users see at
    /// a glance what hardware will be doing the speech work.
    #[must_use]
    pub fn acceleration_summary(&self) -> String {
        if self.os == "macos" && self.arch == "aarch64" {
            return "Apple Silicon (Metal + CoreML) -- Vulkan backend recommended".to_string();
        }
        match self.host_gpu {
            HostGpu::Discrete => "Discrete GPU detected -- Vulkan backend recommended".to_string(),
            HostGpu::IntegratedTensor => {
                "Tensor-capable integrated GPU detected -- Vulkan backend recommended".to_string()
            }
            HostGpu::Integrated => {
                "Integrated GPU detected -- Vulkan backend recommended".to_string()
            }
            HostGpu::None => {
                if self.cpu_features.avx512 {
                    "Legacy / no GPU -- CPU backend (AVX-512)".to_string()
                } else if self.cpu_features.avx2 && self.cpu_features.fma {
                    "Legacy / no GPU -- CPU backend (AVX2 + FMA)".to_string()
                } else if self.cpu_features.avx2 {
                    "Legacy / no GPU -- CPU backend (AVX2)".to_string()
                } else if self.cpu_features.neon {
                    "Legacy / no GPU -- CPU backend (NEON)".to_string()
                } else {
                    "Legacy / no GPU -- CPU backend (no vector extensions; cloud STT recommended)"
                        .to_string()
                }
            }
        }
    }

    /// Return a copy of this snapshot adjusted for the **inference
    /// path** actually available to the running binary.
    ///
    /// The hardware probe records the host's *capability*: a usable
    /// Vulkan GPU upgrades [`host_gpu`](Self::host_gpu) to `Integrated`
    /// or `Discrete`. Whether the currently-running fono binary can
    /// route inference to that GPU is a build-time fact: only the GPU
    /// release variant links against the Vulkan whisper backend, so the
    /// CPU build cannot deliver the speedup encoded in
    /// [`HostGpu::multiplier`].
    ///
    /// Pass `false` whenever inference is CPU-only so the affordability
    /// scorer ([`affords_model`](Self::affords_model)) does not credit
    /// the host with a GPU multiplier it cannot deliver. The probed
    /// `host_gpu` is preserved on the original snapshot for display
    /// purposes (e.g. `fono doctor`'s acceleration summary still tells
    /// the user "your hardware has a Vulkan GPU but you're on the CPU
    /// variant").
    #[must_use]
    pub fn for_inference(&self, gpu_inference_available: bool) -> Self {
        if gpu_inference_available {
            self.clone()
        } else {
            Self { host_gpu: HostGpu::None, ..self.clone() }
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
            return Err(UnsuitableReason::NotEnoughRam { have_gb: ram_gb, need_gb: MIN_RAM_GB });
        }
        if !self.cpu_features.avx2 && !self.cpu_features.neon {
            return Err(UnsuitableReason::NoVectorIsa);
        }
        let disk_gb = u32::try_from(self.free_disk_bytes / GB).unwrap_or(u32::MAX);
        if disk_gb < MIN_DISK_GB {
            return Err(UnsuitableReason::NotEnoughDisk { have_gb: disk_gb, need_gb: MIN_DISK_GB });
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
    let logical =
        std::thread::available_parallelism().map(std::num::NonZero::get).unwrap_or(1) as u32;
    let physical = physical_cores().unwrap_or_else(|| logical.max(1) / 2).max(1);
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
        // Apple Silicon is Integrated (Metal/CoreML); every other
        // platform starts at None and is upgraded by the Vulkan probe
        // (the probe lives in `fono-core::vulkan_probe`, owned by the
        // host binary). See ADR 0028.
        host_gpu: default_host_gpu_for_platform(),
    }
}

/// Static platform default for [`HostGpu`] without consulting a runtime
/// Vulkan probe. Apple Silicon is always [`HostGpu::IntegratedTensor`]
/// (Metal + CoreML on M-series silicon expose the matmul-tensor fast
/// path that whisper.cpp / ggml-metal exploits, same performance tier
/// as desktop iGPUs that expose `VK_KHR_cooperative_matrix`); every
/// other platform starts at [`HostGpu::None`] and is upgraded by the
/// Vulkan probe.
#[must_use]
pub fn default_host_gpu_for_platform() -> HostGpu {
    if std::env::consts::OS == "macos" && std::env::consts::ARCH == "aarch64" {
        HostGpu::IntegratedTensor
    } else {
        HostGpu::None
    }
}

/// Best-effort physical-core detection. Linux: parses `/proc/cpuinfo`.
/// macOS: `hw.physicalcpu` (M-series has no SMT, but the sysctl is
/// authoritative either way). Other OSes: returns `None` and the caller
/// falls back to halving `available_parallelism` (assume SMT siblings).
fn physical_cores() -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        return sysctl_u64("hw.physicalcpu").and_then(|v| u32::try_from(v).ok());
    }
    #[cfg(not(target_os = "macos"))]
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

/// `(total_bytes, available_bytes)` for the running host.
///
/// Linux parses `/proc/meminfo`; macOS asks Mach (`hw.memsize` +
/// `host_statistics64`); other targets return `None` and the caller
/// treats it as `(0, 0)`.
fn read_meminfo() -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    {
        let total = sysctl_u64("hw.memsize")?;
        return Some((total, macos_available_ram().unwrap_or(0)));
    }
    #[cfg(not(target_os = "macos"))]
    {
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
}

#[cfg(not(target_os = "macos"))]
fn parse_kb(s: &str) -> Option<u64> {
    s.trim().trim_end_matches("kB").trim().parse::<u64>().ok()
}

/// Read one integer sysctl by name. Darwin's integer sysctls are a mix
/// of 32-bit (`hw.physicalcpu`) and 64-bit (`hw.memsize`) values, so
/// the buffer accepts either width and widens.
#[cfg(target_os = "macos")]
fn sysctl_u64(name: &str) -> Option<u64> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut buf = [0u8; 8];
    let mut len = buf.len();
    // SAFETY: `cname` is null-terminated; `buf`/`len` describe a valid
    // writable region and sysctlbyname never writes past `len`.
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    match len {
        4 => Some(u64::from(u32::from_ne_bytes(buf[..4].try_into().ok()?))),
        8 => Some(u64::from_ne_bytes(buf)),
        _ => None,
    }
}

/// Available RAM on macOS: free + inactive + purgeable pages, the
/// closest Mach analogue of Linux's `MemAvailable` (memory the kernel
/// can hand out without swapping). Page counts come from
/// `host_statistics64`; the page size from `sysconf(_SC_PAGESIZE)`
/// (16 KiB on Apple Silicon).
#[cfg(target_os = "macos")]
// libc deprecates its mach bindings in favour of the `mach2` crate, but
// pulling a whole new crate for two calls to a stable, frozen kernel
// ABI is not worth the dependency (binary-size rule); the symbols are
// not going anywhere.
#[allow(deprecated)]
fn macos_available_ram() -> Option<u64> {
    let mut stats = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `stats` is a fresh zeroed vm_statistics64 and `count`
    // tells the kernel its size in integer_t units; host_statistics64
    // fills at most that many.
    let rc = unsafe {
        libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            stats.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if rc != libc::KERN_SUCCESS {
        return None;
    }
    // SAFETY: KERN_SUCCESS means the struct was populated.
    let s = unsafe { stats.assume_init() };
    // SAFETY: trivial libc call, no pointers involved.
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    let pages =
        u64::from(s.free_count) + u64::from(s.inactive_count) + u64::from(s.purgeable_count);
    pages.checked_mul(page)
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
        s.free_bytes()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Layout-compatible with glibc/musl `struct statvfs` on 64-bit Linux
/// (all fields `unsigned long` = u64).
#[cfg(all(unix, not(target_os = "macos")))]
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

#[cfg(all(unix, not(target_os = "macos")))]
impl libc_statvfs {
    /// POSIX: the fragment size `f_frsize` is the unit of the block
    /// counts. Checked multiply guards against a nonsense-filled struct
    /// on an untested libc rather than overflowing in release or
    /// panicking in debug.
    fn free_bytes(&self) -> Option<u64> {
        self.f_frsize.checked_mul(self.f_bavail)
    }
}

/// Layout-compatible with Darwin `struct statvfs` (`sys/statvfs.h`):
/// `f_bsize`/`f_frsize` are `unsigned long` (u64), but the block and
/// file counts are `fsblkcnt_t`/`fsfilcnt_t` = `unsigned int` (u32).
/// Reading those through the Linux all-u64 layout produced garbage
/// values whose product overflowed — caught by the live-probe test on
/// the first darwin run.
#[cfg(target_os = "macos")]
#[repr(C)]
#[allow(non_camel_case_types)]
struct libc_statvfs {
    f_bsize: u64,
    f_frsize: u64,
    f_blocks: u32,
    f_bfree: u32,
    f_bavail: u32,
    f_files: u32,
    f_ffree: u32,
    f_favail: u32,
    f_fsid: u64,
    f_flag: u64,
    f_namemax: u64,
}

#[cfg(target_os = "macos")]
impl libc_statvfs {
    /// Same contract as the Linux variant; the u32 counts widen first.
    fn free_bytes(&self) -> Option<u64> {
        self.f_frsize.checked_mul(u64::from(self.f_bavail))
    }
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
            cpu_features: CpuFeatures { avx2, avx512: false, fma: false, neon: false },
            os: "linux".into(),
            arch: "x86_64".into(),
            host_gpu: HostGpu::None,
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
        let s = snap(COMFORTABLE_CORES, COMFORTABLE_RAM_GB, COMFORTABLE_DISK_GB, true);
        assert_eq!(s.tier(), LocalTier::Comfortable);
    }

    #[test]
    fn recommended_at_threshold() {
        let s = snap(RECOMMENDED_CORES, RECOMMENDED_RAM_GB, RECOMMENDED_DISK_GB, true);
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
        assert_eq!(LocalTier::Unsuitable.default_whisper_model(), "tiny");
        assert_eq!(LocalTier::Minimum.default_whisper_model(), "tiny");
        assert_eq!(LocalTier::Comfortable.default_whisper_model(), "small");
        assert_eq!(LocalTier::Recommended.default_whisper_model(), "small");
        assert_eq!(LocalTier::HighEnd.default_whisper_model(), "large-v3-turbo");
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

    fn affords(s: &HardwareSnapshot, (min_ram, approx, rf): (u32, u32, f32)) -> bool {
        s.affords_model(min_ram, approx, rf)
    }

    #[test]
    fn affords_tiny_on_8_core_avx2() {
        // tiny rf=20 × 1.0 × 1.0 × 1.0 = 20 ≥ 2.0 → affords
        let s = snap(8, 16, 100, true);
        assert!(affords(&s, TINY_EN));
    }

    #[test]
    fn affords_small_on_8_core_cpu_only() {
        // small rf=4.0 × 1.0 × 1.0 × 1.0 = 4.0 ≥ 2.0 → affords
        let s = snap(8, 16, 100, true);
        assert!(affords(&s, SMALL_EN));
    }

    #[test]
    fn affords_turbo_on_high_core_cpu_only() {
        // turbo rf=2.5 × sqrt(12/8) ≈ 3.06 ≥ 2.0 → affords
        let s = snap(12, 32, 200, true);
        assert!(affords(&s, TURBO));
    }

    #[test]
    fn affords_turbo_fails_on_typical_laptop_cpu() {
        // Empirical turbo rf=0.6 on 8-core laptop:
        // 0.6 × 1.0 × 1.0 × 1.0 = 0.6 < 2.0 → does not afford.
        let s = snap(8, 16, 200, true);
        let turbo_empirical = (4_000_u32, 1_620_u32, 0.6_f32);
        assert!(!affords(&s, turbo_empirical));
    }

    #[test]
    fn affords_turbo_with_discrete_gpu() {
        // turbo rf=2.5 × 1.0 × 1.0 × 4.0 (Discrete) = 10.0 ≥ 2.0 → affords
        let mut s = snap(8, 16, 200, true);
        s.host_gpu = HostGpu::Discrete;
        assert!(affords(&s, TURBO));
    }

    #[test]
    fn affords_turbo_fails_on_low_rf_even_with_integrated() {
        // Empirical turbo rf=0.6 × sqrt scaling impossible at 8 cores
        // × 1.3 (Integrated) = 0.78 < 2.0 → does not afford.
        let mut s = snap(8, 16, 200, true);
        s.host_gpu = HostGpu::Integrated;
        let turbo_empirical = (4_000_u32, 1_620_u32, 0.6_f32);
        assert!(!affords(&s, turbo_empirical));
    }

    #[test]
    fn affords_turbo_with_integrated_tensor_gpu() {
        // turbo rf=2.5 × 1.0 × 1.0 × 2.0 (IntegratedTensor) = 5.0 ≥ 2.0 → affords.
        // Empirical justification: Lunar Lake / Xe2 hosts measure
        // ~3-4× Vulkan-vs-CPU on q8_0 turbo, matching the 2.0× anchor.
        let mut s = snap(8, 16, 200, true);
        s.host_gpu = HostGpu::IntegratedTensor;
        assert!(affords(&s, TURBO));
    }

    #[test]
    fn affords_small_on_apple_silicon() {
        // Apple Silicon defaults to HostGpu::IntegratedTensor (Metal /
        // CoreML expose the matmul-tensor fast path; same tier as
        // VK_KHR_cooperative_matrix-capable iGPUs).
        // small rf=4.0 × 1.0 × 1.0 × 2.0 = 8.0 ≥ 2.0 → affords
        let s = HardwareSnapshot {
            os: "macos".into(),
            arch: "aarch64".into(),
            cpu_features: CpuFeatures { neon: true, ..Default::default() },
            host_gpu: HostGpu::IntegratedTensor,
            ..snap(8, 16, 100, false)
        };
        assert!(affords(&s, SMALL_EN));
        assert!(affords(&s, TURBO));
    }

    #[test]
    fn affords_fails_when_not_enough_ram() {
        let s = HardwareSnapshot {
            physical_cores: 8,
            logical_cores: 16,
            total_ram_bytes: GB * 8,
            available_ram_bytes: 512 * 1024 * 1024,
            free_disk_bytes: GB * 100,
            cpu_features: CpuFeatures { avx2: true, ..Default::default() },
            os: "linux".into(),
            arch: "x86_64".into(),
            host_gpu: HostGpu::None,
        };
        assert!(!affords(&s, SMALL_EN));
    }

    #[test]
    fn affords_fails_when_not_enough_disk() {
        let s = HardwareSnapshot {
            physical_cores: 8,
            logical_cores: 16,
            total_ram_bytes: GB * 16,
            available_ram_bytes: GB * 8,
            free_disk_bytes: 800 * 1024 * 1024,
            cpu_features: CpuFeatures { avx2: true, ..Default::default() },
            os: "linux".into(),
            arch: "x86_64".into(),
            host_gpu: HostGpu::None,
        };
        assert!(!affords(&s, SMALL_EN));
    }

    #[test]
    fn host_gpu_multipliers_match_calibration_classes() {
        // 1.0× / 1.3× / 2.0× / 4.0× — see ADR 0028 (amended 2026-05-25).
        assert!((HostGpu::None.multiplier() - 1.0).abs() < f32::EPSILON);
        assert!((HostGpu::Integrated.multiplier() - 1.3).abs() < f32::EPSILON);
        assert!((HostGpu::IntegratedTensor.multiplier() - 2.0).abs() < f32::EPSILON);
        assert!((HostGpu::Discrete.multiplier() - 4.0).abs() < f32::EPSILON);
    }

    #[test]
    fn for_inference_zeros_host_gpu_when_unavailable() {
        // CPU variant: regardless of probed iGPU, the inference path
        // gets host_gpu = None so affords_model never credits a GPU
        // multiplier the binary cannot deliver.
        let mut s = snap(8, 16, 100, true);
        s.host_gpu = HostGpu::Integrated;
        let inf = s.for_inference(false);
        assert_eq!(inf.host_gpu, HostGpu::None);
        // Display snapshot is untouched.
        assert_eq!(s.host_gpu, HostGpu::Integrated);
        // Under the CPU-only view (host_gpu = None) the legacy/no-AVX2
        // turbo anchor empirical rf=0.6 × 1.0 × 1.0 × 1.0 = 0.6 < 2.0,
        // so it drops.
        let turbo_empirical = (4_000_u32, 1_620_u32, 0.6_f32);
        assert!(!affords(&inf, turbo_empirical));
        // GPU variant: snapshot is unchanged.
        let inf_gpu = s.for_inference(true);
        assert_eq!(inf_gpu.host_gpu, HostGpu::Integrated);
    }

    #[test]
    fn acceleration_summary_apple_silicon_mentions_metal() {
        let mac_arm = HardwareSnapshot {
            os: "macos".into(),
            arch: "aarch64".into(),
            cpu_features: CpuFeatures { neon: true, ..Default::default() },
            host_gpu: HostGpu::Integrated,
            ..snap(8, 16, 100, false)
        };
        let s = mac_arm.acceleration_summary();
        assert!(s.contains("Apple Silicon"));
        assert!(s.contains("Metal"));
        // No numeric multiplier exposed.
        assert!(!s.contains("2x") && !s.contains("4x"));
    }

    #[test]
    fn acceleration_summary_discrete_gpu_says_discrete() {
        let mut s = snap(8, 16, 100, true);
        s.host_gpu = HostGpu::Discrete;
        let line = s.acceleration_summary();
        assert!(line.contains("Discrete"));
        assert!(line.contains("Vulkan"));
        assert!(!line.contains("4x"));
    }

    #[test]
    fn acceleration_summary_integrated_gpu_says_integrated() {
        let mut s = snap(8, 16, 100, true);
        s.host_gpu = HostGpu::Integrated;
        let line = s.acceleration_summary();
        assert!(line.contains("Integrated"));
        assert!(line.contains("Vulkan"));
        assert!(!line.contains("1.3x"));
    }

    #[test]
    fn acceleration_summary_integrated_tensor_says_tensor() {
        let mut s = snap(8, 16, 100, true);
        s.host_gpu = HostGpu::IntegratedTensor;
        let line = s.acceleration_summary();
        assert!(line.to_lowercase().contains("tensor"));
        assert!(line.contains("Vulkan"));
        assert!(!line.contains("2x"));
    }

    #[test]
    fn acceleration_summary_no_gpu_avx2_says_cpu_backend() {
        let s = snap(8, 16, 100, true).acceleration_summary();
        assert!(s.contains("CPU backend"));
        assert!(s.contains("AVX2"));
    }

    #[test]
    fn acceleration_summary_no_vector_isa_warns() {
        let s = snap(8, 16, 100, false).acceleration_summary();
        assert!(s.contains("no vector"));
        assert!(s.contains("cloud"));
    }
}
