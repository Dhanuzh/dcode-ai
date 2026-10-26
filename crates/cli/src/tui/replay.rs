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
fn should_skip_on_replay(ev: &AgentEvent) -> bool {
    matches!(
        ev,
        AgentEvent::TokensStreamed { .. } | AgentEvent::ThinkingDelta { .. }
    )
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
    let Ok(raw) = std::fs::read_to_string(log_path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        for mut ev in events_on_line(line) {
            if should_skip_on_replay(&ev) {
                continue;
            }
            cap_replayed(&mut ev);
            out.push(ev);
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
