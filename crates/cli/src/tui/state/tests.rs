use super::*;
use dcode_ai_common::event::{
    AgentEvent, InteractiveQuestionPayload, QuestionOption, QuestionSelection,
};

fn test_state() -> TuiSessionState {
    TuiSessionState::new(
        "s".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    )
}

#[test]
fn history_search_matches_filters_recent_first() {
    let mut st = test_state();
    st.composer_history_search_open = true;
    st.composer_history_search_query = "fix".into();
    let history = vec![
        "fix the bug".into(),
        "add feature".into(),
        "fix tests".into(),
    ];
    let matches = st.history_search_matches(&history);
    // Most-recent (last) first, only entries containing "fix".
    assert_eq!(matches, vec!["fix tests", "fix the bug"]);
}

#[test]
fn history_search_typing_and_backspace_update_query() {
    let mut st = test_state();
    st.composer_history_search_open = true;
    st.composer_history_search_index = 3;
    st.apply_history_search_key(HistorySearchKey::Char('a'), &[]);
    st.apply_history_search_key(HistorySearchKey::Char('b'), &[]);
    assert_eq!(st.composer_history_search_query, "ab");
    assert_eq!(st.composer_history_search_index, 0); // reset on edit
    st.apply_history_search_key(HistorySearchKey::Backspace, &[]);
    assert_eq!(st.composer_history_search_query, "a");
}

#[test]
fn history_search_down_clamps_to_matches() {
    let mut st = test_state();
    st.composer_history_search_open = true;
    let history = vec!["one".into(), "two".into()];
    // Two matches → index clamps at 1.
    for _ in 0..5 {
        st.apply_history_search_key(HistorySearchKey::Down, &history);
    }
    assert_eq!(st.composer_history_search_index, 1);
    st.apply_history_search_key(HistorySearchKey::Up, &history);
    assert_eq!(st.composer_history_search_index, 0);
}

#[test]
fn history_search_accept_sets_input_and_closes() {
    let mut st = test_state();
    st.composer_history_search_open = true;
    let history = vec!["cargo build".into(), "cargo test".into()];
    // index 0 → most recent match ("cargo test").
    st.apply_history_search_key(HistorySearchKey::Accept, &history);
    assert_eq!(st.input_buffer, "cargo test");
    assert!(!st.composer_history_search_open);
    assert!(st.composer_history_search_query.is_empty());
}

#[test]
fn history_search_cancel_closes_without_setting_input() {
    let mut st = test_state();
    st.composer_history_search_open = true;
    st.composer_history_search_query = "x".into();
    st.apply_history_search_key(HistorySearchKey::Cancel, &["x cmd".into()]);
    assert!(!st.composer_history_search_open);
    assert!(st.input_buffer.is_empty());
}

fn branch_picker_state() -> TuiSessionState {
    let mut st = test_state();
    st.branch_picker_open = true;
    st.branch_picker_branches = vec!["main".into(), "feature-login".into(), "release".into()];
    st
}

#[test]
fn branch_picker_typing_filters_and_resets_index() {
    let mut st = branch_picker_state();
    st.branch_picker_index = 2;
    assert!(
        st.apply_branch_picker_key(BranchPickerKey::Char('f'))
            .is_none()
    );
    assert_eq!(st.branch_picker_query, "f");
    assert_eq!(st.branch_picker_index, 0);
    st.apply_branch_picker_key(BranchPickerKey::Backspace);
    assert_eq!(st.branch_picker_query, "");
}

#[test]
fn branch_picker_down_clamps_to_filtered_len() {
    let mut st = branch_picker_state();
    for _ in 0..10 {
        st.apply_branch_picker_key(BranchPickerKey::Down);
    }
    assert_eq!(st.branch_picker_index, 2); // 3 branches → max index 2
    st.apply_branch_picker_key(BranchPickerKey::Up);
    assert_eq!(st.branch_picker_index, 1);
}

#[test]
fn branch_picker_accept_existing_returns_switch_and_closes() {
    let mut st = branch_picker_state();
    st.branch_picker_query = "feature-login".into();
    let cmd = st.apply_branch_picker_key(BranchPickerKey::Accept);
    assert!(matches!(cmd, Some(TuiCmd::SwitchBranch(b)) if b == "feature-login"));
    assert!(!st.branch_picker_open);
}

#[test]
fn branch_picker_accept_slash_query_returns_create() {
    let mut st = branch_picker_state();
    st.branch_picker_query = "/new-branch".into();
    let cmd = st.apply_branch_picker_key(BranchPickerKey::Accept);
    assert!(matches!(cmd, Some(TuiCmd::CreateBranch(b)) if b == "new-branch"));
}

#[test]
fn branch_picker_cancel_closes_with_no_command() {
    let mut st = branch_picker_state();
    let cmd = st.apply_branch_picker_key(BranchPickerKey::Cancel);
    assert!(cmd.is_none());
    assert!(!st.branch_picker_open);
}

fn question_state(allow_custom: bool) -> TuiSessionState {
    let mut st = test_state();
    st.active_question = Some(InteractiveQuestionPayload {
        question_id: "q1".into(),
        call_id: "c1".into(),
        prompt: "Pick one".into(),
        options: vec![
            QuestionOption {
                id: "opt-a".into(),
                label: "A".into(),
                description: None,
            },
            QuestionOption {
                id: "opt-b".into(),
                label: "B".into(),
                description: None,
            },
        ],
        allow_custom,
        suggested_answer: String::new(),
        ..Default::default()
    });
    st.question_modal_open = true;
    st.question_modal_index = 0;
    st
}

#[test]
fn question_modal_accepts_the_highlighted_option() {
    let mut st = question_state(false);
    assert_eq!(
        st.apply_question_modal_key(QuestionModalKey::Accept),
        QuestionModalOutcome::Answer {
            question_id: "q1".into(),
            selection: QuestionSelection::Option {
                option_id: "opt-a".into()
            },
        }
    );

    let mut st = question_state(false);
    assert_eq!(
        st.apply_question_modal_key(QuestionModalKey::Char('2')),
        QuestionModalOutcome::Answer {
            question_id: "q1".into(),
            selection: QuestionSelection::Option {
                option_id: "opt-b".into()
            },
        }
    );
}

#[test]
fn question_modal_opens_on_the_recommended_option() {
    let mut st = question_state(true);
    if let Some(q) = st.active_question.as_mut() {
        q.suggested_answer = "opt-b".into();
    }
    st.open_question_modal();
    assert_eq!(st.question_modal_index, 1);
}

#[test]
fn question_modal_down_clamps_to_total() {
    let mut st = question_state(true); // 2 options + "Type something." → max idx 2
    for _ in 0..10 {
        st.apply_question_modal_key(QuestionModalKey::Down);
    }
    assert_eq!(st.question_modal_index, 2);
}

#[test]
fn question_modal_typing_writes_a_custom_answer() {
    let mut st = question_state(true);
    for c in "use redis".chars() {
        st.apply_question_modal_key(QuestionModalKey::Char(c));
    }
    assert_eq!(st.question_modal_index, 2);
    st.apply_question_modal_key(QuestionModalKey::Backspace);
    assert_eq!(
        st.apply_question_modal_key(QuestionModalKey::Accept),
        QuestionModalOutcome::Answer {
            question_id: "q1".into(),
            selection: QuestionSelection::Custom {
                text: "use redi".into()
            },
        }
    );
}

#[test]
fn question_modal_multi_select_ticks_then_submits() {
    let mut st = question_state(false);
    if let Some(q) = st.active_question.as_mut() {
        q.multi_select = true;
    }
    st.apply_question_modal_key(QuestionModalKey::Char(' '));
    st.apply_question_modal_key(QuestionModalKey::Char('2'));
    st.apply_question_modal_key(QuestionModalKey::Char('2'));
    st.apply_question_modal_key(QuestionModalKey::Char('2'));
    assert_eq!(st.question_checked, vec![true, true]);
    st.apply_question_modal_key(QuestionModalKey::Down); // → Submit
    assert_eq!(
        st.apply_question_modal_key(QuestionModalKey::Accept),
        QuestionModalOutcome::Answer {
            question_id: "q1".into(),
            selection: QuestionSelection::Options {
                option_ids: vec!["opt-a".into(), "opt-b".into()]
            },
        }
    );
}

#[test]
fn question_modal_esc_keeps_the_question_for_the_composer() {
    let mut st = question_state(true);
    assert_eq!(
        st.apply_question_modal_key(QuestionModalKey::Cancel),
        QuestionModalOutcome::CloseKeepActive
    );
    assert!(!st.question_modal_open);
    assert!(st.active_question.is_some());
}

#[test]
fn connect_modal_ignores_opening_enter_then_accepts() {
    let mut st = test_state();
    st.connect_modal_open = true;
    st.connect_modal_ignore_enter_once = true;
    // First Enter is swallowed (the one that opened the modal).
    assert!(
        st.apply_connect_modal_key(ConnectModalKey::Accept)
            .is_none()
    );
    assert!(!st.connect_modal_ignore_enter_once);
    assert!(st.connect_modal_open); // still open
    // A real Enter now selects an action (the catalog always has entries).
    let action = st.apply_connect_modal_key(ConnectModalKey::Accept);
    assert!(action.is_some());
    assert!(!st.connect_modal_open);
}

#[test]
fn connect_modal_typing_updates_search() {
    let mut st = test_state();
    st.connect_modal_open = true;
    st.apply_connect_modal_key(ConnectModalKey::Char('o'));
    st.apply_connect_modal_key(ConnectModalKey::Char('p'));
    assert_eq!(st.connect_search, "op");
    st.apply_connect_modal_key(ConnectModalKey::Backspace);
    assert_eq!(st.connect_search, "o");
}

#[test]
fn pins_modal_delete_removes_and_closes_when_empty() {
    let mut st = test_state();
    st.pins_modal_open = true;
    st.pinned_notes = vec![PinnedNote {
        title: "t".into(),
        body: "b".into(),
    }];
    st.pins_modal_index = 0;
    assert!(!st.apply_pins_modal_key(PinsModalKey::Delete));
    assert!(st.pinned_notes.is_empty());
    assert!(!st.pins_modal_open); // auto-closes when last note removed
}

#[test]
fn pins_modal_copy_requests_clipboard() {
    let mut st = test_state();
    st.pins_modal_open = true;
    st.pinned_notes = vec![PinnedNote {
        title: "t".into(),
        body: "b".into(),
    }];
    assert!(st.apply_pins_modal_key(PinsModalKey::Copy)); // signals copy to the loop
    assert!(st.pins_modal_open); // copy doesn't close
}

#[test]
fn provider_picker_accept_applies_selection() {
    use dcode_ai_common::config::ProviderKind;
    let mut st = test_state();
    st.provider_picker_open = true;
    st.provider_picker_index = 0;
    let out = st.apply_provider_picker_key(ProviderPickerKey::Accept);
    assert_eq!(
        out,
        Some(ProviderPickerOutcome::Apply(ProviderKind::ALL[0]))
    );
    assert!(!st.provider_picker_open);
}

#[test]
fn provider_picker_down_wraps() {
    let mut st = test_state();
    st.provider_picker_open = true;
    let n = dcode_ai_common::config::ProviderKind::ALL.len();
    st.provider_picker_index = n - 1;
    st.apply_provider_picker_key(ProviderPickerKey::Down);
    assert_eq!(st.provider_picker_index, 0); // wraps
}

fn backtrack_test_state() -> TuiSessionState {
    let mut st = test_state();
    st.blocks = vec![
        DisplayBlock::User("first question".into()),
        DisplayBlock::Assistant("first answer".into()),
        DisplayBlock::User("second question".into()),
        DisplayBlock::Assistant("second answer".into()),
    ];
    st
}

#[test]
fn backtrack_opens_only_when_idle_with_user_messages() {
    let mut st = test_state();
    assert!(!st.open_backtrack(), "no user messages -> no backtrack");
    let mut st = backtrack_test_state();
    st.busy = true;
    assert!(!st.open_backtrack(), "busy -> no backtrack");
    st.busy = false;
    assert!(st.open_backtrack());
    assert!(st.backtrack_open);
    assert_eq!(st.backtrack_index, 0);
}

#[test]
fn backtrack_labels_are_newest_first() {
    let st = backtrack_test_state();
    let labels = st.backtrack_labels();
    assert_eq!(labels, vec!["second question", "first question"]);
}

#[test]
fn backtrack_accept_truncates_blocks_and_prefills_composer() {
    let mut st = backtrack_test_state();
    st.open_backtrack();
    // Index 0 = newest = "second question".
    assert!(st.apply_backtrack_key(BacktrackKey::Accept).is_none());
    let rewind = st.apply_rewind_scope_key(BacktrackKey::Accept).unwrap();
    assert_eq!(rewind.scope, RewindScope::Both);
    assert_eq!(rewind.user_index_from_end, 0);
    assert_eq!(rewind.text, "second question");
    assert_eq!(st.blocks.len(), 2, "blocks truncated at chosen message");
    assert_eq!(st.input_buffer, "second question");
    assert!(st.request_clear, "scrollback purge requested");
    assert!(!st.backtrack_open);
}

#[test]
fn backtrack_selects_older_message_with_down() {
    let mut st = backtrack_test_state();
    st.open_backtrack();
    st.apply_backtrack_key(BacktrackKey::Down);
    st.apply_backtrack_key(BacktrackKey::Accept);
    let rewind = st.apply_rewind_scope_key(BacktrackKey::Accept).unwrap();
    assert_eq!(rewind.user_index_from_end, 1);
    assert_eq!(rewind.text, "first question");
    assert_eq!(st.blocks.len(), 0, "everything from first message dropped");
}

#[test]
fn backtrack_cancel_leaves_state_untouched() {
    let mut st = backtrack_test_state();
    st.open_backtrack();
    assert!(st.apply_backtrack_key(BacktrackKey::Cancel).is_none());
    assert!(!st.backtrack_open);
    assert_eq!(st.blocks.len(), 4);
    assert!(st.input_buffer.is_empty());
}

#[test]
fn rewind_code_only_keeps_the_transcript() {
    let mut st = backtrack_test_state();
    st.open_backtrack();
    st.apply_backtrack_key(BacktrackKey::Accept);
    assert!(st.rewind_pending.is_some());
    st.apply_rewind_scope_key(BacktrackKey::Down);
    st.apply_rewind_scope_key(BacktrackKey::Down);
    let rewind = st.apply_rewind_scope_key(BacktrackKey::Accept).unwrap();
    assert_eq!(rewind.scope, RewindScope::Code);
    assert_eq!(st.blocks.len(), 4, "transcript untouched");
    assert!(st.input_buffer.is_empty());
    assert!(st.rewind_pending.is_none());
}

#[test]
fn session_picker_accept_returns_filtered_id() {
    let mut st = test_state();
    st.session_picker_open = true;
    st.session_picker_entries = vec![
        SessionPickerEntry {
            id: "sess-1".into(),
            label: "fix auth".into(),
            meta: String::new(),
            search_text: "fix auth".into(),
            content: String::new(),
            preview: String::new(),
        },
        SessionPickerEntry {
            id: "sess-2".into(),
            label: "add tests".into(),
            meta: String::new(),
            search_text: "add tests".into(),
            content: String::new(),
            preview: String::new(),
        },
    ];
    st.session_picker_search = "tests".into();
    st.session_picker_index = 0; // only one match → sess-2
    let id = st.apply_session_picker_key(SessionPickerKey::Accept);
    assert_eq!(id.as_deref(), Some("sess-2"));
    assert!(!st.session_picker_open);
}

#[test]
fn session_search_finds_words_said_in_the_conversation() {
    let e = SessionPickerEntry {
            id: "s1".into(),
            label: "sharepoint sync".into(),
            meta: String::new(),
            search_text: "sharepoint sync s1".into(),
            content: "User: the login keeps failing\nAssistant: The OAuth redirect URI was wrong; fixed it in auth.rs".into(),
            preview: String::new(),
        };
    assert_eq!(e.search("sharepoint"), Some(None));
    let hit = e
        .search("oauth fixed")
        .expect("content match")
        .expect("snippet");
    assert!(hit.contains("OAuth redirect"), "{hit}");
    assert_eq!(e.search("oauth kubernetes"), None);
}

#[test]
fn session_picker_cancel_closes_no_id() {
    let mut st = test_state();
    st.session_picker_open = true;
    assert!(
        st.apply_session_picker_key(SessionPickerKey::Cancel)
            .is_none()
    );
    assert!(!st.session_picker_open);
}

#[test]
fn model_picker_accept_returns_action_and_skips_headers() {
    let mut st = test_state();
    st.model_picker_open = true;
    st.model_picker_entries = vec![
        ModelPickerEntry {
            label: "Anthropic".into(),
            detail: "".into(),
            action: ModelPickerAction::SwitchProvider(
                dcode_ai_common::config::ProviderKind::Anthropic,
            ),
            is_header: true, // header → not selectable
        },
        ModelPickerEntry {
            label: "claude-sonnet-4-6".into(),
            detail: "anthropic".into(),
            action: ModelPickerAction::ApplyModel("claude-sonnet-4-6".into()),
            is_header: false,
        },
    ];
    st.model_picker_index = 0; // first *selectable* = the non-header entry
    let action = st.apply_model_picker_key(ModelPickerKey::Accept);
    assert!(matches!(action, Some(ModelPickerAction::ApplyModel(m)) if m == "claude-sonnet-4-6"));
    assert!(!st.model_picker_open);
}

#[test]
fn model_picker_search_filters_and_resets_index() {
    let mut st = test_state();
    st.model_picker_open = true;
    st.model_picker_index = 5;
    st.model_picker_entries = vec![ModelPickerEntry {
        label: "gpt-4o".into(),
        detail: "openai".into(),
        action: ModelPickerAction::ApplyModel("gpt-4o".into()),
        is_header: false,
    }];
    assert!(
        st.apply_model_picker_key(ModelPickerKey::Char('x'))
            .is_none()
    );
    assert_eq!(st.model_picker_search, "x");
    assert_eq!(st.model_picker_index, 0);
}

#[test]
fn info_modal_scroll_clamps_and_home_resets() {
    let mut st = test_state();
    st.info_modal_open = true;
    st.info_modal_lines = (0..10).map(|i| format!("line {i}")).collect();
    st.info_modal_view_rows = 4; // max_scroll = 10 - 4 = 6
    for _ in 0..50 {
        st.apply_info_modal_key(InfoModalKey::ScrollDown);
    }
    assert_eq!(st.info_modal_scroll, 6);
    st.apply_info_modal_key(InfoModalKey::ScrollUp);
    assert_eq!(st.info_modal_scroll, 5);
    st.apply_info_modal_key(InfoModalKey::Home);
    assert_eq!(st.info_modal_scroll, 0);
    assert_eq!(st.info_modal_hscroll, 0);
}

#[test]
fn info_modal_close_dismisses() {
    let mut st = test_state();
    st.info_modal_open = true;
    st.apply_info_modal_key(InfoModalKey::Close);
    assert!(!st.info_modal_open);
}

#[test]
fn command_palette_accept_loads_slash_command() {
    let mut st = test_state();
    st.command_palette_open = true;
    st.command_palette_query = "switch model".into();
    st.palette_index = 0;
    st.apply_command_palette_key(CommandPaletteKey::Accept);
    assert_eq!(st.input_buffer, "/models");
    assert!(!st.command_palette_open);
    assert!(st.command_palette_query.is_empty());
}

#[test]
fn command_palette_cancel_closes_without_input() {
    let mut st = test_state();
    st.command_palette_open = true;
    st.command_palette_query = "abc".into();
    st.palette_index = 2;
    st.apply_command_palette_key(CommandPaletteKey::Cancel);
    assert!(!st.command_palette_open);
    assert!(st.command_palette_query.is_empty());
    assert_eq!(st.palette_index, 0);
    assert!(st.input_buffer.is_empty());
}

#[test]
fn command_palette_typing_appends_and_clamps_index() {
    let mut st = test_state();
    st.command_palette_open = true;
    st.palette_index = 99;
    st.apply_command_palette_key(CommandPaletteKey::Char('z'));
    assert_eq!(st.command_palette_query, "z");
    // index clamped to the (possibly empty) filtered selectable range.
    assert!(st.palette_index < 50);
}

#[test]
fn approval_allow_pattern_falls_back_on_invalid_input() {
    let req = ApprovalRequest {
        call_id: "c1".into(),
        tool: "execute_bash".into(),
        description: "run".into(),
        input: "not valid json".into(),
    };
    // Invalid JSON → empty input → bare tool wildcard (no panic).
    assert_eq!(req.allow_pattern(), "execute_bash:*");
}

#[test]
fn question_modal_esc_depends_on_allow_custom() {
    let mut st = question_state(false);
    assert_eq!(
        st.apply_question_modal_key(QuestionModalKey::Cancel),
        QuestionModalOutcome::Stay
    );
    assert!(st.question_modal_open); // no-op when custom not allowed

    let mut st = question_state(true);
    assert_eq!(
        st.apply_question_modal_key(QuestionModalKey::Cancel),
        QuestionModalOutcome::CloseKeepActive
    );
    assert!(!st.question_modal_open);
}

#[test]
fn question_requested_sets_active_question() {
    let mut st = TuiSessionState::new(
        "session-x".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    let q = InteractiveQuestionPayload {
        question_id: "q-1".into(),
        call_id: "c1".into(),
        prompt: "Pick".into(),
        options: vec![QuestionOption {
            id: "a".into(),
            label: "A".into(),
            description: None,
        }],
        allow_custom: true,
        suggested_answer: "A".into(),
        ..Default::default()
    };
    st.apply_event(&AgentEvent::QuestionRequested {
        question: q.clone(),
    });
    assert_eq!(
        st.active_question.as_ref().map(|x| x.question_id.as_str()),
        Some("q-1")
    );
    // The live choice is the prompt box, not a transcript block.
    assert!(!matches!(st.blocks.last(), Some(DisplayBlock::Question(_))));

    st.apply_event(&AgentEvent::QuestionResolved {
        question_id: "q-1".into(),
        selection: QuestionSelection::Suggested,
    });
    assert!(st.active_question.is_none());
}

#[test]
fn child_session_activity_updates_subagent_row() {
    let mut st = TuiSessionState::new(
        "session-x".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    st.apply_event(&AgentEvent::ChildSessionSpawned {
        parent_session_id: "session-x".into(),
        child_session_id: "child-abc".into(),
        task: "do the thing".into(),
        workspace: std::path::PathBuf::from("/tmp"),
        branch: None,
    });
    assert_eq!(st.subagents.len(), 1);
    st.apply_event(&AgentEvent::ChildSessionActivity {
        child_session_id: "child-abc".into(),
        phase: "read_file".into(),
        detail: "src/lib.rs".into(),
    });
    assert_eq!(st.subagents[0].phase, "read_file");
    assert_eq!(st.subagents[0].detail, "src/lib.rs");
    let lines_before = st.blocks.len();
    st.apply_event(&AgentEvent::ChildSessionActivity {
        child_session_id: "child-abc".into(),
        phase: "search_code".into(),
        detail: "fn main".into(),
    });
    st.apply_event(&AgentEvent::ChildSessionActivity {
        child_session_id: "child-abc".into(),
        phase: "provider_request".into(),
        detail: "Starting model turn 2".into(),
    });
    assert_eq!(
        st.blocks.len(),
        lines_before,
        "activity updates one line in place"
    );
    assert!(matches!(st.blocks.last(), Some(DisplayBlock::System(s)) if s.contains("search_code")));
    assert_eq!(st.subagents[0].log.len(), 3);
}

#[test]
fn tui_state_defaults_mouse_capture_on() {
    let st = TuiSessionState::new(
        "session-x".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    assert!(st.mouse_capture_on);
}

#[test]
fn approval_requested_promotes_running_tool_with_input() {
    let mut st = TuiSessionState::new(
        "session-x".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    st.apply_event(&AgentEvent::ToolCallStarted {
        call_id: "call-1".into(),
        tool: "execute_bash".into(),
        input: serde_json::json!({"command":"ls -la"}),
    });
    st.apply_event(&AgentEvent::ApprovalRequested {
        call_id: "call-1".into(),
        tool: "execute_bash".into(),
        description: "Tool `execute_bash` requires approval".into(),
    });

    assert!(st.active_approval.is_some());
    let req = st
        .blocks
        .iter()
        .rev()
        .find_map(|block| match block {
            DisplayBlock::ApprovalPending(req) if req.call_id == "call-1" => Some(req),
            _ => None,
        })
        .expect("expected approval block for call-1");
    assert_eq!(req.tool, "execute_bash");
    assert!(req.input.contains("command"));
    assert!(req.input.contains("ls -la"));
}

#[test]
fn replayed_rewind_drops_rewound_turns_but_live_does_not() {
    let mut st = TuiSessionState::new(
        "session-x".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    for (role, text) in [
        ("user", "one"),
        ("assistant", "a1"),
        ("user", "two"),
        ("assistant", "a2"),
    ] {
        st.apply_event(&AgentEvent::MessageReceived {
            role: role.into(),
            content: text.into(),
        });
    }
    let rewound = AgentEvent::ConversationRewound {
        user_messages_dropped: 1,
    };
    let before = st.blocks.len();
    st.apply_event(&rewound);
    assert_eq!(
        st.blocks.len(),
        before,
        "live view already truncated itself"
    );

    st.replaying = true;
    st.apply_event(&rewound);
    let users = st
        .blocks
        .iter()
        .filter(|b| matches!(b, DisplayBlock::User(_)))
        .count();
    assert_eq!(users, 1);
    assert!(
        !st.blocks
            .iter()
            .any(|b| matches!(b, DisplayBlock::Assistant(t) if t == "a2"))
    );
}

#[test]
fn approved_tool_keeps_its_name_and_input_when_done() {
    let mut st = TuiSessionState::new(
        "session-x".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    st.apply_event(&AgentEvent::ToolCallStarted {
        call_id: "call-1".into(),
        tool: "edit_file".into(),
        input: serde_json::json!({"path":"a.txt"}),
    });
    st.apply_event(&AgentEvent::ApprovalRequested {
        call_id: "call-1".into(),
        tool: "edit_file".into(),
        description: "edit".into(),
    });
    st.apply_event(&AgentEvent::ApprovalResolved {
        call_id: "call-1".into(),
        approved: true,
    });
    st.apply_event(&AgentEvent::ToolCallCompleted {
        call_id: "call-1".into(),
        output: dcode_ai_common::tool::ToolResult {
            call_id: "call-1".into(),
            success: true,
            output: "Edited a.txt".into(),
            error: None,
        },
    });
    let done: Vec<_> = st
        .blocks
        .iter()
        .filter_map(|b| match b {
            DisplayBlock::ToolDone { name, input, .. } => Some((name.clone(), input.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].0, "edit_file");
    assert!(done[0].1.contains("a.txt"));
    assert!(
        !st.blocks
            .iter()
            .any(|b| matches!(b, DisplayBlock::ApprovalResolved { .. })),
        "an approval is not narrated"
    );
}

#[test]
fn tool_events_render_as_one_block_without_activity_lines() {
    let mut st = TuiSessionState::new(
        "session-x".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp/repo"),
        false,
    );
    st.apply_event(&AgentEvent::ToolCallStarted {
        call_id: "call-1".into(),
        tool: "web_search".into(),
        input: serde_json::json!({"query":"tokio tutorial"}),
    });
    st.apply_event(&AgentEvent::ToolCallCompleted {
        call_id: "call-1".into(),
        output: dcode_ai_common::tool::ToolResult {
            call_id: "call-1".into(),
            success: true,
            output: "- result".into(),
            error: None,
        },
    });

    // Claude-style: the tool block carries everything; no extra
    // "Using web context…" narration lines.
    assert!(
        !st.blocks
            .iter()
            .any(|block| matches!(block, DisplayBlock::System(_)))
    );
    assert!(st.blocks.iter().any(|block| matches!(
        block,
        DisplayBlock::ToolDone { input, ok: true, .. } if input.contains("tokio tutorial")
    )));
}

#[test]
fn clear_replayed_interaction_state_drops_stale_prompts() {
    let mut st = TuiSessionState::new(
        "session-x".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    st.active_approval = Some(ApprovalRequest {
        call_id: "call-1".into(),
        tool: "execute_bash".into(),
        description: "approve".into(),
        input: "{}".into(),
    });
    st.active_question = Some(InteractiveQuestionPayload {
        question_id: "q-1".into(),
        call_id: "call-2".into(),
        prompt: "Pick".into(),
        options: vec![],
        allow_custom: true,
        suggested_answer: String::new(),
        ..Default::default()
    });

    st.push_block(DisplayBlock::ApprovalPending(ApprovalRequest {
        call_id: "call-1".into(),
        tool: "execute_bash".into(),
        description: "approve".into(),
        input: "{}".into(),
    }));

    st.clear_replayed_interaction_state();

    assert!(st.active_approval.is_none());
    assert!(st.active_question.is_none());
    assert!(matches!(
        st.blocks.last(),
        Some(DisplayBlock::ToolDone { ok: false, detail, .. }) if detail.starts_with("Not answered")
    ));
}

#[test]
fn open_close_question_modal() {
    let mut st = TuiSessionState::new(
        "s".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    assert!(!st.question_modal_open);
    assert_eq!(st.question_modal_index, 0);

    st.open_question_modal();
    assert!(st.question_modal_open);
    assert_eq!(st.question_modal_index, 0);
    assert_eq!(st.question_modal_scroll, 0);

    st.question_modal_index = 3;
    st.close_question_modal();
    assert!(!st.question_modal_open);
    assert_eq!(st.question_modal_index, 0);
    assert_eq!(st.question_modal_scroll, 0);
}

#[test]
fn question_requested_opens_modal() {
    let mut st = TuiSessionState::new(
        "s".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    let q = InteractiveQuestionPayload {
        question_id: "q-1".into(),
        call_id: "c1".into(),
        prompt: "Pick".into(),
        options: vec![QuestionOption {
            id: "a".into(),
            label: "A".into(),
            description: None,
        }],
        allow_custom: true,
        suggested_answer: "A".into(),
        ..Default::default()
    };
    st.apply_event(&AgentEvent::QuestionRequested {
        question: q.clone(),
    });
    assert!(st.question_modal_open);
    assert_eq!(st.question_modal_index, 0);
    assert!(st.active_question.is_some());
}

#[test]
fn question_resolved_closes_modal() {
    let mut st = TuiSessionState::new(
        "s".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    st.question_modal_open = true;
    st.question_modal_index = 2;
    st.apply_event(&AgentEvent::QuestionResolved {
        question_id: "q-1".into(),
        selection: QuestionSelection::Suggested,
    });
    assert!(st.active_question.is_none());
    assert!(!st.question_modal_open);
    assert_eq!(st.question_modal_index, 0);
}

#[test]
fn a_tool_left_running_when_the_turn_ends_is_stopped() {
    let mut st = test_state();
    st.blocks.push(DisplayBlock::ToolRunning {
        name: "execute_bash".into(),
        input: "{}".into(),
        output: String::new(),
        call_id: "c1".into(),
    });
    st.blocks
        .push(DisplayBlock::Assistant("a long reply".into()));
    st.apply_event(&AgentEvent::BusyStateChanged {
        state: BusyState::Idle,
    });
    // Nothing still "running" holds the reply back from the scrollback.
    assert!(
        !st.blocks
            .iter()
            .any(|b| matches!(b, DisplayBlock::ToolRunning { .. }))
    );
    assert!(matches!(
        st.blocks[st.blocks.len() - 2],
        DisplayBlock::ToolDone { ok: false, .. }
    ));
}

#[test]
fn replay_of_an_unfinished_turn_reopens_idle() {
    let mut st = TuiSessionState::new(
        "s".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    st.replaying = true;
    st.apply_event(&AgentEvent::MessageReceived {
        role: "user".into(),
        content: "fix it".into(),
    });
    st.blocks.push(DisplayBlock::ToolRunning {
        name: "execute_bash".into(),
        input: "{}".into(),
        output: String::new(),
        call_id: "c1".into(),
    });
    st.replaying = false;
    st.clear_replayed_interaction_state();
    assert!(!st.busy);
    assert!(!st.current_busy_state.is_active());
    assert!(matches!(
        st.blocks.last(),
        Some(DisplayBlock::ToolDone { ok: false, .. })
    ));
}

#[test]
fn background_job_end_queues_a_notification() {
    use dcode_ai_common::event::BackgroundJobStatus as S;
    let mut st = TuiSessionState::new(
        "s".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
        false,
    );
    let ev = |status, exit_code| AgentEvent::BackgroundJob {
        job_id: 2,
        command: "cargo test".into(),
        status,
        exit_code,
        output_tail: "test result: ok".into(),
    };
    st.apply_event(&ev(S::Running, None));
    assert_eq!(st.background_shells.len(), 1);
    assert!(st.pending_job_notifications.is_empty());

    st.apply_event(&ev(S::Completed, Some(0)));
    assert!(st.background_shells.is_empty());
    assert!(matches!(
        st.blocks.last(),
        Some(DisplayBlock::Notice { text, ok: true })
            if text == "Background command \"cargo test\" completed (exit code 0)"
    ));
    let note = &st.pending_job_notifications[0];
    assert!(note.starts_with("<task-notification>") && note.contains("test result: ok"));

    // Replayed history shows the line but never re-notifies the agent.
    st.pending_job_notifications.clear();
    st.replaying = true;
    st.apply_event(&ev(S::Failed, Some(1)));
    assert!(st.pending_job_notifications.is_empty());
}
