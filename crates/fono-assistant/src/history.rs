// SPDX-License-Identifier: GPL-3.0-only
//! Rolling conversation history for the voice assistant.
//!
//! Keeps the last N turns within a sliding time window relative to
//! the most recent activity. Both bounds are enforced on every
//! [`ConversationHistory::snapshot`] call so the [`crate::Assistant`]
//! trait sees a consistent view regardless of when the trim runs.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
    System,
}

impl ChatRole {
    /// Lower-case wire identifier. OpenAI-compatible providers use
    /// `system|user|assistant`; Anthropic uses `user|assistant` and
    /// elevates `system` to a top-level field — backends translate
    /// as needed.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
    /// Wall-clock instant at which the turn was recorded. Used by
    /// [`ConversationHistory`]'s window-based pruning.
    pub at: Instant,
}

/// Rolling chat history.
///
/// Two pruning rules apply:
///
/// * **Time window**: turns older than `window` from the most recent
///   activity are dropped. The window resets on every push, so an
///   active conversation can run as long as the user keeps talking.
///   Once the user pauses for longer than `window`, the entire
///   history is considered stale and snapshot returns empty.
/// * **Max-turn cap**: independent of the window, retain at most
///   `max_turns` recent turns. Bounds input-token cost on long
///   continuous flows.
#[derive(Debug, Clone)]
pub struct ConversationHistory {
    turns: VecDeque<ChatTurn>,
    last_activity: Option<Instant>,
    window: Duration,
    max_turns: usize,
}

impl ConversationHistory {
    /// Construct a history that prunes turns older than `window` from
    /// the most recent push and caps total turn count at `max_turns`.
    /// `max_turns = 0` is treated as "no cap".
    #[must_use]
    pub fn new(window: Duration, max_turns: usize) -> Self {
        Self {
            turns: VecDeque::new(),
            last_activity: None,
            window,
            max_turns,
        }
    }

    /// Append a user turn.
    pub fn push_user(&mut self, content: String) {
        self.push(ChatRole::User, content);
    }

    /// Append an assistant turn (the model's reply).
    pub fn push_assistant(&mut self, content: String) {
        self.push(ChatRole::Assistant, content);
    }

    fn push(&mut self, role: ChatRole, content: String) {
        let at = Instant::now();
        self.last_activity = Some(at);
        self.turns.push_back(ChatTurn { role, content, at });
        self.trim_max_turns();
    }

    /// Take a pruned snapshot of the history. Applies both the time-
    /// window and max-turn rules, mutating internal state so
    /// subsequent pushes don't see stale entries.
    pub fn snapshot(&mut self) -> Vec<ChatTurn> {
        self.prune_window();
        self.trim_max_turns();
        self.turns.iter().cloned().collect()
    }

    /// Drop the entire history (e.g. on dictation key press when
    /// `auto_clear_on_dictation` is set).
    pub fn clear(&mut self) {
        self.turns.clear();
        self.last_activity = None;
    }

    /// True if no turn has been pushed in the last `window` duration.
    /// Convenient for the orchestrator to log "starting a fresh
    /// conversation" when applicable.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.last_activity
            .is_none_or(|t| t.elapsed() > self.window)
    }

    fn prune_window(&mut self) {
        if self.window.is_zero() {
            return;
        }
        let Some(last) = self.last_activity else {
            return;
        };
        if last.elapsed() > self.window {
            // No activity in the whole window — wipe everything.
            self.turns.clear();
            self.last_activity = None;
            return;
        }
        // Drop turns whose `at` is older than `last - window`. We
        // anchor on `last` rather than `now` so a slow turn doesn't
        // accidentally evict the prior context for the next reply.
        let Some(cutoff) = last.checked_sub(self.window) else {
            return; // window > monotonic clock — leave alone
        };
        while let Some(front) = self.turns.front() {
            if front.at < cutoff {
                self.turns.pop_front();
            } else {
                break;
            }
        }
    }

    fn trim_max_turns(&mut self) {
        if self.max_turns == 0 {
            return;
        }
        while self.turns.len() > self.max_turns {
            self.turns.pop_front();
        }
    }
}

impl Default for ConversationHistory {
    fn default() -> Self {
        // Defaults match the docstring on `[assistant]`: 5 minute
        // window, 12 turn cap.
        Self::new(Duration::from_secs(5 * 60), 12)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_history_is_stale_and_empty() {
        let mut h = ConversationHistory::default();
        assert!(h.is_stale());
        assert!(h.snapshot().is_empty());
    }

    #[test]
    fn push_user_and_assistant_returns_in_order() {
        let mut h = ConversationHistory::default();
        h.push_user("hello".into());
        h.push_assistant("hi back".into());
        let snap = h.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].role, ChatRole::User);
        assert_eq!(snap[0].content, "hello");
        assert_eq!(snap[1].role, ChatRole::Assistant);
        assert_eq!(snap[1].content, "hi back");
    }

    #[test]
    fn max_turns_cap_drops_oldest() {
        let mut h = ConversationHistory::new(Duration::from_secs(60), 3);
        h.push_user("a".into());
        h.push_assistant("b".into());
        h.push_user("c".into());
        h.push_assistant("d".into());
        let snap = h.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].content, "b");
        assert_eq!(snap[2].content, "d");
    }

    #[test]
    fn window_zero_means_no_window_pruning() {
        let mut h = ConversationHistory::new(Duration::ZERO, 0);
        h.push_user("a".into());
        // Even a zero window should not crash on snapshot.
        let snap = h.snapshot();
        assert_eq!(snap.len(), 1);
    }

    #[test]
    fn clear_wipes_state() {
        let mut h = ConversationHistory::default();
        h.push_user("a".into());
        h.clear();
        assert!(h.is_stale());
        assert!(h.snapshot().is_empty());
    }

    #[test]
    fn role_wire_strings() {
        assert_eq!(ChatRole::User.as_str(), "user");
        assert_eq!(ChatRole::Assistant.as_str(), "assistant");
        assert_eq!(ChatRole::System.as_str(), "system");
    }

    #[test]
    fn max_turns_zero_means_unbounded() {
        let mut h = ConversationHistory::new(Duration::from_secs(60), 0);
        for i in 0..50 {
            h.push_user(format!("u{i}"));
        }
        let snap = h.snapshot();
        assert_eq!(snap.len(), 50);
    }
}
