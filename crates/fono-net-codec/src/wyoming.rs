// SPDX-License-Identifier: GPL-3.0-only
//! Wyoming protocol event types.
//!
//! Mirrors the subset of <https://github.com/OHF-Voice/wyoming> we
//! actually use for STT (audio + describe/info + transcribe +
//! transcript, with optional streaming variants). TTS / wake / VAD /
//! intent / handle / satellite / mic / snd events live one upstream
//! version away and will land in follow-up slices.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// Event-type tags. Defined as `&str` constants so the connection-arm
// allow-list can pattern-match without re-parsing the JSON.
pub const AUDIO_START: &str = "audio-start";
pub const AUDIO_CHUNK: &str = "audio-chunk";
pub const AUDIO_STOP: &str = "audio-stop";
pub const DESCRIBE: &str = "describe";
pub const INFO: &str = "info";
pub const TRANSCRIBE: &str = "transcribe";
pub const TRANSCRIPT: &str = "transcript";
pub const TRANSCRIPT_START: &str = "transcript-start";
pub const TRANSCRIPT_CHUNK: &str = "transcript-chunk";
pub const TRANSCRIPT_STOP: &str = "transcript-stop";

/// `audio-start` — open an audio stream with the agreed PCM format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioStart {
    pub rate: u32,
    pub width: u32,
    pub channels: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
}

/// `audio-chunk` — header for one PCM chunk. The chunk bytes travel
/// in `Frame::payload`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioChunk {
    pub rate: u32,
    pub width: u32,
    pub channels: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
}

/// `audio-stop` — close the audio stream.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioStop {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
}

/// `transcribe` — request to convert the just-streamed audio to text.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transcribe {
    /// Model name (optional — server picks default if absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Spoken language hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// `transcript` — final-text response (also sent in the streaming
/// flow for backward compatibility, after the chunked stream ends).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// `transcript-start` — first event of a streaming transcript.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptStart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// `transcript-chunk` — a partial of the streaming transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptChunk {
    pub text: String,
}

/// `info` attribution sub-object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attribution {
    pub name: String,
    pub url: String,
}

/// One ASR model row in an `info.asr[].models` list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AsrModel {
    pub name: String,
    pub languages: Vec<String>,
    pub installed: bool,
    pub attribution: Attribution,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// One ASR service entry in `info.asr`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AsrProgram {
    pub name: String,
    pub attribution: Attribution,
    pub installed: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub models: Vec<AsrModel>,
    #[serde(default)]
    pub supports_transcript_streaming: bool,
}

/// `info` event — the full describe response. Home Assistant's Wyoming
/// loader expects every service family to be present as an array, even
/// when only ASR is installed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Info {
    #[serde(default)]
    pub asr: Vec<AsrProgram>,
    #[serde(default)]
    pub tts: Vec<Value>,
    #[serde(default)]
    pub handle: Vec<Value>,
    #[serde(default)]
    pub intent: Vec<Value>,
    #[serde(default)]
    pub wake: Vec<Value>,
    #[serde(default)]
    pub mic: Vec<Value>,
    #[serde(default)]
    pub snd: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub satellite: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;
    use serde_json::{json, to_value};
    use tokio::io::BufReader;

    #[tokio::test]
    async fn audio_start_round_trip() {
        let event = AudioStart {
            rate: 16000,
            width: 2,
            channels: 1,
            timestamp: None,
        };
        let f = Frame::new(AUDIO_START).with_data(to_value(&event).unwrap());
        let mut buf: Vec<u8> = Vec::new();
        f.write_async(&mut buf).await.unwrap();
        let mut reader = BufReader::new(buf.as_slice());
        let parsed = Frame::read_async(&mut reader).await.unwrap();
        assert_eq!(parsed.kind, AUDIO_START);
        let back: AudioStart = serde_json::from_value(parsed.data).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn info_round_trip_with_streaming_flag() {
        let info = Info {
            asr: vec![AsrProgram {
                name: "fono-asr".into(),
                attribution: Attribution {
                    name: "Fono".into(),
                    url: "https://github.com/bogdanr/fono".into(),
                },
                installed: true,
                description: Some("Fono speech-to-text".into()),
                version: Some("0.0.0".into()),
                models: vec![AsrModel {
                    name: "whisper-large-v3".into(),
                    languages: vec!["en".into(), "ro".into()],
                    installed: true,
                    attribution: Attribution {
                        name: "OpenAI".into(),
                        url: "https://openai.com".into(),
                    },
                    description: None,
                    version: Some("v3".into()),
                }],
                supports_transcript_streaming: true,
            }],
            ..Info::default()
        };
        let v = serde_json::to_value(&info).unwrap();
        let back: Info = serde_json::from_value(v).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn transcribe_omits_optional_fields_when_absent() {
        let req = Transcribe::default();
        let v = serde_json::to_value(&req).unwrap();
        // No `name`, no `language` should appear on the wire.
        assert_eq!(v, json!({}));
    }
}
