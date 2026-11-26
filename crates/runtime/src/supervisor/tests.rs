use super::*;
use chrono::{Duration, Utc};

#[test]
fn keep_warm_goes_out_just_before_the_cache_expires() {
    let s = std::time::Duration::from_secs;
    assert!(!keep_warm_window_open(s(200)));
    assert!(keep_warm_window_open(s(270)));
    assert!(keep_warm_window_open(s(290)));
    // Past five minutes the entry is gone: a refresh would be a full write.
    assert!(!keep_warm_window_open(s(300)));
}

#[test]
fn idle_compaction_runs_only_while_the_cache_is_warm() {
    let s = std::time::Duration::from_secs;
    assert!(!idle_window_open(s(60), 240));
    assert!(idle_window_open(s(240), 240));
    assert!(idle_window_open(s(359), 240));
    // Past it, the cache has expired: compacting saves nothing.
    assert!(!idle_window_open(s(360), 240));
}

#[test]
fn person_names_are_cleaned() {
    assert_eq!(clean_person_name("Priya <script>"), "Priya script");
    assert_eq!(clean_person_name("   "), "guest");
    assert_eq!(clean_person_name(&"a".repeat(40)).len(), 24);
}

#[test]
fn memory_section_lists_user_then_project_and_adds_auto_guidance() {
    let note = |kind: &str, content: &str| crate::memory_store::MemoryNote {
        id: content.into(),
        created_at: Utc::now(),
        kind: kind.into(),
        title: None,
        content: content.into(),
    };
    let user = [note("preference", "prefers terse answers")];
    let project = [note("convention", "run make test")];
    let s = render_memory_section(&user, &project, true).unwrap();
    let (u, p) = (s.find("terse").unwrap(), s.find("make test").unwrap());
    assert!(u < p);
    assert!(s.contains("## Auto memory"));
    assert!(render_memory_section(&[], &[], false).is_none());
    assert!(
        render_memory_section(&[], &[], true)
            .unwrap()
            .contains("save_memory")
    );
}

use dcode_ai_common::event::AgentCommand;
use dcode_ai_common::message::Message;
use dcode_ai_common::session::{SessionMeta, SessionState, SessionStatus};
use std::fs;

fn write_session_for_test(
    workspace: &std::path::Path,
    id: &str,
    updated_at: chrono::DateTime<Utc>,
    model: &str,
    status: SessionStatus,
) {
    let sessions_dir = workspace.join(".dcode-ai").join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let session = SessionState {
        meta: SessionMeta {
            id: id.to_string(),
            session_name: None,
            created_at: updated_at - Duration::minutes(1),
            updated_at,
            workspace: workspace.to_path_buf(),
            model: model.to_string(),
            provider: None,
            status,
            pid: None,
            socket_path: None,
            worktree_path: None,
            branch: None,
            base_branch: None,
            parent_session_id: None,
            child_session_ids: Vec::new(),
            inherited_summary: None,
            spawn_reason: None,
            session_summary: None,
            orchestration: None,
            goal: None,
        },
        messages: vec![Message::user("hello")],
        total_input_tokens: 0,
        total_output_tokens: 0,
        estimated_cost_usd: 0.0,
    };

    let json = serde_json::to_string_pretty(&session).expect("serialize session");
    fs::write(sessions_dir.join(format!("{id}.json")), json).expect("write session");
}

#[tokio::test]
async fn get_last_session_id_falls_back_to_most_recent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path();
    let now = Utc::now();

    // Write sessions WITHOUT .last_session file
    write_session_for_test(
        workspace,
        "session-oldest",
        now - Duration::minutes(10),
        "claude-3-7-sonnet-latest",
        SessionStatus::Completed,
    );
    write_session_for_test(
        workspace,
        "session-middle",
        now - Duration::minutes(5),
        "claude-3-7-sonnet-latest",
        SessionStatus::Completed,
    );
    write_session_for_test(
        workspace,
        "session-newest",
        now,
        "claude-3-7-sonnet-latest",
        SessionStatus::Running,
    );

    let config = dcode_ai_common::config::DcodeAiConfig::default();
    let session_id = get_last_session_id(&config, workspace)
        .await
        .expect("get_last_session_id should succeed")
        .expect("should find a session");

    // Should find the most recent session
    assert_eq!(session_id, "session-newest");

    // The .last_session file should now be updated
    let last_session_path = workspace.join(".dcode-ai").join(".last_session");
    assert!(
        last_session_path.exists(),
        ".last_session should be created"
    );
    let content = std::fs::read_to_string(&last_session_path).unwrap();
    assert_eq!(content.trim(), "session-newest");
}

/// Release gate for compaction (M31): a long tool-heavy session on a
/// 200k window where every request carries a fixed part (system prompt,
/// tools) and the provider keeps extra it counts (thinking) that grows
/// until the history is rewritten. The real supervisor and agent run it
/// against a scripted provider. Old tool output must be cleared before
/// the first compaction, compactions must be rare and far apart, there
/// is at most one summary, the recent conversation survives, and the
/// burn alarm never pauses the session.
#[tokio::test]
async fn long_session_with_big_fixed_prompt_stays_healthy() {
    use dcode_ai_core::provider::{Provider, StreamChunk};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Reads a new file each step for `steps_per_turn` steps, then answers;
    /// tool-less calls are summaries (or titles).
    struct Scripted {
        steps_per_turn: usize,
        step: AtomicUsize,
        file: AtomicUsize,
        /// Steps since the history was last summarized (the provider's
        /// kept thinking grows with them).
        since_summary: AtomicUsize,
        compacted_at: Arc<std::sync::Mutex<Vec<usize>>>,
    }

    #[async_trait::async_trait]
    impl Provider for Scripted {
        async fn chat(
            &self,
            messages: &[Message],
            tools: &[dcode_ai_common::tool::ToolDefinition],
            _model: &str,
            _root: &std::path::Path,
        ) -> Result<mpsc::Receiver<StreamChunk>, ProviderError> {
            let (tx, rx) = mpsc::channel(8);
            let mut chunks = Vec::new();
            if tools.is_empty() {
                let prompt = messages
                    .last()
                    .map(|m| m.content.to_summary_text())
                    .unwrap_or_default();
                if prompt.contains("context compaction") {
                    let at = self.file.load(Ordering::SeqCst);
                    self.compacted_at.lock().unwrap().push(at);
                    self.since_summary.store(0, Ordering::SeqCst);
                }
                chunks.push(StreamChunk::TextDelta("SUMMARY of earlier work".into()));
            } else {
                if messages.last().is_some_and(|m| m.role == Role::User) {
                    self.step.store(0, Ordering::SeqCst);
                }
                let step = self.step.fetch_add(1, Ordering::SeqCst);
                let extra = self.since_summary.fetch_add(1, Ordering::SeqCst) * 300;
                let chars: usize = messages.iter().map(|m| m.content.approx_chars()).sum();
                chunks.push(StreamChunk::Usage {
                    input_tokens: (chars / 4 + 15_000 + extra) as u64,
                    output_tokens: 50,
                });
                if step < self.steps_per_turn {
                    let n = self.file.fetch_add(1, Ordering::SeqCst);
                    chunks.push(StreamChunk::ToolUse(dcode_ai_common::tool::ToolCall {
                        id: format!("call-{n}"),
                        name: "read_file".into(),
                        input: json!({"path": format!("f{n}.txt")}),
                    }));
                } else {
                    chunks.push(StreamChunk::TextDelta("done".into()));
                }
            }
            chunks.push(StreamChunk::Done);
            tokio::spawn(async move {
                for c in chunks {
                    let _ = tx.send(c).await;
                }
            });
            Ok(rx)
        }
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().canonicalize().expect("canonicalize workspace");
    let (turns, steps_per_turn) = (16usize, 25usize);
    for n in 0..turns * steps_per_turn {
        std::fs::write(
            workspace.join(format!("f{n}.txt")),
            format!("file {n}\n{}", "some content here ".repeat(400)),
        )
        .unwrap();
    }

    let mut config = dcode_ai_common::config::DcodeAiConfig::default();
    config.provider.default = dcode_ai_common::config::ProviderKind::Anthropic;
    config.provider.anthropic.backend = dcode_ai_common::config::AnthropicBackend::Api;
    config.provider.anthropic.api_key = Some("sk-ant-test-key".into());
    config.memory.context.auto_detect_context_window = false;
    config.memory.context.query_provider_models_api = false;
    config.memory.context.context_window_target = 200_000;

    let mut sup = Supervisor::create(SupervisorConfig {
        config,
        workspace_root: workspace.clone(),
        safe_mode: false,
        interactive_approvals: false,
        session_id: Some("session-long".into()),
        approval_handler: None,
        orchestration_context: None,
    })
    .await
    .expect("create supervisor");
    let mut events = sup.take_handle().take_event_rx().expect("events");
    let notes: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let sink = notes.clone();
    tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            if let AgentEvent::ContextCompaction { message, .. } = ev {
                sink.lock().unwrap().push(message);
            }
        }
    });
    let compacted_at: Arc<std::sync::Mutex<Vec<usize>>> = Default::default();
    sup.agent_mut().replace_provider(Box::new(Scripted {
        steps_per_turn,
        step: AtomicUsize::new(0),
        file: AtomicUsize::new(0),
        since_summary: AtomicUsize::new(0),
        compacted_at: compacted_at.clone(),
    }));

    for turn in 0..turns {
        let reply = sup.run_turn(&format!("task {turn}")).await.expect("turn");
        assert_eq!(reply, "done", "turn {turn} paused or failed: {reply}");
        let messages = &sup.agent().messages;
        let stacked = messages
            .iter()
            .filter(|m| crate::context_manager::is_summary(m))
            .count();
        assert!(stacked <= 1, "turn {turn}: {stacked} summaries");
        assert!(
            messages.iter().any(|m| m.role == Role::Tool),
            "turn {turn}: the recent conversation was dropped"
        );
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let at = compacted_at.lock().unwrap().clone();
    let notes = notes.lock().unwrap().clone();
    let first_prune = notes
        .iter()
        .position(|n| n.starts_with("Cleared old tool output"));
    let first_compaction = notes.iter().position(|n| n.starts_with("Compacting"));
    // 400 steps of ~2k tokens each into a 200k window: a handful of
    // compactions, each after the conversation grew a long way.
    assert!(!at.is_empty(), "a session this long compacts: {notes:?}");
    assert!(at.len() <= 10, "{} compactions at steps {at:?}", at.len());
    assert!(
        at.windows(2).all(|w| w[1] - w[0] >= 20),
        "compactions too close together (steps): {at:?}"
    );
    let prunes = notes
        .iter()
        .filter(|n| n.starts_with("Cleared old tool output"))
        .count();
    assert!(
        prunes <= at.len() + 1,
        "clearing runs at most once between compactions: {notes:?}"
    );
    assert!(
        first_prune.is_some() && first_prune < first_compaction,
        "old tool output is cleared before the first compaction: {notes:?}"
    );
    assert!(
        !notes.iter().any(|n| n.contains("auto-compact paused")),
        "{notes:?}"
    );
}

#[tokio::test]
async fn empty_start_and_exit_session_is_not_persisted() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().canonicalize().expect("canonicalize workspace");

    let mut config = dcode_ai_common::config::DcodeAiConfig::default();
    config.provider.default = dcode_ai_common::config::ProviderKind::Anthropic;
    config.provider.anthropic.backend = dcode_ai_common::config::AnthropicBackend::Api;
    config.provider.anthropic.api_key = Some("sk-ant-test-key".into());
    config.memory.context.auto_detect_context_window = false;
    config.memory.context.query_provider_models_api = false;

    let mut sup = Supervisor::create(SupervisorConfig {
        config: config.clone(),
        workspace_root: workspace.clone(),
        safe_mode: false,
        interactive_approvals: false,
        session_id: Some("session-empty-exit".into()),
        approval_handler: None,
        orchestration_context: None,
    })
    .await
    .expect("create supervisor");

    sup.finish(EndReason::UserExit).await;

    let store = SessionStore::new(workspace.join(&config.session.history_dir));
    let ids = store.list().await.expect("list sessions");
    assert!(
        !ids.iter().any(|id| id == "session-empty-exit"),
        "no-op session should not be persisted"
    );
}

#[tokio::test]
async fn cleanup_empty_sessions_removes_only_empty_non_running_sessions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().canonicalize().expect("canonicalize workspace");
    let sessions_dir = workspace.join(".dcode-ai").join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let store = SessionStore::new(&sessions_dir);
    let now = Utc::now();

    let empty_completed = SessionState {
        meta: SessionMeta {
            id: "empty-completed".into(),
            session_name: None,
            created_at: now,
            updated_at: now,
            workspace: workspace.clone(),
            model: "gpt-5.2".into(),
            provider: None,
            status: SessionStatus::Completed,
            pid: None,
            socket_path: None,
            worktree_path: None,
            branch: None,
            base_branch: None,
            parent_session_id: None,
            child_session_ids: Vec::new(),
            inherited_summary: None,
            spawn_reason: None,
            session_summary: None,
            orchestration: None,
            goal: None,
        },
        messages: vec![Message::system("You are dcode-ai")],
        total_input_tokens: 0,
        total_output_tokens: 0,
        estimated_cost_usd: 0.0,
    };
    store.save(&empty_completed).await.expect("save empty");

    let meaningful = SessionState {
        meta: SessionMeta {
            id: "non-empty".into(),
            session_name: None,
            created_at: now,
            updated_at: now,
            workspace: workspace.clone(),
            model: "gpt-5.2".into(),
            provider: None,
            status: SessionStatus::Completed,
            pid: None,
            socket_path: None,
            worktree_path: None,
            branch: None,
            base_branch: None,
            parent_session_id: None,
            child_session_ids: Vec::new(),
            inherited_summary: None,
            spawn_reason: None,
            session_summary: None,
            orchestration: None,
            goal: None,
        },
        messages: vec![Message::user("hello")],
        total_input_tokens: 0,
        total_output_tokens: 0,
        estimated_cost_usd: 0.0,
    };
    store.save(&meaningful).await.expect("save meaningful");

    let running_empty = SessionState {
        meta: SessionMeta {
            id: "running-empty".into(),
            session_name: None,
            created_at: now,
            updated_at: now,
            workspace: workspace.clone(),
            model: "gpt-5.2".into(),
            provider: None,
            status: SessionStatus::Running,
            pid: None,
            socket_path: None,
            worktree_path: None,
            branch: None,
            base_branch: None,
            parent_session_id: None,
            child_session_ids: Vec::new(),
            inherited_summary: None,
            spawn_reason: None,
            session_summary: None,
            orchestration: None,
            goal: None,
        },
        messages: vec![],
        total_input_tokens: 0,
        total_output_tokens: 0,
        estimated_cost_usd: 0.0,
    };
    store
        .save(&running_empty)
        .await
        .expect("save running empty");

    let deleted = cleanup_empty_sessions(&store)
        .await
        .expect("cleanup empty sessions");
    assert_eq!(deleted.len(), 1);
    assert!(deleted.iter().any(|id| id == "empty-completed"));

    let remaining = store.list().await.expect("list remaining");
    assert!(remaining.iter().any(|id| id == "non-empty"));
    assert!(remaining.iter().any(|id| id == "running-empty"));
}

#[tokio::test]
async fn resume_preserves_existing_session_messages_instead_of_overwriting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().canonicalize().expect("canonicalize workspace");
    let sessions_dir = workspace.join(".dcode-ai").join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let session_id = "resume-target";
    let now = Utc::now();
    let session = SessionState {
        meta: SessionMeta {
            id: session_id.to_string(),
            session_name: Some("resume target".into()),
            created_at: now - Duration::minutes(5),
            updated_at: now - Duration::minutes(1),
            workspace: workspace.clone(),
            model: "gpt-5.2".to_string(),
            provider: None,
            status: SessionStatus::Completed,
            pid: None,
            socket_path: None,
            worktree_path: None,
            branch: None,
            base_branch: None,
            parent_session_id: None,
            child_session_ids: Vec::new(),
            inherited_summary: None,
            spawn_reason: None,
            session_summary: None,
            orchestration: None,
            goal: None,
        },
        messages: vec![
            Message::system("You are dcode-ai"),
            Message::user("hello"),
            Message::assistant("world"),
        ],
        total_input_tokens: 0,
        total_output_tokens: 0,
        estimated_cost_usd: 0.0,
    };
    std::fs::write(
        sessions_dir.join(format!("{session_id}.json")),
        serde_json::to_string_pretty(&session).expect("serialize session"),
    )
    .expect("write session");

    let mut config = dcode_ai_common::config::DcodeAiConfig::default();
    config.provider.default = dcode_ai_common::config::ProviderKind::Anthropic;
    config.provider.anthropic.backend = dcode_ai_common::config::AnthropicBackend::Api;
    config.provider.anthropic.api_key = Some("sk-ant-test-key".into());
    config.memory.context.auto_detect_context_window = false;

    let sup = Supervisor::resume(config, &workspace, false, false, session_id, None)
        .await
        .expect("resume should load existing session");

    let previews: Vec<String> = sup
        .agent
        .messages
        .iter()
        .map(|m| m.content.event_preview())
        .collect();
    assert!(
        previews.iter().any(|m| m == "world"),
        "expected resumed messages to include prior assistant reply, got: {previews:?}"
    );

    let store = SessionStore::new(sessions_dir);
    let persisted = store
        .load(session_id)
        .await
        .expect("load persisted session");
    let persisted_previews: Vec<String> = persisted
        .messages
        .iter()
        .map(|m| m.content.event_preview())
        .collect();
    assert!(
        persisted_previews.iter().any(|m| m == "world"),
        "resume must not overwrite persisted session before loading"
    );
}

#[tokio::test]
async fn resume_recovers_context_from_event_log_when_snapshot_has_no_turns() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().canonicalize().expect("canonicalize workspace");
    let sessions_dir = workspace.join(".dcode-ai").join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let session_id = "resume-recover";
    let now = Utc::now();
    let session = SessionState {
        meta: SessionMeta {
            id: session_id.to_string(),
            session_name: Some("resume recover".into()),
            created_at: now - Duration::minutes(5),
            updated_at: now - Duration::minutes(1),
            workspace: workspace.clone(),
            model: "gpt-5.2".to_string(),
            provider: None,
            status: SessionStatus::Completed,
            pid: None,
            socket_path: None,
            worktree_path: None,
            branch: None,
            base_branch: None,
            parent_session_id: None,
            child_session_ids: Vec::new(),
            inherited_summary: None,
            spawn_reason: None,
            session_summary: None,
            orchestration: None,
            goal: None,
        },
        messages: vec![Message::system("You are dcode-ai")],
        total_input_tokens: 0,
        total_output_tokens: 0,
        estimated_cost_usd: 0.0,
    };
    std::fs::write(
        sessions_dir.join(format!("{session_id}.json")),
        serde_json::to_string_pretty(&session).expect("serialize session"),
    )
    .expect("write session");

    let log_path = sessions_dir.join(format!("{session_id}.events.jsonl"));
    let user = EventEnvelope::new(
        1,
        AgentEvent::MessageReceived {
            role: "user".into(),
            content: "hello".into(),
        },
    );
    let assistant = EventEnvelope::new(
        2,
        AgentEvent::MessageReceived {
            role: "assistant".into(),
            content: "world".into(),
        },
    );
    let mut lines = String::new();
    lines.push_str(&serde_json::to_string(&user).expect("serialize user event"));
    lines.push('\n');
    lines.push_str(&serde_json::to_string(&assistant).expect("serialize assistant event"));
    lines.push('\n');
    std::fs::write(&log_path, lines).expect("write event log");

    let mut config = dcode_ai_common::config::DcodeAiConfig::default();
    config.provider.default = dcode_ai_common::config::ProviderKind::Anthropic;
    config.provider.anthropic.backend = dcode_ai_common::config::AnthropicBackend::Api;
    config.provider.anthropic.api_key = Some("sk-ant-test-key".into());
    config.memory.context.auto_detect_context_window = false;

    let sup = Supervisor::resume(config, &workspace, false, false, session_id, None)
        .await
        .expect("resume should recover context from event log");

    let previews: Vec<String> = sup
        .agent
        .messages
        .iter()
        .map(|m| m.content.event_preview())
        .collect();
    assert!(
        previews.iter().any(|m| m == "hello"),
        "expected recovered user turn, got: {previews:?}"
    );
    assert!(
        previews.iter().any(|m| m == "world"),
        "expected recovered assistant turn, got: {previews:?}"
    );
}

#[tokio::test]
async fn send_message_forwards_prompt_to_session_queue() {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();

    let task = spawn_command_consumer_with_store(
        cmd_rx,
        None,
        None,
        None,
        None,
        Some(prompt_tx),
        Some(control_tx),
    );

    cmd_tx
        .send(AgentCommand::SendMessage {
            content: "hello from ipc".into(),
        })
        .unwrap();

    let received = tokio::time::timeout(std::time::Duration::from_secs(1), prompt_rx.recv())
        .await
        .expect("prompt should be forwarded")
        .expect("prompt channel should remain open");
    assert_eq!(received, "hello from ipc");

    let _ = cmd_tx.send(AgentCommand::Shutdown);
    task.abort();
}

#[tokio::test]
async fn joined_people_are_announced_and_named_to_the_agent() {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = mpsc::channel(16);
    let task = spawn_command_consumer_with_store(
        cmd_rx,
        None,
        None,
        None,
        Some(event_tx),
        Some(prompt_tx),
        None,
    );
    let next = async |rx: &mut mpsc::Receiver<AgentEvent>| {
        tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("event")
            .expect("open")
    };
    cmd_tx
        .send(AgentCommand::Hello {
            person: "p1".into(),
            name: "Priya<b>".into(),
        })
        .unwrap();
    assert!(matches!(
        next(&mut event_rx).await,
        AgentEvent::PersonJoined { ref name, ref role, .. } if name == "Priyab" && role == "join"
    ));
    cmd_tx
        .send(AgentCommand::Say {
            person: "p1".into(),
            content: "try staging".into(),
            name: None,
        })
        .unwrap();
    assert!(matches!(
        next(&mut event_rx).await,
        AgentEvent::PersonSaid { ref name, ref text, .. } if name == "Priyab" && text == "try staging"
    ));
    let prompt = tokio::time::timeout(std::time::Duration::from_secs(1), prompt_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prompt, "(from Priyab) try staging");
    cmd_tx
        .send(AgentCommand::Bye {
            person: "p1".into(),
        })
        .unwrap();
    assert!(matches!(
        next(&mut event_rx).await,
        AgentEvent::PersonLeft { ref name, .. } if name == "Priyab"
    ));
    task.abort();
}

#[tokio::test]
async fn chat_and_todos_are_events_not_prompts() {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = mpsc::channel(16);
    let task = spawn_command_consumer_with_store(
        cmd_rx,
        None,
        None,
        None,
        Some(event_tx),
        Some(prompt_tx),
        None,
    );
    cmd_tx
        .send(AgentCommand::Chat {
            person: "w".into(),
            text: "lunch?".into(),
            name: Some("Ravi".into()),
            image: None,
        })
        .unwrap();
    // A picture alone is a message; one of the wrong kind is dropped.
    let pic = dcode_ai_common::event::ChatImage {
        name: "bug.png".into(),
        mime: "image/png".into(),
        w: 4,
        h: 4,
        data: "iVBORw0KGgo=".into(),
    };
    cmd_tx
        .send(AgentCommand::Chat {
            person: "w".into(),
            text: String::new(),
            name: Some("Ravi".into()),
            image: Some(pic.clone()),
        })
        .unwrap();
    cmd_tx
        .send(AgentCommand::Chat {
            person: "w".into(),
            text: String::new(),
            name: Some("Ravi".into()),
            image: Some(dcode_ai_common::event::ChatImage {
                mime: "text/html".into(),
                ..pic.clone()
            }),
        })
        .unwrap();
    cmd_tx
        .send(AgentCommand::TodoAdd {
            person: "w".into(),
            text: "fix login".into(),
            name: Some("Ravi".into()),
        })
        .unwrap();
    let next = async |rx: &mut mpsc::Receiver<AgentEvent>| {
        tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap()
    };
    assert!(matches!(next(&mut event_rx).await,
            AgentEvent::PersonChat { ref name, ref text, .. } if name == "Ravi" && text == "lunch?"));
    assert!(matches!(next(&mut event_rx).await,
            AgentEvent::PersonChat { ref image, .. } if image.as_ref() == Some(&pic)));
    let AgentEvent::TeamTodoAdded { id, text, by } = next(&mut event_rx).await else {
        panic!("expected a to-do");
    };
    assert_eq!((text.as_str(), by.as_str()), ("fix login", "Ravi"));
    cmd_tx
        .send(AgentCommand::TodoDone {
            person: "w".into(),
            id: id.clone(),
            name: Some("Ravi".into()),
        })
        .unwrap();
    assert!(matches!(next(&mut event_rx).await,
            AgentEvent::TeamTodoDone { id: ref done, .. } if *done == id));
    // None of it went to the agent.
    assert!(prompt_rx.try_recv().is_err());
    task.abort();
}

#[tokio::test]
async fn answer_question_resolves_pending_channel() {
    use dcode_ai_common::event::QuestionSelection;

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<QuestionSelection>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = oneshot::channel();
    pending.lock().unwrap().insert("q-1".into(), tx);

    let task = spawn_command_consumer_with_store(
        cmd_rx,
        None,
        Some(pending.clone()),
        None,
        None,
        None,
        None,
    );

    cmd_tx
        .send(AgentCommand::AnswerQuestion {
            question_id: "q-1".into(),
            selection: QuestionSelection::Suggested,
        })
        .unwrap();

    let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx)
        .await
        .expect("timeout")
        .expect("channel");
    assert!(matches!(got, QuestionSelection::Suggested));

    let _ = cmd_tx.send(AgentCommand::Shutdown);
    task.abort();
}

#[test]
fn compaction_fallback_summary_uses_default_when_empty() {
    let summary = build_compaction_fallback_summary(&[]);
    assert_eq!(
        summary,
        "Earlier conversation context was compacted due to token limits."
    );
}

#[test]
fn compaction_fallback_summary_prefers_message_summary() {
    let summary = build_compaction_fallback_summary(&[
        Message::user("Need to fix context overflow"),
        Message::assistant("Auto-compact before the next model request."),
    ]);
    assert!(summary.contains("Need to fix context overflow"));
    assert!(summary.contains("Auto-compact before the next model request."));
}

// ─── Session Prune Tests ───────────────────────────────────────────────────

#[tokio::test]
async fn prune_sessions_dry_run_returns_candidates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path();
    let sessions_dir = workspace.join(".dcode-ai").join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let store = SessionStore::new(&sessions_dir);
    let now = Utc::now();

    // Write sessions: 2 recent (keep), 2 old (delete)
    write_session_for_test(
        workspace,
        "session-recent-1",
        now - Duration::hours(1),
        "test",
        SessionStatus::Completed,
    );
    write_session_for_test(
        workspace,
        "session-recent-2",
        now - Duration::hours(2),
        "test",
        SessionStatus::Completed,
    );
    write_session_for_test(
        workspace,
        "session-old-1",
        now - Duration::days(30),
        "test",
        SessionStatus::Completed,
    );
    write_session_for_test(
        workspace,
        "session-old-2",
        now - Duration::days(60),
        "test",
        SessionStatus::Completed,
    );

    let cfg = PruneConfig {
        keep_last: 2,
        older_than: Duration::days(7),
        dry_run: true,
        ..Default::default()
    };

    let result = prune_sessions(&store, workspace, &cfg)
        .await
        .expect("prune_sessions");

    // Dry run: nothing actually deleted
    assert!(result.dry_run);
    assert_eq!(
        result.deleted.len(),
        2,
        "should find 2 old sessions to delete"
    );
    assert!(
        result.deleted.contains(&"session-old-1".to_string()),
        "session-old-1 should be in delete list"
    );
    assert!(
        result.deleted.contains(&"session-old-2".to_string()),
        "session-old-2 should be in delete list"
    );
    // Verify files still exist
    assert!(store.load("session-old-1").await.is_ok());
    assert!(store.load("session-old-2").await.is_ok());
}

#[tokio::test]
async fn prune_sessions_actually_deletes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path();
    let sessions_dir = workspace.join(".dcode-ai").join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let store = SessionStore::new(&sessions_dir);
    let now = Utc::now();

    write_session_for_test(
        workspace,
        "session-old",
        now - Duration::days(30),
        "test",
        SessionStatus::Completed,
    );
    write_session_for_test(
        workspace,
        "session-recent",
        now - Duration::hours(1),
        "test",
        SessionStatus::Completed,
    );

    let result = prune_sessions(
        &store,
        workspace,
        &PruneConfig {
            keep_last: 1,
            older_than: Duration::hours(1),
            dry_run: false,
            ..Default::default()
        },
    )
    .await
    .expect("prune_sessions");

    assert!(!result.dry_run);
    assert_eq!(result.deleted.len(), 1, "should delete 1 old session");
    assert!(result.deleted.contains(&"session-old".to_string()));
    // Verify deletion
    assert!(store.load("session-old").await.is_err());
    assert!(store.load("session-recent").await.is_ok());
}

#[tokio::test]
async fn prune_sessions_skips_running_sessions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path();
    let sessions_dir = workspace.join(".dcode-ai").join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let store = SessionStore::new(&sessions_dir);
    let now = Utc::now();

    write_session_for_test(
        workspace,
        "session-running",
        now - Duration::days(30),
        "test",
        SessionStatus::Running,
    );
    write_session_for_test(
        workspace,
        "session-old",
        now - Duration::days(30),
        "test",
        SessionStatus::Completed,
    );

    let result = prune_sessions(
        &store,
        workspace,
        &PruneConfig {
            keep_last: 0,
            older_than: Duration::hours(1),
            dry_run: true,
            ..Default::default()
        },
    )
    .await
    .expect("prune_sessions");

    assert!(
        !result.deleted.contains(&"session-running".to_string()),
        "should not include running session"
    );
    assert!(
        result.deleted.contains(&"session-old".to_string()),
        "should include old completed session"
    );
}

// ─── Context Overflow Hardening Tests ──────────────────────────────────────

/// Test the rearm logic directly via ContextManager stats to avoid
/// needing a live provider in unit tests.
#[test]
fn context_manager_rearm_logic() {
    use crate::context_manager::ContextManager;

    // Context at 80% should trigger needs_attention
    let config = ContextManagerConfig {
        context_window_target: 10_000,
        max_retained_messages: 50,
        auto_summarize_threshold: 75,
        enable_auto_summarize: true,
        max_message_chars_for_summary: 10000,
    };
    let cm = ContextManager::new(config, "test-model".to_string());

    // Build natural-language text of a known BPE token size. (A run of the
    // same char can't be used: the tokenizer merges repeated chars, so it
    // would not produce a predictable token count.)
    let text_with_tokens = |target: usize| -> String {
        let unit = "the quick brown fox jumps over the lazy dog. ";
        let mut s = String::new();
        while crate::token_count::count_tokens(&s) < target {
            s.push_str(unit);
        }
        s
    };

    // ~69% of 10_000 → below both the 75% summarize and 80% attention thresholds.
    let msgs_small = vec![Message::user(text_with_tokens(6_900).as_str())];
    let stats_small = cm.stats(&msgs_small);
    assert!(
        !stats_small.needs_attention,
        "69% should not need attention (got {}%)",
        stats_small.usage_percent
    );
    assert!(
        !stats_small.should_summarize,
        "69% should not auto-summarize (got {}%)",
        stats_small.usage_percent
    );

    // ~81% of 10_000 → needs_attention (>=80%) and auto-summarize (>=75%).
    let msgs_medium = vec![Message::user(text_with_tokens(8_100).as_str())];
    let stats_medium = cm.stats(&msgs_medium);
    assert!(stats_medium.needs_attention, "80% should need attention");
    assert!(
        stats_medium.should_summarize,
        "80% should trigger auto-summarize"
    );
}

/// Empty AI summary must fall back to extractive summarization.
/// `build_compaction_fallback_summary` handles this.
#[test]
fn empty_summary_triggers_extractive_fallback() {
    let msgs = vec![
        Message::user("What files are in the project?"),
        Message::assistant("There are 50 Rust source files across 4 crates."),
        Message::user("Run the tests"),
        Message::assistant("All tests passed."),
    ];
    let fallback = build_compaction_fallback_summary(&msgs);
    assert!(
        !fallback.trim().is_empty(),
        "fallback summary must not be empty when messages are provided"
    );
    assert!(
        fallback.contains("files") || fallback.contains("project"),
        "fallback should contain content from messages"
    );
}

/// `build_compaction_fallback_summary` with empty input returns a default message.
#[test]
fn empty_messages_fallback_uses_default_message() {
    let fallback = build_compaction_fallback_summary(&[]);
    assert_eq!(
        fallback,
        "Earlier conversation context was compacted due to token limits."
    );
}

#[test]
fn mcp_failures_become_a_note_for_the_model() {
    assert!(mcp_failure_note(&[]).is_none());
    let note = mcp_failure_note(&[("jira".into(), "401 Unauthorized".into())]).unwrap();
    assert!(note.contains("`jira` (401 Unauthorized)"), "{note}");
    assert!(note.contains("not available"), "{note}");
}

#[test]
fn artifacts_capture_files_commands_and_errors() {
    use dcode_ai_common::message::{Message, MessageToolCall};

    let messages = vec![
        Message::assistant_with_tool_calls(
            "edit it",
            vec![MessageToolCall {
                id: "1".into(),
                name: "edit_file".into(),
                arguments: json!({"path": "src/main.rs", "old_text": "a", "new_text": "b"}),
            }],
        ),
        Message::assistant_with_tool_calls(
            "run tests",
            vec![MessageToolCall {
                id: "2".into(),
                name: "execute_bash".into(),
                arguments: json!({"command": "cargo test --workspace"}),
            }],
        ),
        Message::tool("2", "error[E0599]: no method named `foo`"),
    ];

    let artifacts = extract_session_artifacts(&messages);
    assert!(artifacts.contains("src/main.rs"), "files: {artifacts}");
    assert!(
        artifacts.contains("cargo test --workspace"),
        "commands: {artifacts}"
    );
    assert!(artifacts.contains("error[E0599]"), "errors: {artifacts}");

    // And the full fallback nests these under the artifacts header.
    let full = build_compaction_fallback_summary(&messages);
    assert!(full.contains("## Preserved Artifacts"));
}

#[test]
fn compaction_preview_uses_preserved_artifact_summary_without_mutating() {
    use dcode_ai_common::message::{Message, MessageToolCall, Role};

    let context_manager = ContextManager::with_default_config("MiniMax-M2.5".into());
    let messages = vec![
        Message::user("edit src/main.rs"),
        Message {
            role: Role::Assistant,
            content: dcode_ai_common::message::MessageContent::Text("running tests".into()),
            tool_call_id: None,
            tool_calls: Some(vec![MessageToolCall {
                id: "call-1".into(),
                name: "execute_bash".into(),
                arguments: json!({"command": "cargo test -p dcode-ai-runtime"}),
            }]),
            reasoning_content: None,
            provider_blocks: None,
        },
        Message::tool("call-1", "error[E0425]: cannot find value"),
    ];
    let before_count = messages.len();

    let preview = build_compaction_preview_summary(&context_manager, &messages);

    assert_eq!(messages.len(), before_count);
    assert!(preview.contains("## Preserved Artifacts"), "{preview}");
    assert!(preview.contains("src/main.rs"), "{preview}");
    assert!(
        preview.contains("cargo test -p dcode-ai-runtime"),
        "{preview}"
    );
    assert!(preview.contains("error[E0425]"), "{preview}");
}

#[test]
fn compaction_preview_empty_session_uses_default_message() {
    let context_manager = ContextManager::with_default_config("MiniMax-M2.5".into());
    let messages = vec![Message::system("system prompt")];

    assert_eq!(
        build_compaction_preview_summary(&context_manager, &messages),
        "Earlier conversation context was compacted due to token limits."
    );
}

#[test]
fn looks_like_path_filters_noise() {
    assert!(looks_like_path("src/main.rs"));
    assert!(looks_like_path("crates/core/src/cost.rs"));
    assert!(!looks_like_path("https://example.com/x.html"));
    assert!(!looks_like_path("justtext"));
    assert!(!looks_like_path("a/b")); // no extension
}

#[test]
fn context_limit_error_detector_matches_common_strings() {
    let e = ProviderError::Other("Prompt is too long for this model".into());
    assert!(is_context_limit_error(&e));

    let e = ProviderError::RequestFailed("context window exceeded".into());
    assert!(is_context_limit_error(&e));

    let e = ProviderError::Other("network timeout".into());
    assert!(!is_context_limit_error(&e));
}
