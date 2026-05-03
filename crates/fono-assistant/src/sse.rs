// SPDX-License-Identifier: GPL-3.0-only
//! Server-Sent Events line buffer.
//!
//! Both OpenAI-compatible streaming and Anthropic Messages streaming
//! emit SSE-format responses (`data:` / `event:` lines, blank-line
//! event separators). HTTP/2 chunks rarely line up with event
//! boundaries, so we buffer raw bytes and yield complete events one
//! at a time.

use std::str;

/// One parsed SSE event. `event` is the optional event-type tag (used
/// by Anthropic's `event: message_start` etc.); `data` is the
/// concatenation of all `data:` lines in the event.
#[derive(Debug, Clone, Default)]
pub struct SseEvent {
    /// Read by the Anthropic backend; OpenAI-compat ignores it. The
    /// `allow(dead_code)` keeps slim builds (only `openai-compat`)
    /// clippy-clean.
    #[allow(dead_code)]
    pub event: Option<String>,
    pub data: String,
}

/// Append-only line buffer. Feed raw bytes via [`Self::push`]; pull
/// completed events via [`Self::next_event`].
#[derive(Debug, Default)]
pub struct SseBuffer {
    buf: Vec<u8>,
    /// Partially-built event (reset when a blank line ends it).
    cur_event: Option<String>,
    cur_data: String,
}

impl SseBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Pull the next complete event, or `None` if more bytes are
    /// needed. Lines that aren't valid UTF-8 are skipped silently —
    /// every well-known SSE producer is UTF-8.
    pub fn next_event(&mut self) -> Option<SseEvent> {
        loop {
            // Find a `\n` (also strip a trailing `\r`).
            let nl = self.buf.iter().position(|b| *b == b'\n')?;
            let line_bytes = &self.buf[..nl];
            let line_bytes = match line_bytes.last() {
                Some(b'\r') => &line_bytes[..line_bytes.len() - 1],
                _ => line_bytes,
            };
            let line = str::from_utf8(line_bytes).ok().map(str::to_string);
            // Drain the line + newline.
            self.buf.drain(..=nl);
            let Some(line) = line else {
                continue;
            };

            if line.is_empty() {
                // Blank line — event ends.
                if self.cur_event.is_some() || !self.cur_data.is_empty() {
                    let ev = SseEvent {
                        event: self.cur_event.take(),
                        data: std::mem::take(&mut self.cur_data),
                    };
                    return Some(ev);
                }
                // Blank line with nothing buffered (keep-alive); skip.
                continue;
            }

            if line.starts_with(':') {
                // Comment / heartbeat (most servers send `: ping\n\n`).
                continue;
            }

            if let Some(rest) = line.strip_prefix("event:") {
                self.cur_event = Some(rest.trim().to_string());
                continue;
            }

            if let Some(rest) = line.strip_prefix("data:") {
                if !self.cur_data.is_empty() {
                    self.cur_data.push('\n');
                }
                // SSE allows a single optional space after the colon.
                let payload = rest.strip_prefix(' ').unwrap_or(rest);
                self.cur_data.push_str(payload);
            }

            // `id:`, `retry:`, unknown — ignore.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yields_single_data_event() {
        let mut b = SseBuffer::new();
        b.push(b"data: hello\n\n");
        let ev = b.next_event().expect("event");
        assert_eq!(ev.event, None);
        assert_eq!(ev.data, "hello");
        assert!(b.next_event().is_none());
    }

    #[test]
    fn yields_event_with_type() {
        let mut b = SseBuffer::new();
        b.push(b"event: message_delta\ndata: {\"x\":1}\n\n");
        let ev = b.next_event().expect("event");
        assert_eq!(ev.event.as_deref(), Some("message_delta"));
        assert_eq!(ev.data, "{\"x\":1}");
    }

    #[test]
    fn handles_split_chunks() {
        let mut b = SseBuffer::new();
        b.push(b"data: hel");
        assert!(b.next_event().is_none());
        b.push(b"lo wor");
        assert!(b.next_event().is_none());
        b.push(b"ld\n\n");
        let ev = b.next_event().expect("event");
        assert_eq!(ev.data, "hello world");
    }

    #[test]
    fn handles_multiple_events_in_one_push() {
        let mut b = SseBuffer::new();
        b.push(b"data: a\n\ndata: b\n\ndata: c\n\n");
        let mut got = Vec::new();
        while let Some(ev) = b.next_event() {
            got.push(ev.data);
        }
        assert_eq!(got, vec!["a", "b", "c"]);
    }

    #[test]
    fn ignores_comments_and_keepalives() {
        let mut b = SseBuffer::new();
        b.push(b": keepalive ping\n\ndata: real\n\n");
        let ev = b.next_event().expect("event");
        assert_eq!(ev.data, "real");
    }

    #[test]
    fn handles_crlf_line_endings() {
        let mut b = SseBuffer::new();
        b.push(b"data: hello\r\n\r\n");
        let ev = b.next_event().expect("event");
        assert_eq!(ev.data, "hello");
    }

    #[test]
    fn concatenates_multiple_data_lines_with_newline() {
        let mut b = SseBuffer::new();
        b.push(b"data: first\ndata: second\n\n");
        let ev = b.next_event().expect("event");
        assert_eq!(ev.data, "first\nsecond");
    }

    #[test]
    fn ignores_unknown_fields() {
        let mut b = SseBuffer::new();
        b.push(b"id: 42\nretry: 1000\ndata: payload\n\n");
        let ev = b.next_event().expect("event");
        assert_eq!(ev.data, "payload");
    }
}
