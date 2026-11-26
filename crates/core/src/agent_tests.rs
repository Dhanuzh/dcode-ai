use super::*;
use crate::approval::ApprovalPolicy;
use crate::provider::{Provider, ProviderError, StopReason, StreamChunk};
use crate::tools::ToolRegistry;
use async_trait::async_trait;
use dcode_ai_common::config::PermissionConfig;
use dcode_ai_common::message::Message;
use dcode_ai_common::tool::ToolDefinition;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;

#[test]
fn image_paths_from_tool_output() {
    assert_eq!(
        image_paths_in("Saved a screenshot to .dcode-ai/screenshots/a.png"),
        [".dcode-ai/screenshots/a.png"]
    );
    let out = "text\nSaved an image to .dcode-ai/mcp-files/s-1-0.jpg (image/jpeg, 3 KB)\n\
                   Saved an image to .dcode-ai/mcp-files/s-1-1.svg (image/svg+xml, 1 KB)\n\
                   Saved an image to /etc/passwd (x, 1 B)";
    assert_eq!(image_paths_in(out), [".dcode-ai/mcp-files/s-1-0.jpg"]);
}

#[test]
fn a_compaction_must_free_thirty_percent() {
    assert!(compaction_worked(100_000, 70_000));
    assert!(!compaction_worked(100_000, 71_000));
    assert!(!compaction_worked(100_000, 120_000), "growth is refused");
}

#[test]
fn only_loops_pause_a_turn() {
    assert!(loop_alarm(1, 4, None).is_none());
    let looped = loop_alarm(4, 4, Some("browser")).unwrap();
    assert!(looped.contains("`browser` ran 4 times"));
}

#[test]
fn huge_outputs_are_saved_whole_and_capped_with_a_pointer() {
    let temp = tempfile::tempdir().expect("tempdir");
    let big: String = (0..5_000).map(|i| format!("line {i}\n")).collect();
    let capped = spill_tool_output(temp.path(), "call/1", &big).expect("spilled");
    assert!(capped.chars().count() <= MAX_TOOL_OUTPUT_CHARS);
    assert!(capped.contains(".dcode-ai/tool-output/call_1.txt"));
    let saved =
        std::fs::read_to_string(temp.path().join(".dcode-ai/tool-output/call_1.txt")).unwrap();
    assert_eq!(saved, big);
    assert!(spill_tool_output(temp.path(), "c2", "small").is_none());
}

#[test]
fn cap_tool_output_elides_only_huge_outputs() {
    // Small output passes through untouched.
    let small = "hello world";
    assert_eq!(cap_tool_output(small), small);

    // Oversized output is middle-elided with a marker and kept under budget.
    let big = "x".repeat(MAX_TOOL_OUTPUT_CHARS * 3);
    let capped = cap_tool_output(&big);
    assert!(capped.contains("characters left out"));
    assert!(capped.chars().count() < big.chars().count());
    assert!(capped.chars().count() <= MAX_TOOL_OUTPUT_CHARS);
    assert_eq!(
        cap_tool_output(&capped),
        capped,
        "capping twice changes nothing"
    );
}

struct ChunkProvider {
    chunks: Mutex<VecDeque<Vec<StreamChunk>>>,
}

#[async_trait]
impl Provider for ChunkProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let chunks = self
            .chunks
            .lock()
            .expect("provider chunks lock")
            .pop_front()
            .unwrap_or_default();
        tokio::spawn(async move {
            for chunk in chunks {
                let _ = tx.send(chunk).await;
            }
        });
        Ok(rx)
    }
}

/// Answers by model: nothing from `cheap`, "done" from anything else;
/// records which models were asked.
struct TierProvider {
    asked: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for TierProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        self.asked.lock().unwrap().push(model.to_string());
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let empty = model == "cheap";
        tokio::spawn(async move {
            if !empty {
                let _ = tx.send(StreamChunk::TextDelta("done".into())).await;
            }
            let _ = tx.send(StreamChunk::Done).await;
        });
        Ok(rx)
    }
}

#[tokio::test]
async fn auto_routing_sends_quick_ones_cheap_and_moves_up_on_an_empty_answer() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let mut agent = AgentLoop::new(
        Box::new(TierProvider {
            asked: asked.clone(),
        }),
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()),
        "auto".into(),
        event_tx,
        5,
        3,
        1,
        None,
    );
    agent.routing = Some(crate::routing::Routing {
        cheap: "cheap".into(),
        strong: "strong".into(),
    });
    let temp = tempfile::tempdir().expect("tempdir");
    let out = agent
        .run_turn("what does main do?", temp.path(), &[])
        .await
        .expect("answered by the strong model");
    assert_eq!(out, "done");
    assert_eq!(*asked.lock().unwrap(), vec!["cheap", "strong"]);
    let mut notices = Vec::new();
    while let Ok(e) = event_rx.try_recv() {
        if let AgentEvent::Notice { message } = e {
            notices.push(message);
        }
    }
    assert!(
        notices.iter().any(|n| n == "auto → cheap (a quick one)"),
        "{notices:?}"
    );
    assert!(
        notices
            .iter()
            .any(|n| n.contains("answered nothing; auto → strong"))
    );
    // Real work starts on the strong model.
    asked.lock().unwrap().clear();
    agent
        .run_turn("refactor the parser", temp.path(), &[])
        .await
        .unwrap();
    assert_eq!(*asked.lock().unwrap(), vec!["strong"]);
}

/// A mid-stream drop before anything reached the user is retried up to
/// `retry::DEFAULT_MAX_ATTEMPTS` times, but the turn still fails loudly
/// once that budget is exhausted — retrying must never turn into
/// silently hiding a persistent connection failure.
#[tokio::test(start_paused = true)]
async fn provider_stream_error_retries_then_fails_loudly() {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let error_batch = || {
        vec![StreamChunk::Error(
            "openai stream error: connection reset".into(),
        )]
    };
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new(VecDeque::from(vec![
            error_batch();
            retry::DEFAULT_MAX_ATTEMPTS as usize
        ])),
    });
    let mut agent = AgentLoop::new(
        provider,
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        3,
        3,
        1,
        None,
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let result = agent.run_turn("hello", temp.path(), &[]).await;

    assert!(
        matches!(result, Err(ProviderError::RequestFailed(message)) if message.contains("connection reset"))
    );

    let mut saw_error_event = false;
    let mut saw_token_event = false;
    let mut retry_notice_count = 0u32;
    while let Ok(event) = event_rx.try_recv() {
        match event {
            AgentEvent::Error { message } if message.contains("retrying stream") => {
                retry_notice_count += 1;
            }
            AgentEvent::Error { message } if message.contains("connection reset") => {
                saw_error_event = true;
            }
            AgentEvent::TokensStreamed { .. } => {
                saw_token_event = true;
            }
            _ => {}
        }
    }

    // Retries on attempts 1..DEFAULT_MAX_ATTEMPTS-1; the final attempt
    // fails loudly instead of retrying.
    assert_eq!(retry_notice_count, retry::DEFAULT_MAX_ATTEMPTS - 1);
    assert!(saw_error_event);
    assert!(!saw_token_event);
}

#[test]
fn verdict_key_drops_exports_but_keeps_directories() {
    assert_eq!(
        verdict_key("export PATH=$HOME/.nvm/bin:$PATH; cd /p/app && npm run build"),
        "cd /p/app && npm run build"
    );
    assert_eq!(
        verdict_key("export PATH=/x:$PATH && cd /p/app; npm run build"),
        "cd /p/app && npm run build"
    );
    assert_ne!(
        verdict_key("cd ~ && rm -rf x"),
        verdict_key("cd build && rm -rf x")
    );
    assert_eq!(
        verdict_key("export X=$(curl evil); make"),
        "export X=$(curl evil); make"
    );
    assert_eq!(
        verdict_key("export PATH=$(curl evil):$PATH; make"),
        "export PATH=$(curl evil):$PATH; make"
    );
}

/// A tool answering "read <path>".
struct EchoRead;

#[async_trait]
impl crate::tools::ToolExecutor for EchoRead {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: String::new(),
            parameters: json!({}),
        }
    }
    async fn execute(&self, call: &ToolCall) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: format!("read {}", call.input["path"].as_str().unwrap_or("")),
            error: None,
        }
    }
}

/// Code Mode: a script's calls run through the agent; one that needs
/// approval asks (no handler here = denied) and throws inside the script.
#[tokio::test]
async fn run_script_calls_allowed_tools_and_refuses_the_rest() {
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoRead));
    let mut agent = AgentLoop::new(
        Box::new(ChunkProvider {
            chunks: Mutex::new(VecDeque::new()),
        }),
        tools,
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        3,
        3,
        1,
        None,
    );
    let temp = tempfile::tempdir().expect("tempdir");
    let call = ToolCall {
        id: "s1".into(),
        name: "run_script".into(),
        input: json!({"code": "const a = ['x', 'y'].map(p => tools.read_file({path: p}));\n\
                let w; try { tools.write_file({path: 'z', content: ''}); } catch (e) { w = e.message; }\n\
                return {a, w};"}),
    };
    let r = agent.run_script(&call, temp.path()).await;
    assert!(r.success, "{r:?}");
    assert!(
        r.output.contains("read x") && r.output.contains("read y"),
        "{}",
        r.output
    );
    assert!(r.output.contains("the request was denied"), "{}", r.output);
    assert!(!temp.path().join("z").exists());
}

struct ApproveAll;

#[async_trait]
impl crate::approval::ApprovalHandler for ApproveAll {
    async fn resolve(&self, _call: &ToolCall, _description: &str) -> ApprovalVerdict {
        ApprovalVerdict::Approved
    }
}

/// A fake `write_file` that only answers.
struct FakeWrite;

#[async_trait]
impl crate::tools::ToolExecutor for FakeWrite {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            description: String::new(),
            parameters: json!({}),
        }
    }
    async fn execute(&self, call: &ToolCall) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: format!("wrote {}", call.input["path"].as_str().unwrap_or("")),
            error: None,
        }
    }
}

/// Code Mode 2: a call that needs approval asks the user mid-script and
/// runs when approved; every inner call is shown (started + completed);
/// `parallel` reads run together and come back in order.
#[tokio::test]
async fn run_script_asks_and_shows_inner_calls() {
    let (event_tx, mut events) = tokio::sync::mpsc::channel(256);
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoRead));
    tools.register(Box::new(FakeWrite));
    let mut agent = AgentLoop::new(
        Box::new(ChunkProvider {
            chunks: Mutex::new(VecDeque::new()),
        }),
        tools,
        ApprovalPolicy::new(PermissionConfig::default()).with_handler(Arc::new(ApproveAll)),
        "test-model".into(),
        event_tx,
        3,
        3,
        1,
        None,
    );
    let temp = tempfile::tempdir().expect("tempdir");
    let call = ToolCall {
        id: "s2".into(),
        name: "run_script".into(),
        input: json!({"code": "const w = tools.write_file({path: 'z', content: ''});\n\
                const r = parallel([{tool: 'read_file', args: {path: 'a'}}, {tool: 'read_file', args: {path: 'b'}}]);\n\
                return [w, ...r.map(x => x.text)].join('|');"}),
    };
    let r = agent.run_script(&call, temp.path()).await;
    assert!(r.success, "{r:?}");
    assert_eq!(r.output, "wrote z|read a|read b");
    let (mut started, mut completed, mut asked) = (Vec::new(), Vec::new(), Vec::new());
    while let Ok(e) = events.try_recv() {
        match e {
            AgentEvent::ToolCallStarted { call_id, .. } => started.push(call_id),
            AgentEvent::ToolCallCompleted { call_id, output } => {
                assert!(output.success, "{output:?}");
                completed.push(call_id)
            }
            AgentEvent::ApprovalRequested { call_id, .. } => asked.push(call_id),
            _ => {}
        }
    }
    assert_eq!(started, ["s2.1", "s2.2", "s2.3"]);
    assert_eq!(completed, ["s2.1", "s2.2", "s2.3"]);
    assert_eq!(asked, ["s2.1"], "only the write asks");
}

/// A reply stuck repeating one line is stopped and kept to one copy.
#[tokio::test]
async fn repeating_reply_is_stopped() {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1024);
    let line = "Checking the config file once more to be sure.\n";
    let mut chunks = vec![StreamChunk::TextDelta("Plan:\n".into())];
    chunks.extend((0..200).map(|_| StreamChunk::TextDelta(line.into())));
    chunks.push(StreamChunk::Done);
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new(VecDeque::from([chunks])),
    });
    let mut agent = AgentLoop::new(
        provider,
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        3,
        3,
        1,
        None,
    );
    let temp = tempfile::tempdir().expect("tempdir");
    let result = agent.run_turn("hello", temp.path(), &[]).await;
    assert_eq!(result.unwrap(), format!("Plan:\n{line}"));
    let mut noticed = false;
    while let Ok(event) = event_rx.try_recv() {
        if let AgentEvent::Notice { message } = event {
            noticed |= message.contains("repeating itself");
        }
    }
    assert!(noticed);
}

/// If a retried attempt succeeds, the turn completes normally — proving
/// the retry is transparent recovery, not just a slower failure.
#[tokio::test(start_paused = true)]
async fn provider_stream_error_recovers_on_retry() {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new(VecDeque::from([
            vec![StreamChunk::Error(
                "openai stream error: connection reset".into(),
            )],
            vec![StreamChunk::TextDelta("hi there".into()), StreamChunk::Done],
        ])),
    });
    let mut agent = AgentLoop::new(
        provider,
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        3,
        3,
        1,
        None,
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let result = agent.run_turn("hello", temp.path(), &[]).await;

    assert_eq!(result.unwrap(), "hi there");

    let mut saw_retry_notice = false;
    while let Ok(event) = event_rx.try_recv() {
        if let AgentEvent::Error { message } = event
            && message.contains("retrying stream")
        {
            saw_retry_notice = true;
        }
    }
    assert!(saw_retry_notice);
}

async fn run_empty_completion_case(
    chunks: Vec<StreamChunk>,
) -> (Result<String, ProviderError>, Vec<AgentEvent>) {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(32);
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new(VecDeque::from([chunks.clone(), chunks.clone(), chunks])),
    });
    let mut agent = AgentLoop::new(
        provider,
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        5,
        3,
        1,
        None,
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let result = agent.run_turn("hello", temp.path(), &[]).await;
    let mut events = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        events.push(event);
    }
    (result, events)
}

#[tokio::test]
async fn empty_done_only_completion_fails_loudly_after_retries() {
    let (result, events) = run_empty_completion_case(vec![StreamChunk::Done]).await;

    assert!(
        matches!(result, Err(ProviderError::RequestFailed(message)) if message.contains("empty completion")
                && message.contains("test-model")
                && message.contains("no usage was reported")
                && message.contains("after 3 attempts"))
    );
    let retry_events = events
            .iter()
            .filter(|event| {
                matches!(event, AgentEvent::Error { message } if message.contains("retrying empty completion"))
            })
            .count();
    assert_eq!(retry_events, 2);
}

#[tokio::test]
async fn usage_only_completion_fails_loudly_after_retries() {
    let (result, _events) = run_empty_completion_case(vec![
        StreamChunk::Usage {
            input_tokens: 11,
            output_tokens: 0,
        },
        StreamChunk::Done,
    ])
    .await;

    assert!(
        matches!(result, Err(ProviderError::RequestFailed(message)) if message.contains("empty completion")
                && message.contains("usage was reported"))
    );
}

#[tokio::test]
async fn reasoning_only_completion_is_not_successful_assistant_output() {
    let (result, events) = run_empty_completion_case(vec![
        StreamChunk::InternalDelta("thinking only".into()),
        StreamChunk::Done,
    ])
    .await;

    assert!(
        matches!(result, Err(ProviderError::RequestFailed(message)) if message.contains("empty completion"))
    );
    assert!(events.iter().any(
        |event| matches!(event, AgentEvent::ThinkingDelta { delta } if delta == "thinking only")
    ));
    assert!(!events.iter().any(
        |event| matches!(event, AgentEvent::MessageReceived { role, .. } if role == "assistant")
    ));
}

#[tokio::test]
async fn refusal_fails_loudly_without_empty_retries() {
    let (result, events) = run_empty_completion_case(vec![
        StreamChunk::Stop(StopReason::Refusal {
            category: Some("cyber".into()),
        }),
        StreamChunk::Done,
    ])
    .await;

    assert!(
        matches!(result, Err(ProviderError::RequestFailed(message)) if message.contains("refusal") && message.contains("cyber"))
    );
    assert!(!events.iter().any(
            |event| matches!(event, AgentEvent::Error { message } if message.contains("retrying empty completion"))
        ));
}

#[tokio::test]
async fn replay_blocks_are_stored_and_stripped_on_compaction() {
    let (event_tx, _rx) = tokio::sync::mpsc::channel(32);
    let block = serde_json::json!({"type": "thinking", "thinking": "t", "signature": "s"});
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new(VecDeque::from([vec![
            StreamChunk::ReplayBlock(block.clone()),
            StreamChunk::TextDelta("answer".into()),
            StreamChunk::Stop(StopReason::EndTurn),
            StreamChunk::Done,
        ]])),
    });
    let mut agent = AgentLoop::new(
        provider,
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        3,
        3,
        1,
        None,
    );
    let temp = tempfile::tempdir().expect("tempdir");
    agent.run_turn("q", temp.path(), &[]).await.expect("turn");

    let assistant = agent.messages.last().expect("assistant message");
    assert_eq!(assistant.provider_blocks.as_deref(), Some(&[block][..]));

    let history = agent.messages.clone();
    agent.compact_messages(history);
    assert!(agent.messages.iter().all(|m| m.provider_blocks.is_none()));
}

/// Overloaded on every model except `good`.
struct PickyProvider {
    good: &'static str,
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl Provider for PickyProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        self.seen.lock().unwrap().push(model.to_string());
        if model != self.good {
            return Err(ProviderError::RequestFailed(format!(
                "{model}: 529 overloaded_error"
            )));
        }
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx.send(StreamChunk::TextDelta("ok".into())).await;
            let _ = tx.send(StreamChunk::Done).await;
        });
        Ok(rx)
    }
}

#[tokio::test]
async fn an_overloaded_model_falls_back_in_order() {
    let (event_tx, mut events) = tokio::sync::mpsc::channel(64);
    let provider = Box::new(PickyProvider {
        good: "second",
        seen: Mutex::new(Vec::new()),
    });
    let mut agent = AgentLoop::new(
        provider,
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()),
        "primary".into(),
        event_tx,
        3,
        3,
        0,
        None,
    );
    agent.fallback_models = vec!["first".into(), "second".into(), "third".into()];
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(agent.run_turn("hi", temp.path(), &[]).await.unwrap(), "ok");
    let mut notices = 0;
    while let Ok(e) = events.try_recv() {
        if let AgentEvent::Notice { message } = e
            && message.contains("trying fallback model")
        {
            notices += 1;
        }
    }
    assert_eq!(notices, 2);
    // Errors that aren't about the model don't switch models.
    assert!(!fallback_worthy("invalid request: bad tool schema"));
    assert!(fallback_worthy("HTTP 503 Service Unavailable"));
}

/// A message sent while the turn runs is answered in the same turn: the
/// reply that would have ended it is followed by the user's message.
#[tokio::test]
async fn steering_message_is_answered_in_the_same_turn() {
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new(VecDeque::from([
            vec![StreamChunk::TextDelta("first".into()), StreamChunk::Done],
            vec![StreamChunk::TextDelta("second".into()), StreamChunk::Done],
        ])),
    });
    let (event_tx, mut events) = tokio::sync::mpsc::channel(64);
    let mut agent = AgentLoop::new(
        provider,
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()),
        "m".into(),
        event_tx,
        5,
        3,
        0,
        None,
    );
    agent.steer_handle().lock().unwrap().push("use tabs".into());
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(
        agent.run_turn("go", temp.path(), &[]).await.unwrap(),
        "second"
    );
    let texts: Vec<String> = agent
        .messages
        .iter()
        .map(|m| m.content.to_summary_text())
        .collect();
    let first = texts.iter().position(|t| t == "first").unwrap();
    assert!(texts[first + 1].contains("<user-message>\nuse tabs\n</user-message>"));
    assert!(agent.steer_handle().lock().unwrap().is_empty());
    let mut shown = false;
    while let Ok(e) = events.try_recv() {
        if let AgentEvent::MessageReceived { role, content } = e {
            shown |= role == "user" && content == "use tabs";
        }
    }
    assert!(shown);
}

struct DenyAll;

#[async_trait]
impl crate::approval::ApprovalHandler for DenyAll {
    async fn resolve(&self, _call: &ToolCall, _description: &str) -> ApprovalVerdict {
        ApprovalVerdict::Denied
    }
}

/// Approves an edit only in part, as the TUI's hunk picker does: the
/// final content, marked to be written.
struct ApprovePart(String);

#[async_trait]
impl crate::approval::ApprovalHandler for ApprovePart {
    async fn resolve(&self, call: &ToolCall, _description: &str) -> ApprovalVerdict {
        ApprovalVerdict::ApprovedModified(serde_json::json!({
            "path": call.input["path"],
            "content": self.0,
            PARTIAL_WRITE_MARKER: true,
        }))
    }
}

/// A partly approved `edit_file` writes only the chosen content and
/// tells the model so.
#[tokio::test]
async fn partial_approval_writes_only_the_chosen_hunks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = dcode_ai_common::config::canonicalize_simplified(temp.path()).expect("canonical");
    std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    let edit = StreamChunk::ToolUse(ToolCall {
        id: "e1".into(),
        name: "edit_file".into(),
        input: serde_json::json!({"path": "a.txt", "old_text": "one\ntwo\n", "new_text": "ONE\nTWO\n"}),
    });
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new(VecDeque::from([
            vec![edit, StreamChunk::Done],
            vec![StreamChunk::TextDelta("done".into()), StreamChunk::Done],
        ])),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(crate::tools::edit_file::EditFileTool::new(
        root.clone(),
    )));
    tools.register(Box::new(crate::tools::write_file::WriteFileTool::new(
        root.clone(),
    )));
    let (event_tx, _rx) = tokio::sync::mpsc::channel(256);
    let mut agent = AgentLoop::new(
        provider,
        tools,
        ApprovalPolicy::new(PermissionConfig::default())
            .with_handler(Arc::new(ApprovePart("ONE\ntwo\n".into()))),
        "test-model".into(),
        event_tx,
        3,
        3,
        0,
        None,
    );
    agent.run_turn("go", &root, &[]).await.expect("turn");
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "ONE\ntwo\n"
    );
    let result = agent
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .map(|m| m.content.to_summary_text())
        .unwrap();
    assert!(result.contains("accepted only some hunks"), "{result}");
}

/// The session is saved after each tool step, mid-turn.
#[tokio::test]
async fn checkpoint_runs_after_each_tool_step() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = dcode_ai_common::config::canonicalize_simplified(temp.path()).expect("canonical");
    std::fs::write(root.join("a.txt"), "hi").unwrap();
    let read = |id: &str| {
        StreamChunk::ToolUse(ToolCall {
            id: id.into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "a.txt"}),
        })
    };
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new(VecDeque::from([
            vec![read("r1"), StreamChunk::Done],
            vec![read("r2"), StreamChunk::Done],
            vec![StreamChunk::TextDelta("done".into()), StreamChunk::Done],
        ])),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(crate::tools::filesystem::ReadFileTool::new(
        root.clone(),
    )));
    let (event_tx, _rx) = tokio::sync::mpsc::channel(256);
    let mut agent = AgentLoop::new(
        provider,
        tools,
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        3,
        3,
        0,
        None,
    );
    let saved: Arc<std::sync::Mutex<Vec<usize>>> = Default::default();
    let log = saved.clone();
    agent.checkpoint = Some(Arc::new(move |m: &[Message]| {
        log.lock().unwrap().push(m.len());
    }));
    agent.run_turn("go", &root, &[]).await.expect("turn");
    let sizes = saved.lock().unwrap().clone();
    assert_eq!(sizes.len(), 2, "one save per tool step: {sizes:?}");
    assert!(sizes[1] > sizes[0]);
}

/// Once the provider reports a prompt past the compactor's trigger, the
/// next step summarizes older history before calling the model again.
#[tokio::test]
async fn compacts_mid_turn_when_the_prompt_fills_up() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = dcode_ai_common::config::canonicalize_simplified(temp.path()).expect("canonical");
    std::fs::write(root.join("a.txt"), "hi").unwrap();
    let read = StreamChunk::ToolUse(ToolCall {
        id: "r1".into(),
        name: "read_file".into(),
        input: serde_json::json!({"path": "a.txt"}),
    });
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new(VecDeque::from([
            vec![
                read,
                StreamChunk::Usage {
                    input_tokens: 9_000,
                    output_tokens: 10,
                },
                StreamChunk::Done,
            ],
            // The summary request.
            vec![StreamChunk::TextDelta("SUMMARY".into()), StreamChunk::Done],
            vec![StreamChunk::TextDelta("done".into()), StreamChunk::Done],
        ])),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(crate::tools::filesystem::ReadFileTool::new(
        root.clone(),
    )));
    let (event_tx, _rx) = tokio::sync::mpsc::channel(256);
    let mut agent = AgentLoop::new(
        provider,
        tools,
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        5,
        5,
        0,
        None,
    );
    agent.messages = vec![
        Message::system("sys"),
        Message::user("old question"),
        Message::assistant(format!("old answer {}", "detail ".repeat(2_000))),
    ];
    let summaries: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let seen = summaries.clone();
    agent.compactor = Some(Compactor {
        prune_at: 8_000,
        compact_at: 8_000,
        prune: None,
        failures: Default::default(),
        pruned: Default::default(),
        estimate: Arc::new(|m: &[Message]| {
            m.iter().map(|x| x.content.approx_chars()).sum::<usize>() / 4
        }),
        model: "cheap".into(),
        prompt: Arc::new(|_m: &[Message]| Some("summarize".into())),
        apply: Arc::new(move |m: &[Message], summary: &str| {
            seen.lock().unwrap().push(summary.to_string());
            let mut out = vec![m[0].clone(), Message::system(summary)];
            out.extend_from_slice(&m[3..]);
            out
        }),
    });
    let reply = agent.run_turn("go", &root, &[]).await.expect("turn");
    assert_eq!(reply, "done");
    assert_eq!(summaries.lock().unwrap().as_slice(), ["SUMMARY"]);
    assert!(
        !agent
            .messages
            .iter()
            .any(|m| m.content.to_summary_text().contains("old answer"))
    );
    assert_eq!(agent.last_prompt_tokens, 0, "reset by compaction");
}

/// Every step reports a prompt past the compaction point, but the turn
/// compacts at most once (even when that compaction is refused), and
/// not at all below the point.
#[tokio::test]
async fn mid_turn_compaction_runs_at_most_once_per_turn() {
    for (compact_at, expected) in [(usize::MAX, 0usize), (50_000, 1)] {
        let temp = tempfile::tempdir().expect("tempdir");
        let root =
            dcode_ai_common::config::canonicalize_simplified(temp.path()).expect("canonical");
        for i in 0..6 {
            std::fs::write(root.join(format!("a{i}.txt")), "hi").unwrap();
        }
        let step = |i: usize| {
            vec![
                StreamChunk::ToolUse(ToolCall {
                    id: format!("r{i}"),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": format!("a{i}.txt")}),
                }),
                StreamChunk::Usage {
                    input_tokens: 90_000,
                    output_tokens: 10,
                },
                StreamChunk::Done,
            ]
        };
        let mut script: VecDeque<Vec<StreamChunk>> = VecDeque::new();
        script.push_back(step(0));
        for i in 1..6 {
            if expected == 1 && i == 1 {
                // The one summary request.
                script.push_back(vec![StreamChunk::TextDelta("S".into()), StreamChunk::Done]);
            }
            script.push_back(step(i));
        }
        script.push_back(vec![
            StreamChunk::TextDelta("done".into()),
            StreamChunk::Done,
        ]);
        let provider = Box::new(ChunkProvider {
            chunks: Mutex::new(script),
        });
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(crate::tools::filesystem::ReadFileTool::new(
            root.clone(),
        )));
        let (event_tx, _rx) = tokio::sync::mpsc::channel(512);
        let mut agent = AgentLoop::new(
            provider,
            tools,
            ApprovalPolicy::new(PermissionConfig::default()),
            "test-model".into(),
            event_tx,
            20,
            20,
            0,
            None,
        );
        agent.messages = vec![Message::system("sys")];
        let calls: Arc<std::sync::Mutex<usize>> = Default::default();
        let seen = calls.clone();
        agent.compactor = Some(Compactor {
            prune_at: compact_at,
            compact_at,
            prune: None,
            failures: Default::default(),
            pruned: Default::default(),
            estimate: Arc::new(|m: &[Message]| {
                m.iter().map(|x| x.content.approx_chars()).sum::<usize>() / 4
            }),
            model: "cheap".into(),
            prompt: Arc::new(|_m: &[Message]| Some("summarize".into())),
            apply: Arc::new(move |m: &[Message], _s: &str| {
                *seen.lock().unwrap() += 1;
                m[..m.len() - 1].to_vec()
            }),
        });
        let reply = agent.run_turn("go", &root, &[]).await.expect("turn");
        assert_eq!(reply, "done");
        assert_eq!(*calls.lock().unwrap(), expected, "compact at {compact_at}");
    }
}

/// A model that keeps sending the same call is paused, not left to loop.
#[tokio::test]
async fn repeated_identical_calls_pause_the_turn() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = dcode_ai_common::config::canonicalize_simplified(temp.path()).expect("canonical");
    std::fs::write(root.join("a.txt"), "hi").unwrap();
    let read = |i: usize| {
        vec![
            StreamChunk::ToolUse(ToolCall {
                id: format!("r{i}"),
                name: "read_file".into(),
                input: serde_json::json!({"path": "a.txt"}),
            }),
            StreamChunk::Done,
        ]
    };
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new((0..10).map(read).collect()),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(crate::tools::filesystem::ReadFileTool::new(
        root.clone(),
    )));
    let (event_tx, _rx) = tokio::sync::mpsc::channel(512);
    let mut agent = AgentLoop::new(
        provider,
        tools,
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        20,
        20,
        0,
        None,
    );
    let reply = agent.run_turn("go", &root, &[]).await.expect("turn");
    assert!(reply.starts_with("Paused:"), "{reply}");
    let calls = agent
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .count();
    assert_eq!(calls, 4);
}

/// A write followed by a read of the same file in one batch runs in
/// order, and a batch that mixes a denied call with executed ones still
/// finishes.
#[tokio::test]
async fn mixed_batches_run_in_order_and_finish() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = dcode_ai_common::config::canonicalize_simplified(temp.path()).expect("canonical");
    let call = |id: &str, name: &str, input: serde_json::Value| {
        StreamChunk::ToolUse(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
        })
    };
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new(VecDeque::from([
            vec![
                call(
                    "w",
                    "write_file",
                    serde_json::json!({"path": "a.txt", "content": "hi"}),
                ),
                call("r", "read_file", serde_json::json!({"path": "a.txt"})),
                call("d", "delete_path", serde_json::json!({"path": "a.txt"})),
                StreamChunk::Done,
            ],
            vec![StreamChunk::TextDelta("done".into()), StreamChunk::Done],
        ])),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(crate::tools::write_file::WriteFileTool::new(
        root.clone(),
    )));
    tools.register(Box::new(crate::tools::filesystem::ReadFileTool::new(
        root.clone(),
    )));
    let (event_tx, _rx) = tokio::sync::mpsc::channel(256);
    let mut agent = AgentLoop::new(
        provider,
        tools,
        // Auto mode: the edit runs, deleting asks and is denied.
        ApprovalPolicy::new(PermissionConfig {
            mode: PermissionMode::Auto,
            ..PermissionConfig::default()
        })
        .with_handler(Arc::new(DenyAll)),
        "test-model".into(),
        event_tx,
        3,
        3,
        0,
        None,
    );
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        agent.run_turn("go", &root, &[]),
    )
    .await
    .expect("turn finished")
    .expect("turn ok");
    assert_eq!(out, "done");
    let read = agent
        .messages
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("r"))
        .expect("read result");
    assert!(
        matches!(&read.content, dcode_ai_common::message::MessageContent::Text(t) if t.trim() == "hi")
    );
    assert!(root.join("a.txt").exists());
}

#[tokio::test]
async fn max_tokens_during_tool_call_fails_loudly() {
    let (result, _events) = run_empty_completion_case(vec![
        StreamChunk::ToolUse(ToolCall {
            id: "t1".into(),
            name: "write_file".into(),
            input: serde_json::json!({"path": "a"}),
        }),
        StreamChunk::Stop(StopReason::MaxTokens),
        StreamChunk::Done,
    ])
    .await;
    assert!(
        matches!(result, Err(ProviderError::RequestFailed(message)) if message.contains("max_tokens"))
    );
}

#[tokio::test]
async fn compact_messages_replaces_history_and_returns_count() {
    let (event_tx, _rx) = tokio::sync::mpsc::channel(16);
    let provider = Box::new(ChunkProvider {
        chunks: Mutex::new(VecDeque::new()),
    });
    let mut agent = AgentLoop::new(
        provider,
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        3,
        3,
        0,
        None,
    );

    // Populate with some messages
    agent.messages.push(Message::system("You are helpful"));
    agent.messages.push(Message::user("Hello"));
    agent.messages.push(Message::assistant("Hi there!"));
    agent.messages.push(Message::user("How are you?"));
    agent.messages.push(Message::assistant("I'm fine."));
    assert_eq!(agent.messages.len(), 5);

    // Compact: keep only system + last 2 messages
    let compacted = vec![
        Message::system("You are helpful"),
        Message::user("How are you?"),
        Message::assistant("I'm fine."),
    ];
    let new_count = agent.compact_messages(compacted);

    assert_eq!(new_count, 3, "should return the new message count");
    assert_eq!(agent.messages.len(), 3, "messages should be replaced");
    assert_eq!(
        agent.messages[0].role,
        dcode_ai_common::message::Role::System
    );
    assert_eq!(agent.messages[2].content.event_preview(), "I'm fine.");
}
