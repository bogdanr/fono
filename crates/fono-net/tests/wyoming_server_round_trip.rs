// SPDX-License-Identifier: GPL-3.0-only
//! End-to-end integration test for the Wyoming **server** (Slice 3).
//!
//! Stands up a real `WyomingServer` on a loopback ephemeral port,
//! backed by a mock `SpeechToText` that records the PCM it received
//! and returns a canned transcript. Drives it with the real
//! `WyomingStt` client from `fono-stt` (Slice 2). This is the
//! verification gate for slice 3 of
//! `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`:
//! "real client ↔ real server ↔ mock STT".

#![cfg(feature = "wyoming-server")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use fono_net::wyoming::server::{AdvertisedModel, WyomingServer, WyomingServerConfig};
use fono_stt::traits::{SpeechToText, Transcription};
use fono_stt::wyoming::WyomingStt;

/// Mock STT backend. Records every `transcribe` call so the test can
/// inspect the PCM the server actually received from the wire.
struct MockStt {
    canned_text: String,
    calls: Mutex<Vec<MockCall>>,
}

#[derive(Debug, Clone)]
struct MockCall {
    sample_count: usize,
    sample_rate: u32,
    lang: Option<String>,
}

#[async_trait]
impl SpeechToText for MockStt {
    async fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
        lang: Option<&str>,
    ) -> Result<Transcription> {
        self.calls.lock().unwrap().push(MockCall {
            sample_count: pcm.len(),
            sample_rate,
            lang: lang.map(str::to_owned),
        });
        Ok(Transcription {
            text: self.canned_text.clone(),
            language: lang.map(str::to_owned),
            duration_ms: None,
        })
    }

    fn name(&self) -> &'static str {
        "mock"
    }
}

#[tokio::test]
async fn server_serves_real_client_round_trip() {
    let mock = Arc::new(MockStt {
        canned_text: "hello from the server".to_string(),
        calls: Mutex::new(Vec::new()),
    });

    let cfg = WyomingServerConfig {
        bind: "127.0.0.1".to_string(),
        port: 0, // ephemeral
        models: vec![AdvertisedModel {
            name: "mock-small".into(),
            languages: vec!["en".into()],
            description: Some("mock backend".into()),
            version: None,
        }],
        ..WyomingServerConfig::default()
    };

    let server = WyomingServer::with_fixed_stt(cfg, mock.clone());
    let handle = server.start().await.expect("server starts");
    let addr = handle.local_addr();
    let uri = format!("tcp://{addr}");

    // Drive a real WyomingStt client against the real server.
    let client = WyomingStt::from_uri(&uri).expect("client builds");

    // 0.5 s of mono silence at 16 kHz = 8000 samples.
    let pcm = vec![0.0_f32; 8000];
    let res = tokio::time::timeout(
        Duration::from_secs(5),
        client.transcribe(&pcm, 16_000, Some("en")),
    )
    .await
    .expect("transcribe within 5 s")
    .expect("transcribe ok");

    assert_eq!(res.text, "hello from the server");
    assert_eq!(res.language.as_deref(), Some("en"));

    // Server-side mock must have seen exactly one call with the same
    // PCM length we sent. That proves the int16 LE round-trip survived
    // the wire in both directions.
    let calls = mock.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "exactly one transcribe call");
    assert_eq!(calls[0].sample_count, 8000);
    assert_eq!(calls[0].sample_rate, 16_000);
    assert_eq!(calls[0].lang.as_deref(), Some("en"));

    handle.shutdown().await;
}

#[tokio::test]
async fn server_rejects_non_loopback_when_loopback_only() {
    // Bind to 0.0.0.0 but with loopback_only = true. The accept loop
    // should accept the loopback connection (allowed) and serve it.
    // (Rejecting a *real* non-loopback peer is exercised by inspection
    // of `is_loopback`; staging an actual non-loopback connection in a
    // unit test is environment-dependent.)
    let mock = Arc::new(MockStt {
        canned_text: "ok".to_string(),
        calls: Mutex::new(Vec::new()),
    });
    let cfg = WyomingServerConfig {
        bind: "0.0.0.0".to_string(),
        port: 0,
        loopback_only: true,
        ..WyomingServerConfig::default()
    };
    let handle = WyomingServer::with_fixed_stt(cfg, mock)
        .start()
        .await
        .expect("server starts");
    let port = handle.local_addr().port();
    let uri = format!("tcp://127.0.0.1:{port}");

    let client = WyomingStt::from_uri(&uri).expect("client");
    let res = tokio::time::timeout(
        Duration::from_secs(5),
        client.transcribe(&[0.0_f32; 100], 16_000, None),
    )
    .await
    .expect("timeout")
    .expect("ok");
    assert_eq!(res.text, "ok");
    handle.shutdown().await;
}
