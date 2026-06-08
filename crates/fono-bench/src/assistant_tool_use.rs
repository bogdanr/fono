// SPDX-License-Identifier: GPL-3.0-only
//! Simulated Home Assistant light-control tool-use benchmark for assistant models.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_FIXTURE_RELATIVE_PATH: &str =
    "tests/fixtures/assistant_tool_use/homeassistant_lights.toml";
const REPORT_SCHEMA_VERSION: &str = "assistant-tool-use-report-v1";

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantToolUseManifest {
    pub suite_version: String,
    pub prompt_version: String,
    #[serde(default)]
    pub inventory: Vec<HomeAssistantEntity>,
    #[serde(rename = "fixture")]
    pub fixtures: Vec<AssistantToolUseFixture>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeAssistantEntity {
    pub entity_id: String,
    pub name: String,
    pub area: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantToolUseFixture {
    pub id: String,
    pub language: String,
    pub request: String,
    #[serde(default)]
    pub expect_no_tool: bool,
    #[serde(default)]
    pub expected_action: Option<String>,
    #[serde(default)]
    pub expected_entity_ids: Vec<String>,
    #[serde(default)]
    pub expected_area: Option<String>,
    #[serde(default = "default_tool_result")]
    pub tool_result: String,
    #[serde(default)]
    pub expected_final: Vec<String>,
    #[serde(default)]
    pub forbidden_final: Vec<String>,
    #[serde(default = "default_max_words")]
    pub max_words: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantToolUseReport {
    pub schema_version: String,
    pub suite_version: String,
    pub prompt_version: String,
    pub fixture_sha256: String,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub machine_label: Option<String>,
    pub ran_at: String,
    pub iterations: usize,
    pub by_language: BTreeMap<String, AssistantToolUseLangReport>,
    pub by_fixture: Vec<AssistantToolUseFixtureReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantToolUseLangReport {
    pub n: usize,
    pub mean_score: f32,
    pub pass_rate: f32,
    pub p50_latency_ms: u64,
    pub p95_latency_ms: u64,
    pub p50_first_turn_latency_ms: u64,
    pub p95_first_turn_latency_ms: u64,
    pub p50_second_turn_latency_ms: u64,
    pub p95_second_turn_latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantToolUseFixtureReport {
    pub id: String,
    pub language: String,
    pub request: String,
    pub first_message: AssistantToolUseMessageReport,
    pub final_output: String,
    pub latency_ms: u64,
    pub first_turn_latency_ms: u64,
    pub second_turn_latency_ms: u64,
    pub metrics: AssistantToolUseMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantToolUseMessageReport {
    pub content: String,
    pub tool_calls: Vec<ObservedToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantToolUseMetrics {
    pub score: f32,
    pub passed: bool,
    pub expected_tool_called: bool,
    pub unexpected_tool_called: bool,
    pub correct_tool_name: bool,
    pub correct_action: bool,
    pub correct_target: bool,
    pub final_matched_expected: Vec<String>,
    pub final_forbidden_found: Vec<String>,
    pub final_word_count: usize,
    pub final_too_verbose: bool,
    pub leaked_thinking: bool,
}

#[derive(Debug, Clone)]
pub struct AssistantToolUseRunConfig {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub machine_label: Option<String>,
    pub iterations: usize,
    pub languages: Vec<String>,
}

pub fn load_manifest(path: &Path) -> Result<AssistantToolUseManifest> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let manifest: AssistantToolUseManifest = toml::from_str(&text)
        .with_context(|| format!("parse assistant tool-use fixtures from {}", path.display()))?;
    manifest.validate()?;
    Ok(manifest)
}

impl AssistantToolUseManifest {
    pub fn validate(&self) -> Result<()> {
        if self.suite_version.trim().is_empty() {
            return Err(anyhow!("assistant tool-use manifest has empty suite_version"));
        }
        if self.prompt_version.trim().is_empty() {
            return Err(anyhow!("assistant tool-use manifest has empty prompt_version"));
        }
        if self.inventory.is_empty() {
            return Err(anyhow!("assistant tool-use manifest has empty Home Assistant inventory"));
        }
        if self.fixtures.is_empty() {
            return Err(anyhow!("assistant tool-use manifest has no fixtures"));
        }
        let mut ids = Vec::with_capacity(self.fixtures.len());
        for fx in &self.fixtures {
            if fx.id.trim().is_empty() {
                return Err(anyhow!("assistant tool-use fixture with empty id"));
            }
            if fx.language.trim().is_empty() {
                return Err(anyhow!("{}: empty language", fx.id));
            }
            if fx.request.trim().is_empty() {
                return Err(anyhow!("{}: empty request", fx.id));
            }
            if !fx.expect_no_tool && fx.expected_action.is_none() {
                return Err(anyhow!("{}: tool fixtures must set expected_action", fx.id));
            }
            ids.push(fx.id.as_str());
        }
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        if ids.len() != before {
            return Err(anyhow!("duplicate assistant tool-use fixture id"));
        }
        Ok(())
    }
}

pub async fn run_assistant_tool_use(
    manifest_path: &Path,
    manifest: &AssistantToolUseManifest,
    cfg: AssistantToolUseRunConfig,
) -> Result<AssistantToolUseReport> {
    let fixture_sha256 = sha256_file(manifest_path)?;
    let wanted_langs: Vec<String> = cfg.languages.iter().map(|s| s.to_ascii_lowercase()).collect();
    let fixtures: Vec<&AssistantToolUseFixture> = manifest
        .fixtures
        .iter()
        .filter(|f| wanted_langs.is_empty() || wanted_langs.iter().any(|l| l == &f.language))
        .collect();
    if fixtures.is_empty() {
        return Err(anyhow!(
            "no assistant tool-use fixtures matched languages {:?}",
            cfg.languages
        ));
    }

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(45))
        .build()?;
    let mut by_fixture = Vec::with_capacity(fixtures.len() * cfg.iterations.max(1));
    for fx in fixtures {
        for _ in 0..cfg.iterations.max(1) {
            let started = Instant::now();
            let first_started = Instant::now();
            let first = if cfg.provider == "fake" {
                fake_first_message(fx)
            } else {
                call_first_turn(&client, manifest, fx, &cfg).await?
            };
            let first_turn_latency_ms = first_started.elapsed().as_millis() as u64;
            let second_started = Instant::now();
            let final_output = if fx.expect_no_tool || first.tool_calls.is_empty() {
                first.content.clone()
            } else if cfg.provider == "fake" {
                fake_final_message(fx)
            } else {
                call_second_turn(&client, manifest, fx, &cfg, &first).await?
            };
            let second_turn_latency_ms = second_started.elapsed().as_millis() as u64;
            let latency_ms = started.elapsed().as_millis() as u64;
            let metrics = score_fixture(fx, &first, &final_output);
            by_fixture.push(AssistantToolUseFixtureReport {
                id: fx.id.clone(),
                language: fx.language.clone(),
                request: fx.request.clone(),
                first_message: first,
                final_output: final_output.trim().to_string(),
                latency_ms,
                first_turn_latency_ms,
                second_turn_latency_ms,
                metrics,
            });
        }
    }

    let by_language = aggregate_by_language(&by_fixture);
    Ok(AssistantToolUseReport {
        schema_version: REPORT_SCHEMA_VERSION.to_string(),
        suite_version: manifest.suite_version.clone(),
        prompt_version: manifest.prompt_version.clone(),
        fixture_sha256,
        provider: cfg.provider,
        model: cfg.model,
        endpoint: cfg.endpoint,
        machine_label: cfg.machine_label,
        ran_at: now_rfc3339(),
        iterations: cfg.iterations.max(1),
        by_language,
        by_fixture,
    })
}

async fn call_first_turn(
    client: &reqwest::Client,
    manifest: &AssistantToolUseManifest,
    fx: &AssistantToolUseFixture,
    cfg: &AssistantToolUseRunConfig,
) -> Result<AssistantToolUseMessageReport> {
    let req = serde_json::json!({
        "model": cfg.model,
        "messages": [
            { "role": "system", "content": system_prompt(&fx.language, &manifest.inventory) },
            { "role": "user", "content": fx.request }
        ],
        "tools": [homeassistant_light_tool()],
        "tool_choice": "auto",
        "temperature": 0.0,
        "max_completion_tokens": 96,
        "stream": false,
        "think": false,
        "chat_template_kwargs": { "enable_thinking": false }
    });
    let value = post_chat(client, cfg, req).await?;
    parse_message_report(&value)
}

async fn call_second_turn(
    client: &reqwest::Client,
    manifest: &AssistantToolUseManifest,
    fx: &AssistantToolUseFixture,
    cfg: &AssistantToolUseRunConfig,
    first: &AssistantToolUseMessageReport,
) -> Result<String> {
    let call = first
        .tool_calls
        .first()
        .ok_or_else(|| anyhow!("{}: second turn requested without tool call", fx.id))?;
    let req = serde_json::json!({
        "model": cfg.model,
        "messages": [
            { "role": "system", "content": system_prompt(&fx.language, &manifest.inventory) },
            { "role": "user", "content": fx.request },
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": call.arguments.to_string()
                    }
                }]
            },
            { "role": "tool", "tool_call_id": call.id, "content": fx.tool_result }
        ],
        "temperature": 0.0,
        "max_completion_tokens": 48,
        "stream": false,
        "think": false,
        "chat_template_kwargs": { "enable_thinking": false }
    });
    let value = post_chat(client, cfg, req).await?;
    Ok(value["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string())
}

async fn post_chat(
    client: &reqwest::Client,
    cfg: &AssistantToolUseRunConfig,
    req: serde_json::Value,
) -> Result<serde_json::Value> {
    let mut builder = client.post(&cfg.endpoint).json(&req);
    if let Some(key) = cfg.api_key.as_deref().filter(|s| !s.is_empty()) {
        builder = builder.bearer_auth(key);
    }
    let response = builder.send().await.context("assistant tool-use chat POST failed")?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("assistant tool-use chat returned {status}: {text}"));
    }
    serde_json::from_str(&text).context("parse assistant tool-use chat response")
}

fn parse_message_report(value: &serde_json::Value) -> Result<AssistantToolUseMessageReport> {
    let message = &value["choices"][0]["message"];
    let content = message["content"].as_str().unwrap_or_default().to_string();
    let tool_calls = message["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|call| {
            let id = call["id"].as_str().unwrap_or_default().to_string();
            let function = &call["function"];
            let name = function["name"].as_str().unwrap_or_default().to_string();
            let raw_args = function["arguments"].as_str().unwrap_or("{}");
            let arguments = serde_json::from_str(raw_args)
                .unwrap_or_else(|_| serde_json::json!({ "_parse_error": true, "raw": raw_args }));
            ObservedToolCall { id, name, arguments }
        })
        .collect();
    Ok(AssistantToolUseMessageReport { content, tool_calls })
}

pub fn score_fixture(
    fx: &AssistantToolUseFixture,
    first: &AssistantToolUseMessageReport,
    final_output: &str,
) -> AssistantToolUseMetrics {
    let tool_called = !first.tool_calls.is_empty();
    let unexpected_tool_called = fx.expect_no_tool && tool_called;
    let expected_tool_called = if fx.expect_no_tool { !tool_called } else { tool_called };
    let call = first.tool_calls.first();
    let correct_tool_name = fx.expect_no_tool
        || call.is_some_and(|c| {
            c.name == "homeassistant_light" || c.name == "homeassistant_light_control"
        });
    let correct_action = fx.expect_no_tool
        || call.is_some_and(|c| {
            fx.expected_action.as_ref().is_some_and(|expected| {
                argument_strings(&c.arguments).iter().any(|s| s == expected)
            })
        });
    let correct_target = fx.expect_no_tool
        || call.is_some_and(|c| {
            let values = argument_strings(&c.arguments);
            let entity_ok = fx.expected_entity_ids.is_empty()
                || fx
                    .expected_entity_ids
                    .iter()
                    .any(|expected| values.iter().any(|v| v == expected));
            let area_ok = fx
                .expected_area
                .as_ref()
                .is_none_or(|expected| values.iter().any(|v| v == &normalize(expected)));
            entity_ok && area_ok
        });

    let normalized_final = normalize(final_output);
    let final_matched_expected = fx
        .expected_final
        .iter()
        .filter(|s| normalized_final.contains(&normalize(s)))
        .cloned()
        .collect::<Vec<_>>();
    let final_forbidden_found = fx
        .forbidden_final
        .iter()
        .filter(|s| normalized_final.contains(&normalize(s)))
        .cloned()
        .collect::<Vec<_>>();
    let final_word_count = final_output.split_whitespace().count();
    let final_too_verbose = final_word_count > fx.max_words;
    let leaked_thinking = looks_like_thinking_leak(&format!("{}\n{final_output}", first.content));

    let final_ok = fx.expected_final.is_empty() || !final_matched_expected.is_empty();
    let mut penalties = 0.0_f32;
    if !expected_tool_called {
        penalties += 0.35;
    }
    if unexpected_tool_called {
        penalties += 0.55;
    }
    if !correct_tool_name {
        penalties += 0.20;
    }
    if !correct_action {
        penalties += 0.25;
    }
    if !correct_target {
        penalties += 0.25;
    }
    if !final_ok {
        penalties += 0.15;
    }
    if !final_forbidden_found.is_empty() {
        penalties += 0.20;
    }
    if final_too_verbose {
        penalties += 0.10;
    }
    if leaked_thinking {
        penalties += 0.20;
    }
    let score = (1.0 - penalties).clamp(0.0, 1.0);
    let passed = expected_tool_called
        && !unexpected_tool_called
        && correct_tool_name
        && correct_action
        && correct_target
        && final_ok
        && final_forbidden_found.is_empty()
        && !final_too_verbose
        && !leaked_thinking;

    AssistantToolUseMetrics {
        score,
        passed,
        expected_tool_called,
        unexpected_tool_called,
        correct_tool_name,
        correct_action,
        correct_target,
        final_matched_expected,
        final_forbidden_found,
        final_word_count,
        final_too_verbose,
        leaked_thinking,
    }
}

fn system_prompt(language: &str, inventory: &[HomeAssistantEntity]) -> String {
    let intro = match language {
        "ro" => {
            "Fono voice assistant. Folosește unealta numai pentru comenzi clare de aprindere/stingere lumini. Dacă lipsește camera/lampa, întreabă scurt; nu apela unealta. Confirmă scurt după rezultat."
        }
        _ => {
            "Fono voice assistant. Use the tool only for clear light on/off commands. If the room/light is missing, ask briefly; do not call the tool. Confirm briefly after the result."
        }
    };
    let mut s = String::from(intro);
    s.push_str("\nLights: ");
    for (index, entity) in inventory.iter().enumerate() {
        if index > 0 {
            s.push_str("; ");
        }
        s.push_str(&format!("{}({},{})", entity.entity_id, entity.name, entity.area));
    }
    s
}

fn homeassistant_light_tool() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "homeassistant_light",
            "description": "Control one listed light for explicit on/off requests.",
            "parameters": {
                "type": "object",
                "required": ["action"],
                "properties": {
                    "action": { "type": "string", "enum": ["turn_on", "turn_off"] },
                    "entity_id": { "type": "string" },
                    "area": { "type": "string" }
                }
            }
        }
    })
}

fn fake_first_message(fx: &AssistantToolUseFixture) -> AssistantToolUseMessageReport {
    if fx.expect_no_tool {
        return AssistantToolUseMessageReport {
            content: fx.expected_final.first().cloned().unwrap_or_else(|| "Which light?".into()),
            tool_calls: Vec::new(),
        };
    }
    let mut args = serde_json::json!({ "action": fx.expected_action.clone().unwrap_or_default() });
    if let Some(entity_id) = fx.expected_entity_ids.first() {
        args["entity_id"] = serde_json::Value::String(entity_id.clone());
    }
    if let Some(area) = fx.expected_area.as_ref() {
        args["area"] = serde_json::Value::String(area.clone());
    }
    AssistantToolUseMessageReport {
        content: String::new(),
        tool_calls: vec![ObservedToolCall {
            id: "call_fake_1".into(),
            name: "homeassistant_light".into(),
            arguments: args,
        }],
    }
}

fn fake_final_message(fx: &AssistantToolUseFixture) -> String {
    fx.expected_final.first().cloned().unwrap_or_else(|| fx.tool_result.clone())
}

fn argument_strings(arguments: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_argument_strings(arguments, &mut out);
    out
}

fn collect_argument_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(normalize(s)),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_argument_strings(value, out);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_argument_strings(value, out);
            }
        }
        _ => {}
    }
}

fn normalize(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn looks_like_thinking_leak(output: &str) -> bool {
    let lower = output.to_lowercase();
    lower.contains("<think")
        || lower.contains("</think")
        || lower.contains("let me think")
        || lower.contains("reasoning:")
        || lower.contains("chain of thought")
}

fn aggregate_by_language(
    rows: &[AssistantToolUseFixtureReport],
) -> BTreeMap<String, AssistantToolUseLangReport> {
    let mut grouped: BTreeMap<String, Vec<&AssistantToolUseFixtureReport>> = BTreeMap::new();
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
            let mut first_turn_latencies =
                rows.iter().map(|r| r.first_turn_latency_ms).collect::<Vec<_>>();
            first_turn_latencies.sort_unstable();
            let mut second_turn_latencies =
                rows.iter().map(|r| r.second_turn_latency_ms).collect::<Vec<_>>();
            second_turn_latencies.sort_unstable();
            let report = AssistantToolUseLangReport {
                n,
                mean_score,
                pass_rate,
                p50_latency_ms: percentile(&latencies, 50),
                p95_latency_ms: percentile(&latencies, 95),
                p50_first_turn_latency_ms: percentile(&first_turn_latencies, 50),
                p95_first_turn_latency_ms: percentile(&first_turn_latencies, 95),
                p50_second_turn_latency_ms: percentile(&second_turn_latencies, 50),
                p95_second_turn_latency_ms: percentile(&second_turn_latencies, 95),
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

fn default_tool_result() -> String {
    "success".to_string()
}

fn default_max_words() -> usize {
    16
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
        assert!(manifest.fixtures.iter().any(|f| f.expect_no_tool));
        assert!(manifest.fixtures.iter().any(|f| !f.expect_no_tool));
    }

    #[test]
    fn scoring_accepts_expected_tool_call() {
        let fx = AssistantToolUseFixture {
            id: "x".into(),
            language: "en".into(),
            request: "turn on kitchen".into(),
            expect_no_tool: false,
            expected_action: Some("turn_on".into()),
            expected_entity_ids: vec!["light.kitchen_ceiling".into()],
            expected_area: None,
            tool_result: "success".into(),
            expected_final: vec!["kitchen".into()],
            forbidden_final: Vec::new(),
            max_words: 8,
        };
        let first = AssistantToolUseMessageReport {
            content: String::new(),
            tool_calls: vec![ObservedToolCall {
                id: "call_1".into(),
                name: "homeassistant_light".into(),
                arguments: serde_json::json!({
                    "action": "turn_on",
                    "entity_id": "light.kitchen_ceiling"
                }),
            }],
        };
        let metrics = score_fixture(&fx, &first, "Kitchen light is on.");
        assert!(metrics.passed);
        assert_eq!(metrics.score, 1.0);
    }

    #[test]
    fn scoring_rejects_tool_for_ambiguous_request() {
        let fx = AssistantToolUseFixture {
            id: "x".into(),
            language: "en".into(),
            request: "turn on the light".into(),
            expect_no_tool: true,
            expected_action: None,
            expected_entity_ids: Vec::new(),
            expected_area: None,
            tool_result: "success".into(),
            expected_final: vec!["which light".into()],
            forbidden_final: Vec::new(),
            max_words: 10,
        };
        let first = AssistantToolUseMessageReport {
            content: String::new(),
            tool_calls: vec![ObservedToolCall {
                id: "call_1".into(),
                name: "homeassistant_light".into(),
                arguments: serde_json::json!({ "action": "turn_on" }),
            }],
        };
        let metrics = score_fixture(&fx, &first, "Done.");
        assert!(!metrics.passed);
        assert!(metrics.unexpected_tool_called);
    }
}
