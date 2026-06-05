//! Benchmarks for TUI transcript rendering and large event-log replay.
//! Run with `cargo bench -p dcode-ai-cli --bench tui_perf`.

use criterion::{Criterion, criterion_group, criterion_main};
use dcode_ai_cli::tui::replay::replay_event_log_into_state;
use dcode_ai_cli::tui::state::{DisplayBlock, TuiSessionState};
use dcode_ai_cli::tui::transcript::transcript_line_count_for_bench;
use dcode_ai_common::event::{AgentEvent, EndReason, EventEnvelope};
use serde_json::json;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn large_tui_state(block_count: usize) -> TuiSessionState {
    let mut state = TuiSessionState::new(
        "session-bench".into(),
        "MiniMax-M2.5".into(),
        "build".into(),
        "default".into(),
        PathBuf::from("."),
        true,
    );

    for index in 0..block_count {
        state.blocks.push(DisplayBlock::User(format!(
            "Please inspect crates/runtime/src/supervisor.rs and explain issue {index}."
        )));
        state.blocks.push(DisplayBlock::Assistant(format!(
            "Found relevant context for issue {index}.\n\n- file: crates/runtime/src/supervisor.rs\n- next: run focused tests"
        )));
        state.blocks.push(DisplayBlock::ToolDone {
            name: "execute_bash".into(),
            call_id: format!("call-{index}"),
            ok: index % 7 != 0,
            detail: format!(
                "command: cargo test -p dcode-ai-runtime supervisor::tests\nstatus: {}",
                if index % 7 == 0 { "failed" } else { "ok" }
            ),
            duration_ms: Some(120 + index as u64),
        });
    }

    state
}

fn large_event_log(event_count: u64) -> String {
    let mut raw = String::new();
    for id in 0..event_count {
        let event = match id % 5 {
            0 => AgentEvent::MessageReceived {
                role: "user".into(),
                content: format!("turn {id}: inspect @crates/cli/src/main.rs"),
            },
            1 => AgentEvent::MessageReceived {
                role: "assistant".into(),
                content: format!("turn {id}: analysis complete"),
            },
            2 => AgentEvent::ToolCallStarted {
                call_id: format!("call-{id}"),
                tool: "execute_bash".into(),
                input: json!({"command": "cargo test -p dcode-ai-cli json"}),
            },
            3 => AgentEvent::ToolCallCompleted {
                call_id: format!("call-{id}"),
                output: dcode_ai_common::tool::ToolResult {
                    call_id: format!("call-{id}"),
                    success: true,
                    output: "ok".into(),
                    error: None,
                },
            },
            _ => AgentEvent::SessionEnded {
                reason: EndReason::Completed,
            },
        };
        raw.push_str(&serde_json::to_string(&EventEnvelope::new(id, event)).expect("event json"));
        raw.push('\n');
    }
    raw
}

fn bench_transcript_render(c: &mut Criterion) {
    let state = large_tui_state(200);
    c.bench_function("tui/transcript_lines_and_hits/600_blocks_width_100", |b| {
        b.iter(|| transcript_line_count_for_bench(black_box(&state), black_box(100)))
    });
}

fn bench_event_log_replay(c: &mut Criterion) {
    let raw = large_event_log(1_000);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.events.jsonl");
    std::fs::write(&path, raw).expect("write event log");
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    c.bench_function("tui/replay_event_log_into_state/1000_events", |b| {
        b.iter(|| {
            let state = Arc::new(Mutex::new(TuiSessionState::new(
                "session-bench".into(),
                "MiniMax-M2.5".into(),
                "build".into(),
                "default".into(),
                PathBuf::from("."),
                true,
            )));
            rt.block_on(replay_event_log_into_state(
                black_box(&path),
                black_box(&state),
            ));
        })
    });
}

criterion_group!(benches, bench_transcript_render, bench_event_log_replay);
criterion_main!(benches);
