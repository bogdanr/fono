// SPDX-License-Identifier: GPL-3.0-only
//! Supertonic `voice.bin` style-pack parser (Slice 2, Task 2.2).
//!
//! `voice.bin` is a small binary holding every speaker's style vectors for
//! the two style-conditioned graphs: a 6×`i64` little-endian header giving the
//! `ttl` (text-encoder / vector-estimator) tensor shape `[S, ·, ·]` and the
//! `dp` (duration-predictor) tensor shape `[S, ·, ·]`, followed by the two
//! `f32` payloads back to back (`ttl` then `dp`). `S` is the speaker count
//! (10 for Supertonic 3). Ported verbatim (including the overflow / size
//! validation) from `ParseVoiceStyleFromBinary` +
//! `GetStyleSliceForSid` in the sherpa reference
//! `offline-tts-supertonic-impl.cc`.

use anyhow::{bail, Result};

/// A parsed `voice.bin`: the two style payloads and their 3-D shapes, both
/// batched over `S` speakers in dimension 0.
#[derive(Debug, Clone, PartialEq)]
pub struct SupertonicStyle {
    /// Flat `ttl` style data, row-major over `ttl_shape`.
    pub ttl_data: Vec<f32>,
    /// Flat `dp` style data, row-major over `dp_shape`.
    pub dp_data: Vec<f32>,
    /// `[S, ·, ·]` shape of the `ttl` payload.
    pub ttl_shape: [i64; 3],
    /// `[S, ·, ·]` shape of the `dp` payload.
    pub dp_shape: [i64; 3],
}

/// A per-speaker view into a [`SupertonicStyle`]: the `[1, ·, ·]` slices fed to
/// the graphs for one `sid`. Borrows the parent's buffers.
#[derive(Debug, Clone, Copy)]
pub struct StyleSlice<'a> {
    pub ttl_data: &'a [f32],
    pub ttl_shape: [i64; 3],
    pub dp_data: &'a [f32],
    pub dp_shape: [i64; 3],
}

/// Header is six `i64`: `ttl` dims [0..3) then `dp` dims [3..6).
const HEADER_LEN: usize = 6 * std::mem::size_of::<i64>();

/// Upper bound on a single payload, matching the reference's `kMaxPayloadBytes`
/// (64 MiB) — a corrupt header can't provoke a huge allocation.
const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

impl SupertonicStyle {
    /// Parse and fully validate a `voice.bin` buffer.
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < HEADER_LEN {
            bail!(
                "invalid voice.bin: file too small ({} bytes, need {} header)",
                buf.len(),
                HEADER_LEN
            );
        }
        let mut dims = [0i64; 6];
        for (i, d) in dims.iter_mut().enumerate() {
            let mut b = [0u8; 8];
            b.copy_from_slice(&buf[i * 8..i * 8 + 8]);
            *d = i64::from_le_bytes(b);
            if *d <= 0 {
                bail!("invalid voice.bin: dims[{i}] = {} <= 0", *d);
            }
        }

        let ttl_elems = mul3(dims[0], dims[1], dims[2], "ttl")?;
        let dp_elems = mul3(dims[3], dims[4], dims[5], "dp")?;

        // Element counts are already bounded by mul3; convert to bytes with an
        // explicit overflow guard mirroring the reference. The MAX_PAYLOAD_BYTES
        // bound is enforced on the summed payload below.
        let ttl_bytes = ttl_elems
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| anyhow::anyhow!("invalid voice.bin: ttl byte size overflow"))?;
        let dp_bytes = dp_elems
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| anyhow::anyhow!("invalid voice.bin: dp byte size overflow"))?;

        let payload_bytes = ttl_bytes
            .checked_add(dp_bytes)
            .ok_or_else(|| anyhow::anyhow!("invalid voice.bin: payload size overflow"))?;
        if payload_bytes > MAX_PAYLOAD_BYTES {
            bail!(
                "invalid voice.bin: payload too large ({payload_bytes} bytes, max {MAX_PAYLOAD_BYTES})"
            );
        }

        let expected_total = HEADER_LEN + payload_bytes;
        if buf.len() != expected_total {
            bail!(
                "invalid voice.bin: size mismatch (got {} bytes, expected exactly {expected_total})",
                buf.len()
            );
        }

        let ttl_data = read_f32_le(&buf[HEADER_LEN..HEADER_LEN + ttl_bytes]);
        let dp_data = read_f32_le(&buf[HEADER_LEN + ttl_bytes..expected_total]);

        Ok(Self {
            ttl_data,
            dp_data,
            ttl_shape: [dims[0], dims[1], dims[2]],
            dp_shape: [dims[3], dims[4], dims[5]],
        })
    }

    /// Number of speakers `S` (the shared leading dimension of both payloads).
    #[must_use]
    pub fn num_speakers(&self) -> i64 {
        self.ttl_shape[0]
    }

    /// The `[1, ·, ·]` style slices for one speaker id. `sid` is clamped into
    /// `[0, num_speakers-1]` (the reference clamps rather than errors).
    #[must_use]
    pub fn slice_for_sid(&self, sid: i64) -> StyleSlice<'_> {
        let s = if self.num_speakers() == 1 {
            0
        } else {
            sid.clamp(0, self.num_speakers() - 1) as usize
        };
        let ttl_slice = (self.ttl_shape[1] * self.ttl_shape[2]) as usize;
        let dp_slice = (self.dp_shape[1] * self.dp_shape[2]) as usize;
        StyleSlice {
            ttl_data: &self.ttl_data[s * ttl_slice..(s + 1) * ttl_slice],
            ttl_shape: [1, self.ttl_shape[1], self.ttl_shape[2]],
            dp_data: &self.dp_data[s * dp_slice..(s + 1) * dp_slice],
            dp_shape: [1, self.dp_shape[1], self.dp_shape[2]],
        }
    }
}

/// `a*b*c` as `usize` with the reference's positivity + overflow guard.
fn mul3(a: i64, b: i64, c: i64, name: &str) -> Result<usize> {
    let overflow = || anyhow::anyhow!("invalid voice.bin: {name} dims overflow");
    if a <= 0 || b <= 0 || c <= 0 {
        return Err(overflow());
    }
    let ab = (a as u64).checked_mul(b as u64).ok_or_else(overflow)?;
    let abc = ab.checked_mul(c as u64).ok_or_else(overflow)?;
    usize::try_from(abc).map_err(|_| overflow())
}

/// Reinterpret a byte slice (length a multiple of 4) as little-endian `f32`.
fn read_f32_le(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact-enough float equality for the integer-valued fixtures below
    /// (avoids clippy's `float_cmp`).
    fn eqf(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    /// Build a synthetic `voice.bin` for `s` speakers with `ttl` inner shape
    /// `[a,b]` and `dp` inner shape `[c,d]`, filling each speaker's data with
    /// its index so slicing is verifiable.
    fn make(spk_count: i64, ttl_a: i64, ttl_b: i64, dp_a: i64, dp_b: i64) -> Vec<u8> {
        let mut out = Vec::new();
        for dim in [spk_count, ttl_a, ttl_b, spk_count, dp_a, dp_b] {
            out.extend_from_slice(&dim.to_le_bytes());
        }
        let ttl_per = (ttl_a * ttl_b) as usize;
        let dp_per = (dp_a * dp_b) as usize;
        for spk in 0..spk_count {
            for _ in 0..ttl_per {
                out.extend_from_slice(&(spk as f32).to_le_bytes());
            }
        }
        for spk in 0..spk_count {
            for _ in 0..dp_per {
                out.extend_from_slice(&((100 + spk) as f32).to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn parses_multi_speaker_pack() {
        let buf = make(10, 1, 4, 1, 3);
        let st = SupertonicStyle::parse(&buf).expect("valid");
        assert_eq!(st.num_speakers(), 10);
        assert_eq!(st.ttl_shape, [10, 1, 4]);
        assert_eq!(st.dp_shape, [10, 1, 3]);
        assert_eq!(st.ttl_data.len(), 10 * 4);
        assert_eq!(st.dp_data.len(), 10 * 3);
    }

    #[test]
    fn slice_for_sid_selects_the_right_speaker() {
        let buf = make(10, 1, 4, 1, 3);
        let st = SupertonicStyle::parse(&buf).unwrap();
        let sl = st.slice_for_sid(7);
        assert_eq!(sl.ttl_shape, [1, 1, 4]);
        assert_eq!(sl.dp_shape, [1, 1, 3]);
        assert!(sl.ttl_data.iter().all(|&x| eqf(x, 7.0)), "ttl slice is speaker 7's rows");
        assert!(sl.dp_data.iter().all(|&x| eqf(x, 107.0)), "dp slice is speaker 7's rows");
    }

    #[test]
    fn slice_for_sid_clamps_out_of_range() {
        let buf = make(3, 1, 2, 1, 2);
        let st = SupertonicStyle::parse(&buf).unwrap();
        // sid 99 clamps to the last speaker (index 2).
        assert!(st.slice_for_sid(99).ttl_data.iter().all(|&x| eqf(x, 2.0)));
        // negative clamps to 0.
        assert!(st.slice_for_sid(-5).ttl_data.iter().all(|&x| eqf(x, 0.0)));
    }

    #[test]
    fn rejects_truncated_header() {
        let err = SupertonicStyle::parse(&[0u8; 8]).unwrap_err();
        assert!(err.to_string().contains("too small"));
    }

    #[test]
    fn rejects_nonpositive_dim() {
        let mut buf = make(2, 1, 2, 1, 2);
        // Zero out the very first dim (ttl S).
        buf[0..8].copy_from_slice(&0i64.to_le_bytes());
        let err = SupertonicStyle::parse(&buf).unwrap_err();
        assert!(err.to_string().contains("dims[0]"));
    }

    #[test]
    fn rejects_size_mismatch() {
        let mut buf = make(2, 1, 2, 1, 2);
        buf.push(0); // one trailing byte
        let err = SupertonicStyle::parse(&buf).unwrap_err();
        assert!(err.to_string().contains("size mismatch"));
    }

    #[test]
    fn rejects_oversized_payload() {
        // Header claims a payload far larger than MAX_PAYLOAD_BYTES, with no
        // matching bytes present — must fail on the size bound, not allocate.
        let mut buf = Vec::new();
        for dim in [1i64, 1, 20_000_000, 1, 1, 1] {
            buf.extend_from_slice(&dim.to_le_bytes());
        }
        let err = SupertonicStyle::parse(&buf).unwrap_err();
        assert!(
            err.to_string().contains("too large") || err.to_string().contains("overflow"),
            "got: {err}"
        );
    }
}
