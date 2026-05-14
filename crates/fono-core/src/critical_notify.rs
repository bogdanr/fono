// SPDX-License-Identifier: GPL-3.0-only
//! Session-scoped desktop notifications for critical pipeline failures
//! (STT/LLM/TTS/Assistant/Inject auth errors, network outages, total
//! pipeline collapse).
//!
//! Cascade rule (issue #8): **at most one** critical notification per
//! dictation session, no matter how many downstream stages cascade-fail
//! off the same root cause. Example: when the user's API key expires,
//! the STT stage 401s → notification fires → the LLM cleanup stage
//! also 401s and the assistant-mode TTS would 401 too, but those are
//! swallowed silently until [`reset_session_flag`] is called at the
//! start of the next dictation. This is intentional: one popup with a
//! clear remediation is signal; three popups for the same root cause
//! are noise.
//!
//! The classifier still tracks per-`(stage, provider, class)` dedup as
//! a secondary defence (a single backend that flaps repeatedly inside
//! one session also gets one popup), but the global session gate is
//! the authoritative cap.
//!
//! Only invoked from the **daemon** orchestrator (`spawn_pipeline`,
//! `run_assistant_turn`). The `fono record` CLI prints to stderr
//! instead and must not call this module: a one-shot terminal command
//! should not pop a desktop notification on top of its own output.
//!
//! Platform fallback is delegated to [`crate::notify::send`]: on Linux
//! it shells out to `notify-send` and warns on absence; on macOS /
//! Windows it routes through `notify-rust`. See `crate::notify` for
//! the full contract.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::notify::{self, Urgency};

/// Auto-reset window: if no [`reset_session_flag`] call arrives within
/// this duration of the most recent notification, the dedup set is
/// cleared. Covers the panic-skips-reset edge case so a long-running
/// daemon eventually re-arms.
const AUTO_RESET_AFTER: Duration = Duration::from_secs(120);

/// Pipeline stage that produced the error. Used as part of the dedup
/// key. Marked `#[non_exhaustive]` so adding new stages does not break
/// downstream matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Stage {
    /// Speech-to-text (cloud or local backend).
    Stt,
    /// LLM cleanup of dictation transcripts.
    Llm,
    /// Text-to-speech (assistant-mode reply playback).
    Tts,
    /// Conversational assistant chat backend.
    Assistant,
    /// Text injection / typing into the focused window.
    Inject,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stt => "stt",
            Self::Llm => "llm",
            Self::Tts => "tts",
            Self::Assistant => "assistant",
            Self::Inject => "inject",
        }
    }
}

/// Human-readable label used in notification summaries. Distinct from
/// the lowercase `as_str` ids that go into log lines + dedup keys.
fn stage_user_label(stage: Stage) -> &'static str {
    match stage {
        Stage::Stt => "STT",
        Stage::Llm => "LLM",
        Stage::Tts => "TTS",
        Stage::Assistant => "Assistant",
        Stage::Inject => "Injection",
    }
}

/// Classified error category. Drives both the dedup key and the
/// user-facing notification copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorClass {
    /// HTTP 401 / 403 — user must update their API key.
    Auth,
    /// HTTP 429 — already surfaced by `fono_stt::rate_limit_notify`;
    /// the critical-notify path is a no-op for this class.
    RateLimit,
    /// reqwest connect/timeout/dns failure.
    Network,
    /// Provider requires the org admin to accept model-specific terms
    /// before the model can be invoked (e.g. Groq's `model_terms_required`
    /// on Orpheus / PlayAI). Surfaced as a one-shot notification with
    /// the acceptance URL embedded in the body.
    TermsRequired,
    /// Required API key is not present in `secrets.toml` or the
    /// process environment. Triggered when a backend build fails at
    /// daemon startup or after a tray-driven reload because the user
    /// switched to a backend whose key was never added. The render
    /// path extracts the env var name from the error and suggests
    /// `fono keys add <VAR>`.
    MissingKey,
    /// Anything else (5xx, parse errors, clarification refusal, …).
    Other,
}

/// Classify a pipeline error message by scanning for well-known
/// substrings. The error strings produced by the cloud STT/LLM
/// backends follow stable shapes; see the unit tests for the exact
/// patterns we anchor on.
///
/// Order matters: rate-limit detection runs before generic auth so a
/// 429 body that mentions "Unauthorized" elsewhere doesn't misroute.
#[must_use]
pub fn classify(err_msg: &str) -> ErrorClass {
    // Rate limit first — the 429 body sometimes contains the word
    // "Unauthorized" in upgrade-pitch copy.
    if contains_status(err_msg, 429) {
        return ErrorClass::RateLimit;
    }
    if contains_status(err_msg, 401) || contains_status(err_msg, 403) {
        return ErrorClass::Auth;
    }
    // Anthropic surfaces auth failures as `authentication_error` in
    // the typed body; OpenAI's `expired_api_key` / `invalid_api_key`
    // codes are caught by the 401 branch above but the typed codes
    // are also a strong signal when status got stripped.
    let lower = err_msg.to_ascii_lowercase();
    // Groq returns 400 + `model_terms_required` when an org admin
    // hasn't accepted a model's terms. Detect this before the generic
    // Other bucket so the user gets actionable copy with the URL.
    if lower.contains("model_terms_required") || lower.contains("requires terms acceptance") {
        return ErrorClass::TermsRequired;
    }
    // Backend build failed because the API key is missing. Both the
    // STT and TTS factories produce strings like:
    //   `<provider> TTS API key "<VAR>" not found in secrets.toml or environment;
    //    run `fono keys add <VAR>` to add it`
    // Detect either the canonical `fono keys add` hint or the bare
    // `not found in secrets.toml` phrase so we keep firing if the
    // factories ever reword the suffix.
    if lower.contains("not found in secrets.toml") || lower.contains("fono keys add") {
        return ErrorClass::MissingKey;
    }
    if lower.contains("invalid_api_key")
        || lower.contains("expired_api_key")
        || lower.contains("authentication_error")
        || lower.contains("invalid api key")
    {
        return ErrorClass::Auth;
    }
    // Common reqwest transport failures.
    if lower.contains("connection refused")
        || lower.contains("dns error")
        || lower.contains("timed out")
        || lower.contains("connect timeout")
        || lower.contains("error sending request")
    {
        return ErrorClass::Network;
    }
    ErrorClass::Other
}

/// Pull the first `<VAR>_API_KEY`-shaped env var name out of an error
/// message. Returns `None` if no UPPER_SNAKE_CASE token containing
/// `API_KEY` / `_KEY` / `_TOKEN` is present.
fn extract_env_var(msg: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut current = String::new();
    for c in msg.chars() {
        if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' {
            current.push(c);
        } else if current.len() >= 6
            && (current.contains("API_KEY")
                || current.contains("_TOKEN")
                || current.ends_with("_KEY"))
            && best.is_none()
        {
            best = Some(std::mem::take(&mut current));
        } else {
            current.clear();
        }
    }
    if best.is_none()
        && current.len() >= 6
        && (current.contains("API_KEY") || current.contains("_TOKEN") || current.ends_with("_KEY"))
    {
        best = Some(current);
    }
    best
}

/// Pull the first `https?://…` URL out of an error message, stripping
/// trailing punctuation that commonly clings to URLs in JSON bodies.
fn extract_url(msg: &str) -> Option<String> {
    let start = msg.find("http")?;
    let tail = &msg[start..];
    let end = tail
        .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | '>' | '`'))
        .unwrap_or(tail.len());
    let mut url = tail[..end].to_string();
    while matches!(url.chars().last(), Some('.' | ',' | ';' | ':' | '!' | '?')) {
        url.pop();
    }
    if url.len() < "http://a".len() {
        return None;
    }
    Some(url)
}

/// Match a bare HTTP status with whitespace boundaries so we don't
/// false-positive on the digits appearing inside a longer number
/// (e.g. an error code `4011`).
fn contains_status(msg: &str, status: u16) -> bool {
    let needle = format!(" {status} ");
    if msg.contains(&needle) {
        return true;
    }
    // Some upstream errors render as `... 401:` (colon-suffixed).
    let alt = format!(" {status}:");
    msg.contains(&alt)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DedupKey {
    stage: Stage,
    provider: &'static str,
    class: ErrorClass,
}

static FIRED: Mutex<Option<HashSet<DedupKey>>> = Mutex::new(None);
static LAST_FIRED_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// Global single-shot gate: once *any* critical notification has fired
/// during the current session, no further critical notifications fire
/// until [`reset_session_flag`] is called. This is the authoritative
/// cascade cap (see module docs).
static SESSION_HAS_FIRED: Mutex<bool> = Mutex::new(false);

/// In-memory recorder used only by tests + the dedup-test hook. Stays
/// empty in production builds.
#[cfg(test)]
static TEST_RECORDER: Mutex<Vec<(Stage, &'static str, ErrorClass, String)>> =
    Mutex::new(Vec::new());

/// Clear the per-session dedup set **and** the global single-shot
/// gate. Call from every entry point that starts a new dictation
/// session so each F8/F9/F10 press gets one fresh notification
/// opportunity.
pub fn reset_session_flag() {
    if let Ok(mut g) = FIRED.lock() {
        *g = None;
    }
    if let Ok(mut g) = LAST_FIRED_AT.lock() {
        *g = None;
    }
    if let Ok(mut g) = SESSION_HAS_FIRED.lock() {
        *g = false;
    }
}

/// Surface a critical pipeline failure as a desktop notification.
///
/// Dedup is per `(stage, provider, class)`: within one session,
/// repeated failures of the same shape fire exactly once. After
/// [`AUTO_RESET_AFTER`] of inactivity the dedup set is cleared
/// defensively. Returns `true` when a notification was fired.
///
/// `provider` should be a `'static` backend name (`"groq"`,
/// `"openai"`, …) so the dedup key stays cheap; the `details` string
/// is the human-readable error summary used as the notification body.
pub fn notify(stage: Stage, provider: &'static str, class: ErrorClass, details: &str) -> bool {
    // RateLimit is already handled by `fono_stt::rate_limit_notify`;
    // bail early so we don't double-fire.
    if matches!(class, ErrorClass::RateLimit) {
        tracing::debug!(
            "critical_notify: skip RateLimit (handled by rate_limit_notify) \
             stage={} provider={provider}",
            stage.as_str(),
        );
        return false;
    }

    // Auto-reset stale dedup set (and the global gate). A long-idle
    // daemon eventually re-arms even if the recording-start reset path
    // was skipped (e.g. a panic in the FSM).
    if let Ok(mut g) = LAST_FIRED_AT.lock() {
        if let Some(at) = *g {
            if at.elapsed() >= AUTO_RESET_AFTER {
                if let Ok(mut fired) = FIRED.lock() {
                    *fired = None;
                }
                if let Ok(mut sess) = SESSION_HAS_FIRED.lock() {
                    *sess = false;
                }
                *g = None;
            }
        }
    }

    // Cascade cap: a single root cause (e.g. a rotated API key) will
    // make STT auth-fail, then LLM cleanup auth-fail, then assistant
    // chat auth-fail, then assistant TTS auth-fail — all inside the
    // same dictation session. The user only needs to see *one*
    // notification; subsequent failures stay in the journal.
    if let Ok(g) = SESSION_HAS_FIRED.lock() {
        if *g {
            tracing::debug!(
                "critical_notify: suppressed (session cap reached) \
                 stage={} provider={provider} class={class:?}",
                stage.as_str(),
            );
            return false;
        }
    }

    let key = DedupKey {
        stage,
        provider,
        class,
    };
    let already = {
        let Ok(mut g) = FIRED.lock() else {
            return false;
        };
        let set = g.get_or_insert_with(HashSet::new);
        !set.insert(key)
    };
    if already {
        tracing::debug!(
            "critical_notify: suppressed (already fired this session) \
             stage={} provider={provider} class={class:?}",
            stage.as_str(),
        );
        return false;
    }

    if let Ok(mut g) = LAST_FIRED_AT.lock() {
        *g = Some(Instant::now());
    }
    if let Ok(mut g) = SESSION_HAS_FIRED.lock() {
        *g = true;
    }

    fire(stage, provider, class, details);
    true
}

/// Convenience wrapper used by daemon reload sites: classify the error
/// and only fire when it's a user-actionable class (Auth, Network,
/// TermsRequired, MissingKey). Returns the classified class so the
/// caller can decide what else to log.
///
/// Useful at backend-build failure sites that swallow the error and
/// fall back to a degraded state — the desktop notification is the
/// only feedback the user gets that their tray click did something
/// but the daemon couldn't honour it.
pub fn notify_actionable(
    stage: Stage,
    provider: &'static str,
    details: &str,
) -> (ErrorClass, bool) {
    let class = classify(details);
    let fired = matches!(
        class,
        ErrorClass::Auth | ErrorClass::Network | ErrorClass::TermsRequired | ErrorClass::MissingKey
    ) && notify(stage, provider, class, details);
    (class, fired)
}

fn fire(stage: Stage, provider: &'static str, class: ErrorClass, details: &str) {
    let (summary, body) = render(stage, provider, class, details);
    #[cfg(test)]
    {
        if let Ok(mut rec) = TEST_RECORDER.lock() {
            rec.push((stage, provider, class, body.clone()));
        }
    }
    notify::send(&summary, &body, "dialog-error", 10_000, Urgency::Critical);
}

#[allow(clippy::too_many_lines)]
fn render(
    stage: Stage,
    provider: &'static str,
    class: ErrorClass,
    details: &str,
) -> (String, String) {
    match (stage, class) {
        (Stage::Stt, ErrorClass::Auth) => (
            format!("Fono — STT key rejected ({provider})"),
            format!(
                "{provider} rejected the API key (401/403). Dictation failed; no text \
                 was injected. Open the tray → Configure → STT to update the key, or \
                 run `fono doctor`."
            ),
        ),
        (Stage::Stt, ErrorClass::Network) => (
            format!("Fono — STT unreachable ({provider})"),
            format!(
                "Could not reach {provider}: {details}. Check your network or fall back \
                 to a local STT backend in the tray."
            ),
        ),
        (Stage::Stt, ErrorClass::Other | ErrorClass::RateLimit) => (
            format!("Fono — STT failed ({provider})"),
            format!(
                "Speech-to-text failed: {details}. Run `fono doctor` or check the \
                 journal for the full error."
            ),
        ),
        (Stage::Llm, ErrorClass::Auth) => (
            format!("Fono — LLM key rejected ({provider})"),
            format!(
                "{provider} rejected the API key (401/403). The raw transcript was \
                 injected without cleanup. Update the LLM key in the tray, or disable \
                 LLM cleanup."
            ),
        ),
        (Stage::Llm, ErrorClass::Network) => (
            format!("Fono — LLM unreachable ({provider})"),
            format!(
                "Could not reach {provider}: {details}. Raw transcript was injected \
                 without cleanup."
            ),
        ),
        (Stage::Llm, ErrorClass::Other | ErrorClass::RateLimit) => (
            format!("Fono — LLM cleanup failed ({provider})"),
            format!(
                "LLM cleanup failed: {details}. Raw transcript was injected without \
                 cleanup."
            ),
        ),
        (Stage::Tts, ErrorClass::Auth) => (
            format!("Fono — TTS key rejected ({provider})"),
            format!(
                "{provider} rejected the API key (401/403). The assistant reply was \
                 generated but could not be spoken. Update the TTS key in the tray, \
                 or switch to a local TTS backend."
            ),
        ),
        (Stage::Tts, ErrorClass::Network) => (
            format!("Fono — TTS unreachable ({provider})"),
            format!(
                "Could not reach {provider}: {details}. The assistant reply could not \
                 be spoken; switch to a local TTS backend or check your network."
            ),
        ),
        (Stage::Tts, ErrorClass::TermsRequired) => (
            format!("Fono — TTS model requires terms acceptance ({provider})"),
            {
                let url = extract_url(details);
                let suffix = url.map_or_else(
                    || {
                        "Open the provider console to accept the model terms, then retry."
                            .to_string()
                    },
                    |u| format!("Accept the model terms at {u}, then retry."),
                );
                format!(
                    "{provider} refused the request because the model's terms have \
                     not been accepted by the org admin. The assistant reply was \
                     generated but could not be spoken. {suffix}"
                )
            },
        ),
        (Stage::Tts, ErrorClass::Other | ErrorClass::RateLimit) => (
            format!("Fono — TTS failed ({provider})"),
            format!(
                "Text-to-speech failed: {details}. The assistant reply was generated \
                 but could not be spoken."
            ),
        ),
        (Stage::Assistant, ErrorClass::Auth) => (
            format!("Fono — Assistant key rejected ({provider})"),
            format!(
                "{provider} rejected the API key (401/403). The assistant turn was \
                 aborted. Update the assistant key in the tray → Configure, or run \
                 `fono doctor`."
            ),
        ),
        (Stage::Assistant, ErrorClass::Network) => (
            format!("Fono — Assistant unreachable ({provider})"),
            format!(
                "Could not reach {provider}: {details}. The assistant turn was \
                 aborted; check your network or try again."
            ),
        ),
        (Stage::Assistant, ErrorClass::Other | ErrorClass::RateLimit) => (
            format!("Fono — Assistant failed ({provider})"),
            format!(
                "Assistant turn failed: {details}. Check the journal or run \
                 `fono doctor` for details."
            ),
        ),
        (_, ErrorClass::TermsRequired) => (
            format!("Fono — model requires terms acceptance ({provider})"),
            {
                let url = extract_url(details);
                let suffix = url.map_or_else(
                    || {
                        "Open the provider console to accept the model terms, then retry."
                            .to_string()
                    },
                    |u| format!("Accept the model terms at {u}, then retry."),
                );
                format!(
                    "{provider} refused the request because the model's terms have \
                     not been accepted by the org admin. {suffix}"
                )
            },
        ),
        (stage, ErrorClass::MissingKey) => (
            format!(
                "Fono — {} key missing ({provider})",
                stage_user_label(stage)
            ),
            {
                let var = extract_env_var(details);
                let hint = var.map_or_else(
                    || {
                        format!(
                            "{provider}'s API key is not configured. Open the tray → Configure → \
                         {} to add it, or run `fono keys add <VAR>`.",
                            stage_user_label(stage),
                        )
                    },
                    |v| {
                        format!(
                            "No `{v}` in secrets.toml or environment. Run `fono keys add {v}` \
                         (or open the tray → Configure) to add it."
                        )
                    },
                );
                let consequence = match stage {
                    Stage::Tts => " The assistant reply was generated but could not be spoken.",
                    Stage::Llm => " The raw transcript was injected without cleanup.",
                    Stage::Assistant => " The assistant turn was aborted.",
                    Stage::Stt => " Dictation was skipped; no text was injected.",
                    Stage::Inject => "",
                };
                format!("{hint}{consequence}")
            },
        ),
        (Stage::Inject, _) => (
            format!("Fono — text injection failed ({provider})"),
            format!(
                "Could not type the dictated text via {provider}: {details}. The \
                 cleaned text is on the clipboard — press Ctrl+V to paste, or \
                 install a key-injection backend (wtype/ydotool on Wayland, \
                 xdotool on X11)."
            ),
        ),
    }
}

/// Test-only: drain the in-memory recorder. Returns every `(stage,
/// provider, class, body)` tuple recorded since the last drain.
#[cfg(test)]
#[doc(hidden)]
pub fn drain_test_recorder() -> Vec<(Stage, &'static str, ErrorClass, String)> {
    TEST_RECORDER
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shared global state — tests must run serially.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn fresh() -> std::sync::MutexGuard<'static, ()> {
        let g = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_session_flag();
        let _ = drain_test_recorder();
        g
    }

    #[test]
    fn classify_groq_stt_401() {
        // Verbatim error from the user's bug report.
        let msg = r#"STT groq: groq STT 401 Unauthorized: {"error":{"message":"Invalid API Key","type":"invalid_request_error","code":"expired_api_key"}}"#;
        assert_eq!(classify(msg), ErrorClass::Auth);
    }

    #[test]
    fn classify_groq_429_is_rate_limit_not_auth() {
        // 429 body sometimes mentions "Unauthorized" elsewhere; rate
        // limit must win.
        let msg = "groq STT 429 Too Many Requests: {\"error\":{\"message\":\"Rate limit\"}}";
        assert_eq!(classify(msg), ErrorClass::RateLimit);
    }

    #[test]
    fn classify_openai_compat_llm_401() {
        let msg = "groq LLM 401 Unauthorized: {\"error\":{\"message\":\"Invalid API Key\"}}";
        assert_eq!(classify(msg), ErrorClass::Auth);
    }

    #[test]
    fn classify_anthropic_401() {
        let msg = "anthropic LLM 401 Unauthorized: {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"}}";
        assert_eq!(classify(msg), ErrorClass::Auth);
    }

    #[test]
    fn classify_403_forbidden() {
        let msg = "openai LLM 403 Forbidden: {\"error\":{\"message\":\"region blocked\"}}";
        assert_eq!(classify(msg), ErrorClass::Auth);
    }

    #[test]
    fn classify_network_timeout() {
        let msg = "chat POST failed: error sending request for url (https://api.groq.com/…): operation timed out";
        assert_eq!(classify(msg), ErrorClass::Network);
    }

    #[test]
    fn classify_unknown_is_other() {
        let msg = "groq STT 500 Internal Server Error: upstream blew up";
        assert_eq!(classify(msg), ErrorClass::Other);
    }

    #[test]
    fn classify_does_not_match_substring_digits() {
        // The bare digits 401 appearing inside an error code must not
        // trip the auth classifier — boundaries matter.
        let msg = "weird error code 4011 happened";
        assert_eq!(classify(msg), ErrorClass::Other);
    }

    #[test]
    fn first_notify_fires_then_dedups() {
        let _g = fresh();
        assert!(notify(Stage::Stt, "groq", ErrorClass::Auth, "401"));
        assert!(!notify(Stage::Stt, "groq", ErrorClass::Auth, "401 again"));
        assert!(!notify(
            Stage::Stt,
            "groq",
            ErrorClass::Auth,
            "401 once more"
        ));
        let recorded = drain_test_recorder();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, Stage::Stt);
        assert_eq!(recorded[0].1, "groq");
    }

    #[test]
    fn cascade_cap_only_one_notification_per_session() {
        // Issue #8 cap: when a rotated API key cascade-fails through
        // STT → LLM → Assistant → TTS in the same session, the user
        // sees one popup, not four. Subsequent stages are silently
        // swallowed (still logged at debug) until reset_session_flag
        // is called at the start of the next dictation.
        let _g = fresh();
        assert!(notify(Stage::Stt, "groq", ErrorClass::Auth, "stt-401"));
        assert!(!notify(Stage::Llm, "groq", ErrorClass::Auth, "llm-401"));
        assert!(!notify(
            Stage::Assistant,
            "anthropic",
            ErrorClass::Auth,
            "assistant-401"
        ));
        assert!(!notify(Stage::Tts, "openai", ErrorClass::Auth, "tts-401"));
        assert!(!notify(
            Stage::Inject,
            "wtype",
            ErrorClass::Other,
            "inject-failed"
        ));
        let recorded = drain_test_recorder();
        assert_eq!(recorded.len(), 1, "cascade cap violated: {recorded:?}");
        assert_eq!(recorded[0].0, Stage::Stt);
    }

    #[test]
    fn cap_re_arms_after_reset() {
        // Each new dictation session must get a fresh notification
        // opportunity — the cap is per-session, not lifetime.
        let _g = fresh();
        assert!(notify(Stage::Stt, "groq", ErrorClass::Auth, "1"));
        assert!(!notify(Stage::Llm, "groq", ErrorClass::Auth, "2"));
        reset_session_flag();
        let _ = drain_test_recorder();
        // Different stage, fresh session → fires again.
        assert!(notify(Stage::Llm, "groq", ErrorClass::Auth, "3"));
        assert_eq!(drain_test_recorder().len(), 1);
    }

    #[test]
    fn stt_and_llm_auth_in_same_session_cap_to_one() {
        // Pre-cap behaviour fired twice (one per stage). With the
        // cascade cap only the first stage fires; this locks the new
        // semantics in place against regression.
        let _g = fresh();
        assert!(notify(Stage::Stt, "groq", ErrorClass::Auth, "stt-401"));
        assert!(!notify(Stage::Llm, "groq", ErrorClass::Auth, "llm-401"));
        assert_eq!(drain_test_recorder().len(), 1);
    }

    #[test]
    fn different_providers_still_cap_to_one() {
        // Even across providers, the cap holds — a user with two
        // mis-configured backends sees one popup, not two.
        let _g = fresh();
        assert!(notify(Stage::Llm, "groq", ErrorClass::Auth, "g"));
        assert!(!notify(Stage::Llm, "openai", ErrorClass::Auth, "o"));
        assert_eq!(drain_test_recorder().len(), 1);
    }

    #[test]
    fn rate_limit_class_is_no_op() {
        let _g = fresh();
        assert!(!notify(Stage::Stt, "groq", ErrorClass::RateLimit, "429"));
        assert!(drain_test_recorder().is_empty());
    }

    #[test]
    fn reset_re_arms() {
        let _g = fresh();
        assert!(notify(Stage::Stt, "groq", ErrorClass::Auth, "1"));
        assert!(!notify(Stage::Stt, "groq", ErrorClass::Auth, "2"));
        reset_session_flag();
        let _ = drain_test_recorder();
        assert!(notify(Stage::Stt, "groq", ErrorClass::Auth, "3"));
        assert_eq!(drain_test_recorder().len(), 1);
    }

    #[test]
    fn auto_reset_after_window() {
        let _g = fresh();
        assert!(notify(Stage::Stt, "groq", ErrorClass::Auth, "1"));
        assert!(!notify(Stage::Stt, "groq", ErrorClass::Auth, "2"));
        // Simulate clock advance past the auto-reset window.
        if let Ok(mut g) = LAST_FIRED_AT.lock() {
            let stale = Instant::now()
                .checked_sub(AUTO_RESET_AFTER + Duration::from_secs(1))
                .expect("clock supports the test offset");
            *g = Some(stale);
        }
        let _ = drain_test_recorder();
        assert!(notify(Stage::Stt, "groq", ErrorClass::Auth, "3"));
    }

    #[test]
    fn classify_missing_key_from_tts_factory() {
        // Verbatim error from fono_tts::build_tts when CARTESIA_API_KEY
        // isn't set and the user picks Cartesia from the tray.
        let msg = r#"Cartesia TTS API key "CARTESIA_API_KEY" not found in secrets.toml or environment; run `fono keys add CARTESIA_API_KEY` to add it"#;
        assert_eq!(classify(msg), ErrorClass::MissingKey);
    }

    #[test]
    fn extract_env_var_grabs_token_from_factory_error() {
        let msg = r#"Cartesia TTS API key "CARTESIA_API_KEY" not found in secrets.toml or environment; run `fono keys add CARTESIA_API_KEY` to add it"#;
        assert_eq!(extract_env_var(msg).as_deref(), Some("CARTESIA_API_KEY"));
    }

    #[test]
    fn extract_env_var_ignores_short_words() {
        // Bare HTTP / TOML / JSON shouldn't trip the env-var picker.
        assert_eq!(extract_env_var("got HTTP 401 TOML"), None);
    }

    #[test]
    fn missing_key_render_mentions_fono_keys_add() {
        let details = r#"Cartesia TTS API key "CARTESIA_API_KEY" not found in secrets.toml or environment; run `fono keys add CARTESIA_API_KEY` to add it"#;
        let (summary, body) = render(Stage::Tts, "cartesia", ErrorClass::MissingKey, details);
        assert!(summary.contains("TTS key missing"));
        assert!(summary.contains("cartesia"));
        assert!(body.contains("fono keys add CARTESIA_API_KEY"));
        assert!(body.contains("reply was generated but could not be spoken"));
    }

    #[test]
    fn notify_actionable_fires_for_missing_key() {
        let _g = fresh();
        let details =
            r#"Cartesia TTS API key "CARTESIA_API_KEY" not found in secrets.toml or environment"#;
        let (class, fired) = notify_actionable(Stage::Tts, "cartesia", details);
        assert_eq!(class, ErrorClass::MissingKey);
        assert!(fired);
        let recorded = drain_test_recorder();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].2, ErrorClass::MissingKey);
    }

    #[test]
    fn notify_actionable_skips_other_class() {
        // A 5xx isn't user-actionable from the reload site — it
        // should not fire a popup.
        let _g = fresh();
        let details = "groq STT 500 Internal Server Error: upstream blew up";
        let (class, fired) = notify_actionable(Stage::Tts, "groq", details);
        assert_eq!(class, ErrorClass::Other);
        assert!(!fired);
        assert!(drain_test_recorder().is_empty());
    }

    #[test]
    fn classify_groq_model_terms_required() {
        // Verbatim error body from a Groq Orpheus TTS request when
        // the org admin hasn't accepted the model terms.
        let msg = r#"groq TTS returned 400 Bad Request: {"error":{"message":"The model `canopylabs/orpheus-v1-english` requires terms acceptance. Please have the org admin accept the terms at https://console.groq.com/playground?model=canopylabs%2Forpheus-v1-english","type":"invalid_request_error","code":"model_terms_required"}}"#;
        assert_eq!(classify(msg), ErrorClass::TermsRequired);
    }

    #[test]
    fn extract_url_pulls_groq_console_link() {
        let msg = "Please have the org admin accept the terms at https://console.groq.com/playground?model=canopylabs%2Forpheus-v1-english\",\"type\":\"invalid_request_error\"";
        let url = extract_url(msg).expect("url present");
        assert_eq!(
            url,
            "https://console.groq.com/playground?model=canopylabs%2Forpheus-v1-english"
        );
    }

    #[test]
    fn terms_required_renders_with_acceptance_url() {
        let details = r#"groq TTS returned 400 Bad Request: {"error":{"message":"The model `canopylabs/orpheus-v1-english` requires terms acceptance. Please have the org admin accept the terms at https://console.groq.com/playground?model=canopylabs%2Forpheus-v1-english","code":"model_terms_required"}}"#;
        let (summary, body) = render(Stage::Tts, "groq", ErrorClass::TermsRequired, details);
        assert!(summary.contains("terms acceptance"));
        assert!(summary.contains("groq"));
        assert!(body.contains("https://console.groq.com/playground"));
    }

    #[test]
    fn terms_required_notification_fires() {
        let _g = fresh();
        let details = "groq TTS 400: model_terms_required at https://console.groq.com/x";
        assert!(notify(
            Stage::Tts,
            "groq",
            ErrorClass::TermsRequired,
            details
        ));
        let recorded = drain_test_recorder();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].2, ErrorClass::TermsRequired);
        assert!(recorded[0].3.contains("https://console.groq.com/x"));
    }

    #[test]
    fn render_includes_provider_and_details() {
        let (summary, body) = render(Stage::Stt, "groq", ErrorClass::Auth, "expired_api_key");
        assert!(summary.contains("groq"));
        assert!(summary.contains("STT"));
        assert!(body.contains("groq"));
    }
}
