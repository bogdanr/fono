// SPDX-License-Identifier: GPL-3.0-only
//! OpenAI-compatible audio surface: `POST /v1/audio/speech` and
//! `POST /v1/audio/transcriptions`.
//!
//! Both are thin wire adapters over daemon-supplied closures. Speech reuses
//! the same [`crate::web_settings::SpeechFn`] the settings server mounts, so
//! the exact routing/synthesis logic is shared between the two HTTP surfaces.
//! Transcription parses the multipart upload (no new dependency — a tiny
//! hand-rolled `multipart/form-data` splitter), hands the raw file bytes to
//! the [`TranscribeProvider`], and returns the transcript in OpenAI's
//! `{"text": …}` JSON shape (or plain text when `response_format=text`).
//!
//! When the daemon supplies no audio closures (audio disabled), the routes
//! return a clean OpenAI-shaped 404 rather than pretending to exist.

use std::sync::Arc;

use futures::future::BoxFuture;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use super::access_log::ReqLog;
use super::messages::read_body_bytes;
use super::{audio_response, openai_error, ResBody, ServerCtx};

/// Parsed `/v1/audio/transcriptions` request handed to the daemon closure.
pub struct TranscribeRequest {
    /// Raw uploaded file bytes (the daemon decodes WAV/PCM).
    pub audio: Vec<u8>,
    /// Requested model / route selector (may be empty = configured backend).
    pub model: String,
    /// Optional language hint (ISO-639-1).
    pub language: Option<String>,
}

/// Daemon closure that transcribes an uploaded clip and returns its text.
pub type TranscribeProvider =
    Arc<dyn Fn(TranscribeRequest) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

// --- POST /v1/audio/speech ----------------------------------------------

pub async fn speech(
    req: Request<Incoming>,
    ctx: &ServerCtx,
    _log: &mut ReqLog,
) -> Response<ResBody> {
    let Some(speak) = ctx.speech.clone() else {
        return openai_error(StatusCode::NOT_FOUND, "speech synthesis is not enabled");
    };
    let bytes = match read_body_bytes(req).await {
        Ok(b) => b,
        Err(e) => return openai_error(StatusCode::BAD_REQUEST, &e),
    };
    let body: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return openai_error(StatusCode::BAD_REQUEST, &format!("invalid JSON body: {e}")),
    };
    match speak(body).await {
        Ok((content_type, audio)) => audio_response(content_type, audio),
        Err(e) => openai_error(StatusCode::BAD_REQUEST, &e),
    }
}

// --- POST /v1/audio/transcriptions --------------------------------------

pub async fn transcriptions(
    req: Request<Incoming>,
    ctx: &ServerCtx,
    _log: &mut ReqLog,
) -> Response<ResBody> {
    let Some(transcribe) = ctx.transcribe.clone() else {
        return openai_error(StatusCode::NOT_FOUND, "transcription is not enabled");
    };
    // The multipart boundary lives in the Content-Type header, which we must
    // read before consuming the body.
    let boundary = req
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(boundary_of);
    let Some(boundary) = boundary else {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "expected multipart/form-data with a boundary",
        );
    };
    let bytes = match read_body_bytes(req).await {
        Ok(b) => b,
        Err(e) => return openai_error(StatusCode::BAD_REQUEST, &e),
    };
    let parts = parse_multipart(&bytes, &boundary);
    let field = |name: &str| parts.iter().find(|(n, _)| n == name).map(|(_, v)| v);
    let Some(audio) = field("file") else {
        return openai_error(StatusCode::BAD_REQUEST, "missing `file` part");
    };
    let model =
        field("model").map(|v| String::from_utf8_lossy(v).trim().to_string()).unwrap_or_default();
    let language = field("language")
        .map(|v| String::from_utf8_lossy(v).trim().to_string())
        .filter(|s| !s.is_empty());
    let response_format = field("response_format")
        .map(|v| String::from_utf8_lossy(v).trim().to_string())
        .unwrap_or_else(|| "json".to_string());

    let request = TranscribeRequest { audio: audio.clone(), model, language };
    match transcribe(request).await {
        Ok(text) => {
            if response_format == "text" {
                super::text_response(StatusCode::OK, &text)
            } else {
                super::json_ok(&serde_json::json!({ "text": text }))
            }
        }
        Err(e) => openai_error(StatusCode::BAD_REQUEST, &e),
    }
}

// --- minimal multipart/form-data parsing (no extra dependency) ----------

/// Extract the `boundary=` token from a `multipart/form-data` content type.
fn boundary_of(content_type: &str) -> Option<String> {
    if !content_type.trim_start().starts_with("multipart/form-data") {
        return None;
    }
    content_type.split(';').find_map(|p| {
        let p = p.trim();
        p.strip_prefix("boundary=").map(|b| b.trim_matches('"').to_string())
    })
}

/// Split a multipart body into `(field-name, raw-value-bytes)` pairs. Only the
/// `name=` disposition and the raw part body are recovered — enough for the
/// transcription upload (`file`, `model`, `language`, `response_format`).
fn parse_multipart(body: &[u8], boundary: &str) -> Vec<(String, Vec<u8>)> {
    let delim = format!("--{boundary}");
    let mut out = Vec::new();
    for part in split_on(body, delim.as_bytes()) {
        // Each real part starts with CRLF after the delimiter; the closing
        // "--" segment and any preamble/epilogue lack a header block.
        let part = part.strip_prefix(b"\r\n").unwrap_or(part);
        let Some(idx) = find(part, b"\r\n\r\n") else { continue };
        let headers = String::from_utf8_lossy(&part[..idx]);
        let mut value = &part[idx + 4..];
        if value.ends_with(b"\r\n") {
            value = &value[..value.len() - 2];
        }
        if let Some(name) = disposition_name(&headers) {
            out.push((name, value.to_vec()));
        }
    }
    out
}

/// Pull `name="…"` out of a part's Content-Disposition header block.
fn disposition_name(headers: &str) -> Option<String> {
    let marker = "name=\"";
    let start = headers.find(marker)? + marker.len();
    let rest = &headers[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Split `hay` on every occurrence of `sep`, returning the between-slices.
fn split_on<'a>(hay: &'a [u8], sep: &[u8]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + sep.len() <= hay.len() {
        if &hay[i..i + sep.len()] == sep {
            out.push(&hay[start..i]);
            i += sep.len();
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(&hay[start..]);
    out
}

/// First index of `needle` in `hay`, or `None`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_parsing() {
        assert_eq!(boundary_of("multipart/form-data; boundary=abc123").as_deref(), Some("abc123"));
        assert_eq!(boundary_of("application/json"), None);
    }

    #[test]
    fn multipart_extracts_named_fields() {
        let b = "X";
        let body = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-1\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\n\
             Content-Type: audio/wav\r\n\r\nRIFFDATA\r\n--{b}--\r\n"
        );
        let parts = parse_multipart(body.as_bytes(), b);
        let get = |n: &str| parts.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
        assert_eq!(get("model").unwrap(), b"whisper-1");
        assert_eq!(get("file").unwrap(), b"RIFFDATA");
    }
}
