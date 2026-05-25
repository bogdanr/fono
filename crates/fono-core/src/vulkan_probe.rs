// SPDX-License-Identifier: GPL-3.0-only
//! Runtime detection of Vulkan availability on the host.
//!
//! Per slice 2 of `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`, the
//! CPU variant of fono needs to know whether the user could benefit
//! from upgrading to the GPU variant. The probe answers that question
//! by:
//!
//! 1. Opening `libvulkan.so.1` via `ash::Entry::load()` (which uses
//!    `libloading` under the hood — no link-time dep).
//! 2. Creating a minimal `VkInstance` (Vulkan 1.2 requested so the
//!    `shaderFloat16` device feature is reachable via core
//!    `vkGetPhysicalDeviceFeatures2`).
//! 3. Enumerating physical devices and capturing their friendly names,
//!    `VkPhysicalDeviceType`, and `shaderFloat16` support — the three
//!    facts the wizard's `HostGpu` classifier (ADR 0028) needs.
//!
//! ## Why we run the probe in a subprocess
//!
//! Loading `libvulkan.so.1` triggers the Vulkan loader to enumerate and
//! `dlopen` every ICD it can find. Several ICDs (notably Mesa's
//! `vulkan-mesa-lvp` software fallback, but also some buggy NVIDIA
//! driver builds) leave background worker threads attached to libraries
//! that get unmapped during glibc's `dl_fini` at process exit, which
//! produces a segfault *after* the daemon has already done its job.
//! The crash is reproducible on stock Debian 13 just by running
//! `fono` and pressing Ctrl-C.
//!
//! Rather than try to clean that up in-process — which is unsolvable
//! across the matrix of distro / driver / loader versions we ship to —
//! `probe()` re-execs the current binary with the
//! `FONO_INTERNAL_VULKAN_PROBE` environment variable set, captures the
//! result on stdout, and returns. The child carries all of libvulkan's
//! shutdown hazard with it; if the child happens to segfault on `_exit`
//! after writing its result line we ignore that and trust the line we
//! already received. The long-lived parent never touches libvulkan.
//!
//! For library consumers whose host binary does **not** honour the
//! re-exec hook (e.g. integration tests linking `fono-core` directly),
//! the subprocess hand-off short-circuits to "Vulkan: not available
//! (probe helper unavailable)" rather than fall back to an in-process
//! probe — that keeps the `probe()` contract "never trigger the
//! shutdown hazard in the caller's process" universally. Direct
//! in-process probing is still available via `probe_in_process()` for
//! callers that explicitly want it (and accept the consequences).
//!
//! Cost on first call: ~50–300 ms on Mesa (driver enumeration + child
//! exec); single tens of ms when libvulkan is absent. The result is
//! cached per-process so subsequent calls are free.

use std::ffi::c_char;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::hwcheck::HostGpu;

/// Environment variable used to signal an `FONO_INTERNAL_VULKAN_PROBE=1`
/// re-exec. The host binary's entry point should call
/// [`run_subprocess_probe_if_requested`] before initialising any
/// long-lived state; if the env var is set we run the probe, print the
/// result on stdout in the protocol [`probe`] expects, and `_exit`.
pub const PROBE_ENV_VAR: &str = "FONO_INTERNAL_VULKAN_PROBE";

/// How long the parent waits for the child to print its result before
/// giving up. The actual probe finishes in well under a second on
/// healthy systems; a generous timeout keeps slow/cold first-boot Mesa
/// driver enumeration from being mis-classified as failure.
const CHILD_TIMEOUT: Duration = Duration::from_secs(8);

/// Coarse `VkPhysicalDeviceType` mapping. Only the four classes that
/// matter for the `HostGpu` classifier (ADR 0028); the spec's `Other`
/// is folded into [`DeviceClass::Cpu`] (treated as no-real-GPU).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceClass {
    /// `VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU`.
    Integrated,
    /// `VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU`.
    Discrete,
    /// `VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU`. Treat as no real GPU.
    Virtual,
    /// `VK_PHYSICAL_DEVICE_TYPE_CPU` or `_OTHER` (software rasteriser).
    Cpu,
}

impl DeviceClass {
    fn as_code(self) -> u8 {
        match self {
            Self::Integrated => 1,
            Self::Discrete => 2,
            Self::Virtual => 3,
            Self::Cpu => 4,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Integrated),
            2 => Some(Self::Discrete),
            3 => Some(Self::Virtual),
            4 => Some(Self::Cpu),
            _ => None,
        }
    }
}

/// One physical device, as reported by the Vulkan loader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// `VkPhysicalDeviceProperties.deviceName`.
    pub name: String,
    /// Coarse class (Integrated / Discrete / Virtual / CPU). Drives the
    /// `HostGpu` classifier in ADR 0028.
    pub class: DeviceClass,
    /// `VkPhysicalDeviceVulkan12Features.shaderFloat16` (or the
    /// `KHR_shader_float16_int8` extension feature on 1.1 loaders).
    /// Whisper.cpp's ggml-vulkan backend requires this for its fp16
    /// kernels; legacy iGPUs without it run the slower fp32 path.
    pub supports_fp16: bool,
    /// `VK_KHR_cooperative_matrix` extension is exposed by the driver.
    /// Presence of this extension is what unlocks whisper.cpp's
    /// ggml-vulkan tensor matmul kernel and is the discriminator
    /// between [`HostGpu::Integrated`] (fp16-capable but no tensor
    /// matmul path: Kaby Lake-R through Alder Lake iGPUs) and
    /// [`HostGpu::IntegratedTensor`] (Lunar Lake / Arc / Battlemage /
    /// RDNA3+ APUs / Apple Silicon via MoltenVK). Decoders that
    /// receive a wire-protocol payload from an older fono build that
    /// pre-dates this field set it to `false` (forward-compatible).
    pub supports_cooperative_matrix: bool,
}

/// Outcome of a single probe attempt.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// `libvulkan.so.1` was loadable and at least one physical device was
    /// reported.
    Available { devices: Vec<DeviceInfo> },
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
                let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
                format!("Vulkan: detected ({})", names.join(", "))
            }
            Self::NotAvailable { reason } => format!("Vulkan: not available ({reason})"),
        }
    }

    #[must_use]
    pub fn is_usable(&self) -> bool {
        matches!(self, Self::Available { devices } if !devices.is_empty())
    }

    /// Classify this probe outcome into a [`HostGpu`] (ADR 0028).
    ///
    /// Rule: any `Discrete` device wins; else any `Integrated` device
    /// with `shaderFloat16` **and** `VK_KHR_cooperative_matrix` is
    /// [`HostGpu::IntegratedTensor`]; else any `Integrated` device with
    /// `shaderFloat16` (no cooperative matrix) is [`HostGpu::Integrated`];
    /// everything else (legacy iGPU without fp16, virtual, software
    /// rasteriser, no Vulkan, probe failure) is [`HostGpu::None`].
    #[must_use]
    pub fn host_gpu_class(&self) -> HostGpu {
        let Self::Available { devices } = self else {
            return HostGpu::None;
        };
        if devices.iter().any(|d| d.class == DeviceClass::Discrete) {
            return HostGpu::Discrete;
        }
        if devices.iter().any(|d| {
            d.class == DeviceClass::Integrated && d.supports_fp16 && d.supports_cooperative_matrix
        }) {
            return HostGpu::IntegratedTensor;
        }
        if devices.iter().any(|d| d.class == DeviceClass::Integrated && d.supports_fp16) {
            return HostGpu::Integrated;
        }
        HostGpu::None
    }
}

/// Probe the host for Vulkan loader + at least one physical device.
///
/// Always returns a value — never panics on a missing loader, broken
/// driver, sandboxed environment, helper-spawn failure, or child crash
/// after the result has been written. Result is cached for the lifetime
/// of the calling process; subsequent calls are O(1).
///
/// The probe runs in a short-lived child process for the reasons
/// described in the module docs. Set `FONO_VULKAN_PROBE_DISABLE=1`
/// to skip it entirely (returns `NotAvailable { reason: "disabled" }`).
#[must_use]
pub fn probe() -> Outcome {
    static CACHE: OnceLock<Outcome> = OnceLock::new();
    CACHE.get_or_init(probe_uncached).clone()
}

fn probe_uncached() -> Outcome {
    if std::env::var_os("FONO_VULKAN_PROBE_DISABLE").is_some() {
        return Outcome::NotAvailable {
            reason: "disabled via FONO_VULKAN_PROBE_DISABLE".to_string(),
        };
    }
    match run_in_subprocess() {
        Ok(outcome) => outcome,
        Err(reason) => Outcome::NotAvailable { reason },
    }
}

/// Run the probe in-process. **Carries the libvulkan shutdown hazard
/// into the caller's process** — see module docs. Exposed for
/// (a) the subprocess hook in `run_subprocess_probe_if_requested` and
/// (b) tests that explicitly want the in-process behaviour.
#[must_use]
pub fn probe_in_process() -> Outcome {
    // SAFETY: `ash::Entry::load` opens `libvulkan.so.1` via libloading.
    // This is unsafe per ash's contract because the resulting Entry
    // assumes the library exposes the Vulkan ABI; on success we only
    // call standardised Vulkan entry points, so the contract holds.
    let entry = match unsafe { ash::Entry::load() } {
        Ok(entry) => entry,
        Err(err) => {
            return Outcome::NotAvailable { reason: format!("libvulkan.so.1 not loadable: {err}") };
        }
    };

    // Minimal application info. Request Vulkan 1.2 so
    // `vkGetPhysicalDeviceFeatures2` is core and the
    // `VkPhysicalDeviceVulkan12Features.shaderFloat16` query works
    // uniformly across drivers. If a loader refuses (very old
    // libvulkan), `vkCreateInstance` returns an error and we degrade
    // gracefully to `NotAvailable` — the caller treats that as
    // `HostGpu::None`, which is the right verdict for any host too old
    // to satisfy the 1.2 instance contract.
    let app_info = ash::vk::ApplicationInfo::default()
        .application_name(c"fono")
        .application_version(0)
        .engine_name(c"fono-vulkan-probe")
        .engine_version(0)
        .api_version(ash::vk::API_VERSION_1_2);
    let create_info = ash::vk::InstanceCreateInfo::default().application_info(&app_info);

    // SAFETY: create_instance is a standard Vulkan entry point;
    // CreateInfo references stay alive for the call duration.
    let instance = match unsafe { entry.create_instance(&create_info, None) } {
        Ok(inst) => inst,
        Err(err) => {
            return Outcome::NotAvailable { reason: format!("vkCreateInstance rejected: {err}") };
        }
    };

    // SAFETY: instance is valid; the call only reads properties.
    let devices_result = unsafe { instance.enumerate_physical_devices() };
    let devices = match devices_result {
        Ok(devs) => devs
            .into_iter()
            .map(|dev| {
                // SAFETY: instance is valid; properties is a POD output.
                let props = unsafe { instance.get_physical_device_properties(dev) };
                let name = device_name_from_properties(&props.device_name);
                let class = match props.device_type {
                    ash::vk::PhysicalDeviceType::INTEGRATED_GPU => DeviceClass::Integrated,
                    ash::vk::PhysicalDeviceType::DISCRETE_GPU => DeviceClass::Discrete,
                    ash::vk::PhysicalDeviceType::VIRTUAL_GPU => DeviceClass::Virtual,
                    _ => DeviceClass::Cpu,
                };
                // Query shaderFloat16 via the Vulkan 1.2 features chain.
                let mut vk12 = ash::vk::PhysicalDeviceVulkan12Features::default();
                let mut features2 =
                    ash::vk::PhysicalDeviceFeatures2::default().push_next(&mut vk12);
                // SAFETY: instance is valid; features2 chain is properly
                // initialised; the call only reads device features.
                unsafe { instance.get_physical_device_features2(dev, &mut features2) };
                let supports_fp16 = vk12.shader_float16 != 0;
                // VK_KHR_cooperative_matrix presence — query device
                // extensions and look for the string. The feature
                // struct itself is in `ash::vk::khr_cooperative_matrix`
                // but for the wizard's classifier we only care whether
                // the extension is *advertised* (presence is sufficient
                // to know ggml-vulkan can take its tensor matmul path).
                // SAFETY: instance is valid; the call only reads
                // extension properties.
                let supports_cooperative_matrix =
                    unsafe { instance.enumerate_device_extension_properties(dev) }
                        .map(|exts| {
                            exts.iter().any(|ext| {
                                let bytes: &[c_char] = &ext.extension_name;
                                let name: Vec<u8> = bytes
                                    .iter()
                                    .take_while(|&&c| c != 0)
                                    .map(|&c| c as u8)
                                    .collect();
                                name == b"VK_KHR_cooperative_matrix"
                            })
                        })
                        .unwrap_or(false);
                DeviceInfo { name, class, supports_fp16, supports_cooperative_matrix }
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

    Outcome::Available { devices }
}

/// If the current process was spawned by [`probe`] (i.e. the
/// [`PROBE_ENV_VAR`] environment variable is set), run the in-process
/// probe, write its result to stdout in the wire protocol `probe`
/// expects, and exit immediately.
///
/// **Must be called from the host binary's `main` before initialising
/// any long-running state**, so the child does the minimum amount of
/// work and so the parent's logger never injects extra lines onto the
/// child's stdout. Returns `false` if no probe was requested (the
/// caller should continue normal startup); never returns `true` (it
/// `_exit`s instead).
pub fn run_subprocess_probe_if_requested() -> bool {
    if std::env::var_os(PROBE_ENV_VAR).is_none() {
        return false;
    }
    let outcome = probe_in_process();
    let line = encode(&outcome);
    // Use raw stdout writes; stdlib `println!` is fine here, errors
    // ignored — the parent's read-side timeout / parse will surface
    // any mishap.
    println!("{line}");
    // Force-flush stdout before we exit, so the parent's blocking read
    // on the pipe receives the result line *before* whatever happens
    // during teardown. Note that `std::process::exit(0)` will run
    // atexit handlers and `dl_fini` — and on Mesa's lvp ICD that will
    // itself segfault as libvulkan gets unmapped while its workers are
    // still parked. That's *fine*: the parent already has our line,
    // and `run_in_subprocess` ignores the child's exit status. The
    // shutdown crash now happens in this short-lived subprocess
    // instead of the long-lived daemon, which is exactly the
    // separation we want.
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

/// Encode an `Outcome` for transport on the child's stdout. Single
/// line, tab-separated, ASCII-safe. Each device is encoded as
/// `name|class_code|fp16_flag` where `class_code` is the
/// [`DeviceClass`] integer and `fp16_flag` is `0` or `1`.
fn encode(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Available { devices } if devices.is_empty() => "OK_EMPTY".to_string(),
        Outcome::Available { devices } => {
            let joined = devices
                .iter()
                .map(|d| {
                    format!(
                        "{}|{}|{}|{}",
                        sanitize_field(&d.name).replace('|', " "),
                        d.class.as_code(),
                        u8::from(d.supports_fp16),
                        u8::from(d.supports_cooperative_matrix),
                    )
                })
                .collect::<Vec<_>>()
                .join("\t");
            format!("OK\t{joined}")
        }
        Outcome::NotAvailable { reason } => format!("ERR\t{}", sanitize_field(reason)),
    }
}

fn decode(line: &str) -> Option<Outcome> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line == "OK_EMPTY" {
        return Some(Outcome::Available { devices: vec![] });
    }
    if let Some(rest) = line.strip_prefix("OK\t") {
        let mut devices = Vec::new();
        for field in rest.split('\t') {
            let mut parts = field.splitn(4, '|');
            let name = parts.next()?.to_string();
            let class_code: u8 = parts.next()?.parse().ok()?;
            let fp16_flag: u8 = parts.next()?.parse().ok()?;
            // Forward-compatible: pre-cooperative-matrix builds emit
            // only three fields; treat the missing 4th as 0.
            let coopmat_flag: u8 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let class = DeviceClass::from_code(class_code)?;
            devices.push(DeviceInfo {
                name,
                class,
                supports_fp16: fp16_flag != 0,
                supports_cooperative_matrix: coopmat_flag != 0,
            });
        }
        return Some(Outcome::Available { devices });
    }
    if let Some(rest) = line.strip_prefix("ERR\t") {
        return Some(Outcome::NotAvailable { reason: rest.to_string() });
    }
    None
}

/// Replace tabs/newlines/CR with spaces so the wire protocol stays
/// single-line + tab-delimited regardless of what an ICD reports as a
/// device name (some Mesa builds embed "(LLVM 19.1.7, 256 bits)"
/// strings with parentheses; safe — but we still defensively scrub
/// control chars).
fn sanitize_field(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\t' | '\r' | '\n' => ' ',
            _ => c,
        })
        .collect()
}

/// Spawn the current binary with `PROBE_ENV_VAR` set, wait up to
/// [`CHILD_TIMEOUT`] for it to print one line on stdout, and parse it
/// into an `Outcome`. Any spawn / IO / parse / timeout failure becomes
/// an `Err(reason)` so the caller surfaces it as `NotAvailable`.
fn run_in_subprocess() -> Result<Outcome, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe unavailable: {e}"))?;

    // We pass `--help` as a sacrificial argument so that, if the host
    // binary does NOT honour the env-var hook, clap will print its
    // help to stdout, our parser will fail to find the marker, and we
    // fall through to an `Err` rather than hanging until the child
    // would itself start daemons / open audio devices / etc. The
    // cooperating hook in `run_subprocess_probe_if_requested` runs
    // before clap so it short-circuits before `--help` is processed.
    //
    // Note: we deliberately do *not* fall back to `probe_in_process()`
    // when the helper is unavailable — see module docs.
    let mut child = Command::new(&exe)
        .arg("--help")
        .env(PROBE_ENV_VAR, "1")
        // Belt-and-braces: prevent recursive probing if some downstream
        // helper of the host binary itself spawns a probe.
        .env("FONO_VULKAN_PROBE_DISABLE", "1")
        // Quiet the host binary in case --help isn't reached: no tray,
        // no logger output to stderr that we care about.
        .env("FONO_LOG", "off")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn probe helper failed: {e}"))?;

    // Read stdout with a wall-clock timeout. We don't need a
    // sophisticated event loop: the helper writes one short line then
    // exits, and stdout closes on exit, so a blocking read in a thread
    // with a join-with-timeout is enough.
    let mut stdout = child.stdout.take().ok_or_else(|| "child stdout pipe missing".to_string())?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(256);
        // Cap the read so a misbehaving (non-cooperating) child that
        // streams `--help` text forever can't exhaust memory.
        let _ = stdout.by_ref().take(64 * 1024).read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("probe helper timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("probe helper wait failed: {e}"));
            }
        }
    }

    let buf = reader.join().map_err(|_| "probe helper stdout reader panicked".to_string())?;
    let text = String::from_utf8_lossy(&buf);

    // Find the first line that decodes as our wire protocol. If the
    // host binary printed `--help` instead of honouring the hook, none
    // of the lines will match → the host binary did not cooperate.
    for line in text.lines() {
        if let Some(outcome) = decode(line) {
            return Ok(outcome);
        }
    }

    Err("probe helper unavailable (no result line received)".to_string())
}

/// Convert a fixed-size `c_char` array (Vulkan device-name field) to a
/// printable `String`. Stops at the first NUL or at the array boundary.
fn device_name_from_properties(name: &[c_char]) -> String {
    let bytes: Vec<u8> = name.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
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
        assert_eq!(absent.host_gpu_class(), HostGpu::None);

        let empty = Outcome::Available { devices: vec![] };
        assert_eq!(empty.summary_line(), "Vulkan: loader present but no physical devices");
        assert!(!empty.is_usable());
        assert_eq!(empty.host_gpu_class(), HostGpu::None);

        let with_dev = Outcome::Available {
            devices: vec![DeviceInfo {
                name: "Test GPU".into(),
                class: DeviceClass::Discrete,
                supports_fp16: true,
                supports_cooperative_matrix: true,
            }],
        };
        assert_eq!(with_dev.summary_line(), "Vulkan: detected (Test GPU)");
        assert!(with_dev.is_usable());
        assert_eq!(with_dev.host_gpu_class(), HostGpu::Discrete);
    }

    #[test]
    fn host_gpu_class_picks_best_present() {
        // Discrete wins over integrated.
        let mixed = Outcome::Available {
            devices: vec![
                DeviceInfo {
                    name: "iGPU".into(),
                    class: DeviceClass::Integrated,
                    supports_fp16: true,
                    supports_cooperative_matrix: false,
                },
                DeviceInfo {
                    name: "dGPU".into(),
                    class: DeviceClass::Discrete,
                    supports_fp16: true,
                    supports_cooperative_matrix: true,
                },
            ],
        };
        assert_eq!(mixed.host_gpu_class(), HostGpu::Discrete);

        // Integrated without fp16 falls back to None.
        let legacy = Outcome::Available {
            devices: vec![DeviceInfo {
                name: "HD 620".into(),
                class: DeviceClass::Integrated,
                supports_fp16: false,
                supports_cooperative_matrix: false,
            }],
        };
        assert_eq!(legacy.host_gpu_class(), HostGpu::None);

        // Modern integrated with fp16 but no cooperative_matrix
        // (Iris Xe / UHD 620 on modern Mesa) → Integrated.
        let xe = Outcome::Available {
            devices: vec![DeviceInfo {
                name: "Iris Xe".into(),
                class: DeviceClass::Integrated,
                supports_fp16: true,
                supports_cooperative_matrix: false,
            }],
        };
        assert_eq!(xe.host_gpu_class(), HostGpu::Integrated);

        // Tensor-capable integrated (Lunar Lake / Xe2): fp16 +
        // VK_KHR_cooperative_matrix → IntegratedTensor.
        let xe2 = Outcome::Available {
            devices: vec![DeviceInfo {
                name: "Intel Graphics (LNL)".into(),
                class: DeviceClass::Integrated,
                supports_fp16: true,
                supports_cooperative_matrix: true,
            }],
        };
        assert_eq!(xe2.host_gpu_class(), HostGpu::IntegratedTensor);

        // Software rasteriser → None.
        let llvmpipe = Outcome::Available {
            devices: vec![DeviceInfo {
                name: "llvmpipe".into(),
                class: DeviceClass::Cpu,
                supports_fp16: true,
                supports_cooperative_matrix: true,
            }],
        };
        assert_eq!(llvmpipe.host_gpu_class(), HostGpu::None);
    }

    #[test]
    fn wire_protocol_roundtrips() {
        let cases = [
            Outcome::Available {
                devices: vec![
                    DeviceInfo {
                        name: "GeForce RTX 4090".into(),
                        class: DeviceClass::Discrete,
                        supports_fp16: true,
                        supports_cooperative_matrix: true,
                    },
                    DeviceInfo {
                        name: "llvmpipe (LLVM 19, 256 bits)".into(),
                        class: DeviceClass::Cpu,
                        supports_fp16: false,
                        supports_cooperative_matrix: false,
                    },
                ],
            },
            Outcome::Available { devices: vec![] },
            Outcome::Available {
                devices: vec![DeviceInfo {
                    name: "single".into(),
                    class: DeviceClass::Integrated,
                    supports_fp16: true,
                    supports_cooperative_matrix: false,
                }],
            },
            Outcome::NotAvailable {
                reason: "libvulkan.so.1 not loadable: cannot open shared object".into(),
            },
        ];
        for c in &cases {
            let encoded = encode(c);
            assert!(!encoded.contains('\n'), "encoded line must be single-line");
            let decoded = decode(&encoded).expect("decode");
            assert_eq!(format!("{c:?}"), format!("{decoded:?}"));
        }
    }

    #[test]
    fn decode_rejects_non_protocol_input() {
        assert!(decode("Usage: fono [OPTIONS]").is_none());
        assert!(decode("").is_none());
        assert!(decode("OK").is_none()); // missing tab+payload
    }

    #[test]
    fn sanitize_strips_control_chars() {
        assert_eq!(sanitize_field("a\tb\nc\rd"), "a b c d");
        assert_eq!(sanitize_field("normal text"), "normal text");
    }

    /// Run-only-on-demand smoke test: invokes the in-process probe
    /// against the host. The subprocess wrapper can't run here because
    /// the test binary doesn't honour `PROBE_ENV_VAR`. Use
    /// `cargo test -p fono-core vulkan_probe_smoke -- --ignored
    /// --nocapture` to see the output. CI runners don't have a GPU
    /// driver loaded, so this is `#[ignore]` to keep the default
    /// suite host-independent.
    #[test]
    #[ignore = "host-dependent: invokes real libvulkan if present"]
    fn vulkan_probe_smoke() {
        let outcome = probe_in_process();
        eprintln!("{}", outcome.summary_line());
        eprintln!("is_usable: {}", outcome.is_usable());
    }
}
