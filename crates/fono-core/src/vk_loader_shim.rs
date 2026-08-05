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
//! ## No-usable-device behaviour (the subtle part)
//!
//! Returning null from `vkGetInstanceProcAddr` is not an option. ggml
//! bootstraps its dynamic dispatcher with our `vkGetInstanceProcAddr`
//! and then immediately calls `vk::enumerateInstanceVersion()` through
//! that dispatcher; a null `PFN` there is an indirect call through null,
//! i.e. a segfault before ggml's own guard can react.
//!
//! Making that path *fail* is not an option either, and this is the part
//! that is easy to get wrong. ggml guards init with
//! `try { ggml_vk_instance_init(); } catch (vk::SystemError)`, so a
//! failing entry point looks safe on paper: Vulkan-Hpp's `resultCheck`
//! throws, ggml catches, zero Vulkan devices get registered, inference
//! falls back to the CPU. It works on Linux. On Windows the process dies
//! with `STATUS_HEAP_CORRUPTION` instead, inside the CRT's own
//! `__std_exception_destroy` as the catch block tears the exception
//! object down — the free of the exception's message is rejected by the
//! heap. Any ggml-vulkan exception is enough to trigger it, so the
//! machine that most needs the CPU fallback (a Windows box with no
//! Vulkan driver) is the one that crashes.
//!
//! So the shim does not fail — it reports, truthfully, that there are no
//! Vulkan devices, which ggml handles with a plain early `return` and no
//! exception at all. Two pieces:
//!
//! 1. **Pre-flight.** On first use, once the loader is open, the shim
//!    asks it — through its own entry points — to create a throwaway
//!    instance and count physical devices. A live loader with at least
//!    one device means everything is forwarded to the real Vulkan
//!    unchanged. Anything else (loader absent, instance creation
//!    refused, zero devices) selects the emulation.
//! 2. **Emulation.** A handful of stubs that succeed: API version 1.3,
//!    no layers, no instance extensions, an opaque instance handle, and
//!    **zero physical devices**. ggml walks its normal init path, finds
//!    the device list empty, logs `ggml_vulkan: No devices found.` and
//!    returns. Zero Vulkan devices ⇒ the CPU backend runs the work.
//!
//! Entry points the emulation does not implement return null, which is
//! safe because ggml stops asking once it sees no devices.
//!
//! The other two forwarders are only ever reached *after* a Vulkan
//! device has been created (i.e. the loader was present and had one), so
//! they will always have a live target when called; they no-op
//! defensively if not.
//!
//! ## Cross-platform loader
//!
//! The only per-OS difference is *how* the loader is opened: `dlopen`
//! (`libvulkan.so.1`) on Linux, `LoadLibraryA` (`vulkan-1.dll`) on
//! Windows. Everything else — the three `#[no_mangle]` forwarders, the
//! error-stub fallback, the lazy `OnceLock` — is shared. The Windows
//! `vulkan-1.dll` is installed by the GPU vendor driver (not the OS), so
//! the same "loader may be absent" reasoning applies there: a single
//! Vulkan-accelerated `fono.exe` uses the GPU when the driver's loader
//! is present and falls back to CPU when it isn't.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::OnceLock;

/// Per-platform primitives for opening the Vulkan loader and resolving a
/// symbol from it. The bodies wrap the raw C / Win32 APIs directly (no
/// crate dependency, matching the zero-dep spirit of `fono-core`).
#[cfg(target_os = "linux")]
mod sys {
    use super::{c_void, CStr};

    const RTLD_NOW: core::ffi::c_int = 0x2;
    const RTLD_LOCAL: core::ffi::c_int = 0;

    extern "C" {
        fn dlopen(filename: *const super::c_char, flag: core::ffi::c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const super::c_char) -> *mut c_void;
    }

    /// File name of the Vulkan loader shipped by the vendor driver.
    pub const LOADER_NAME: &CStr = c"libvulkan.so.1";

    /// # Safety
    /// `name` is a valid NUL-terminated library name.
    pub unsafe fn open(name: &CStr) -> *mut c_void {
        unsafe { dlopen(name.as_ptr(), RTLD_NOW | RTLD_LOCAL) }
    }

    /// # Safety
    /// `handle` is a live loader handle from [`open`]; `name` is a valid
    /// NUL-terminated symbol name.
    pub unsafe fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
        unsafe { dlsym(handle, name.as_ptr()) }
    }
}

#[cfg(target_os = "windows")]
mod sys {
    use super::{c_void, CStr};

    // `LoadLibraryA` / `GetProcAddress` from kernel32. Declared directly
    // rather than pulling `windows-sys` — these two entry points are all
    // the shim needs, and the binary already links kernel32.
    extern "system" {
        fn LoadLibraryA(name: *const super::c_char) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const super::c_char) -> *mut c_void;
    }

    /// File name of the Vulkan loader shipped by the vendor driver.
    /// Resolved through the standard DLL search order (System32, where
    /// the driver installs it).
    pub const LOADER_NAME: &CStr = c"vulkan-1.dll";

    /// # Safety
    /// `name` is a valid NUL-terminated library name.
    pub unsafe fn open(name: &CStr) -> *mut c_void {
        unsafe { LoadLibraryA(name.as_ptr()) }
    }

    /// # Safety
    /// `handle` is a live module handle from [`open`]; `name` is a valid
    /// NUL-terminated symbol name.
    pub unsafe fn symbol(handle: *mut c_void, name: &CStr) -> *mut c_void {
        unsafe { GetProcAddress(handle, name.as_ptr()) }
    }
}

/// `VkResult` for success (`0`).
const VK_SUCCESS: c_int = 0;

/// `VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO`.
const VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO: u32 = 1;

/// `VK_API_VERSION_1_3`, reported by the emulation. ggml refuses to
/// continue below 1.2, and it must continue far enough to see the empty
/// device list.
const VK_API_VERSION_1_3: u32 = (1 << 22) | (3 << 12);

/// `VkInstanceCreateInfo` — the one Vulkan struct the shim builds itself,
/// for the pre-flight instance. Field order and padding match the C
/// definition.
#[repr(C)]
struct VkInstanceCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    p_application_info: *const c_void,
    enabled_layer_count: u32,
    pp_enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    pp_enabled_extension_names: *const *const c_char,
}

/// Stand-in for the `VkInstance` the emulation reports creating. Handles
/// must be non-null, and only the stubs below ever see this one, so its
/// contents never matter.
static EMULATED_INSTANCE: u8 = 0;

/// `VkResult vkEnumerateInstanceVersion(uint32_t *pApiVersion)`.
extern "C" fn stub_instance_version(api_version: *mut u32) -> c_int {
    if !api_version.is_null() {
        // SAFETY: Vulkan-Hpp passes a pointer to its own live `uint32_t`.
        unsafe { *api_version = VK_API_VERSION_1_3 };
    }
    VK_SUCCESS
}

/// `VkResult vkCreateInstance(const VkInstanceCreateInfo*,
/// const VkAllocationCallbacks*, VkInstance *pInstance)`.
extern "C" fn stub_create_instance(
    _create_info: *const c_void,
    _allocator: *const c_void,
    instance: *mut *mut c_void,
) -> c_int {
    if !instance.is_null() {
        // SAFETY: Vulkan-Hpp passes a pointer to its own live handle slot.
        unsafe { *instance = std::ptr::addr_of!(EMULATED_INSTANCE).cast_mut().cast() };
    }
    VK_SUCCESS
}

/// `VkResult vkEnumerateInstanceLayerProperties(uint32_t *pCount,
/// VkLayerProperties*)` — reports none.
extern "C" fn stub_no_layers(count: *mut u32, _properties: *mut c_void) -> c_int {
    if !count.is_null() {
        // SAFETY: Vulkan-Hpp passes a pointer to its own live counter.
        unsafe { *count = 0 };
    }
    VK_SUCCESS
}

/// Shared shape of `vkEnumerateInstanceExtensionProperties(const char
/// *pLayerName, uint32_t *pCount, …)` and
/// `vkEnumeratePhysicalDevices(VkInstance, uint32_t *pCount, …)`: a
/// leading name or handle, then the count. Reports none of either — the
/// empty device list is what sends ggml down its exception-free early
/// return.
extern "C" fn stub_none_of(_first: *const c_void, count: *mut u32, _out: *mut c_void) -> c_int {
    if !count.is_null() {
        // SAFETY: Vulkan-Hpp passes a pointer to its own live counter.
        unsafe { *count = 0 };
    }
    VK_SUCCESS
}

/// `void vkDestroyInstance(VkInstance, const VkAllocationCallbacks*)` —
/// there is nothing to destroy.
extern "C" fn stub_destroy_instance(_instance: *mut c_void, _allocator: *const c_void) {}

/// Entry points of the no-device emulation, by name. Unknown names get
/// null: ggml asks for hundreds while priming its dispatcher, and calls
/// none of them once it has seen that there are no devices.
fn emulated_proc(p_name: *const c_char) -> *const c_void {
    if p_name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: Vulkan's C ABI requires `p_name` to be NUL-terminated.
    let name = unsafe { CStr::from_ptr(p_name) };
    match name.to_bytes() {
        b"vkEnumerateInstanceVersion" => stub_instance_version as *const c_void,
        b"vkEnumerateInstanceLayerProperties" => stub_no_layers as *const c_void,
        b"vkEnumerateInstanceExtensionProperties" | b"vkEnumeratePhysicalDevices" => {
            stub_none_of as *const c_void
        }
        b"vkCreateInstance" => stub_create_instance as *const c_void,
        b"vkDestroyInstance" => stub_destroy_instance as *const c_void,
        _ => std::ptr::null(),
    }
}

/// Ask the system loader, through its own entry points, whether a usable
/// Vulkan device exists: create a throwaway instance and count physical
/// devices. Fewer than one device means the accelerated path cannot run
/// and the emulation takes over.
///
/// # Safety
/// `gipa` is the loader's genuine `vkGetInstanceProcAddr`.
unsafe fn device_available(gipa: *mut c_void) -> bool {
    // SAFETY: transmuted to the Vulkan C ABI for the real trampoline.
    let gipa: unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_void =
        unsafe { std::mem::transmute(gipa) };
    unsafe {
        let create = gipa(std::ptr::null_mut(), c"vkCreateInstance".as_ptr());
        if create.is_null() {
            return false;
        }
        let create: unsafe extern "C" fn(
            *const VkInstanceCreateInfo,
            *const c_void,
            *mut *mut c_void,
        ) -> c_int = std::mem::transmute(create);
        let info = VkInstanceCreateInfo {
            s_type: VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            p_application_info: std::ptr::null(),
            enabled_layer_count: 0,
            pp_enabled_layer_names: std::ptr::null(),
            enabled_extension_count: 0,
            pp_enabled_extension_names: std::ptr::null(),
        };
        let mut instance: *mut c_void = std::ptr::null_mut();
        if create(&raw const info, std::ptr::null(), &raw mut instance) != VK_SUCCESS
            || instance.is_null()
        {
            return false;
        }

        let mut count: u32 = 0;
        let enumerate = gipa(instance, c"vkEnumeratePhysicalDevices".as_ptr());
        if !enumerate.is_null() {
            let f: unsafe extern "C" fn(*mut c_void, *mut u32, *mut c_void) -> c_int =
                std::mem::transmute(enumerate);
            if f(instance, &raw mut count, std::ptr::null_mut()) != VK_SUCCESS {
                count = 0;
            }
        }
        let destroy = gipa(instance, c"vkDestroyInstance".as_ptr());
        if !destroy.is_null() {
            let f: unsafe extern "C" fn(*mut c_void, *const c_void) = std::mem::transmute(destroy);
            f(instance, std::ptr::null());
        }
        count > 0
    }
}

/// Real entry points resolved from the system Vulkan loader, or all null
/// when the loader could not be opened or reported no Vulkan device.
#[derive(Clone, Copy)]
struct Loader {
    get_instance_proc_addr: *mut c_void,
    cmd_copy_buffer: *mut c_void,
    get_physical_device_features2: *mut c_void,
}

impl Loader {
    /// The all-null loader: forwarding is off, the emulation answers.
    const fn none() -> Self {
        Self {
            get_instance_proc_addr: std::ptr::null_mut(),
            cmd_copy_buffer: std::ptr::null_mut(),
            get_physical_device_features2: std::ptr::null_mut(),
        }
    }
}

// SAFETY: the fields are opaque function pointers into the (process-
// global, never-unloaded) Vulkan loader; sharing them across threads is
// sound.
unsafe impl Send for Loader {}
unsafe impl Sync for Loader {}

fn loader() -> &'static Loader {
    static LOADER: OnceLock<Loader> = OnceLock::new();
    LOADER.get_or_init(|| unsafe {
        let handle = sys::open(sys::LOADER_NAME);
        if handle.is_null() {
            return Loader::none();
        }
        let gipa = sys::symbol(handle, c"vkGetInstanceProcAddr");
        if gipa.is_null() || !device_available(gipa) {
            return Loader::none();
        }
        Loader {
            get_instance_proc_addr: gipa,
            cmd_copy_buffer: sys::symbol(handle, c"vkCmdCopyBuffer"),
            get_physical_device_features2: sys::symbol(handle, c"vkGetPhysicalDeviceFeatures2"),
        }
    })
}

/// `PFN_vkVoidFunction vkGetInstanceProcAddr(VkInstance, const char*)`.
///
/// With a Vulkan device present this delegates to the real
/// `vkGetInstanceProcAddr`. Without one it answers from the no-device
/// emulation, which reports zero physical devices so ggml registers no
/// Vulkan devices and inference runs on the CPU. See the module docs for
/// why it must neither return null nor report an error here.
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
        return emulated_proc(p_name);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(name: &CStr) -> *const c_void {
        emulated_proc(name.as_ptr())
    }

    // The whole point of the emulation is that ggml's init walk finds
    // every entry point it needs, succeeds at each, and ends up with an
    // empty device list. A null here would fault; an error would throw.
    #[test]
    fn emulation_answers_the_init_walk() {
        for name in [
            c"vkEnumerateInstanceVersion",
            c"vkEnumerateInstanceLayerProperties",
            c"vkEnumerateInstanceExtensionProperties",
            c"vkCreateInstance",
            c"vkEnumeratePhysicalDevices",
            c"vkDestroyInstance",
        ] {
            assert!(!proc(name).is_null(), "{name:?} must resolve without the loader");
        }
    }

    #[test]
    fn emulation_declines_anything_else() {
        assert!(proc(c"vkCreateDevice").is_null());
        assert!(emulated_proc(std::ptr::null()).is_null());
    }

    #[test]
    fn emulation_reports_no_devices_and_a_usable_api_version() {
        let mut version = 0u32;
        assert_eq!(stub_instance_version(&raw mut version), VK_SUCCESS);
        assert!(version >= (1 << 22) | (2 << 12), "ggml requires Vulkan 1.2 or newer");

        let mut count = 7u32;
        let mut instance: *mut c_void = std::ptr::null_mut();
        assert_eq!(
            stub_create_instance(std::ptr::null(), std::ptr::null(), &raw mut instance),
            VK_SUCCESS
        );
        assert!(!instance.is_null(), "Vulkan handles must be non-null");
        assert_eq!(stub_none_of(instance, &raw mut count, std::ptr::null_mut()), VK_SUCCESS);
        assert_eq!(count, 0, "zero devices is what keeps ggml on its exception-free path");
    }
}
