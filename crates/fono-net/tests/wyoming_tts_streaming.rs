// SPDX-License-Identifier: GPL-3.0-only
//! Streamed text-to-speech over a real socket, driven the way Home
//! Assistant drives it: `synthesize-start`, text in `synthesize-chunk`s, the
//! whole text again as a plain `synthesize`, then `synthesize-stop`.

#![cfg(feature = "wyoming-server")]

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use fono_net::wyoming::server::{AdvertisedVoice, WyomingServer, WyomingServerConfig};
use fono_net_codec::wyoming::{
    Info, Synthesize, SynthesizeChunk, AUDIO_CHUNK, AUDIO_START, AUDIO_STOP, DESCRIBE, INFO,
    SYNTHESIZE, SYNTHESIZE_CHUNK, SYNTHESIZE_START, SYNTHESIZE_STOP, SYNTHESIZE_STOPPED,
};
use fono_net_codec::Frame;
use fono_stt::traits::{SpeechToText, Transcription};
use fono_tts::traits::{TextToSpeech, TtsAudio};
use serde_json::{json, to_value};
use tokio::io::BufReader;
use tokio::net::TcpStream;

struct NoStt;

#[async_trait]
impl SpeechToText for NoStt {
    async fn transcribe(&self, _: &[f32], _: u32, _: Option<&str>) -> Result<Transcription> {
        Err(anyhow!("not used"))
    }
    fn name(&self) -> &'static str {
        "none"
    }
}

/// Ten samples per word, and a record of every text it was asked to speak.
struct WordTts(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl TextToSpeech for WordTts {
    async fn synthesize(&self, text: &str, _: Option<&str>, _: Option<&str>) -> Result<TtsAudio> {
        self.0.lock().unwrap().push(text.to_string());
        Ok(TtsAudio { pcm: vec![0.1; 10 * text.split_whitespace().count()], sample_rate: 16_000 })
    }
    fn name(&self) -> &'static str {
        "words"
    }
    fn native_sample_rate(&self) -> u32 {
        16_000
    }
}

async fn start(spoken: Arc<Mutex<Vec<String>>>) -> fono_net::wyoming::server::WyomingServerHandle {
    WyomingServer::with_fixed_stt(
        WyomingServerConfig {
            port: 0,
            tts_voices: vec![AdvertisedVoice {
                name: "words".into(),
                languages: vec!["en".into()],
                description: None,
                version: None,
            }],
            ..WyomingServerConfig::default()
        },
        Arc::new(NoStt),
    )
    .with_fixed_tts(Arc::new(WordTts(spoken)))
    .start()
    .await
    .expect("server starts")
}

async fn send(wr: &mut tokio::net::tcp::OwnedWriteHalf, kind: &str, data: serde_json::Value) {
    Frame::new(kind).with_data(data).write_async(wr).await.unwrap();
}

#[tokio::test]
async fn streamed_text_is_spoken_once_and_audio_starts_before_the_text_ends() {
    let spoken = Arc::new(Mutex::new(Vec::new()));
    let server = start(Arc::clone(&spoken)).await;
    let sock = TcpStream::connect(server.local_addr()).await.unwrap();
    let (rd, mut wr) = sock.into_split();
    let mut rd = BufReader::new(rd);

    send(&mut wr, DESCRIBE, json!({})).await;
    let info = Frame::read_async(&mut rd).await.unwrap();
    assert_eq!(info.kind, INFO);
    let info: Info = serde_json::from_value(info.data).unwrap();
    assert!(info.tts[0].supports_synthesize_streaming);

    let first = "The kitchen light is on now, as you asked. ";
    let rest = "The hallway light is off, and the door is locked.";
    send(&mut wr, SYNTHESIZE_START, json!({})).await;
    let chunk = SynthesizeChunk { text: first.into() };
    send(&mut wr, SYNTHESIZE_CHUNK, to_value(&chunk).unwrap()).await;

    // The first sentence is spoken while the rest of the text has not
    // even been sent.
    assert_eq!(Frame::read_async(&mut rd).await.unwrap().kind, AUDIO_START);
    let early = Frame::read_async(&mut rd).await.unwrap();
    assert_eq!(early.kind, AUDIO_CHUNK);
    let mut samples = early.payload.len() / 2;

    let chunk = SynthesizeChunk { text: rest.into() };
    send(&mut wr, SYNTHESIZE_CHUNK, to_value(&chunk).unwrap()).await;
    let full = Synthesize { text: format!("{first}{rest}"), voice: None };
    send(&mut wr, SYNTHESIZE, to_value(&full).unwrap()).await;
    send(&mut wr, SYNTHESIZE_STOP, json!({})).await;

    let words = format!("{first}{rest}").split_whitespace().count();
    let mut saw_stop = false;
    loop {
        let f = Frame::read_async(&mut rd).await.unwrap();
        match f.kind.as_str() {
            AUDIO_CHUNK => samples += f.payload.len() / 2,
            AUDIO_STOP => saw_stop = true,
            SYNTHESIZE_STOPPED => break,
            other => panic!("unexpected {other}"),
        }
    }
    assert!(saw_stop);
    assert_eq!(samples, 10 * words, "every word once, nothing twice");
    let spoken = spoken.lock().unwrap().clone();
    assert_eq!(spoken.join(" ").split_whitespace().count(), words, "{spoken:?}");

    server.shutdown().await;
}
