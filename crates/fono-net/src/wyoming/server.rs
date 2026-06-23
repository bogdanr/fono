// SPDX-License-Identifier: GPL-3.0-only
//! Wyoming-protocol STT server.
//!
//! Accepts LAN peers (Home Assistant satellites, other Fono daemons,
//! Rhasspy clients, …) and serves the active `Arc<dyn SpeechToText>` —
//! whisper-rs, a cloud STT backend, anything implementing the trait.
//!
//! Slice 3 of `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`.
//! One TCP listener task; one tokio task per accepted connection. The
//! per-connection task runs the canonical Wyoming sequence:
//!
//! ```text
//! [optional]  describe       -> info
//!             audio-start    \
//!             audio-chunk … |  collect PCM
//!             audio-stop     /
//!             transcribe     -> transcript
//! ```
//!
//! `transcript-start` / `transcript-chunk` / `transcript-stop` (the
//! streaming server response variant) is *not* emitted in Slice 3 —
//! `SpeechToText::transcribe` is one-shot, so there's no preview text
//! to forward. Slice 3 advertises `supports_transcript_streaming =
//! false` accordingly so streaming-aware clients fall back to the
//! one-shot lane gracefully. Streaming response support arrives when
//! the daemon plumbs `Arc<dyn StreamingStt>` here (post-Slice 4).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use fono_audio::wakeword::WakeWord;
use fono_net_codec::wyoming::{
    AsrModel, AsrProgram, Attribution, AudioChunk, AudioStart, AudioStop, Detect, Detection, Info,
    Synthesize, Transcribe, Transcript, TtsProgram, TtsVoice, WakeModel, WakeProgram, AUDIO_CHUNK,
    AUDIO_START, AUDIO_STOP, DESCRIBE, DETECT, DETECTION, INFO, SYNTHESIZE, TRANSCRIBE, TRANSCRIPT,
};
use fono_net_codec::Frame;
use fono_stt::traits::SpeechToText;
use fono_tts::traits::TextToSpeech;
use serde_json::to_value;
use tokio::io::{AsyncWrite, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Default Wyoming TCP port (matches `wyoming-faster-whisper`).
pub const DEFAULT_PORT: u16 = 10300;

/// How long a single connection may stay idle before we close it. Real
/// peers send something every few seconds; idle TCP holds eat FDs.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Configuration for [`WyomingServer::start`]. Built from `[server.wyoming]`
/// at the daemon layer; tests construct it directly.
#[derive(Debug, Clone)]
pub struct WyomingServerConfig {
    /// Host the listener binds to. `127.0.0.1` is the safe default;
    /// `0.0.0.0` / RFC1918 / link-local addresses are accepted too.
    pub bind: String,
    /// TCP port. Default [`DEFAULT_PORT`] (`10300`).
    pub port: u16,
    /// Optional pre-shared bearer token, resolved by the daemon. Wyoming
    /// v1 has no auth event, so this is plumbed for the Slice 5 Fono
    /// extension; today its value is logged at `debug!` and otherwise
    /// unused. Storing it here keeps the wiring honest.
    pub auth_token: Option<String>,
    /// Server name advertised in `info.asr[].name`,
    /// `info.asr[].models[].attribution.name`, and (later) in mDNS TXT
    /// records. Typical: `"Fono"`.
    pub server_name: String,
    /// Server version string surfaced via `info`. Typical:
    /// `env!("CARGO_PKG_VERSION")`.
    pub server_version: String,
    /// Models to advertise in `info.asr[].models`. Synthesised by the
    /// daemon from the active STT config; can be a single entry for
    /// most setups.
    pub models: Vec<AdvertisedModel>,
    /// Loopback-only flag. When `true`, refuses non-loopback peers
    /// even if the bind address would have allowed them. Set when
    /// `bind = "127.0.0.1"` for defence in depth.
    pub loopback_only: bool,
    /// Voices advertised in `info.tts[].voices`. Empty (the default)
    /// means this listener advertises ASR only. The daemon derives this
    /// from the active `[tts]` backend when it also binds a TTS backend
    /// via [`WyomingServer::with_tts`]; advertisement and serving are
    /// kept in lockstep that way.
    pub tts_voices: Vec<AdvertisedVoice>,
    /// Wake-word models advertised in `info.wake[].models`. Empty (the
    /// default) means this listener advertises **no** wake service — the
    /// wake direction is strictly opt-in, so existing STT/TTS-only
    /// servers are unaffected. The daemon derives this from the active
    /// `[wakeword]` phrases when it also binds a wake detector via
    /// [`WyomingServer::with_wake`]; advertisement and detection are kept
    /// in lockstep that way (the server *is* the detector — audio never
    /// leaves the machine).
    pub wake_models: Vec<AdvertisedWakeModel>,
}

/// One wake-word model surfaced via `info.wake[].models`. Derived by the
/// daemon from the active `[wakeword]` phrases. The server keeps audio
/// local: it advertises these and runs the local detector over streamed
/// `audio-chunk` events, emitting a `detection` event on a fire.
#[derive(Debug, Clone)]
pub struct AdvertisedWakeModel {
    pub name: String,
    pub languages: Vec<String>,
    /// Human-readable spoken phrase (e.g. `"hey fono"`), surfaced so
    /// Wyoming consumers can show it in a picker.
    pub phrase: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
}

/// One model entry surfaced via `info.asr[].models`.
#[derive(Debug, Clone)]
pub struct AdvertisedModel {
    pub name: String,
    pub languages: Vec<String>,
    pub description: Option<String>,
    pub version: Option<String>,
}

/// One voice entry surfaced via `info.tts[].voices`. Derived by the
/// daemon from the active `[tts]` backend + the local voice catalogue.
#[derive(Debug, Clone)]
pub struct AdvertisedVoice {
    pub name: String,
    pub languages: Vec<String>,
    pub description: Option<String>,
    pub version: Option<String>,
}

impl Default for WyomingServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".to_string(),
            port: DEFAULT_PORT,
            auth_token: None,
            server_name: "Fono".to_string(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            models: Vec::new(),
            loopback_only: true,
            tts_voices: Vec::new(),
            wake_models: Vec::new(),
        }
    }
}

/// Handle returned by [`WyomingServer::start`]. Drop or call
/// [`WyomingServerHandle::shutdown`] to stop the listener.
pub struct WyomingServerHandle {
    pub local_addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl WyomingServerHandle {
    /// Bound socket address (useful in tests with `port = 0`).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Politely stop the listener. Existing in-flight connections are
    /// allowed to finish their current request.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for WyomingServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // We intentionally don't `await` the join here — Drop is sync.
        // Callers that need clean shutdown call `.shutdown().await`.
    }
}

/// Provider closure invoked once per accepted connection to obtain
/// the currently-active STT backend. Returning a fresh `Arc` on each
/// call allows the server to track `Reload` swaps in the daemon
/// without restarting the listener.
pub type SttProvider = Arc<dyn Fn() -> Arc<dyn SpeechToText> + Send + Sync>;

/// Provider closure for the active TTS backend, parallel to
/// [`SttProvider`]. Bound via [`WyomingServer::with_tts`]; when the
/// server holds `None` it does not answer `synthesize` requests and
/// advertises no `info.tts` service.
pub type TtsProvider = Arc<dyn Fn() -> Arc<dyn TextToSpeech> + Send + Sync>;

/// Provider closure for the wake-word detector, invoked once per accepted
/// connection to obtain a **fresh** detector instance. A wake session is
/// inherently stateful (the streaming melspectrogram/embedding ring lives
/// in the detector), so each connection gets its own; the closure simply
/// constructs one. When the server holds `None` it neither advertises a
/// wake service nor answers `detect` — the wake direction is opt-in.
///
/// The detector runs **locally** inside the server process: the server
/// *is* the wake detector, so audio stays on the machine. This is the
/// recommended Wyoming wake integration (cf. the opt-in client direction,
/// which would stream idle mic audio over the LAN).
pub type WakeProvider = Arc<dyn Fn() -> Box<dyn WakeWord> + Send + Sync>;

/// The server itself. Stateless beyond the config; one instance per
/// `[server.wyoming]` block.
pub struct WyomingServer {
    cfg: WyomingServerConfig,
    stt: SttProvider,
    tts: Option<TtsProvider>,
    wake: Option<WakeProvider>,
}

impl WyomingServer {
    /// Build a server. Does not bind yet — call [`Self::start`].
    #[must_use]
    pub fn new(cfg: WyomingServerConfig, stt: SttProvider) -> Self {
        Self { cfg, stt, tts: None, wake: None }
    }

    /// Convenience constructor for callers that want to pin a single
    /// backend for the listener's lifetime (no Reload tracking).
    /// Tests use this; production wires the closure form via [`Self::new`].
    #[must_use]
    pub fn with_fixed_stt(cfg: WyomingServerConfig, stt: Arc<dyn SpeechToText>) -> Self {
        Self::new(cfg, Arc::new(move || Arc::clone(&stt)))
    }

    /// Bind a TTS backend provider so this listener also answers
    /// `synthesize` requests and advertises `info.tts`. The closure is
    /// invoked once per accepted connection to track `Reload` swaps,
    /// exactly like [`SttProvider`].
    #[must_use]
    pub fn with_tts(mut self, tts: TtsProvider) -> Self {
        self.tts = Some(tts);
        self
    }

    /// Convenience: pin a single TTS backend for the listener's
    /// lifetime (no `Reload` tracking). Tests use this.
    #[must_use]
    pub fn with_fixed_tts(self, tts: Arc<dyn TextToSpeech>) -> Self {
        self.with_tts(Arc::new(move || Arc::clone(&tts)))
    }

    /// Bind a wake-word detector provider so this listener advertises a
    /// `info.wake` service and answers wake sessions: it feeds streamed
    /// `audio-chunk` PCM to a locally-run [`WakeWord`] detector and emits
    /// a `detection` event on a fire. The closure is invoked once per
    /// accepted connection to obtain a fresh detector (wake sessions are
    /// stateful). Audio stays on the machine — the server *is* the
    /// detector. Opt-in: with no provider bound, no wake service is
    /// advertised and `detect` is ignored.
    #[must_use]
    pub fn with_wake(mut self, wake: WakeProvider) -> Self {
        self.wake = Some(wake);
        self
    }

    /// Bind the listener and spawn the accept loop. Returns once the
    /// socket is listening so callers can `.local_addr()` immediately.
    pub async fn start(self) -> Result<WyomingServerHandle> {
        let addr = format!("{}:{}", self.cfg.bind, self.cfg.port);
        let listener = TcpListener::bind(&addr)
            .await
            .with_context(|| format!("binding wyoming server to {addr}"))?;
        let local_addr = listener.local_addr().context("listener.local_addr")?;
        tracing::info!(
            target: "fono::wyoming::server",
            %local_addr,
            loopback_only = self.cfg.loopback_only,
            "wyoming server listening"
        );

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let cfg = Arc::new(self.cfg);
        let stt = self.stt;
        let tts = self.tts;
        let wake = self.wake;
        let join = tokio::spawn(async move {
            // Bound mpsc just to give us a place to count active conns
            // and to provide a backpressure point if accept() outpaces
            // the workers (which it never will at LAN volumes, but the
            // pattern is cheap).
            let (slot_tx, mut slot_rx) = mpsc::channel::<()>(64);
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        tracing::debug!(target: "fono::wyoming::server", "shutdown signal received");
                        break;
                    }
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((sock, peer)) => {
                                if cfg.loopback_only && !is_loopback(&peer) {
                                    tracing::warn!(
                                        target: "fono::wyoming::server",
                                        %peer,
                                        "rejecting non-loopback peer (bind is loopback-only)"
                                    );
                                    drop(sock);
                                    continue;
                                }
                                let cfg2 = Arc::clone(&cfg);
                                let stt_snapshot = (stt)();
                                let tts_snapshot = tts.as_ref().map(|f| f());
                                let wake_snapshot = wake.as_ref().map(|f| f());
                                let slot_tx2 = slot_tx.clone();
                                tokio::spawn(async move {
                                    let _slot = slot_tx2.try_send(()).ok();
                                    if let Err(e) = handle_connection(sock, peer, cfg2, stt_snapshot, tts_snapshot, wake_snapshot).await {
                                        tracing::debug!(
                                            target: "fono::wyoming::server",
                                            %peer,
                                            "connection ended: {e:#}"
                                        );
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::warn!(
                                    target: "fono::wyoming::server",
                                    "accept failed: {e:#}"
                                );
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        }
                    }
                    Some(()) = slot_rx.recv() => {
                        // Drain the slot signal; channel exists purely for
                        // its bounded counter (above).
                    }
                }
            }
        });

        Ok(WyomingServerHandle { local_addr, shutdown_tx: Some(shutdown_tx), join: Some(join) })
    }
}

fn is_loopback(addr: &SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(v) => v.is_loopback(),
        std::net::IpAddr::V6(v) => v.is_loopback(),
    }
}

/// Per-connection state machine. Returns when the peer disconnects or
/// sends a malformed event; the spawned task logs and exits.
async fn handle_connection(
    sock: TcpStream,
    peer: SocketAddr,
    cfg: Arc<WyomingServerConfig>,
    stt: Arc<dyn SpeechToText>,
    tts: Option<Arc<dyn TextToSpeech>>,
    mut wake: Option<Box<dyn WakeWord>>,
) -> Result<()> {
    sock.set_nodelay(true).ok();
    let (read_half, mut write_half) = sock.into_split();
    let mut reader = BufReader::new(read_half);

    // PCM accumulated across audio-chunk events, in mono f32. Wyoming
    // peers send int16 LE which we convert as we go so we don't keep
    // two copies in RAM.
    let mut pcm_f32: Vec<f32> = Vec::new();
    let mut sample_rate: u32 = 16_000;
    let mut audio_started = false;
    let mut pending_transcribe: Option<Transcribe> = None;

    loop {
        let frame = match tokio::time::timeout(IDLE_TIMEOUT, Frame::read_async(&mut reader)).await {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => return Err(anyhow!("frame read error: {e}")),
            Err(_) => return Err(anyhow!("idle timeout — no traffic for {IDLE_TIMEOUT:?}")),
        };

        match frame.kind.as_str() {
            DESCRIBE => {
                let info = build_info(&cfg);
                Frame::new(INFO).with_data(to_value(&info)?).write_async(&mut write_half).await?;
            }
            AUDIO_START => {
                let s: AudioStart =
                    serde_json::from_value(frame.data).context("decoding audio-start")?;
                sample_rate = s.rate;
                pcm_f32.clear();
                audio_started = true;
            }
            AUDIO_CHUNK => {
                let hdr: AudioChunk =
                    serde_json::from_value(frame.data).context("decoding audio-chunk header")?;
                if !audio_started {
                    tracing::trace!(
                        target: "fono::wyoming::server",
                        %peer,
                        "accepting audio-chunk before audio-start"
                    );
                    audio_started = true;
                }
                sample_rate = hdr.rate;
                let samples = decode_pcm_le(&frame.payload, hdr.width, hdr.channels)?;
                // Wake path (opt-in): feed the freshly-decoded samples to the
                // locally-run detector and emit a `detection` event on a fire.
                // The detector keeps audio on-device — the server *is* the
                // wake word detector.
                feed_wake(&mut wake, peer, &mut write_half, &samples).await?;
                pcm_f32.extend(samples);
            }
            AUDIO_STOP => {
                let _: AudioStop =
                    serde_json::from_value(frame.data).context("decoding audio-stop")?;
                audio_started = false;
                if let Some(req) = pending_transcribe.take() {
                    finish_transcription(
                        peer,
                        stt.as_ref(),
                        &mut write_half,
                        &mut pcm_f32,
                        sample_rate,
                        req,
                    )
                    .await?;
                }
            }
            TRANSCRIBE => {
                let req: Transcribe =
                    serde_json::from_value(frame.data).context("decoding transcribe")?;
                if pcm_f32.is_empty() || audio_started {
                    tracing::debug!(
                        target: "fono::wyoming::server",
                        %peer,
                        samples = pcm_f32.len(),
                        rate = sample_rate,
                        lang = req.language.as_deref(),
                        "queued transcribe request until audio-stop"
                    );
                    pending_transcribe = Some(req);
                } else {
                    finish_transcription(
                        peer,
                        stt.as_ref(),
                        &mut write_half,
                        &mut pcm_f32,
                        sample_rate,
                        req,
                    )
                    .await?;
                }
            }
            SYNTHESIZE => {
                dispatch_synthesize(peer, tts.as_ref(), &mut write_half, frame.data).await?;
            }
            DETECT => {
                let req: Detect = serde_json::from_value(frame.data).context("decoding detect")?;
                log_detect_session(peer, wake.is_some(), &req);
            }
            other => {
                tracing::trace!(
                    target: "fono::wyoming::server",
                    event = other,
                    "ignoring unsupported event"
                );
            }
        }
    }
}

/// Log a wake `detect` session start. The optional `names` narrowing is
/// accepted for protocol completeness; the bound detector already knows
/// which phrases it loaded, so the streamed audio drives detection.
fn log_detect_session(peer: SocketAddr, wake_bound: bool, req: &Detect) {
    if wake_bound {
        tracing::debug!(
            target: "fono::wyoming::server",
            %peer,
            names = ?req.names,
            "wake detect session started"
        );
    } else {
        tracing::warn!(
            target: "fono::wyoming::server",
            %peer,
            "detect requested but no wake detector is bound; ignoring"
        );
    }
}

/// Feed freshly-decoded mono `f32` samples to the locally-run wake
/// detector (when one is bound) and emit a `detection` event on a fire.
/// No-op when no detector is bound. The detector runs in-process so the
/// audio never leaves the machine. Detector errors are logged and
/// swallowed rather than tearing down the connection — a transient
/// inference error must not kill a long-lived wake session.
///
/// NOTE (follow-up seam): the detector's `feed` is synchronous and, for
/// the ONNX backend, CPU-bound (~one melspectrogram+embedding pass per
/// 80 ms hop). It is called inline here; if a future high-throughput
/// deployment shows it stalling the connection's task, move it behind a
/// `spawn_blocking` worker (the detector is `Send`).
async fn feed_wake<W>(
    wake: &mut Option<Box<dyn WakeWord>>,
    peer: SocketAddr,
    write_half: &mut W,
    samples: &[f32],
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let Some(detector) = wake.as_deref_mut() else { return Ok(()) };
    match detector.feed(samples) {
        Ok(decision) if decision.fired => {
            let name = decision.phrase.unwrap_or_else(|| "wake".to_string());
            tracing::info!(
                target: "fono::wyoming::server",
                %peer,
                phrase = %name,
                score = decision.score,
                "wake word fired; emitting detection"
            );
            let event = Detection { name, timestamp: Some(unix_millis()), speaker: None };
            Frame::new(DETECTION).with_data(to_value(&event)?).write_async(write_half).await?;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::debug!(target: "fono::wyoming::server", %peer, "wake detector error: {e:#}");
        }
    }
    Ok(())
}

/// Milliseconds since the Unix epoch, saturating to 0 if the clock is
/// before the epoch (never on a sane host).
fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn finish_transcription<W>(
    peer: SocketAddr,
    stt: &dyn SpeechToText,
    write_half: &mut W,
    pcm_f32: &mut Vec<f32>,
    sample_rate: u32,
    req: Transcribe,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if pcm_f32.is_empty() {
        return Err(anyhow!("transcribe requested with no audio samples"));
    }

    let lang = req.language.as_deref();
    tracing::info!(
        target: "fono::wyoming::server",
        %peer,
        samples = pcm_f32.len(),
        rate = sample_rate,
        lang = lang,
        "processing transcription request"
    );
    let started = std::time::Instant::now();
    let res = stt
        .transcribe(pcm_f32.as_slice(), sample_rate, lang)
        .await
        .context("backend stt.transcribe")?;
    tracing::info!(
        target: "fono::wyoming::server",
        %peer,
        elapsed_ms = started.elapsed().as_millis() as u64,
        chars = res.text.chars().count(),
        "transcription request complete"
    );
    let resp = Transcript { text: res.text, language: res.language };
    Frame::new(TRANSCRIPT).with_data(to_value(&resp)?).write_async(write_half).await?;
    pcm_f32.clear();
    Ok(())
}

/// Samples per `audio-chunk` frame. At 22.05 kHz mono this is ~93 ms
/// of audio per chunk — small enough that Home Assistant begins
/// playback promptly, large enough that framing overhead stays
/// negligible.
const TTS_CHUNK_SAMPLES: usize = 2048;

/// Decode a `synthesize` event and, if a TTS backend is bound, drive
/// it via [`handle_synthesize`]. With no backend bound the request is
/// logged and dropped — an ASR-only listener stays well-behaved rather
/// than tearing down the connection.
async fn dispatch_synthesize<W>(
    peer: SocketAddr,
    tts: Option<&Arc<dyn TextToSpeech>>,
    write_half: &mut W,
    frame_data: serde_json::Value,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let req: Synthesize = serde_json::from_value(frame_data).context("decoding synthesize")?;
    if let Some(tts) = tts {
        handle_synthesize(peer, tts.as_ref(), write_half, req).await?;
    } else {
        tracing::warn!(
            target: "fono::wyoming::server",
            %peer,
            "synthesize requested but no TTS backend is bound; ignoring"
        );
    }
    Ok(())
}

/// Drive a `synthesize` request: call the backend, then stream the
/// result back as the canonical `audio-start` / `audio-chunk`* /
/// `audio-stop` sequence in int16 LE mono (the Wyoming wire format).
/// Empty `text` yields an empty PCM buffer (trait contract), which we
/// honour as `audio-start` immediately followed by `audio-stop` with no
/// chunks in between.
async fn handle_synthesize<W>(
    peer: SocketAddr,
    tts: &dyn TextToSpeech,
    write_half: &mut W,
    req: Synthesize,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let (voice, lang) =
        req.voice.as_ref().map_or((None, None), |v| (v.name.as_deref(), v.language.as_deref()));
    tracing::info!(
        target: "fono::wyoming::server",
        %peer,
        chars = req.text.chars().count(),
        voice,
        lang,
        "processing synthesize request"
    );
    let started = std::time::Instant::now();
    let audio = tts.synthesize(&req.text, voice, lang).await.context("backend tts.synthesize")?;
    let rate = audio.sample_rate;

    Frame::new(AUDIO_START)
        .with_data(to_value(AudioStart { rate, width: 2, channels: 1, timestamp: None })?)
        .write_async(write_half)
        .await?;

    for chunk in audio.pcm.chunks(TTS_CHUNK_SAMPLES) {
        let mut bytes = Vec::with_capacity(chunk.len() * 2);
        for &s in chunk {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        Frame::new(AUDIO_CHUNK)
            .with_data(to_value(AudioChunk { rate, width: 2, channels: 1, timestamp: None })?)
            .with_payload(bytes)
            .write_async(write_half)
            .await?;
    }

    Frame::new(AUDIO_STOP)
        .with_data(to_value(AudioStop::default())?)
        .write_async(write_half)
        .await?;

    tracing::info!(
        target: "fono::wyoming::server",
        %peer,
        elapsed_ms = started.elapsed().as_millis() as u64,
        samples = audio.pcm.len(),
        rate,
        "synthesize request complete"
    );
    Ok(())
}

/// Convert PCM bytes to mono `f32` samples in the -1.0..1.0 range.
/// Wyoming ASR clients, including Home Assistant, normally send signed
/// int16 LE. Multi-channel int16 input is mixed down by averaging each
/// interleaved frame.
fn decode_pcm_le(bytes: &[u8], width: u32, channels: u32) -> Result<Vec<f32>> {
    if width != 2 {
        return Err(anyhow!("unsupported audio width {width}; expected 2-byte PCM"));
    }
    if channels == 0 {
        return Err(anyhow!("unsupported audio channel count 0"));
    }

    let channel_count = channels as usize;
    let frame_bytes = 2 * channel_count;
    let mut out = Vec::with_capacity(bytes.len() / frame_bytes);
    for frame in bytes.chunks_exact(frame_bytes) {
        let mut sum = 0.0_f32;
        for ch in 0..channel_count {
            let offset = ch * 2;
            let i = i16::from_le_bytes([frame[offset], frame[offset + 1]]);
            sum += f32::from(i) / 32767.0;
        }
        out.push(sum / channels as f32);
    }
    Ok(out)
}

/// Convert int16 LE bytes (Wyoming wire format) to mono `f32` samples
/// in the -1.0..1.0 range. Trailing odd byte is silently dropped (a
/// valid stream is always even-length; the alternative is failing the
/// connection on a single torn frame, which is harsher than helpful).
#[cfg(test)]
fn decode_int16_le(bytes: &[u8]) -> impl Iterator<Item = f32> + '_ {
    bytes.chunks_exact(2).map(|pair| {
        let i = i16::from_le_bytes([pair[0], pair[1]]);
        f32::from(i) / 32767.0
    })
}

/// Synthesise the `info` event from the server config + advertised
/// model list.
fn build_info(cfg: &WyomingServerConfig) -> Info {
    let attribution = Attribution {
        name: cfg.server_name.clone(),
        url: "https://github.com/bogdanr/fono".to_string(),
    };
    let models = cfg
        .models
        .iter()
        .map(|m| AsrModel {
            name: m.name.clone(),
            languages: m.languages.clone(),
            installed: true,
            attribution: attribution.clone(),
            description: m.description.clone(),
            version: m.version.clone().or_else(|| Some(cfg.server_version.clone())),
        })
        .collect();
    let asr = vec![AsrProgram {
        name: cfg.server_name.clone(),
        attribution: attribution.clone(),
        installed: true,
        description: Some("Fono speech-to-text".to_string()),
        version: Some(cfg.server_version.clone()),
        models,
        // Streaming-response support arrives when StreamingStt is
        // wired through. For now, advertise the one-shot lane only.
        supports_transcript_streaming: false,
    }];
    // Advertise a TTS program only when voices are configured — an
    // empty `tts` array means "ASR only" to Home Assistant's loader.
    let tts = if cfg.tts_voices.is_empty() {
        Vec::new()
    } else {
        vec![TtsProgram {
            name: cfg.server_name.clone(),
            attribution: attribution.clone(),
            installed: true,
            description: Some("Fono text-to-speech".to_string()),
            version: Some(cfg.server_version.clone()),
            voices: cfg
                .tts_voices
                .iter()
                .map(|v| TtsVoice {
                    name: v.name.clone(),
                    languages: v.languages.clone(),
                    speakers: Vec::new(),
                    installed: true,
                    attribution: attribution.clone(),
                    description: v.description.clone(),
                    version: v.version.clone().or_else(|| Some(cfg.server_version.clone())),
                })
                .collect(),
            // Sentence-level streaming is plumbed when the local engine
            // streams; the one-shot lane is correct for now.
            supports_synthesize_streaming: false,
        }]
    };
    // Advertise a wake program only when wake models are configured — an
    // empty `wake` array means "no wake service" (the wake direction is
    // opt-in; STT/TTS-only servers stay unaffected).
    let wake = if cfg.wake_models.is_empty() {
        Vec::new()
    } else {
        vec![WakeProgram {
            name: cfg.server_name.clone(),
            attribution: attribution.clone(),
            installed: true,
            description: Some("Fono wake-word detector".to_string()),
            version: Some(cfg.server_version.clone()),
            models: cfg
                .wake_models
                .iter()
                .map(|m| WakeModel {
                    name: m.name.clone(),
                    languages: m.languages.clone(),
                    installed: true,
                    attribution: attribution.clone(),
                    description: m.description.clone(),
                    version: m.version.clone().or_else(|| Some(cfg.server_version.clone())),
                    phrase: m.phrase.clone(),
                })
                .collect(),
        }]
    };
    Info { asr, tts, wake, ..Info::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_int16_le_round_trips_silence() {
        let bytes = vec![0_u8; 100];
        let v: Vec<f32> = decode_int16_le(&bytes).collect();
        assert_eq!(v.len(), 50);
        assert!(v.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn decode_int16_le_drops_odd_trailing_byte() {
        let bytes = vec![0, 0, 0, 0, 0xff]; // 4 valid bytes + 1 trailing
        assert_eq!(decode_int16_le(&bytes).count(), 2);
    }

    #[test]
    fn decode_int16_le_recovers_full_scale() {
        // 0x7fff little-endian = 32767 = +1.0 (full scale).
        let bytes = vec![0xff, 0x7f];
        let v: Vec<f32> = decode_int16_le(&bytes).collect();
        assert_eq!(v, vec![1.0]);
    }

    #[test]
    fn build_info_advertises_models() {
        let cfg = WyomingServerConfig {
            models: vec![AdvertisedModel {
                name: "small".into(),
                languages: vec!["en".into(), "ro".into()],
                description: Some("ggml-small".into()),
                version: None,
            }],
            ..WyomingServerConfig::default()
        };
        let info = build_info(&cfg);
        let asr = info.asr.first().expect("asr present");
        assert_eq!(asr.name, "Fono");
        assert!(asr.installed);
        assert_eq!(asr.models.len(), 1);
        assert_eq!(asr.models[0].name, "small");
        assert_eq!(asr.models[0].languages, vec!["en", "ro"]);
        assert!(!asr.supports_transcript_streaming);
        assert_eq!(asr.models[0].attribution.name, "Fono");
    }

    use fono_tts::traits::TtsAudio;
    use tokio::io::BufReader;

    /// Minimal in-process TTS backend that returns a fixed PCM buffer,
    /// so the server-side `synthesize` framing can be tested without a
    /// real engine or a network peer.
    struct MockTts {
        pcm: Vec<f32>,
        rate: u32,
    }

    #[async_trait::async_trait]
    impl TextToSpeech for MockTts {
        async fn synthesize(
            &self,
            _text: &str,
            _voice: Option<&str>,
            _lang: Option<&str>,
        ) -> Result<TtsAudio> {
            Ok(TtsAudio { pcm: self.pcm.clone(), sample_rate: self.rate })
        }
        fn name(&self) -> &'static str {
            "mock"
        }
        fn native_sample_rate(&self) -> u32 {
            self.rate
        }
    }

    /// Run one `synthesize` through the handler and parse the emitted
    /// frames back, returning `(audio-start header, total samples across
    /// chunks, saw audio-stop)`.
    async fn collect_synth_frames(pcm: Vec<f32>, rate: u32) -> (AudioStart, usize, bool) {
        let tts = MockTts { pcm, rate };
        let req = Synthesize { text: "hello".into(), voice: None };
        let peer: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let mut buf: Vec<u8> = Vec::new();
        handle_synthesize(peer, &tts, &mut buf, req).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let start_frame = Frame::read_async(&mut reader).await.unwrap();
        assert_eq!(start_frame.kind, AUDIO_START);
        let start: AudioStart = serde_json::from_value(start_frame.data).unwrap();

        let mut samples = 0usize;
        let saw_stop;
        loop {
            let f = Frame::read_async(&mut reader).await.unwrap();
            match f.kind.as_str() {
                AUDIO_STOP => {
                    saw_stop = true;
                    break;
                }
                AUDIO_CHUNK => samples += f.payload.len() / 2,
                other => panic!("unexpected frame {other}"),
            }
        }
        (start, samples, saw_stop)
    }

    #[tokio::test]
    async fn synthesize_streams_audio_start_chunks_stop() {
        let (start, samples, saw_stop) = collect_synth_frames(vec![0.25_f32; 5000], 22050).await;
        assert_eq!(start.rate, 22050);
        assert_eq!(start.width, 2);
        assert_eq!(start.channels, 1);
        assert_eq!(samples, 5000, "all PCM samples must survive chunking");
        assert!(saw_stop, "stream must terminate with audio-stop");
    }

    #[tokio::test]
    async fn synthesize_empty_text_emits_start_then_stop_no_chunks() {
        let (start, samples, saw_stop) = collect_synth_frames(Vec::new(), 16000).await;
        assert_eq!(start.rate, 16000);
        assert_eq!(samples, 0, "empty PCM must emit no audio-chunk frames");
        assert!(saw_stop);
    }

    #[tokio::test]
    async fn synthesize_round_trips_full_scale_sample() {
        // +1.0 maps to 0x7fff (32767) little-endian.
        let tts = MockTts { pcm: vec![1.0], rate: 22050 };
        let peer: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let mut buf: Vec<u8> = Vec::new();
        handle_synthesize(peer, &tts, &mut buf, Synthesize { text: "x".into(), voice: None })
            .await
            .unwrap();
        let mut reader = BufReader::new(buf.as_slice());
        let _start = Frame::read_async(&mut reader).await.unwrap();
        let chunk = Frame::read_async(&mut reader).await.unwrap();
        assert_eq!(chunk.kind, AUDIO_CHUNK);
        assert_eq!(chunk.payload, vec![0xff, 0x7f]);
    }

    #[test]
    fn build_info_advertises_tts_voices_when_configured() {
        let cfg = WyomingServerConfig {
            tts_voices: vec![AdvertisedVoice {
                name: "ro_RO-mihai-medium".into(),
                languages: vec!["ro".into()],
                description: Some("Piper".into()),
                version: None,
            }],
            ..WyomingServerConfig::default()
        };
        let info = build_info(&cfg);
        assert_eq!(info.tts.len(), 1);
        let prog = &info.tts[0];
        assert_eq!(prog.name, "Fono");
        assert!(prog.installed);
        assert_eq!(prog.voices.len(), 1);
        assert_eq!(prog.voices[0].name, "ro_RO-mihai-medium");
        assert_eq!(prog.voices[0].languages, vec!["ro"]);
        assert_eq!(prog.voices[0].attribution.name, "Fono");
    }

    #[test]
    fn build_info_omits_tts_when_no_voices() {
        let info = build_info(&WyomingServerConfig::default());
        assert!(info.tts.is_empty(), "no voices configured => no tts program");
        assert!(!info.asr.is_empty(), "asr is always advertised");
    }

    #[test]
    fn build_info_advertises_wake_models_when_configured() {
        let cfg = WyomingServerConfig {
            wake_models: vec![AdvertisedWakeModel {
                name: "hey_fono".into(),
                languages: vec!["en".into()],
                phrase: Some("hey fono".into()),
                description: Some("default clean-license model".into()),
                version: None,
            }],
            ..WyomingServerConfig::default()
        };
        let info = build_info(&cfg);
        assert_eq!(info.wake.len(), 1);
        let prog = &info.wake[0];
        assert_eq!(prog.name, "Fono");
        assert!(prog.installed);
        assert_eq!(prog.models.len(), 1);
        assert_eq!(prog.models[0].name, "hey_fono");
        assert_eq!(prog.models[0].phrase.as_deref(), Some("hey fono"));
        assert_eq!(prog.models[0].attribution.name, "Fono");
    }

    #[test]
    fn build_info_omits_wake_when_no_models() {
        let info = build_info(&WyomingServerConfig::default());
        assert!(info.wake.is_empty(), "no wake models configured => no wake program");
    }
}
