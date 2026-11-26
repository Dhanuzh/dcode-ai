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
use std::time::{Duration, Instant};

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
/// Saves the conversation mid-turn (see [`AgentLoop::checkpoint`]).
pub type Checkpoint = Arc<dyn Fn(&[Message]) + Send + Sync>;

pub type SteerInbox = Arc<std::sync::Mutex<Vec<String>>>;

/// Builds a summary request from the history (see [`Compactor::prompt`]).
pub type SummaryPrompt = Arc<dyn Fn(&[Message]) -> Option<String> + Send + Sync>;
/// Counts the conversation's tokens the way the caller's context manager
/// does (see [`Compactor::estimate`]).
pub type EstimateTokens = Arc<dyn Fn(&[Message]) -> usize + Send + Sync>;
/// Replaces older history with a summary (see [`Compactor::apply`]).
pub type ApplySummary = Arc<dyn Fn(&[Message], &str) -> Vec<Message> + Send + Sync>;

/// Summarizes the conversation mid-turn when it fills up, like Claude Code's
/// auto-compact (see [`AgentLoop::compactor`]). The supervisor builds it from
/// its context manager each turn.
/// Clears old tool outputs: `(history, protect_from)` → the pruned history
/// and the tokens freed, or `None` when not worth it (see [`Compactor::prune`]).
pub type PruneFn = Arc<dyn Fn(&[Message], usize) -> Option<(Vec<Message>, usize)> + Send + Sync>;

#[derive(Clone)]
pub struct Compactor {
    /// Clear old tool outputs (no model call) once the prompt reaches this.
    pub prune_at: usize,
    /// Summarize once the prompt reaches this.
    pub compact_at: usize,
    /// The conversation's size, counted like the supervisor counts it (used
    /// to check that a compaction really shrank it).
    pub estimate: EstimateTokens,
    /// `None` when pruning is turned off.
    pub prune: Option<PruneFn>,
    /// Model that writes the summary (a cheap one where possible).
    pub model: String,
    /// The summary request for the older part of the history; `None` when
    /// there is nothing old enough to summarize.
    pub prompt: SummaryPrompt,
    /// The history with its older part replaced by `summary` (an empty
    /// summary means the model gave none: build one from the messages).
    pub apply: ApplySummary,
    /// Failed or ineffective compactions in a row, shared with the
    /// supervisor; at 3 automatic compaction stops (circuit breaker).
    pub failures: Arc<std::sync::atomic::AtomicU32>,
    /// Old tool output was cleared since the last compaction (shared with
    /// the supervisor): clearing it again would cost another full re-read
    /// of the prompt, so the next time it summarizes instead.
    pub pruned: Arc<std::sync::atomic::AtomicBool>,
}

/// Failed compactions in a row after which automatic compaction stops.
pub const MAX_COMPACTION_FAILURES: u32 = 3;

/// Whether a compaction from `before` to `after` tokens freed enough to
/// keep (at least 30% smaller), as Gemini CLI rejects one that doesn't.
/// The same message on both sides of a compaction (provider blocks are
/// stripped from the kept tail, so they don't count).
fn same_message(a: &Message, b: &Message) -> bool {
    a.role == b.role
        && a.content == b.content
        && a.tool_call_id == b.tool_call_id
        && a.tool_calls == b.tool_calls
}

pub fn compaction_worked(before: usize, after: usize) -> bool {
    after * 10 <= before * 7
}

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
    /// Shared with `spawn_subagent`: the conversation as of a batch that
    /// starts a sub-agent (its summary, or a fork's copy).
    pub messages_mirror: Option<crate::tools::spawn_subagent::MessagesMirror>,
    /// Code Mode so far (the supervisor takes it after each turn):
    /// (scripts, inner calls, inner result chars, script output chars).
    pub script_stats: (u32, u32, u64, u64),
    /// Called with the conversation after every tool step, so a crash or a
    /// closed terminal mid-turn loses nothing (the session saves itself).
    pub checkpoint: Option<Checkpoint>,
    /// Summarizes the history between tool steps once it fills up.
    pub compactor: Option<Compactor>,
    /// History length when the last request was sent: what came after it
    /// is added to the measured size.
    last_prompt_len: usize,
    /// Old tool outputs were already cleared this turn.
    pruned_this_turn: bool,
    /// When the last model request was sent (the provider's prompt cache
    /// expires after a few idle minutes).
    last_request_at: Option<std::time::Instant>,
    /// Prompt size of the last model request as the provider counted it
    /// (input + cache reads + cache writes). 0 until a request reports usage
    /// and after the history is compacted.
    last_prompt_tokens: u64,
    /// The browser page open when the user message at each history index
    /// was sent (for `/rewind`).
    page_at_message: std::collections::BTreeMap<usize, String>,
    /// Requests that found nothing in the provider's cache (cache writes, no
    /// cache reads) and the tokens they wrote: the whole prompt re-sent at
    /// full price.
    pub cold_requests: u64,
    pub cold_tokens: u64,
    /// A mid-turn compaction already ran this turn: the conversation's size
    /// right after it.
    compacted_this_turn: Option<usize>,
    /// Cheap model for small side questions (auto mode's command check);
    /// `None` uses the session's model.
    pub utility_model: Option<String>,
    /// Subfolder instruction files already passed to the model.
    nested_instructions: std::collections::HashSet<std::path::PathBuf>,
    hooks: Option<HookRunner>,
    undo: UndoManager,
    /// Effective input-token budget for mid-turn trimming. 0 disables it.
    context_token_limit: usize,
    /// Stop once the estimated session cost reaches this (USD). 0 = off.
    /// Shared, so `/budget` can change it while the session runs.
    pub cost_budget: CostBudget,
    /// Auto mode: the model's safe/ask verdict per command, this session.
    auto_verdicts: std::collections::HashMap<String, bool>,
    /// Models to try, in order, when the model is overloaded or unavailable.
    pub fallback_models: Vec<String>,
    /// `model = "auto"`: the cheap and strong models picked per request.
    pub routing: Option<crate::routing::Routing>,
    /// The tier this run is on (auto routing).
    route_tier: crate::routing::Tier,
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
            messages_mirror: None,
            script_stats: (0, 0, 0, 0),
            nested_instructions: Default::default(),
            utility_model: None,
            checkpoint: None,
            compactor: None,
            last_prompt_tokens: 0,
            last_prompt_len: 0,
            pruned_this_turn: false,
            last_request_at: None,
            compacted_this_turn: None,
            page_at_message: Default::default(),
            cold_requests: 0,
            cold_tokens: 0,
            hooks,
            undo: UndoManager::default(),
            context_token_limit: 0,
            cost_budget: CostBudget::default(),
            auto_verdicts: std::collections::HashMap::new(),
            fallback_models: Vec::new(),
            routing: None,
            route_tier: crate::routing::Tier::Strong,
        }
    }

    /// The next fallback model to try after `error`, if the error means the
    /// model itself is the problem (overloaded, rate-limited, unavailable,
    /// unknown). Advances `next`.
    /// The model the next request goes to: the routed one in auto mode.
    pub fn current_model(&self) -> String {
        match &self.routing {
            Some(r) => r.model(self.route_tier).to_string(),
            None => self.model.clone(),
        }
    }

    /// Auto mode on the cheap model: move this run up to the strong one.
    /// Returns it, or `None` when there's nowhere to go.
    fn escalate(&mut self) -> Option<String> {
        let r = self.routing.as_ref()?;
        if self.route_tier == crate::routing::Tier::Strong || r.cheap == r.strong {
            return None;
        }
        self.route_tier = crate::routing::Tier::Strong;
        Some(r.strong.clone())
    }

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
        // `cd x &&` / `export PATH=…;` prefixes don't change what a command
        // does to the project, so one answer covers them all.
        let key = verdict_key(command);
        if let Some(v) = self.auto_verdicts.get(&key) {
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
        let model = self
            .utility_model
            .clone()
            .unwrap_or_else(|| self.current_model());
        let verdict = async {
            let mut rx = self
                .provider
                .chat(&messages, &[], &model, workspace_root)
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
        self.auto_verdicts.insert(key, safe);
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
        let old = std::mem::replace(&mut self.messages, new_messages);
        if self.messages.len() < old.len() {
            // A shorter history: archive it as an epoch so turns from before
            // can still be rewound to; its tail lives on in the new one.
            let kept = old
                .iter()
                .rev()
                .zip(self.messages.iter().rev())
                .take_while(|(a, b)| same_message(a, b))
                .count();
            self.undo.next_epoch(&old, kept);
        }
        // Same length (old tool output cleared): positions still line up.
        self.page_at_message.clear();
        // Compaction rewrites history, so every replayed provider block now
        // sits on a changed prefix and would be rejected or ignored.
        strip_provider_blocks_from(&mut self.messages, 0);
        self.last_prompt_tokens = 0;
        self.last_prompt_len = 0;
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
        self.compacted_this_turn = None;
        self.pruned_this_turn = false;
        // A run stopped mid-tool may have left calls without results; the
        // API refuses the whole history until each has one.
        dcode_ai_common::message::close_unanswered_tool_calls(&mut self.messages);
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
        if let Some(url) = crate::browser::last_url() {
            self.page_at_message.insert(self.messages.len(), url);
        }
        self.messages.push(user_msg);
        self.emit(AgentEvent::MessageReceived {
            role: "user".into(),
            content: preview,
        })
        .await;
        // Auto routing: this request's tier.
        if self.routing.is_some() {
            let (tier, why) = crate::routing::classify(user_input);
            self.route_tier = tier;
            self.emit(AgentEvent::Notice {
                message: format!("auto → {} ({why})", self.current_model()),
            })
            .await;
        }

        let mut turn = 0_u32;
        let mut empty_retries = 0_u32;
        let mut attachments_cleaned = attachments.is_empty();
        const MAX_EMPTY_RETRIES: u32 = 2;
        // Consecutive failures of the same tool — stops infinite retry loops.
        let mut consecutive_tool_failures: u32 = 0;
        let mut last_failed_tool: String = String::new();
        const MAX_CONSECUTIVE_TOOL_FAILURES: u32 = 3;
        // Loop guard: the same tool call repeated back to back.
        let mut last_call = String::new();
        let mut same_call_repeats = 0u32;
        const MAX_SAME_CALL: u32 = 4;

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
            let (spent, limit) = (
                self.cost_tracker.estimated_cost_usd(),
                self.cost_budget.get(),
            );
            if limit > 0.0 && spent >= limit * 0.8 && self.cost_budget.warn_once(limit) {
                self.emit(AgentEvent::Error {
                    message: format!(
                        "heads-up: {} of this session's {} budget is spent (/budget raises it)",
                        usd(spent),
                        usd(limit)
                    ),
                })
                .await;
            }
            if let Some(msg) = cost_budget_exceeded(spent, limit) {
                // Ask before going over (a run with nobody to ask stops).
                let go_on = if self.approval.should_fail_on_ask() {
                    false
                } else {
                    let raised = (limit * 1.5).max(spent * 1.2);
                    let call = ToolCall {
                        id: format!("budget-{}", turn),
                        name: "cost_budget".into(),
                        input: json!({"spent_usd": spent, "budget_usd": limit, "raise_to_usd": raised}),
                    };
                    let description = format!(
                        "{msg} — continue with the budget raised to {}? (No stops here.)",
                        usd(raised)
                    );
                    self.emit(AgentEvent::ApprovalRequested {
                        call_id: call.id.clone(),
                        tool: call.name.clone(),
                        description: description.clone(),
                    })
                    .await;
                    let ok = !matches!(
                        self.approval.resolve(&call, &description).await,
                        crate::approval::ApprovalVerdict::Denied
                    );
                    if ok {
                        self.cost_budget.set(raised);
                    }
                    ok
                };
                if !go_on {
                    self.emit(AgentEvent::Error {
                        message: msg.clone(),
                    })
                    .await;
                    self.undo.abort_turn();
                    return Err(ProviderError::Configuration(msg));
                }
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
            // Keep the prompt from ballooning during a long tool-heavy turn:
            // summarize first, then trim old tool output if still too big.
            if turn > 1 {
                self.maybe_compact(workspace_root).await;
            }
            self.maybe_trim_context().await;
            dcode_ai_common::message::close_unanswered_tool_calls(&mut self.messages);
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
            let mut model = self.current_model();
            let mut next_fallback = 0usize;
            let (assistant_text, assistant_reasoning, tool_calls, got_usage, replay_blocks) = loop {
                stream_attempt += 1;
                self.last_prompt_tokens = 0;
                self.last_prompt_len = self.messages.len();
                self.last_request_at = Some(std::time::Instant::now());
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
                        if !self.is_cancelled()
                            && let Some(up) = self.escalate()
                        {
                            self.emit(AgentEvent::Notice {
                                message: format!("{model} failed ({e}); auto → {up}"),
                            })
                            .await;
                            model = up;
                            stream_attempt = 0;
                            continue;
                        }
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
                let mut next_loop_check = 0usize;

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
                            // A model stuck repeating itself would burn the
                            // whole token budget: stop it, keep one copy.
                            if assistant_text.len() >= next_loop_check {
                                next_loop_check = assistant_text.len() + 256;
                                if let Some(keep) = crate::repetition::looping_tail(&assistant_text)
                                {
                                    assistant_text.truncate(keep);
                                    self.emit(AgentEvent::Notice {
                                        message: "The model started repeating itself, so \
                                                  dcode-ai stopped the reply."
                                            .into(),
                                    })
                                    .await;
                                    break;
                                }
                            }
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
                            self.last_prompt_tokens += input_tokens;
                            let context_tokens = self.context_tokens();
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
                            self.last_prompt_tokens += read_tokens + creation_tokens;
                            // A big write with no read: the cache missed.
                            if read_tokens == 0 && creation_tokens >= 1024 {
                                self.cold_requests += 1;
                                self.cold_tokens += creation_tokens;
                            }
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
                if let Some(up) = self.escalate() {
                    self.emit(AgentEvent::Notice {
                        message: format!("{model} failed ({message}); auto → {up}"),
                    })
                    .await;
                    model = up;
                    stream_attempt = 0;
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
                    // Auto routing: an empty cheap answer goes up a tier first.
                    if let Some(up) = self.escalate() {
                        self.emit(AgentEvent::Notice {
                            message: format!("{model} answered nothing; auto → {up}"),
                        })
                        .await;
                        continue;
                    }
                    empty_retries += 1;
                    let detail = empty_completion_detail(&model, got_usage);
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
            // Calls the user approved only in part (some hunks).
            let mut partial_edits: std::collections::HashSet<String> = Default::default();

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
                                    // A partial edit arrives as the file's
                                    // final content: write it (same
                                    // workspace checks as write_file).
                                    if let Some(obj) = patched.input.as_object_mut()
                                        && obj.remove(PARTIAL_WRITE_MARKER).is_some()
                                    {
                                        patched.name = "write_file".into();
                                        partial_edits.insert(call.id.clone());
                                    }
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
            if let Some(mirror) = &self.messages_mirror
                && to_execute.iter().any(|(_, c)| c.name == "spawn_subagent")
                && let Ok(mut m) = mirror.lock()
            {
                *m = self.messages.clone();
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
                    if call.name == "run_script" {
                        // Code Mode: the script's tool calls run here, through
                        // the same checks (it watches for cancel itself).
                        results[*i] = Some(self.run_script(call, workspace_root).await);
                        if self.is_cancelled() {
                            break;
                        }
                        continue;
                    }
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
                // Auto routing: a cheap model stuck on a tool moves up.
                if consecutive_tool_failures >= 2
                    && let Some(up) = self.escalate()
                {
                    self.emit(AgentEvent::Notice {
                        message: format!("{tool_name} keeps failing; auto → {up}"),
                    })
                    .await;
                }
            } else {
                consecutive_tool_failures = 0;
                last_failed_tool.clear();
            }

            let sees_images = self.provider.capabilities().supports_native_images;
            let mut screenshots: Vec<String> = Vec::new();
            for mut result in final_results {
                if result.success && partial_edits.contains(&result.call_id) {
                    result.output.push_str(
                        "\n\nThe user accepted only some hunks of this change; the file now \
                         holds their selection. Read it before editing it again.",
                    );
                }
                // A browser screenshot or an image an MCP tool sent: let a
                // model that can see images see it.
                if sees_images && result.success {
                    screenshots.extend(image_paths_in(&result.output));
                }
                if result.success
                    && let Some(capped) =
                        spill_tool_output(workspace_root, &result.call_id, &result.output)
                {
                    result.output = capped;
                }
                let mut content = format_tool_result(&result);
                // First touch of a subfolder brings its AGENTS.md / CLAUDE.md.
                if result.success
                    && let Some(path) =
                        tool_calls
                            .iter()
                            .find(|c| c.id == result.call_id)
                            .and_then(|c| {
                                c.input["path"]
                                    .as_str()
                                    .or_else(|| c.input["file_path"].as_str())
                            })
                    && let Some(extra) = crate::nested_instructions::for_path(
                        workspace_root,
                        path,
                        &mut self.nested_instructions,
                    )
                {
                    content.push_str(&extra);
                }
                self.messages
                    .push(Message::tool(result.call_id.clone(), content));
                // The event log and UI get the same capped text the model
                // sees: one 23 MB grep result made every resume crawl.
                result.output = cap_tool_output(&result.output);
                self.emit(AgentEvent::ToolCallCompleted {
                    call_id: result.call_id.clone(),
                    output: result,
                })
                .await;
            }

            for path in screenshots {
                self.messages.push(Message::user_with_parts(vec![
                    dcode_ai_common::message::ContentPart::Text {
                        text: format!("The image saved to {path}:"),
                    },
                    dcode_ai_common::message::ContentPart::Image {
                        media_type: match path.rsplit('.').next() {
                            Some("jpg" | "jpeg") => "image/jpeg",
                            Some("gif") => "image/gif",
                            Some("webp") => "image/webp",
                            _ => "image/png",
                        }
                        .into(),
                        path,
                    },
                ]));
            }

            if let Some(save) = &self.checkpoint {
                save(&self.messages);
            }

            for call in &tool_calls {
                let key = format!("{}:{}", call.name, call.input);
                if key == last_call {
                    same_call_repeats += 1;
                } else {
                    last_call = key;
                    same_call_repeats = 1;
                }
            }
            if let Some(msg) = loop_alarm(
                same_call_repeats,
                MAX_SAME_CALL,
                tool_calls.last().map(|c| c.name.as_str()),
            ) {
                self.emit(AgentEvent::Error {
                    message: msg.clone(),
                })
                .await;
                break msg;
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
            let context_tokens = self.context_tokens();
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

    /// The conversation's size right after this turn's mid-turn compaction,
    /// if one ran.
    pub fn compacted_this_turn(&self) -> Option<usize> {
        self.compacted_this_turn
    }

    /// Keep this conversation's prompt cache warm (see
    /// [`crate::provider::Provider::keep_warm`]); the request counts as the
    /// last one, and its tokens are added to the cost.
    pub async fn keep_warm(
        &mut self,
        workspace_root: &std::path::Path,
    ) -> Result<Option<crate::provider::KeepWarm>, crate::provider::ProviderError> {
        let tools = self.tool_definitions();
        let r = self
            .provider
            .keep_warm(
                &self.messages,
                &tools,
                &self.current_model(),
                workspace_root,
            )
            .await?;
        if let Some(k) = r {
            self.last_request_at = Some(std::time::Instant::now());
            self.cost_tracker.add(k.input, 0);
            self.cost_tracker.add_cache(k.cache_read, k.cache_write);
        }
        Ok(r)
    }

    /// How long since the last model request (`None` before the first).
    pub fn idle_for(&self) -> Option<std::time::Duration> {
        self.last_request_at.map(|t| t.elapsed())
    }

    /// The provider's count for the last request's prompt (0 when unknown).
    pub fn last_prompt_tokens(&self) -> u64 {
        self.last_prompt_tokens
    }

    /// How full the context is: the provider's count for the last request
    /// (it includes the system prompt and tool list) or the estimate,
    /// whichever is larger.
    pub fn context_tokens(&self) -> u64 {
        self.context_used()
    }

    /// The prompt's size as the next request would send it: the provider's
    /// count for the last request plus what was added since. Before any
    /// request (or right after a compaction) the local estimate of the
    /// history and the tool list.
    pub fn context_used(&self) -> u64 {
        let chars = |m: &[Message]| m.iter().map(|x| x.content.approx_chars()).sum::<usize>();
        if self.last_prompt_tokens > 0 {
            let tail = &self.messages[self.last_prompt_len.min(self.messages.len())..];
            return self.last_prompt_tokens + (chars(tail) / 4) as u64;
        }
        let tools: usize = self
            .tools
            .definitions()
            .iter()
            .map(|d| d.name.len() + d.description.len() + d.parameters.to_string().len())
            .sum();
        ((chars(&self.messages) + tools) / 4) as u64
    }

    /// Mid-turn context stages, between steps of a long turn. At the
    /// compaction point: first clear old tool outputs (no model call); if
    /// that brings the prompt back under the prune point, that's enough;
    /// otherwise summarize the older history with the cheap model. Each
    /// once a turn. Every rewrite of old history makes the provider re-read
    /// the whole prompt without its cache, so neither runs earlier. A
    /// compaction that doesn't shrink the history by 30% is thrown away and
    /// counts as a failure; after three in a row automatic compaction stops.
    async fn maybe_compact(&mut self, workspace_root: &Path) {
        use std::sync::atomic::Ordering;
        let Some(c) = self.compactor.clone() else {
            return;
        };
        if c.failures.load(Ordering::Relaxed) >= MAX_COMPACTION_FAILURES
            || self.compacted_this_turn.is_some()
            || (self.context_used() as usize) < c.compact_at
        {
            return;
        }
        if !self.pruned_this_turn
            && !c.pruned.load(Ordering::Relaxed)
            && let Some(prune) = &c.prune
        {
            self.pruned_this_turn = true;
            // The last few messages are what the model is acting on now.
            let protect_from = self.messages.len().saturating_sub(6);
            if let Some((pruned, freed)) = prune(&self.messages, protect_from) {
                c.pruned.store(true, Ordering::Relaxed);
                self.compact_messages(pruned);
                self.emit(AgentEvent::ContextCompaction {
                    phase: "completed".into(),
                    message: format!("Cleared old tool output (~{}k tokens)", freed / 1000),
                    context_tokens: self.context_used(),
                })
                .await;
                if (self.context_used() as usize) <= c.prune_at {
                    return;
                }
            }
        }
        let before_used = self.context_used();
        let Some(prompt) = (c.prompt)(&self.messages) else {
            return;
        };
        self.emit(AgentEvent::ContextCompaction {
            phase: "starting".into(),
            message: "Compacting conversation…".into(),
            context_tokens: before_used,
        })
        .await;
        let summary = match self
            .provider
            .chat(&[Message::user(prompt)], &[], &c.model, workspace_root)
            .await
        {
            Ok(mut stream) => {
                let mut text = String::new();
                while let Some(chunk) = stream.recv().await {
                    match chunk {
                        StreamChunk::TextDelta(d) => text.push_str(&d),
                        StreamChunk::Done | StreamChunk::Error(_) | StreamChunk::FatalError(_) => {
                            break;
                        }
                        _ => {}
                    }
                }
                text
            }
            Err(e) => {
                tracing::warn!("mid-turn compaction summary failed: {e}");
                String::new()
            }
        };
        let before = (c.estimate)(&self.messages);
        self.compacted_this_turn = Some(before);
        let compacted = (c.apply)(&self.messages, &summary);
        let after = (c.estimate)(&compacted);
        if !compaction_worked(before, after) {
            let n = c.failures.fetch_add(1, Ordering::Relaxed) + 1;
            self.emit(AgentEvent::ContextCompaction {
                phase: "completed".into(),
                message: if n >= MAX_COMPACTION_FAILURES {
                    "Compaction couldn't free enough room; auto-compact paused (/compact to try again)"
                        .into()
                } else {
                    "Compaction wouldn't free enough room; kept the conversation as it was".into()
                },
                context_tokens: before_used,
            })
            .await;
            return;
        }
        c.failures.store(0, Ordering::Relaxed);
        c.pruned.store(false, Ordering::Relaxed);
        self.compact_messages(compacted);
        self.compacted_this_turn = Some(after);
        self.emit(AgentEvent::ContextCompaction {
            phase: "completed".into(),
            message: format!(
                "Compacted: ~{}k → ~{}k tokens (summary, your recent messages, files changed)",
                before_used / 1000,
                self.context_used() / 1000
            ),
            context_tokens: self.context_used(),
        })
        .await;
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

    /// Restore files to before the turn at message `pos` of history
    /// `epoch` (from before a compaction).
    /// What rewinding files to `(epoch, pos)` would change, per file.
    pub fn rewind_preview_at(&self, epoch: u32, pos: usize) -> Vec<(std::path::PathBuf, String)> {
        self.undo.rewind_preview_at(epoch, pos)
    }

    pub fn rewind_files_to_at(&mut self, epoch: u32, pos: usize) -> Result<usize, String> {
        self.undo.rewind_to_at(epoch, pos)
    }

    /// Keep turn checkpoints (and archived history) in `dir`, loading any
    /// already there: undo and rewind survive restarts and resumes.
    pub fn attach_checkpoints(&mut self, dir: std::path::PathBuf) {
        self.undo.attach(dir);
    }

    /// The current history epoch (one per compaction that shortened it).
    pub fn history_epoch(&self) -> u32 {
        self.undo.epoch()
    }

    /// The history as it was in an earlier `epoch`, and how many of its last
    /// messages carried on into the next.
    pub fn archived_history(&self, epoch: u32) -> Option<(Vec<Message>, usize)> {
        self.undo.archived(epoch)
    }

    /// Make an archived epoch's history (up to a point) the conversation.
    pub fn restore_archived_history(&mut self, epoch: u32, messages: Vec<Message>) {
        self.messages = messages;
        self.page_at_message.clear();
        strip_provider_blocks_from(&mut self.messages, 0);
        self.last_prompt_tokens = 0;
        self.last_prompt_len = 0;
        self.undo.resume_epoch(epoch);
    }

    /// The browser page that was open when the user message at `pos` was
    /// sent, if the browser was in use then.
    pub fn page_at_message(&self, pos: usize) -> Option<String> {
        self.page_at_message.get(&pos).cloned()
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

    /// Use handles the UI already holds (after it swapped in a resumed
    /// session), so Esc and mid-turn messages keep reaching this loop.
    pub fn adopt_turn_handles(&mut self, cancel: Arc<AtomicBool>, steer: SteerInbox) {
        self.cancel_flag = cancel;
        self.steer = steer;
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

    /// Runs a `run_script` call: the script on its own thread, each tool it
    /// calls here — through the permission checks (asking the user when the
    /// mode needs it), hooks, undo checkpoints, redaction and events like any
    /// call. Read-only batches from `parallel()` run at once.
    async fn run_script(&mut self, call: &ToolCall, workspace_root: &Path) -> ToolResult {
        use crate::tools::run_script::{self as rs, ScriptMsg};
        let fail = |e: String| ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(e),
        };
        let Some(code) = call.input.get("code").and_then(|v| v.as_str()) else {
            return fail("run_script needs `code`".into());
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let code = code.to_string();
        if let Err(e) = rs::spawn(code, tx) {
            return fail(format!("couldn't start the script: {e}"));
        }
        let mut n = 0u32;
        let mut deadline = Instant::now() + rs::MAX_TIME;
        loop {
            let msg = tokio::select! {
                m = rx.recv() => m,
                _ = self.cancelled() => return fail("cancelled".into()),
                // Computing past the limit with no tool calls: stopped
                // (dropping `rx` ends a script child).
                _ = tokio::time::sleep_until(deadline.into()) => {
                    return fail(format!("the script ran past {}s", rs::MAX_TIME.as_secs()));
                }
            };
            match msg {
                Some(ScriptMsg::Calls { calls, reply }) => {
                    let mut waited = Duration::ZERO;
                    // Permission checks one by one (they may ask the user).
                    let mut ready: Vec<Result<ToolCall, String>> = Vec::with_capacity(calls.len());
                    let mut ids = Vec::with_capacity(calls.len());
                    for (name, args) in calls {
                        n += 1;
                        self.script_stats.1 += 1;
                        let inner = ToolCall {
                            id: format!("{}.{n}", call.id),
                            name,
                            input: args,
                        };
                        ids.push(inner.id.clone());
                        let (prepared, w) = self.script_prepare(inner, workspace_root).await;
                        waited += w;
                        ready.push(prepared);
                        if self.is_cancelled() {
                            return fail("cancelled".into());
                        }
                    }
                    deadline += waited;
                    let left = deadline.saturating_duration_since(Instant::now());
                    let results = self.script_execute(&ready, left).await;
                    if self.is_cancelled() {
                        return fail("cancelled".into());
                    }
                    let mut answers = Vec::with_capacity(results.len());
                    for ((id, prepared), result) in ids.iter().zip(&ready).zip(results) {
                        let answer = self.script_finish(id, prepared, result).await;
                        self.script_stats.2 += match &answer {
                            Ok(t) | Err(t) => t.chars().count() as u64,
                        };
                        answers.push(answer);
                    }
                    let _ = reply.send((answers, waited));
                }
                Some(ScriptMsg::Done(Ok(out))) => {
                    self.script_stats.0 += 1;
                    self.script_stats.3 += out.chars().count() as u64;
                    return ToolResult {
                        call_id: call.id.clone(),
                        success: true,
                        output: if out.trim().is_empty() {
                            format!(
                                "(script finished; {n} tool calls, nothing printed or returned)"
                            )
                        } else {
                            out
                        },
                        error: None,
                    };
                }
                Some(ScriptMsg::Done(Err(e))) => return fail(e),
                None => return fail("the script stopped unexpectedly".into()),
            }
        }
    }

    /// Before one of a script's calls runs: shown as started, permission
    /// checked (asking the user if needed), PreToolUse hooks, undo
    /// checkpoint. Returns the call to run (maybe with an edited input) or
    /// why it can't, and how long the user took to answer.
    async fn script_prepare(
        &mut self,
        call: ToolCall,
        workspace_root: &Path,
    ) -> (Result<ToolCall, String>, Duration) {
        let mut waited = Duration::ZERO;
        self.emit(AgentEvent::ToolCallStarted {
            call_id: call.id.clone(),
            tool: call.name.clone(),
            input: call.input.clone(),
        })
        .await;
        if call.name == "run_script" {
            return (Err("scripts can't start scripts".into()), waited);
        }
        let mut tier = self.approval.check(&call.name, &call.input.to_string());
        if tier == PermissionTier::Ask
            && let Some(command) = self
                .approval
                .auto_review(&call.name, &call.input.to_string())
            && self.auto_mode_safe(&command, workspace_root).await
        {
            tier = PermissionTier::Allowed;
        }
        let mut call = call;
        match tier {
            PermissionTier::Allowed => {}
            PermissionTier::Denied => {
                return (
                    Err(format!("tool `{}` denied by policy", call.name)),
                    waited,
                );
            }
            PermissionTier::Ask => {
                let description = format!("Tool `{}` (from a script) requires approval", call.name);
                self.emit(AgentEvent::ApprovalRequested {
                    call_id: call.id.clone(),
                    tool: call.name.clone(),
                    description: description.clone(),
                })
                .await;
                let asked = Instant::now();
                let verdict = self.approval.resolve(&call, &description).await;
                waited = asked.elapsed();
                if let Some(hooks) = &self.hooks {
                    hooks
                        .run_best_effort(
                            HookEventKind::ApprovalRequested,
                            Some(&call.name),
                            &json!({"call_id": call.id, "tool": call.name,
                                    "input": call.input, "description": description}),
                        )
                        .await;
                }
                let approved = verdict.is_approved();
                self.emit(AgentEvent::ApprovalResolved {
                    call_id: call.id.clone(),
                    approved,
                })
                .await;
                match verdict {
                    ApprovalVerdict::AllowPattern(pattern) => {
                        self.approval.add_session_allow(pattern)
                    }
                    ApprovalVerdict::ApprovedModified(input) => {
                        call.input = input;
                        if let Some(obj) = call.input.as_object_mut()
                            && obj.remove(PARTIAL_WRITE_MARKER).is_some()
                        {
                            call.name = "write_file".into();
                        }
                    }
                    _ => {}
                }
                if !approved {
                    let why = if self.approval.should_fail_on_ask() {
                        format!(
                            "`{}` needs approval and this run is headless; use a mode such as `dont-ask` or `bypass-permissions`",
                            call.name
                        )
                    } else {
                        format!("`{}` needs approval; the request was denied", call.name)
                    };
                    return (Err(why), waited);
                }
            }
        }
        if let Some(hooks) = &self.hooks
            && let Err(reason) = hooks
                .run(
                    HookEventKind::PreToolUse,
                    Some(&call.name),
                    &json!({"call_id": call.id, "tool": call.name, "input": call.input}),
                )
                .await
        {
            return (Err(reason), waited);
        }
        self.undo.record_tool_call(&call, workspace_root);
        (Ok(call), waited)
    }

    /// One script call, failed when it runs past what's left of the deadline.
    async fn script_call_timed(&self, call: &ToolCall, left: Duration) -> ToolResult {
        match tokio::time::timeout(left, self.tools.execute(call)).await {
            Ok(r) => r,
            Err(_) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!(
                    "the script ran past {}s",
                    crate::tools::run_script::MAX_TIME.as_secs()
                )),
            },
        }
    }

    /// Runs a script's prepared calls — all at once when every one is
    /// read-only, else in order — each bounded by `left`.
    async fn script_execute(
        &self,
        ready: &[Result<ToolCall, String>],
        left: Duration,
    ) -> Vec<Option<ToolResult>> {
        let timed = |call| self.script_call_timed(call, left);
        let mut results: Vec<Option<ToolResult>> = (0..ready.len()).map(|_| None).collect();
        let parallel = ready
            .iter()
            .all(|r| r.as_ref().map_or(true, |c| concurrency_safe(&c.name)));
        if parallel {
            let mut futures: FuturesUnordered<_> = ready
                .iter()
                .enumerate()
                .filter_map(|(i, r)| r.as_ref().ok().map(|c| (i, c)))
                .map(|(i, c)| {
                    let fut = timed(c);
                    async move { (i, fut.await) }
                })
                .collect();
            loop {
                tokio::select! {
                    next = futures.next() => match next {
                        Some((i, r)) => results[i] = Some(r),
                        None => break,
                    },
                    _ = self.cancelled() => break,
                }
            }
        } else {
            for (i, r) in ready.iter().enumerate() {
                let Ok(c) = r else { continue };
                tokio::select! {
                    r = timed(c) => results[i] = Some(r),
                    _ = self.cancelled() => break,
                }
            }
        }
        results
    }

    /// After one of a script's calls: redaction, undo bookkeeping,
    /// PostToolUse hooks, the completed event. Returns what the script sees.
    async fn script_finish(
        &mut self,
        id: &str,
        prepared: &Result<ToolCall, String>,
        result: Option<ToolResult>,
    ) -> Result<String, String> {
        let call = match prepared {
            Ok(c) => c,
            Err(e) => {
                // Refused before running: shown as a failed call.
                self.emit(AgentEvent::ToolCallCompleted {
                    call_id: id.to_string(),
                    output: ToolResult {
                        call_id: id.to_string(),
                        success: false,
                        output: String::new(),
                        error: Some(e.clone()),
                    },
                })
                .await;
                return Err(e.clone());
            }
        };
        let mut result = result.unwrap_or_else(|| ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some("cancelled".into()),
        });
        dcode_ai_common::redact::redact_tool_result(&mut result);
        self.undo
            .note_results(std::slice::from_ref(call), std::slice::from_ref(&result));
        if let Some(hooks) = &self.hooks {
            let kind = if result.success {
                HookEventKind::PostToolUse
            } else {
                HookEventKind::PostToolFailure
            };
            hooks
                .run_best_effort(
                    kind,
                    None,
                    &json!({"call_id": result.call_id, "success": result.success,
                            "output": result.output, "error": result.error}),
                )
                .await;
        }
        let answer = if result.success {
            Ok(result.output.clone())
        } else {
            Err(result
                .error
                .clone()
                .unwrap_or_else(|| format!("`{}` failed", call.name)))
        };
        result.output = cap_tool_output(&result.output);
        self.emit(AgentEvent::ToolCallCompleted {
            call_id: result.call_id.clone(),
            output: result,
        })
        .await;
        answer
    }

    /// Returns a future that completes when cancellation is requested.
    /// Polls the cancel flag at 50ms intervals.
    async fn cancelled(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Images a tool result points at: a browser screenshot (the whole output)
/// or `Saved an image to <path> (…)` lines (MCP images); at most 4.
fn image_paths_in(output: &str) -> Vec<String> {
    if let Some(path) = output.strip_prefix("Saved a screenshot to ") {
        return vec![path.trim().to_string()];
    }
    output
        .lines()
        .filter_map(|l| l.strip_prefix("Saved an image to "))
        .filter_map(|rest| rest.split(" (").next())
        .map(|p| p.trim().to_string())
        .filter(|p| {
            p.starts_with(".dcode-ai/mcp-files/")
                && [".png", ".jpg", ".jpeg", ".gif", ".webp"]
                    .iter()
                    .any(|e| p.ends_with(e))
        })
        .take(4)
        .collect()
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

/// The part of a shell command that decides its safety: leading
/// `export NAME=value` steps (joined by `&&` or `;`) are dropped, `cd` steps
/// are kept (where a command runs matters) but written the same way.
fn verdict_key(command: &str) -> String {
    let mut rest = command.trim();
    let mut dirs: Vec<&str> = Vec::new();
    loop {
        let (head, tail) = match (rest.find("&&"), rest.find(';')) {
            (Some(a), Some(b)) if b < a => (&rest[..b], &rest[b + 1..]),
            (Some(a), _) => (&rest[..a], &rest[a + 2..]),
            (None, Some(b)) => (&rest[..b], &rest[b + 1..]),
            (None, None) => break,
        };
        let h = head.trim();
        // No command substitution anywhere in a step we look past.
        if h.contains("$(") || h.contains('`') {
            break;
        }
        if h.starts_with("cd ") {
            dirs.push(h);
        } else if !(h.starts_with("export ") && (!h.contains('$') || h.starts_with("export PATH=")))
        {
            break;
        }
        rest = tail.trim();
    }
    dirs.push(rest);
    dirs.join(" && ")
}

/// Upper bound on a single tool result's size in the conversation (~4k tokens).
/// Huge outputs (large file reads, `git log`, verbose command output) otherwise
/// accumulate across a long turn until the prompt balloons and every model call
/// slows to a crawl (the "stuck for minutes" symptom). Anything larger is
/// middle-elided, keeping the informative head and tail; the agent loop saves
/// the full text to a file first (see [`spill_tool_output`]).
const MAX_TOOL_OUTPUT_CHARS: usize = 16_000;

pub fn cap_tool_output(s: &str) -> String {
    cap_tool_output_with(
        s,
        "re-run with a narrower query/range if you need the omitted part",
    )
}

/// [`cap_tool_output`] with `hint` saying how to get the omitted part. The
/// result (hint included) stays within the cap, so capping again is a no-op.
fn cap_tool_output_with(s: &str, hint: &str) -> String {
    // Byte length bounds the char count; skip counting for the common case.
    if s.len() <= MAX_TOOL_OUTPUT_CHARS {
        return s.to_string();
    }
    let total = s.chars().count();
    if total <= MAX_TOOL_OUTPUT_CHARS {
        return s.to_string();
    }
    let room = MAX_TOOL_OUTPUT_CHARS.saturating_sub(hint.chars().count() + 120);
    let head = room * 2 / 3;
    let tail = room - head;
    let chars: Vec<char> = s.chars().collect();
    let head_str: String = chars[..head].iter().collect();
    let tail_str: String = chars[chars.len() - tail..].iter().collect();
    let omitted = total - head - tail;
    format!(
        "{head_str}\n\n… [{omitted} characters left out to keep the context small — {hint}] …\n\n{tail_str}"
    )
}

/// Marker in an approved input that carries a partial edit as the file's
/// final content (see the TUI's hunk picker).
const PARTIAL_WRITE_MARKER: &str = "__dcode_partial_write";

/// Why a turn should pause: it sent the same tool call `max_same` times in
/// a row (a loop). Long turns are never paused for their size.
fn loop_alarm(same_call_repeats: u32, max_same: u32, tool: Option<&str>) -> Option<String> {
    if same_call_repeats >= max_same {
        return Some(format!(
            "Paused: `{}` ran {same_call_repeats} times in a row with the same input, which \
             looks like a loop. Send \"continue\" to keep going, or say what to do instead.",
            tool.unwrap_or("a tool")
        ));
    }
    None
}

/// Spilled outputs kept per workspace; older ones are deleted.
const SPILL_KEEP: usize = 50;

/// When a tool output is too big for the conversation, save all of it under
/// `.dcode-ai/tool-output/` and cap it with a pointer there, so nothing is
/// lost and the agent reads only the part it needs.
fn spill_tool_output(workspace_root: &Path, call_id: &str, output: &str) -> Option<String> {
    if output.chars().count() <= MAX_TOOL_OUTPUT_CHARS {
        return None;
    }
    let dir = workspace_root.join(".dcode-ai").join("tool-output");
    std::fs::create_dir_all(&dir).ok()?;
    let safe: String = call_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    let name = format!("{safe}.txt");
    std::fs::write(dir.join(&name), output).ok()?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
            .flatten()
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        if files.len() > SPILL_KEEP {
            files.sort();
            for (_, path) in &files[..files.len() - SPILL_KEEP] {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    let rel = format!(".dcode-ai/tool-output/{name}");
    let lines = output.lines().count();
    Some(cap_tool_output_with(
        output,
        &format!(
            "the full output ({lines} lines) is in {rel}; read_file it with a line range for the rest"
        ),
    ))
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
/// Cents for normal amounts, more digits for tiny ones.
fn usd(v: f64) -> String {
    if v < 1.0 {
        format!("${v:.4}")
    } else {
        format!("${v:.2}")
    }
}

/// A session's cost budget (USD; 0 = none), shared between the agent and
/// whoever changes it (`/budget`).
#[derive(Debug, Clone, Default)]
pub struct CostBudget {
    bits: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The budget the 80 % heads-up was given for.
    warned: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl CostBudget {
    pub fn get(&self) -> f64 {
        f64::from_bits(self.bits.load(std::sync::atomic::Ordering::Relaxed))
    }

    pub fn set(&self, usd: f64) {
        self.bits
            .store(usd.max(0.0).to_bits(), std::sync::atomic::Ordering::Relaxed);
    }

    /// True the first time it's asked for this budget.
    fn warn_once(&self, limit: f64) -> bool {
        self.warned
            .swap(limit.to_bits(), std::sync::atomic::Ordering::Relaxed)
            != limit.to_bits()
    }
}

fn cost_budget_exceeded(spent: f64, limit: f64) -> Option<String> {
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
#[path = "agent_tests.rs"]
mod tests;

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
