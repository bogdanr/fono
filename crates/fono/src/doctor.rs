// SPDX-License-Identifier: GPL-3.0-only
//! `fono doctor` — diagnostic report.

use std::fmt::Write;
use std::io::IsTerminal;
use std::sync::OnceLock;

use anyhow::Result;
use fono_core::hwcheck;
use fono_core::{Config, Paths, Secrets};

/// Whether to emit ANSI color escapes in the report. True iff stdout
/// is a TTY and `NO_COLOR` is unset. Cached on first call.
fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}

fn paint(code: &str, s: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
#[rustfmt::skip] fn ok(s: &str) -> String { paint("32", s) } // green
#[rustfmt::skip] fn bad(s: &str) -> String { paint("31;1", s) } // bold red
#[rustfmt::skip] fn warn(s: &str) -> String { paint("33", s) } // yellow
#[rustfmt::skip] fn dim(s: &str) -> String { paint("2", s) } // dim
#[rustfmt::skip] fn head(s: &str) -> String { paint("1;36", s) } // bold cyan
#[rustfmt::skip]
fn star(active: bool) -> String {
    if active { paint("1;36", "*") } else { " ".into() }
}

#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
pub async fn report(paths: &Paths) -> Result<String> {
    let mut out = String::new();
    let variant = crate::variant::VARIANT;
    writeln!(
        out,
        "{} v{} ({} variant — {})",
        head("Fono doctor —"),
        env!("CARGO_PKG_VERSION"),
        variant.label(),
        variant.description(),
    )?;
    writeln!(out)?;

    // ----------------------------------------------------------------
    // Hardware probe + tier (drives wizard recommendations + helps
    // diagnose "why is local STT slow on my machine?")
    // ----------------------------------------------------------------
    let mut snap = hwcheck::probe(&paths.cache_dir);
    // Upgrade `host_gpu` from the Vulkan probe (no-op on Apple Silicon).
    // See ADR 0028.
    if snap.host_gpu == hwcheck::HostGpu::None {
        snap.host_gpu = fono_core::vulkan_probe::probe().host_gpu_class();
    }
    let tier = snap.tier();
    // Recommendation is the largest multilingual model that this binary
    // can actually afford to load — walking the same affordability gate
    // the wizard uses, against the inference path actually available to
    // this build (the CPU variant cannot reach the host's Vulkan GPU).
    // Replaces the static `tier.default_whisper_model()` lookup so
    // `fono doctor` and the wizard never disagree on the recommendation.
    let inference_snap =
        snap.for_inference(matches!(crate::variant::VARIANT, crate::variant::Variant::Gpu,));
    let recommended_model = fono_stt::registry::ModelRegistry::pick_default_local(&inference_snap);
    let ram_gb = snap.total_ram_bytes / (1024 * 1024 * 1024);
    let disk_gb = snap.free_disk_bytes / (1024 * 1024 * 1024);
    let isa = if snap.cpu_features.avx2 {
        "AVX2"
    } else if snap.cpu_features.neon {
        "NEON"
    } else {
        "no-vec"
    };
    writeln!(out, "{}", head("Hardware:"))?;
    writeln!(
        out,
        "  cores : {} physical / {} logical  ({isa})",
        snap.physical_cores, snap.logical_cores
    )?;
    writeln!(
        out,
        "  ram   : {ram_gb} GB total · disk free : {disk_gb} GB · arch : {}/{}",
        snap.os, snap.arch
    )?;
    writeln!(out, "  local-tier : {} (recommends whisper-{})", tier.as_str(), recommended_model)?;
    if let Err(reason) = snap.suitability() {
        writeln!(out, "  {} {reason}", bad("unsuitable because:"))?;
    }
    writeln!(out)?;

    // Compute backends — what's compiled into this variant + what the
    // host's Vulkan loader reports. Slice 2 of
    // `plans/2026-05-02-fono-cpu-gpu-variants-v1.md`.
    {
        use crate::variant::{Variant, VARIANT};
        use fono_core::vulkan_probe::probe;
        writeln!(out, "{}", head("Compute backends:"))?;
        writeln!(out, "  variant  : {} ({})", VARIANT.label(), VARIANT.description())?;
        let outcome = probe();
        writeln!(out, "  vulkan   : {}", outcome.summary_line())?;
        if matches!(VARIANT, Variant::Cpu) && outcome.is_usable() {
            writeln!(
                out,
                "  hint     : your machine has Vulkan-capable GPU(s); the GPU release \
                 variant runs inference faster on this hardware. Download \
                 `fono-gpu-vX.Y.Z-x86_64` from the Releases page (or upgrade in-place \
                 once `fono update --variant gpu` lands)."
            )?;
        }
        writeln!(out)?;
    }

    writeln!(out, "{}", head("Paths:"))?;
    writeln!(out, "  config : {}", paths.config_file().display())?;
    writeln!(out, "  data   : {}", paths.data_dir.display())?;
    writeln!(out, "  cache  : {}", paths.cache_dir.display())?;
    writeln!(out, "  state  : {}", paths.state_dir.display())?;
    writeln!(out)?;

    writeln!(out, "{} {}", head("Install:"), crate::install::doctor_state())?;
    writeln!(out)?;

    let config_exists = paths.config_file().exists();
    writeln!(
        out,
        "{} {}",
        head("Config :"),
        if config_exists { ok("present") } else { bad("MISSING (run `fono setup`)") }
    )?;
    let cfg = if config_exists {
        match Config::load(&paths.config_file()) {
            Ok(c) => {
                writeln!(out, "  version        : {}", c.version)?;
                writeln!(out, "  stt.backend    : {:?}", c.stt.backend)?;
                writeln!(out, "  stt.local.model: {}", c.stt.local.model)?;
                writeln!(out, "  polish.backend    : {:?}", c.polish.backend)?;
                writeln!(out, "  polish.local.model: {}", c.polish.local.model)?;
                writeln!(
                    out,
                    "  hotkeys        : dictation={} assistant={} (short=toggle, long=hold)",
                    c.hotkeys.dictation, c.hotkeys.assistant,
                )?;
                Some(c)
            }
            Err(e) => {
                writeln!(out, "  {} {e}", bad("FAILED TO LOAD:"))?;
                None
            }
        }
    } else {
        None
    };
    writeln!(out)?;

    // Personal vocabulary (ADR 0037): entry count / parse status.
    {
        let vpath = paths.vocabulary_file();
        let state = if vpath.exists() {
            match fono_core::correction::VocabularyFile::load(&vpath) {
                Ok(f) => match f.to_table() {
                    Ok(t) => ok(&format!("{} rule(s)", t.len())),
                    Err(e) => bad(&format!("INVALID ({e}) — corrections disabled")),
                },
                Err(e) => bad(&format!("FAILED TO LOAD: {e}")),
            }
        } else {
            "none (add with `fono vocabulary add <wrong> <right>`)".to_string()
        };
        writeln!(out, "{} {}", head("Vocabulary:"), state)?;
        writeln!(out, "  file : {}", vpath.display())?;
    }
    writeln!(out)?;

    // ----------------------------------------------------------------
    // Backend factories — if the user picked a cloud backend, exercise
    // the factory so they see a clear "API key missing" or "feature
    // missing" message right here rather than having to start the
    // daemon and read the log.
    // ----------------------------------------------------------------
    let secrets = Secrets::load(&paths.secrets_file()).unwrap_or_default();
    if let Some(c) = cfg.as_ref() {
        writeln!(out, "{}", head("Backends:"))?;
        match fono_stt::build_stt(&c.stt, &c.general, &secrets, &paths.whisper_models_dir()) {
            Ok(s) => writeln!(out, "  stt: {} {}", s.name(), ok("ready"))?,
            Err(e) => writeln!(out, "  stt: {} {e:#}", bad("FAIL —"))?,
        }
        match fono_polish::build_polish(&c.polish, &secrets, &paths.polish_models_dir()) {
            Ok(Some(l)) => writeln!(out, "  polish: {} {}", l.name(), ok("ready"))?,
            Ok(None) => writeln!(out, "  polish: {}", dim("disabled (cleanup off)"))?,
            Err(e) => writeln!(out, "  polish: {} {e:#}", bad("FAIL —"))?,
        }
        // `mut` is only touched by the `realtime`-gated arm below; allow
        // the unused-mut when that feature is compiled out.
        #[allow(unused_mut)]
        let mut assistant_realtime_only = false;
        match fono_assistant::build_assistant_handle(
            &c.assistant,
            &secrets,
            &paths.polish_models_dir(),
        ) {
            Ok(Some(fono_assistant::AssistantHandle::Staged(a))) => {
                writeln!(out, "  assistant: {} {} (staged)", a.name(), ok("ready"))?;
            }
            #[cfg(feature = "realtime")]
            Ok(Some(fono_assistant::AssistantHandle::Realtime(a))) => {
                assistant_realtime_only = true;
                writeln!(
                    out,
                    "  assistant: {} {} (realtime speech-to-speech)",
                    a.name(),
                    ok("ready")
                )?;
            }
            Ok(None) => writeln!(out, "  assistant: {}", dim("disabled"))?,
            Err(e) => writeln!(out, "  assistant: {} {e:#}", bad("FAIL —"))?,
        }
        match fono_tts::build_tts(&c.tts, &secrets, &c.general.languages, &paths.voices_dir()) {
            Ok(Some(t)) => writeln!(out, "  tts: {} {}", t.name(), ok("ready"))?,
            Ok(None) => {
                writeln!(out, "  tts: {}", warn("disabled (assistant replies will be silent)"))?;
            }
            Err(e) => writeln!(out, "  tts: {} {e:#}", bad("FAIL —"))?,
        }
        // Local LLM inference server (OpenAI + Ollama HTTP API; ADR 0036).
        // The served model tracks the active [assistant] backend, with a
        // same-provider text fallback when that backend is realtime.
        if c.server.llm.enabled {
            let scope = if c.server.llm.bind == "127.0.0.1" || c.server.llm.bind == "::1" {
                "loopback only"
            } else {
                "LAN-exposed"
            };
            let m = c.server.llm.model.trim();
            let override_model = (!m.is_empty()).then_some(m);
            let served = fono_assistant::server_assistant_model_name(&c.assistant, override_model);
            let served = if served.is_empty() { "fono".to_string() } else { served };
            // Proxy fast-lane (ADR 0036): OpenAI-compat cloud backends are
            // forwarded verbatim (full tool/vision/parameter fidelity);
            // everything else is adapted through the assistant trait.
            let proxyable = c.assistant.enabled
                && fono_assistant::chat_endpoint(&c.assistant.backend).is_some();
            let mode = if proxyable {
                "OpenAI surface proxied to the cloud provider (full tool/vision fidelity)"
            } else {
                "served via the local adapter"
            };
            writeln!(
                out,
                "  llm server: {} on {}:{} ({scope}); serving {served}; {mode}; OpenAI + Ollama API",
                ok("enabled"),
                c.server.llm.bind,
                c.server.llm.port,
            )?;
            if assistant_realtime_only {
                writeln!(
                    out,
                    "    {} the active assistant is realtime (speech-to-speech); the API \
                     auto-serves the same provider's text model ({served}) instead — set \
                     [server.llm].model to pin a different one.",
                    dim("note:"),
                )?;
            }
        } else {
            writeln!(
                out,
                "  llm server: {} (enable `[server.llm]` to serve local inference over HTTP)",
                dim("disabled"),
            )?;
        }
        writeln!(out)?;

        // ------------------------------------------------------------
        // Per-provider key + reachability matrix (provider-switching
        // plan task S18). One line per known backend with active marker
        // so users see at a glance which providers are ready to switch
        // to via `fono use stt …` / `fono use polish …`.
        // ------------------------------------------------------------
        writeln!(out, "{}", head("Providers (STT):"))?;
        for b in fono_core::providers::all_stt_backends() {
            let active = b == c.stt.backend;
            let mark = star(active);
            let name = fono_core::providers::stt_backend_str(&b);
            let needs_key = fono_core::providers::stt_requires_key(&b);
            let key_env = fono_core::providers::stt_key_env(&b);
            let key_status = if !needs_key {
                dim("no key needed")
            } else if secrets.resolve(key_env).is_some() {
                ok(&format!("{key_env} present"))
            } else {
                dim(&format!("{key_env} missing"))
            };
            let model = if needs_key {
                fono_stt::defaults::default_cloud_model(name).to_string()
            } else {
                c.stt.local.model.clone()
            };
            writeln!(out, "  {mark} {name:<14} model: {model:<32} {key_status}")?;
        }
        writeln!(out)?;

        writeln!(out, "{}", head("Providers (LLM):"))?;
        for b in fono_core::providers::all_polish_backends() {
            let active = b == c.polish.backend;
            let mark = star(active);
            let name = fono_core::providers::polish_backend_str(&b);
            let needs_key = fono_core::providers::polish_requires_key(&b);
            let key_env = fono_core::providers::polish_key_env(&b);
            let key_status = if !needs_key {
                dim("no key needed")
            } else if secrets.resolve(key_env).is_some() {
                ok(&format!("{key_env} present"))
            } else {
                dim(&format!("{key_env} missing"))
            };
            let model = if matches!(b, fono_core::config::PolishBackend::None) {
                "—".to_string()
            } else if needs_key || matches!(b, fono_core::config::PolishBackend::Ollama) {
                fono_polish::defaults::default_cloud_model(name).to_string()
            } else {
                c.polish.local.model.clone()
            };
            writeln!(out, "  {mark} {name:<14} model: {model:<32} {key_status}")?;
        }
        writeln!(out)?;

        writeln!(out, "{}", head("Providers (assistant):"))?;
        for b in fono_core::providers::all_assistant_backends() {
            let active = b == c.assistant.backend;
            let mark = star(active);
            let name = fono_core::providers::assistant_backend_str(&b);
            let needs_key = fono_core::providers::assistant_requires_key(&b);
            let key_env = fono_core::providers::assistant_key_env(&b);
            let key_status = if !needs_key {
                dim("no key needed")
            } else if secrets.resolve(key_env).is_some() {
                ok(&format!("{key_env} present"))
            } else {
                dim(&format!("{key_env} missing"))
            };
            writeln!(out, "  {mark} {name:<14} {key_status}")?;
        }
        writeln!(out)?;

        writeln!(out, "{}", head("Providers (TTS):"))?;
        for b in fono_core::providers::all_tts_backends() {
            let active = b == c.tts.backend;
            let mark = star(active);
            let name = fono_core::providers::tts_backend_str(&b);
            let needs_key = fono_core::providers::tts_requires_key(&b);
            let key_env = fono_core::providers::tts_key_env(&b);
            let extra = match b {
                fono_core::config::TtsBackend::Wyoming => c
                    .tts
                    .wyoming
                    .as_ref()
                    .map(|w| format!("uri={}", w.uri))
                    .unwrap_or_else(|| dim("uri=(unset)")),
                fono_core::config::TtsBackend::OpenAI
                | fono_core::config::TtsBackend::Groq
                | fono_core::config::TtsBackend::OpenRouter
                | fono_core::config::TtsBackend::Cartesia
                | fono_core::config::TtsBackend::Deepgram
                | fono_core::config::TtsBackend::ElevenLabs
                | fono_core::config::TtsBackend::Speechmatics
                | fono_core::config::TtsBackend::Gemini => {
                    if secrets.resolve(key_env).is_some() {
                        ok(&format!("{key_env} present"))
                    } else {
                        dim(&format!("{key_env} missing"))
                    }
                }
                fono_core::config::TtsBackend::Local => {
                    if c.tts.local.voice.is_empty() {
                        dim("voice=(default)")
                    } else {
                        format!("voice={}", c.tts.local.voice)
                    }
                }
                fono_core::config::TtsBackend::None => dim("—"),
            };
            let _ = needs_key;
            writeln!(out, "  {mark} {name:<14} {extra}")?;
        }
        writeln!(out)?;
        writeln!(out, "(* = active. Switch with `fono use stt|polish|assistant|tts <backend>`.)")?;
        writeln!(out)?;
    }

    writeln!(out, "{}", head("Session:"))?;
    writeln!(
        out,
        "  XDG_SESSION_TYPE : {}",
        std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "(unset)".into())
    )?;
    writeln!(
        out,
        "  WAYLAND_DISPLAY  : {}",
        std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "(unset)".into())
    )?;
    writeln!(
        out,
        "  DISPLAY          : {}",
        std::env::var("DISPLAY").unwrap_or_else(|_| "(unset)".into())
    )?;
    writeln!(out)?;

    writeln!(out, "{} {:?}", head("Audio stack :"), fono_audio::mute::detect())?;
    // Input device matrix: list every device the active stack
    // (PulseAudio / PipeWire via pactl, or cpal as fallback) reports,
    // marking whichever the OS currently considers default. Fono no
    // longer keeps an `[audio].input_device` override; microphone
    // selection is delegated to the OS layer (pavucontrol / GNOME /
    // KDE settings on Linux, Sound preferences on macOS / Windows).
    let devices = fono_audio::devices::list_input_devices();
    writeln!(out, "{}", head("Audio inputs:"))?;
    if devices.is_empty() {
        // Platform-appropriate recovery hint: the Linux advice
        // (install pactl/wpctl) is meaningless on macOS, where the
        // usual causes are no mic hardware (Mac Studio / mini) or a
        // denied Microphone permission in System Settings.
        let hint = if cfg!(target_os = "macos") {
            "(no input devices reported — connect a microphone, and check System Settings → Privacy & Security → Microphone if capture fails)"
        } else {
            "(no input devices reported — install `wireplumber` (wpctl) or `pulseaudio-utils` (pactl), or check that your microphone is plugged in)"
        };
        writeln!(out, "  {}", bad(hint))?;
    } else {
        for d in &devices {
            let mark = star(d.is_default);
            writeln!(out, "  {mark} {}", d.display_name)?;
        }
        writeln!(
            out,
            "(* = system default. Change via the tray Microphone submenu, \
             pavucontrol, or your OS sound settings.)"
        )?;
    }
    let injector = fono_inject::inject::Injector::detect();
    writeln!(out, "{} {injector:?}", head("Injector    :"))?;
    // macOS: injection is gated by the Accessibility TCC grant, and
    // CGEventPost drops events *silently* when it's missing — so the
    // probe is the only honest signal (macOS port plan Task 9.3).
    if let Some(trusted) = fono_inject::accessibility_trusted() {
        if trusted {
            writeln!(out, "{} {}", head("Accessibility:"), ok("granted (fono can type for you)"))?;
        } else {
            writeln!(
                out,
                "{} {}\n  {}",
                head("Accessibility:"),
                warn("not granted — dictation falls back to the clipboard (paste with Cmd+V)"),
                dim(&format!(
                    "Flip the Fono toggle once under System Settings → Privacy & \
                     Security → Accessibility, or run:\n  open \"{}\"",
                    fono_inject::ACCESSIBILITY_SETTINGS_URL
                ))
            )?;
        }
    }
    // Focused-window probe — powers the per-app context rules (terminal
    // shell vocabulary, code-editor hints, private-window history
    // suppression). Shown on every platform. Over a non-interactive
    // session (e.g. headless SSH, or a locked screen) there is no
    // foreground window and this reads "none detected".
    let focus = fono_inject::detect_focus().unwrap_or_default();
    if let Some(class) = focus.window_class.as_deref() {
        let profile = fono_inject::ContextClassifier::classify(
            focus.window_class.as_deref(),
            focus.window_title.as_deref(),
        );
        let label = profile.as_ref().map_or_else(
            || dim("no per-app rule — base prompt"),
            |p| ok(&format!("{} profile", p.name)),
        );
        writeln!(out, "{} {} ({})", head("Focus       :"), class, label)?;
    } else {
        writeln!(out, "{} {}", head("Focus       :"), dim("none detected — no foreground window"))?;
    }
    // Clipboard fallback — fono copies the cleaned text here when no
    // key-injection backend works, so the dictation is never lost.
    let mut clip_tools = Vec::new();
    for t in ["wl-copy", "xclip", "xsel"] {
        if which_in_path(t) {
            clip_tools.push(t);
        }
    }
    if clip_tools.is_empty() {
        writeln!(out, "{} {}", head("Clipboard   :"), ok("native (arboard)"))?;
    } else {
        writeln!(
            out,
            "{} {} {}",
            head("Clipboard   :"),
            clip_tools.join(", "),
            dim("(native arboard preferred)")
        )?;
    }
    // Probe for a clipboard manager. We check both the ICCCM
    // `CLIPBOARD_MANAGER` selection owner *and* the running process
    // list (clipit, parcellite, xfce4-clipman, klipper, copyq, gpaste,
    // greenclip, diodon, clipmenud, cliphist) because not every
    // manager implements the ICCCM handoff — clipit, for example, is
    // a polling manager that watches the CLIPBOARD selection on a
    // timer. This is an X11/Wayland concept only: `detect_clipboard_manager`
    // returns `None` on macOS and Windows (their OS clipboards have no
    // such handoff/manager ecosystem), so the whole probe — and its
    // X11-specific "typed via XTEST" guidance — is compiled on Linux only.
    #[cfg(target_os = "linux")]
    {
        use fono_inject::ClipboardManager;
        match fono_inject::detect_clipboard_manager() {
            ClipboardManager::Icccm => writeln!(
                out,
                "{} {}",
                head("Clip manager:"),
                ok("present (owns CLIPBOARD_MANAGER selection)")
            )?,
            ClipboardManager::Polling(name) => writeln!(
                out,
                "{} {}",
                head("Clip manager:"),
                ok(&format!("present ({name}, polling — no CLIPBOARD_MANAGER selection)"))
            )?,
            ClipboardManager::None => writeln!(
                out,
                "{} {}\n  {}",
                head("Clip manager:"),
                dim("none detected"),
                dim("Typing is unaffected — fono types directly via XTEST and never \
                     touches the clipboard. The optional `also_copy_to_clipboard` path \
                     keeps working while fono runs because the daemon holds one \
                     persistent arboard handle. Clipboard contents are lost when fono \
                     exits unless a manager (clipit / parcellite / xfce4-clipman / \
                     klipper / copyq / gpaste / greenclip) is running.")
            )?,
        }
    }
    writeln!(
        out,
        "{} {} ({})",
        head("IPC socket  :"),
        paths.ipc_socket().display(),
        if paths.ipc_socket().exists() { ok("exists") } else { warn("absent") }
    )?;

    // Overlay backend probe. Uses `fono_overlay::backend::probe_selection`
    // which only reads env vars + walks the candidate list — it does
    // not actually connect to Wayland or open a window, so doctor stays
    // cheap and side-effect-free.
    {
        use fono_overlay::{BackendCapabilities, BackendId};
        let (id, reason) = fono_overlay::backend::probe_selection();
        let caps = match id {
            BackendId::WlrLayerShell
            | BackendId::X11OverrideRedirect
            | BackendId::MacPanel
            | BackendId::Win32LayeredToolWindow => BackendCapabilities {
                transparency: true,
                client_positioning: true,
                focus_passthrough: true,
                click_passthrough: true,
            },
            BackendId::Noop => BackendCapabilities {
                transparency: false,
                client_positioning: false,
                focus_passthrough: true,
                click_passthrough: true,
            },
        };
        writeln!(out, "{} {} ({}) — {reason}", head("Overlay     :"), id.as_str(), caps.summary())?;
        // Wayland session without layer-shell and without Xwayland
        // — we have no graphical overlay path. Tell the user how
        // to fix it.
        if matches!(id, BackendId::Noop)
            && std::env::var_os("WAYLAND_DISPLAY").is_some()
            && std::env::var_os("DISPLAY").is_none()
        {
            writeln!(
                out,
                "  {}",
                warn(
                    "hint: Wayland session without Xwayland or layer-shell. Install \
                     your distro's xwayland package (e.g. `sudo apt install xwayland`) \
                     to enable the overlay."
                )
            )?;
        }
    }

    // ----------------------------------------------------------------
    // Screen capture section
    // ----------------------------------------------------------------
    {
        use fono_core::screen_capture::{GrabberProbe, RungKind};
        let probe = GrabberProbe::detect();

        writeln!(out, "{}", head("Screen capture:"))?;
        writeln!(out, "  Session type : {}", probe.session_label())?;

        let auto_rungs = probe.auto_rungs();
        if auto_rungs.is_empty() {
            writeln!(
                out,
                "  Active (auto): {}",
                bad("none — install scrot (X11) or grim+slurp (Wayland)")
            )?;
        } else {
            let auto_display: Vec<String> = auto_rungs
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let name = r.display_name();
                    if i == 0 {
                        format!("{} {}", ok(&format!("{name} ✓")), dim("(active)"))
                    } else if r == &RungKind::Portal || which_in_path(r.binary()) {
                        format!("{name} {}", ok("✓"))
                    } else {
                        format!("{name} {}", dim("[missing]"))
                    }
                })
                .collect();
            writeln!(out, "  Auto rungs   : {}", auto_display.join("  "))?;
            writeln!(out, "  Active (auto): {}", ok(auto_rungs[0].display_name()))?;
        }

        let sel_rungs = probe.select_rungs();
        if sel_rungs.is_empty() {
            writeln!(
                out,
                "  Active (sel.): {}",
                bad("none — install grim+slurp (Wayland) or scrot -s (X11)")
            )?;
        } else {
            let sel_display: Vec<String> = sel_rungs
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let name = r.display_name();
                    if i == 0 {
                        format!("{} {}", ok(&format!("{name} ✓")), dim("(active)"))
                    } else if r == &RungKind::Portal || which_in_path(r.binary()) {
                        format!("{name} {}", ok("✓"))
                    } else {
                        format!("{name} {}", dim("[missing]"))
                    }
                })
                .collect();
            writeln!(out, "  Select rungs : {}", sel_display.join("  "))?;
            writeln!(out, "  Active (sel.): {}", ok(sel_rungs[0].display_name()))?;
        }
        writeln!(out)?;
    }

    // ----------------------------------------------------------------
    // Wake word — always-on "hey fono" activation (Phase J of
    // plans/2026-06-23-wake-word-openwakeword-v2.md). Honest field
    // reporting: enabled?, the detector backend that WOULD run, each
    // phrase's target + license badge + cache state, the clean default
    // model's cache state, NonCommercial consent, and the loud Wyoming
    // client-direction privacy warning. Cheap + side-effect free: it only
    // reads config and stats files (no network, no daemon, no model load).
    // ----------------------------------------------------------------
    if let Some(c) = cfg.as_ref() {
        use fono_audio::wake_registry;
        use fono_core::config::{WakeTarget, WakeWyoming};
        let w = &c.wakeword;
        writeln!(out, "{}", head("Wake word:"))?;
        if w.enabled {
            writeln!(out, "  enabled        : {}", ok("true"))?;

            // Which detector backend would actually run. Reuses the registry's
            // path resolution (no duplicated filename logic) and mirrors
            // `wake::build_detector` / `try_load_onnx`: ONNX only when the
            // feature is compiled AND every configured phrase's model files
            // resolve on disk; otherwise the energy stub.
            let any_phrases = !w.phrases.is_empty();
            let inputs = WakeBackendInputs {
                onnx_compiled: cfg!(feature = "wakeword-onnx"),
                graphs_present: wake_graphs_present(&paths.cache_dir),
                all_classifiers_present: any_phrases
                    && w.phrases
                        .iter()
                        .all(|p| wake_classifier_path(&p.model, &paths.cache_dir).exists()),
            };
            match wake_backend(&inputs) {
                WakeBackend::Onnx => {
                    writeln!(out, "  backend        : {} (openWakeWord ONNX)", ok("ONNX"))?;
                }
                WakeBackend::EnergyStub => {
                    let why = if !inputs.onnx_compiled {
                        "wakeword-onnx feature not compiled into this build"
                    } else if !any_phrases {
                        "no phrases configured"
                    } else if !inputs.graphs_present {
                        "shared melspec/embedding graphs not yet downloaded"
                    } else {
                        "one or more phrase classifiers not yet downloaded"
                    };
                    writeln!(out, "  backend        : {} ({why})", warn("energy stub"))?;
                }
            }
            if w.phrases.is_empty() {
                writeln!(out, "  phrases        : {}", warn("none configured"))?;
            } else {
                writeln!(out, "  phrases:")?;
                for p in &w.phrases {
                    let entry = wake_registry::get(&p.model);
                    let license = match entry {
                        Some(e) if e.is_noncommercial() => warn(e.license.spdx()),
                        Some(e) => dim(e.license.spdx()),
                        None => dim("custom/unknown"),
                    };
                    let cache_state = if wake_classifier_path(&p.model, &paths.cache_dir).exists() {
                        ok("cached")
                    } else {
                        dim("not downloaded")
                    };
                    let target = match p.target {
                        WakeTarget::Dictation => "dictation",
                        WakeTarget::Assistant => "assistant",
                    };
                    writeln!(
                        out,
                        "    {} {:<14} target={target:<10} sens={:.2}  {license}  {cache_state}",
                        star(false),
                        p.model,
                        p.sensitivity,
                    )?;
                }
            }

            // Runtime default model (the phrase served when none is
            // configured — e.g. the auto-served Wyoming wake path). TEMPORARY:
            // points at the community `hey_jarvis` until the clean Apache
            // `hey_fono` artifact is trained + pinned.
            let default_id = wake_registry::DEFAULT_WAKE_MODEL;
            let default_cached = wake_registry::resolved_paths(default_id, &paths.cache_dir)
                .is_some_and(|r| r.classifier.exists());
            let default_badge = match wake_registry::get(default_id) {
                Some(e) if e.is_noncommercial() => warn(e.license.spdx()),
                Some(e) => dim(e.license.spdx()),
                None => dim("unknown"),
            };
            writeln!(
                out,
                "  default model  : {default_id}  {default_badge}  {}",
                if default_cached { ok("cached") } else { dim("not yet downloaded") }
            )?;

            // NonCommercial community models: no consent is stored — Fono
            // notifies the user at download time and proceeds. Flag if any
            // configured phrase resolves to a CC-BY-NC-SA model.
            let nc_any = w.phrases.iter().any(|p| {
                wake_registry::get(&p.model)
                    .is_some_and(wake_registry::WakeModelEntry::is_noncommercial)
            });
            if nc_any {
                writeln!(
                    out,
                    "  community model: {}",
                    warn(
                        "NonCommercial (CC-BY-NC-SA-4.0) — you are notified at download; \
                         non-commercial use only"
                    )
                )?;
            }

            // Wyoming wake serving is automatic, mirroring STT/TTS: whenever
            // the LAN server is enabled and this build can do wake detection,
            // Fono advertises + serves its LOCAL detector (audio stays on the
            // machine). The opt-in CLIENT direction (`[wakeword].wyoming` with
            // a uri) is the only path that leaks idle mic audio off-box, so it
            // gets the loud shared warning.
            if cfg!(feature = "wakeword-onnx") {
                if c.server.wyoming.enabled {
                    writeln!(
                        out,
                        "  Wyoming        : {} — local wake Detection served automatically over \
                         the LAN server; audio stays on this machine",
                        ok("served")
                    )?;
                } else {
                    writeln!(
                        out,
                        "  Wyoming        : {} (enable `[server.wyoming]` to share wake on the LAN)",
                        dim("available")
                    )?;
                }
            } else {
                writeln!(
                    out,
                    "  Wyoming        : {}",
                    dim("unavailable (wakeword-onnx not compiled into this build)")
                )?;
            }
            if w.wyoming.as_ref().is_some_and(WakeWyoming::is_client) {
                writeln!(
                    out,
                    "  Wyoming client : {} (idle mic audio leaves the machine)",
                    bad("CLIENT — opt-in")
                )?;
                writeln!(out, "  {}", bad(WakeWyoming::CLIENT_PRIVACY_WARNING))?;
            }
        } else {
            writeln!(
                out,
                "  enabled        : {} {}",
                dim("false"),
                dim("(default — no idle mic stream is opened; enable with \
                     `[wakeword].enabled = true`)")
            )?;
        }
        writeln!(out)?;
    }
    // ----------------------------------------------------------------
    // Coding agents — MCP server status
    // ----------------------------------------------------------------
    if let Some(c) = cfg.as_ref() {
        writeln!(out, "{}", head("Coding agents (MCP server):"))?;
        if c.mcp.enabled {
            writeln!(out, "  mcp.enabled          : {}", ok("true"))?;
        } else {
            writeln!(
                out,
                "  mcp.enabled          : {} (enable with `fono use mcp-server on`)",
                warn("false")
            )?;
        }
        writeln!(out, "  transport            : stdio")?;
        writeln!(
            out,
            "  tools available      : fono.speak, fono.listen, fono.confirm, fono.screen"
        )?;
        writeln!(
            out,
            "  voice preset         : assets/agent-presets/voice.md (see docs/coding-agents.md)"
        )?;
        writeln!(out)?;
    }

    writeln!(out)?;
    writeln!(out, "{} ({}):", head("Log tail"), paths.log_file().display())?;
    match tail_log(&paths.log_file(), 10) {
        Ok(lines) if lines.is_empty() => writeln!(out, "  {}", dim("(log is empty)"))?,
        Ok(lines) => {
            for line in lines {
                writeln!(out, "  {line}")?;
            }
        }
        Err(e) => writeln!(out, "  {} {e}", bad("(cannot read log:"))?,
    }

    Ok(out)
}

/// The detector backend `fono doctor` reports the daemon *would* build for an
/// enabled `[wakeword]`. Pure mirror of `crate::wake::build_detector`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakeBackend {
    /// Real openWakeWord three-stage ONNX detector.
    Onnx,
    /// Energy/VAD fallback stub.
    EnergyStub,
}

/// Inputs to [`wake_backend`] — the conditions the daemon's `build_detector`
/// keys off, grouped to keep the decision pure and the signature readable.
struct WakeBackendInputs {
    /// The `wakeword-onnx` feature is compiled into this build.
    onnx_compiled: bool,
    /// The shared melspectrogram + embedding graphs are present on disk.
    graphs_present: bool,
    /// Every configured phrase's classifier file is present on disk (which
    /// also implies at least one phrase is configured).
    all_classifiers_present: bool,
}

/// Decide which wake detector backend would run, purely from the inputs the
/// daemon's `build_detector` keys off. ONNX requires the feature compiled, the
/// shared graphs present, and every classifier present (the last already
/// implies a phrase is configured); anything missing falls back to the energy
/// stub. Kept pure + tested so doctor and the real loader can never disagree.
fn wake_backend(i: &WakeBackendInputs) -> WakeBackend {
    if i.onnx_compiled && i.graphs_present && i.all_classifiers_present {
        WakeBackend::Onnx
    } else {
        WakeBackend::EnergyStub
    }
}

/// Resolve where a phrase's classifier file lives in the wake-word cache,
/// reusing the registry's path resolution for known model ids and falling back
/// to the `<id>.ort` convention the ONNX loader uses for custom phrases. No
/// duplicated filename literals — the registry stays the single source.
fn wake_classifier_path(model: &str, cache_dir: &std::path::Path) -> std::path::PathBuf {
    match fono_audio::wake_registry::resolved_paths(model, cache_dir) {
        Some(r) => r.classifier,
        None => fono_audio::wake_registry::wakeword_dir(cache_dir).join(format!("{model}.ort")),
    }
}

/// Whether the shared melspectrogram + embedding graphs are present on disk
/// (both required before ONNX can load). Uses the registry's resolved paths.
fn wake_graphs_present(cache_dir: &std::path::Path) -> bool {
    fono_audio::wake_registry::resolved_paths("hey_fono", cache_dir)
        .is_some_and(|r| r.melspec.exists() && r.embedding.exists())
}

/// Read up to the last `n` lines of `path`. Preserves embedded ANSI.
fn tail_log(path: &std::path::Path, n: usize) -> std::io::Result<Vec<String>> {
    let data = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = data.lines().collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].iter().map(|s| (*s).to_string()).collect())
}

/// `fono doctor -f`: stream the log file forever via `tail -f`.
/// ANSI escapes pass through to the terminal unchanged.
pub async fn follow_log(paths: &Paths) -> Result<()> {
    let path = paths.log_file();
    println!();
    println!("Following {} (Ctrl-C to stop):", path.display());
    let status = tokio::process::Command::new("tail")
        .arg("-n")
        .arg("0")
        .arg("-F")
        .arg(&path)
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("tail exited with {status}");
    }
    Ok(())
}

/// Best-effort PATH lookup; mirrors fono-inject's `which` so doctor
/// reports the same set of clipboard tools the real fallback will try.
fn which_in_path(tool: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|p| p.join(tool).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ONNX is reported only when every precondition holds; missing any one
    /// of {feature, graphs, classifiers} falls back to the stub — the exact
    /// gate `wake::build_detector` applies.
    #[test]
    fn wake_backend_requires_all_preconditions() {
        let decide = |onnx, graphs, classifiers| {
            wake_backend(&WakeBackendInputs {
                onnx_compiled: onnx,
                graphs_present: graphs,
                all_classifiers_present: classifiers,
            })
        };
        assert_eq!(decide(true, true, true), WakeBackend::Onnx);
        // Any single missing input drops to the stub.
        assert_eq!(decide(false, true, true), WakeBackend::EnergyStub);
        assert_eq!(decide(true, false, true), WakeBackend::EnergyStub);
        assert_eq!(decide(true, true, false), WakeBackend::EnergyStub);
        assert_eq!(decide(false, false, false), WakeBackend::EnergyStub);
    }

    /// A registry model resolves to its registered classifier basename
    /// (`<id>.ort`); an unknown id falls back to the same `<id>.ort`
    /// convention. Both land under the wake-word cache dir (no duplicated
    /// filename literals).
    #[test]
    fn wake_classifier_path_uses_registry_then_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = fono_audio::wake_registry::wakeword_dir(tmp.path());
        // Known default model: registry-resolved basename.
        assert_eq!(wake_classifier_path("hey_fono", tmp.path()), dir.join("hey_fono.ort"));
        // Known community model: `.ort` conversion on the mirror (the loader
        // contract is always `<id>.ort`).
        assert_eq!(wake_classifier_path("hey_jarvis", tmp.path()), dir.join("hey_jarvis.ort"));
        // Unknown / custom phrase: `<id>.ort` fallback.
        assert_eq!(
            wake_classifier_path("my_custom_phrase", tmp.path()),
            dir.join("my_custom_phrase.ort")
        );
    }

    /// Graphs are absent on a fresh cache and present only when both `.ort`
    /// files exist.
    #[test]
    fn wake_graphs_present_needs_both_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!wake_graphs_present(tmp.path()), "fresh cache has no graphs");
        let dir = fono_audio::wake_registry::wakeword_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("melspectrogram.ort"), b"x").unwrap();
        assert!(!wake_graphs_present(tmp.path()), "melspec alone is not enough");
        std::fs::write(dir.join("embedding.ort"), b"x").unwrap();
        assert!(wake_graphs_present(tmp.path()), "both graphs present");
    }
}
