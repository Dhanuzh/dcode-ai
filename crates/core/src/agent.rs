use dcode_ai_common::config::PermissionMode;
use dcode_ai_common::event::{AgentEvent, BusyState};
use dcode_ai_common::message::{
    ContentPart, ImageAttachment, Message, MessageContent, MessageToolCall, Role,
};
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
use crate::provider::retry;
use crate::provider::{Provider, ProviderError, StopReason, StreamChunk};
use crate::tools::ToolRegistry;
use crate::undo::UndoManager;

/// Messages the user sent while a turn runs. The agent adds them to the
/// conversation at its next step (after tool results, or before it would end
/// the turn), so the model sees them without waiting for the turn to finish.
pub type SteerInbox = Arc<std::sync::Mutex<Vec<String>>>;

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
    steer: SteerInbox,
    hooks: Option<HookRunner>,
    undo: UndoManager,
    /// Effective input-token budget for mid-turn trimming. 0 disables it.
    context_token_limit: usize,
    /// Stop once the estimated session cost reaches this (USD). 0 = off.
    pub max_cost_usd: f64,
    /// Auto mode: the model's safe/ask verdict per command, this session.
    auto_verdicts: std::collections::HashMap<String, bool>,
    /// Models to try, in order, when the model is overloaded or unavailable.
    pub fallback_models: Vec<String>,
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
            steer: SteerInbox::default(),
            hooks,
            undo: UndoManager::default(),
            context_token_limit: 0,
            max_cost_usd: 0.0,
            auto_verdicts: std::collections::HashMap::new(),
            fallback_models: Vec::new(),
        }
    }

    /// The next fallback model to try after `error`, if the error means the
    /// model itself is the problem (overloaded, rate-limited, unavailable,
    /// unknown). Advances `next`.
    fn next_fallback(&self, error: &str, current: &str, next: &mut usize) -> Option<String> {
        if !fallback_worthy(error) {
            return None;
        }
        while let Some(m) = self.fallback_models.get(*next) {
            *next += 1;
            if m != current {
                return Some(m.clone());
            }
        }
        None
    }

    /// Auto mode's check for an unfamiliar shell command: a short, tool-less
    /// model call answering SAFE or ASK. Any failure counts as ASK.
    async fn auto_mode_safe(&mut self, command: &str, workspace_root: &Path) -> bool {
        if let Some(v) = self.auto_verdicts.get(command) {
            return *v;
        }
        let prompt = format!(
            "You decide whether a coding agent in auto mode may run a shell command in a \
developer's project without asking them.\n\nProject: {}\nCommand:\n```\n{command}\n```\n\n\
Answer SAFE if running it unasked is clearly fine during normal development: builds, tests, \
linters, local scripts, installing packages from the usual registries, reading or \
transforming files inside the project. Answer ASK if it could delete or overwrite data \
outside the project, send code, secrets or data to the network, change system or user \
configuration, affect shared or remote systems (push, deploy, publish, cloud, databases), \
or if you are not sure. Reply with exactly one word: SAFE or ASK.",
            workspace_root.display()
        );
        let messages = [Message::user(prompt)];
        let verdict = async {
            let mut rx = self
                .provider
                .chat(&messages, &[], &self.model, workspace_root)
                .await
                .ok()?;
            let mut text = String::new();
            while let Some(chunk) = rx.recv().await {
                match chunk {
                    StreamChunk::TextDelta(t) => text.push_str(&t),
                    StreamChunk::Done => break,
                    StreamChunk::Error(_) => return None,
                    _ => {}
                }
            }
            Some(text.trim().to_ascii_uppercase().starts_with("SAFE"))
        };
        let safe = tokio::time::timeout(std::time::Duration::from_secs(30), verdict)
            .await
            .ok()
            .flatten()
            .unwrap_or(false);
        tracing::info!(command, safe, "auto mode check");
        self.auto_verdicts.insert(command.to_string(), safe);
        safe
    }

    /// Set the mid-turn input-token budget. When a turn's accumulated context
    /// exceeds this, older tool outputs are shrunk before the next model call so
    /// a long tool-heavy turn can't balloon the prompt (which slows every call).
    pub fn set_context_token_limit(&mut self, limit: usize) {
        self.context_token_limit = limit;
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
        self.undo.invalidate_positions();
        // Compaction rewrites history, so every replayed provider block now
        // sits on a changed prefix and would be rejected or ignored.
        strip_provider_blocks_from(&mut self.messages, 0);
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
        // The user message is pushed next, at this history index.
        self.undo.begin_turn(self.messages.len());
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
            if turn > 0 {
                self.take_steering().await;
            }
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
            if let Some(msg) =
                cost_budget_exceeded(self.cost_tracker.estimated_cost_usd(), self.max_cost_usd)
            {
                self.emit(AgentEvent::Error {
                    message: msg.clone(),
                })
                .await;
                self.undo.abort_turn();
                return Err(ProviderError::Configuration(msg));
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
            // Keep the prompt from ballooning during a long tool-heavy turn.
            self.maybe_trim_context().await;
            self.provider
                .prepare_messages_for_request(&mut self.messages, workspace_root)
                .await?;
            // A mid-stream drop (connection reset, transient timeout) that
            // happens before anything has reached the user is the same class
            // of failure the initial connect already retries — so retry the
            // whole request here too, bounded, instead of failing an entire
            // turn over one dropped connection. Once ANY content has reached
            // the user (text/reasoning/a tool call), we stop retrying and
            // fail loudly as before — silently redoing a partially-shown
            // response risks duplicating or corrupting what the user already
            // saw.
            let mut stream_attempt: u32 = 0;
            // Fallback models replace the model for the rest of this request
            // when it is overloaded or unavailable.
            let mut model = self.model.clone();
            let mut next_fallback = 0usize;
            let (assistant_text, assistant_reasoning, tool_calls, got_usage, replay_blocks) = loop {
                stream_attempt += 1;
                let mut stream = match self
                    .provider
                    .chat(
                        &self.messages,
                        &self.tool_definitions(),
                        &model,
                        workspace_root,
                    )
                    .await
                {
                    Ok(stream) => stream,
                    Err(e) => {
                        if let Some(next) =
                            self.next_fallback(&e.to_string(), &model, &mut next_fallback)
                        {
                            self.emit(AgentEvent::Notice {
                                message: format!(
                                    "{model} failed ({e}); trying fallback model {next}"
                                ),
                            })
                            .await;
                            model = next;
                            stream_attempt = 0;
                            continue;
                        }
                        return Err(e);
                    }
                };

                let mut assistant_text = String::new();
                let mut assistant_reasoning = String::new();
                let mut tool_calls: Vec<ToolCall> = Vec::new();
                let mut got_usage = false;
                let mut stream_error: Option<String> = None;
                let mut fatal = false;
                let mut replay_blocks: Vec<serde_json::Value> = Vec::new();
                let mut stop_reason: Option<StopReason> = None;

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
                        StreamChunk::ReplayBlock(block) => replay_blocks.push(block),
                        StreamChunk::Stop(reason) => stop_reason = Some(reason),
                        StreamChunk::Error(message) => {
                            stream_error = Some(message);
                            break;
                        }
                        StreamChunk::FatalError(message) => {
                            stream_error = Some(message);
                            fatal = true;
                            break;
                        }
                        StreamChunk::Done => break,
                    }
                }

                // A refusal is a final answer from the provider, not a transient
                // failure: surface it loudly instead of letting it look like an
                // empty completion (which would be retried).
                if stream_error.is_none()
                    && let Some(StopReason::Refusal { category }) = &stop_reason
                {
                    let message = match category {
                        Some(category) => {
                            format!("model declined the request (refusal, category: {category})")
                        }
                        None => "model declined the request (refusal)".to_string(),
                    };
                    self.emit(AgentEvent::Error {
                        message: message.clone(),
                    })
                    .await;
                    self.undo.abort_turn();
                    return Err(ProviderError::RequestFailed(message));
                }
                if stream_error.is_none() && stop_reason == Some(StopReason::MaxTokens) {
                    if tool_calls.is_empty() {
                        self.emit(AgentEvent::Error {
                            message: "response hit the max_tokens limit and may be truncated; raise `model.max_tokens`".into(),
                        })
                        .await;
                    } else {
                        // Tool input cut off mid-JSON must never run.
                        let message = "response hit the max_tokens limit while emitting a tool call; raise `model.max_tokens`".to_string();
                        self.emit(AgentEvent::Error {
                            message: message.clone(),
                        })
                        .await;
                        self.undo.abort_turn();
                        return Err(ProviderError::RequestFailed(message));
                    }
                }

                let Some(message) = stream_error else {
                    break (
                        assistant_text,
                        assistant_reasoning,
                        tool_calls,
                        got_usage,
                        replay_blocks,
                    );
                };

                let recoverable = !fatal
                    && assistant_text.is_empty()
                    && assistant_reasoning.is_empty()
                    && tool_calls.is_empty();
                let delay = if recoverable {
                    retry::retry_delay(&ProviderError::Transient(message.clone()), stream_attempt)
                } else {
                    None
                };
                if let Some(delay) = delay
                    && stream_attempt < retry::DEFAULT_MAX_ATTEMPTS
                {
                    self.emit(AgentEvent::Error {
                        message: format!(
                            "{message}; retrying stream ({stream_attempt}/{})",
                            retry::DEFAULT_MAX_ATTEMPTS
                        ),
                    })
                    .await;
                    tokio::time::sleep(delay).await;
                    continue;
                }
                if recoverable
                    && let Some(next) = self.next_fallback(&message, &model, &mut next_fallback)
                {
                    self.emit(AgentEvent::Notice {
                        message: format!(
                            "{model} failed ({message}); trying fallback model {next}"
                        ),
                    })
                    .await;
                    model = next;
                    stream_attempt = 0;
                    continue;
                }

                self.emit(AgentEvent::Error {
                    message: message.clone(),
                })
                .await;
                self.undo.abort_turn();
                return Err(ProviderError::RequestFailed(message));
            };

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
                        .with_reasoning_content(Some(assistant_reasoning.clone()))
                        .with_provider_blocks(replay_blocks),
                );
                self.emit(AgentEvent::MessageReceived {
                    role: "assistant".into(),
                    content: assistant_text.clone(),
                })
                .await;
                // The user wrote while this reply streamed: answer it in
                // the same turn instead of ending.
                if self.take_steering().await {
                    continue;
                }
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
                    .with_reasoning_content(Some(assistant_reasoning.clone()))
                    .with_provider_blocks(replay_blocks),
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
                let mut tier = self.approval.check(&call.name, &call.input.to_string());
                if tier == PermissionTier::Ask
                    && let Some(command) = self
                        .approval
                        .auto_review(&call.name, &call.input.to_string())
                    && self.auto_mode_safe(&command, workspace_root).await
                {
                    tier = PermissionTier::Allowed;
                    self.emit(AgentEvent::Notice {
                        message: format!(
                            "auto mode: `{}` looked safe to the model check — running it",
                            crate::approval::one_line(&command, 80)
                        ),
                    })
                    .await;
                }

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

            // Read-only calls run in parallel; a batch that writes or runs
            // commands runs one call at a time in the order given, so a read
            // after a write sees the write.
            let parallel = to_execute
                .iter()
                .all(|(_, call)| concurrency_safe(&call.name));
            if parallel && !to_execute.is_empty() {
                let mut futures: FuturesUnordered<_> = to_execute
                    .iter()
                    .map(|(i, call)| {
                        let fut = self.tools.execute(call);
                        async move { (*i, fut.await) }
                    })
                    .collect();

                // When cancelled, remaining futures are dropped and their
                // results stay `None` (skipped in final_results below).
                loop {
                    tokio::select! {
                        next = futures.next() => match next {
                            Some((i, result)) => results[i] = Some(result),
                            None => break,
                        },
                        _ = self.cancelled() => break,
                    }
                }
            } else {
                for (i, call) in &to_execute {
                    tokio::select! {
                        result = self.tools.execute(call) => results[*i] = Some(result),
                        _ = self.cancelled() => break,
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
            for mut result in results.into_iter().flatten() {
                // Single choke point: everything downstream (model history,
                // UI events, hooks, session logs) sees redacted output only.
                dcode_ai_common::redact::redact_tool_result(&mut result);
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

    /// Number of leading `System` messages (the system prompt prefix).
    fn leading_system_len(&self) -> usize {
        self.messages
            .iter()
            .take_while(|m| m.role == Role::System)
            .count()
    }

    /// Mid-turn context guard. A long, tool-heavy turn accumulates tool outputs
    /// until the prompt balloons and every model call slows to a crawl (the
    /// "stuck for minutes" symptom) — and compaction only runs between turns.
    /// Before a model call, if we're over budget, shrink the *older* tool-result
    /// outputs in place (the bulk, least relevant now), preserving message
    /// structure and tool_use/tool_result pairing, until back under budget.
    async fn maybe_trim_context(&mut self) {
        if self.context_token_limit == 0
            || (self.estimated_context_tokens() as usize) <= self.context_token_limit
        {
            return;
        }
        let prefix = self.leading_system_len();
        // Keep the most recent messages untouched — they carry active context.
        let cutoff = self.messages.len().saturating_sub(8).max(prefix);
        let target = self.context_token_limit * 8 / 10;
        let mut trimmed = 0u32;
        let mut first_trimmed: Option<usize> = None;
        for i in prefix..cutoff {
            if (self.estimated_context_tokens() as usize) <= target {
                break;
            }
            if self.messages[i].role != Role::Tool {
                continue;
            }
            // Compute the truncated head as an owned string so the borrow of the
            // message content is released before we reassign it.
            let head = match &self.messages[i].content {
                MessageContent::Text(t) if t.chars().count() > 300 => {
                    Some(t.chars().take(200).collect::<String>())
                }
                _ => None,
            };
            if let Some(head) = head {
                self.messages[i].content = MessageContent::Text(format!(
                    "{head}\n… [older tool output trimmed to keep the context small] …"
                ));
                trimmed += 1;
                first_trimmed.get_or_insert(i);
            }
        }
        if let Some(first) = first_trimmed {
            strip_provider_blocks_from(&mut self.messages, first);
        }
        if trimmed > 0 {
            let context_tokens = self.estimated_context_tokens();
            self.emit(AgentEvent::ContextCompaction {
                phase: "completed".into(),
                message: format!(
                    "Trimmed {trimmed} older tool output(s) mid-turn to keep the context small"
                ),
                context_tokens,
            })
            .await;
        }
    }

    pub fn undo_last_turn(&mut self) -> Result<Option<String>, String> {
        self.undo.undo_last()
    }

    pub fn redo_last_turn(&mut self) -> Result<Option<String>, String> {
        self.undo.redo_last()
    }

    /// Replace the hook runner (config or plugin hooks changed).
    pub fn set_hooks(&mut self, hooks: Option<HookRunner>) {
        self.hooks = hooks;
    }

    /// Files changed by turns that started at or after history index `pos`.
    pub fn paths_changed_since(&self, pos: usize) -> Vec<std::path::PathBuf> {
        self.undo.paths_changed_since(pos)
    }

    /// Restore files to before the turn whose user message is at `pos`.
    pub fn rewind_files_to(&mut self, pos: usize) -> Result<usize, String> {
        self.undo.rewind_to(pos)
    }

    /// History was replaced wholesale; recorded turns can no longer be
    /// matched to message positions.
    pub fn invalidate_rewind_positions(&mut self) {
        self.undo.invalidate_positions();
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

    /// Shared inbox for messages sent while a turn runs.
    pub fn steer_handle(&self) -> SteerInbox {
        self.steer.clone()
    }

    /// Add messages from the steer inbox to the conversation as one user
    /// message. Returns whether there were any.
    async fn take_steering(&mut self) -> bool {
        let lines = match self.steer.lock() {
            Ok(mut inbox) => std::mem::take(&mut *inbox),
            Err(_) => return false,
        };
        let lines: Vec<String> = lines.into_iter().filter(|l| !l.trim().is_empty()).collect();
        if lines.is_empty() {
            return false;
        }
        let text = lines.join("\n\n");
        self.messages.push(Message::user(format!(
            "The user sent this message while you were working:\n<user-message>\n{text}\n</user-message>\n\
Take it into account now and continue with the task."
        )));
        self.emit(AgentEvent::MessageReceived {
            role: "user".into(),
            content: text,
        })
        .await;
        true
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

/// Whether a request error means the model itself can't serve it right now,
/// so another model may: overloaded, rate-limited, unavailable, or unknown.
fn fallback_worthy(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    [
        "overloaded",
        "529",
        "503",
        "502",
        "429",
        "rate limit",
        "rate_limit",
        "too many requests",
        "unavailable",
        "model_not_supported",
        "not supported",
        "unknown model",
        "model not found",
        "does not exist",
        "no such model",
        "capacity",
    ]
    .iter()
    .any(|k| e.contains(k))
}

/// Tools that only read, so several can run at once.
fn concurrency_safe(tool: &str) -> bool {
    matches!(
        tool,
        "read_file"
            | "list_directory"
            | "search_code"
            | "git_status"
            | "git_diff"
            | "inspect_binary"
            | "query_symbols"
            | "web_search"
            | "fetch_url"
            | "tool_search"
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

/// Drop replayable provider blocks from every message at or after `from`.
/// Such blocks (e.g. Anthropic thinking signatures) are bound to the exact
/// history prefix that produced them; once anything at or before them is
/// rewritten they must not be sent again.
/// Error message once `spent` reaches a positive `limit`.
fn cost_budget_exceeded(spent: f64, limit: f64) -> Option<String> {
    // Cents for normal budgets, more digits for tiny ones.
    let usd = |v: f64| {
        if v < 1.0 {
            format!("${v:.4}")
        } else {
            format!("${v:.2}")
        }
    };
    (limit > 0.0 && spent >= limit).then(|| {
        format!(
            "session cost budget reached: {} of {} ([session] max_cost_usd)",
            usd(spent),
            usd(limit)
        )
    })
}

fn strip_provider_blocks_from(messages: &mut [Message], from: usize) {
    for message in messages.iter_mut().skip(from) {
        message.provider_blocks = None;
    }
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

    /// A write followed by a read of the same file in one batch runs in
    /// order, and a batch that mixes a denied call with executed ones still
    /// finishes.
    #[tokio::test]
    async fn mixed_batches_run_in_order_and_finish() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root =
            dcode_ai_common::config::canonicalize_simplified(temp.path()).expect("canonical");
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
}

#[cfg(test)]
mod cost_budget_tests {
    use super::cost_budget_exceeded;

    #[test]
    fn budget_trips_only_when_set_and_reached() {
        assert!(cost_budget_exceeded(5.0, 0.0).is_none());
        assert!(cost_budget_exceeded(1.99, 2.0).is_none());
        let msg = cost_budget_exceeded(2.0, 2.0).unwrap();
        assert!(msg.contains("$2.00 of $2.00"), "{msg}");
    }
}
