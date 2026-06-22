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
//! When the user has opted into vision (`prefer_vision`) and a screen-capture
//! backend is available, a single screenshot of the focused window is sent as
//! a `realtimeInput.video` frame right after setup (before audio) so the model
//! can answer questions about what is on screen for this turn.
//!
//! General tool-calling (for `fono-action`) is still absent (Path B: audio
//! loop first). The one exception is full-duplex live mode, which declares a
//! single `end_conversation` function and instructs the model to call it when
//! the user signals they are done, so the session can close on intent and not
//! only on silence (`toolCall` → [`RealtimeEvent::EndConversation`]).
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

use fono_core::screen_capture::CaptureMode;

use crate::history::{ChatRole, ChatTurn};
use crate::traits::{
    AssistantContext, RealtimeAssistant, RealtimeEvent, RealtimeMode, RealtimeSession,
    ScreenCaptureFn,
};

/// Mic-input channel depth (PCM frames). Bounded so a stalled socket applies
/// backpressure to the capture path rather than growing unboundedly.
const AUDIO_IN_CAPACITY: usize = 64;

/// How long to wait for `setupComplete` after sending the setup message.
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Name of the function the model is told to call when the conversation is
/// over (full-duplex live mode only). A `toolCall` for this name maps to
/// [`RealtimeEvent::EndConversation`].
pub(crate) const END_CONVERSATION_FN: &str = "end_conversation";

/// Appended to the system instruction in full-duplex live mode so the model
/// closes the session on intent (user says goodbye / "that's all") rather than
/// waiting for the silence timeout. Kept terse and language-neutral — the
/// model speaks the user's language, so the cue is described, not enumerated.
const END_CONVERSATION_GUIDANCE: &str = "This is a live, hands-free voice conversation. \
When the user signals they are finished (e.g. says goodbye, thanks you and \
indicates they need nothing more, or asks to stop), call the `end_conversation` \
function to close the session. Do not call it while the user still has an open \
request.";

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
///
/// When `seed_history` is true, `historyConfig.initialHistoryInClientContent`
/// is set so the server accepts a follow-up `clientContent` message carrying
/// the prior conversation (see [`build_client_content_json`]). Without this
/// flag the Live API rejects `clientContent` seeding, and the session starts
/// with no memory of earlier turns.
#[must_use]
pub fn build_setup_json(
    model: &str,
    system_prompt: &str,
    voice: &str,
    seed_history: bool,
    full_duplex: bool,
) -> serde_json::Value {
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
    if seed_history {
        setup["historyConfig"] = json!({ "initialHistoryInClientContent": true });
    }
    if full_duplex {
        // Full-duplex live mode: enable server-side automatic activity
        // detection so the model endpoints turns itself, and let a fresh
        // user activity start interrupt the model's reply (acoustic
        // barge-in). Push-to-talk leaves this off and commits with
        // `audioStreamEnd` instead.
        //
        // Sensitivity is deliberately damped. With a continuously open
        // mic and `START_OF_ACTIVITY_INTERRUPTS`, default (high)
        // sensitivity makes the model barge in on its own — breath,
        // room noise, or the tail of the user's speech trips the VAD and
        // discards the reply mid-sentence (observed even on headphones,
        // where there is no echo). `START_SENSITIVITY_LOW` +
        // `END_SENSITIVITY_LOW` require clearer onset/offset, and a
        // longer `silenceDurationMs` stops the model endpointing the
        // user prematurely. These are the knobs to revisit once AEC
        // lands; they trade a touch of barge-in latency for not
        // self-interrupting.
        setup["realtimeInputConfig"] = json!({
            "automaticActivityDetection": {
                "startOfSpeechSensitivity": "START_SENSITIVITY_LOW",
                "endOfSpeechSensitivity": "END_SENSITIVITY_LOW",
                "prefixPaddingMs": 300,
                "silenceDurationMs": 800,
            },
            "activityHandling": "START_OF_ACTIVITY_INTERRUPTS",
        });
        // Let the model close the session on intent ("goodbye", "that's
        // all") via a single function call, in addition to the local
        // silence timeout. Declare the tool and fold the how/when into
        // the system instruction (creating one if the caller gave none).
        setup["tools"] = json!([{
            "functionDeclarations": [{
                "name": END_CONVERSATION_FN,
                "description": "End the live voice conversation when the user \
                    indicates they are finished and need nothing more.",
            }]
        }]);
        let guided = if system_prompt.is_empty() {
            END_CONVERSATION_GUIDANCE.to_string()
        } else {
            format!("{system_prompt}\n\n{END_CONVERSATION_GUIDANCE}")
        };
        setup["systemInstruction"] = json!({ "parts": [{ "text": guided }] });
    }
    json!({ "setup": setup })
}

/// Map one rolling-history [`ChatTurn`] onto a Live `Content` value, or `None`
/// when the turn has no Live equivalent. `System` turns live in
/// `systemInstruction` and `Tool` turns have no place under Path B (no
/// function calling), so both are skipped, as are empty-text turns.
fn turn_to_live(turn: &ChatTurn) -> Option<serde_json::Value> {
    let role = match turn.role {
        ChatRole::User => "user",
        ChatRole::Assistant => "model",
        ChatRole::System | ChatRole::Tool => return None,
    };
    if turn.content.is_empty() {
        return None;
    }
    Some(json!({ "role": role, "parts": [{ "text": turn.content }] }))
}

/// Build the `clientContent` message that seeds prior conversation into a
/// freshly-opened Live session. Sent once, after `setupComplete` and before
/// any `realtimeInput` audio, with `turnComplete: true` so the server records
/// it as context **without** triggering a model reply. Exposed for tests.
#[must_use]
pub fn build_client_content_json(turns: &[ChatTurn]) -> serde_json::Value {
    let live_turns: Vec<serde_json::Value> = turns.iter().filter_map(turn_to_live).collect();
    json!({
        "clientContent": {
            "turns": live_turns,
            "turnComplete": true,
        }
    })
}

/// Build a `realtimeInput.video` frame carrying a single still image (PNG).
/// Gemini Live ingests video as individual image blobs; one screenshot per
/// turn gives the model visual context for the spoken question. Exposed for
/// tests.
#[must_use]
pub fn encode_video_frame(png_bytes: &[u8]) -> serde_json::Value {
    let data = BASE64.encode(png_bytes);
    json!({
        "realtimeInput": {
            "video": { "mimeType": "image/png", "data": data }
        }
    })
}

/// Run the (blocking) screen-capture closure off the async runtime and return
/// the captured PNG bytes, or `None` when capture is unavailable, blocked by
/// the privacy gate, cancelled, or fails. A capture failure is never fatal to
/// the session — the turn simply proceeds without vision.
async fn capture_screen_png(capture: ScreenCaptureFn) -> Option<Vec<u8>> {
    match tokio::task::spawn_blocking(move || capture(CaptureMode::Automatic)).await {
        Ok(Ok(img)) => Some(img.png_bytes),
        Ok(Err(e)) => {
            tracing::debug!("gemini live: screen capture skipped: {e}");
            None
        }
        Err(e) => {
            tracing::debug!("gemini live: screen capture task panicked: {e}");
            None
        }
    }
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
    #[serde(default, rename = "toolCall")]
    tool_call: Option<ToolCall>,
}

/// A `toolCall` server message: the model is invoking one or more declared
/// functions. Live mode only declares `end_conversation`, so the reader only
/// inspects the function names.
#[derive(Deserialize, Debug, Default)]
struct ToolCall {
    #[serde(default, rename = "functionCalls")]
    function_calls: Vec<FunctionCall>,
}

#[derive(Deserialize, Debug, Default)]
struct FunctionCall {
    #[serde(default)]
    name: String,
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
    async fn open_session(
        &self,
        ctx: &AssistantContext,
        mode: RealtimeMode,
    ) -> Result<RealtimeSession> {
        // Auth: Gemini Live takes the API key as a `?key=` query param on the
        // upgrade URL (there is no header form for the WS handshake).
        let url = format!("{base}?key={key}", base = self.ws_url, key = self.api_key);
        let request =
            url.as_str().into_client_request().context("building Gemini Live WS request")?;
        let (ws, _resp) = tokio_tungstenite::connect_async(request)
            .await
            .context("Gemini Live WS connect failed")?;
        let (mut write, mut read) = ws.split();

        // 1. Send setup. If there is prior conversation to seed, flag it here
        // so the server will accept the follow-up `clientContent` message.
        let client_content = build_client_content_json(&ctx.history);
        let seed_history =
            client_content["clientContent"]["turns"].as_array().is_some_and(|t| !t.is_empty());
        let full_duplex = matches!(mode, RealtimeMode::FullDuplex);
        let setup = build_setup_json(
            &self.model,
            &ctx.system_prompt,
            &self.voice,
            seed_history,
            full_duplex,
        );
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

        // 3. Seed prior conversation (if any) as initial history. Sent with
        // `turnComplete: true`; the server records it as context and does not
        // reply, so the reader stays one-shot on the real (audio) turn.
        if seed_history {
            write
                .send(Message::Text(client_content.to_string()))
                .await
                .context("Gemini Live: seeding conversation history")?;
        }

        // 4. Optional screen vision: when the user opted into vision and a
        // capture backend is available, grab the focused window and send it as
        // a single `realtimeInput.video` frame before any audio, so the visual
        // context is in place when the model processes the spoken question.
        // Capture failures are non-fatal — the turn proceeds without vision.
        if ctx.prefer_vision {
            if let Some(capture) = ctx.screen_capture.clone() {
                if let Some(png) = capture_screen_png(capture).await {
                    let frame = encode_video_frame(&png);
                    if let Err(e) = write.send(Message::Text(frame.to_string())).await {
                        tracing::warn!("gemini live: video frame send error: {e:#}");
                    }
                }
            }
        }

        let (audio_tx, audio_rx) = mpsc::channel::<Vec<f32>>(AUDIO_IN_CAPACITY);
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Result<RealtimeEvent>>();

        tokio::spawn(run_reader(read, ev_tx, self.output_rate, full_duplex));
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
    full_duplex: bool,
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

        // Tool call: in live mode the model invokes `end_conversation` to
        // close the session on intent. Emit the neutral event and keep
        // reading so any in-flight reply audio still drains; the consumer
        // closes the session. Sibling of `serverContent`, so check before
        // the content guard below.
        if let Some(tc) = &msg.tool_call {
            if tc.function_calls.iter().any(|f| f.name == END_CONVERSATION_FN) {
                let _ = ev_tx.send(Ok(RealtimeEvent::EndConversation));
            }
        }

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
            if full_duplex {
                // Live mode: one session spans many turns. `Done` is a
                // turn boundary, not the end of the session — reset the
                // per-turn state and keep reading. The model's own VAD
                // opens the next user turn over the still-open mic.
                user_emitted = false;
                input_buf.clear();
                continue;
            }
            break; // PTT one-shot: one press = one user turn + one reply
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
        let v =
            build_setup_json("gemini-3.1-flash-live-preview", "Be terse.", "Kore", false, false);
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
        // No history to seed → no historyConfig.
        assert!(setup.get("historyConfig").is_none());
    }

    #[test]
    fn setup_json_normalises_bare_and_prefixed_model() {
        let bare = build_setup_json("gemini-x", "", "Puck", false, false);
        assert_eq!(bare["setup"]["model"], "models/gemini-x");
        let prefixed = build_setup_json("models/gemini-x", "", "Puck", false, false);
        assert_eq!(prefixed["setup"]["model"], "models/gemini-x");
    }

    #[test]
    fn setup_json_omits_empty_system_instruction() {
        let v = build_setup_json("gemini-x", "", "Kore", false, false);
        assert!(v["setup"].get("systemInstruction").is_none());
    }

    // PTT contract: the open-time setup must NOT enable server-side
    // automatic activity detection. The push-to-talk path sends the whole
    // buffered utterance and commits the turn with `audioStreamEnd`, so the
    // model must not endpoint/reply mid-utterance on its own VAD. Server VAD
    // belongs to the future full-duplex live mode, not PTT.
    #[test]
    fn setup_json_does_not_enable_server_vad() {
        let v = build_setup_json("gemini-x", "Be terse.", "Kore", false, false);
        let setup = &v["setup"];
        assert!(
            setup.get("realtimeInputConfig").is_none(),
            "PTT must not configure realtimeInputConfig (server VAD)"
        );
        // Belt-and-braces: the literal toggle must be absent anywhere in the
        // setup payload so PTT relies solely on audioStreamEnd to commit.
        assert!(
            !v.to_string().contains("automaticActivityDetection"),
            "PTT setup must not enable automaticActivityDetection"
        );
    }

    #[test]
    fn setup_json_sets_history_config_when_seeding() {
        let v = build_setup_json("gemini-x", "", "Kore", true, false);
        assert_eq!(v["setup"]["historyConfig"]["initialHistoryInClientContent"], true);
    }

    // Full-duplex live mode: setup MUST enable server-side automatic activity
    // detection so the model endpoints its own turns, and configure
    // start-of-activity interruption so the user can barge in by speaking.
    // Sensitivity is damped (LOW) so the model does not interrupt itself on
    // breath / ambient noise over the continuously open mic.
    #[test]
    fn setup_json_enables_server_vad_in_full_duplex() {
        let v = build_setup_json("gemini-x", "Be terse.", "Kore", false, true);
        let rt = &v["setup"]["realtimeInputConfig"];
        let aad = &rt["automaticActivityDetection"];
        assert!(aad.is_object(), "full-duplex must enable server VAD");
        assert_eq!(aad["startOfSpeechSensitivity"], "START_SENSITIVITY_LOW");
        assert_eq!(aad["endOfSpeechSensitivity"], "END_SENSITIVITY_LOW");
        assert_eq!(rt["activityHandling"], "START_OF_ACTIVITY_INTERRUPTS");
    }

    // Full-duplex declares the `end_conversation` tool and folds its usage
    // into the system instruction so the model can close the session on
    // intent; PTT declares neither.
    #[test]
    fn full_duplex_declares_end_conversation_tool_ptt_does_not() {
        let live = build_setup_json("gemini-x", "Be terse.", "Kore", false, true);
        let fns = &live["setup"]["tools"][0]["functionDeclarations"];
        assert_eq!(fns[0]["name"], END_CONVERSATION_FN, "live mode must declare the tool");
        let sys = live["setup"]["systemInstruction"]["parts"][0]["text"].as_str().unwrap();
        assert!(sys.starts_with("Be terse."), "caller prompt must be preserved");
        assert!(sys.contains(END_CONVERSATION_FN), "guidance must mention the tool");

        let ptt = build_setup_json("gemini-x", "Be terse.", "Kore", false, false);
        assert!(ptt["setup"]["tools"].is_null(), "PTT must not declare tools");
    }

    // A `toolCall` for `end_conversation` maps to a single `EndConversation`
    // event; an unrelated tool name does not.
    #[test]
    fn reader_maps_end_conversation_tool_call_to_event() {
        use tokio_tungstenite::tungstenite::{Error as WsError, Message};
        fn count_end(rx: &mut mpsc::UnboundedReceiver<Result<RealtimeEvent>>) -> usize {
            let mut n = 0;
            while let Ok(ev) = rx.try_recv() {
                if matches!(ev, Ok(RealtimeEvent::EndConversation)) {
                    n += 1;
                }
            }
            n
        }
        let frames = |name: &str| -> Vec<Result<Message, WsError>> {
            vec![
                Ok(Message::Text(format!(
                    r#"{{"toolCall":{{"functionCalls":[{{"name":"{name}"}}]}}}}"#
                ))),
                Ok(Message::Text(r#"{"serverContent":{"turnComplete":true}}"#.to_string())),
            ]
        };

        let (tx, mut rx) = mpsc::unbounded_channel();
        futures::executor::block_on(run_reader(
            futures::stream::iter(frames(END_CONVERSATION_FN)),
            tx,
            24_000,
            true,
        ));
        assert_eq!(count_end(&mut rx), 1, "end_conversation tool call must emit the event");

        let (tx, mut rx) = mpsc::unbounded_channel();
        futures::executor::block_on(run_reader(
            futures::stream::iter(frames("some_other_tool")),
            tx,
            24_000,
            true,
        ));
        assert_eq!(count_end(&mut rx), 0, "unrelated tool call must not end the session");
    }

    // The reader is one-shot for PTT (stop at the first `turnComplete`) but
    // must span the whole session for full-duplex live mode (each
    // `turnComplete` is just a turn boundary). Regression for the bug where a
    // live session closed after a single turn.
    #[test]
    fn reader_loops_in_full_duplex_but_is_one_shot_for_ptt() {
        use tokio_tungstenite::tungstenite::{Error as WsError, Message};
        fn count_dones(rx: &mut mpsc::UnboundedReceiver<Result<RealtimeEvent>>) -> usize {
            let mut n = 0;
            while let Ok(ev) = rx.try_recv() {
                if matches!(ev, Ok(RealtimeEvent::Done)) {
                    n += 1;
                }
            }
            n
        }
        let two_turns = || -> Vec<Result<Message, WsError>> {
            vec![
                Ok(Message::Text(
                    r#"{"serverContent":{"outputTranscription":{"text":"hi"}}}"#.to_string(),
                )),
                Ok(Message::Text(r#"{"serverContent":{"turnComplete":true}}"#.to_string())),
                Ok(Message::Text(
                    r#"{"serverContent":{"outputTranscription":{"text":"again"}}}"#.to_string(),
                )),
                Ok(Message::Text(r#"{"serverContent":{"turnComplete":true}}"#.to_string())),
            ]
        };

        // Full-duplex: both turns read → two `Done` boundaries.
        let (tx, mut rx) = mpsc::unbounded_channel();
        futures::executor::block_on(run_reader(
            futures::stream::iter(two_turns()),
            tx,
            24_000,
            true,
        ));
        assert_eq!(count_dones(&mut rx), 2, "full-duplex reader must span multiple turns");

        // PTT: stops at the first `turnComplete`; the second turn is ignored.
        let (tx, mut rx) = mpsc::unbounded_channel();
        futures::executor::block_on(run_reader(
            futures::stream::iter(two_turns()),
            tx,
            24_000,
            false,
        ));
        assert_eq!(count_dones(&mut rx), 1, "PTT reader is one-shot");
    }

    #[test]
    fn client_content_maps_user_and_assistant_turns() {
        let now = std::time::Instant::now();
        let mk = |role: ChatRole, content: &str| ChatTurn {
            role,
            content: content.to_string(),
            at: now,
            tool_calls: Vec::new(),
            tool_call_id: None,
        };
        let turns = [
            mk(ChatRole::System, "ignored"),
            mk(ChatRole::User, "hi there"),
            mk(ChatRole::Assistant, "hello back"),
            mk(ChatRole::Assistant, ""), // empty → dropped
        ];
        let v = build_client_content_json(&turns);
        let cc = &v["clientContent"];
        assert_eq!(cc["turnComplete"], true);
        let arr = cc["turns"].as_array().expect("turns array");
        // System + empty dropped; user + assistant kept, in order.
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["role"], "user");
        assert_eq!(arr[0]["parts"][0]["text"], "hi there");
        assert_eq!(arr[1]["role"], "model");
        assert_eq!(arr[1]["parts"][0]["text"], "hello back");
    }

    #[test]
    fn client_content_empty_for_no_seedable_turns() {
        let v = build_client_content_json(&[]);
        assert!(v["clientContent"]["turns"].as_array().unwrap().is_empty());
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
    fn video_frame_carries_png_base64() {
        let png = [0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let v = encode_video_frame(&png);
        let frame = &v["realtimeInput"]["video"];
        assert_eq!(frame["mimeType"], "image/png");
        let data = frame["data"].as_str().expect("data string");
        let decoded = BASE64.decode(data).expect("valid base64");
        assert_eq!(decoded, png);
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
