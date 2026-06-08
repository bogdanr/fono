// SPDX-License-Identifier: GPL-3.0-only
//! Text-only polish benchmark fixtures, scoring, and JSON reports.
//!
//! This path isolates LLM cleanup quality from STT quality: each fixture
//! supplies a raw transcript, one acceptable polished reference, and cheap
//! invariants that should hold for any good cleanup output.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use fono_polish::traits::{FormatContext, TextFormatter};
use serde::{Deserialize, Serialize};

use crate::wer::word_error_rate;

pub const DEFAULT_FIXTURE_RELATIVE_PATH: &str = "tests/fixtures/polish_text/fixtures.toml";
const REPORT_SCHEMA_VERSION: &str = "polish-text-report-v1";

#[derive(Debug, Clone, Deserialize)]
pub struct PolishTextManifest {
    pub suite_version: String,
    pub prompt_version: String,
    #[serde(rename = "fixture")]
    pub fixtures: Vec<PolishTextFixture>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolishTextFixture {
    pub id: String,
    pub language: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub raw: String,
    pub reference: String,
    #[serde(default)]
    pub must_contain: Vec<String>,
    #[serde(default)]
    pub must_not_contain: Vec<String>,
    #[serde(default)]
    pub preserve_diacritics: bool,
    #[serde(default = "default_max_length_ratio")]
    pub max_length_ratio: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolishTextReport {
    pub schema_version: String,
    pub suite_version: String,
    pub prompt_version: String,
    pub fixture_sha256: String,
    pub provider: String,
    pub model: String,
    pub endpoint: Option<String>,
    pub runtime: BTreeMap<String, String>,
    pub machine_label: Option<String>,
    pub ran_at: String,
    pub iterations: usize,
    pub by_language: BTreeMap<String, PolishTextLangReport>,
    pub by_fixture: Vec<PolishTextFixtureReport>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolishTextLangReport {
    pub n: usize,
    pub mean_score: f32,
    pub mean_reference_wer: f32,
    pub pass_rate: f32,
    pub p50_latency_ms: u64,
    pub p95_latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolishTextFixtureReport {
    pub id: String,
    pub language: String,
    pub tags: Vec<String>,
    pub raw: String,
    pub reference: String,
    pub output: String,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub metrics: PolishTextMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolishTextMetrics {
    pub score: f32,
    pub passed: bool,
    pub reference_wer: f32,
    pub length_ratio: f32,
    pub empty_output: bool,
    pub must_contain_missing: Vec<String>,
    pub must_not_contain_found: Vec<String>,
    pub missing_diacritics: Vec<String>,
    pub looks_like_chatter: bool,
}

#[derive(Debug, Clone)]
pub struct PolishTextRunConfig {
    pub provider: String,
    pub model: String,
    pub endpoint: Option<String>,
    pub runtime: BTreeMap<String, String>,
    pub machine_label: Option<String>,
    pub iterations: usize,
    pub languages: Vec<String>,
}

pub fn load_manifest(path: &Path) -> Result<PolishTextManifest> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let manifest: PolishTextManifest = toml::from_str(&text)
        .with_context(|| format!("parse polish fixtures from {}", path.display()))?;
    manifest.validate()?;
    Ok(manifest)
}

impl PolishTextManifest {
    pub fn validate(&self) -> Result<()> {
        if self.suite_version.trim().is_empty() {
            return Err(anyhow!("polish fixture manifest has empty suite_version"));
        }
        if self.prompt_version.trim().is_empty() {
            return Err(anyhow!("polish fixture manifest has empty prompt_version"));
        }
        if self.fixtures.is_empty() {
            return Err(anyhow!("polish fixture manifest has no fixtures"));
        }
        let mut ids = Vec::with_capacity(self.fixtures.len());
        for fx in &self.fixtures {
            if fx.id.trim().is_empty() {
                return Err(anyhow!("polish fixture with empty id"));
            }
            if fx.language.trim().is_empty() {
                return Err(anyhow!("{}: empty language", fx.id));
            }
            if fx.raw.trim().is_empty() {
                return Err(anyhow!("{}: empty raw text", fx.id));
            }
            if fx.reference.trim().is_empty() {
                return Err(anyhow!("{}: empty reference", fx.id));
            }
            if fx.max_length_ratio < 1.0 {
                return Err(anyhow!("{}: max_length_ratio must be >= 1.0", fx.id));
            }
            ids.push(fx.id.as_str());
        }
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        if ids.len() != before {
            return Err(anyhow!("duplicate polish fixture id"));
        }
        Ok(())
    }
}

pub async fn run_polish_text(
    manifest_path: &Path,
    manifest: &PolishTextManifest,
    formatter: Arc<dyn TextFormatter>,
    cfg: PolishTextRunConfig,
) -> Result<PolishTextReport> {
    let fixture_sha256 = sha256_file(manifest_path)?;
    let wanted_langs: Vec<String> = cfg.languages.iter().map(|s| s.to_ascii_lowercase()).collect();
    let fixtures: Vec<&PolishTextFixture> = manifest
        .fixtures
        .iter()
        .filter(|f| wanted_langs.is_empty() || wanted_langs.iter().any(|l| l == &f.language))
        .collect();
    if fixtures.is_empty() {
        return Err(anyhow!("no polish text fixtures matched languages {:?}", cfg.languages));
    }

    formatter.prewarm().await?;

    let mut by_fixture = Vec::with_capacity(fixtures.len() * cfg.iterations.max(1));
    for fx in fixtures {
        for _ in 0..cfg.iterations.max(1) {
            let ctx = FormatContext {
                language: Some(fx.language.clone()),
                candidate_languages: vec![fx.language.clone(), "en".to_string()],
                ..FormatContext::default()
            };
            let started = Instant::now();
            let result = formatter.format(&fx.raw, &ctx).await;
            let latency_ms = started.elapsed().as_millis() as u64;
            let (output, error) = match result {
                Ok(output) => (output, None),
                Err(err) => (String::new(), Some(format!("{err:#}"))),
            };
            let metrics = score_fixture(fx, &output);
            by_fixture.push(PolishTextFixtureReport {
                id: fx.id.clone(),
                language: fx.language.clone(),
                tags: fx.tags.clone(),
                raw: fx.raw.clone(),
                reference: fx.reference.clone(),
                output,
                latency_ms,
                error,
                metrics,
            });
        }
    }

    let by_language = aggregate_by_language(&by_fixture);
    Ok(PolishTextReport {
        schema_version: REPORT_SCHEMA_VERSION.to_string(),
        suite_version: manifest.suite_version.clone(),
        prompt_version: manifest.prompt_version.clone(),
        fixture_sha256,
        provider: cfg.provider,
        model: cfg.model,
        endpoint: cfg.endpoint,
        runtime: cfg.runtime,
        machine_label: cfg.machine_label,
        ran_at: now_rfc3339(),
        iterations: cfg.iterations.max(1),
        by_language,
        by_fixture,
    })
}

pub fn score_fixture(fx: &PolishTextFixture, output: &str) -> PolishTextMetrics {
    let trimmed = output.trim();
    let empty_output = trimmed.is_empty();
    let reference_wer = word_error_rate(&fx.reference, trimmed);
    let raw_chars = fx.raw.chars().filter(|c| !c.is_whitespace()).count().max(1);
    let out_chars = trimmed.chars().filter(|c| !c.is_whitespace()).count();
    let length_ratio = out_chars as f32 / raw_chars as f32;
    let output_lower = trimmed.to_lowercase();

    let must_contain_missing = fx
        .must_contain
        .iter()
        .filter(|s| !output_lower.contains(&s.to_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    let must_not_contain_found = fx
        .must_not_contain
        .iter()
        .filter(|s| output_lower.contains(&s.to_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    let missing_diacritics = if fx.preserve_diacritics {
        required_diacritics(fx).into_iter().filter(|s| !trimmed.contains(s)).collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let looks_like_chatter = looks_like_chatter(trimmed);

    let mut penalties = 0.0_f32;
    if empty_output {
        penalties += 1.0;
    }
    penalties += (must_contain_missing.len() as f32 * 0.15).min(0.45);
    penalties += (must_not_contain_found.len() as f32 * 0.20).min(0.40);
    penalties += (missing_diacritics.len() as f32 * 0.10).min(0.30);
    if length_ratio > fx.max_length_ratio {
        penalties += ((length_ratio - fx.max_length_ratio) * 0.25).min(0.25);
    }
    if looks_like_chatter {
        penalties += 0.25;
    }
    penalties += (reference_wer * 0.20).min(0.20);
    let score = (1.0 - penalties).clamp(0.0, 1.0);
    let passed = !empty_output
        && must_contain_missing.is_empty()
        && must_not_contain_found.is_empty()
        && missing_diacritics.is_empty()
        && !looks_like_chatter
        && length_ratio <= fx.max_length_ratio;

    PolishTextMetrics {
        score,
        passed,
        reference_wer,
        length_ratio,
        empty_output,
        must_contain_missing,
        must_not_contain_found,
        missing_diacritics,
        looks_like_chatter,
    }
}

fn required_diacritics(fx: &PolishTextFixture) -> Vec<String> {
    let source = format!("{} {}", fx.reference, fx.must_contain.join(" "));
    let allowed = match fx.language.as_str() {
        "ro" => "ăâîșțĂÂÎȘȚ",
        "es" => "áéíóúñüÁÉÍÓÚÑÜ",
        "fr" => "àâæçéèêëîïôœùûüÿÀÂÆÇÉÈÊËÎÏÔŒÙÛÜŸ",
        "de" => "äöüßÄÖÜẞ",
        _ => "",
    };
    let mut out = Vec::new();
    for ch in source.chars().filter(|c| allowed.contains(*c)) {
        let s = ch.to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn looks_like_chatter(output: &str) -> bool {
    let lower = output.trim_start().to_lowercase();
    lower.starts_with("here is")
        || lower.starts_with("sure")
        || lower.starts_with("i can")
        || lower.starts_with("the cleaned")
        || lower.contains("<<<")
        || lower.contains(">>>")
}

fn aggregate_by_language(
    rows: &[PolishTextFixtureReport],
) -> BTreeMap<String, PolishTextLangReport> {
    let mut grouped: BTreeMap<String, Vec<&PolishTextFixtureReport>> = BTreeMap::new();
    for row in rows {
        grouped.entry(row.language.clone()).or_default().push(row);
    }
    grouped
        .into_iter()
        .map(|(language, rows)| {
            let n = rows.len();
            let mean_score = rows.iter().map(|r| r.metrics.score).sum::<f32>() / n as f32;
            let mean_reference_wer =
                rows.iter().map(|r| r.metrics.reference_wer).sum::<f32>() / n as f32;
            let pass_rate = rows.iter().filter(|r| r.metrics.passed).count() as f32 / n as f32;
            let mut latencies = rows.iter().map(|r| r.latency_ms).collect::<Vec<_>>();
            latencies.sort_unstable();
            let report = PolishTextLangReport {
                n,
                mean_score,
                mean_reference_wer,
                pass_rate,
                p50_latency_ms: percentile(&latencies, 50),
                p95_latency_ms: percentile(&latencies, 95),
            };
            (language, report)
        })
        .collect()
}

fn percentile(sorted: &[u64], p: u8) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p as f32 / 100.0) * sorted.len() as f32).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn default_max_length_ratio() -> f32 {
    2.0
}

fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, mo, d, h, mi, s) = epoch_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[allow(clippy::many_single_char_names)]
fn epoch_to_ymdhms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let (s, m, h) = (secs % 60, (secs / 60) % 60, (secs / 3600) % 24);
    let mut days = (secs / 86_400) as i64;
    let mut year: i64 = 1970;
    loop {
        let ydays = if is_leap(year) { 366 } else { 365 };
        if days < ydays {
            break;
        }
        days -= ydays;
        year += 1;
    }
    let leap = is_leap(year);
    let month_days: [i64; 12] =
        [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1i64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    (year as u32, month as u32, (days + 1) as u32, h as u32, m as u32, s as u32)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_fixture_file_loads() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let path = root.join(DEFAULT_FIXTURE_RELATIVE_PATH);
        let manifest = load_manifest(&path).unwrap();
        assert!(manifest.fixtures.iter().any(|f| f.language == "ro"));
        assert!(manifest.fixtures.iter().filter(|f| f.language == "ro").count() >= 4);
    }

    #[test]
    fn scoring_flags_missing_romanian_diacritics() {
        let fx = PolishTextFixture {
            id: "ro".into(),
            language: "ro".into(),
            tags: vec![],
            raw: "maine sedinta".into(),
            reference: "Mâine am ședință.".into(),
            must_contain: vec!["Mâine".into(), "ședință".into()],
            must_not_contain: vec!["Here is".into()],
            preserve_diacritics: true,
            max_length_ratio: 2.0,
        };
        let metrics = score_fixture(&fx, "Maine am sedinta.");
        assert!(!metrics.passed);
        assert!(!metrics.missing_diacritics.is_empty());
    }

    #[test]
    fn scoring_accepts_good_polish() {
        let fx = PolishTextFixture {
            id: "en".into(),
            language: "en".into(),
            tags: vec![],
            raw: "hi john send invoice".into(),
            reference: "Hi John, send the invoice.".into(),
            must_contain: vec!["John".into(), "invoice".into()],
            must_not_contain: vec!["Here is".into()],
            preserve_diacritics: false,
            max_length_ratio: 2.0,
        };
        let metrics = score_fixture(&fx, "Hi John, send the invoice.");
        assert!(metrics.passed);
        assert!(metrics.score > 0.9);
    }

    #[tokio::test]
    async fn fake_run_serializes_report() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let path = root.join(DEFAULT_FIXTURE_RELATIVE_PATH);
        let manifest = load_manifest(&path).unwrap();
        let cfg = PolishTextRunConfig {
            provider: "fake".into(),
            model: "fake".into(),
            endpoint: None,
            runtime: BTreeMap::new(),
            machine_label: Some("unit-test".into()),
            iterations: 1,
            languages: vec!["ro".into()],
        };
        let report =
            run_polish_text(&path, &manifest, Arc::new(crate::fakes::FakePolish::new()), cfg)
                .await
                .unwrap();
        assert_eq!(report.schema_version, REPORT_SCHEMA_VERSION);
        assert_eq!(report.machine_label.as_deref(), Some("unit-test"));
        assert!(report.by_language.contains_key("ro"));
        assert!(report.runtime.is_empty());
        assert!(!report.fixture_sha256.is_empty());
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("polish-text-report-v1"));
    }
}
