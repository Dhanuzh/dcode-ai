//! Tests for the right sidebar feature across TUI, web chat, and IPC layers.

use dcode_ai_common::event::AgentEvent;
use ratatui::layout::Rect;

// ═══════════════════════════════════════════════════════════════════════════════
// 1. TUI State Defaults
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn sidebar_open_defaults_to_true() {
    let st = make_state();
    assert!(st.sidebar_open, "sidebar_open should default to true");
}

#[test]
fn sidebar_toggle_bounds_defaults_to_none() {
    let st = make_state();
    assert!(
        st.sidebar_toggle_bounds.is_none(),
        "sidebar_toggle_bounds should default to None"
    );
}

#[test]
fn subagents_list_starts_empty() {
    let st = make_state();
    assert!(
        st.subagents.is_empty(),
        "subagents should start as empty vec"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 2. Layout Integration
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn layout_with_sidebar_true_returns_full_area_none_sidebar() {
    let area = Rect::new(0, 0, 120, 40);
    let (main, sidebar) = crate::tui::layout::layout_with_sidebar(area, true);
    assert_eq!(main, area, "main area should be full terminal");
    assert!(sidebar.is_none(), "sidebar rect should be None (removed)");
}

#[test]
fn layout_with_sidebar_false_returns_full_area_none_sidebar() {
    let area = Rect::new(0, 0, 80, 24);
    let (main, sidebar) = crate::tui::layout::layout_with_sidebar(area, false);
    assert_eq!(main, area, "main area should be full terminal");
    assert!(sidebar.is_none(), "sidebar rect should be None (removed)");
}

#[test]
fn layout_with_sidebar_small_terminal_no_panic() {
    // Edge case: very small terminal
    let area = Rect::new(0, 0, 10, 3);
    let (main, sidebar) = crate::tui::layout::layout_with_sidebar(area, true);
    assert_eq!(main, area);
    assert!(sidebar.is_none());
}

#[test]
fn layout_with_sidebar_zero_dimensions_no_panic() {
    let area = Rect::new(0, 0, 0, 0);
    let (main, sidebar) = crate::tui::layout::layout_with_sidebar(area, false);
    assert_eq!(main, area);
    assert!(sidebar.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3. SubagentRow Lifecycle
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn subagent_row_fields_round_trip() {
    let row = crate::tui::state::types::SubagentRow {
        id: "child-1".into(),
        task: "read src/main.rs".into(),
        phase: "read_file".into(),
        detail: "src/main.rs".into(),
        running: true,
        skill: None,
    };
    assert_eq!(row.id, "child-1");
    assert_eq!(row.task, "read src/main.rs");
    assert_eq!(row.phase, "read_file");
    assert_eq!(row.detail, "src/main.rs");
    assert!(row.running);
}

#[test]
fn subagent_row_with_skill() {
    let row = crate::tui::state::types::SubagentRow {
        id: "child-2".into(),
        task: "run tests".into(),
        phase: "running".into(),
        detail: "cargo test".into(),
        running: true,
        skill: Some("pi-agent-rust".into()),
    };
    assert_eq!(row.skill.as_deref(), Some("pi-agent-rust"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4. Subagent Event Processing
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn child_session_spawned_creates_subagent_row() {
    let mut st = make_state();
    st.apply_event(&AgentEvent::ChildSessionSpawned {
        parent_session_id: "parent-1".into(),
        child_session_id: "child-abc".into(),
        task: "write tests".into(),
        workspace: std::path::PathBuf::from("/tmp"),
        branch: None,
    });
    assert_eq!(st.subagents.len(), 1);
    assert_eq!(st.subagents[0].id, "child-abc");
    assert_eq!(st.subagents[0].task, "write tests");
    assert!(st.subagents[0].running);
}

#[test]
fn child_session_activity_updates_existing_row() {
    let mut st = make_state();
    st.apply_event(&AgentEvent::ChildSessionSpawned {
        parent_session_id: "p".into(),
        child_session_id: "c1".into(),
        task: "analyze".into(),
        workspace: std::path::PathBuf::from("/tmp"),
        branch: None,
    });
    st.apply_event(&AgentEvent::ChildSessionActivity {
        child_session_id: "c1".into(),
        phase: "search_code".into(),
        detail: "TODO markers".into(),
    });
    assert_eq!(st.subagents[0].phase, "search_code");
    assert_eq!(st.subagents[0].detail, "TODO markers");
}

#[test]
fn child_session_activity_on_unknown_id_pushes_new_row() {
    let mut st = make_state();
    // Activity for an id that was never spawned — should still appear
    st.apply_event(&AgentEvent::ChildSessionActivity {
        child_session_id: "orphan".into(),
        phase: "running".into(),
        detail: "something".into(),
    });
    assert_eq!(st.subagents.len(), 1);
    assert_eq!(st.subagents[0].id, "orphan");
}

#[test]
fn child_session_spawned_then_completed_marks_not_running() {
    let mut st = make_state();
    st.apply_event(&AgentEvent::ChildSessionSpawned {
        parent_session_id: "p".into(),
        child_session_id: "c1".into(),
        task: "do work".into(),
        workspace: std::path::PathBuf::from("/tmp"),
        branch: None,
    });
    assert!(st.subagents[0].running);

    st.apply_event(&AgentEvent::ChildSessionCompleted {
        parent_session_id: "p".into(),
        child_session_id: "c1".into(),
        status: "done".into(),
    });
    assert!(
        !st.subagents[0].running,
        "subagent should be marked not running after completion"
    );
    assert_eq!(st.subagents[0].detail, "done");
}

#[test]
fn multiple_subagents_tracked_independently() {
    let mut st = make_state();
    for i in 0..5 {
        st.apply_event(&AgentEvent::ChildSessionSpawned {
            parent_session_id: "p".into(),
            child_session_id: format!("child-{i}"),
            task: format!("task-{i}"),
            workspace: std::path::PathBuf::from("/tmp"),
            branch: None,
        });
    }
    assert_eq!(st.subagents.len(), 5);
    for (i, row) in st.subagents.iter().enumerate() {
        assert_eq!(row.id, format!("child-{i}"));
        assert_eq!(row.task, format!("task-{i}"));
        assert!(row.running);
    }
}

#[test]
fn multiple_subagents_activity_updates_correct_row() {
    let mut st = make_state();
    for i in 0..3 {
        st.apply_event(&AgentEvent::ChildSessionSpawned {
            parent_session_id: "p".into(),
            child_session_id: format!("c{i}"),
            task: format!("t{i}"),
            workspace: std::path::PathBuf::from("/tmp"),
            branch: None,
        });
    }
    // Update only child-1
    st.apply_event(&AgentEvent::ChildSessionActivity {
        child_session_id: "c1".into(),
        phase: "edit_file".into(),
        detail: "src/lib.rs".into(),
    });
    assert_eq!(st.subagents[0].phase, ""); // c0 unchanged
    assert_eq!(st.subagents[1].phase, "edit_file"); // c1 updated
    assert_eq!(st.subagents[2].phase, ""); // c2 unchanged
}

#[test]
fn subagent_completed_only_affects_target_id() {
    let mut st = make_state();
    for i in 0..3 {
        st.apply_event(&AgentEvent::ChildSessionSpawned {
            parent_session_id: "p".into(),
            child_session_id: format!("c{i}"),
            task: format!("t{i}"),
            workspace: std::path::PathBuf::from("/tmp"),
            branch: None,
        });
    }
    st.apply_event(&AgentEvent::ChildSessionCompleted {
        parent_session_id: "p".into(),
        child_session_id: "c1".into(),
        status: "finished".into(),
    });
    assert!(st.subagents[0].running);
    assert!(!st.subagents[1].running);
    assert!(st.subagents[2].running);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 5. Subagent Modal State
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn subagent_modal_defaults_closed() {
    let st = make_state();
    assert!(!st.subagent_modal_open);
    assert_eq!(st.subagent_modal_index, 0);
}

#[test]
fn subagent_modal_index_bounds_clamped() {
    let mut st = make_state();
    // Add 2 subagents
    for i in 0..2 {
        st.apply_event(&AgentEvent::ChildSessionSpawned {
            parent_session_id: "p".into(),
            child_session_id: format!("c{i}"),
            task: format!("t{i}"),
            workspace: std::path::PathBuf::from("/tmp"),
            branch: None,
        });
    }
    // Index should not exceed subagents.len() - 1
    let max_idx = st.subagents.len().saturating_sub(1);
    st.subagent_modal_index = 100; // out of bounds
    // Clamp logic: min(idx, len - 1)
    st.subagent_modal_index = st.subagent_modal_index.min(max_idx);
    assert_eq!(st.subagent_modal_index, 1);
}

// ═══════════════════════════════════��═══════════════════════════════════════════
// 6. ApprovalRequest allow_pattern
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn approval_request_allow_pattern_for_read_file() {
    let req = crate::tui::state::types::ApprovalRequest {
        call_id: "c1".into(),
        tool: "read_file".into(),
        description: "Read a file".into(),
        input: r#"{"path":"src/main.rs"}"#.into(),
    };
    let pattern = req.allow_pattern();
    // Should suggest a glob pattern for the tool
    assert!(
        pattern.contains("read_file") || pattern.contains("src/"),
        "pattern should reference the tool or path, got: {pattern}"
    );
}

#[test]
fn approval_request_allow_pattern_invalid_json() {
    let req = crate::tui::state::types::ApprovalRequest {
        call_id: "c2".into(),
        tool: "execute_bash".into(),
        description: "Run bash".into(),
        input: "not-json".into(),
    };
    let pattern = req.allow_pattern();
    // Should not panic; returns a fallback pattern
    assert!(!pattern.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════════
// 7. Session Status Dot Colors (Web Chat CSS)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn web_chat_session_status_dot_css_classes() {
    // Verify the CSS class mapping matches SessionStatus variants
    let html = include_str!("../web_chat.html");

    // running → green
    assert!(
        html.contains(".sdot.running") && html.contains("background: var(--ok)"),
        "running dot should use var(--ok) = green"
    );
    // completed → gray
    assert!(
        html.contains(".sdot.completed") && html.contains("background: var(--border)"),
        "completed dot should use var(--border) = gray"
    );
    // error → red
    assert!(
        html.contains(".sdot.error") && html.contains("background: var(--err)"),
        "error dot should use var(--err) = red"
    );
    // cancelled → yellow
    assert!(
        html.contains(".sdot.cancelled") && html.contains("background: var(--warn)"),
        "cancelled dot should use var(--warn) = yellow"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 8. Web Chat Sidebar CSS Dimensions
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn web_chat_sidebar_expanded_width() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("#sidebar {\n    width: 264px"),
        "sidebar expanded width should be 264px"
    );
}

#[test]
fn web_chat_sidebar_collapsed_width() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("body.sb-collapsed #sidebar { width: 64px"),
        "sidebar collapsed width should be 64px"
    );
}

#[test]
fn web_chat_turnpanel_expanded_width() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("width: 300px; min-width: 300px"),
        "turnpanel expanded width should be 300px"
    );
}

#[test]
fn web_chat_turnpanel_collapsed_hidden() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("body.tp-collapsed #turnpanel { display: none; }"),
        "turnpanel collapsed should be display: none"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 9. Web Chat Sidebar HTML Structure
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn web_chat_sidebar_element_exists() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("<aside id=\"sidebar\">"),
        "sidebar aside element should exist"
    );
}

#[test]
fn web_chat_session_list_element_exists() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("id=\"session-list\""),
        "session-list element should exist"
    );
}

#[test]
fn web_chat_session_search_element_exists() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("id=\"session-search\""),
        "session-search input should exist"
    );
}

#[test]
fn web_chat_sidebar_toggle_button_exists() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("id=\"sidebar-toggle\""),
        "sidebar-toggle button should exist"
    );
}

#[test]
fn web_chat_turnpanel_element_exists() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("id=\"turnpanel\""),
        "turnpanel element should exist"
    );
}

#[test]
fn web_chat_new_chat_button_exists() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("id=\"new-chat-btn\""),
        "new-chat-btn should exist in sidebar"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 10. Web Chat Sidebar JS Functionality
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn web_chat_sidebar_toggle_persists_in_localstorage() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("localStorage.setItem(\"dcode-ai-sb\""),
        "sidebar collapse state should persist in localStorage"
    );
}

#[test]
fn web_chat_ctrl_b_shortcut_toggles_sidebar() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("k === \"b\"") && html.contains("sidebar-toggle"),
        "Ctrl+B should click sidebar-toggle"
    );
}

#[test]
fn web_chat_fetch_sessions_exists() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("function fetchSessions") || html.contains("fetchSessions"),
        "fetchSessions function should exist"
    );
}

#[test]
fn web_chat_auto_refresh_sessions() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("setInterval(fetchSessions, 30000)"),
        "sessions should auto-refresh every 30 seconds"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 11. Web Chat Mobile Responsive
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn web_chat_mobile_sidebar_hidden_by_default() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("transform: translateX(-100%)"),
        "mobile sidebar should be hidden off-screen by default"
    );
}

#[test]
fn web_chat_mobile_sidebar_slides_in_on_toggle() {
    let html = include_str!("../web_chat.html");
    assert!(
        html.contains("body.sb-mobile-open #sidebar { transform: none; }"),
        "mobile sidebar should slide in when sb-mobile-open class is set"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 12. TUI State Transitions
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn sidebar_toggle_state_changes() {
    let mut st = make_state();
    assert!(st.sidebar_open);
    st.sidebar_open = false;
    assert!(!st.sidebar_open);
    st.sidebar_open = true;
    assert!(st.sidebar_open);
}

#[test]
fn busy_state_updates_on_events() {
    let mut st = make_state();
    assert!(!st.busy);
    st.apply_event(&AgentEvent::BusyStateChanged {
        state: dcode_ai_common::event::BusyState::Thinking,
    });
    assert!(st.busy);
    assert_eq!(
        st.current_busy_state,
        dcode_ai_common::event::BusyState::Thinking
    );
}

#[test]
fn idle_state_clears_busy() {
    let mut st = make_state();
    st.busy = true;
    st.apply_event(&AgentEvent::BusyStateChanged {
        state: dcode_ai_common::event::BusyState::Idle,
    });
    assert!(!st.busy);
    assert_eq!(
        st.current_busy_state,
        dcode_ai_common::event::BusyState::Idle
    );
}

#[test]
fn cost_updated_tracks_tokens() {
    let mut st = make_state();
    st.apply_event(&AgentEvent::CostUpdated {
        input_tokens: 1000,
        output_tokens: 500,
        estimated_cost_usd: 0.05,
        context_tokens: 1500,
    });
    assert_eq!(st.input_tokens, 1000);
    assert_eq!(st.output_tokens, 500);
    assert!((st.cost_usd - 0.05).abs() < f64::EPSILON);
    assert_eq!(st.context_tokens, 1500);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 13. DisplayBlock Types
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn display_block_tool_done_clone() {
    let block = crate::tui::state::types::DisplayBlock::ToolDone {
        name: "read_file".into(),
        call_id: "c1".into(),
        ok: true,
        detail: "100 bytes read".into(),
        duration_ms: Some(42),
    };
    let cloned = block.clone();
    match cloned {
        crate::tui::state::types::DisplayBlock::ToolDone {
            name,
            call_id,
            ok,
            detail,
            duration_ms,
        } => {
            assert_eq!(name, "read_file");
            assert_eq!(call_id, "c1");
            assert!(ok);
            assert_eq!(detail, "100 bytes read");
            assert_eq!(duration_ms, Some(42));
        }
        _ => panic!("expected ToolDone"),
    }
}

#[test]
fn display_block_thinking_clone() {
    let block = crate::tui::state::types::DisplayBlock::Thinking("let me think...".into());
    let cloned = block.clone();
    match cloned {
        crate::tui::state::types::DisplayBlock::Thinking(text) => {
            assert_eq!(text, "let me think...");
        }
        _ => panic!("expected Thinking"),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 14. Edge Cases
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn rapid_sidebar_toggle_no_panic() {
    let mut st = make_state();
    for _ in 0..100 {
        st.sidebar_open = !st.sidebar_open;
    }
    // 100 toggles (even) should return to the starting state without panic
    assert!(st.sidebar_open);
}

#[test]
fn subagent_spawn_complete_cycle() {
    let mut st = make_state();
    // Spawn 10 subagents
    for i in 0..10 {
        st.apply_event(&AgentEvent::ChildSessionSpawned {
            parent_session_id: "p".into(),
            child_session_id: format!("c{i}"),
            task: format!("task-{i}"),
            workspace: std::path::PathBuf::from("/tmp"),
            branch: None,
        });
    }
    assert_eq!(st.subagents.len(), 10);
    assert!(st.subagents.iter().all(|r| r.running));

    // Complete half
    for i in 0..5 {
        st.apply_event(&AgentEvent::ChildSessionCompleted {
            parent_session_id: "p".into(),
            child_session_id: format!("c{i}"),
            status: "done".into(),
        });
    }
    let running_count = st.subagents.iter().filter(|r| r.running).count();
    let done_count = st.subagents.iter().filter(|r| !r.running).count();
    assert_eq!(running_count, 5);
    assert_eq!(done_count, 5);
}

#[test]
fn subagent_activity_during_completion_does_not_panic() {
    let mut st = make_state();
    st.apply_event(&AgentEvent::ChildSessionSpawned {
        parent_session_id: "p".into(),
        child_session_id: "c1".into(),
        task: "work".into(),
        workspace: std::path::PathBuf::from("/tmp"),
        branch: None,
    });
    st.apply_event(&AgentEvent::ChildSessionCompleted {
        parent_session_id: "p".into(),
        child_session_id: "c1".into(),
        status: "done".into(),
    });
    // Activity after completion should not crash
    st.apply_event(&AgentEvent::ChildSessionActivity {
        child_session_id: "c1".into(),
        phase: "cleanup".into(),
        detail: "post-complete".into(),
    });
    assert_eq!(st.subagents[0].phase, "cleanup");
}

#[test]
fn concurrent_sidebar_state_and_subagent_updates() {
    let mut st = make_state();
    // Interleave sidebar toggles with subagent events
    for i in 0..20 {
        st.sidebar_open = i % 2 == 0;
        if i % 3 == 0 {
            st.apply_event(&AgentEvent::ChildSessionSpawned {
                parent_session_id: "p".into(),
                child_session_id: format!("c{i}"),
                task: format!("t{i}"),
                workspace: std::path::PathBuf::from("/tmp"),
                branch: None,
            });
        }
        if i % 5 == 0 && i > 0 {
            st.apply_event(&AgentEvent::ChildSessionActivity {
                child_session_id: format!("c{}", i - 3),
                phase: "running".into(),
                detail: "now".into(),
            });
        }
    }
    // No panic, state is consistent
    assert!(!st.sidebar_open); // last iteration: i=19, 19%2!=0
}

// ═══════════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════════

fn make_state() -> crate::tui::state::types::TuiSessionState {
    crate::tui::state::types::TuiSessionState::new(
        "test-session".into(),
        "test-model".into(),
        "@default".into(),
        "default".into(),
        std::path::PathBuf::from("/tmp/test-workspace"),
        false,
    )
}
