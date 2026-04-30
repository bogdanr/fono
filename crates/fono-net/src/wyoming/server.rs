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
use fono_net_codec::wyoming::{
    AsrInfo, AsrModel, Attribution, AudioChunk, AudioStart, Info, Transcribe, Transcript,
    AUDIO_CHUNK, AUDIO_START, AUDIO_STOP, DESCRIBE, INFO, TRANSCRIBE, TRANSCRIPT,
};
use fono_net_codec::Frame;
use fono_stt::traits::SpeechToText;
use serde_json::to_value;
use tokio::io::BufReader;
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
    /// Server name advertised in `info.asr.models[].attribution.name`
    /// and (later) in mDNS TXT records. Typical: `"fono"`.
    pub server_name: String,
    /// Server version string surfaced via `info`. Typical:
    /// `env!("CARGO_PKG_VERSION")`.
    pub server_version: String,
    /// Models to advertise in `info.asr.models`. Synthesised by the
    /// daemon from the active STT config; can be a single entry for
    /// most setups.
    pub models: Vec<AdvertisedModel>,
    /// Loopback-only flag. When `true`, refuses non-loopback peers
    /// even if the bind address would have allowed them. Set when
    /// `bind = "127.0.0.1"` for defence in depth.
    pub loopback_only: bool,
}

/// One model entry surfaced via `info.asr.models`.
#[derive(Debug, Clone)]
pub struct AdvertisedModel {
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
            server_name: "fono".to_string(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            models: Vec::new(),
            loopback_only: true,
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

/// The server itself. Stateless beyond the config; one instance per
/// `[server.wyoming]` block.
pub struct WyomingServer {
    cfg: WyomingServerConfig,
    stt: SttProvider,
}

impl WyomingServer {
    /// Build a server. Does not bind yet — call [`Self::start`].
    #[must_use]
    pub fn new(cfg: WyomingServerConfig, stt: SttProvider) -> Self {
        Self { cfg, stt }
    }

    /// Convenience constructor for callers that want to pin a single
    /// backend for the listener's lifetime (no Reload tracking).
    /// Tests use this; production wires the closure form via [`Self::new`].
    #[must_use]
    pub fn with_fixed_stt(cfg: WyomingServerConfig, stt: Arc<dyn SpeechToText>) -> Self {
        Self::new(cfg, Arc::new(move || Arc::clone(&stt)))
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
                                let slot_tx2 = slot_tx.clone();
                                tokio::spawn(async move {
                                    let _slot = slot_tx2.try_send(()).ok();
                                    if let Err(e) = handle_connection(sock, peer, cfg2, stt_snapshot).await {
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

        Ok(WyomingServerHandle {
            local_addr,
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        })
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

    loop {
        let frame = match tokio::time::timeout(IDLE_TIMEOUT, Frame::read_async(&mut reader)).await {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => return Err(anyhow!("frame read error: {e}")),
            Err(_) => return Err(anyhow!("idle timeout — no traffic for {IDLE_TIMEOUT:?}")),
        };

        match frame.kind.as_str() {
            DESCRIBE => {
                let info = build_info(&cfg);
                Frame::new(INFO)
                    .with_data(to_value(&info)?)
                    .write_async(&mut write_half)
                    .await?;
            }
            AUDIO_START => {
                let s: AudioStart =
                    serde_json::from_value(frame.data).context("decoding audio-start")?;
                sample_rate = s.rate;
                pcm_f32.clear();
                audio_started = true;
            }
            AUDIO_CHUNK => {
                if !audio_started {
                    return Err(anyhow!("audio-chunk before audio-start"));
                }
                let _hdr: AudioChunk =
                    serde_json::from_value(frame.data).context("decoding audio-chunk header")?;
                pcm_f32.extend(decode_int16_le(&frame.payload));
            }
            AUDIO_STOP => {
                // No-op beyond closing the stream — we keep collecting
                // until `transcribe` arrives so a peer can issue
                // multiple audio-stop / audio-start sequences if they
                // really want to. (Spec doesn't require that, but
                // tolerating it costs nothing.)
                audio_started = false;
            }
            TRANSCRIBE => {
                let req: Transcribe =
                    serde_json::from_value(frame.data).context("decoding transcribe")?;
                let lang = req.language.as_deref();
                tracing::debug!(
                    target: "fono::wyoming::server",
                    %peer,
                    samples = pcm_f32.len(),
                    rate = sample_rate,
                    lang = lang,
                    "transcribe request"
                );
                let started = std::time::Instant::now();
                let res = stt
                    .transcribe(&pcm_f32, sample_rate, lang)
                    .await
                    .context("backend stt.transcribe")?;
                tracing::debug!(
                    target: "fono::wyoming::server",
                    %peer,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    chars = res.text.chars().count(),
                    "transcribe complete"
                );
                let resp = Transcript {
                    text: res.text,
                    language: res.language,
                };
                Frame::new(TRANSCRIPT)
                    .with_data(to_value(&resp)?)
                    .write_async(&mut write_half)
                    .await?;
                pcm_f32.clear();
                audio_started = false;
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

/// Convert int16 LE bytes (Wyoming wire format) to mono `f32` samples
/// in the -1.0..1.0 range. Trailing odd byte is silently dropped (a
/// valid stream is always even-length; the alternative is failing the
/// connection on a single torn frame, which is harsher than helpful).
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
            version: m
                .version
                .clone()
                .or_else(|| Some(cfg.server_version.clone())),
        })
        .collect();
    Info {
        asr: Some(AsrInfo {
            models,
            // Streaming-response support arrives when StreamingStt is
            // wired through. For now, advertise the one-shot lane only.
            supports_transcript_streaming: false,
        }),
    }
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
        let asr = info.asr.expect("asr present");
        assert_eq!(asr.models.len(), 1);
        assert_eq!(asr.models[0].name, "small");
        assert_eq!(asr.models[0].languages, vec!["en", "ro"]);
        assert!(!asr.supports_transcript_streaming);
        assert_eq!(asr.models[0].attribution.name, "fono");
    }
}
