// SPDX-License-Identifier: GPL-3.0-only
//! Wyoming TTS client backend.
//!
//! Mirrors the shape of `fono_stt::wyoming`: one TCP connection per
//! `synthesize()` call, request is the `synthesize` event, response is
//! the standard `audio-start` / `audio-chunk`+ / `audio-stop` framed
//! PCM. Talks to any wyoming-protocol TTS server (`wyoming-piper`,
//! `wyoming-openvoice`, future `fono serve wyoming-tts`).
//!
//! Wire format lives in `fono-net-codec`; this module orchestrates
//! the event sequence and converts the server's int16 LE PCM to mono
//! `f32`.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fono_net_codec::wyoming::{
    AudioChunk, AudioStart, Info, Synthesize, Voice, AUDIO_CHUNK, AUDIO_START, AUDIO_STOP,
    DESCRIBE, INFO, SYNTHESIZE,
};
use fono_net_codec::Frame;
use serde_json::to_value;
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::traits::{TextToSpeech, TtsAudio};

const DEFAULT_PORT: u16 = 10200;
/// Best-effort connect timeout; servers should be sub-second on LAN.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Cap end-to-end synthesise wall-time. 60 seconds is generous for a
/// typical sentence; assistant responses are streamed sentence-by-
/// sentence so individual calls are short.
const SYNTHESIZE_TIMEOUT: Duration = Duration::from_secs(60);
/// Best guess at the most common wyoming-piper voice rate. Used only
/// as the [`TextToSpeech::native_sample_rate`] hint; the actual
/// `TtsAudio.sample_rate` is read from the server's `audio-start`.
const NATIVE_RATE_HINT: u32 = 22050;

/// Parsed `(host, port)` pair from a Wyoming URI.
fn parse_uri(uri: &str) -> Result<(String, u16)> {
    let stripped = uri
        .trim()
        .strip_prefix("tcp://")
        .or_else(|| uri.trim().strip_prefix("wyoming://"))
        .unwrap_or_else(|| uri.trim());
    if stripped.is_empty() {
        return Err(anyhow!("wyoming TTS URI is empty"));
    }
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

/// Decode a buffer of int16 LE bytes into mono `f32` samples in
/// the -1.0..1.0 range. A trailing odd byte is silently dropped.
fn pcm_i16_le_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32767.0)
        .collect()
}

/// Wyoming TTS client. One instance per configured server; a fresh
/// TCP connection is opened per synthesize call.
pub struct WyomingTts {
    host: String,
    port: u16,
    /// Reserved for the `fono.auth` extension (Wyoming v1 has no
    /// in-band auth). Plumbed but unused today.
    auth_token: Option<String>,
}

impl WyomingTts {
    pub fn from_uri(uri: &str) -> Result<Self> {
        let (host, port) = parse_uri(uri)?;
        Ok(Self {
            host,
            port,
            auth_token: None,
        })
    }

    #[must_use]
    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.auth_token = token.filter(|t| !t.trim().is_empty());
        self
    }

    async fn connect(&self) -> Result<TcpStream> {
        let addr = format!("{}:{}", self.host, self.port);
        let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .with_context(|| format!("connecting to wyoming TTS server at {addr} timed out"))?
            .with_context(|| format!("connecting to wyoming TTS server at {addr}"))?;
        stream.set_nodelay(true).ok();
        Ok(stream)
    }

    fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[async_trait]
impl TextToSpeech for WyomingTts {
    fn name(&self) -> &'static str {
        "wyoming"
    }

    fn native_sample_rate(&self) -> u32 {
        NATIVE_RATE_HINT
    }

    async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        lang: Option<&str>,
    ) -> Result<TtsAudio> {
        if text.is_empty() {
            return Ok(TtsAudio {
                pcm: Vec::new(),
                sample_rate: NATIVE_RATE_HINT,
            });
        }
        let stream = self.connect().await?;
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // 1. synthesize request.
        let voice_obj = match (voice, lang) {
            (None, None) => None,
            (v, l) => {
                let voice = Voice {
                    name: v.filter(|s| !s.trim().is_empty()).map(str::to_string),
                    language: l.filter(|s| !s.trim().is_empty()).map(str::to_string),
                    speaker: None,
                };
                if voice.is_empty() {
                    None
                } else {
                    Some(voice)
                }
            }
        };
        let req = Synthesize {
            text: text.to_string(),
            voice: voice_obj,
        };
        Frame::new(SYNTHESIZE)
            .with_data(to_value(&req)?)
            .write_async(&mut write_half)
            .await?;

        // 2. read audio-start → audio-chunk* → audio-stop, accumulating PCM.
        let mut sample_rate: u32 = NATIVE_RATE_HINT;
        let mut pcm: Vec<f32> = Vec::new();
        let started = std::time::Instant::now();

        let result = timeout(SYNTHESIZE_TIMEOUT, async {
            loop {
                let f = Frame::read_async(&mut reader).await.with_context(|| {
                    format!("reading from wyoming TTS server at {}", self.endpoint())
                })?;
                match f.kind.as_str() {
                    AUDIO_START => {
                        let s: AudioStart = serde_json::from_value(f.data)
                            .context("decoding wyoming `audio-start` event")?;
                        sample_rate = s.rate;
                    }
                    AUDIO_CHUNK => {
                        // Some servers put rate/width in `data`; we
                        // honour `audio-start` for the format and
                        // append payload bytes regardless.
                        let _hdr: AudioChunk =
                            serde_json::from_value(f.data).unwrap_or(AudioChunk {
                                rate: sample_rate,
                                width: 2,
                                channels: 1,
                                timestamp: None,
                            });
                        pcm.extend(pcm_i16_le_to_f32(&f.payload));
                    }
                    AUDIO_STOP => {
                        return Ok::<(), anyhow::Error>(());
                    }
                    other => {
                        tracing::trace!(
                            target: "fono::tts::wyoming",
                            event = other,
                            "unexpected event ignored"
                        );
                    }
                }
            }
        })
        .await;

        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(anyhow!(
                    "wyoming TTS server at {} did not finish synthesis within {:?}",
                    self.endpoint(),
                    SYNTHESIZE_TIMEOUT
                ));
            }
        }

        tracing::debug!(
            target: "fono::tts::wyoming",
            samples = pcm.len(),
            rate = sample_rate,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "synthesise ok"
        );

        Ok(TtsAudio { pcm, sample_rate })
    }

    async fn prewarm(&self) -> Result<()> {
        // Open a connection, send `describe`, read `info`, drop. Pays
        // TCP handshake + first JSON parse before the user's first
        // F10 press.
        let stream = self.connect().await?;
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        Frame::new(DESCRIBE).write_async(&mut write_half).await?;
        let f = timeout(Duration::from_secs(5), Frame::read_async(&mut reader))
            .await
            .with_context(|| {
                format!(
                    "wyoming TTS describe to {} timed out (server unreachable?)",
                    self.endpoint()
                )
            })??;
        if f.kind == INFO {
            let info: Info = serde_json::from_value(f.data).unwrap_or_default();
            let voices = info.tts.iter().map(|p| p.voices.len()).sum::<usize>();
            tracing::debug!(
                target: "fono::tts::wyoming",
                server = %self.endpoint(),
                voices,
                "wyoming TTS describe ok"
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
        let (h, p) = parse_uri("piper.local").unwrap();
        assert_eq!(h, "piper.local");
        assert_eq!(p, 10200);
    }

    #[test]
    fn parse_host_port_overrides_default() {
        let (h, p) = parse_uri("192.168.1.50:10301").unwrap();
        assert_eq!(h, "192.168.1.50");
        assert_eq!(p, 10301);
    }

    #[test]
    fn parse_tcp_scheme_strips() {
        let (h, p) = parse_uri("tcp://piper.local:10200").unwrap();
        assert_eq!(h, "piper.local");
        assert_eq!(p, 10200);
    }

    #[test]
    fn parse_ipv6_with_port() {
        let (h, p) = parse_uri("[::1]:10200").unwrap();
        assert_eq!(h, "::1");
        assert_eq!(p, 10200);
    }

    #[test]
    fn parse_empty_errors() {
        assert!(parse_uri("").is_err());
        assert!(parse_uri("tcp://").is_err());
    }

    #[test]
    fn pcm_i16_le_decode_round_trips() {
        let bytes: Vec<u8> = [0_i16, 32767, -32767, 16383]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let f = pcm_i16_le_to_f32(&bytes);
        assert_eq!(f.len(), 4);
        assert!((f[0]).abs() < 1e-6);
        assert!((f[1] - 1.0).abs() < 1e-3);
        assert!((f[2] - -1.0).abs() < 1e-3);
        assert!((f[3] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn pcm_i16_le_drops_trailing_odd_byte() {
        let f = pcm_i16_le_to_f32(&[0, 0, 1]);
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn empty_text_returns_empty_audio_without_connecting() {
        // Calls synthesize on a client pointing at an unreachable port.
        // Empty `text` must short-circuit before opening a connection.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = WyomingTts::from_uri("127.0.0.1:1").unwrap();
        let audio = rt
            .block_on(client.synthesize("", None, None))
            .expect("empty text must not error");
        assert!(audio.pcm.is_empty());
    }
}
