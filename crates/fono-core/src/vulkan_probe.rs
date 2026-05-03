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
//! 2. Creating a minimal `VkInstance`.
//! 3. Enumerating physical devices and capturing their friendly names.
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
//! `fono daemon` and pressing Ctrl-C.
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
/// line, tab-separated, ASCII-safe.
fn encode(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Available { devices } if devices.is_empty() => "OK_EMPTY".to_string(),
        Outcome::Available { devices } => {
            let joined = devices
                .iter()
                .map(|d| sanitize_field(d))
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
        return Some(Outcome::Available {
            devices: rest.split('\t').map(str::to_string).collect(),
        });
    }
    if let Some(rest) = line.strip_prefix("ERR\t") {
        return Some(Outcome::NotAvailable {
            reason: rest.to_string(),
        });
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
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout pipe missing".to_string())?;
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

    let buf = reader
        .join()
        .map_err(|_| "probe helper stdout reader panicked".to_string())?;
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

    #[test]
    fn wire_protocol_roundtrips() {
        let cases = [
            Outcome::Available {
                devices: vec![
                    "GeForce RTX 4090".into(),
                    "llvmpipe (LLVM 19, 256 bits)".into(),
                ],
            },
            Outcome::Available { devices: vec![] },
            Outcome::Available {
                devices: vec!["single".into()],
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
