use dcode_ai_common::config::PermissionMode;
use dcode_ai_common::event::{AgentEvent, BusyState};
use dcode_ai_common::message::{ContentPart, ImageAttachment, Message, MessageToolCall, Role};
use dcode_ai_common::tool::{PermissionTier, ToolCall, ToolDefinition, ToolResult};
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::approval::{ApprovalPolicy, ApprovalVerdict};
use crate::cost::CostTracker;
use crate::hooks::{HookEventKind, HookRunner};
use crate::provider::{Provider, ProviderError, StreamChunk};
use crate::tools::ToolRegistry;
use crate::undo::UndoManager;

/// Drives the multi-turn conversation and tool-use loop.
pub struct AgentLoop {
    pub provider: Box<dyn Provider>,
    pub tools: ToolRegistry,
    pub approval: ApprovalPolicy,
    pub messages: Vec<Message>,
    pub model: String,
    pub cost_tracker: CostTracker,
    event_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    max_turns: u32,
    max_tool_calls_per_turn: u32,
    checkpoint_interval: u32,
    cancel_flag: Arc<AtomicBool>,
    hooks: Option<HookRunner>,
    undo: UndoManager,
}

impl AgentLoop {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Box<dyn Provider>,
        tools: ToolRegistry,
        approval: ApprovalPolicy,
        model: String,
        event_tx: tokio::sync::mpsc::Sender<AgentEvent>,
        max_turns: u32,
        max_tool_calls_per_turn: u32,
        checkpoint_interval: u32,
        hooks: Option<HookRunner>,
    ) -> Self {
        Self {
            provider,
            tools,
            approval,
            messages: Vec::new(),
            cost_tracker: CostTracker::for_model(model.clone()),
            model,
            event_tx,
            max_turns,
            max_tool_calls_per_turn,
            checkpoint_interval,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            hooks,
            undo: UndoManager::default(),
        }
    }

    /// Add a system prompt once at startup.
    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        self.messages.push(Message::system(prompt));
    }

    /// Replace the leading system prompt in place (used when config changes
    /// mid-session, e.g. `/personality`). Keeps the rest of the conversation.
    pub fn replace_system_prompt(&mut self, prompt: impl Into<String>) {
        let msg = Message::system(prompt);
        match self.messages.first_mut() {
            Some(first) if matches!(first.role, Role::System) => *first = msg,
            _ => self.messages.insert(0, msg),
        }
    }

    /// Replace the LLM provider (e.g. after user switches provider in-session).
    pub fn replace_provider(&mut self, provider: Box<dyn Provider>) {
        self.provider = provider;
    }

    /// Apply a context compaction to the message history.
    /// Called by Supervisor when context exceeds threshold.
    /// Returns the new message count.
    pub fn compact_messages(&mut self, new_messages: Vec<Message>) -> usize {
        let old_count = self.messages.len();
        self.messages = new_messages;
        let new_count = self.messages.len();
        tracing::debug!(
            "context compacted: {} messages → {} messages",
            old_count,
            new_count
        );
        new_count
    }

    /// Run one turn: send messages to the provider, execute any tool calls,
    /// and repeat until the provider returns a final text response.
    pub async fn run_turn(
        &mut self,
        user_input: &str,
        workspace_root: &Path,
        attachments: &[ImageAttachment],
    ) -> Result<String, ProviderError> {
        self.cancel_flag.store(false, Ordering::SeqCst);
        self.undo.begin_turn();
        let user_msg = if attachments.is_empty() {
            Message::user(user_input)
        } else {
            let mut parts: Vec<ContentPart> = Vec::new();
            let trimmed = user_input.trim();
            if !trimmed.is_empty() {
                parts.push(ContentPart::Text {
                    text: user_input.to_string(),
                });
            } else {
                parts.push(ContentPart::Text {
                    text: "(See attached image(s).)".into(),
                });
            }
            for a in attachments {
                parts.push(ContentPart::Image {
                    media_type: a.media_type.clone(),
                    path: a.path.clone(),
                });
            }
            Message::user_with_parts(parts)
        };
        let preview = user_msg.event_preview();
        self.messages.push(user_msg);
        self.emit(AgentEvent::MessageReceived {
            role: "user".into(),
            content: preview,
        })
        .await;

        let mut turn = 0_u32;
        let mut empty_retries = 0_u32;
        let mut attachments_cleaned = attachments.is_empty();
        const MAX_EMPTY_RETRIES: u32 = 2;
        // Consecutive failures of the same tool — stops infinite retry loops.
        let mut consecutive_tool_failures: u32 = 0;
        let mut last_failed_tool: String = String::new();
        const MAX_CONSECUTIVE_TOOL_FAILURES: u32 = 3;

        let final_text = loop {
            if self.is_cancelled() {
                self.emit(AgentEvent::Error {
                    message: "Run cancelled".into(),
                })
                .await;
                self.undo.abort_turn();
                return Err(ProviderError::Other("run cancelled".into()));
            }
            turn += 1;
            if turn > self.max_turns {
                self.undo.abort_turn();
                return Err(ProviderError::Other(format!(
                    "turn budget exceeded (max {})",
                    self.max_turns
                )));
            }

            self.emit(AgentEvent::BusyStateChanged {
                state: BusyState::Thinking,
            })
            .await;
            self.emit(AgentEvent::Checkpoint {
                phase: "provider_request".into(),
                detail: format!("Starting model turn {turn}"),
                turn,
            })
            .await;
            self.provider
                .prepare_messages_for_request(&mut self.messages, workspace_root)
                .await?;
            let mut stream = self
                .provider
                .chat(
                    &self.messages,
                    &self.tool_definitions(),
                    &self.model,
                    workspace_root,
                )
                .await?;

            let mut assistant_text = String::new();
            let mut assistant_reasoning = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut got_usage = false;

            let mut cancel_poll = tokio::time::interval(Duration::from_millis(25));
            cancel_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let chunk = tokio::select! {
                    _ = cancel_poll.tick() => {
                        if self.is_cancelled() {
                            self.emit(AgentEvent::Error {
                                message: "Run cancelled while streaming model output".into(),
                            })
                            .await;
                            self.undo.abort_turn();
                            return Err(ProviderError::Other("run cancelled".into()));
                        }
                        continue;
                    }
                    chunk = stream.recv() => chunk,
                };
                let Some(chunk) = chunk else {
                    break;
                };
                match chunk {
                    StreamChunk::InternalDelta(delta) => {
                        assistant_reasoning.push_str(&delta);
                        self.emit(AgentEvent::ThinkingDelta { delta }).await;
                    }
                    StreamChunk::TextDelta(delta) => {
                        if assistant_text.is_empty() {
                            self.emit(AgentEvent::BusyStateChanged {
                                state: BusyState::Streaming,
                            })
                            .await;
                        }
                        assistant_text.push_str(&delta);
                        self.emit(AgentEvent::TokensStreamed { delta }).await;
                    }
                    StreamChunk::ToolUse(call) => {
                        self.emit(AgentEvent::ToolCallStarted {
                            call_id: call.id.clone(),
                            tool: call.name.clone(),
                            input: call.input.clone(),
                        })
                        .await;
                        tool_calls.push(call);
                    }
                    StreamChunk::Usage {
                        input_tokens,
                        output_tokens,
                    } => {
                        got_usage = true;
                        self.cost_tracker.add(input_tokens, output_tokens);
                        let context_tokens = self.estimated_context_tokens();
                        self.emit(AgentEvent::CostUpdated {
                            input_tokens: self.cost_tracker.total_input_tokens(),
                            output_tokens: self.cost_tracker.output_tokens,
                            estimated_cost_usd: self.cost_tracker.estimated_cost_usd(),
                            context_tokens,
                        })
                        .await;
                    }
                    StreamChunk::CacheUsage {
                        read_tokens,
                        creation_tokens,
                    } => {
                        self.cost_tracker.add_cache(read_tokens, creation_tokens);
                    }
                    StreamChunk::Error(message) => {
                        self.emit(AgentEvent::Error {
                            message: message.clone(),
                        })
                        .await;
                        self.undo.abort_turn();
                        return Err(ProviderError::RequestFailed(message));
                    }
                    StreamChunk::Done => break,
                }
            }

            if !attachments_cleaned {
                cleanup_processed_attachments(&mut self.messages, workspace_root, attachments);
                attachments_cleaned = true;
            }

            if tool_calls.is_empty() {
                if assistant_text.trim().is_empty() {
                    empty_retries += 1;
                    let detail = empty_completion_detail(&self.model, got_usage);
                    if empty_retries <= MAX_EMPTY_RETRIES {
                        self.emit(AgentEvent::Error {
                            message: format!(
                                "{detail}; retrying empty completion ({empty_retries}/{MAX_EMPTY_RETRIES})"
                            ),
                        })
                        .await;
                        continue;
                    }
                    let message = format!(
                        "{detail} after {} attempts",
                        MAX_EMPTY_RETRIES.saturating_add(1)
                    );
                    self.emit(AgentEvent::Error {
                        message: message.clone(),
                    })
                    .await;
                    self.undo.abort_turn();
                    return Err(ProviderError::RequestFailed(message));
                }
                self.messages.push(
                    Message::assistant(assistant_text.clone())
                        .with_reasoning_content(Some(assistant_reasoning.clone())),
                );
                self.emit(AgentEvent::MessageReceived {
                    role: "assistant".into(),
                    content: assistant_text.clone(),
                })
                .await;
                break assistant_text;
            }

            let replay_tool_calls = tool_calls
                .iter()
                .map(|call| MessageToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.input.clone(),
                })
                .collect();

            self.messages.push(
                Message::assistant_with_tool_calls(assistant_text, replay_tool_calls)
                    .with_reasoning_content(Some(assistant_reasoning.clone())),
            );

            if tool_calls.len() as u32 > self.max_tool_calls_per_turn {
                self.undo.abort_turn();
                return Err(ProviderError::Other(format!(
                    "tool-call budget exceeded in turn {turn} ({} > {})",
                    tool_calls.len(),
                    self.max_tool_calls_per_turn
                )));
            }

            // ── Phase 1: permission checks (sequential — approvals may be interactive) ──
            //
            // Produces, in original order, either a pre-resolved result (denied /
            // approval-denied) or a ticket to execute concurrently in phase 2.
            enum Ticket {
                Resolved(ToolResult),
                Execute(ToolCall),
            }

            if self.is_cancelled() {
                self.emit(AgentEvent::Error {
                    message: "Run cancelled before tool execution".into(),
                })
                .await;
                self.undo.abort_turn();
                return Err(ProviderError::Other("run cancelled".into()));
            }

            let mut tickets: Vec<Ticket> = Vec::with_capacity(tool_calls.len());

            for call in &tool_calls {
                let tier = self.approval.check(&call.name, &call.input.to_string());

                match tier {
                    PermissionTier::Denied => {
                        tickets.push(Ticket::Resolved(ToolResult {
                            call_id: call.id.clone(),
                            success: false,
                            output: String::new(),
                            error: Some(format!("tool `{}` denied by policy", call.name)),
                        }));
                    }

                    PermissionTier::Ask => {
                        let description = format!("Tool `{}` requires approval", call.name);
                        self.emit(AgentEvent::ApprovalRequested {
                            call_id: call.id.clone(),
                            tool: call.name.clone(),
                            description: description.clone(),
                        })
                        .await;
                        // Register the pending approval (inside `resolve`) with no
                        // awaitable gap after emitting the event above. Running the
                        // best-effort hook *before* this point left a window where a
                        // fast verdict could arrive before the pending entry existed,
                        // get dropped, and hang the turn. The hook can't influence
                        // the verdict, so we run it after registration instead.
                        let verdict = self.approval.resolve(call, &description).await;
                        if let Some(hooks) = &self.hooks {
                            hooks
                                .run_best_effort(
                                    HookEventKind::ApprovalRequested,
                                    Some(&call.name),
                                    &json!({
                                        "call_id": call.id.clone(),
                                        "tool": call.name.clone(),
                                        "input": call.input.clone(),
                                        "description": description,
                                    }),
                                )
                                .await;
                        }
                        let approved = verdict.is_approved();
                        self.emit(AgentEvent::ApprovalResolved {
                            call_id: call.id.clone(),
                            approved,
                        })
                        .await;
                        if let ApprovalVerdict::AllowPattern(pattern) = &verdict {
                            self.approval.add_session_allow(pattern.clone());
                        }
                        if approved
                            && call.name == "execute_bash"
                            && self.approval.mode() == PermissionMode::BypassPermissions
                        {
                            // In bypass mode, the first explicit shell approval enables
                            // the rest of the session's shell calls.
                            self.approval.add_session_allow("execute_bash:*".into());
                        }

                        if approved {
                            // If the user partially approved (hunk selection), use
                            // the modified input instead of the original.
                            let effective_call =
                                if let ApprovalVerdict::ApprovedModified(modified_input) = &verdict
                                {
                                    let mut patched = call.clone();
                                    patched.input = modified_input.clone();
                                    patched
                                } else {
                                    call.clone()
                                };
                            if let Some(hooks) = &self.hooks
                                && let Err(reason) = hooks
                                    .run(
                                        HookEventKind::PreToolUse,
                                        Some(&effective_call.name),
                                        &json!({
                                            "call_id": effective_call.id.clone(),
                                            "tool": effective_call.name.clone(),
                                            "input": effective_call.input.clone(),
                                        }),
                                    )
                                    .await
                            {
                                tickets.push(Ticket::Resolved(ToolResult {
                                    call_id: effective_call.id.clone(),
                                    success: false,
                                    output: String::new(),
                                    error: Some(reason),
                                }));
                                continue;
                            }
                            tickets.push(Ticket::Execute(effective_call));
                        } else {
                            if self.approval.should_fail_on_ask() {
                                let message = format!(
                                    "tool `{}` requires approval in headless mode; rerun with a non-interactive permission mode such as `dont-ask` or `bypass-permissions`",
                                    call.name
                                );
                                self.emit(AgentEvent::Error {
                                    message: message.clone(),
                                })
                                .await;
                                self.undo.abort_turn();
                                return Err(ProviderError::Other(message));
                            }
                            tickets.push(Ticket::Resolved(ToolResult {
                                call_id: call.id.clone(),
                                success: false,
                                output: String::new(),
                                error: Some(format!(
                                    "tool `{}` requires approval; request was denied",
                                    call.name
                                )),
                            }));
                        }
                    }

                    PermissionTier::Allowed => {
                        if let Some(hooks) = &self.hooks
                            && let Err(reason) = hooks
                                .run(
                                    HookEventKind::PreToolUse,
                                    Some(&call.name),
                                    &json!({
                                        "call_id": call.id.clone(),
                                        "tool": call.name.clone(),
                                        "input": call.input.clone(),
                                    }),
                                )
                                .await
                        {
                            tickets.push(Ticket::Resolved(ToolResult {
                                call_id: call.id.clone(),
                                success: false,
                                output: String::new(),
                                error: Some(reason),
                            }));
                            continue;
                        }
                        tickets.push(Ticket::Execute(call.clone()));
                    }
                }
            }

            // ── Phase 2: concurrent execution ────────────────────────────────────────
            //
            // All approved calls run simultaneously. `ToolRegistry::execute` takes
            // `&self` so multiple concurrent borrows are safe.
            //
            // We keep a parallel `Option<ToolResult>` vec (None = still executing)
            // and fill it from the join results.
            let n = tickets.len();
            let mut results: Vec<Option<ToolResult>> = (0..n).map(|_| None).collect();

            // Gather indices and refs for calls that actually need execution
            let to_execute: Vec<(usize, &ToolCall)> = tickets
                .iter()
                .enumerate()
                .filter_map(|(i, t)| {
                    if let Ticket::Execute(call) = t {
                        Some((i, call))
                    } else {
                        None
                    }
                })
                .collect();

            for (_, call) in &to_execute {
                self.undo.record_tool_call(call, workspace_root);
            }

            if !to_execute.is_empty() {
                let mut futures: FuturesUnordered<_> = to_execute
                    .iter()
                    .map(|(i, call)| {
                        let fut = self.tools.execute(call);
                        async move { (*i, fut.await) }
                    })
                    .collect();

                // Poll tool futures with cancellation checking.
                // When cancelled, remaining futures are dropped and their
                // results stay `None` (skipped in final_results below).
                loop {
                    if results.iter().all(|r| r.is_some()) {
                        break;
                    }
                    tokio::select! {
                        Some((i, result)) = futures.next() => {
                            results[i] = Some(result);
                        }
                        _ = self.cancelled() => {
                            break;
                        }
                    }
                }
            }

            // Fill pre-resolved slots
            for (i, ticket) in tickets.into_iter().enumerate() {
                if let Ticket::Resolved(result) = ticket {
                    results[i] = Some(result);
                }
            }

            let mut final_results = Vec::new();
            for result in results.into_iter().flatten() {
                final_results.push(result);
            }
            self.undo.note_results(&tool_calls, &final_results);

            if let Some(hooks) = &self.hooks {
                for result in &final_results {
                    let hook_event = if result.success {
                        HookEventKind::PostToolUse
                    } else {
                        HookEventKind::PostToolFailure
                    };
                    hooks
                        .run_best_effort(
                            hook_event,
                            None,
                            &json!({
                                "call_id": result.call_id,
                                "success": result.success,
                                "output": result.output,
                                "error": result.error,
                            }),
                        )
                        .await;
                }
            }

            // ── Phase 3: push results to history + emit events (in original order) ──
            if self.checkpoint_interval > 0 && n as u32 >= self.checkpoint_interval {
                self.emit(AgentEvent::Checkpoint {
                    phase: "tool_execution".into(),
                    detail: format!("Executed {n} tool calls in turn {turn}"),
                    turn,
                })
                .await;
            }

            // Track consecutive failures of the same tool to detect infinite retry loops.
            let all_failed_same_tool = !final_results.is_empty()
                && final_results.iter().all(|r| !r.success)
                && tool_calls.len() == 1;
            if all_failed_same_tool {
                let tool_name = &tool_calls[0].name;
                if *tool_name == last_failed_tool {
                    consecutive_tool_failures += 1;
                } else {
                    last_failed_tool = tool_name.clone();
                    consecutive_tool_failures = 1;
                }
            } else {
                consecutive_tool_failures = 0;
                last_failed_tool.clear();
            }

            for result in final_results {
                self.messages.push(Message::tool(
                    result.call_id.clone(),
                    format_tool_result(&result),
                ));
                self.emit(AgentEvent::ToolCallCompleted {
                    call_id: result.call_id.clone(),
                    output: result,
                })
                .await;
            }

            if consecutive_tool_failures >= MAX_CONSECUTIVE_TOOL_FAILURES {
                let msg = format!(
                    "Tool `{}` failed {} times consecutively — stopping to avoid infinite loop.",
                    last_failed_tool, consecutive_tool_failures
                );
                self.emit(AgentEvent::Error {
                    message: msg.clone(),
                })
                .await;
                break msg;
            }
        };

        if self.cost_tracker.input_tokens == 0 && self.cost_tracker.output_tokens == 0 {
            let estimated_input = (self
                .messages
                .iter()
                .map(|message| message.content.approx_chars())
                .sum::<usize>()
                / 4) as u64;
            let estimated_output = (final_text.len() / 4) as u64;
            self.cost_tracker.add(estimated_input, estimated_output);
            let context_tokens = self.estimated_context_tokens();
            self.emit(AgentEvent::CostUpdated {
                input_tokens: self.cost_tracker.total_input_tokens(),
                output_tokens: self.cost_tracker.output_tokens,
                estimated_cost_usd: self.cost_tracker.estimated_cost_usd(),
                context_tokens,
            })
            .await;
        }

        self.emit(AgentEvent::BusyStateChanged {
            state: BusyState::Idle,
        })
        .await;
        if let Err(error) = self.undo.finalize_turn() {
            tracing::warn!("undo finalize failed: {error}");
        }
        Ok(final_text)
    }

    /// Rough token estimate of the live conversation (current context-window
    /// occupancy), using ~4 chars/token over all message content.
    fn estimated_context_tokens(&self) -> u64 {
        (self
            .messages
            .iter()
            .map(|message| message.content.approx_chars())
            .sum::<usize>()
            / 4) as u64
    }

    pub fn undo_last_turn(&mut self) -> Result<Option<String>, String> {
        self.undo.undo_last()
    }

    pub fn redo_last_turn(&mut self) -> Result<Option<String>, String> {
        self.undo.redo_last()
    }

    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools.definitions()
    }

    async fn emit(&self, event: AgentEvent) {
        let _ = self.event_tx.send(event).await;
    }

    pub fn event_sender(&self) -> Option<tokio::sync::mpsc::Sender<AgentEvent>> {
        Some(self.event_tx.clone())
    }

    pub fn request_cancel(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
    }

    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel_flag.clone()
    }

    fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::SeqCst)
    }

    /// Returns a future that completes when cancellation is requested.
    /// Polls the cancel flag at 50ms intervals.
    async fn cancelled(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

fn cleanup_processed_attachments(
    messages: &mut [Message],
    workspace_root: &Path,
    attachments: &[ImageAttachment],
) {
    let removed_paths: HashSet<String> = attachments.iter().map(|a| a.path.clone()).collect();
    if removed_paths.is_empty() {
        return;
    }

    for message in messages {
        let _ = message.content.strip_image_paths(&removed_paths);
    }

    for attachment in attachments {
        let full_path = workspace_root.join(&attachment.path);
        if let Err(err) = std::fs::remove_file(&full_path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                "failed to remove processed image attachment {}: {}",
                full_path.display(),
                err
            );
        }
        if let Some(parent) = full_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

/// Upper bound on a single tool result's size in the conversation (~6k tokens).
/// Huge outputs (large file reads, `git log`, verbose command output) otherwise
/// accumulate across a long turn until the prompt balloons and every model call
/// slows to a crawl (the "stuck for minutes" symptom). Anything larger is
/// middle-elided, keeping the informative head and tail.
const MAX_TOOL_OUTPUT_CHARS: usize = 24_000;

fn cap_tool_output(s: &str) -> String {
    let total = s.chars().count();
    if total <= MAX_TOOL_OUTPUT_CHARS {
        return s.to_string();
    }
    let head = MAX_TOOL_OUTPUT_CHARS * 2 / 3;
    let tail = MAX_TOOL_OUTPUT_CHARS - head;
    let chars: Vec<char> = s.chars().collect();
    let head_str: String = chars[..head].iter().collect();
    let tail_str: String = chars[chars.len() - tail..].iter().collect();
    let omitted = total - head - tail;
    format!(
        "{head_str}\n\n… [{omitted} characters truncated to keep the context small — re-run with a narrower query/range if you need the omitted part] …\n\n{tail_str}"
    )
}

fn format_tool_result(result: &dcode_ai_common::tool::ToolResult) -> String {
    let raw = if result.success {
        result.output.clone()
    } else {
        result
            .error
            .clone()
            .unwrap_or_else(|| "tool failed".to_string())
    };
    cap_tool_output(&raw)
}

fn empty_completion_detail(model: &str, got_usage: bool) -> String {
    let usage_detail = if got_usage {
        "usage was reported"
    } else {
        "no usage was reported"
    };
    format!(
        "Provider returned empty completion for model `{model}`: no assistant text or tool calls ({usage_detail})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::ApprovalPolicy;
    use crate::provider::{Provider, ProviderError, StreamChunk};
    use crate::tools::ToolRegistry;
    use async_trait::async_trait;
    use dcode_ai_common::config::PermissionConfig;
    use dcode_ai_common::message::Message;
    use dcode_ai_common::tool::ToolDefinition;
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::Mutex;

    #[test]
    fn cap_tool_output_elides_only_huge_outputs() {
        // Small output passes through untouched.
        let small = "hello world";
        assert_eq!(cap_tool_output(small), small);

        // Oversized output is middle-elided with a marker and kept under budget.
        let big = "x".repeat(MAX_TOOL_OUTPUT_CHARS * 3);
        let capped = cap_tool_output(&big);
        assert!(capped.contains("characters truncated"));
        assert!(capped.chars().count() < big.chars().count());
        assert!(capped.chars().count() <= MAX_TOOL_OUTPUT_CHARS + 200);
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

    #[tokio::test]
    async fn provider_stream_error_fails_loudly() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(16);
        let provider = Box::new(ChunkProvider {
            chunks: Mutex::new(VecDeque::from([vec![StreamChunk::Error(
                "openai stream error: connection reset".into(),
            )]])),
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
        while let Ok(event) = event_rx.try_recv() {
            match event {
                AgentEvent::Error { message } if message.contains("connection reset") => {
                    saw_error_event = true;
                }
                AgentEvent::TokensStreamed { .. } => {
                    saw_token_event = true;
                }
                _ => {}
            }
        }

        assert!(saw_error_event);
        assert!(!saw_token_event);
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
}
