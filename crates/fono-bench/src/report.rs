// SPDX-License-Identifier: GPL-3.0-only
//! Aggregated reporting. One `Report` per `fono-bench` invocation;
//! contains per-clip detail and per-language aggregates with p50 / p95
//! latencies and mean WER.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipReport {
    pub id: String,
    pub language: String,
    pub reference: String,
    pub hypothesis: String,
    pub wer: f32,
    pub stt_ms: u64,
    pub llm_ms: Option<u64>,
    pub total_ms: u64,
    pub samples: usize,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LangReport {
    pub n: usize,
    pub mean_wer: f32,
    pub p50_total_ms: u64,
    pub p95_total_ms: u64,
    pub p50_stt_ms: u64,
    pub p95_stt_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub provider_stt: String,
    pub provider_llm: Option<String>,
    pub ran_at: String,
    pub by_language: BTreeMap<String, LangReport>,
    pub by_clip: Vec<ClipReport>,
}

impl Report {
    pub fn build(
        provider_stt: impl Into<String>,
        provider_llm: Option<String>,
        clips: Vec<ClipReport>,
    ) -> Self {
        let mut by_lang: BTreeMap<String, Vec<ClipReport>> = BTreeMap::new();
        for c in &clips {
            by_lang
                .entry(c.language.clone())
                .or_default()
                .push(c.clone());
        }

        let by_language = by_lang
            .into_iter()
            .map(|(lang, group)| {
                let n = group.len();
                let mean_wer = group.iter().map(|c| c.wer).sum::<f32>() / n as f32;
                let mut totals: Vec<u64> = group.iter().map(|c| c.total_ms).collect();
                let mut stts: Vec<u64> = group.iter().map(|c| c.stt_ms).collect();
                totals.sort_unstable();
                stts.sort_unstable();
                let lr = LangReport {
                    n,
                    mean_wer,
                    p50_total_ms: percentile(&totals, 50),
                    p95_total_ms: percentile(&totals, 95),
                    p50_stt_ms: percentile(&stts, 50),
                    p95_stt_ms: percentile(&stts, 95),
                };
                (lang, lr)
            })
            .collect();

        Self {
            provider_stt: provider_stt.into(),
            provider_llm,
            ran_at: now_rfc3339(),
            by_language,
            by_clip: clips,
        }
    }

    /// Return `Err(diff)` if any language regresses against `baseline`
    /// beyond `wer_pp_max` percentage points or `latency_pct_max`
    /// percent on `p95_total_ms`. Used by CI gating.
    pub fn check_regression(
        &self,
        baseline: &Self,
        wer_pp_max: f32,
        latency_pct_max: f32,
    ) -> Result<(), Vec<String>> {
        let mut issues = Vec::new();
        for (lang, cur) in &self.by_language {
            let Some(base) = baseline.by_language.get(lang) else {
                continue;
            };
            let wer_diff = cur.mean_wer - base.mean_wer;
            if wer_diff > wer_pp_max / 100.0 {
                issues.push(format!(
                    "{lang}: WER regressed by {:.1}pp (baseline {:.3} → now {:.3})",
                    wer_diff * 100.0,
                    base.mean_wer,
                    cur.mean_wer
                ));
            }
            if base.p95_total_ms > 0 {
                let pct = (cur.p95_total_ms as f32 - base.p95_total_ms as f32)
                    / base.p95_total_ms as f32
                    * 100.0;
                if pct > latency_pct_max {
                    issues.push(format!(
                        "{lang}: p95 latency regressed by {pct:.1}% (baseline {} ms → now {} ms)",
                        base.p95_total_ms, cur.p95_total_ms
                    ));
                }
            }
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }
}

/// Nearest-rank percentile on a pre-sorted slice. Returns 0 for empty input.
fn percentile(sorted: &[u64], p: u8) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p as f32 / 100.0) * sorted.len() as f32).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Lightweight RFC-3339 formatter; we deliberately don't pull `chrono`.
    let (y, mo, d, h, mi, s) = epoch_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[allow(clippy::many_single_char_names)]
fn epoch_to_ymdhms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let (s, m, h) = (secs % 60, (secs / 60) % 60, (secs / 3600) % 24);
    let mut days = (secs / 86_400) as i64;
    // 1970-01-01 was a Thursday; we don't need DOW. Year math:
    let mut year: i64 = 1970;
    loop {
        let leap = is_leap(year);
        let ydays = if leap { 366 } else { 365 };
        if days < ydays {
            break;
        }
        days -= ydays;
        year += 1;
    }
    let leap = is_leap(year);
    let month_days: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1i64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    let day = (days + 1) as u32;
    (year as u32, month as u32, day, h as u32, m as u32, s as u32)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_basic() {
        let v = vec![10u64, 20, 30, 40, 50];
        assert_eq!(percentile(&v, 50), 30);
        assert_eq!(percentile(&v, 95), 50);
        assert_eq!(percentile(&[], 50), 0);
    }

    #[test]
    fn build_groups_by_language() {
        let clips = vec![
            ClipReport {
                id: "a".into(),
                language: "en".into(),
                reference: "x".into(),
                hypothesis: "x".into(),
                wer: 0.0,
                stt_ms: 100,
                llm_ms: Some(50),
                total_ms: 150,
                samples: 16,
                sample_rate: 16_000,
            },
            ClipReport {
                id: "b".into(),
                language: "es".into(),
                reference: "y".into(),
                hypothesis: "z".into(),
                wer: 1.0,
                stt_ms: 200,
                llm_ms: None,
                total_ms: 200,
                samples: 16,
                sample_rate: 16_000,
            },
        ];
        let r = Report::build("fake", None, clips);
        assert_eq!(r.by_language.len(), 2);
        assert_eq!(r.by_language["en"].n, 1);
        assert_eq!(r.by_language["es"].mean_wer, 1.0);
    }

    #[test]
    fn regression_check_flags_wer_drift() {
        let baseline = Report::build(
            "fake",
            None,
            vec![ClipReport {
                id: "a".into(),
                language: "en".into(),
                reference: "x".into(),
                hypothesis: "x".into(),
                wer: 0.05,
                stt_ms: 100,
                llm_ms: None,
                total_ms: 100,
                samples: 16,
                sample_rate: 16_000,
            }],
        );
        let current = Report::build(
            "fake",
            None,
            vec![ClipReport {
                id: "a".into(),
                language: "en".into(),
                reference: "x".into(),
                hypothesis: "x".into(),
                wer: 0.20, // +15pp
                stt_ms: 100,
                llm_ms: None,
                total_ms: 100,
                samples: 16,
                sample_rate: 16_000,
            }],
        );
        let issues = current.check_regression(&baseline, 5.0, 15.0).unwrap_err();
        assert!(issues.iter().any(|s| s.contains("WER regressed")));
    }
}
