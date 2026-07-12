// SPDX-License-Identifier: GPL-3.0-only
//! Soft-load shim for the Vulkan loader.
//!
//! ggml-vulkan dispatches almost all Vulkan calls through a runtime
//! dispatcher (`VULKAN_HPP_DISPATCH_LOADER_DYNAMIC`), but it still
//! references a small set of Vulkan entry points as *bare, link-time*
//! symbols. Concretely, on the pinned whisper.cpp / llama.cpp ggml the
//! binary carries exactly three undefined `vk*` symbols:
//!
//! - `vkGetInstanceProcAddr` — the dispatcher bootstrap
//!   (`ggml-vulkan.cpp:5401`).
//! - `vkGetPhysicalDeviceFeatures2` — direct calls
//!   (`ggml-vulkan.cpp:4862,5348,15171`).
//! - `vkCmdCopyBuffer` — direct calls (`ggml-vulkan.cpp:6313,6384,6535`).
//!
//! whisper-rs-sys' `build.rs` emits `cargo:rustc-link-lib=vulkan`
//! (and llama-cpp-sys' fork does the same), so those symbols are
//! satisfied by hard-linking `libvulkan.so.1`, which lands in the
//! binary's `DT_NEEDED` set. That makes the GPU build refuse to even
//! *start* on a host without the Vulkan loader.
//!
//! This module defines those three symbols itself, as lazy forwarders
//! that `dlopen("libvulkan.so.1")` at first use. Combined with the
//! linker's `--as-needed`, that lets the loader drop out of `NEEDED`:
//! nothing references `libvulkan` anymore, because our own definitions
//! satisfy ggml's references.
//!
//! ## Why it lives in `fono-core`
//!
//! Both Vulkan ggml consumers — `whisper-rs/vulkan` (via `fono-stt`) and
//! `llama-cpp-2/vulkan` (via `fono-polish` / `fono-assistant`) — link
//! the *same* ggml and reference the *same* three bare symbols. The shim
//! must therefore be compiled whenever *either* backend is active, and
//! it must be defined exactly *once* (two `#[no_mangle]` definitions in
//! the same binary is a duplicate-symbol link error). `fono-core` is the
//! shared low-level crate both depend on, so it is the single correct
//! home: `fono-stt/accel-vulkan` and `fono-polish/accel-vulkan` each
//! enable `fono-core/accel-vulkan`, and cargo feature unification then
//! compiles this module once. Placing it in one backend crate instead
//! would silently drop the shim from any build that links the *other*
//! backend's Vulkan without the first (e.g. a polish-only GPU build),
//! reintroducing both the hard link and the loader-absent crash.
//!
//! ## Loader-absent behaviour (the subtle part)
//!
//! When the loader is genuinely absent we must *not* simply return null
//! from `vkGetInstanceProcAddr`. ggml bootstraps its dynamic dispatcher
//! with our `vkGetInstanceProcAddr` and then, in `ggml_vk_instance_init`
//! (`ggml-vulkan.cpp:5403`), immediately calls `vk::enumerateInstanceVersion()`
//! *through that dispatcher*. If the bootstrap handed back a null `PFN`,
//! that call dereferences a null function pointer and the process
//! **segfaults** — before ggml's own guard can react.
//!
//! ggml *does* guard init: `ggml_backend_vk_reg` (`ggml-vulkan.cpp:15091`)
//! wraps `ggml_vk_instance_init` in a `try { … } catch (vk::SystemError)`
//! that returns a null registration (⇒ zero Vulkan devices ⇒ CPU
//! backend). The catch only fires for a thrown C++ exception, never for
//! a hardware segfault. So the trick is to make the absent-loader path
//! *throw* instead of *fault*: when `dlopen` fails, our
//! `vkGetInstanceProcAddr` returns a non-null pointer to an **error
//! stub** that reports `VK_ERROR_INITIALIZATION_FAILED`. Vulkan-Hpp's
//! `resultCheck` turns that into a `vk::SystemError`, ggml catches it,
//! registers zero Vulkan devices, and inference falls back to CPU —
//! exactly the behaviour we want for a single "runs everywhere" build.
//!
//! The other two forwarders are only ever reached *after* a Vulkan
//! device has been created (i.e. the loader was present), so they will
//! always have a live target when called; they no-op defensively if
//! not.
//!
//! Gated to `target_os = "linux"` for now; the Windows sibling
//! (`LoadLibraryW("vulkan-1.dll")`) lands with the single Vulkan
//! Windows build (see
//! `plans/2026-07-12-vulkan-soft-load-single-build-v1.md`, Phase 2).

use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

const RTLD_NOW: c_int = 0x2;
const RTLD_LOCAL: c_int = 0;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `VkResult` for "the implementation could not be initialised" (`-3`).
/// Returned by [`vk_stub_incompatible`] so Vulkan-Hpp throws rather than
/// the process faulting.
const VK_ERROR_INITIALIZATION_FAILED: c_int = -3;

/// Error stub handed back by [`vkGetInstanceProcAddr`] for *every*
/// instance-level entry point when the Vulkan loader is absent.
///
/// All the global-scope commands ggml calls during
/// `ggml_vk_instance_init` (`vkEnumerateInstanceVersion`,
/// `vkEnumerateInstanceExtensionProperties`, `vkCreateInstance`, …)
/// return a `VkResult`. Reporting `VK_ERROR_INITIALIZATION_FAILED` makes
/// Vulkan-Hpp's `resultCheck` throw `vk::SystemError`, which ggml's
/// `ggml_backend_vk_reg` catches and turns into a zero-device
/// registration ⇒ CPU fallback.
///
/// It is declared with no parameters on purpose: on the C ABI the
/// caller passes arguments in registers/stack that a zero-arg callee
/// simply ignores, and every caller here only consumes the `VkResult`
/// return value. The first such call (`vkEnumerateInstanceVersion` at
/// `ggml-vulkan.cpp:5403`) throws immediately, so no later entry point
/// is ever reached.
extern "C" fn vk_stub_incompatible() -> c_int {
    VK_ERROR_INITIALIZATION_FAILED
}

/// Real entry points resolved from the system Vulkan loader, or all
/// null when the loader could not be opened.
#[derive(Clone, Copy)]
struct Loader {
    get_instance_proc_addr: *mut c_void,
    cmd_copy_buffer: *mut c_void,
    get_physical_device_features2: *mut c_void,
}

// SAFETY: the fields are opaque function pointers into the (process-
// global, never-unloaded) Vulkan loader; sharing them across threads is
// sound.
unsafe impl Send for Loader {}
unsafe impl Sync for Loader {}

fn loader() -> &'static Loader {
    static LOADER: OnceLock<Loader> = OnceLock::new();
    LOADER.get_or_init(|| unsafe {
        let handle = dlopen(c"libvulkan.so.1".as_ptr(), RTLD_NOW | RTLD_LOCAL);
        if handle.is_null() {
            return Loader {
                get_instance_proc_addr: std::ptr::null_mut(),
                cmd_copy_buffer: std::ptr::null_mut(),
                get_physical_device_features2: std::ptr::null_mut(),
            };
        }
        Loader {
            get_instance_proc_addr: dlsym(handle, c"vkGetInstanceProcAddr".as_ptr()),
            cmd_copy_buffer: dlsym(handle, c"vkCmdCopyBuffer".as_ptr()),
            get_physical_device_features2: dlsym(handle, c"vkGetPhysicalDeviceFeatures2".as_ptr()),
        }
    })
}

/// `PFN_vkVoidFunction vkGetInstanceProcAddr(VkInstance, const char*)`.
///
/// When the loader is present this delegates to the real
/// `vkGetInstanceProcAddr`. When it is absent, it returns a non-null
/// pointer to [`vk_stub_incompatible`] for *any* requested entry point,
/// so ggml's dispatcher gets a callable that reports
/// `VK_ERROR_INITIALIZATION_FAILED` (Vulkan-Hpp then throws, ggml
/// catches, and inference falls back to CPU). Returning null here would
/// instead crash ggml when it calls `vk::enumerateInstanceVersion`
/// through a null pointer.
///
/// # Safety
/// Called by ggml with a valid (or null) `VkInstance` and a
/// NUL-terminated `p_name`; matches the Vulkan C ABI.
#[no_mangle]
pub unsafe extern "C" fn vkGetInstanceProcAddr(
    instance: *mut c_void,
    p_name: *const c_char,
) -> *const c_void {
    let real = loader().get_instance_proc_addr;
    if real.is_null() {
        // Loader absent: hand back an error stub (never null) so the
        // caller throws instead of faulting. See the module docs.
        return vk_stub_incompatible as *const c_void;
    }
    // SAFETY: `real` is the loader's genuine `vkGetInstanceProcAddr`
    // trampoline; the transmuted signature matches the Vulkan C ABI.
    unsafe {
        let f: unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_void =
            std::mem::transmute(real);
        f(instance, p_name)
    }
}

/// `void vkCmdCopyBuffer(VkCommandBuffer, VkBuffer, VkBuffer, uint32_t,
/// const VkBufferCopy*)`.
///
/// # Safety
/// Only reached after a Vulkan device exists (loader present); matches
/// the Vulkan C ABI. `VkBuffer` is a non-dispatchable `uint64_t` handle.
#[no_mangle]
pub unsafe extern "C" fn vkCmdCopyBuffer(
    command_buffer: *mut c_void,
    src_buffer: u64,
    dst_buffer: u64,
    region_count: u32,
    p_regions: *const c_void,
) {
    let real = loader().cmd_copy_buffer;
    if real.is_null() {
        return;
    }
    // SAFETY: `real` is the loader's genuine `vkCmdCopyBuffer`
    // trampoline; the transmuted signature matches the Vulkan C ABI.
    unsafe {
        let f: unsafe extern "C" fn(*mut c_void, u64, u64, u32, *const c_void) =
            std::mem::transmute(real);
        f(command_buffer, src_buffer, dst_buffer, region_count, p_regions);
    }
}

/// `void vkGetPhysicalDeviceFeatures2(VkPhysicalDevice,
/// VkPhysicalDeviceFeatures2*)`.
///
/// # Safety
/// Only reached after physical-device enumeration succeeded (loader
/// present); matches the Vulkan C ABI.
#[no_mangle]
pub unsafe extern "C" fn vkGetPhysicalDeviceFeatures2(
    physical_device: *mut c_void,
    p_features: *mut c_void,
) {
    let real = loader().get_physical_device_features2;
    if real.is_null() {
        return;
    }
    // SAFETY: `real` is the loader's genuine
    // `vkGetPhysicalDeviceFeatures2` trampoline; the transmuted
    // signature matches the Vulkan C ABI.
    unsafe {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) = std::mem::transmute(real);
        f(physical_device, p_features);
    }
}
