// SPDX-License-Identifier: GPL-3.0-only
//! Wyoming STT client backend.
//!
//! Talks the [Wyoming protocol](https://github.com/OHF-Voice/wyoming) so
//! Fono can use any existing speech-to-text server in the ecosystem
//! (`wyoming-faster-whisper`, `wyoming-whisper-cpp`, Rhasspy, Home
//! Assistant satellites, plus future `fono serve wyoming` daemons) as a
//! drop-in cloud STT replacement that runs on the LAN. Slice 2 of
//! `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`.
//!
//! Wire format is implemented in `fono-net-codec`; this module only
//! orchestrates the event sequence and converts Fono's mono `f32` PCM
//! to/from the Wyoming-native int16 LE.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_net_codec::wyoming::{
    AudioChunk, AudioStart, AudioStop, Info, Transcribe, Transcript, AUDIO_CHUNK, AUDIO_START,
    AUDIO_STOP, DESCRIBE, INFO, TRANSCRIBE, TRANSCRIPT, TRANSCRIPT_CHUNK, TRANSCRIPT_START,
    TRANSCRIPT_STOP,
};
use fono_net_codec::Frame;
use serde_json::to_value;
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::lang::LanguageSelection;
use crate::traits::{SpeechToText, Transcription};

#[allow(dead_code)] // reserved for the LanguageCache wiring in Slice 4
pub(crate) const BACKEND_KEY: &str = "wyoming";
const DEFAULT_PORT: u16 = 10300;
/// Send PCM in ~125 ms chunks (2 KiB at 16 kHz int16 mono). Small enough
/// to keep a streaming server's pipeline busy, large enough that we
/// don't pay framing overhead for every 20-sample turn.
const PCM_CHUNK_SAMPLES: usize = 2048;
/// Best-effort connect timeout; servers should be sub-second on LAN.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Cap end-to-end transcribe wall-time. 5 minutes is generous for a
/// typical dictation slice; long batch jobs should use the server's
/// own CLI.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(300);

/// Parsed `(host, port)` pair from a Wyoming URI.
fn parse_uri(uri: &str) -> Result<(String, u16)> {
    let stripped = uri
        .trim()
        .strip_prefix("tcp://")
        .or_else(|| uri.trim().strip_prefix("wyoming://"))
        .unwrap_or_else(|| uri.trim());
    if stripped.is_empty() {
        return Err(anyhow!("wyoming URI is empty"));
    }
    // Walk from the right to support IPv6 literals like `[::1]:10300`.
    if let Some(stripped_v6) = stripped
        .strip_prefix('[')
        .and_then(|rest| rest.find(']').map(|i| (rest, i)))
    {
        let (rest, end) = stripped_v6;
        let host = &rest[..end];
        let after = &rest[end + 1..];
        let port = after
            .strip_prefix(':')
            .map_or(Ok(DEFAULT_PORT), str::parse::<u16>)
            .context("parsing IPv6 port")?;
        return Ok((host.to_string(), port));
    }
    let (host, port) = stripped.rsplit_once(':').map_or_else(
        || (stripped.to_string(), DEFAULT_PORT),
        |(h, p)| (h.to_string(), p.parse::<u16>().unwrap_or(DEFAULT_PORT)),
    );
    Ok((host, port))
}

/// Convert mono f32 PCM (-1.0..1.0) to int16 little-endian bytes,
/// saturating on out-of-range samples. Wyoming spec: width=2, channels=1.
fn pcm_f32_to_i16_le(pcm: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        let clamped = s.clamp(-1.0, 1.0);
        let i = (clamped * 32767.0) as i16;
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}

/// Wyoming STT client. One instance per configured server; one fresh
/// TCP connection per `transcribe()` call (Wyoming spec recommends
/// servers serve N connections serially).
pub struct WyomingStt {
    host: String,
    port: u16,
    /// Optional model hint sent in `transcribe.name`. `None` lets the
    /// server pick its default.
    model: Option<String>,
    /// Configured language allow-list (see `crate::lang`). Forwarded
    /// to the server as `transcribe.language` when non-auto.
    languages: Vec<String>,
    /// Optional pre-shared bearer token. Reserved for the future
    /// `fono.auth` extension event Slice 5 ships; for now Wyoming v1
    /// has no in-band auth so this field is plumbed but unused. Set
    /// via `with_auth_token`; `fono doctor` can verify it's present.
    auth_token: Option<String>,
}

impl WyomingStt {
    /// Construct from a URI like `tcp://host:10300` (or bare
    /// `host:port`, or `host` with the default port).
    pub fn from_uri(uri: &str) -> Result<Self> {
        let (host, port) = parse_uri(uri)?;
        Ok(Self {
            host,
            port,
            model: None,
            languages: Vec::new(),
            auth_token: None,
        })
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        let m = model.into();
        if !m.trim().is_empty() {
            self.model = Some(m);
        }
        self
    }

    #[must_use]
    pub fn with_languages(mut self, codes: Vec<String>) -> Self {
        self.languages = codes;
        self
    }

    #[must_use]
    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.auth_token = token.filter(|t| !t.trim().is_empty());
        self
    }

    fn effective_selection(&self, lang_override: Option<&str>) -> LanguageSelection {
        LanguageSelection::from_config(&self.languages).with_override(lang_override)
    }

    async fn connect(&self) -> Result<TcpStream> {
        let addr = format!("{}:{}", self.host, self.port);
        let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .with_context(|| format!("connecting to wyoming server at {addr} timed out"))?
            .with_context(|| format!("connecting to wyoming server at {addr}"))?;
        // Disable Nagle: framed protocol, every event already buffered.
        stream.set_nodelay(true).ok();
        Ok(stream)
    }

    /// `[host]:port` for log lines.
    fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[async_trait]
impl SpeechToText for WyomingStt {
    fn name(&self) -> &'static str {
        "wyoming"
    }

    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        let stream = self.connect().await?;
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // 1. audio-start
        let start = AudioStart {
            rate: sample_rate,
            width: 2,
            channels: 1,
            timestamp: None,
        };
        Frame::new(AUDIO_START)
            .with_data(to_value(&start)?)
            .write_async(&mut write_half)
            .await?;

        // 2. audio-chunk × N — int16 LE PCM in `payload`, header carries format.
        for chunk in pcm.chunks(PCM_CHUNK_SAMPLES) {
            let payload = pcm_f32_to_i16_le(chunk);
            let header = AudioChunk {
                rate: sample_rate,
                width: 2,
                channels: 1,
                timestamp: None,
            };
            Frame::new(AUDIO_CHUNK)
                .with_data(to_value(&header)?)
                .with_payload(payload)
                .write_async(&mut write_half)
                .await?;
        }

        // 3. audio-stop
        Frame::new(AUDIO_STOP)
            .with_data(to_value(AudioStop::default())?)
            .write_async(&mut write_half)
            .await?;

        // 4. transcribe — send model + language hints. Server may ignore.
        let selection = self.effective_selection(lang);
        let language = selection.fallback_hint().map(str::to_string);
        let req = Transcribe {
            name: self.model.clone(),
            language,
        };
        Frame::new(TRANSCRIBE)
            .with_data(to_value(&req)?)
            .write_async(&mut write_half)
            .await?;

        // 5. read frames until `transcript` (final). Streaming servers
        //    interleave `transcript-start`/`-chunk`/`-stop`; we collect
        //    them into a buffer for callers that don't use the
        //    `StreamingStt` path, then prefer the final `transcript`
        //    text when one arrives.
        let mut streaming_buf = String::new();
        let mut detected_lang: Option<String> = None;
        let started = std::time::Instant::now();

        let result = timeout(TRANSCRIBE_TIMEOUT, async {
            loop {
                let f = Frame::read_async(&mut reader)
                    .await
                    .with_context(|| format!("reading from wyoming server at {}", self.endpoint()))?;
                match f.kind.as_str() {
                    TRANSCRIPT => {
                        let t: Transcript = serde_json::from_value(f.data)
                            .context("decoding wyoming `transcript` event")?;
                        return Ok::<(String, Option<String>), anyhow::Error>((t.text, t.language));
                    }
                    TRANSCRIPT_START => {
                        let s: fono_net_codec::wyoming::TranscriptStart =
                            serde_json::from_value(f.data).unwrap_or_default();
                        detected_lang = s.language;
                        streaming_buf.clear();
                    }
                    TRANSCRIPT_CHUNK => {
                        let c: fono_net_codec::wyoming::TranscriptChunk =
                            serde_json::from_value(f.data)
                                .context("decoding wyoming `transcript-chunk` event")?;
                        streaming_buf.push_str(&c.text);
                    }
                    TRANSCRIPT_STOP => {
                        // Some servers close the stream with -stop and
                        // never send a final `transcript`. Fall back to
                        // the accumulated chunks in that case.
                        return Ok((std::mem::take(&mut streaming_buf), detected_lang.take()));
                    }
                    other => {
                        tracing::trace!(target: "fono::wyoming", event = other, "unexpected event ignored");
                    }
                }
            }
        })
        .await;

        let (text, language) = match result {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(anyhow!(
                    "wyoming server at {} did not return a transcript within {:?}",
                    self.endpoint(),
                    TRANSCRIBE_TIMEOUT
                ));
            }
        };

        Ok(Transcription {
            text,
            language,
            duration_ms: u64::try_from(started.elapsed().as_millis()).ok(),
        })
    }

    async fn prewarm(&self) -> Result<()> {
        // Open a connection, send `describe`, read `info`, drop.
        // Latency win: TCP handshake + first JSON parse are paid before
        // the user's first hotkey press, so the actual transcribe is
        // bottlenecked only by the server.
        let stream = self.connect().await?;
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        Frame::new(DESCRIBE).write_async(&mut write_half).await?;
        let f = timeout(Duration::from_secs(5), Frame::read_async(&mut reader))
            .await
            .with_context(|| {
                format!(
                    "wyoming describe to {} timed out (server unreachable?)",
                    self.endpoint()
                )
            })??;
        if f.kind == INFO {
            let info: Info = serde_json::from_value(f.data).unwrap_or_default();
            let models = info.asr.iter().map(|asr| asr.models.len()).sum::<usize>();
            let streaming = info.asr.iter().any(|asr| asr.supports_transcript_streaming);
            tracing::debug!(
                target: "fono::wyoming",
                server = %self.endpoint(),
                models,
                streaming,
                "wyoming describe ok"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_host_uses_default_port() {
        let (h, p) = parse_uri("kitchen-pc.local").unwrap();
        assert_eq!(h, "kitchen-pc.local");
        assert_eq!(p, 10300);
    }

    #[test]
    fn parse_host_port() {
        let (h, p) = parse_uri("192.168.1.50:10301").unwrap();
        assert_eq!(h, "192.168.1.50");
        assert_eq!(p, 10301);
    }

    #[test]
    fn parse_tcp_scheme_strips() {
        let (h, p) = parse_uri("tcp://kitchen-pc.local:10300").unwrap();
        assert_eq!(h, "kitchen-pc.local");
        assert_eq!(p, 10300);
    }

    #[test]
    fn parse_wyoming_scheme_strips() {
        let (h, p) = parse_uri("wyoming://server").unwrap();
        assert_eq!(h, "server");
        assert_eq!(p, 10300);
    }

    #[test]
    fn parse_ipv6_with_port() {
        let (h, p) = parse_uri("[::1]:10300").unwrap();
        assert_eq!(h, "::1");
        assert_eq!(p, 10300);
    }

    #[test]
    fn parse_empty_errors() {
        assert!(parse_uri("").is_err());
        assert!(parse_uri("tcp://").is_err());
    }

    #[test]
    fn pcm_quantiser_saturates() {
        let pcm = vec![0.0_f32, 1.0, -1.0, 2.0, -2.0, 0.5];
        let bytes = pcm_f32_to_i16_le(&pcm);
        assert_eq!(bytes.len(), pcm.len() * 2);
        // 0.0
        assert_eq!(&bytes[0..2], &0_i16.to_le_bytes());
        // 1.0 → 32767
        assert_eq!(&bytes[2..4], &32767_i16.to_le_bytes());
        // -1.0 → -32767 (saturated, not -32768)
        assert_eq!(&bytes[4..6], &(-32767_i16).to_le_bytes());
        // 2.0 saturates to 32767
        assert_eq!(&bytes[6..8], &32767_i16.to_le_bytes());
        // -2.0 saturates to -32767
        assert_eq!(&bytes[8..10], &(-32767_i16).to_le_bytes());
        // 0.5 → 16383 (truncating cast)
        assert_eq!(&bytes[10..12], &16383_i16.to_le_bytes());
    }

    #[test]
    fn builder_clears_blank_model_and_token() {
        let s = WyomingStt::from_uri("server")
            .unwrap()
            .with_model("")
            .with_auth_token(Some("   ".into()));
        assert!(s.model.is_none());
        assert!(s.auth_token.is_none());
    }

    #[test]
    fn builder_keeps_real_model_and_token() {
        let s = WyomingStt::from_uri("server")
            .unwrap()
            .with_model("whisper-large-v3")
            .with_auth_token(Some("secret".into()));
        assert_eq!(s.model.as_deref(), Some("whisper-large-v3"));
        assert_eq!(s.auth_token.as_deref(), Some("secret"));
    }
}
