// SPDX-License-Identifier: GPL-3.0-only
//! The Wyoming conversation service, driven over a real socket the way
//! Home Assistant's Wyoming conversation agent drives it: `describe`, then
//! a `transcript` carrying a `conversation_id`, answered with `handled`.

#![cfg(feature = "wyoming-server")]

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use fono_net::wyoming::server::{
    AdvertisedHandle, HandleProvider, HandleReply, HandleRequest, WyomingServer,
    WyomingServerConfig,
};
use fono_net_codec::wyoming::{
    Handled, Info, NotHandled, Transcript, DESCRIBE, HANDLED, INFO, NOT_HANDLED, TRANSCRIPT,
};
use fono_net_codec::Frame;
use fono_stt::traits::{SpeechToText, Transcription};
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

/// Start a server whose conversation service echoes the request, or fails
/// when the text is "fail". Every request is recorded.
async fn start(
    seen: Arc<Mutex<Vec<HandleRequest>>>,
) -> fono_net::wyoming::server::WyomingServerHandle {
    let cfg = WyomingServerConfig {
        port: 0,
        handle: Some(AdvertisedHandle {
            languages: vec!["en".into()],
            model: "test-model".into(),
            home_control: true,
        }),
        ..WyomingServerConfig::default()
    };
    let provider: HandleProvider = Arc::new(move |req: HandleRequest| {
        seen.lock().unwrap().push(req.clone());
        Box::pin(async move {
            if req.text == "fail" {
                return Err(anyhow!("assistant unavailable"));
            }
            Ok(HandleReply { text: format!("you said {}", req.text), continue_conversation: false })
        })
    });
    WyomingServer::with_fixed_stt(cfg, Arc::new(NoStt))
        .with_handle(provider)
        .start()
        .await
        .expect("server starts")
}

#[tokio::test]
async fn conversation_service_answers_a_transcript() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = start(Arc::clone(&seen)).await;
    let sock = TcpStream::connect(server.local_addr()).await.unwrap();
    let (rd, mut wr) = sock.into_split();
    let mut rd = BufReader::new(rd);

    Frame::new(DESCRIBE).write_async(&mut wr).await.unwrap();
    let info = Frame::read_async(&mut rd).await.unwrap();
    assert_eq!(info.kind, INFO);
    let info: Info = serde_json::from_value(info.data).unwrap();
    assert_eq!(info.handle.len(), 1);
    assert!(info.handle[0].supports_home_control);
    assert_eq!(info.handle[0].models[0].name, "test-model");

    let ctx = json!({ "conversation_id": "abc" });
    let t = Transcript {
        text: "turn on the light".into(),
        language: Some("en".into()),
        context: Some(ctx.clone()),
    };
    Frame::new(TRANSCRIPT).with_data(to_value(&t).unwrap()).write_async(&mut wr).await.unwrap();
    let reply = Frame::read_async(&mut rd).await.unwrap();
    assert_eq!(reply.kind, HANDLED);
    let handled: Handled = serde_json::from_value(reply.data).unwrap();
    assert_eq!(handled.text.as_deref(), Some("you said turn on the light"));
    assert_eq!(handled.context, Some(ctx), "context is echoed back");

    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(req.conversation_id.as_deref(), Some("abc"));
    assert_eq!(req.language.as_deref(), Some("en"));

    // A failure comes back as `not-handled`, on the same connection.
    let t = Transcript { text: "fail".into(), language: None, context: None };
    Frame::new(TRANSCRIPT).with_data(to_value(&t).unwrap()).write_async(&mut wr).await.unwrap();
    let reply = Frame::read_async(&mut rd).await.unwrap();
    assert_eq!(reply.kind, NOT_HANDLED);
    let nh: NotHandled = serde_json::from_value(reply.data).unwrap();
    assert!(nh.text.unwrap().contains("assistant unavailable"));

    server.shutdown().await;
}

#[tokio::test]
async fn no_conversation_service_is_advertised_by_default() {
    let server = WyomingServer::with_fixed_stt(
        WyomingServerConfig { port: 0, ..WyomingServerConfig::default() },
        Arc::new(NoStt),
    )
    .start()
    .await
    .unwrap();
    let sock = TcpStream::connect(server.local_addr()).await.unwrap();
    let (rd, mut wr) = sock.into_split();
    let mut rd = BufReader::new(rd);
    Frame::new(DESCRIBE).write_async(&mut wr).await.unwrap();
    let info: Info =
        serde_json::from_value(Frame::read_async(&mut rd).await.unwrap().data).unwrap();
    assert!(info.handle.is_empty());
    server.shutdown().await;
}
