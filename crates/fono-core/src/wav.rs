// SPDX-License-Identifier: GPL-3.0-only
//! Minimal mono WAV (RIFF/PCM) encode + decode helpers, shared across the
//! crates that move audio over HTTP.
//!
//! Two callers need this and must not depend on each other: `fono-stt`
//! encodes captured microphone PCM into a WAV upload for cloud transcription,
//! and `fono-net`'s OpenAI-compatible `/v1/audio/speech` gateway encodes
//! synthesized [`fono_tts::TtsAudio`] PCM into a WAV response body. Both crates
//! already depend on `fono-core`, so this is the natural shared home (avoids a
//! `fono-net → fono-stt` edge). No external crate is pulled in — the format is
//! a fixed 44-byte header plus interleaved little-endian samples.

/// Encode mono `f32` samples (nominally in `[-1.0, 1.0]`) as a 16-bit PCM WAV
/// blob at `sample_rate` Hz. Samples are clamped before quantization so
/// out-of-range values never wrap.
#[must_use]
pub fn encode_wav(pcm: &[f32], sample_rate: u32) -> Vec<u8> {
    let num_samples = pcm.len() as u32;
    let byte_rate = sample_rate * 2;
    let data_size = num_samples * 2;
    let mut out = Vec::with_capacity(44 + data_size as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    for s in pcm {
        let clamped = s.clamp(-1.0, 1.0);
        let i = (clamped * i16::MAX as f32) as i16;
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}

/// Encode mono `f32` samples as raw little-endian 16-bit PCM (no header),
/// the OpenAI `response_format = "pcm"` shape (signed 16-bit, mono).
#[must_use]
pub fn encode_pcm_s16le(pcm: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        let i = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}

/// Decode a mono/stereo 16-bit PCM WAV blob into mono `f32` samples plus the
/// sample rate. Multi-channel input is downmixed to mono by averaging.
///
/// This is intentionally minimal: it handles the canonical 16-bit PCM RIFF
/// layout that Fono itself emits and that the common OpenAI-client uploads
/// use (`file` field of `/v1/audio/transcriptions`). Non-WAV or non-16-bit
/// input yields an error so the caller can return a clean 400 rather than
/// pulling in a general audio-decoding dependency (binary-size budget).
///
/// # Errors
/// Returns a message when the header is not a 16-bit PCM `WAVE`/`RIFF`
/// container or the `data`/`fmt ` chunks cannot be located.
pub fn decode_wav(bytes: &[u8]) -> std::result::Result<(Vec<f32>, u32), String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE container".to_string());
    }
    let mut pos = 12;
    let mut sample_rate = 0u32;
    let mut channels = 1u16;
    let mut bits = 16u16;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size =
            u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]])
                as usize;
        let body_start = pos + 8;
        let body_end = (body_start + size).min(bytes.len());
        match id {
            b"fmt " if size >= 16 => {
                channels =
                    u16::from_le_bytes([bytes[body_start + 2], bytes[body_start + 3]]).max(1);
                sample_rate = u32::from_le_bytes([
                    bytes[body_start + 4],
                    bytes[body_start + 5],
                    bytes[body_start + 6],
                    bytes[body_start + 7],
                ]);
                bits = u16::from_le_bytes([bytes[body_start + 14], bytes[body_start + 15]]);
            }
            b"data" => data = Some(&bytes[body_start..body_end]),
            _ => {}
        }
        // Chunks are word-aligned: an odd size carries a pad byte.
        pos = body_start + size + (size & 1);
    }
    if bits != 16 {
        return Err(format!("unsupported WAV bit depth {bits} (only 16-bit PCM is decoded)"));
    }
    let data = data.ok_or_else(|| "WAV has no data chunk".to_string())?;
    if sample_rate == 0 {
        return Err("WAV has no fmt chunk / sample rate".to_string());
    }
    let ch = channels as usize;
    let frames = data.len() / (2 * ch);
    let mut pcm = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut acc = 0.0f32;
        for c in 0..ch {
            let o = (f * ch + c) * 2;
            let s = i16::from_le_bytes([data[o], data[o + 1]]);
            acc += f32::from(s) / f32::from(i16::MAX);
        }
        pcm.push(acc / ch as f32);
    }
    Ok((pcm, sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_44_bytes_plus_data() {
        let blob = encode_wav(&[0.0; 16], 16_000);
        assert_eq!(&blob[..4], b"RIFF");
        assert_eq!(&blob[8..12], b"WAVE");
        assert_eq!(blob.len(), 44 + 32);
    }

    #[test]
    fn pcm_is_two_bytes_per_sample_and_clamps() {
        let blob = encode_pcm_s16le(&[0.0, 1.0, -1.0, 2.0]);
        assert_eq!(blob.len(), 8);
        // 1.0 → i16::MAX, -1.0 → -i16::MAX, 2.0 clamps to i16::MAX.
        assert_eq!(i16::from_le_bytes([blob[2], blob[3]]), i16::MAX);
        assert_eq!(i16::from_le_bytes([blob[4], blob[5]]), -i16::MAX);
        assert_eq!(i16::from_le_bytes([blob[6], blob[7]]), i16::MAX);
    }

    #[test]
    fn wav_encode_decode_round_trips() {
        let src = vec![0.0f32, 0.5, -0.5, 1.0, -1.0];
        let blob = encode_wav(&src, 16_000);
        let (pcm, sr) = decode_wav(&blob).expect("decode");
        assert_eq!(sr, 16_000);
        assert_eq!(pcm.len(), src.len());
        for (a, b) in pcm.iter().zip(src.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn decode_rejects_non_wav() {
        assert!(decode_wav(b"not a wav at all").is_err());
    }
}
