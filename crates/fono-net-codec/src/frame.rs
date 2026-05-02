// SPDX-License-Identifier: GPL-3.0-only
//! Transport-agnostic [`Frame`] codec.
//!
//! Wire format (Wyoming spec, reused unchanged by Fono-native):
//!
//! ```text
//! { "type": "...", "data": { ... }, "data_length": N, "payload_length": M }\n
//! <data_length bytes — UTF-8 JSON object, merged on top of header.data>
//! <payload_length bytes — raw bytes (typically PCM)>
//! ```
//!
//! Parsing tolerates split data blocks (peers that send `data_length > 0`
//! and a separate JSON object after the header). Writes use the same
//! canonical Wyoming shape as the upstream Python library: non-empty
//! `data` is sent as a separate JSON block and the header carries
//! `data_length` plus a protocol `version` marker.

use serde_json::{Map, Value};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Wyoming Python package version whose wire shape we mirror for
/// Home Assistant compatibility.
pub const WYOMING_VERSION: &str = "1.8.0";
/// 1 MiB max header line — defends against malicious peers feeding an
/// unbounded line until OOM. Real Wyoming headers are < 1 KiB.
pub const MAX_HEADER_LINE_BYTES: usize = 1024 * 1024;
/// 1 MiB max data block — same defence; real data blocks are tiny.
pub const MAX_DATA_BLOCK_BYTES: usize = 1024 * 1024;
/// 64 MiB max payload — generous ceiling for a single audio chunk.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("missing required `type` field on frame header")]
    MissingType,
    #[error("header line too long ({0} bytes)")]
    HeaderTooLong(usize),
    #[error("data block too long ({0} bytes)")]
    DataTooLong(usize),
    #[error("payload too long ({0} bytes)")]
    PayloadTooLong(usize),
    #[error("unexpected end of stream while reading {0}")]
    Truncated(&'static str),
}

/// One protocol message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Event type, e.g. `"audio-chunk"` or `"fono.cleanup-request"`.
    pub kind: String,
    /// Merged event data. Always a JSON object; defaults to empty.
    pub data: Value,
    /// Optional raw binary payload (typically PCM audio).
    pub payload: Vec<u8>,
}

impl Frame {
    /// New empty frame with the given event type.
    #[must_use]
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            data: Value::Object(Map::new()),
            payload: Vec::new(),
        }
    }

    /// Replace the data field. Non-object values are normalised to an
    /// empty object (the Wyoming spec only allows `data` to be an
    /// object).
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = if data.is_object() {
            data
        } else {
            Value::Object(Map::new())
        };
        self
    }

    /// Replace the binary payload.
    #[must_use]
    pub fn with_payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = payload;
        self
    }

    /// Read one frame from `reader`. The reader MUST be buffered
    /// (`tokio::io::BufReader::new(stream)`); using an unbuffered
    /// reader works but defeats the line-wise header read.
    pub async fn read_async<R>(reader: &mut R) -> Result<Self, FrameError>
    where
        R: AsyncBufRead + Unpin,
    {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(FrameError::Truncated("header line"));
        }
        if line.len() > MAX_HEADER_LINE_BYTES {
            return Err(FrameError::HeaderTooLong(line.len()));
        }
        // Strip trailing newline (tolerate \r\n from naive peers).
        while matches!(line.as_bytes().last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        let header: Value = serde_json::from_str(&line)?;
        let kind = header
            .get("type")
            .and_then(Value::as_str)
            .ok_or(FrameError::MissingType)?
            .to_string();
        let data_length = header
            .get("data_length")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let payload_length = header
            .get("payload_length")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if data_length > MAX_DATA_BLOCK_BYTES {
            return Err(FrameError::DataTooLong(data_length));
        }
        if payload_length > MAX_PAYLOAD_BYTES {
            return Err(FrameError::PayloadTooLong(payload_length));
        }

        // Start from header.data, normalised to an object.
        let mut data = header
            .get("data")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        if !data.is_object() {
            data = Value::Object(Map::new());
        }

        // Read data block (if any) and merge into data.
        if data_length > 0 {
            let mut buf = vec![0u8; data_length];
            read_exact_or_truncated(reader, &mut buf, "data block").await?;
            let extra: Value = serde_json::from_slice(&buf)?;
            if let (Value::Object(target), Value::Object(extra_map)) = (&mut data, extra) {
                for (k, v) in extra_map {
                    target.insert(k, v);
                }
            }
        }

        // Read binary payload.
        let mut payload = Vec::new();
        if payload_length > 0 {
            payload.resize(payload_length, 0);
            read_exact_or_truncated(reader, &mut payload, "payload").await?;
        }

        Ok(Self {
            kind,
            data,
            payload,
        })
    }

    /// Serialise the frame onto `writer`. Uses the canonical Wyoming
    /// framing: header line with `type`, `version`, optional lengths,
    /// then optional JSON data block and optional payload. Flushes after
    /// the final byte.
    pub async fn write_async<W>(&self, writer: &mut W) -> Result<(), FrameError>
    where
        W: AsyncWrite + Unpin,
    {
        let mut header = Map::new();
        header.insert("type".to_string(), Value::String(self.kind.clone()));
        header.insert(
            "version".to_string(),
            Value::String(WYOMING_VERSION.to_string()),
        );

        let data_bytes = if let Value::Object(map) = &self.data {
            if map.is_empty() {
                None
            } else {
                Some(serde_json::to_vec(&self.data)?)
            }
        } else {
            Some(serde_json::to_vec(&self.data)?)
        };
        if let Some(bytes) = &data_bytes {
            header.insert("data_length".to_string(), Value::Number(bytes.len().into()));
        }
        if !self.payload.is_empty() {
            header.insert(
                "payload_length".to_string(),
                Value::Number(self.payload.len().into()),
            );
        }
        let mut header_bytes = serde_json::to_vec(&Value::Object(header))?;
        header_bytes.push(b'\n');
        writer.write_all(&header_bytes).await?;
        if let Some(bytes) = &data_bytes {
            writer.write_all(bytes).await?;
        }
        if !self.payload.is_empty() {
            writer.write_all(&self.payload).await?;
        }
        writer.flush().await?;
        Ok(())
    }
}

async fn read_exact_or_truncated<R>(
    reader: &mut R,
    buf: &mut [u8],
    what: &'static str,
) -> Result<(), FrameError>
where
    R: AsyncBufRead + Unpin,
{
    match reader.read_exact(buf).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(FrameError::Truncated(what)),
        Err(e) => Err(FrameError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::BufReader;

    async fn round_trip(frame: Frame) -> Frame {
        let mut buf: Vec<u8> = Vec::new();
        frame.write_async(&mut buf).await.unwrap();
        let mut reader = BufReader::new(buf.as_slice());
        Frame::read_async(&mut reader).await.unwrap()
    }

    #[tokio::test]
    async fn round_trip_empty_frame() {
        let f = Frame::new("audio-stop");
        assert_eq!(round_trip(f.clone()).await, f);
    }

    #[tokio::test]
    async fn round_trip_with_data() {
        let f = Frame::new("audio-start").with_data(json!({
            "rate": 16000,
            "width": 2,
            "channels": 1
        }));
        assert_eq!(round_trip(f.clone()).await, f);
    }

    #[tokio::test]
    async fn round_trip_with_payload() {
        let pcm = vec![0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let f = Frame::new("audio-chunk")
            .with_data(json!({"rate": 16000, "width": 2, "channels": 1}))
            .with_payload(pcm.clone());
        let parsed = round_trip(f.clone()).await;
        assert_eq!(parsed, f);
        assert_eq!(parsed.payload, pcm);
    }

    #[tokio::test]
    async fn payload_with_embedded_newlines_is_safe() {
        // Payload bytes that look like JSON / newlines must not
        // confuse the framer.
        let payload: Vec<u8> = b"\n{\"type\":\"transcript\"}\n\r\n".to_vec();
        let f = Frame::new("audio-chunk")
            .with_data(json!({"rate": 16000, "width": 2, "channels": 1}))
            .with_payload(payload.clone());
        let parsed = round_trip(f.clone()).await;
        assert_eq!(parsed.payload, payload);
        assert_eq!(parsed.kind, "audio-chunk");
    }

    #[tokio::test]
    async fn read_back_to_back_frames() {
        let a = Frame::new("audio-chunk")
            .with_data(json!({"rate": 16000, "width": 2, "channels": 1}))
            .with_payload(vec![1, 2, 3, 4]);
        let b = Frame::new("audio-stop");
        let c = Frame::new("transcript").with_data(json!({"text": "hello"}));
        let mut buf: Vec<u8> = Vec::new();
        a.write_async(&mut buf).await.unwrap();
        b.write_async(&mut buf).await.unwrap();
        c.write_async(&mut buf).await.unwrap();
        let mut reader = BufReader::new(buf.as_slice());
        assert_eq!(Frame::read_async(&mut reader).await.unwrap(), a);
        assert_eq!(Frame::read_async(&mut reader).await.unwrap(), b);
        assert_eq!(Frame::read_async(&mut reader).await.unwrap(), c);
    }

    #[tokio::test]
    async fn parses_split_data_block_from_peer() {
        // Synthesise a frame the way a strict Wyoming peer might —
        // header has data_length and a separate JSON object on the
        // wire. Our reader must merge them.
        let header = json!({
            "type": "audio-chunk",
            "data": { "rate": 16000 },
            "data_length": 28,
        });
        let mut buf = serde_json::to_vec(&header).unwrap();
        buf.push(b'\n');
        let extra = br#"{"width": 2, "channels": 1}"#;
        assert_eq!(extra.len(), 27);
        // Pad to data_length=28 with a space (still valid JSON).
        let extra = b"{\"width\": 2, \"channels\": 1} ";
        assert_eq!(extra.len(), 28);
        buf.extend_from_slice(extra);
        let mut reader = BufReader::new(buf.as_slice());
        let f = Frame::read_async(&mut reader).await.unwrap();
        assert_eq!(f.kind, "audio-chunk");
        assert_eq!(f.data["rate"], 16000);
        assert_eq!(f.data["width"], 2);
        assert_eq!(f.data["channels"], 1);
    }

    #[tokio::test]
    async fn writes_canonical_wyoming_data_block() {
        let f = Frame::new("info").with_data(json!({"asr": []}));
        let mut buf: Vec<u8> = Vec::new();
        f.write_async(&mut buf).await.unwrap();

        let header_end = buf.iter().position(|b| *b == b'\n').expect("newline");
        let header: Value = serde_json::from_slice(&buf[..header_end]).unwrap();
        assert_eq!(header["type"], "info");
        assert_eq!(header["version"], WYOMING_VERSION);
        assert_eq!(header["data_length"], 10);
        assert!(header.get("data").is_none());
        assert_eq!(&buf[(header_end + 1)..], br#"{"asr":[]}"#);
    }

    #[tokio::test]
    async fn writes_empty_frame_without_data_length() {
        let f = Frame::new("describe");
        let mut buf: Vec<u8> = Vec::new();
        f.write_async(&mut buf).await.unwrap();

        let header_end = buf.iter().position(|b| *b == b'\n').expect("newline");
        let header: Value = serde_json::from_slice(&buf[..header_end]).unwrap();
        assert_eq!(header["type"], "describe");
        assert_eq!(header["version"], WYOMING_VERSION);
        assert!(header.get("data_length").is_none());
        assert_eq!(buf.len(), header_end + 1);
    }

    #[tokio::test]
    async fn rejects_missing_type() {
        let buf = b"{\"foo\": \"bar\"}\n".to_vec();
        let mut reader = BufReader::new(buf.as_slice());
        let err = Frame::read_async(&mut reader).await.unwrap_err();
        assert!(matches!(err, FrameError::MissingType), "got {err:?}");
    }

    #[tokio::test]
    async fn rejects_malformed_json() {
        let buf = b"not json\n".to_vec();
        let mut reader = BufReader::new(buf.as_slice());
        let err = Frame::read_async(&mut reader).await.unwrap_err();
        assert!(matches!(err, FrameError::Json(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn rejects_truncated_payload() {
        let header = json!({"type": "audio-chunk", "payload_length": 100});
        let mut buf = serde_json::to_vec(&header).unwrap();
        buf.push(b'\n');
        buf.extend_from_slice(&[0u8; 10]); // only 10 of 100 bytes
        let mut reader = BufReader::new(buf.as_slice());
        let err = Frame::read_async(&mut reader).await.unwrap_err();
        assert!(
            matches!(err, FrameError::Truncated("payload")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn rejects_oversized_payload_length() {
        let header = json!({
            "type": "audio-chunk",
            "payload_length": MAX_PAYLOAD_BYTES + 1,
        });
        let mut buf = serde_json::to_vec(&header).unwrap();
        buf.push(b'\n');
        let mut reader = BufReader::new(buf.as_slice());
        let err = Frame::read_async(&mut reader).await.unwrap_err();
        assert!(matches!(err, FrameError::PayloadTooLong(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn rejects_oversized_data_length() {
        let header = json!({
            "type": "info",
            "data_length": MAX_DATA_BLOCK_BYTES + 1,
        });
        let mut buf = serde_json::to_vec(&header).unwrap();
        buf.push(b'\n');
        let mut reader = BufReader::new(buf.as_slice());
        let err = Frame::read_async(&mut reader).await.unwrap_err();
        assert!(matches!(err, FrameError::DataTooLong(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn rejects_eof_before_header() {
        let buf: Vec<u8> = Vec::new();
        let mut reader = BufReader::new(buf.as_slice());
        let err = Frame::read_async(&mut reader).await.unwrap_err();
        assert!(
            matches!(err, FrameError::Truncated("header line")),
            "got {err:?}"
        );
    }
}
