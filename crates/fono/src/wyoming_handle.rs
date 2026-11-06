// SPDX-License-Identifier: GPL-3.0-only
//! Fono's assistant as a conversation agent over Wyoming.
//!
//! Home Assistant's Wyoming integration sends the user's words as a
//! `transcript` and speaks the `handled` text that comes back. This module
//! answers it with the same assistant, tools and prompt the hold-to-talk
//! assistant uses, so a "turn on the kitchen light" works the same from a
//! Home Assistant voice satellite as from the desktop.
//!
//! Each Home Assistant `conversation_id` keeps its own rolling history, so
//! a follow-up question on one satellite does not see another room's talk.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use fono_assistant::traits::{AssistantContext, ToolEvent};
use fono_assistant::ConversationHistory;
use fono_core::{Config, Paths};
use fono_net::wyoming::server::{AdvertisedHandle, HandleProvider, HandleReply, HandleRequest};
use futures::StreamExt;
use tokio::sync::Mutex;

use crate::session::SessionOrchestrator;

/// Conversations kept at once. Home Assistant opens a new id per
/// conversation, so without a cap the map would grow for as long as the
/// daemon runs.
const MAX_CONVERSATIONS: usize = 32;

type Histories = Arc<Mutex<HashMap<String, (Instant, ConversationHistory)>>>;

/// What `info.handle` should advertise, or `None` when no assistant is set
/// up and Fono should not offer itself as a conversation agent.
pub fn advertised(config: &Config, orch: &SessionOrchestrator) -> Option<AdvertisedHandle> {
    let assistant = orch.assistant_snapshot()?;
    let tools = &config.assistant.tools;
    Some(AdvertisedHandle {
        languages: config.general.languages.clone(),
        model: assistant.model().unwrap_or_else(|| assistant.name().to_string()),
        home_control: tools.enabled && !tools.mcp.is_empty() && assistant.can_run_actions(),
    })
}

/// Build the provider the Wyoming server calls for each `transcript`.
pub fn provider(orch: Arc<SessionOrchestrator>, paths: Paths) -> HandleProvider {
    let histories: Histories = Arc::default();
    let paths = Arc::new(paths);
    Arc::new(move |req| {
        let (orch, paths, histories) = (Arc::clone(&orch), Arc::clone(&paths), histories.clone());
        Box::pin(async move { answer(&orch, &paths, &histories, req).await })
    })
}

async fn answer(
    orch: &SessionOrchestrator,
    paths: &Paths,
    histories: &Histories,
    req: HandleRequest,
) -> Result<HandleReply> {
    let cfg = orch.current_config();
    let assistant =
        orch.assistant_snapshot().ok_or_else(|| anyhow!("Fono has no assistant set up"))?;
    let learning = crate::actions::Learning::new(paths);
    let actions = crate::actions::build(&cfg, paths, None, &learning);
    let (actions, tools_note) =
        crate::actions::for_backend(actions, assistant.can_run_actions(), assistant.name());
    let key = req.conversation_id.unwrap_or_default();
    let history = take_history(histories, &cfg, &key).await;
    let language = req.language.filter(|l| !l.is_empty());
    let ctx = AssistantContext {
        system_prompt: crate::session::assistant_prompt_context(tools_note.as_deref()),
        instructions: Some(cfg.assistant.prompt_main.trim().to_string()),
        language: language.clone(),
        history,
        actions,
        ..AssistantContext::default()
    };

    let mut stream = assistant.reply_stream(&req.text, &ctx).await?;
    let mut reply = String::new();
    let mut tool_events = Vec::new();
    while let Some(delta) = stream.next().await {
        let delta = delta?;
        reply.push_str(&delta.text);
        tool_events.extend(delta.tool_event);
    }
    let reply = reply.trim().to_string();
    // Writing the turn down touches the catalogue on disk; do it off the
    // reply path so Home Assistant hears back without waiting for it.
    let (said, lang) = (req.text.clone(), language.clone().unwrap_or_default());
    tokio::task::spawn_blocking(move || learning.finished(&said, &lang));

    let mut map = histories.lock().await;
    let (used, history) = map.entry(key).or_insert_with(|| (Instant::now(), new_history(&cfg)));
    *used = Instant::now();
    history.push_user(req.text);
    let acted = record_tools(history, tool_events);
    if !reply.is_empty() {
        history.push_assistant(reply.clone());
    }
    // Same rule as the desktop assistant: after it acted on the home,
    // start the next turn fresh, so a stale "turn it off" cannot refer
    // back to the wrong device.
    if acted {
        history.clear();
    }
    drop(map);
    Ok(HandleReply { text: reply, continue_conversation: false })
}

/// Push a turn's tool calls and results into `history`; `true` when the
/// assistant called at least one tool.
fn record_tools(history: &mut ConversationHistory, events: Vec<ToolEvent>) -> bool {
    let mut acted = false;
    for event in events {
        match event {
            ToolEvent::Called(call) => {
                history.push_assistant_tool_calls(String::new(), vec![call]);
                acted = true;
            }
            ToolEvent::Result { tool_call_id, summary, .. } => {
                history.push_tool_result(tool_call_id, summary);
            }
        }
    }
    acted
}

/// The history for conversation `key`, evicting the least recently used
/// conversation when the map is full.
async fn take_history(
    histories: &Histories,
    cfg: &Config,
    key: &str,
) -> Vec<fono_assistant::ChatTurn> {
    let mut map = histories.lock().await;
    if !map.contains_key(key) && map.len() >= MAX_CONVERSATIONS {
        if let Some(oldest) = map.iter().min_by_key(|(_, (used, _))| *used).map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
    }
    let (used, history) =
        map.entry(key.to_string()).or_insert_with(|| (Instant::now(), new_history(cfg)));
    *used = Instant::now();
    let turns = history.snapshot();
    drop(map);
    turns
}

fn new_history(cfg: &Config) -> ConversationHistory {
    ConversationHistory::new(
        std::time::Duration::from_secs(60 * u64::from(cfg.assistant.history_window_minutes)),
        cfg.assistant.history_max_turns as usize,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fono_assistant::ToolCall;

    #[test]
    fn tool_calls_mark_the_turn_as_acted() {
        let mut h = ConversationHistory::new(std::time::Duration::from_secs(60), 0);
        assert!(!record_tools(&mut h, Vec::new()));
        let call = ToolCall { id: "1".into(), name: "HassTurnOn".into(), arguments: "{}".into() };
        let events = vec![
            ToolEvent::Called(call),
            ToolEvent::Result {
                tool_call_id: "1".into(),
                summary: "done".into(),
                failed: false,
                sent: None,
            },
        ];
        assert!(record_tools(&mut h, events));
        assert_eq!(h.snapshot().len(), 2);
    }

    #[tokio::test]
    async fn least_recently_used_conversation_is_evicted() {
        let histories: Histories = Arc::default();
        let cfg = Config::default();
        for i in 0..=MAX_CONVERSATIONS {
            take_history(&histories, &cfg, &i.to_string()).await;
        }
        let (len, has_oldest) = {
            let map = histories.lock().await;
            (map.len(), map.contains_key("0"))
        };
        assert_eq!(len, MAX_CONVERSATIONS);
        assert!(!has_oldest, "the oldest conversation goes first");
    }
}
