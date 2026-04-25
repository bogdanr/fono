// SPDX-License-Identifier: GPL-3.0-only
//! Minimal 16-bit PCM mono WAV reader/writer.
//!
//! The Fono runtime only ever produces 16 kHz mono 16-bit WAVs (see the
//! encoder in `crates/fono-stt/src/groq.rs`). This module parses the
//! same dialect back into `Vec<f32>` samples for the bench runner. We
//! deliberately don't pull in `hound` or `wav` to keep the bench crate
//! dependency surface tiny.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct WavData {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

/// Read a 16-bit PCM WAV from disk and decode into mono `Vec<f32>` in
/// the `[-1.0, 1.0]` range. Stereo input is averaged to mono.
pub fn read(path: &Path) -> Result<WavData> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    decode(&bytes).with_context(|| format!("decode {}", path.display()))
}

fn decode(bytes: &[u8]) -> Result<WavData> {
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(anyhow!("not a RIFF/WAVE file"));
    }

    // Walk chunks looking for `fmt ` and `data`.
    let mut pos = 12usize;
    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut bits_per_sample = 0u16;
    let mut audio_format = 0u16;
    let mut data: &[u8] = &[];

    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into()?) as usize;
        let body_start = pos + 8;
        let body_end = body_start + size;
        if body_end > bytes.len() {
            return Err(anyhow!("chunk {id:?} runs past EOF"));
        }
        match id {
            b"fmt " => {
                let f = &bytes[body_start..body_end];
                if f.len() < 16 {
                    return Err(anyhow!("fmt chunk too small"));
                }
                audio_format = u16::from_le_bytes(f[0..2].try_into()?);
                channels = u16::from_le_bytes(f[2..4].try_into()?);
                sample_rate = u32::from_le_bytes(f[4..8].try_into()?);
                bits_per_sample = u16::from_le_bytes(f[14..16].try_into()?);
            }
            b"data" => {
                data = &bytes[body_start..body_end];
            }
            _ => { /* skip LIST/INFO/etc. */ }
        }
        // Chunks are word-aligned.
        pos = body_end + (body_end & 1);
    }

    if audio_format != 1 {
        return Err(anyhow!("only PCM (format=1) supported, got {audio_format}"));
    }
    if bits_per_sample != 16 {
        return Err(anyhow!("only 16-bit PCM supported, got {bits_per_sample}"));
    }
    if channels == 0 || sample_rate == 0 || data.is_empty() {
        return Err(anyhow!("missing fmt/data fields"));
    }

    // i16 LE -> f32 normalised.
    let frame_bytes = 2 * channels as usize;
    let frames = data.len() / frame_bytes;
    let mut samples = Vec::with_capacity(frames);
    for i in 0..frames {
        let base = i * frame_bytes;
        let mut acc = 0i32;
        for c in 0..channels as usize {
            let s = i16::from_le_bytes(data[base + 2 * c..base + 2 * c + 2].try_into()?);
            acc += s as i32;
        }
        let avg = acc as f32 / channels as f32 / i16::MAX as f32;
        samples.push(avg.clamp(-1.0, 1.0));
    }

    Ok(WavData {
        sample_rate,
        channels: 1, // we collapsed to mono
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid 16-bit PCM mono WAV in memory.
    fn make_wav(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        let data_size = (samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(44 + data_size as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_size).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
        out.extend_from_slice(&2u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_size.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn roundtrip_known_samples() {
        let bytes = make_wav(16_000, &[0, 16383, -16384, i16::MAX, i16::MIN]);
        let wav = decode(&bytes).unwrap();
        assert_eq!(wav.sample_rate, 16_000);
        assert_eq!(wav.channels, 1);
        assert_eq!(wav.samples.len(), 5);
        assert!((wav.samples[0]).abs() < 1e-3);
        assert!((wav.samples[3] - 1.0).abs() < 1e-3);
        assert!((wav.samples[4] - -1.0).abs() < 1e-3);
    }

    #[test]
    fn rejects_non_riff() {
        assert!(decode(b"\x00\x00\x00\x00").is_err());
    }
}
