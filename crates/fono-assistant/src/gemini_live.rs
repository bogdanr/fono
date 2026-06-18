// SPDX-License-Identifier: GPL-3.0-only
//! Gemini Live (`BidiGenerateContent`) realtime client — a *native*
//! WebSocket against
//! `wss://generativelanguage.googleapis.com/ws/...BidiGenerateContent`.
//!
//! Unlike the staged Gemini path (STT → LLM → batch TTS), this opens one
//! bidirectional WebSocket where the model owns VAD, transcription, and audio
//! synthesis: mic PCM streams in, reply audio streams back as one continuous
//! voice. This fixes both pains of the staged path — per-sentence voice drift
//! (each `:generateContent` TTS call drifts in timbre) and the ~6 s/sentence
//! batch latency (each call returns its whole clip as one terminal block).
//!
//! Lifecycle (push-to-talk, one-shot per F8 press):
//!
//! 1. Connect to the Live URL with `?key=<API_KEY>` on the upgrade request.
//! 2. Send the `setup` message (model, `responseModalities: ["AUDIO"]`, voice,
//!    system instruction, input/output transcription on).
//! 3. Await `setupComplete` before forwarding any audio.
//! 4. Writer task: drain mic PCM from the caller's `audio_in`, send each chunk
//!    as `realtimeInput.audio` (s16le base64). When `audio_in` closes
//!    (user released the key), send `realtimeInput.audioStreamEnd: true` so the
//!    model finalises the turn.
//! 5. Reader task: map `serverContent` frames to [`RealtimeEvent`] — inline PCM
//!    parts → `Audio`, output transcription → `AssistantTextDelta`, input
//!    transcription → `UserTextFinal`, `turnComplete` → `Done` (then stop).
//!
//! Tool-calling is intentionally absent (Path B: audio loop first). A
//! `toolCall` server message plus a tool-response client message will be added
//! when `fono-action` lands.
//!
//! Increment 2 of the realtime arc; see the realtime design plan.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures::stream::{BoxStream, StreamExt};
use futures::SinkExt;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::traits::{AssistantContext, RealtimeAssistant, RealtimeEvent, RealtimeSession};

/// Mic-input channel depth (PCM frames). Bounded so a stalled socket applies
/// backpressure to the capture path rather than growing unboundedly.
const AUDIO_IN_CAPACITY: usize = 64;

/// How long to wait for `setupComplete` after sending the setup message.
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Gemini Live realtime client. One instance is reused across F8 presses;
/// each [`open_session`](RealtimeAssistant::open_session) opens a fresh
/// WebSocket (one utterance + one reply, then teardown).
pub struct GeminiLive {
    api_key: String,
    model: String,
    ws_url: String,
    voice: String,
    input_rate: u32,
    output_rate: u32,
}

impl GeminiLive {
    /// Construct from explicit parameters. The factory maps a catalogue
    /// `RealtimeProfile` (+ resolved key and voice) onto these.
    #[must_use]
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        ws_url: impl Into<String>,
        voice: impl Into<String>,
        input_rate: u32,
        output_rate: u32,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            ws_url: ws_url.into(),
            voice: voice.into(),
            input_rate,
            output_rate,
        }
    }
}

/// Build the `setup` message that opens a Live session. Exposed for tests.
///
/// `model` is normalised to a `models/<id>` resource name (the API rejects a
/// bare id). When `system_prompt` is empty the `systemInstruction` field is
/// omitted entirely.
#[must_use]
pub fn build_setup_json(model: &str, system_prompt: &str, voice: &str) -> serde_json::Value {
    let model_field =
        if model.starts_with("models/") { model.to_string() } else { format!("models/{model}") };
    let mut setup = json!({
        "model": model_field,
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {
                "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": voice } }
            }
        },
        "inputAudioTranscription": {},
        "outputAudioTranscription": {},
    });
    if !system_prompt.is_empty() {
        setup["systemInstruction"] = json!({ "parts": [{ "text": system_prompt }] });
    }
    json!({ "setup": setup })
}

/// Build a `realtimeInput` audio chunk message. `rate` is stamped into the
/// `mimeType` so the model knows the wire format. Exposed for tests.
#[must_use]
pub fn encode_audio_chunk(pcm: &[f32], rate: u32) -> serde_json::Value {
    let data = BASE64.encode(f32_to_s16le_bytes(pcm));
    json!({
        "realtimeInput": {
            "audio": { "mimeType": format!("audio/pcm;rate={rate}"), "data": data }
        }
    })
}

/// The end-of-input control message: tells the model the user stopped
/// speaking so it can finalise and respond. Exposed for tests.
#[must_use]
pub fn audio_stream_end_json() -> serde_json::Value {
    json!({ "realtimeInput": { "audioStreamEnd": true } })
}

/// Convert f32 PCM in [-1.0, 1.0] to little-endian s16 bytes. Out-of-range
/// values are clamped so they cannot wrap.
fn f32_to_s16le_bytes(pcm: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() * 2);
    for &s in pcm {
        #[allow(clippy::cast_possible_truncation)]
        let i = (s.clamp(-1.0, 1.0) * 32_767.0) as i16;
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}

/// Convert little-endian s16 PCM bytes to f32 in [-1.0, 1.0]. A trailing odd
/// byte (truncated frame) is dropped.
fn s16le_bytes_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(2).map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32_768.0).collect()
}

/// Decode base64 s16le PCM (an `inlineData.data` payload) to f32 samples.
fn decode_inline_pcm(b64: &str) -> Result<Vec<f32>> {
    let bytes = BASE64.decode(b64.as_bytes()).context("gemini live: inline audio not base64")?;
    Ok(s16le_bytes_to_f32(&bytes))
}

/// Parse the sample rate out of a `audio/pcm;rate=24000` mime type, falling
/// back to `default` when absent or unparseable.
fn parse_rate(mime: &str, default: u32) -> u32 {
    mime.split(';')
        .find_map(|p| p.trim().strip_prefix("rate="))
        .and_then(|r| r.parse().ok())
        .unwrap_or(default)
}

/// Subset of a server→client Live message. Every field is `serde(default)`
/// and unknown fields are ignored so additive schema drift cannot break the
/// parser (Gemini Live extends this envelope regularly: `usageMetadata`,
/// `goAway`, `toolCall`, …).
#[derive(Deserialize, Debug, Default)]
struct ServerMessage {
    #[serde(default, rename = "setupComplete")]
    setup_complete: Option<serde_json::Value>,
    #[serde(default, rename = "serverContent")]
    server_content: Option<ServerContent>,
}

#[derive(Deserialize, Debug, Default)]
struct ServerContent {
    #[serde(default, rename = "modelTurn")]
    model_turn: Option<ModelTurn>,
    #[serde(default, rename = "inputTranscription")]
    input_transcription: Option<Transcription>,
    #[serde(default, rename = "outputTranscription")]
    output_transcription: Option<Transcription>,
    #[serde(default)]
    interrupted: bool,
    #[serde(default, rename = "turnComplete")]
    turn_complete: bool,
}

#[derive(Deserialize, Debug, Default)]
struct Transcription {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize, Debug, Default)]
struct ModelTurn {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Deserialize, Debug, Default)]
struct Part {
    #[serde(default, rename = "inlineData")]
    inline_data: Option<InlineData>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct InlineData {
    #[serde(default, rename = "mimeType")]
    mime_type: String,
    #[serde(default)]
    data: String,
}

/// Decode a raw WebSocket frame's bytes into a [`ServerMessage`]. Gemini Live
/// sends its JSON envelopes as **binary** frames (not text), so the reader
/// must attempt a parse on both. Returns `None` for unparseable / non-JSON
/// payloads (ignored for forward-compat).
fn parse_server_frame(bytes: &[u8]) -> Option<ServerMessage> {
    serde_json::from_slice(bytes).ok()
}

#[async_trait]
impl RealtimeAssistant for GeminiLive {
    async fn open_session(&self, ctx: &AssistantContext) -> Result<RealtimeSession> {
        // Auth: Gemini Live takes the API key as a `?key=` query param on the
        // upgrade URL (there is no header form for the WS handshake).
        let url = format!("{base}?key={key}", base = self.ws_url, key = self.api_key);
        let request =
            url.as_str().into_client_request().context("building Gemini Live WS request")?;
        let (ws, _resp) = tokio_tungstenite::connect_async(request)
            .await
            .context("Gemini Live WS connect failed")?;
        let (mut write, mut read) = ws.split();

        // 1. Send setup.
        let setup = build_setup_json(&self.model, &ctx.system_prompt, &self.voice);
        write
            .send(Message::Text(setup.to_string()))
            .await
            .context("Gemini Live: sending setup message")?;

        // 2. Await setupComplete (bounded) before forwarding audio.
        loop {
            match timeout(SETUP_TIMEOUT, read.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    if parse_server_frame(t.as_bytes()).is_some_and(|m| m.setup_complete.is_some())
                    {
                        break;
                    }
                }
                Ok(Some(Ok(Message::Binary(b)))) => {
                    if parse_server_frame(&b).is_some_and(|m| m.setup_complete.is_some()) {
                        break;
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(e))) => return Err(anyhow!("Gemini Live WS error before setup: {e}")),
                Ok(None) => return Err(anyhow!("Gemini Live WS closed before setupComplete")),
                Err(_) => return Err(anyhow!("Gemini Live: timed out waiting for setupComplete")),
            }
        }

        let (audio_tx, audio_rx) = mpsc::channel::<Vec<f32>>(AUDIO_IN_CAPACITY);
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Result<RealtimeEvent>>();

        tokio::spawn(run_reader(read, ev_tx, self.output_rate));
        tokio::spawn(run_writer(write, audio_rx, self.input_rate));

        let events: BoxStream<'static, Result<RealtimeEvent>> =
            UnboundedReceiverStream::new(ev_rx).boxed();
        Ok(RealtimeSession { audio_in: audio_tx, events })
    }

    fn name(&self) -> &'static str {
        "gemini-live"
    }

    fn native_input_rate(&self) -> u32 {
        self.input_rate
    }
}

/// Reader task body: translate `serverContent` frames to [`RealtimeEvent`]s
/// until `turnComplete` (one-shot) or the socket closes. Generic over the
/// read half so it stays decoupled from the concrete TLS stream type.
async fn run_reader<R>(
    mut read: R,
    ev_tx: mpsc::UnboundedSender<Result<RealtimeEvent>>,
    output_rate: u32,
) where
    R: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin
        + Send
        + 'static,
{
    let mut user_emitted = false;
    let mut input_buf = String::new();
    while let Some(frame) = read.next().await {
        let bytes = match frame {
            Ok(Message::Text(t)) => t.into_bytes(),
            Ok(Message::Binary(b)) => b,
            Ok(Message::Close(reason)) => {
                tracing::debug!("gemini live: WS closed: {reason:?}");
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                let _ = ev_tx.send(Err(anyhow!("gemini live WS read error: {e}")));
                break;
            }
        };
        let Some(msg) = parse_server_frame(&bytes) else { continue };
        let Some(content) = msg.server_content else { continue };

        // Barge-in: the model's own VAD detected the user speaking over the
        // reply and is discarding the rest of this turn's audio. Forward it
        // first so the consumer stops playback before any further frames.
        if content.interrupted {
            let _ = ev_tx.send(Ok(RealtimeEvent::Interrupted));
        }

        if let Some(it) = content.input_transcription {
            input_buf.push_str(&it.text);
        }

        // Once the model starts replying, the user's utterance is final —
        // flush it as the user turn before any reply text.
        let model_started = content.model_turn.is_some() || content.output_transcription.is_some();
        if model_started && !user_emitted && !input_buf.is_empty() {
            let _ = ev_tx.send(Ok(RealtimeEvent::UserTextFinal(std::mem::take(&mut input_buf))));
            user_emitted = true;
        }

        if let Some(ot) = content.output_transcription {
            if !ot.text.is_empty() {
                let _ = ev_tx.send(Ok(RealtimeEvent::AssistantTextDelta(ot.text)));
            }
        }

        if let Some(mt) = content.model_turn {
            for part in mt.parts {
                if let Some(inline) = part.inline_data {
                    if inline.mime_type.starts_with("audio/") && !inline.data.is_empty() {
                        match decode_inline_pcm(&inline.data) {
                            Ok(pcm) => {
                                let rate = parse_rate(&inline.mime_type, output_rate);
                                if ev_tx
                                    .send(Ok(RealtimeEvent::Audio { pcm, sample_rate: rate }))
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Err(e) => tracing::debug!("gemini live: {e:#}"),
                        }
                    }
                }
                if let Some(text) = part.text {
                    if !text.is_empty() {
                        let _ = ev_tx.send(Ok(RealtimeEvent::AssistantTextDelta(text)));
                    }
                }
            }
        }

        if content.turn_complete {
            if !user_emitted && !input_buf.is_empty() {
                let _ =
                    ev_tx.send(Ok(RealtimeEvent::UserTextFinal(std::mem::take(&mut input_buf))));
            }
            let _ = ev_tx.send(Ok(RealtimeEvent::Done));
            break; // one-shot: one press = one user turn + one reply
        }
    }
}

/// Writer task body: pump mic PCM into `realtimeInput` frames, then send
/// `audioStreamEnd` when the caller closes `audio_in`. Generic over the write
/// half. Dropping the sink afterwards does not close the socket — the split
/// shares one underlying stream and the reader keeps the connection alive
/// until `turnComplete`.
async fn run_writer<W>(mut write: W, mut audio_rx: mpsc::Receiver<Vec<f32>>, input_rate: u32)
where
    W: futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    while let Some(chunk) = audio_rx.recv().await {
        let msg = encode_audio_chunk(&chunk, input_rate);
        if let Err(e) = write.send(Message::Text(msg.to_string())).await {
            tracing::warn!("gemini live: audio send error: {e:#}");
            return;
        }
    }
    let end = audio_stream_end_json();
    if let Err(e) = write.send(Message::Text(end.to_string())).await {
        tracing::debug!("gemini live: audioStreamEnd send failed: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_json_has_audio_modality_and_voice() {
        let v = build_setup_json("gemini-3.1-flash-live-preview", "Be terse.", "Kore");
        let setup = &v["setup"];
        assert_eq!(setup["model"], "models/gemini-3.1-flash-live-preview");
        assert_eq!(setup["generationConfig"]["responseModalities"][0], "AUDIO");
        assert_eq!(
            setup["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]
                ["voiceName"],
            "Kore"
        );
        assert_eq!(setup["systemInstruction"]["parts"][0]["text"], "Be terse.");
        // Transcription on both lanes so we can recover user + assistant text.
        assert!(setup.get("inputAudioTranscription").is_some());
        assert!(setup.get("outputAudioTranscription").is_some());
    }

    #[test]
    fn setup_json_normalises_bare_and_prefixed_model() {
        let bare = build_setup_json("gemini-x", "", "Puck");
        assert_eq!(bare["setup"]["model"], "models/gemini-x");
        let prefixed = build_setup_json("models/gemini-x", "", "Puck");
        assert_eq!(prefixed["setup"]["model"], "models/gemini-x");
    }

    #[test]
    fn setup_json_omits_empty_system_instruction() {
        let v = build_setup_json("gemini-x", "", "Kore");
        assert!(v["setup"].get("systemInstruction").is_none());
    }

    #[test]
    fn audio_chunk_carries_rate_and_base64() {
        let v = encode_audio_chunk(&[0.0, 1.0, -1.0], 16_000);
        let chunk = &v["realtimeInput"]["audio"];
        assert_eq!(chunk["mimeType"], "audio/pcm;rate=16000");
        let data = chunk["data"].as_str().expect("data string");
        // 3 samples × 2 bytes = 6 bytes → base64 of 6 bytes is 8 chars.
        let decoded = BASE64.decode(data).expect("valid base64");
        assert_eq!(decoded.len(), 6);
    }

    #[test]
    fn audio_stream_end_shape() {
        let v = audio_stream_end_json();
        assert_eq!(v["realtimeInput"]["audioStreamEnd"], true);
    }

    #[test]
    fn pcm_round_trips_through_s16le() {
        let pcm = [0.0_f32, 0.5, -0.5];
        let bytes = f32_to_s16le_bytes(&pcm);
        let back = s16le_bytes_to_f32(&bytes);
        assert_eq!(back.len(), 3);
        assert!((back[0] - 0.0).abs() < 1e-3);
        assert!((back[1] - 0.5).abs() < 1e-3);
        assert!((back[2] + 0.5).abs() < 1e-3);
    }

    #[test]
    fn f32_to_s16le_clamps_out_of_range() {
        let bytes = f32_to_s16le_bytes(&[2.5, -3.0]);
        assert_eq!(&bytes[0..2], &[0xFF, 0x7F]); // +1.0 → 32767
        assert_eq!(&bytes[2..4], &[0x01, 0x80]); // -1.0 → -32767
    }

    #[test]
    fn decode_inline_pcm_matches_known_bytes() {
        // i16 LE [0, 32767] = bytes [00 00 FF 7F].
        let raw = [0x00u8, 0x00, 0xFF, 0x7F];
        let b64 = BASE64.encode(raw);
        let pcm = decode_inline_pcm(&b64).expect("decode");
        assert_eq!(pcm.len(), 2);
        assert!((pcm[0]).abs() < 1e-6);
        assert!((pcm[1] - 0.999_97).abs() < 1e-3);
    }

    #[test]
    fn parse_rate_reads_mime_and_falls_back() {
        assert_eq!(parse_rate("audio/pcm;rate=24000", 16_000), 24_000);
        assert_eq!(parse_rate("audio/pcm", 24_000), 24_000);
        assert_eq!(parse_rate("audio/pcm;rate=bogus", 48_000), 48_000);
    }

    #[test]
    fn parse_setup_complete_frame() {
        let msg = parse_server_frame(br#"{"setupComplete":{}}"#).expect("parse");
        assert!(msg.setup_complete.is_some());
        assert!(msg.server_content.is_none());
    }

    #[test]
    fn parse_server_content_audio_and_transcription() {
        let raw = BASE64.encode([0x00u8, 0x00, 0x01, 0x00]);
        let body = format!(
            r#"{{"serverContent":{{
                "outputTranscription":{{"text":"hello"}},
                "modelTurn":{{"parts":[
                    {{"inlineData":{{"mimeType":"audio/pcm;rate=24000","data":"{raw}"}}}}
                ]}}
            }}}}"#
        );
        let msg = parse_server_frame(body.as_bytes()).expect("parse");
        let content = msg.server_content.expect("content");
        assert_eq!(content.output_transcription.unwrap().text, "hello");
        let mt = content.model_turn.expect("modelTurn");
        let inline = mt.parts[0].inline_data.as_ref().expect("inline");
        assert_eq!(parse_rate(&inline.mime_type, 16_000), 24_000);
        assert_eq!(decode_inline_pcm(&inline.data).unwrap().len(), 2);
    }

    #[test]
    fn parse_turn_complete_frame() {
        let msg = parse_server_frame(br#"{"serverContent":{"turnComplete":true}}"#).expect("parse");
        assert!(msg.server_content.unwrap().turn_complete);
    }

    #[test]
    fn parse_interrupted_frame() {
        // Gemini Live signals barge-in with serverContent.interrupted.
        let msg = parse_server_frame(br#"{"serverContent":{"interrupted":true}}"#).expect("parse");
        let content = msg.server_content.expect("content");
        assert!(content.interrupted);
        assert!(!content.turn_complete);
    }

    #[test]
    fn interrupted_defaults_false_when_absent() {
        let msg = parse_server_frame(br#"{"serverContent":{"turnComplete":true}}"#).expect("parse");
        assert!(!msg.server_content.unwrap().interrupted);
    }

    #[test]
    fn parse_ignores_unknown_message_kinds() {
        // usageMetadata / goAway etc. must not break the parser.
        let msg =
            parse_server_frame(br#"{"usageMetadata":{"totalTokenCount":42}}"#).expect("parse");
        assert!(msg.setup_complete.is_none());
        assert!(msg.server_content.is_none());
    }

    #[test]
    fn builder_exposes_name_and_rate() {
        let c = GeminiLive::new("k", "m", "wss://x", "Kore", 16_000, 24_000);
        assert_eq!(c.name(), "gemini-live");
        assert_eq!(c.native_input_rate(), 16_000);
    }
}
