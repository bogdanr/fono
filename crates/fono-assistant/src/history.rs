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
    /// Result of a tool/function call executed on behalf of the
    /// model. Paired with the originating assistant turn via
    /// [`ChatTurn::tool_call_id`]. OpenAI-compatible providers use
    /// the literal wire role `"tool"`; Anthropic is text-only for
    /// now and downgrades these turns to a brief narration.
    Tool,
}

impl ChatRole {
    /// Lower-case wire identifier. OpenAI-compatible providers use
    /// `system|user|assistant|tool`; Anthropic uses `user|assistant`
    /// and elevates `system` to a top-level field — backends
    /// translate as needed.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

/// One tool/function call as emitted by the model. Mirrors the
/// OpenAI `tool_calls[].function` wire shape. `arguments` is a raw
/// JSON string per the spec (it is _not_ pre-parsed because the
/// model may emit invalid JSON that still needs to be echoed back
/// verbatim on subsequent turns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
    /// Wall-clock instant at which the turn was recorded. Used by
    /// [`ConversationHistory`]'s window-based pruning.
    pub at: Instant,
    /// Populated on [`ChatRole::Assistant`] turns where the model
    /// invoked one or more tools. Empty otherwise. When non-empty,
    /// `content` is usually empty too (the model emits either text
    /// _or_ tool calls in a single completion, not both).
    pub tool_calls: Vec<ToolCall>,
    /// Populated on [`ChatRole::Tool`] turns; pairs the result back
    /// to the originating call in the preceding assistant turn.
    pub tool_call_id: Option<String>,
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
        Self { turns: VecDeque::new(), last_activity: None, window, max_turns }
    }

    /// Append a user turn.
    pub fn push_user(&mut self, content: String) {
        self.push_full(ChatRole::User, content, Vec::new(), None);
    }

    /// Append an assistant turn (the model's reply).
    pub fn push_assistant(&mut self, content: String) {
        self.push_full(ChatRole::Assistant, content, Vec::new(), None);
    }

    /// Append an assistant turn whose only output was a set of tool
    /// calls. `content` may be empty (and usually is — the model
    /// either spoke or called tools, not both). The list MUST match
    /// the tool calls actually issued so the next turn's wire
    /// serialisation can echo them back.
    pub fn push_assistant_tool_calls(&mut self, content: String, calls: Vec<ToolCall>) {
        self.push_full(ChatRole::Assistant, content, calls, None);
    }

    /// Append a tool-result turn. `content` is a short text summary
    /// that the model can read in subsequent turns; the actual tool
    /// payload (image data, large blobs) is _not_ retained in
    /// history.
    pub fn push_tool_result(&mut self, tool_call_id: String, summary: String) {
        self.push_full(ChatRole::Tool, summary, Vec::new(), Some(tool_call_id));
    }

    fn push_full(
        &mut self,
        role: ChatRole,
        content: String,
        tool_calls: Vec<ToolCall>,
        tool_call_id: Option<String>,
    ) {
        let at = Instant::now();
        self.last_activity = Some(at);
        self.turns.push_back(ChatTurn { role, content, at, tool_calls, tool_call_id });
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

    /// Drop the entire history (e.g. on the tray "Forget conversation"
    /// entry / `fono assistant forget` CLI).
    pub fn clear(&mut self) {
        self.turns.clear();
        self.last_activity = None;
    }

    /// True if no turn has been pushed in the last `window` duration.
    /// Convenient for the orchestrator to log "starting a fresh
    /// conversation" when applicable.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.last_activity.is_none_or(|t| t.elapsed() > self.window)
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
        assert_eq!(ChatRole::Tool.as_str(), "tool");
    }

    #[test]
    fn tool_call_turns_round_trip_through_history() {
        let mut h = ConversationHistory::default();
        h.push_user("what am I looking at?".into());
        h.push_assistant_tool_calls(
            String::new(),
            vec![ToolCall {
                id: "call_1".into(),
                name: "fono_screen".into(),
                arguments: "{\"mode\":\"automatic\"}".into(),
            }],
        );
        h.push_tool_result("call_1".into(), "Captured 800x600 PNG of focused window".into());
        h.push_assistant("Looks like a terminal with an error.".into());

        let snap = h.snapshot();
        assert_eq!(snap.len(), 4);
        assert_eq!(snap[0].role, ChatRole::User);
        assert_eq!(snap[1].role, ChatRole::Assistant);
        assert_eq!(snap[1].tool_calls.len(), 1);
        assert_eq!(snap[1].tool_calls[0].name, "fono_screen");
        assert_eq!(snap[2].role, ChatRole::Tool);
        assert_eq!(snap[2].tool_call_id.as_deref(), Some("call_1"));
        assert!(snap[2].content.contains("Captured"));
        assert_eq!(snap[3].role, ChatRole::Assistant);
        assert!(snap[3].tool_calls.is_empty());
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
