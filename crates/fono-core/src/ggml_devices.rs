// SPDX-License-Identifier: GPL-3.0-only
//! Inventory of the compute devices ggml has registered.
//!
//! This asks ggml itself which devices exist and how much memory each has
//! free, rather than asking a graphics API directly. The distinction matters:
//! these are the devices llama.cpp will actually choose between when it loads
//! a model, so there is no gap between what was measured and what got picked.
//! It also covers Vulkan, CUDA, ROCm, Metal and SYCL through one interface
//! instead of one probe per backend.
//!
//! `crates/fono-core/src/vulkan_probe.rs` answers a different question — is a
//! usable Vulkan loader and physical device present on this host, for the
//! `HostGpu` classification in ADR 0028 — and can run in builds that do not
//! link llama.cpp at all. This module needs the ggml symbols, so it exists
//! only under `llama-local`.
//!
//! The four `ggml_backend_dev_*` entry points are declared here by hand. They
//! are already compiled into the static ggml archive the binary links, but
//! `llama-cpp-sys-2`'s bindgen wrapper header does not include
//! `ggml-backend.h`, so bindgen never generated them. Declaring the C ABI
//! directly reaches them without patching a vendored crate and without adding
//! a dependency — the same approach `hwcheck`'s hand-written `statvfs` binding
//! takes. Only the scalar accessors are declared: `ggml_backend_dev_get_props`
//! returns more in one call, but binding it would commit this file to the
//! exact layout of a C struct that upstream is free to extend.

use std::ffi::{c_char, c_void, CStr};

/// Opaque `ggml_backend_dev_t`.
type GgmlBackendDev = *mut c_void;

extern "C" {
    fn ggml_backend_dev_count() -> usize;
    fn ggml_backend_dev_get(index: usize) -> GgmlBackendDev;
    fn ggml_backend_dev_name(device: GgmlBackendDev) -> *const c_char;
    fn ggml_backend_dev_description(device: GgmlBackendDev) -> *const c_char;
    fn ggml_backend_dev_memory(device: GgmlBackendDev, free: *mut usize, total: *mut usize);
    fn ggml_backend_dev_type(device: GgmlBackendDev) -> u32;
}

/// What kind of device this is, mirroring `enum ggml_backend_dev_type` in
/// `ggml/include/ggml-backend.h`. The discriminants are part of ggml's public
/// C API; an unrecognised one is preserved rather than guessed at, so a future
/// device kind cannot be silently mistaken for one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GgmlDeviceKind {
    /// Runs on the CPU using system memory.
    Cpu,
    /// Discrete accelerator with its own dedicated memory.
    Gpu,
    /// Integrated GPU sharing system memory with the CPU.
    IGpu,
    /// Helper device used alongside the CPU backend (BLAS, AMX).
    Accel,
    /// Wrapper over several devices for tensor parallelism.
    Meta,
    /// A device kind this build of Fono predates.
    Unknown(u32),
}

impl GgmlDeviceKind {
    fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Cpu,
            1 => Self::Gpu,
            2 => Self::IGpu,
            3 => Self::Accel,
            4 => Self::Meta,
            other => Self::Unknown(other),
        }
    }

    /// Whether this device draws on the same memory the rest of the machine
    /// uses. Deciding factor for offload sizing: on dedicated memory an
    /// over-commit is a recoverable allocation error, while on shared memory
    /// it is an out-of-memory kill nothing can catch.
    #[must_use]
    pub fn shares_system_memory(self) -> bool {
        matches!(self, Self::Cpu | Self::IGpu)
    }
}

/// One device as ggml reports it.
#[derive(Debug, Clone)]
pub struct GgmlDevice {
    /// Short backend-assigned name, e.g. `Vulkan0`.
    pub name: String,
    /// Human-readable device description, e.g. the marketing name.
    pub description: String,
    pub kind: GgmlDeviceKind,
    /// Free memory as the driver reports it. Advisory: drivers disagree about
    /// what "free" means and some do not implement the query at all — see
    /// [`GgmlDevice::free_is_whole_heap`].
    pub free_bytes: u64,
    pub total_bytes: u64,
}

impl GgmlDevice {
    /// Whether the free figure is the device's entire memory rather than a
    /// real measurement of what is unused.
    ///
    /// The Vulkan backend accumulates `total` and `free` over the same set of
    /// heaps, differing only in the term added to `free`: the driver's
    /// remaining budget when `VK_EXT_memory_budget` is available, and the
    /// whole heap size when it is not. So the two sums are equal exactly when
    /// the extension is missing and the figure is meaningless — an equality
    /// that follows from the code, not a heuristic threshold.
    ///
    /// A genuinely idle device with a working budget query can report the same
    /// equality. That direction is harmless: it only makes the caller treat a
    /// trustworthy figure as untrustworthy, which costs a little unused
    /// capacity rather than an over-commit.
    #[must_use]
    pub fn free_is_whole_heap(&self) -> bool {
        self.total_bytes > 0 && self.free_bytes >= self.total_bytes
    }

    /// Whether a memory figure from this device can be believed on its own, or
    /// must additionally be bounded by free system RAM.
    #[must_use]
    pub fn free_is_trustworthy(&self) -> bool {
        !self.kind.shares_system_memory() && !self.free_is_whole_heap()
    }
}

/// Every device ggml has registered, in ggml's own order.
///
/// Returns empty in a build without any accelerated backend compiled in, and
/// on any host where the backends found no usable device — both are ordinary
/// answers, not failures.
#[must_use]
pub fn devices() -> Vec<GgmlDevice> {
    // The backend singleton must exist before the device registry is read:
    // registration happens during backend init, and querying first would
    // report an empty machine.
    let _ = crate::llama_backend::backend();

    // SAFETY: `ggml_backend_dev_count` takes no arguments and only reads a
    // registry ggml populated during init.
    let count = unsafe { ggml_backend_dev_count() };
    (0..count).filter_map(device_at).collect()
}

fn device_at(index: usize) -> Option<GgmlDevice> {
    // SAFETY: `index` is below the count ggml just reported, which is the
    // documented precondition.
    let dev = unsafe { ggml_backend_dev_get(index) };
    if dev.is_null() {
        return None;
    }
    let mut free: usize = 0;
    let mut total: usize = 0;
    // SAFETY: `dev` is a non-null handle from the registry; both out-pointers
    // address live locals of the correct type.
    unsafe { ggml_backend_dev_memory(dev, &raw mut free, &raw mut total) };
    // SAFETY: same handle. The returned pointers are owned by ggml and stay
    // valid for the life of the registry, so they are copied out immediately
    // rather than borrowed.
    let (name, description, raw_kind) = unsafe {
        (
            cstr_to_string(ggml_backend_dev_name(dev)),
            cstr_to_string(ggml_backend_dev_description(dev)),
            ggml_backend_dev_type(dev),
        )
    };
    Some(GgmlDevice {
        name,
        description,
        kind: GgmlDeviceKind::from_raw(raw_kind),
        free_bytes: free as u64,
        total_bytes: total as u64,
    })
}

/// Copy a ggml-owned C string, tolerating both null and invalid UTF-8 rather
/// than failing an inventory over a cosmetic field.
unsafe fn cstr_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: caller guarantees a null-terminated string owned by ggml.
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_kind_maps_the_documented_discriminants() {
        assert_eq!(GgmlDeviceKind::from_raw(0), GgmlDeviceKind::Cpu);
        assert_eq!(GgmlDeviceKind::from_raw(1), GgmlDeviceKind::Gpu);
        assert_eq!(GgmlDeviceKind::from_raw(2), GgmlDeviceKind::IGpu);
        assert_eq!(GgmlDeviceKind::from_raw(3), GgmlDeviceKind::Accel);
        assert_eq!(GgmlDeviceKind::from_raw(4), GgmlDeviceKind::Meta);
        assert_eq!(GgmlDeviceKind::from_raw(99), GgmlDeviceKind::Unknown(99));
    }

    #[test]
    fn only_dedicated_memory_stands_alone() {
        assert!(GgmlDeviceKind::Gpu.shares_system_memory().eq(&false));
        assert!(GgmlDeviceKind::IGpu.shares_system_memory());
        assert!(GgmlDeviceKind::Cpu.shares_system_memory());
        // An unrecognised device is not assumed to have memory of its own.
        assert!(!GgmlDeviceKind::Unknown(7).shares_system_memory());
    }

    fn device(kind: GgmlDeviceKind, free: u64, total: u64) -> GgmlDevice {
        GgmlDevice {
            name: "test".into(),
            description: "test".into(),
            kind,
            free_bytes: free,
            total_bytes: total,
        }
    }

    #[test]
    fn free_equal_to_total_means_the_driver_did_not_answer() {
        let d = device(GgmlDeviceKind::Gpu, 8 << 30, 8 << 30);
        assert!(d.free_is_whole_heap());
        assert!(!d.free_is_trustworthy());
    }

    #[test]
    fn a_real_budget_on_a_dedicated_card_is_trustworthy() {
        let d = device(GgmlDeviceKind::Gpu, 6 << 30, 8 << 30);
        assert!(!d.free_is_whole_heap());
        assert!(d.free_is_trustworthy());
    }

    #[test]
    fn shared_memory_is_never_trustworthy_alone_even_with_a_real_budget() {
        let d = device(GgmlDeviceKind::IGpu, 6 << 30, 8 << 30);
        assert!(!d.free_is_whole_heap());
        assert!(!d.free_is_trustworthy());
    }

    #[test]
    fn a_device_reporting_nothing_is_not_called_a_whole_heap() {
        let d = device(GgmlDeviceKind::Gpu, 0, 0);
        assert!(!d.free_is_whole_heap());
    }
}
