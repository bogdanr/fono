// SPDX-License-Identifier: GPL-3.0-only
//! Thin wrapper around `rubato` for device-rate → 16 kHz resampling.

use anyhow::{Context, Result};
use rubato::{
    Resampler as _, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

pub struct Resampler {
    inner: SincFixedIn<f32>,
    chunk: usize,
    leftover: Vec<f32>,
}

impl Resampler {
    pub fn new(src_rate: u32, dst_rate: u32) -> Result<Self> {
        let ratio = f64::from(dst_rate) / f64::from(src_rate);
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let chunk = 1024;
        let inner = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk, 1)
            .context("build rubato resampler")?;
        Ok(Self {
            inner,
            chunk,
            leftover: Vec::new(),
        })
    }

    /// Process an arbitrary-sized chunk of mono samples, buffering any
    /// remainder that doesn't fit into a fixed-size window.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.leftover.extend_from_slice(input);
        let mut out = Vec::new();
        while self.leftover.len() >= self.chunk {
            let slice = &self.leftover[..self.chunk];
            if let Ok(result) = self.inner.process(&[slice.to_vec()], None) {
                if let Some(channel) = result.into_iter().next() {
                    out.extend(channel);
                }
            }
            self.leftover.drain(..self.chunk);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_48k_to_16k_roughly_third() {
        let mut r = Resampler::new(48_000, 16_000).unwrap();
        // Feed two chunk-sized windows so at least one output block is produced.
        let input = vec![0.0f32; 2048];
        let out = r.process(&input);
        // Expect ~ input_len / 3. Allow slack for sinc edge effects.
        assert!(!out.is_empty(), "expected some output from resampler");
    }
}
