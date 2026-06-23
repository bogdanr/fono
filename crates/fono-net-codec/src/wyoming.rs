// SPDX-License-Identifier: GPL-3.0-only
//! Wyoming protocol event types.
//!
//! Mirrors the subset of <https://github.com/OHF-Voice/wyoming> we
//! actually use: STT (audio + describe/info + transcribe + transcript,
//! with optional streaming variants), TTS (synthesize + audio-* on
//! the response side), and **wake** (detect + detection on the wake
//! side — Phase H of the wake-word plan, letting Fono expose its local
//! detector as a Wyoming wake service). VAD / intent / handle /
//! satellite / mic / snd events live one upstream version away and will
//! land in follow-up slices.

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
/// Client → server: ask the TTS service to synthesise audio for `text`.
/// Server replies with the standard `audio-start` / `audio-chunk`+ /
/// `audio-stop` sequence (already typed above for the STT path).
pub const SYNTHESIZE: &str = "synthesize";
/// Client → server: start a wake-word detection session, optionally
/// narrowing which wake names to listen for. The client then streams
/// `audio-chunk` events; the server replies with a `detection` event
/// each time a wake word fires.
pub const DETECT: &str = "detect";
/// Server → client: a wake word fired. Carries the detected name and an
/// optional timestamp (ms into the audio stream).
pub const DETECTION: &str = "detection";

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

/// One speaker row inside a `TtsVoice`. Wyoming voices may expose
/// multiple speakers (e.g. en_US-libritts has ~900). `name` is the
/// only required field; description is free-form.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TtsSpeaker {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// One voice row in a TTS program's `voices` list. The shape mirrors
/// `AsrModel` plus a `speakers` field. `language` (singular) is the
/// upstream field name for the primary language code, while `languages`
/// (plural) appears on some forks; we accept either for robustness.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TtsVoice {
    pub name: String,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub speakers: Vec<TtsSpeaker>,
    pub installed: bool,
    pub attribution: Attribution,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// One TTS service entry in `info.tts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TtsProgram {
    pub name: String,
    pub attribution: Attribution,
    pub installed: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub voices: Vec<TtsVoice>,
    /// Some servers (wyoming-piper >= 1.5) signal streaming TTS
    /// (sentence-by-sentence chunked output) here.
    #[serde(default)]
    pub supports_synthesize_streaming: bool,
}

/// One wake-word model row in an `info.wake[].models` list. Mirrors
/// [`AsrModel`] with an extra optional `phrase` — the spoken phrase the
/// model fires on (e.g. `"hey fono"`), which upstream Wyoming wake
/// services (openWakeWord, microWakeWord) surface so consumers can show
/// the phrase in a picker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WakeModel {
    pub name: String,
    #[serde(default)]
    pub languages: Vec<String>,
    pub installed: bool,
    pub attribution: Attribution,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phrase: Option<String>,
}

/// One wake service entry in `info.wake`. Mirrors [`AsrProgram`] /
/// [`TtsProgram`]; lists the wake models the service can detect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WakeProgram {
    pub name: String,
    pub attribution: Attribution,
    pub installed: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub models: Vec<WakeModel>,
}

/// `detect` — client → server: begin a wake detection session, optionally
/// restricting to a subset of wake `names`. Empty `names` means "listen
/// for every model the service advertises".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Detect {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

/// `detection` — server → client: a wake word fired. `name` is the model
/// id that fired; `timestamp` is the optional millisecond offset into the
/// audio stream; `speaker` is reserved for future per-speaker wake models.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Detection {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

/// Voice selector inside a `synthesize` request. All fields optional;
/// `None` lets the server pick its default voice.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Voice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

impl Voice {
    /// True when every field is `None`. Helpful for callers that want
    /// to omit the voice block entirely.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.language.is_none() && self.speaker.is_none()
    }
}

/// `synthesize` — request to convert `text` to audio. Server responds
/// with `audio-start` / `audio-chunk`+ / `audio-stop` framed PCM.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Synthesize {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<Voice>,
}

/// `info` event — the full describe response. Home Assistant's Wyoming
/// loader expects every service family to be present as an array, even
/// when only ASR is installed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Info {
    #[serde(default)]
    pub asr: Vec<AsrProgram>,
    #[serde(default)]
    pub tts: Vec<TtsProgram>,
    #[serde(default)]
    pub handle: Vec<Value>,
    #[serde(default)]
    pub intent: Vec<Value>,
    #[serde(default)]
    pub wake: Vec<WakeProgram>,
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
        let event = AudioStart { rate: 16000, width: 2, channels: 1, timestamp: None };
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

    #[tokio::test]
    async fn synthesize_round_trip() {
        let req = Synthesize {
            text: "Hello, world!".into(),
            voice: Some(Voice {
                name: Some("en_US-amy-low".into()),
                language: Some("en".into()),
                speaker: None,
            }),
        };
        let f = Frame::new(SYNTHESIZE).with_data(to_value(&req).unwrap());
        let mut buf: Vec<u8> = Vec::new();
        f.write_async(&mut buf).await.unwrap();
        let mut reader = BufReader::new(buf.as_slice());
        let parsed = Frame::read_async(&mut reader).await.unwrap();
        assert_eq!(parsed.kind, SYNTHESIZE);
        let back: Synthesize = serde_json::from_value(parsed.data).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn synthesize_omits_voice_when_absent() {
        let req = Synthesize { text: "hi".into(), voice: None };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v, json!({"text": "hi"}));
    }

    #[test]
    fn voice_omits_unset_fields() {
        let v = Voice { name: Some("en_US-amy-low".into()), language: None, speaker: None };
        assert_eq!(serde_json::to_value(&v).unwrap(), json!({"name": "en_US-amy-low"}));
    }

    #[test]
    fn voice_is_empty_helper() {
        assert!(Voice::default().is_empty());
        assert!(!Voice { name: Some("x".into()), ..Voice::default() }.is_empty());
    }

    #[test]
    fn info_round_trip_with_tts_program() {
        let info = Info {
            tts: vec![TtsProgram {
                name: "wyoming-piper".into(),
                attribution: Attribution {
                    name: "Piper".into(),
                    url: "https://github.com/rhasspy/piper".into(),
                },
                installed: true,
                description: Some("Piper text-to-speech".into()),
                version: Some("1.5.0".into()),
                voices: vec![TtsVoice {
                    name: "en_US-amy-low".into(),
                    languages: vec!["en_US".into()],
                    speakers: vec![],
                    installed: true,
                    attribution: Attribution {
                        name: "Piper".into(),
                        url: "https://github.com/rhasspy/piper".into(),
                    },
                    description: None,
                    version: Some("1.0".into()),
                }],
                supports_synthesize_streaming: true,
            }],
            ..Info::default()
        };
        let v = serde_json::to_value(&info).unwrap();
        let back: Info = serde_json::from_value(v).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn info_default_has_empty_tts_vec() {
        // Default Info must have an empty tts array, not omitted —
        // Home Assistant's Wyoming loader requires the field to be
        // present even when no TTS service is installed.
        let v = serde_json::to_value(Info::default()).unwrap();
        assert_eq!(v["tts"], json!([]));
    }

    #[tokio::test]
    async fn detection_round_trip() {
        let event = Detection { name: "hey_fono".into(), timestamp: Some(1234), speaker: None };
        let f = Frame::new(DETECTION).with_data(to_value(&event).unwrap());
        let mut buf: Vec<u8> = Vec::new();
        f.write_async(&mut buf).await.unwrap();
        let mut reader = BufReader::new(buf.as_slice());
        let parsed = Frame::read_async(&mut reader).await.unwrap();
        assert_eq!(parsed.kind, DETECTION);
        let back: Detection = serde_json::from_value(parsed.data).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn detection_omits_optional_fields_when_absent() {
        let event = Detection { name: "hey_fono".into(), timestamp: None, speaker: None };
        let v = serde_json::to_value(&event).unwrap();
        assert_eq!(v, json!({"name": "hey_fono"}));
    }

    #[test]
    fn detect_omits_names_when_empty() {
        let req = Detect::default();
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v, json!({}));
    }

    #[test]
    fn detect_round_trips_names() {
        let req = Detect { names: vec!["hey_fono".into(), "alexa".into()] };
        let back: Detect = serde_json::from_value(serde_json::to_value(&req).unwrap()).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn info_round_trip_with_wake_program() {
        let info = Info {
            wake: vec![WakeProgram {
                name: "Fono".into(),
                attribution: Attribution {
                    name: "Fono".into(),
                    url: "https://github.com/bogdanr/fono".into(),
                },
                installed: true,
                description: Some("Fono wake-word detector".into()),
                version: Some("0.0.0".into()),
                models: vec![WakeModel {
                    name: "hey_fono".into(),
                    languages: vec!["en".into()],
                    installed: true,
                    attribution: Attribution {
                        name: "Fono".into(),
                        url: "https://github.com/bogdanr/fono".into(),
                    },
                    description: None,
                    version: None,
                    phrase: Some("hey fono".into()),
                }],
            }],
            ..Info::default()
        };
        let v = serde_json::to_value(&info).unwrap();
        let back: Info = serde_json::from_value(v).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn info_default_has_empty_wake_vec() {
        // HA's loader wants every service family present as an array.
        let v = serde_json::to_value(Info::default()).unwrap();
        assert_eq!(v["wake"], json!([]));
    }
}
