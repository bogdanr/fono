// SPDX-License-Identifier: GPL-3.0-only
//! End-to-end integration test for the Wyoming **wake** server path
//! (Phase H of `plans/2026-06-23-wake-word-openwakeword-v2.md`).
//!
//! Stands up a real `WyomingServer` on a loopback ephemeral port with a
//! mock STT plus a bound wake detector (the deterministic `EnergyWakeStub`
//! from `fono-audio`, configured with an energy threshold so a loud hop
//! fires predictably). Drives it with raw Wyoming frames over a TCP
//! socket and asserts:
//!
//! 1. `describe` → `info` advertises a `wake` program with the configured
//!    models (encode → decode of the new `WakeProgram`/`WakeModel` path).
//! 2. streaming a loud `audio-chunk` makes the detector fire and the
//!    server emits a `detection` event naming the fired phrase.
//!
//! No real audio device, no external services — fully deterministic, the
//! same pattern as `wyoming_server_round_trip.rs`.

#![cfg(feature = "wyoming-server")]

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use fono_audio::wakeword::EnergyWakeStub;
use fono_net::wyoming::server::{
    AdvertisedWakeModel, WakeProvider, WyomingServer, WyomingServerConfig,
};
use fono_net_codec::wyoming::{
    AudioChunk, AudioStart, Detect, Detection, Info, AUDIO_CHUNK, AUDIO_START, DESCRIBE, DETECT,
    DETECTION, INFO,
};
use fono_net_codec::Frame;
use fono_stt::traits::{SpeechToText, Transcription};
use serde_json::to_value;
use tokio::io::BufReader;
use tokio::net::TcpStream;

/// Mock STT backend — never actually called on the wake path, but the
/// server requires one to construct.
struct MockStt;

#[async_trait]
impl SpeechToText for MockStt {
    async fn transcribe(
        &self,
        _pcm: &[f32],
        _sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        Ok(Transcription {
            text: String::new(),
            language: lang.map(str::to_owned),
            duration_ms: None,
        })
    }
    fn name(&self) -> &'static str {
        "mock"
    }
}

/// Build a server that advertises a `hey_fono` wake model and drives an
/// energy stub that fires when a hop's RMS crosses `0.5`.
fn wake_server() -> WyomingServer {
    let cfg = WyomingServerConfig {
        bind: "127.0.0.1".to_string(),
        port: 0,
        wake_models: vec![AdvertisedWakeModel {
            name: "hey_fono".into(),
            languages: vec!["en".into()],
            phrase: Some("hey fono".into()),
            description: Some("default clean-license model".into()),
            version: None,
        }],
        ..WyomingServerConfig::default()
    };
    let wake: WakeProvider = Arc::new(|| Box::new(EnergyWakeStub::with_threshold(0.5, "hey_fono")));
    WyomingServer::with_fixed_stt(cfg, Arc::new(MockStt)).with_wake(wake)
}

#[tokio::test]
async fn describe_advertises_wake_program() {
    let handle = wake_server().start().await.expect("server starts");
    let addr = handle.local_addr();
    let mut sock = TcpStream::connect(addr).await.expect("connect");

    Frame::new(DESCRIBE).write_async(&mut sock).await.unwrap();

    let mut reader = BufReader::new(sock);
    let resp = tokio::time::timeout(Duration::from_secs(5), Frame::read_async(&mut reader))
        .await
        .expect("info within 5 s")
        .expect("info frame");
    assert_eq!(resp.kind, INFO);
    let info: Info = serde_json::from_value(resp.data).unwrap();
    assert_eq!(info.wake.len(), 1, "exactly one wake program advertised");
    assert_eq!(info.wake[0].models.len(), 1);
    assert_eq!(info.wake[0].models[0].name, "hey_fono");
    assert_eq!(info.wake[0].models[0].phrase.as_deref(), Some("hey fono"));

    handle.shutdown().await;
}

#[tokio::test]
async fn loud_audio_chunk_emits_detection() {
    let handle = wake_server().start().await.expect("server starts");
    let addr = handle.local_addr();
    let mut sock = TcpStream::connect(addr).await.expect("connect");

    // Open a detection session, then stream one loud hop (1280 samples at
    // 16 kHz) as int16 LE. ~0.9 amplitude clears the stub's 0.5 RMS gate.
    Frame::new(DETECT)
        .with_data(to_value(Detect::default()).unwrap())
        .write_async(&mut sock)
        .await
        .unwrap();
    Frame::new(AUDIO_START)
        .with_data(
            to_value(AudioStart { rate: 16_000, width: 2, channels: 1, timestamp: None }).unwrap(),
        )
        .write_async(&mut sock)
        .await
        .unwrap();

    // 1280 samples (one HOP_SAMPLES window) of ~0.9 full-scale int16.
    let loud = (0.9_f32 * 32767.0) as i16;
    let mut payload = Vec::with_capacity(1280 * 2);
    for _ in 0..1280 {
        payload.extend_from_slice(&loud.to_le_bytes());
    }
    Frame::new(AUDIO_CHUNK)
        .with_data(
            to_value(AudioChunk { rate: 16_000, width: 2, channels: 1, timestamp: None }).unwrap(),
        )
        .with_payload(payload)
        .write_async(&mut sock)
        .await
        .unwrap();

    let mut reader = BufReader::new(sock);
    let resp = tokio::time::timeout(Duration::from_secs(5), Frame::read_async(&mut reader))
        .await
        .expect("detection within 5 s")
        .expect("detection frame");
    assert_eq!(resp.kind, DETECTION);
    let detection: Detection = serde_json::from_value(resp.data).unwrap();
    assert_eq!(detection.name, "hey_fono");
    assert!(detection.timestamp.is_some(), "detection carries a timestamp");

    handle.shutdown().await;
}

#[tokio::test]
async fn no_wake_provider_advertises_no_wake_service() {
    // A plain STT server (no `with_wake`) must not advertise a wake
    // program — the wake direction is strictly opt-in.
    let mock = Arc::new(MockStt);
    let handle =
        WyomingServer::with_fixed_stt(WyomingServerConfig { port: 0, ..Default::default() }, mock)
            .start()
            .await
            .expect("server starts");
    let addr = handle.local_addr();
    let mut sock = TcpStream::connect(addr).await.expect("connect");
    Frame::new(DESCRIBE).write_async(&mut sock).await.unwrap();
    let mut reader = BufReader::new(sock);
    let resp = Frame::read_async(&mut reader).await.expect("info frame");
    let info: Info = serde_json::from_value(resp.data).unwrap();
    assert!(info.wake.is_empty(), "no wake provider => no wake program advertised");
    handle.shutdown().await;
}
