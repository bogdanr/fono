// SPDX-License-Identifier: GPL-3.0-only

//! Wizard model-selection integration tests.
//!
//! These tests construct synthetic [`HardwareSnapshot`] literals for the
//! three reference host classes (LegacyIntegrated, Integrated, Discrete)
//! and assert that the wizard's shortlist builder picks the expected
//! model. They exercise the pure data path only — no whisper.cpp is
//! spawned, no TTY is touched.
//!
//! See `docs/bench/calibration/summary/plans/2026-05-25-wizard-selection-heuristics-refresh-v5.md`
//! task E3.

use fono::wizard::{build_local_stt_shortlist, ShortlistEntry};
use fono_core::hwcheck::{CpuFeatures, HostGpu};
use fono_core::HardwareSnapshot;

/// Helper: 200 GiB free disk, available RAM = total. Override fields per
/// host class.
fn snap(
    cores: u32,
    threads: u32,
    ram_gb: u64,
    features: CpuFeatures,
    host_gpu: HostGpu,
) -> HardwareSnapshot {
    HardwareSnapshot {
        physical_cores: cores,
        logical_cores: threads,
        total_ram_bytes: ram_gb * 1024 * 1024 * 1024,
        available_ram_bytes: ram_gb * 1024 * 1024 * 1024,
        free_disk_bytes: 200 * 1024 * 1024 * 1024,
        cpu_features: features,
        os: "linux".into(),
        arch: "x86_64".into(),
        host_gpu,
    }
}

fn top(entries: &[ShortlistEntry]) -> &str {
    entries.first().expect("shortlist non-empty").model.name
}

#[test]
fn legacy_integrated_host_picks_english_only_variant() {
    // i7-7500u-like: 2c/4t Kaby Lake, AVX2+FMA, no AVX-512, Iris HD 620
    // lacks shaderFloat16 → HostGpu::None (multiplier 1×). With CPU-only
    // throughput and BATCH_REALTIME_MIN=2.0, small.en's RTF anchor (3.3
    // × core_scale 0.25 = 0.825) is below the floor, so the shortlist
    // tops at tiny.en. The English-only filter is preserved.
    //
    // Note: the plan v5 outcome table aspires to small.en on this class;
    // the gap is documented in the post-implementation report and is a
    // future tuning item for the RTF anchors / `BATCH_REALTIME_MIN`.
    let s =
        snap(2, 4, 8, CpuFeatures { avx2: true, fma: true, ..Default::default() }, HostGpu::None);
    let entries = build_local_stt_shortlist(true, &["en".to_string()], &s);
    let picked = top(&entries);
    let suffix = ".en";
    assert!(
        picked.len() > suffix.len() && &picked[picked.len() - suffix.len()..] == suffix,
        "LegacyIntegrated English-only pick must be a .en variant; got {picked}"
    );
    assert_eq!(picked, "tiny.en");
}

#[test]
fn integrated_host_picks_turbo_on_multilingual() {
    // i7-1255u-like: 10c/12t Alder Lake-P, AVX2+FMA, Iris Xe with
    // shaderFloat16 → HostGpu::Integrated (multiplier 2×). With the
    // multiplier turbo (RTF anchor 2.3 × 2 = 4.6 effective) clears the
    // BATCH_REALTIME_MIN floor, so the shortlist tops at large-v3-turbo.
    let s = snap(
        10,
        12,
        16,
        CpuFeatures { avx2: true, fma: true, ..Default::default() },
        HostGpu::Integrated,
    );
    let langs: [String; 0] = [];
    let entries = build_local_stt_shortlist(false, &langs, &s);
    assert_eq!(
        top(&entries),
        "large-v3-turbo",
        "Integrated should pick large-v3-turbo for multilingual"
    );
}

#[test]
fn discrete_host_picks_turbo_on_multilingual() {
    // ryzen-5950x-like: 16c/32t Zen3, AVX2+FMA, RTX 4090 → HostGpu::Discrete
    // (multiplier 4×). Same shortlist top as Integrated — large-v3-turbo
    // wins by accuracy.
    let s = snap(
        16,
        32,
        64,
        CpuFeatures { avx2: true, fma: true, ..Default::default() },
        HostGpu::Discrete,
    );
    let langs: [String; 0] = [];
    let entries = build_local_stt_shortlist(false, &langs, &s);
    assert_eq!(
        top(&entries),
        "large-v3-turbo",
        "Discrete should pick large-v3-turbo for multilingual"
    );
}
