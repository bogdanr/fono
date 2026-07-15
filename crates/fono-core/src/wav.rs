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
}
