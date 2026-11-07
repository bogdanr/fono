// SPDX-License-Identifier: GPL-3.0-only
//! Ensure the local-model files referenced by `config.toml` are present
//! on disk, downloading them on demand.
//!
//! Called from:
//! * the daemon startup path (before the IPC loop begins),
//! * the wizard after a fresh `Setup`,
//! * the tray's STT/LLM switcher (when the user picks `Local` and the
//!   weights are missing — see [`ensure_local_stt`] / [`ensure_local_llm`]).
//!
//! Both whisper STT and llama-cpp LLM are covered; the LLM auto-download
//! resolves the model name in `config.polish.local.model` against the
//! `fono-polish` registry and writes to `<polish_models_dir>/<name>.gguf`,
//! mirroring the path resolver in `fono-polish::factory`.
//!
//! Whisper STT additionally honours `[stt.local].quantization` — the
//! user-facing model name (e.g. `small`) and the quantization
//! preference (e.g. `auto`, `q8_0`, `fp16`) together resolve to a
//! single GGML file (e.g. `ggml-small-q5_1.bin`) via
//! [`fono_stt::ModelRegistry`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fono_core::config::{Config, LlmBackend, SttBackend};
use fono_core::Paths;
use fono_stt::{ModelInfo, ModelRegistry, Quantization, QuantizationPref};
use tracing::{debug, info, warn};

/// Outcome of an `ensure_*` call, for callers that want to surface a
/// notification only on the first download (and stay quiet on subsequent
/// daemon starts when the model is already cached).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// The model file was already on disk; nothing was downloaded.
    AlreadyPresent,
    /// The model file was missing and a download succeeded.
    Downloaded,
    /// The configured model name is not in the registry — nothing we
    /// can auto-download. The caller should leave the existing file (if
    /// any) alone.
    Unknown,
}

/// How long we watch the transfer, once bytes are actually moving, before
/// deciding whether to say anything — so both the "will this take a while?"
/// verdict and the "done in about N minutes" figure come from the user's real
/// download speed rather than a guess.
///
/// Measured against the real mirrors, three seconds of *flowing* transfer
/// lands within about 10% of the true finish time, which is well inside the
/// precision of a "~4 min left" message. The window is timed from the first
/// byte rather than from the call, because that is what makes three seconds
/// enough — see [`wait_for_first_byte`].
const RATE_SAMPLE: Duration = Duration::from_secs(3);

/// How long to wait for the transfer to actually start before giving up on
/// measuring it.
///
/// DNS, TLS and the redirect to the CDN cost about a second before a single
/// byte lands, and that second is dead time at zero bytes. Averaging it into
/// the rate is what made a naive three-second sample overestimate the finish
/// time by ~35%; anchoring the window to the first byte instead brings the
/// error to roughly zero. If nothing arrives within this budget the download
/// really is stalled or offline, which is its own answer.
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(20);

/// How often to poll for that first byte. Fine enough not to add meaningful
/// lag to the anchor, coarse enough to be free.
const FIRST_BYTE_POLL: Duration = Duration::from_millis(100);

/// Only downloads projected to run longer than this are worth interrupting
/// the user for. A first run may fetch three models — two small ones that
/// land in seconds and one large one — and popping a toast for the quick
/// ones is just noise; by the time the user's eyes reach the notification
/// the download it describes is already finished.
const NOTIFY_THRESHOLD: Duration = Duration::from_secs(15);

/// Set synchronously when a download starts, so [`take_download_notice`]
/// knows a verdict is coming and waits for it instead of racing ahead.
static DOWNLOAD_PENDING: AtomicBool = AtomicBool::new(false);

/// Set only when the "downloading" notification was actually shown, which is
/// what makes the matching "ready" notification owed.
static DOWNLOAD_ANNOUNCED: AtomicBool = AtomicBool::new(false);

/// Set once the notify-or-stay-silent decision has been made (and the
/// notification, if any, sent), so a waiter that arrives late can tell
/// "decided, stay silent" apart from "not decided yet".
static DOWNLOAD_DECIDED: AtomicBool = AtomicBool::new(false);

/// Wakes anyone waiting on that decision. See [`take_download_notice`].
static DOWNLOAD_DECIDED_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Tell the user, in a desktop notification, that a model download is under
/// way — including roughly how long it will take — but only if it is going
/// to take long enough to be worth saying.
///
/// Fono is often started from a systemd user unit or an autostart entry,
/// where nobody is watching the console, so a first-run download of a few
/// hundred megabytes looks exactly like a hang. `summary` and `body`
/// describe the models; `total_mb` is their combined download size and is
/// what makes the time estimate possible.
///
/// Both the estimate and the decision need a transfer rate, and the only
/// honest way to get one is to measure it: we wait for the transfer to
/// actually start, watch it for [`RATE_SAMPLE`], and project the finish time
/// from the bytes `fono-download` wrote in that window. A download that will
/// be over within [`NOTIFY_THRESHOLD`] is left unmentioned — no "downloading"
/// toast, and no "ready" toast either. Fire-and-forget: it runs on its own
/// task and can neither block nor fail the download it describes.
fn notify_download_started(summary: String, body: String, total_mb: u32) {
    DOWNLOAD_PENDING.store(true, Ordering::Relaxed);
    let start_bytes = fono_download::bytes_written();
    tokio::spawn(async move {
        let overall = Instant::now();
        // Anchor the measurement to the first byte, not to this call: the
        // connection setup in between is dead time that would otherwise be
        // averaged into the rate and inflate the estimate.
        let anchor_bytes = wait_for_first_byte(start_bytes).await;
        let sampled_from = Instant::now();
        let (done, per_sec) = match anchor_bytes {
            Some(anchor) => {
                tokio::time::sleep(RATE_SAMPLE).await;
                let now = fono_download::bytes_written();
                let in_window = now.saturating_sub(anchor);
                let rate = in_window as f64 / sampled_from.elapsed().as_secs_f64().max(0.001);
                (now.saturating_sub(start_bytes), rate)
            }
            // Nothing ever started flowing; report it as such and let the
            // no-estimate branch below speak up.
            None => (0, 0.0),
        };
        let remaining = remaining_secs(total_mb, done, per_sec);
        // A measured, short finish time is the one case we stay quiet. No
        // measurement at all (nothing moved: stalled, offline, or a mirror
        // still handshaking) is precisely when the run looks like a hang, so
        // that speaks up.
        if remaining.is_some_and(|secs| {
            overall.elapsed().as_secs_f64() + secs < NOTIFY_THRESHOLD.as_secs_f64()
        }) {
            debug!("model download will finish shortly; skipping the notification");
        } else {
            let body = match remaining.map(|secs| eta_text(secs, per_sec)) {
                Some(eta) => format!("{body} — {eta}"),
                None => body,
            };
            fono_core::notify::send(
                &summary,
                &body,
                "emblem-downloads",
                10_000,
                fono_core::notify::Urgency::Normal,
            );
            DOWNLOAD_ANNOUNCED.store(true, Ordering::Relaxed);
        }
        DOWNLOAD_DECIDED.store(true, Ordering::Relaxed);
        DOWNLOAD_DECIDED_WAKE.notify_waiters();
    });
}

/// Block until the transfer has actually moved past `start_bytes`, and return
/// the byte count at that moment — the anchor the rate window is measured
/// from. `None` if nothing arrives within [`FIRST_BYTE_TIMEOUT`].
async fn wait_for_first_byte(start_bytes: u64) -> Option<u64> {
    let deadline = Instant::now() + FIRST_BYTE_TIMEOUT;
    loop {
        let now = fono_download::bytes_written();
        if now > start_bytes {
            return Some(now);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(FIRST_BYTE_POLL).await;
    }
}

/// Claim the pending "models are ready" notification, if one is owed.
///
/// Returns `true` only when this start both had to download something *and*
/// told the user about it, so the "ready" notification never arrives on its
/// own: a start with every model already cached, or one whose download was
/// too quick to be worth mentioning, stays silent. Whoever claims it (the
/// startup warmup coordinator, once the weights are actually loaded into
/// memory) owns sending that notification.
///
/// Waits for the notify-or-stay-silent verdict first, since it is held back
/// briefly to measure the transfer rate and everything else can finish inside
/// that window.
pub async fn take_download_notice() -> bool {
    if !DOWNLOAD_PENDING.swap(false, Ordering::Relaxed) {
        return false;
    }
    let wake = DOWNLOAD_DECIDED_WAKE.notified();
    // Check only after subscribing, so a verdict landing in between still
    // wakes us rather than leaving us to time out. The budget covers the
    // worst case the verdict can take: waiting out the first byte, then the
    // sample window itself.
    if !DOWNLOAD_DECIDED.load(Ordering::Relaxed) {
        let _ = tokio::time::timeout(FIRST_BYTE_TIMEOUT + RATE_SAMPLE * 2, wake).await;
    }
    DOWNLOAD_DECIDED.store(false, Ordering::Relaxed);
    DOWNLOAD_ANNOUNCED.swap(false, Ordering::Relaxed)
}

/// Seconds left on a download of `total_mb` given the bytes already in and
/// the measured rate. `None` when nothing moved during the sample window
/// (offline, stalled, or a mirror still handshaking) — the caller then has no
/// basis to either estimate or stay silent, and errs towards speaking up.
fn remaining_secs(total_mb: u32, done_bytes: u64, bytes_per_sec: f64) -> Option<f64> {
    if bytes_per_sec < 1_000.0 {
        return None;
    }
    let total = u64::from(total_mb) * 1_000_000;
    Some(total.saturating_sub(done_bytes) as f64 / bytes_per_sec)
}

/// Render "about 4 min left (5.2 MB/s)" from a remaining time and a rate.
fn eta_text(remaining_secs: f64, bytes_per_sec: f64) -> String {
    let rate = bytes_per_sec / 1_000_000.0;
    if remaining_secs < 60.0 {
        format!("less than a minute left ({rate:.1} MB/s)")
    } else {
        format!("about {} min left ({rate:.1} MB/s)", (remaining_secs / 60.0).ceil() as u64)
    }
}

/// One model the current config needs that is not on disk yet: a
/// user-facing description and its approximate download size in MB.
struct Pending {
    what: String,
    mb: u32,
}

/// Everything the current config references that still has to be downloaded.
/// Empty on every start after the first, which is what keeps the startup
/// notification from becoming noise.
fn pending_downloads(paths: &Paths, config: &Config) -> Vec<Pending> {
    let mut pending = Vec::new();
    let local_models = fono_core::hwcheck::local_models_unsupported().is_none();
    if local_models && config.stt.backend == SttBackend::Local {
        let name = &config.stt.local.model;
        let quant = &config.stt.local.quantization;
        let missing = resolve_local_stt(name, quant)
            .ok()
            .flatten()
            .is_some_and(|(info, q)| !whisper_dest(paths, info.name, q).exists());
        if missing {
            let mb = local_stt_size_mb(name, quant).unwrap_or(0);
            pending.push(Pending { what: format!("speech model {name}"), mb });
        }
    }
    // The cleanup and assistant LLMs are often the same GGUF; list it once.
    // Both are gated on `enabled`: a role switched off must never pull a
    // multi-gigabyte model down for something that will not run.
    let mut llms: Vec<(&str, &String)> = Vec::new();
    if local_models && config.polish.enabled && config.polish.backend == LlmBackend::Local {
        llms.push(("cleanup model", &config.polish.local.model));
    }
    if local_models && config.assistant.enabled && config.assistant.backend == LlmBackend::Local {
        llms.push(("assistant model", &config.assistant.local.model));
    }
    let mut seen: Vec<String> = Vec::new();
    for (role, name) in llms {
        let stem = fono_polish::LocalLlmRegistry::resolve_filename_stem(name);
        if seen.contains(&stem) || paths.polish_models_dir().join(format!("{stem}.gguf")).exists() {
            continue;
        }
        seen.push(stem);
        let mb = local_llm_size_mb(name).unwrap_or(0);
        pending.push(Pending { what: format!("{role} {name}"), mb });
    }
    #[cfg(feature = "tts-local")]
    if config.tts.backend == fono_core::config::TtsBackend::Local {
        if let Some(mb) = local_tts_pending_mb(paths, config) {
            pending.push(Pending { what: "text-to-speech voice".to_string(), mb });
        }
    }
    pending
}

/// Whether any model the config needs still has to be downloaded.
#[must_use]
pub fn downloads_pending(paths: &Paths, config: &Config) -> bool {
    !pending_downloads(paths, config).is_empty()
}

/// Whether the local speech-to-text model is one of those downloads, so a
/// missing model means "not here yet" rather than a broken setup.
#[must_use]
pub fn local_stt_pending(paths: &Paths, config: &Config) -> bool {
    pending_downloads(paths, config).iter().any(|p| p.what.starts_with("speech model"))
}

/// Whether the local text-to-speech voice is one of those downloads.
#[must_use]
pub fn local_tts_pending(paths: &Paths, config: &Config) -> bool {
    pending_downloads(paths, config).iter().any(|p| p.what == "text-to-speech voice")
}

/// Announce the first-run model download to the desktop, if there is one
/// worth announcing.
///
/// Called from [`ensure_models`], i.e. on the daemon-startup path, precisely
/// because that path is usually invisible: launched from a systemd user unit
/// or a desktop autostart entry, Fono would otherwise spend several silent
/// minutes fetching weights with no hint that anything is happening. All the
/// missing models are described in a single notification rather than one per
/// model, and [`notify_download_started`] drops even that when the whole
/// batch is going to be over in seconds.
fn announce_pending_downloads(paths: &Paths, config: &Config) {
    let pending = pending_downloads(paths, config);
    if pending.is_empty() {
        return;
    }
    let total_mb: u32 = pending.iter().map(|p| p.mb).sum();
    let summary = if pending.len() == 1 {
        "Fono — downloading a model".to_string()
    } else {
        format!("Fono — downloading {} models", pending.len())
    };
    let what = pending.iter().map(|p| p.what.as_str()).collect::<Vec<_>>().join(", ");
    let body = if total_mb > 0 { format!("{what} ({total_mb} MB)") } else { what };
    info!("first-run download: {body}");
    notify_download_started(summary, body, total_mb);
}

/// Resolve a `(name, quantization_pref)` pair from config into the
/// registry entry + concrete quantization + on-disk filename. Returns
/// `Ok(None)` when the model name is unknown — callers translate that
/// into a warning and `EnsureOutcome::Unknown`. Returns `Err` when the
/// model exists but the user pinned a quantization the registry does
/// not ship (e.g. `model = "tiny"` with `quantization = "fp16"` — `tiny`
/// ships only `q5_1`).
pub fn resolve_local_stt(
    name: &str,
    quantization: &str,
) -> Result<Option<(&'static ModelInfo, Quantization)>> {
    let Some(info) = ModelRegistry::get(name) else {
        return Ok(None);
    };
    let pref = QuantizationPref::parse(quantization).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid `[stt.local].quantization = {quantization:?}` — \
             expected `auto`, `fp16`, `q5_1`, or `q8_0`"
        )
    })?;
    let q = ModelRegistry::resolve_quantization(info, pref).map_err(anyhow::Error::msg)?;
    Ok(Some((info, q)))
}

/// Path the whisper GGML file should live at, given the resolved
/// `(name, quantization)`. Pure naming function — does not touch disk.
#[must_use]
pub fn whisper_dest(paths: &Paths, name: &str, quant: Quantization) -> PathBuf {
    paths.whisper_models_dir().join(ModelRegistry::filename(name, quant))
}

/// Check every model the current config references and download any that
/// are missing. Individual failures log a warning but do not abort the
/// daemon; this is invoked unconditionally from startup and we never
/// want a transient HTTP failure to keep the daemon from coming up.
///
/// When anything does have to be fetched, and the fetch is slow enough to be
/// worth mentioning, the user gets a desktop notification about it (with a
/// time estimate) — see [`announce_pending_downloads`] — because this runs on
/// the invisible startup path where a multi-minute download would otherwise
/// look like a hang. Starts where every model is already cached, or where the
/// download finishes in seconds, stay silent.
pub async fn ensure_models(paths: &Paths, config: &Config) -> Result<()> {
    announce_pending_downloads(paths, config);
    // A CPU that can't run local models never loads them, so don't fetch them.
    let local_models = fono_core::hwcheck::local_models_unsupported().is_none();
    if local_models && config.stt.backend == SttBackend::Local {
        let r =
            ensure_local_stt(paths, &config.stt.local.model, &config.stt.local.quantization).await;
        if let Err(e) = r {
            warn!("auto-download of whisper model failed: {e:#}");
        }
    }
    if local_models && config.polish.enabled && config.polish.backend == LlmBackend::Local {
        // Boxed: the LLM-ensure future may carry registry/download state
        // large enough to trip stack-frame lints when inlined here.
        if let Err(e) = Box::pin(ensure_local_llm(paths, &config.polish.local.model)).await {
            warn!("auto-download of LLM model failed: {e:#}");
        }
    }
    if local_models && config.assistant.enabled && config.assistant.backend == LlmBackend::Local {
        // Boxed for the same reason as the polish LLM ensure above; local
        // assistant and cleanup share the registry/download path.
        if let Err(e) = Box::pin(ensure_local_llm(paths, &config.assistant.local.model)).await {
            warn!("auto-download of assistant LLM model failed: {e:#}");
        }
    }
    #[cfg(feature = "tts-local")]
    if config.tts.backend == fono_core::config::TtsBackend::Local {
        // Boxed: the voice-ensure future (catalog Voice + download buffers)
        // is large enough to trip `clippy::large_futures` if inlined here.
        if let Err(e) = Box::pin(ensure_local_tts(paths, config)).await {
            warn!("auto-download of local TTS voice failed: {e:#}");
        }
    }
    Ok(())
}

/// Ensure the local TTS voices (`.ort` model + `.onnx.json` config) are
/// cached under `voices_dir`, downloading and verifying them from the
/// `fono-voice` mirror when missing. Resolution mirrors
/// `fono_tts::factory`: an explicit `[tts.local].voice` pins a single
/// voice; otherwise one voice per configured language is ensured so the
/// language router can switch voices offline (a Romanian reply gets the
/// Romanian voice, an English reply the English one). Languages without a
/// catalog voice are skipped with a warning rather than failing the lot.
#[cfg(feature = "tts-local")]
pub async fn ensure_local_tts(paths: &Paths, config: &Config) -> Result<EnsureOutcome> {
    ensure_local_tts_route(&paths.voices_dir(), &config.tts.local, &config.general.languages).await
}

/// Ensure the local TTS assets for an *already-resolved* route are on disk,
/// downloading them on demand. Unlike [`ensure_local_tts`] this takes the
/// concrete `[tts.local]` block and language list directly, so the
/// `/v1/audio/speech` handler can prepare a voice for a per-request engine
/// (e.g. testing Kokoro while the configured backend is Piper) without a
/// daemon restart. Resolution mirrors `fono_tts::factory`: an explicit
/// `voice` pins a single voice; otherwise one voice per language is ensured.
#[cfg(feature = "tts-local")]
pub async fn ensure_local_tts_route(
    voices_dir: &std::path::Path,
    local: &fono_core::config::TtsLocal,
    languages: &[String],
) -> Result<EnsureOutcome> {
    let base_url = &local.base_url;
    let base = (!base_url.is_empty()).then_some(base_url.as_str());
    // Supertonic is a single shared pack outside the per-language catalog, so
    // when the user pins that engine we ensure the pack instead of catalog
    // voices (mirroring the voice-ensure flow).
    if local.engine == fono_core::config::TtsLocalEngine::Supertonic {
        let dir = fono_tts::supertonic::supertonic_dir(voices_dir);
        let already = dir.join(fono_tts::supertonic::CONFIG.file).is_file();
        if !already {
            debug!("Supertonic voice pack missing; downloading from the fono-voice mirror");
        }
        fono_tts::supertonic::ensure_pack(voices_dir, base)
            .await
            .context("ensuring the Supertonic voice pack")?;
        info!("Supertonic voice pack ready");
        return Ok(if already { EnsureOutcome::AlreadyPresent } else { EnsureOutcome::Downloaded });
    }
    let voices = resolve_local_tts_voices(local, languages)?;
    let mut any_downloaded = false;
    for voice in &voices {
        let already = voices_dir.join(&voice.model.file).is_file()
            && voice.config.as_ref().is_none_or(|c| voices_dir.join(&c.file).is_file())
            && voice.style.as_ref().is_none_or(|s| voices_dir.join(&s.file).is_file());
        if !already {
            any_downloaded = true;
            debug!("local voice {:?} missing; downloading from the fono-voice mirror", voice.name);
        }
        fono_tts::voices::ensure_voice(voice, voices_dir, base)
            .await
            .with_context(|| format!("ensuring local voice {:?}", voice.name))?;
        if already {
            debug!("local voice ready: {}", voice.name);
        } else {
            info!("local voice installed: {}", voice.name);
        }
    }
    Ok(if any_downloaded { EnsureOutcome::Downloaded } else { EnsureOutcome::AlreadyPresent })
}

/// Resolve which catalog voices the local backend needs, mirroring
/// `fono_tts::factory`: an explicit `[tts.local].voice` pins one voice,
/// otherwise one voice per configured language (deduped) is chosen.
/// Languages without a catalog voice are skipped with a warning.
#[cfg(feature = "tts-local")]
fn resolve_local_tts_voices(
    local: &fono_core::config::TtsLocal,
    languages: &[String],
) -> Result<Vec<fono_tts::voices::Voice>> {
    let engine_filter = local.engine.catalog_filter();
    // Honour an explicit voice only when it belongs to the pinned engine (if
    // any); a cross-engine voice is ignored so the engine selection wins and
    // we download *that* engine's assets (mirrors `fono_tts::factory`).
    if !local.voice.is_empty() {
        match fono_tts::voices::by_name(&local.voice)? {
            Some(v) if engine_filter.is_none_or(|e| v.engine == e) => return Ok(vec![v]),
            Some(_) => {} // engine pin wins → fall through
            None if engine_filter.is_none() => {
                return Err(anyhow::anyhow!(
                    "[tts.local].voice = {:?} is not in the voice catalog",
                    local.voice
                ));
            }
            None => {} // unknown voice but an engine is pinned → fall through
        }
    }
    let mut langs: Vec<&str> = languages.iter().map(String::as_str).collect();
    if langs.is_empty() {
        langs.push("en");
    }
    let mut chosen: Vec<fono_tts::voices::Voice> = Vec::new();
    for lang in langs {
        let found = match engine_filter {
            Some(e) => fono_tts::voices::for_language_engine(lang, e)?,
            None => fono_tts::voices::for_language(lang)?,
        };
        match found {
            Some(v) if !chosen.iter().any(|c| c.name == v.name) => chosen.push(v),
            Some(_) => {} // a different language already mapped to this voice
            None => warn!(
                "no local TTS voice in the catalog for configured language {lang:?}; \
                 it will fall back to the primary voice"
            ),
        }
    }
    // Engine pinned but no per-language match (e.g. Kokoro is English-only and
    // no configured language is English): fall back to that engine's first
    // catalog voice so the test still has something to download and play.
    if chosen.is_empty() {
        if let Some(e) = engine_filter {
            if let Some(v) = fono_tts::voices::for_engine(e)?.into_iter().next() {
                chosen.push(v);
            }
        }
    }
    if chosen.is_empty() {
        let lang = languages.first().map_or("en", String::as_str);
        return Err(anyhow::anyhow!(
            "no local voice in the catalog for any configured language (e.g. {lang:?}); \
             set [tts.local].voice to a catalog voice id"
        ));
    }
    Ok(chosen)
}

/// Approximate total download size (MB) for the local TTS voices the
/// current config requires that are **not yet on disk**, or `None` when
/// every required voice is already cached (so callers can skip the
/// "downloading…" notification). Used by the tray switcher.
#[cfg(feature = "tts-local")]
#[must_use]
pub fn local_tts_pending_mb(paths: &Paths, config: &Config) -> Option<u32> {
    let voices_dir = paths.voices_dir();
    // Supertonic (the default) is a single shared pack outside the per-language
    // catalog, so size it by pack presence rather than catalog voices. The
    // pack has no per-asset size metadata; report its documented approximate
    // download (~140 MiB int8 pack) only when it is not yet cached.
    if config.tts.local.engine == fono_core::config::TtsLocalEngine::Supertonic {
        let dir = fono_tts::supertonic::supertonic_dir(&voices_dir);
        let present = dir.join(fono_tts::supertonic::CONFIG.file).is_file();
        return (!present).then_some(140);
    }
    let voices = resolve_local_tts_voices(&config.tts.local, &config.general.languages).ok()?;
    let pending: u64 = voices
        .iter()
        .filter(|v| {
            let present = voices_dir.join(&v.model.file).is_file()
                && v.config.as_ref().is_none_or(|c| voices_dir.join(&c.file).is_file())
                && v.style.as_ref().is_none_or(|s| voices_dir.join(&s.file).is_file());
            !present
        })
        .map(|v| {
            v.model.size
                + v.config.as_ref().map_or(0, |c| c.size)
                + v.style.as_ref().map_or(0, |s| s.size)
        })
        .sum();
    if pending == 0 {
        None
    } else {
        Some((pending / 1_000_000).max(1) as u32)
    }
}

/// Ensure the named whisper model (at the configured quantization) is
/// on disk. Returns the outcome so the tray switcher can show
/// "downloading…" / "ready" notifications only when work was actually
/// done.
pub async fn ensure_local_stt(
    paths: &Paths,
    model_name: &str,
    quantization: &str,
) -> Result<EnsureOutcome> {
    let resolved = resolve_local_stt(model_name, quantization)?;
    let Some((info, quant)) = resolved else {
        warn!(
            "config references unknown whisper model {model_name:?} — run \
             `fono models list` to see available names"
        );
        return Ok(EnsureOutcome::Unknown);
    };
    let variant = ModelRegistry::variant_for(info, quant)
        .expect("resolve_quantization guarantees variant exists");
    let dest = whisper_dest(paths, info.name, quant);
    if dest.exists() {
        debug!("whisper model ready: {}", dest.display());
        return Ok(EnsureOutcome::AlreadyPresent);
    }
    let url =
        ModelRegistry::url_for(info, quant).expect("variant lookup succeeded so URL must resolve");
    debug!(
        "whisper model {model_name:?} ({quant}) missing; downloading {} MB from {url}",
        variant.approx_mb
    );
    fono_download::download(&url, &dest, variant.sha256)
        .await
        .with_context(|| format!("downloading whisper model {model_name:?} ({quant})"))?;
    info!("whisper model installed: {}", dest.display());
    Ok(EnsureOutcome::Downloaded)
}

/// Ensure the named local LLM (`.gguf`) is on disk. Path resolution
/// matches `fono-polish::factory::resolve_local_model_path`:
/// `<polish_models_dir>/<name>.gguf`.
pub async fn ensure_local_llm(paths: &Paths, model_name: &str) -> Result<EnsureOutcome> {
    let Some(info) = fono_polish::LocalLlmRegistry::get(model_name) else {
        warn!(
            "config references unknown LLM model {model_name:?} — run \
             `fono models list` to see available names"
        );
        return Ok(EnsureOutcome::Unknown);
    };
    let dest = paths.polish_models_dir().join(format!("{}.gguf", info.name));
    if dest.exists() {
        debug!("LLM model ready: {}", dest.display());
        return Ok(EnsureOutcome::AlreadyPresent);
    }
    let url = fono_polish::LocalLlmRegistry::url_for(info);
    debug!("LLM model {model_name:?} missing; downloading {} MB from {url}", info.approx_mb);
    fono_download::download(&url, &dest, info.sha256)
        .await
        .with_context(|| format!("downloading LLM model {model_name:?}"))?;
    info!("LLM model installed: {}", dest.display());
    Ok(EnsureOutcome::Downloaded)
}

/// Approximate download size (MB) for the given local STT model name +
/// quantization preference, or `None` when the registry doesn't know
/// the combination. Used by the tray switcher to put a useful number
/// in the "downloading…" notification.
#[must_use]
pub fn local_stt_size_mb(model_name: &str, quantization: &str) -> Option<u32> {
    let (info, quant) = resolve_local_stt(model_name, quantization).ok().flatten()?;
    ModelRegistry::variant_for(info, quant).map(|v| v.approx_mb)
}

/// Approximate download size (MB) for the given local LLM model name,
/// or `None` when the registry doesn't know about it.
#[must_use]
pub fn local_llm_size_mb(model_name: &str) -> Option<u32> {
    fono_polish::LocalLlmRegistry::get(model_name).map(|m| m.approx_mb)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Would this download be announced, given the rate measured by the
    /// decision point? Mirrors [`notify_download_started`]; `spent_secs` is
    /// the total time elapsed when the verdict is reached.
    fn would_notify_after(
        total_mb: u32,
        done_bytes: u64,
        bytes_per_sec: f64,
        spent_secs: f64,
    ) -> bool {
        let remaining = remaining_secs(total_mb, done_bytes, bytes_per_sec);
        !remaining.is_some_and(|secs| spent_secs + secs < NOTIFY_THRESHOLD.as_secs_f64())
    }

    /// The common case: about a second of DNS/TLS, then the sample window.
    fn would_notify(total_mb: u32, done_bytes: u64, bytes_per_sec: f64) -> bool {
        would_notify_after(total_mb, done_bytes, bytes_per_sec, 1.0 + RATE_SAMPLE.as_secs_f64())
    }

    #[test]
    fn a_quick_download_is_not_worth_a_notification() {
        // A 30 MB voice on a 10 MB/s link: 30 MB in by the 3 s mark, so it is
        // done at ~3 s. Well inside the threshold — say nothing.
        assert!(!would_notify(30, 30_000_000, 10_000_000.0));
    }

    #[test]
    fn a_slow_download_is_announced() {
        // A 700 MB LLM at 2 MB/s: ~5 minutes to go. Worth interrupting for,
        // especially when Fono was started from a systemd unit.
        assert!(would_notify(700, 6_000_000, 2_000_000.0));
    }

    #[test]
    fn a_download_just_over_the_threshold_is_announced() {
        // 1 s connecting + 3 s sampled + 12 s projected = 16 s, past the bar.
        assert!(would_notify(15, 3_000_000, 1_000_000.0));
    }

    /// The regression this timing was built for, using figures measured
    /// against the real mirror: a 292 MB model that genuinely finishes in
    /// ~11 s, whose first byte lands at ~1.0 s and which has written 73.5 MB
    /// by the 4 s mark.
    ///
    /// Averaging the dead first second into the rate gives 18.4 MB/s, which
    /// projects ~16 s and wrongly clears the 15 s bar. Measuring only the
    /// three seconds of flowing transfer gives the true 24.5 MB/s, projects
    /// ~13 s, and correctly stays quiet.
    #[test]
    fn connection_setup_is_not_averaged_into_the_rate() {
        let done = 73_500_000;
        let naive_rate = done as f64 / 4.0;
        assert!(
            would_notify_after(292, done, naive_rate, 4.0),
            "averaging in the idle first second over-estimates and notifies needlessly"
        );

        let anchored_rate = done as f64 / 3.0;
        assert!(
            !would_notify_after(292, done, anchored_rate, 4.0),
            "the true rate shows this finishes inside the threshold"
        );
    }

    #[test]
    fn a_stalled_download_is_announced_even_without_an_estimate() {
        // Nothing moved: offline, or a mirror still handshaking. This is
        // exactly when a silent run looks like a hang, so speak up.
        assert!(would_notify(300, 0, 0.0));
    }

    #[test]
    fn eta_reports_minutes_remaining_at_the_measured_rate() {
        assert_eq!(eta_text(294.0, 1_000_000.0), "about 5 min left (1.0 MB/s)");
    }

    #[test]
    fn eta_says_less_than_a_minute_when_nearly_done() {
        assert_eq!(eta_text(2.0, 5_000_000.0), "less than a minute left (5.0 MB/s)");
    }

    #[test]
    fn remaining_is_unknown_when_the_transfer_has_not_moved() {
        assert!(remaining_secs(300, 0, 0.0).is_none());
    }
}
