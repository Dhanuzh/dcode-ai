//! Replay persisted `*.events.jsonl` into `TuiSessionState` so resume shows prior transcript.

use crate::tui::state::TuiSessionState;
use dcode_ai_common::event::{AgentEvent, EventEnvelope};
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};

fn parse_json_values_on_line(line: &str) -> Vec<Value> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let de = serde_json::Deserializer::from_str(trimmed);
    for res in de.into_iter::<Value>() {
        match res {
            Ok(v) => out.push(v),
            Err(_) => break,
        }
    }
    out
}

fn value_to_event(v: &Value) -> Option<AgentEvent> {
    if v.get("event").is_some() {
        serde_json::from_value::<EventEnvelope>(v.clone())
            .ok()
            .map(|e| e.event)
    } else {
        serde_json::from_value::<AgentEvent>(v.clone()).ok()
    }
}

/// Skip streaming deltas when replaying; final `MessageReceived` for assistant already has full text.
/// Session start/end lines don't belong in a replay either: logs written
/// before 1.0.32 can hold another session's start, and applying it switched
/// the live session (and its log) to that other id.
fn should_skip_on_replay(ev: &AgentEvent) -> bool {
    matches!(
        ev,
        AgentEvent::TokensStreamed { .. }
            | AgentEvent::ThinkingDelta { .. }
            | AgentEvent::SessionStarted { .. }
            | AgentEvent::SessionEnded { .. }
    )
}

/// The logs that hold a session's conversation, oldest first: its own,
/// then logs of throwaway ids that resuming it used before 1.0.32 (a
/// `SessionStarted` for an id with no saved state, found in this chain).
/// Those ids got the resumed conversation's events, so they are read back
/// in order. Nothing is rewritten.
pub fn session_log_chain(log_path: &Path) -> Vec<std::path::PathBuf> {
    let mut chain = vec![log_path.to_path_buf()];
    let Some(dir) = log_path.parent() else {
        return chain;
    };
    let mut i = 0;
    while i < chain.len() && chain.len() < 50 {
        let Ok(raw) = std::fs::read_to_string(&chain[i]) else {
            i += 1;
            continue;
        };
        for line in raw.lines().filter(|l| l.contains("\"SessionStarted\"")) {
            for ev in events_on_line(line) {
                if let AgentEvent::SessionStarted { session_id, .. } = ev {
                    let log = dir.join(format!("{session_id}.events.jsonl"));
                    let has_state = dir.join(format!("{session_id}.json")).exists();
                    if !has_state && log.exists() && !chain.contains(&log) {
                        chain.push(log);
                    }
                }
            }
        }
        i += 1;
    }
    chain
}

/// Cheap check before parsing: streaming deltas are most of a log's lines
/// and are skipped anyway. The type tag sits near the start of the line.
fn is_delta_line(line: &str) -> bool {
    let mut end = line.len().min(160);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let head = &line[..end];
    head.contains(r#""type":"TokensStreamed""#) || head.contains(r#""type":"ThinkingDelta""#)
}

/// Parse one log line: usually one envelope, rarely several objects run
/// together (a crash mid-write).
fn events_on_line(line: &str) -> Vec<AgentEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() || is_delta_line(trimmed) {
        return Vec::new();
    }
    if let Ok(env) = serde_json::from_str::<EventEnvelope>(trimmed) {
        return vec![env.event];
    }
    parse_json_values_on_line(trimmed)
        .iter()
        .filter_map(value_to_event)
        .collect()
}

/// Logs written before tool outputs were capped can hold huge results
/// (one was 23 MB); show them the way new logs store them.
fn cap_replayed(ev: &mut AgentEvent) {
    if let AgentEvent::ToolCallCompleted { output, .. } = ev {
        output.output = dcode_ai_core::agent::cap_tool_output(&output.output);
    }
}

/// Blocks shown on resume; older ones stay in memory for the transcript
/// view (ctrl+o) but are not printed again.
const RESUME_VISIBLE_BLOCKS: usize = 200;

/// Read and parse a log off the async runtime.
fn read_events(log_path: &Path) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    for path in session_log_chain(log_path) {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in raw.lines() {
            for mut ev in events_on_line(line) {
                if should_skip_on_replay(&ev) {
                    continue;
                }
                cap_replayed(&mut ev);
                out.push(ev);
            }
        }
    }
    out
}

/// Load historical events from disk into TUI state (used on resume / reopen same log).
pub async fn replay_event_log_into_state(log_path: &Path, state: &Arc<Mutex<TuiSessionState>>) {
    let path = log_path.to_path_buf();
    let events = tokio::task::spawn_blocking(move || read_events(&path))
        .await
        .unwrap_or_default();
    if events.is_empty() {
        return;
    }

    let applied = events.len();
    if let Ok(mut g) = state.lock() {
        let first_block = g.blocks.len();
        g.replaying = true;
        for ev in &events {
            g.apply_event(ev);
        }
        g.replaying = false;
        g.streaming_assistant = None;
        g.clear_replayed_interaction_state();
        hide_old_blocks(&mut g, first_block);
    }

    tracing::debug!(path = %log_path.display(), events_applied = applied, "replayed event log into TUI");
}

/// Rebuild the transcript from a session's saved messages (Claude Code
/// resumes from its transcript the same way): your messages, the replies,
/// and each tool call with its result.
pub fn replay_messages_into_state(
    messages: &[dcode_ai_common::message::Message],
    state: &Arc<Mutex<TuiSessionState>>,
) {
    use dcode_ai_common::message::Role;
    let results: std::collections::HashMap<&str, String> = messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| {
            m.tool_call_id
                .as_deref()
                .map(|id| (id, m.content.to_summary_text()))
        })
        .collect();
    let Ok(mut g) = state.lock() else {
        return;
    };
    let first_block = g.blocks.len();
    g.replaying = true;
    for m in messages {
        let text = m.content.to_summary_text();
        match m.role {
            Role::System | Role::Tool => {}
            Role::User => {
                if !text.trim().is_empty() {
                    g.apply_event(&AgentEvent::MessageReceived {
                        role: "user".into(),
                        content: text,
                    });
                }
            }
            Role::Assistant => {
                if !text.trim().is_empty() {
                    g.apply_event(&AgentEvent::MessageReceived {
                        role: "assistant".into(),
                        content: text,
                    });
                }
                for call in m.tool_calls.iter().flatten() {
                    g.apply_event(&AgentEvent::ToolCallStarted {
                        call_id: call.id.clone(),
                        tool: call.name.clone(),
                        input: call.arguments.clone(),
                    });
                    let output = results.get(call.id.as_str()).cloned().unwrap_or_default();
                    g.apply_event(&AgentEvent::ToolCallCompleted {
                        call_id: call.id.clone(),
                        output: dcode_ai_common::tool::ToolResult {
                            call_id: call.id.clone(),
                            success: !output.starts_with("Error"),
                            output: dcode_ai_core::agent::cap_tool_output(&output),
                            error: None,
                        },
                    });
                }
            }
        }
    }
    g.replaying = false;
    g.streaming_assistant = None;
    g.clear_replayed_interaction_state();
    hide_old_blocks(&mut g, first_block);
}

/// Restore a resumed session's transcript: from its event log, or — when
/// the log is missing or holds far fewer of your messages than the session
/// (logs written before 1.0.24 could land in another session's file) —
/// from the saved messages.
pub async fn restore_transcript(
    log_path: &Path,
    messages: &[dcode_ai_common::message::Message],
    state: &Arc<Mutex<TuiSessionState>>,
) {
    let first_block = state.lock().map(|g| g.blocks.len()).unwrap_or(0);
    replay_event_log_into_state(log_path, state).await;
    let saved_users = messages
        .iter()
        .filter(|m| m.role == dcode_ai_common::message::Role::User)
        .filter(|m| !m.content.to_summary_text().trim().is_empty())
        .count();
    let shown_users = state
        .lock()
        .map(|g| {
            g.blocks[first_block.min(g.blocks.len())..]
                .iter()
                .filter(|b| matches!(b, crate::tui::state::DisplayBlock::User(_)))
                .count()
        })
        .unwrap_or(0);
    // Some user messages are hidden on purpose (goal continuations, task
    // notifications), so only a log that clearly lacks the conversation
    // is replaced.
    if saved_users > 0 && (shown_users == 0 || shown_users * 2 < saved_users) {
        if let Ok(mut g) = state.lock() {
            g.blocks.truncate(first_block);
            g.flushed_block_count = g.flushed_block_count.min(first_block);
        }
        replay_messages_into_state(messages, state);
    }
}

/// Print only the newest blocks of a long history: the rest are marked
/// as already flushed, behind a one-line note.
fn hide_old_blocks(g: &mut TuiSessionState, first_block: usize) {
    let replayed = g.blocks.len().saturating_sub(first_block);
    if replayed <= RESUME_VISIBLE_BLOCKS || g.flushed_block_count > first_block {
        return;
    }
    let cut = g.blocks.len() - RESUME_VISIBLE_BLOCKS;
    let hidden = cut - first_block;
    g.blocks.insert(
        cut,
        crate::tui::state::DisplayBlock::System(format!(
            "⋯ {hidden} earlier messages not shown · /transcript shows the full history"
        )),
    );
    g.flushed_block_count = cut;
    g.touch_transcript();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_envelope_and_bare_event() {
        let line = r#"{"id":1,"event":{"type":"MessageReceived","role":"user","content":"hi"}}"#;
        let vals = parse_json_values_on_line(line);
        assert_eq!(vals.len(), 1);
        let ev = value_to_event(&vals[0]).expect("event");
        assert!(matches!(ev, AgentEvent::MessageReceived { .. }));

        let line2 = r#"{"type":"MessageReceived","role":"assistant","content":"yo"}"#;
        let vals2 = parse_json_values_on_line(line2);
        let ev2 = value_to_event(&vals2[0]).expect("event");
        assert!(matches!(ev2, AgentEvent::MessageReceived { .. }));
    }

    #[test]
    fn parses_two_concatenated_json_objects() {
        let line = concat!(
            r#"{"type":"SessionEnded","reason":{"Completed":null}}"#,
            r#"{"type":"SessionStarted","session_id":"s","workspace":".","model":"m"}"#
        );
        let vals = parse_json_values_on_line(line);
        assert_eq!(vals.len(), 2);
    }

    #[test]
    fn delta_lines_are_skipped_before_parsing() {
        let delta = r#"{"schema_version":1,"id":3,"ts":"2026-10-23T11:18:22Z","event":{"type":"TokensStreamed","delta":"x"}}"#;
        assert!(events_on_line(delta).is_empty());
        let msg = r#"{"schema_version":1,"id":4,"ts":"2026-10-23T11:18:22Z","event":{"type":"MessageReceived","role":"user","content":"hi"}}"#;
        assert_eq!(events_on_line(msg).len(), 1);
    }

    fn fresh_state() -> Arc<Mutex<TuiSessionState>> {
        Arc::new(Mutex::new(TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "Default".into(),
            std::path::PathBuf::from("/tmp"),
            false,
        )))
    }

    fn convo() -> Vec<dcode_ai_common::message::Message> {
        use dcode_ai_common::message::{Message, MessageToolCall};
        vec![
            Message::system("system prompt"),
            Message::user("fix the login page"),
            Message::assistant_with_tool_calls(
                "Reading it.",
                vec![MessageToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "login.ts"}),
                }],
            ),
            Message::tool("c1", "export const x = 1;"),
            Message::assistant("Fixed the redirect."),
        ]
    }

    #[tokio::test]
    async fn a_missing_log_is_rebuilt_from_the_saved_messages() {
        let st = fresh_state();
        let dir = tempfile::tempdir().unwrap();
        restore_transcript(&dir.path().join("none.events.jsonl"), &convo(), &st).await;
        let g = st.lock().unwrap();
        use crate::tui::state::DisplayBlock as B;
        assert!(
            g.blocks
                .iter()
                .any(|b| matches!(b, B::User(t) if t == "fix the login page"))
        );
        assert!(
            g.blocks
                .iter()
                .any(|b| matches!(b, B::Assistant(t) if t.contains("Fixed the redirect")))
        );
        assert!(
            g.blocks
                .iter()
                .any(|b| matches!(b, B::ToolDone { name, .. } if name == "read_file"))
        );
        assert!(
            !g.blocks
                .iter()
                .any(|b| matches!(b, B::System(t) if t.contains("system prompt")))
        );
    }

    #[tokio::test]
    async fn a_complete_log_is_kept() {
        let st = fresh_state();
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("s.events.jsonl");
        std::fs::write(
            &log,
            concat!(
                r#"{"schema_version":1,"id":1,"ts":"2026-10-27T10:00:00Z","event":{"type":"MessageReceived","role":"user","content":"fix the login page"}}"#,
                "\n",
                r#"{"schema_version":1,"id":2,"ts":"2026-10-27T10:00:01Z","event":{"type":"MessageReceived","role":"assistant","content":"from the log"}}"#,
                "\n"
            ),
        )
        .unwrap();
        restore_transcript(&log, &convo(), &st).await;
        let g = st.lock().unwrap();
        use crate::tui::state::DisplayBlock as B;
        assert!(
            g.blocks
                .iter()
                .any(|b| matches!(b, B::Assistant(t) if t == "from the log"))
        );
        assert!(
            !g.blocks
                .iter()
                .any(|b| matches!(b, B::Assistant(t) if t.contains("Fixed")))
        );
    }

    #[test]
    fn long_histories_show_only_the_newest_blocks() {
        let mut g = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "Default".into(),
            std::path::PathBuf::from("/tmp"),
            false,
        );
        for i in 0..(RESUME_VISIBLE_BLOCKS + 50) {
            g.blocks
                .push(crate::tui::state::DisplayBlock::User(format!("m{i}")));
        }
        hide_old_blocks(&mut g, 0);
        assert_eq!(g.flushed_block_count, 50);
        assert!(matches!(
            &g.blocks[50],
            crate::tui::state::DisplayBlock::System(t) if t.contains("50 earlier messages")
        ));
    }

    /// Logs written by the old resume (a throwaway id with no saved state)
    /// are read back after the session's own log; a real session's log
    /// (it has saved state) is not.
    #[test]
    fn replay_follows_logs_of_old_resume_ids() {
        let dir = tempfile::tempdir().unwrap();
        let line = |ev: &str| format!("{{\"id\":1,\"event\":{ev}}}\n");
        let user = |t: &str| {
            line(&format!(
                r#"{{"type":"MessageReceived","role":"user","content":"{t}"}}"#
            ))
        };
        let started = |id: &str| {
            line(&format!(
                r#"{{"type":"SessionStarted","session_id":"{id}","workspace":"/w","model":"m"}}"#
            ))
        };
        std::fs::write(
            dir.path().join("s.events.jsonl"),
            user("first") + &started("boot") + &started("other"),
        )
        .unwrap();
        std::fs::write(dir.path().join("s.json"), "{}").unwrap();
        std::fs::write(dir.path().join("boot.events.jsonl"), user("latest")).unwrap();
        std::fs::write(dir.path().join("other.events.jsonl"), user("not mine")).unwrap();
        std::fs::write(dir.path().join("other.json"), "{}").unwrap();

        let events = read_events(&dir.path().join("s.events.jsonl"));
        let said: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::MessageReceived { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(said, ["first", "latest"]);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::SessionStarted { .. })),
            "a replay never switches the live session"
        );
    }

    #[test]
    fn skips_tokens_streamed_on_replay_flag() {
        assert!(should_skip_on_replay(&AgentEvent::TokensStreamed {
            delta: "x".into()
        }));
        assert!(!should_skip_on_replay(&AgentEvent::MessageReceived {
            role: "user".into(),
            content: "a".into(),
        }));
    }
}
