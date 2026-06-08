// SPDX-License-Identifier: GPL-3.0-only
//! Factual-question assistant benchmark fixtures, scoring, and JSON reports.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use fono_assistant::{Assistant, AssistantContext, ChatRole, ChatTurn};
use fono_core::turn_trace::{current_instant, TurnTrace};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const DEFAULT_FIXTURE_RELATIVE_PATH: &str = "tests/fixtures/assistant_factual/fixtures.toml";
const REPORT_SCHEMA_VERSION: &str = "assistant-factual-report-v1";

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantFactualManifest {
    pub suite_version: String,
    pub prompt_version: String,
    #[serde(rename = "fixture")]
    pub fixtures: Vec<AssistantFactualFixture>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantFactualFixture {
    pub id: String,
    pub language: String,
    pub question: String,
    #[serde(default)]
    pub expected: Vec<String>,
    #[serde(default)]
    pub forbidden: Vec<String>,
    #[serde(default = "default_max_words")]
    pub max_words: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantFactualReport {
    pub schema_version: String,
    pub suite_version: String,
    pub prompt_version: String,
    pub fixture_sha256: String,
    pub provider: String,
    pub model: String,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub runtime: BTreeMap<String, String>,
    pub machine_label: Option<String>,
    pub ran_at: String,
    pub iterations: usize,
    pub by_language: BTreeMap<String, AssistantFactualLangReport>,
    pub by_fixture: Vec<AssistantFactualFixtureReport>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssistantFactualLangReport {
    pub n: usize,
    pub mean_score: f32,
    pub pass_rate: f32,
    pub p50_latency_ms: u64,
    pub p95_latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantFactualFixtureReport {
    pub id: String,
    pub language: String,
    pub question: String,
    pub output: String,
    pub latency_ms: u64,
    pub time_to_first_token_ms: Option<u64>,
    #[serde(default)]
    pub delta_count: usize,
    #[serde(default)]
    pub prompt_chars: usize,
    #[serde(default)]
    pub system_prompt_chars: usize,
    #[serde(default)]
    pub history_turns: usize,
    #[serde(default)]
    pub history_chars: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_path: Option<String>,
    pub metrics: AssistantFactualMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantFactualMetrics {
    pub score: f32,
    pub passed: bool,
    pub matched_expected: Vec<String>,
    pub missing_expected: bool,
    pub forbidden_found: Vec<String>,
    pub word_count: usize,
    pub too_verbose: bool,
    pub empty_output: bool,
    pub looks_like_chatter: bool,
}

#[derive(Debug, Clone)]
pub struct AssistantFactualRunConfig {
    pub provider: String,
    pub model: String,
    pub endpoint: Option<String>,
    pub machine_label: Option<String>,
    pub iterations: usize,
    pub languages: Vec<String>,
    pub system_prompt_override: Option<String>,
    pub history_turns: usize,
    pub runtime: BTreeMap<String, String>,
}

pub fn load_manifest(path: &Path) -> Result<AssistantFactualManifest> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let manifest: AssistantFactualManifest = toml::from_str(&text)
        .with_context(|| format!("parse assistant factual fixtures from {}", path.display()))?;
    manifest.validate()?;
    Ok(manifest)
}

impl AssistantFactualManifest {
    pub fn validate(&self) -> Result<()> {
        if self.suite_version.trim().is_empty() {
            return Err(anyhow!("assistant factual manifest has empty suite_version"));
        }
        if self.prompt_version.trim().is_empty() {
            return Err(anyhow!("assistant factual manifest has empty prompt_version"));
        }
        if self.fixtures.is_empty() {
            return Err(anyhow!("assistant factual manifest has no fixtures"));
        }
        let mut ids = Vec::with_capacity(self.fixtures.len());
        for fx in &self.fixtures {
            if fx.id.trim().is_empty() {
                return Err(anyhow!("assistant factual fixture with empty id"));
            }
            if fx.language.trim().is_empty() {
                return Err(anyhow!("{}: empty language", fx.id));
            }
            if fx.question.trim().is_empty() {
                return Err(anyhow!("{}: empty question", fx.id));
            }
            if fx.expected.is_empty() {
                return Err(anyhow!(
                    "{}: expected must list at least one acceptable answer",
                    fx.id
                ));
            }
            ids.push(fx.id.as_str());
        }
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        if ids.len() != before {
            return Err(anyhow!("duplicate assistant factual fixture id"));
        }
        Ok(())
    }
}

pub async fn run_assistant_factual(
    manifest_path: &Path,
    manifest: &AssistantFactualManifest,
    assistant: Arc<dyn Assistant>,
    cfg: AssistantFactualRunConfig,
) -> Result<AssistantFactualReport> {
    let fixture_sha256 = sha256_file(manifest_path)?;
    let wanted_langs: Vec<String> = cfg.languages.iter().map(|s| s.to_ascii_lowercase()).collect();
    let fixtures: Vec<&AssistantFactualFixture> = manifest
        .fixtures
        .iter()
        .filter(|f| wanted_langs.is_empty() || wanted_langs.iter().any(|l| l == &f.language))
        .collect();
    if fixtures.is_empty() {
        return Err(anyhow!("no assistant factual fixtures matched languages {:?}", cfg.languages));
    }

    assistant.prewarm().await?;

    let mut by_fixture = Vec::with_capacity(fixtures.len() * cfg.iterations.max(1));
    for fx in fixtures {
        for _ in 0..cfg.iterations.max(1) {
            let ctx = AssistantContext {
                system_prompt: cfg
                    .system_prompt_override
                    .clone()
                    .unwrap_or_else(|| system_prompt_for(&fx.language)),
                language: Some(fx.language.clone()),
                history: synthetic_history(&fx.language, cfg.history_turns),
                ..AssistantContext::default()
            };
            let system_prompt_chars = ctx.system_prompt.chars().count();
            let history_turns = ctx.history.len();
            let history_chars =
                ctx.history.iter().map(|t| t.content.chars().count()).sum::<usize>();
            let prompt_chars = fx.question.chars().count() + system_prompt_chars + history_chars;
            let trace = TurnTrace::start_from_env();
            let _trace_guard = trace.as_ref().map(TurnTrace::make_current);
            current_instant(
                "bench.assistant_factual_request",
                "bench.assistant",
                "bench",
                json!({
                    "fixture_id": fx.id,
                    "language": fx.language,
                    "question_chars": fx.question.chars().count(),
                    "prompt_chars_approx": prompt_chars,
                    "system_prompt_chars": system_prompt_chars,
                    "history_turns": history_turns,
                    "history_chars": history_chars,
                }),
            );
            let started = Instant::now();
            let mut first_token_ms = None;
            let mut output = String::new();
            let mut delta_count = 0_usize;
            let mut stream = assistant
                .reply_stream(&fx.question, &ctx)
                .await
                .with_context(|| format!("assistant factual fixture {} failed", fx.id))?;
            while let Some(delta) = stream.next().await {
                let delta = delta.with_context(|| {
                    format!("assistant factual fixture {} stream failed", fx.id)
                })?;
                if delta.tool_event.is_some() {
                    continue;
                }
                if !delta.text.is_empty() {
                    delta_count += 1;
                    if first_token_ms.is_none() {
                        first_token_ms = Some(started.elapsed().as_millis() as u64);
                    }
                    output.push_str(&delta.text);
                }
            }
            let latency_ms = started.elapsed().as_millis() as u64;
            let metrics = score_fixture(fx, &output);
            if let Some(trace) = trace.as_ref() {
                trace.finish(json!({
                    "fixture_id": fx.id,
                    "language": fx.language,
                    "latency_ms": latency_ms,
                    "time_to_first_token_ms": first_token_ms,
                    "delta_count": delta_count,
                    "output_chars": output.chars().count(),
                    "prompt_chars_approx": prompt_chars,
                    "system_prompt_chars": system_prompt_chars,
                    "history_turns": history_turns,
                    "history_chars": history_chars,
                    "passed": metrics.passed,
                }));
            }
            let trace_path = trace.as_ref().map(|t| t.path().display().to_string());
            by_fixture.push(AssistantFactualFixtureReport {
                id: fx.id.clone(),
                language: fx.language.clone(),
                question: fx.question.clone(),
                output: output.trim().to_string(),
                latency_ms,
                time_to_first_token_ms: first_token_ms,
                delta_count,
                prompt_chars,
                system_prompt_chars,
                history_turns,
                history_chars,
                trace_path,
                metrics,
            });
        }
    }

    let by_language = aggregate_by_language(&by_fixture);
    Ok(AssistantFactualReport {
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

pub fn score_fixture(fx: &AssistantFactualFixture, output: &str) -> AssistantFactualMetrics {
    let trimmed = output.trim();
    let normalized = normalize(trimmed);
    let empty_output = normalized.is_empty();
    let matched_expected = fx
        .expected
        .iter()
        .filter(|s| normalized.contains(&normalize(s)))
        .cloned()
        .collect::<Vec<_>>();
    let missing_expected = matched_expected.is_empty();
    let forbidden_found = fx
        .forbidden
        .iter()
        .filter(|s| normalized.contains(&normalize(s)))
        .cloned()
        .collect::<Vec<_>>();
    let word_count = trimmed.split_whitespace().count();
    let too_verbose = word_count > fx.max_words;
    let looks_like_chatter = looks_like_chatter(trimmed);

    let mut penalties = 0.0_f32;
    if empty_output {
        penalties += 1.0;
    }
    if missing_expected {
        penalties += 0.65;
    }
    penalties += (forbidden_found.len() as f32 * 0.35).min(0.70);
    if too_verbose {
        penalties += 0.15;
    }
    if looks_like_chatter {
        penalties += 0.15;
    }
    let score = (1.0 - penalties).clamp(0.0, 1.0);
    let passed = !empty_output
        && !missing_expected
        && forbidden_found.is_empty()
        && !too_verbose
        && !looks_like_chatter;

    AssistantFactualMetrics {
        score,
        passed,
        matched_expected,
        missing_expected,
        forbidden_found,
        word_count,
        too_verbose,
        empty_output,
        looks_like_chatter,
    }
}

fn system_prompt_for(language: &str) -> String {
    match language {
        "ro" => "Răspunde concis la întrebarea factuală. Respectă limba utilizatorului. Nu explica dacă utilizatorul cere doar răspunsul.".to_string(),
        _ => "Answer the factual question concisely. Follow the requested output format. Do not explain when the user asks for only the answer.".to_string(),
    }
}

fn synthetic_history(language: &str, turns: usize) -> Vec<ChatTurn> {
    if turns == 0 {
        return Vec::new();
    }
    let pairs = turns.div_ceil(2);
    let mut out = Vec::with_capacity(turns);
    for idx in 0..pairs {
        if out.len() < turns {
            out.push(ChatTurn {
                role: ChatRole::User,
                content: synthetic_user_turn(language, idx),
                at: Instant::now(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            });
        }
        if out.len() < turns {
            out.push(ChatTurn {
                role: ChatRole::Assistant,
                content: synthetic_assistant_turn(language, idx),
                at: Instant::now(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            });
        }
    }
    out
}

fn synthetic_user_turn(language: &str, idx: usize) -> String {
    match language {
        "ro" => format!("Întrebare anterioară scurtă numărul {}.", idx + 1),
        _ => format!("Short previous question number {}.", idx + 1),
    }
}

fn synthetic_assistant_turn(language: &str, idx: usize) -> String {
    match language {
        "ro" => format!("Răspuns anterior concis numărul {}.", idx + 1),
        _ => format!("Concise previous answer number {}.", idx + 1),
    }
}

fn normalize(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn looks_like_chatter(output: &str) -> bool {
    let lower = output.trim_start().to_lowercase();
    lower.starts_with("sure")
        || lower.starts_with("here is")
        || lower.starts_with("the answer")
        || lower.starts_with("răspunsul")
        || lower.starts_with("sigur")
}

fn aggregate_by_language(
    rows: &[AssistantFactualFixtureReport],
) -> BTreeMap<String, AssistantFactualLangReport> {
    let mut grouped: BTreeMap<String, Vec<&AssistantFactualFixtureReport>> = BTreeMap::new();
    for row in rows {
        grouped.entry(row.language.clone()).or_default().push(row);
    }
    grouped
        .into_iter()
        .map(|(language, rows)| {
            let n = rows.len();
            let mean_score = rows.iter().map(|r| r.metrics.score).sum::<f32>() / n as f32;
            let pass_rate = rows.iter().filter(|r| r.metrics.passed).count() as f32 / n as f32;
            let mut latencies = rows.iter().map(|r| r.latency_ms).collect::<Vec<_>>();
            latencies.sort_unstable();
            let report = AssistantFactualLangReport {
                n,
                mean_score,
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

fn default_max_words() -> usize {
    24
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
        assert!(manifest.fixtures.iter().any(|f| f.language == "en"));
    }

    #[test]
    fn scoring_accepts_expected_answer() {
        let fx = AssistantFactualFixture {
            id: "x".into(),
            language: "en".into(),
            question: "capital?".into(),
            expected: vec!["Paris".into()],
            forbidden: vec!["Lyon".into()],
            max_words: 3,
        };
        let metrics = score_fixture(&fx, "Paris");
        assert!(metrics.passed);
        assert_eq!(metrics.score, 1.0);
    }

    #[test]
    fn scoring_rejects_wrong_verbose_answer() {
        let fx = AssistantFactualFixture {
            id: "x".into(),
            language: "en".into(),
            question: "capital?".into(),
            expected: vec!["Paris".into()],
            forbidden: vec!["Lyon".into()],
            max_words: 3,
        };
        let metrics = score_fixture(&fx, "Sure, the answer is Lyon, not Paris.");
        assert!(!metrics.passed);
        assert!(metrics.too_verbose);
        assert!(!metrics.forbidden_found.is_empty());
        assert!(metrics.looks_like_chatter);
    }
}
