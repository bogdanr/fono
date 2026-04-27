// SPDX-License-Identifier: GPL-3.0-only
//! Generate the two synthetic-tone WAV fixtures committed under
//! `tests/fixtures/equivalence/`. Run once via:
//!
//! ```bash
//! cargo run -p fono-bench --example gen_equivalence_fixtures
//! ```
//!
//! The output is deterministic: same code path -> bit-identical bytes.
//! Re-run if the schema below changes.

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

const SAMPLE_RATE: u32 = 16_000;

fn write_wav(path: &PathBuf, samples: &[i16]) -> std::io::Result<()> {
    let data_size = (samples.len() * 2) as u32;
    let mut f = File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_size).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&SAMPLE_RATE.to_le_bytes())?;
    f.write_all(&(SAMPLE_RATE * 2).to_le_bytes())?; // byte rate
    f.write_all(&2u16.to_le_bytes())?; // block align
    f.write_all(&16u16.to_le_bytes())?; // bits
    f.write_all(b"data")?;
    f.write_all(&data_size.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

/// Produce a 440 Hz tone at 0.2 amplitude for `secs` seconds.
fn tone(secs: f32) -> Vec<i16> {
    let n = (SAMPLE_RATE as f32 * secs) as usize;
    let mut out = Vec::with_capacity(n);
    let two_pi_f = std::f32::consts::TAU * 440.0 / SAMPLE_RATE as f32;
    for i in 0..n {
        let s = (two_pi_f * i as f32).sin() * 0.2;
        out.push((s * i16::MAX as f32) as i16);
    }
    out
}

fn silence(secs: f32) -> Vec<i16> {
    vec![0i16; (SAMPLE_RATE as f32 * secs) as usize]
}

fn main() -> std::io::Result<()> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_dir = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("tests")
        .join("fixtures")
        .join("equivalence");
    fs::create_dir_all(&out_dir)?;

    // short-clean: 3 s of 440 Hz tone.
    let mut short = Vec::new();
    short.extend(tone(3.0));
    write_wav(&out_dir.join("short-clean.wav"), &short)?;

    // medium-pauses: tone(2s) + silence(1.5s) + tone(2s) + silence(1.5s) + tone(3s) ≈ 10s.
    let mut medium = Vec::new();
    medium.extend(tone(2.0));
    medium.extend(silence(1.5));
    medium.extend(tone(2.0));
    medium.extend(silence(1.5));
    medium.extend(tone(3.0));
    write_wav(&out_dir.join("medium-pauses.wav"), &medium)?;

    println!("wrote 2 synthetic-tone fixtures to {}", out_dir.display());
    Ok(())
}
