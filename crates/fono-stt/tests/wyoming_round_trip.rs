// SPDX-License-Identifier: GPL-3.0-only
//! End-to-end integration tests for the Wyoming STT client.
//!
//! Stands up a minimal in-process Wyoming server on a loopback ephemeral
//! port and drives a real `WyomingStt` client against it. Covers two
//! flows from Slice 2 of
//! `plans/2026-04-29-2026-04-29-client-server-wyoming-fono-and-mdns-v2.md`:
//!
//! * the non-streaming path (`audio-start` → chunked `audio-chunk` →
//!   `audio-stop` → `transcribe` → single `transcript`); and
//! * the streaming path (`transcript-start` → N × `transcript-chunk` →
//!   `transcript-stop` with no final `transcript` envelope).
//!
//! Both assert `Transcription.text` matches the canned response and
//! verify the server received the expected PCM length (so the int16 LE
//! quantiser is exercised over the wire, not just in unit tests).

#![cfg(feature = "wyoming")]

use std::time::Duration;

use fono_net_codec::wyoming::{
    AudioChunk, AudioStart, AudioStop, Transcribe, Transcript, TranscriptChunk, TranscriptStart,
    AUDIO_CHUNK, AUDIO_START, AUDIO_STOP, DESCRIBE, INFO, TRANSCRIBE, TRANSCRIPT, TRANSCRIPT_CHUNK,
    TRANSCRIPT_START, TRANSCRIPT_STOP,
};
use fono_net_codec::Frame;
use fono_stt::traits::SpeechToText;
use fono_stt::wyoming::WyomingStt;
use serde_json::{to_value, Value};
use tokio::io::BufReader;
use tokio::net::TcpListener;

/// Spawn the in-process server. Returns `(uri, transcribed_bytes_rx)`.
/// The `mode` selects which of the two response shapes the server uses.
async fn spawn_server(
    mode: ServerMode,
    canned: &'static str,
) -> (String, tokio::sync::oneshot::Receiver<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let uri = format!("tcp://{addr}");

    let (tx_bytes, rx_bytes) = tokio::sync::oneshot::channel::<usize>();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.expect("accept");
        let (rd, mut wr) = sock.into_split();
        let mut reader = BufReader::new(rd);
        let mut audio_bytes_total: usize = 0;
        let mut got_audio_start = false;
        let mut got_audio_stop = false;
        loop {
            let Ok(frame) = Frame::read_async(&mut reader).await else {
                return;
            };
            match frame.kind.as_str() {
                DESCRIBE => {
                    // Reply with a minimal `info`. Used by `prewarm` only;
                    // not exercised in this test, but keeps the server
                    // honest for future test additions.
                    let info = serde_json::json!({
                        "asr": {
                            "models": [],
                            "supports_transcript_streaming": matches!(mode, ServerMode::Streaming),
                        }
                    });
                    Frame::new(INFO)
                        .with_data(info)
                        .write_async(&mut wr)
                        .await
                        .ok();
                }
                AUDIO_START => {
                    let _: AudioStart =
                        serde_json::from_value(frame.data).expect("audio-start parse");
                    got_audio_start = true;
                }
                AUDIO_CHUNK => {
                    let _: AudioChunk =
                        serde_json::from_value(frame.data).expect("audio-chunk parse");
                    audio_bytes_total += frame.payload.len();
                }
                AUDIO_STOP => {
                    let _: AudioStop =
                        serde_json::from_value(frame.data).expect("audio-stop parse");
                    got_audio_stop = true;
                }
                TRANSCRIBE => {
                    let _: Transcribe =
                        serde_json::from_value(frame.data).expect("transcribe parse");
                    assert!(got_audio_start, "audio-start must precede transcribe");
                    assert!(got_audio_stop, "audio-stop must precede transcribe");
                    match mode {
                        ServerMode::OneShot => {
                            let t = Transcript {
                                text: canned.to_string(),
                                language: Some("en".into()),
                            };
                            Frame::new(TRANSCRIPT)
                                .with_data(to_value(&t).unwrap())
                                .write_async(&mut wr)
                                .await
                                .ok();
                        }
                        ServerMode::Streaming => {
                            // Emit transcript-start, then stream the canned
                            // text in 4-char chunks, then transcript-stop —
                            // intentionally with NO final `transcript`.
                            Frame::new(TRANSCRIPT_START)
                                .with_data(
                                    to_value(&TranscriptStart {
                                        language: Some("en".into()),
                                    })
                                    .unwrap(),
                                )
                                .write_async(&mut wr)
                                .await
                                .ok();
                            for piece in chunked(canned, 4) {
                                let c = TranscriptChunk { text: piece.into() };
                                Frame::new(TRANSCRIPT_CHUNK)
                                    .with_data(to_value(&c).unwrap())
                                    .write_async(&mut wr)
                                    .await
                                    .ok();
                            }
                            Frame::new(TRANSCRIPT_STOP)
                                .with_data(Value::Object(serde_json::Map::new()))
                                .write_async(&mut wr)
                                .await
                                .ok();
                        }
                    }
                    let _ = tx_bytes.send(audio_bytes_total);
                    return;
                }
                _ => { /* ignore unknown */ }
            }
        }
    });
    (uri, rx_bytes)
}

#[derive(Copy, Clone)]
enum ServerMode {
    OneShot,
    Streaming,
}

fn chunked(s: &str, n: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let end = (i + n).min(s.len());
        // Snap to char boundary to avoid slicing inside a UTF-8 codepoint.
        let mut e = end;
        while !s.is_char_boundary(e) && e > i {
            e -= 1;
        }
        if e == i {
            e = (i + n).min(s.len());
            while !s.is_char_boundary(e) && e < s.len() {
                e += 1;
            }
        }
        out.push(&s[i..e]);
        i = e;
    }
    out
}

#[tokio::test]
async fn one_shot_round_trip() {
    let (uri, rx) = spawn_server(ServerMode::OneShot, "hello world").await;
    let stt = WyomingStt::from_uri(&uri).expect("uri");
    // 0.25 s of silence at 16 kHz = 4000 samples → 8000 bytes int16 LE.
    let pcm = vec![0.0_f32; 4000];
    let res = tokio::time::timeout(Duration::from_secs(5), stt.transcribe(&pcm, 16_000, None))
        .await
        .expect("client did not time out")
        .expect("transcribe ok");
    assert_eq!(res.text, "hello world");
    assert_eq!(res.language.as_deref(), Some("en"));
    let bytes = rx.await.expect("server bytes channel");
    assert_eq!(
        bytes,
        pcm.len() * 2,
        "all PCM samples must reach server as int16 LE"
    );
}

#[tokio::test]
async fn streaming_round_trip_aggregates_chunks() {
    let (uri, rx) = spawn_server(ServerMode::Streaming, "streaming dictation").await;
    let stt = WyomingStt::from_uri(&uri).expect("uri");
    // 0.1 s of silence at 16 kHz = 1600 samples → 3200 bytes int16 LE.
    let pcm = vec![0.0_f32; 1600];
    let res = tokio::time::timeout(Duration::from_secs(5), stt.transcribe(&pcm, 16_000, None))
        .await
        .expect("client did not time out")
        .expect("transcribe ok");
    assert_eq!(res.text, "streaming dictation");
    let bytes = rx.await.expect("server bytes channel");
    assert_eq!(bytes, pcm.len() * 2);
}
