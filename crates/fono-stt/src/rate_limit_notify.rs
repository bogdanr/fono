// SPDX-License-Identifier: GPL-3.0-only
//! Session-scoped 429 rate-limit notification dedup + global preview
//! lane throttle.
//!
//! When a cloud provider returns HTTP 429, we want to surface this to
//! the user as a desktop notification — but exactly **once per
//! dictation session** (per F8/F9 press), not once per request. The
//! session orchestrator calls [`reset_session_flag`] at the start of
//! every recording / live-dictation session; the cloud STT backend
//! calls [`notify_once`] from each 429 site and the static
//! `AtomicBool` ensures only the first call per session fires the
//! notification.
//!
//! A defensive 120-second auto-reset covers the edge case where a
//! panic mid-session skips the orchestrator's reset call: a long-
//! running daemon that never gets a fresh `reset_session_flag` will
//! re-arm after two minutes so future rate-limit problems remain
//! observable.
//!
//! [`mark_rate_limited`] additionally arms a 60-second window during
//! which streaming preview re-POSTs are skipped (only VAD-boundary
//! finalize requests fire). This lets the user keep dictating without
//! every preview tick churning into another 429.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static NOTIFIED_THIS_SESSION: AtomicBool = AtomicBool::new(false);
static LAST_NOTIFIED_AT: Mutex<Option<Instant>> = Mutex::new(None);
static THROTTLE_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// Auto-reset window: if no `reset_session_flag` call arrives within
/// this duration of the most recent `notify_once`, the flag re-arms.
const AUTO_RESET_AFTER: Duration = Duration::from_secs(120);

/// How long after a 429 we suppress the streaming preview lane and
/// fall back to VAD-boundary-only finalize requests. One minute lines
/// up with Groq's "Limit X per minute" cap so the next minute clears.
pub const THROTTLE_WINDOW: Duration = Duration::from_secs(60);

/// Clear the dedup flag. Call this from `SessionOrchestrator::on_start_*`
/// at every new dictation session so the user gets one fresh
/// notification opportunity per F8/F9 press.
pub fn reset_session_flag() {
    NOTIFIED_THIS_SESSION.store(false, Ordering::SeqCst);
    if let Ok(mut g) = LAST_NOTIFIED_AT.lock() {
        *g = None;
    }
}

/// Test-only: clear the throttle window. Production code should never
/// call this — the 60-second window is intentional. Used by streaming
/// tests that share the static `THROTTLE_UNTIL` with the
/// `rate_limit_notify` tests.
#[doc(hidden)]
pub fn clear_throttle_for_tests() {
    if let Ok(mut g) = THROTTLE_UNTIL.lock() {
        *g = None;
    }
}

/// Arm the preview-lane throttle for [`THROTTLE_WINDOW`]. Called from
/// the 429 sites; checked by the streaming pseudo-stream loop before
/// each preview tick.
pub fn mark_rate_limited() {
    if let Ok(mut g) = THROTTLE_UNTIL.lock() {
        let until = Instant::now() + THROTTLE_WINDOW;
        // Only extend, never shorten — repeated 429s within the window
        // keep the throttle armed.
        if g.is_none_or(|prev| until > prev) {
            *g = Some(until);
        }
    }
}

/// True if a 429 was observed within [`THROTTLE_WINDOW`] and the
/// streaming preview lane should be suppressed.
pub fn is_throttled() -> bool {
    if let Ok(g) = THROTTLE_UNTIL.lock() {
        if let Some(until) = *g {
            return Instant::now() < until;
        }
    }
    false
}

/// Surface a rate-limit notification at most once per session. The
/// first call after [`reset_session_flag`] fires the desktop
/// notification (when the `notify` feature is compiled in); subsequent
/// calls within the same session are suppressed and logged at DEBUG.
///
/// Returns `true` if the notification was fired, `false` if it was
/// suppressed by the session-scoped dedup.
///
/// Note: this function does **not** arm the preview-lane throttle.
/// Call [`mark_rate_limited`] separately at the 429 site so the
/// throttle and notification can be tested independently.
pub fn notify_once(provider: &str, body: &str) -> bool {
    // Auto-reset stale flag — covers the panic-skips-reset edge case.
    if let Ok(mut g) = LAST_NOTIFIED_AT.lock() {
        if let Some(at) = *g {
            if at.elapsed() >= AUTO_RESET_AFTER {
                NOTIFIED_THIS_SESSION.store(false, Ordering::SeqCst);
                *g = None;
            }
        }
    }

    let was_set = NOTIFIED_THIS_SESSION.swap(true, Ordering::SeqCst);
    if was_set {
        tracing::debug!(
            "rate_limit_notify: suppressed (already fired this session) provider={provider}"
        );
        return false;
    }

    if let Ok(mut g) = LAST_NOTIFIED_AT.lock() {
        *g = Some(Instant::now());
    }

    fire_notification(provider, body);
    true
}

#[cfg(feature = "notify")]
fn fire_notification(provider: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .summary(&format!("Fono — {provider} rate-limited"))
        .body(body)
        .icon("dialog-warning")
        .timeout(notify_rust::Timeout::Milliseconds(8_000))
        .show();
}

#[cfg(not(feature = "notify"))]
fn fire_notification(_provider: &str, _body: &str) {
    // Slim builds without notify-rust: the `tracing::warn!` at the
    // 429 site is the only surface.
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests share global state (NOTIFIED_THIS_SESSION + LAST_NOTIFIED_AT),
    // so they must run serially. A dedicated mutex sequences them.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn first_call_after_reset_fires_then_dedups() {
        let _g = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_session_flag();
        assert!(notify_once("groq", "test 1"));
        assert!(!notify_once("groq", "test 2"));
        assert!(!notify_once("groq", "test 3"));
    }

    #[test]
    fn reset_re_arms_the_flag() {
        let _g = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_session_flag();
        assert!(notify_once("groq", "first"));
        assert!(!notify_once("groq", "second"));
        reset_session_flag();
        assert!(notify_once("groq", "third"));
    }

    #[test]
    fn auto_reset_after_window_re_arms() {
        let _g = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_session_flag();
        assert!(notify_once("groq", "first"));
        if let Ok(mut g) = LAST_NOTIFIED_AT.lock() {
            let stale = Instant::now()
                .checked_sub(AUTO_RESET_AFTER + Duration::from_secs(1))
                .expect("clock supports the test offset");
            *g = Some(stale);
        }
        assert!(notify_once("groq", "second"));
        assert!(!notify_once("groq", "third"));
    }
}
