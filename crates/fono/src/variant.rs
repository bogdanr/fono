// SPDX-License-Identifier: GPL-3.0-only
//! Build-time identifier for which release variant this binary is.
//!
//! Per `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`, fono ships in
//! two variants: a compact CPU-only build (~18 MB) and a Vulkan-enabled
//! build (~60 MB). The two share source; the only difference is whether
//! `accel-vulkan` was on at build time.
//!
//! Used by `fono doctor`, the daemon startup log, the (future) wizard
//! upgrade prompt, and the (future) self-update flow to pick the right
//! release asset.

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Variant {
    /// Compact CPU-only ship. NEEDED set is the universal glibc + libgcc_s
    /// ABI; size budget 20 MiB.
    Cpu,
    /// Vulkan-enabled ship. Adds `libvulkan.so.1` to NEEDED; size ~60 MB.
    Gpu,
}

impl Variant {
    /// Short label used in logs and asset names. `"cpu"` or `"gpu"`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        }
    }

    /// Release-asset basename prefix: `fono` for the CPU build,
    /// `fono-gpu` for the GPU build. Combined with the version + arch
    /// to form the full asset name (`fono-vX.Y.Z-x86_64`,
    /// `fono-gpu-vX.Y.Z-x86_64`).
    #[must_use]
    pub const fn release_asset_prefix(self) -> &'static str {
        match self {
            Self::Cpu => "fono",
            Self::Gpu => "fono-gpu",
        }
    }

    /// Human-readable description for tray menus and wizard prompts.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Cpu => "CPU-only (compact, runs everywhere)",
            Self::Gpu => "GPU-accelerated via Vulkan",
        }
    }
}

/// The variant of *this* binary, determined at compile time from the
/// `accel-vulkan` cargo feature. `Variant::Gpu` when the feature is on,
/// `Variant::Cpu` otherwise.
pub const VARIANT: Variant = {
    #[cfg(feature = "accel-vulkan")]
    {
        Variant::Gpu
    }
    #[cfg(not(feature = "accel-vulkan"))]
    {
        Variant::Cpu
    }
};
