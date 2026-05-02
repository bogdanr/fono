// SPDX-License-Identifier: GPL-3.0-only
//! Runtime detection of Vulkan availability on the host.
//!
//! Per slice 2 of `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`, the
//! CPU variant of fono needs to know whether the user could benefit
//! from upgrading to the GPU variant. We probe the host at runtime by:
//!
//! 1. Opening `libvulkan.so.1` via `ash::Entry::load()` (which uses
//!    `libloading` under the hood — no link-time dep).
//! 2. Creating a minimal `VkInstance`.
//! 3. Enumerating physical devices and capturing their friendly names.
//!
//! Each step is fallible; any failure collapses to `Outcome::NotAvailable`
//! with a short reason string. The probe is **side-effect-free** beyond
//! the brief instance lifetime; it does not allocate device memory or
//! select a queue family.
//!
//! Cost on first call: ~50–300 ms on Mesa (driver enumeration); single
//! tens of ms when libvulkan is absent. Run once at daemon startup and
//! cache the result for the session.

use std::ffi::c_char;

/// Outcome of a single probe attempt.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// `libvulkan.so.1` was loadable and at least one physical device was
    /// reported. The names come from `VkPhysicalDeviceProperties.deviceName`.
    Available { devices: Vec<String> },
    /// Probe failed at some step — libvulkan missing, instance creation
    /// rejected, or device enumeration empty/error. The short reason is
    /// suitable for a doctor / log line.
    NotAvailable { reason: String },
}

impl Outcome {
    /// Render as a single line for doctor / log output.
    #[must_use]
    pub fn summary_line(&self) -> String {
        match self {
            Self::Available { devices } if devices.is_empty() => {
                "Vulkan: loader present but no physical devices".to_string()
            }
            Self::Available { devices } => {
                format!("Vulkan: detected ({})", devices.join(", "))
            }
            Self::NotAvailable { reason } => format!("Vulkan: not available ({reason})"),
        }
    }

    #[must_use]
    pub const fn is_usable(&self) -> bool {
        matches!(self, Self::Available { devices } if !devices.is_empty())
    }
}

/// Probe the host for Vulkan loader + at least one physical device.
/// Always returns a value — never panics on a missing loader, broken
/// driver, or sandboxed environment.
#[must_use]
pub fn probe() -> Outcome {
    // SAFETY: `ash::Entry::load` opens `libvulkan.so.1` via libloading.
    // This is unsafe per ash's contract because the resulting Entry
    // assumes the library exposes the Vulkan ABI; on success we only
    // call standardised Vulkan entry points, so the contract holds.
    let entry = match unsafe { ash::Entry::load() } {
        Ok(entry) => entry,
        Err(err) => {
            return Outcome::NotAvailable {
                reason: format!("libvulkan.so.1 not loadable: {err}"),
            };
        }
    };

    // Minimal application info — required by spec, contents unobserved
    // by the loader for an enumeration-only run.
    let app_info = ash::vk::ApplicationInfo::default()
        .application_name(c"fono")
        .application_version(0)
        .engine_name(c"fono-vulkan-probe")
        .engine_version(0)
        .api_version(ash::vk::API_VERSION_1_0);
    let create_info = ash::vk::InstanceCreateInfo::default().application_info(&app_info);

    // SAFETY: create_instance is a standard Vulkan entry point;
    // CreateInfo references stay alive for the call duration.
    let instance = match unsafe { entry.create_instance(&create_info, None) } {
        Ok(inst) => inst,
        Err(err) => {
            return Outcome::NotAvailable {
                reason: format!("vkCreateInstance rejected: {err}"),
            };
        }
    };

    // SAFETY: instance is valid; the call only reads properties.
    let devices_result = unsafe { instance.enumerate_physical_devices() };
    let names = match devices_result {
        Ok(devs) => devs
            .into_iter()
            .map(|dev| {
                // SAFETY: instance is valid; properties is a POD output.
                let props = unsafe { instance.get_physical_device_properties(dev) };
                device_name_from_properties(&props.device_name)
            })
            .collect::<Vec<_>>(),
        Err(err) => {
            // SAFETY: instance is valid; clean it up before returning.
            unsafe { instance.destroy_instance(None) };
            return Outcome::NotAvailable {
                reason: format!("vkEnumeratePhysicalDevices failed: {err}"),
            };
        }
    };

    // SAFETY: instance is valid; clean shutdown before returning the names.
    unsafe { instance.destroy_instance(None) };

    Outcome::Available { devices: names }
}

/// Convert a fixed-size `c_char` array (Vulkan device-name field) to a
/// printable `String`. Stops at the first NUL or at the array boundary.
fn device_name_from_properties(name: &[c_char]) -> String {
    let bytes: Vec<u8> = name
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_summary_lines_format() {
        let absent = Outcome::NotAvailable {
            reason: "libvulkan.so.1 not loadable: cannot open shared object file".to_string(),
        };
        assert!(absent.summary_line().starts_with("Vulkan: not available"));
        assert!(!absent.is_usable());

        let empty = Outcome::Available { devices: vec![] };
        assert_eq!(
            empty.summary_line(),
            "Vulkan: loader present but no physical devices"
        );
        assert!(!empty.is_usable());

        let with_dev = Outcome::Available {
            devices: vec!["Test GPU".to_string()],
        };
        assert_eq!(with_dev.summary_line(), "Vulkan: detected (Test GPU)");
        assert!(with_dev.is_usable());
    }

    /// Run-only-on-demand smoke test: invokes the actual probe against
    /// the host. `cargo test -p fono vulkan_probe_smoke -- --ignored
    /// --nocapture` to see the output. CI runners don't have a GPU
    /// driver loaded, so this is `#[ignore]` to keep the default
    /// suite host-independent.
    #[test]
    #[ignore = "host-dependent: invokes real libvulkan if present"]
    fn vulkan_probe_smoke() {
        let outcome = probe();
        eprintln!("{}", outcome.summary_line());
        eprintln!("is_usable: {}", outcome.is_usable());
    }
}
