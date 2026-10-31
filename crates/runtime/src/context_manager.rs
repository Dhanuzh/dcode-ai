//! Context management for preventing context window overflow.
//!
//! This module provides:
//! - Token counting and context size tracking
//! - Message windowing (sliding window of recent messages)
//! - Automatic summarization when context approaches limits
//! - Preservation of critical messages (system prompt, memory, etc.)

use dcode_ai_common::message::{ContentPart, Message, MessageContent, Role};
use serde::{Deserialize, Serialize};

/// Configuration for context management behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextManagerConfig {
    /// Target context size in tokens (rough approximation).
    /// When context exceeds this, we'll start compacting.
    pub context_window_target: usize,
    /// Maximum messages to retain after compaction.
    /// This is the "sliding window" size for recent conversation.
    pub max_retained_messages: usize,
    /// Percentage (0-100) of context window that triggers auto-summarize.
    /// E.g., 75 means start summarizing when at 75% of context_window_target.
    pub auto_summarize_threshold: u8,
    /// Enable automatic context summarization.
    pub enable_auto_summarize: bool,
    /// Maximum tokens per message before truncation during summary.
    pub max_message_chars_for_summary: usize,
}

impl Default for ContextManagerConfig {
    fn default() -> Self {
        Self {
            // Default to ~32k tokens target (rough: chars / 4 ≈ tokens)
            context_window_target: 32_000,
            // Keep last 50 messages + system + memory
            max_retained_messages: 50,
            // Start summarizing at 75% of target
            auto_summarize_threshold: 75,
            enable_auto_summarize: true,
            // Truncate very long messages when preparing for summary
            max_message_chars_for_summary: 10_000,
        }
    }
}

/// Context statistics exposed via the CLI/API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextStats {
    /// Model name
    pub model: String,
    /// Model context window in tokens
    pub context_window: usize,
    /// Estimated total tokens in context
    pub estimated_tokens: usize,
    /// Number of messages in context
    pub message_count: usize,
    /// Percentage of context window used (0-100)
    pub usage_percent: u8,
    /// Whether context is approaching limit
    pub needs_attention: bool,
    /// Whether auto-summarize should trigger
    pub should_summarize: bool,
}

/// First line of a compaction summary message.
pub const SUMMARY_HEADER: &str = "## Conversation Summary (Earlier Context)";

/// A system message holding an earlier compaction summary.
pub fn is_summary(message: &Message) -> bool {
    message.role == Role::System
        && matches!(&message.content, MessageContent::Text(t) if t.starts_with(SUMMARY_HEADER))
}

/// Longest a merged summary may get (characters, newest part kept).
const MERGED_SUMMARY_MAX_CHARS: usize = 16_000;

/// Safety net: a conversation never carries more than one compaction
/// summary. Several (from an older version, or anything else) are merged
/// into one, without a model call: their texts in order, keeping the newest
/// part when too long. Returns `None` when there was nothing to merge.
pub fn merge_stacked_summaries(messages: &[Message]) -> Option<Vec<Message>> {
    let positions: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| is_summary(m))
        .map(|(i, _)| i)
        .collect();
    if positions.len() < 2 {
        return None;
    }
    let bodies: Vec<String> = positions
        .iter()
        .map(|&i| {
            let text = messages[i].content.to_summary_text();
            text.strip_prefix(SUMMARY_HEADER)
                .unwrap_or(&text)
                .trim()
                .to_string()
        })
        .collect();
    let joined = bodies.join("\n\n---\n\n");
    let total = joined.chars().count();
    let body: String = if total > MERGED_SUMMARY_MAX_CHARS {
        let tail: String = joined
            .chars()
            .skip(total - MERGED_SUMMARY_MAX_CHARS)
            .collect();
        format!("(older summaries shortened)\n…{tail}")
    } else {
        joined
    };
    let mut out = Vec::with_capacity(messages.len() - positions.len() + 1);
    for (i, m) in messages.iter().enumerate() {
        if i == positions[0] {
            out.push(Message::system(format!("{SUMMARY_HEADER}\n\n{body}")));
        } else if !positions.contains(&i) {
            out.push(m.clone());
        }
    }
    Some(out)
}

/// Where the context stages happen in the model's window (M31): prune old
/// tool output, then compact. One window for the footer, `/context` and the
/// triggers, compared with the provider's measured prompt size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    /// The model's input window (200k working window, or 1M for `[1m]`).
    pub window: usize,
    /// Room kept for the reply.
    pub output_reserve: usize,
    /// Room kept for the summary request itself.
    pub buffer: usize,
    /// `[memory.context] auto_compact_at` (% of the window); `None` = automatic.
    pub compact_at_percent: Option<u8>,
}

impl ContextBudget {
    pub fn new(window: usize, max_output: usize, compact_at_percent: Option<u8>) -> Self {
        let window = window.max(8_000);
        Self {
            window,
            output_reserve: max_output.min(32_000).min(window / 4),
            buffer: (window * 75 / 1000).clamp(13_000.min(window / 5), window / 5),
            compact_at_percent: compact_at_percent.filter(|p| (10..=95).contains(p)),
        }
    }

    /// Compact once the prompt reaches this: the window minus the reply and
    /// the summary request, kept between 60% and 90% of the window.
    pub fn compact_at(&self) -> usize {
        match self.compact_at_percent {
            Some(p) => self.window * p as usize / 100,
            None => (self.window - self.output_reserve - self.buffer)
                .clamp(self.window * 60 / 100, self.window * 90 / 100),
        }
    }

    /// Clear old tool output (no model call) from here.
    pub fn prune_at(&self) -> usize {
        self.compact_at().saturating_sub(self.window / 10)
    }

    /// Hard ceiling for the agent's last-resort trimming: the window minus
    /// the reply.
    pub fn hard_limit(&self) -> usize {
        self.window - self.output_reserve
    }

    /// What a compaction aims to leave.
    pub fn target_after(&self) -> usize {
        self.window * 35 / 100
    }

    pub fn percent(&self, tokens: usize) -> u8 {
        (tokens.min(self.window) * 100 / self.window) as u8
    }
}

/// The session's auto-compact point as % of the window, for the footer
/// (0 = not set yet: the UI works it out from the model).
static AUTO_COMPACT_PERCENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_auto_compact_percent(p: u8) {
    AUTO_COMPACT_PERCENT.store(p, std::sync::atomic::Ordering::Relaxed);
}

/// Where automatic compaction happens for `model`, as % of its window.
pub fn auto_compact_percent(model: &str) -> u8 {
    match AUTO_COMPACT_PERCENT.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {
            let b = ContextBudget::new(
                crate::model_limits::effective_context_window(model),
                crate::model_limits::detect_max_output_tokens(model),
                None,
            );
            b.percent(b.compact_at())
        }
        p => p,
    }
}

/// Start of a cleared tool output.
pub const PRUNED_MARK: &str = "[output cleared to save context";
/// Newest tool output kept when pruning (tokens).
pub const PRUNE_KEEP_TOKENS: usize = 40_000;
/// Pruning is only worth it when it frees this much.
pub const PRUNE_MIN_FREE: usize = 20_000;

/// Replace old tool outputs with a one-line stub, keeping the newest
/// `keep_tokens` of tool output and every message from `protect_from` on.
/// Returns the new history and the tokens freed, or `None` when it would
/// free less than `min_free` (opencode's prune; Claude Code's micro-compact).
pub fn prune_tool_outputs(
    messages: &[Message],
    protect_from: usize,
    keep_tokens: usize,
    min_free: usize,
) -> Option<(Vec<Message>, usize)> {
    let names: std::collections::HashMap<&str, &str> = messages
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();
    let mut out = messages.to_vec();
    let mut kept = 0usize;
    let mut freed = 0usize;
    for i in (0..messages.len()).rev() {
        let m = &messages[i];
        if m.role != Role::Tool {
            continue;
        }
        let text = m.content.to_summary_text();
        if text.starts_with(PRUNED_MARK) {
            continue;
        }
        let tokens = ContextManager::estimate_tokens(m);
        if i >= protect_from || kept + tokens <= keep_tokens || tokens < 200 {
            kept += tokens;
            continue;
        }
        let tool = m
            .tool_call_id
            .as_deref()
            .and_then(|id| names.get(id))
            .copied()
            .unwrap_or("tool");
        let stub = format!(
            "{PRUNED_MARK}: {tool} output, {} lines. Run it again if you need it.]",
            text.lines().count()
        );
        out[i].content = MessageContent::Text(stub);
        out[i].provider_blocks = None;
        freed += tokens.saturating_sub(ContextManager::estimate_tokens(&out[i]));
    }
    (freed >= min_free).then_some((out, freed))
}

/// Paths changed by tool calls in `messages` (for the summary's file list).
fn changed_paths(messages: &[Message]) -> Vec<String> {
    const WRITERS: [&str; 7] = [
        "write_file",
        "edit_file",
        "apply_patch",
        "notebook_edit",
        "delete_path",
        "move_path",
        "rename_path",
    ];
    let mut paths: Vec<String> = Vec::new();
    for call in messages
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
    {
        if !WRITERS.contains(&call.name.as_str()) {
            continue;
        }
        let args = &call.arguments;
        let path = ["path", "file_path", "from", "source"]
            .iter()
            .find_map(|k| args[*k].as_str());
        if let Some(p) = path
            && !paths.iter().any(|x| x == p)
        {
            paths.push(p.to_string());
        }
    }
    paths
}

/// Most of the user's own words carried into a compaction (tokens).
pub const KEEP_USER_MESSAGES_TOKENS: usize = 20_000;

/// The user's own messages in `messages`, newest first, up to `budget`
/// tokens, returned in their original order (Codex keeps these verbatim).
fn recent_user_messages(messages: &[Message], budget: usize) -> Vec<String> {
    let mut picked = Vec::new();
    let mut used = 0usize;
    for m in messages.iter().rev() {
        if m.role != Role::User {
            continue;
        }
        let text = m.content.to_summary_text();
        let text = text.trim();
        // Skip tool-output carriers and app notes.
        if text.is_empty() || text.starts_with('<') || text.starts_with("The browser screenshot") {
            continue;
        }
        let tokens = crate::token_count::count_tokens(text);
        if used + tokens > budget {
            break;
        }
        used += tokens;
        picked.push(text.to_string());
    }
    picked.reverse();
    picked
}

/// Manages conversation context to prevent token overflow.
///
/// The context manager maintains:
/// - A sliding window of recent user/assistant messages
/// - Preservation of system messages (always at start)
/// - Track tool messages that follow the relevant turns
#[derive(Clone)]
pub struct ContextManager {
    config: ContextManagerConfig,
    model: String,
    /// Prompt tokens outside the conversation (system prompt, tool list)
    /// already reserved out of `context_window_target`.
    prompt_overhead: usize,
    budget: ContextBudget,
}

impl ContextManager {
    pub fn new(config: ContextManagerConfig, model: String) -> Self {
        let budget = ContextBudget::new(config.context_window_target, 8_000, None);
        Self {
            config,
            model,
            prompt_overhead: 0,
            budget,
        }
    }

    /// Where pruning and compaction happen; set by the supervisor.
    pub fn set_budget(&mut self, budget: ContextBudget) {
        self.budget = budget;
    }

    pub fn budget(&self) -> ContextBudget {
        self.budget
    }

    pub fn with_default_config(model: String) -> Self {
        Self::new(ContextManagerConfig::default(), model)
    }

    /// Calculate estimated token count for a message.
    ///
    /// Backed by the `o200k_base` BPE (see [`crate::token_count`]) rather than
    /// the old `chars / 4` heuristic, so code- and JSON-heavy turns are budgeted
    /// accurately. Image parts contribute via their summary text only; their
    /// true vision-token cost comes back in the provider `Usage` event.
    pub fn estimate_tokens(message: &Message) -> usize {
        let content_tokens = crate::token_count::count_tokens(&message.content.to_summary_text());

        // Add overhead for tool calls
        let tool_call_overhead = message
            .tool_calls
            .as_ref()
            .map(|calls| calls.len() * 50) // ~50 tokens per tool call structure
            .unwrap_or(0);

        content_tokens + tool_call_overhead + 10 // ~10 tokens base overhead
    }

    /// Calculate estimated token count for a slice of messages.
    pub fn estimate_tokens_for_slice(messages: &[Message]) -> usize {
        messages.iter().map(Self::estimate_tokens).sum()
    }

    /// Get current context statistics.
    pub fn stats(&self, messages: &[Message]) -> ContextStats {
        let estimated_tokens = Self::estimate_tokens_for_slice(messages);
        let message_count = messages.len();

        let context_window = self.config.context_window_target.max(1);
        let usage_percent =
            ((estimated_tokens as f64 / context_window as f64) * 100.0).min(100.0) as u8;

        let needs_attention = usage_percent >= 80;
        let should_summarize = self.config.enable_auto_summarize
            && usage_percent >= self.config.auto_summarize_threshold;

        ContextStats {
            model: self.model.clone(),
            context_window,
            estimated_tokens,
            message_count,
            usage_percent,
            needs_attention,
            should_summarize,
        }
    }

    /// Tokens of every request outside the conversation that the window
    /// target already leaves room for.
    pub fn set_prompt_overhead(&mut self, tokens: usize) {
        self.prompt_overhead = tokens;
    }

    pub fn prompt_overhead(&self) -> usize {
        self.prompt_overhead
    }

    /// Find the contiguous leading system-message prefix (the system
    /// prompt). These messages are preserved during compaction; earlier
    /// compaction summaries are not part of it — the next compaction folds
    /// them into its own summary instead of stacking another one.
    pub fn leading_system_prefix_len(messages: &[Message]) -> usize {
        messages
            .iter()
            .take_while(|m| m.role == Role::System && !is_summary(m))
            .count()
    }

    /// Adjust a cutoff index so it never lands inside a tool_use/tool_result group.
    /// If the message at `cutoff` is a Role::Tool, walk backwards to include the
    /// preceding assistant message that contains the matching tool_calls.
    /// `min_cutoff` prevents walking past system messages.
    fn adjust_cutoff_for_tool_groups(
        messages: &[Message],
        mut cutoff: usize,
        min_cutoff: usize,
    ) -> usize {
        while cutoff > min_cutoff && cutoff < messages.len() && messages[cutoff].role == Role::Tool
        {
            cutoff -= 1;
        }
        cutoff
    }

    /// Find the earliest index that retains the last `keep_non_system`
    /// non-system messages while preserving order.
    fn recent_start_by_non_system(
        messages: &[Message],
        prefix_len: usize,
        keep_non_system: usize,
    ) -> usize {
        if keep_non_system == 0 {
            return messages.len();
        }
        let mut seen = 0usize;
        for idx in (prefix_len..messages.len()).rev() {
            if messages[idx].role != Role::System {
                seen += 1;
                if seen == keep_non_system {
                    return idx;
                }
            }
        }
        prefix_len
    }

    /// Check if the context needs compaction.
    pub fn needs_compaction(&self, messages: &[Message]) -> bool {
        let stats = self.stats(messages);
        stats.should_summarize || stats.needs_attention
    }

    /// Compact messages using a sliding window strategy.
    /// Returns the indices of messages to keep.
    ///
    /// Strategy:
    /// 1. Always keep all system messages (at start)
    /// 2. Keep recent messages up to max_retained_messages
    /// 3. Mark older messages for summarization
    pub fn get_compaction_plan(&self, messages: &[Message]) -> CompactionPlan {
        let prefix_len = Self::leading_system_prefix_len(messages);
        let keep_all = || CompactionPlan {
            keep_indices: (0..messages.len()).collect(),
            summarize_range: None,
            preserve_system: true,
        };

        // Keep the recent messages that fit in ~20% of the window (and at
        // most `max_retained_messages`), never a summary: everything older
        // is summarized. Counting tokens, not messages, means one compaction
        // frees real room even when a few tool outputs are huge; with the
        // 45% trigger each compaction frees about a quarter of the window.
        let budget = self.config.context_window_target * 15 / 100;
        let mut kept = 0usize;
        let mut kept_tokens = 0usize;
        let mut recent_start = messages.len();
        for idx in (prefix_len..messages.len()).rev() {
            let m = &messages[idx];
            if is_summary(m) {
                break;
            }
            let tokens = Self::estimate_tokens(m);
            if m.role != Role::System {
                if kept >= self.config.max_retained_messages
                    || (kept > 0 && kept_tokens + tokens > budget)
                {
                    break;
                }
                kept += 1;
            }
            kept_tokens += tokens;
            recent_start = idx;
        }
        let recent_start = Self::adjust_cutoff_for_tool_groups(messages, recent_start, prefix_len);

        // Worth doing only when it removes conversation or merges summaries.
        let older = &messages[prefix_len..recent_start];
        let conversation = older.iter().filter(|m| !is_summary(m)).count();
        let summaries = older.len() - conversation;
        if conversation == 0 && summaries < 2 {
            return keep_all();
        }

        let mut keep_indices: Vec<usize> = (0..prefix_len).collect();
        keep_indices.extend(recent_start..messages.len());
        CompactionPlan {
            keep_indices,
            summarize_range: Some(SummarizeRange {
                start: prefix_len,
                end: recent_start,
            }),
            preserve_system: true,
        }
    }

    /// Get messages that should be summarized.
    /// These are the older messages that will be replaced by a summary.
    pub fn get_messages_to_summarize(&self, messages: &[Message]) -> Vec<Message> {
        let plan = self.get_compaction_plan(messages);

        if let Some(range) = plan.summarize_range {
            let start = range.start.min(messages.len());
            let end = range.end.min(messages.len());
            if start >= end {
                Vec::new()
            } else {
                messages[start..end].to_vec()
            }
        } else {
            Vec::new()
        }
    }

    /// Apply a summary to the context, replacing old messages.
    /// Returns the new message list with the summary inserted.
    pub fn apply_summary(&self, messages: &[Message], summary: &str) -> Vec<Message> {
        let plan = self.get_compaction_plan(messages);
        let prefix_len = Self::leading_system_prefix_len(messages);

        // If there's nothing to summarize, just return the original messages
        let Some(range) = plan.summarize_range else {
            return messages.to_vec();
        };

        // Get recent messages to keep (everything after the summarize range).
        // Adjust to avoid splitting tool_use/tool_result groups.
        let recent_start = Self::adjust_cutoff_for_tool_groups(messages, range.end, prefix_len);

        // Build new message list: system + summary + recent
        let mut result = Vec::with_capacity(prefix_len + 10);

        // Add system messages
        result.extend(messages.iter().take(prefix_len).cloned());

        // Insert summary as a special system message, framed as a hand-off,
        // with the user's own recent words and the files changed so far.
        if !summary.trim().is_empty() {
            let older = &messages[range.start..recent_start];
            let mut text = format!(
                "{SUMMARY_HEADER}\n\nThe earlier part of this conversation was summarized \
                 to save space. Continue the task from here; read files again before \
                 editing them.\n\n{}",
                summary.trim()
            );
            let said = recent_user_messages(older, KEEP_USER_MESSAGES_TOKENS);
            if !said.is_empty() {
                text.push_str("\n\n### The user's recent messages (verbatim)\n");
                for m in &said {
                    text.push_str(&format!("\n> {}\n", m.replace('\n', "\n> ")));
                }
            }
            let files = changed_paths(older);
            if !files.is_empty() {
                text.push_str("\n\n### Files changed so far\n");
                for f in &files {
                    text.push_str(&format!("- {f}\n"));
                }
            }
            result.push(Message {
                role: Role::System,
                content: MessageContent::Text(text),
                tool_call_id: None,
                tool_calls: None,
                reasoning_content: None,
                provider_blocks: None,
            });
        }

        // Add recent messages
        result.extend_from_slice(&messages[recent_start..]);

        result
    }

    /// Get a sliding window of recent messages for context.
    /// Preserves system messages and keeps recent conversation.
    /// Ensures tool_use/tool_result groups are never split.
    pub fn get_sliding_window(
        &self,
        messages: &[Message],
        max_messages: Option<usize>,
    ) -> Vec<Message> {
        let keep_non_system = max_messages.unwrap_or(self.config.max_retained_messages);
        let prefix_len = Self::leading_system_prefix_len(messages);
        let non_system_count = messages[prefix_len..]
            .iter()
            .filter(|m| m.role != Role::System)
            .count();

        if non_system_count <= keep_non_system {
            return messages.to_vec();
        }

        let cutoff = Self::recent_start_by_non_system(messages, prefix_len, keep_non_system);
        let cutoff = Self::adjust_cutoff_for_tool_groups(messages, cutoff, prefix_len);

        let mut result: Vec<Message> = messages[..prefix_len].to_vec();
        // Keep the latest summary of what is dropped.
        if let Some(summary) = messages[prefix_len..cutoff]
            .iter()
            .rev()
            .find(|m| is_summary(m))
        {
            result.push(summary.clone());
        }
        result.extend_from_slice(&messages[cutoff..]);

        result
    }

    /// Truncate the textual content of each message to at most `max_chars`
    /// (middle-elided, keeping head and tail). Last-resort compaction for when a
    /// single retained message — typically a huge tool output — alone exceeds
    /// the budget. Structure (roles, tool ids, tool_calls) is preserved so
    /// tool_use / tool_result pairing stays intact.
    pub fn truncate_message_contents(
        &self,
        messages: &[Message],
        max_chars: usize,
    ) -> Vec<Message> {
        messages
            .iter()
            .map(|m| {
                let mut out = m.clone();
                out.content = match &m.content {
                    MessageContent::Text(t) => MessageContent::Text(truncate_middle(t, max_chars)),
                    MessageContent::Parts(parts) => MessageContent::Parts(
                        parts
                            .iter()
                            .map(|p| match p {
                                ContentPart::Text { text } => ContentPart::Text {
                                    text: truncate_middle(text, max_chars),
                                },
                                other => other.clone(),
                            })
                            .collect(),
                    ),
                };
                out
            })
            .collect()
    }

    /// Truncate very long messages for summary generation.
    pub fn prepare_for_summary(&self, messages: &[Message]) -> Vec<Message> {
        messages
            .iter()
            .map(|m| {
                if m.content.approx_chars() > self.config.max_message_chars_for_summary {
                    let truncated: String = m
                        .content
                        .to_summary_text()
                        .chars()
                        .take(self.config.max_message_chars_for_summary)
                        .collect();
                    Message {
                        role: m.role.clone(),
                        content: MessageContent::Text(format!("{}...[truncated]", truncated)),
                        tool_call_id: m.tool_call_id.clone(),
                        tool_calls: m.tool_calls.clone(),
                        reasoning_content: m.reasoning_content.clone(),
                        provider_blocks: None,
                    }
                } else {
                    m.clone()
                }
            })
            .collect()
    }

    /// Generate a prompt for the AI to summarize conversation.
    pub fn summary_prompt(&self, messages: &[Message]) -> String {
        let prepared = self.prepare_for_summary(messages);
        let stats = self.stats(&prepared);
        let conversation = render_summary_conversation(&prepared);
        let max_words = (self.config.context_window_target / 200).clamp(120, 450);

        format!(
            r#"Please summarize the following conversation for context compaction.

The summary should:
1. Capture the key topics and goals discussed
2. Note any important decisions or findings
3. Preserve critical context (file paths, variable names, errors, etc.)
4. Capture unresolved tasks, pending approvals, and open questions
5. Be written so the assistant can continue without re-asking context

Format requirements:
- Use sections: Goals, Decisions, Files (each path touched and what changed
  or what it contains that matters), Important Artifacts, Open Work
- Keep the summary under {} words
- Prefer concise bullet points

Current context stats:
- Messages: {}
- Estimated tokens: {} (target: {})

---

Conversation to summarize:

{}
"#,
            max_words,
            stats.message_count,
            stats.estimated_tokens,
            self.config.context_window_target,
            conversation
        )
    }

    /// Get the config.
    pub fn config(&self) -> &ContextManagerConfig {
        &self.config
    }

    /// Update the config.
    pub fn set_config(&mut self, config: ContextManagerConfig) {
        self.config = config;
    }
}

/// Plan for compacting messages.
#[derive(Debug, Clone)]
pub struct CompactionPlan {
    /// Indices of messages to keep
    pub keep_indices: Vec<usize>,
    /// Range of messages to summarize (if any)
    pub summarize_range: Option<SummarizeRange>,
    /// Whether to preserve system messages
    pub preserve_system: bool,
}

/// Range of messages to summarize.
#[derive(Debug, Clone)]
pub struct SummarizeRange {
    pub start: usize,
    pub end: usize,
}

fn render_summary_conversation(messages: &[Message]) -> String {
    if messages.is_empty() {
        return "(no messages provided)".to_string();
    }

    messages
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            let role = match message.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
                Role::Tool => "Tool",
            };
            let raw = message.content.to_summary_text();
            let trimmed = raw.trim();
            // Earlier summaries carry the whole history so far.
            let room = if is_summary(message) { 8000 } else { 2000 };
            let content = if trimmed.is_empty() {
                "<empty>".to_string()
            } else {
                trimmed.chars().take(room).collect::<String>()
            };
            format!("[{}] {}:\n{}", idx + 1, role, content)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Truncate a string to at most `max_chars` characters, keeping the head and
/// tail and eliding the middle with a marker. Operates on `char`s so it never
/// splits a UTF-8 boundary.
fn truncate_middle(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        return s.to_string();
    }
    let head = max_chars / 2;
    let tail = max_chars.saturating_sub(head).saturating_sub(1);
    let omitted = chars.len().saturating_sub(head).saturating_sub(tail);
    let head_str: String = chars[..head].iter().collect();
    let tail_str: String = chars[chars.len() - tail..].iter().collect();
    format!("{head_str}\n…[{omitted} chars truncated]…\n{tail_str}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_message(role: Role, content: &str) -> Message {
        Message {
            role,
            content: MessageContent::Text(content.to_string()),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
            provider_blocks: None,
        }
    }

    #[test]
    fn test_estimate_tokens() {
        let msg = make_message(Role::User, "Hello, world!");
        assert!(ContextManager::estimate_tokens(&msg) > 0);
    }

    #[test]
    fn truncate_message_contents_only_shrinks_oversized() {
        let cm = ContextManager::new(ContextManagerConfig::default(), "m".into());
        let small = make_message(Role::User, "short");
        let mut big = make_message(Role::Tool, &"x".repeat(50_000));
        big.tool_call_id = Some("call_1".into());

        let out = cm.truncate_message_contents(&[small.clone(), big], 10_000);

        // Small message is left untouched.
        assert_eq!(out[0].content, small.content);
        // Big message is truncated but keeps its role + tool_call_id.
        assert_eq!(out[1].role, Role::Tool);
        assert_eq!(out[1].tool_call_id.as_deref(), Some("call_1"));
        match &out[1].content {
            MessageContent::Text(t) => {
                assert!(t.contains("chars truncated"));
                assert!(t.chars().count() < 50_000);
            }
            _ => panic!("expected text content"),
        }
    }

    fn compacting_manager() -> ContextManager {
        ContextManager::new(
            ContextManagerConfig {
                context_window_target: 10_000,
                max_retained_messages: 50,
                auto_summarize_threshold: 70,
                enable_auto_summarize: true,
                max_message_chars_for_summary: 10_000,
            },
            "test-model".to_string(),
        )
    }

    /// Repeated compactions leave one summary (the newest folds the older
    /// ones in) and keep only what fits the recent budget.
    #[test]
    fn compactions_fold_earlier_summaries() {
        let manager = compacting_manager();
        let mut messages = vec![make_message(Role::System, "system prompt")];
        for round in 0..5 {
            for i in 0..20 {
                let text = format!("round {round} message {i} {}", "word ".repeat(150));
                messages.push(make_message(Role::User, &text));
                messages.push(make_message(Role::Assistant, &text));
            }
            let older = manager.get_messages_to_summarize(&messages);
            assert!(!older.is_empty(), "round {round} had nothing to summarize");
            messages = manager.apply_summary(&messages, &format!("summary {round}"));
        }
        let summaries: Vec<_> = messages.iter().filter(|m| is_summary(m)).collect();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].content.to_summary_text().contains("summary 4"));
        assert_eq!(messages[0].content.to_summary_text(), "system prompt");
        let kept = ContextManager::estimate_tokens_for_slice(&messages[2..]);
        assert!(kept <= 2_000 + 400, "recent part stays near 20%: {kept}");
    }

    #[test]
    fn stacked_summaries_merge_into_one() {
        let mut messages = vec![make_message(Role::System, "system prompt")];
        for i in 0..292 {
            messages.push(make_message(
                Role::System,
                &format!("{SUMMARY_HEADER}\n\nsummary {i} {}", "x".repeat(200)),
            ));
        }
        messages.push(make_message(Role::User, "latest"));
        let merged = merge_stacked_summaries(&messages).expect("merged");
        assert_eq!(merged.len(), 3);
        assert!(is_summary(&merged[1]));
        let text = merged[1].content.to_summary_text();
        assert!(text.contains("summary 291"), "newest part kept");
        assert!(text.chars().count() <= MERGED_SUMMARY_MAX_CHARS + 200);
        assert!(merge_stacked_summaries(&merged).is_none());
    }

    #[test]
    fn budget_compacts_late_and_prunes_before() {
        // Sonnet-like: 200k window, 64k max output (32k reserved).
        let b = ContextBudget::new(200_000, 64_000, None);
        assert_eq!(b.output_reserve, 32_000);
        assert_eq!(b.compact_at(), 200_000 - 32_000 - 15_000);
        assert_eq!(b.prune_at(), b.compact_at() - 20_000);
        assert!(b.target_after() < b.prune_at());
        // A 1M window keeps the cap at 90%.
        let big = ContextBudget::new(1_000_000, 64_000, None);
        assert!(big.compact_at() <= 900_000 && big.compact_at() >= 600_000);
        // A small window never compacts below 60%.
        let small = ContextBudget::new(32_000, 16_000, None);
        assert!(small.compact_at() >= 19_200);
        // An explicit percentage wins.
        assert_eq!(
            ContextBudget::new(200_000, 64_000, Some(70)).compact_at(),
            140_000
        );
    }

    fn tool_round(i: usize, words: usize) -> [Message; 2] {
        let mut call = make_message(Role::Assistant, "");
        call.tool_calls = Some(vec![dcode_ai_common::message::MessageToolCall {
            id: format!("c{i}"),
            name: "browser".into(),
            arguments: serde_json::json!({"action": "snapshot"}),
        }]);
        let mut out = make_message(Role::Tool, &format!("page {i} {}", "word ".repeat(words)));
        out.tool_call_id = Some(format!("c{i}"));
        [call, out]
    }

    #[test]
    fn prune_clears_old_tool_output_and_keeps_the_newest() {
        let mut messages = vec![
            make_message(Role::System, "sys"),
            make_message(Role::User, "go"),
        ];
        for i in 0..40 {
            messages.extend(tool_round(i, 3_000));
        }
        let protect = messages.len() - 6;
        let (pruned, freed) =
            prune_tool_outputs(&messages, protect, PRUNE_KEEP_TOKENS, PRUNE_MIN_FREE)
                .expect("pruned");
        assert!(freed >= PRUNE_MIN_FREE);
        let stubs = pruned
            .iter()
            .filter(|m| m.content.to_summary_text().starts_with(PRUNED_MARK))
            .count();
        assert!(stubs > 0 && stubs < 40);
        // The newest outputs stay whole; the stub names the tool.
        assert!(
            pruned
                .last()
                .unwrap()
                .content
                .to_summary_text()
                .starts_with("page 39")
        );
        let stub = pruned
            .iter()
            .find(|m| m.content.to_summary_text().starts_with(PRUNED_MARK))
            .unwrap();
        assert!(stub.content.to_summary_text().contains("browser output"));
        // Structure is kept (same count, tool ids intact).
        assert_eq!(pruned.len(), messages.len());
        // Nothing worth clearing: no prune.
        assert!(prune_tool_outputs(&messages[..8], 2, PRUNE_KEEP_TOKENS, PRUNE_MIN_FREE).is_none());
    }

    #[test]
    fn summary_keeps_the_users_words_and_changed_files() {
        let manager = compacting_manager();
        let mut messages = vec![make_message(Role::System, "system prompt")];
        messages.push(make_message(
            Role::User,
            "please fix the login bug in auth.rs",
        ));
        let mut edit = make_message(Role::Assistant, "");
        edit.tool_calls = Some(vec![dcode_ai_common::message::MessageToolCall {
            id: "e1".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({"path": "src/auth.rs"}),
        }]);
        messages.push(edit);
        let mut res = make_message(Role::Tool, "edited");
        res.tool_call_id = Some("e1".into());
        messages.push(res);
        for i in 0..30 {
            messages.push(make_message(
                Role::User,
                &format!("step {i} {}", "word ".repeat(150)),
            ));
            messages.push(make_message(
                Role::Assistant,
                &format!("ok {i} {}", "word ".repeat(150)),
            ));
        }
        let out = manager.apply_summary(&messages, "SUMMARY");
        let summary = out
            .iter()
            .find(|m| is_summary(m))
            .unwrap()
            .content
            .to_summary_text();
        assert!(summary.contains("Continue the task from here"));
        assert!(summary.contains("> please fix the login bug in auth.rs"));
        assert!(summary.contains("- src/auth.rs"));
    }

    #[test]
    fn test_stats() {
        let config = ContextManagerConfig {
            context_window_target: 1000,
            max_retained_messages: 10,
            auto_summarize_threshold: 50,
            enable_auto_summarize: true,
            max_message_chars_for_summary: 1000,
        };
        let manager = ContextManager::new(config, "test-model".to_string());

        let messages = vec![
            make_message(Role::System, "You are a helpful assistant."),
            make_message(Role::User, "Hello"),
            make_message(Role::Assistant, "Hi there!"),
        ];

        let stats = manager.stats(&messages);
        assert!(stats.message_count == 3);
        assert!(stats.estimated_tokens > 0);
        assert_eq!(stats.model, "test-model");
        assert_eq!(stats.context_window, 1000); // stats use configured target
    }

    #[test]
    fn test_sliding_window() {
        let config = ContextManagerConfig {
            context_window_target: 32000,
            max_retained_messages: 3,
            auto_summarize_threshold: 75,
            enable_auto_summarize: true,
            max_message_chars_for_summary: 10000,
        };
        let manager = ContextManager::new(config, "test-model".to_string());

        let messages: Vec<Message> = (0..10)
            .map(|i| make_message(Role::User, &format!("Message {}", i)))
            .collect();

        let window = manager.get_sliding_window(&messages, None);
        // System messages + last 3
        assert!(window.len() <= 3);
    }

    #[test]
    fn test_apply_summary() {
        // Use a config that will trigger compaction for 5 messages
        let config = ContextManagerConfig {
            context_window_target: 32_000,
            max_retained_messages: 2, // Only keep 2 messages, forcing compaction
            auto_summarize_threshold: 75,
            enable_auto_summarize: true,
            max_message_chars_for_summary: 10_000,
        };
        let manager = ContextManager::new(config, "test-model".to_string());

        let messages = vec![
            make_message(Role::System, "System"),
            make_message(Role::User, "Hello"),
            make_message(Role::Assistant, "Hi"),
            make_message(Role::User, "How are you?"),
            make_message(Role::Assistant, "I'm fine!"),
        ];

        let summary = "User greeted the assistant and asked how it was doing.";
        let result = manager.apply_summary(&messages, summary);

        // Should have system + summary + recent messages (2)
        // Note: The compaction plan keeps last 2 non-system messages
        assert!(
            result.len() >= 2,
            "Expected at least 2 messages, got {}",
            result.len()
        );
        // The summary should be in a system message
        assert!(
            result
                .iter()
                .any(|m| m.content.to_summary_text().contains("Conversation Summary"))
        );
    }

    #[test]
    fn compaction_plan_keeps_latest_non_system_messages() {
        let config = ContextManagerConfig {
            context_window_target: 32_000,
            max_retained_messages: 3,
            auto_summarize_threshold: 75,
            enable_auto_summarize: true,
            max_message_chars_for_summary: 10_000,
        };
        let manager = ContextManager::new(config, "test-model".to_string());
        let messages = vec![
            make_message(Role::System, "system"),
            make_message(Role::User, "u1"),
            make_message(Role::Assistant, "a1"),
            make_message(Role::User, "u2"),
            make_message(Role::Assistant, "a2"),
            make_message(Role::User, "u3"),
            make_message(Role::Assistant, "a3"),
        ];

        let plan = manager.get_compaction_plan(&messages);
        let range = plan.summarize_range.expect("expected summarize range");
        assert_eq!(range.start, 1);
        assert_eq!(range.end, 4);
        assert!(plan.keep_indices.contains(&0));
        assert!(plan.keep_indices.contains(&4));
        assert!(plan.keep_indices.contains(&6));
    }

    #[test]
    fn compaction_plan_noop_when_within_limit() {
        let config = ContextManagerConfig {
            context_window_target: 32_000,
            max_retained_messages: 5,
            auto_summarize_threshold: 75,
            enable_auto_summarize: true,
            max_message_chars_for_summary: 10_000,
        };
        let manager = ContextManager::new(config, "test-model".to_string());
        let messages = vec![
            make_message(Role::System, "system"),
            make_message(Role::User, "u1"),
            make_message(Role::Assistant, "a1"),
            make_message(Role::User, "u2"),
            make_message(Role::Assistant, "a2"),
        ];

        let plan = manager.get_compaction_plan(&messages);
        assert!(plan.summarize_range.is_none());
        assert_eq!(plan.keep_indices.len(), messages.len());
    }

    use dcode_ai_common::message::MessageToolCall;

    fn make_tool_call(id: &str, name: &str) -> MessageToolCall {
        MessageToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: serde_json::json!({}),
        }
    }

    /// Helper: assert no Role::Tool message appears without its matching
    /// assistant tool_use in the preceding assistant message.
    fn assert_no_orphaned_tool_results(messages: &[Message]) {
        let mut expected_tool_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for msg in messages {
            match msg.role {
                Role::Assistant => {
                    expected_tool_ids.clear();
                    if let Some(calls) = &msg.tool_calls {
                        for call in calls {
                            expected_tool_ids.insert(call.id.clone());
                        }
                    }
                }
                Role::Tool => {
                    let id = msg.tool_call_id.as_deref().unwrap_or("");
                    assert!(
                        expected_tool_ids.contains(id),
                        "Orphaned tool_result with id '{}' — no matching tool_use in preceding assistant message. Messages: {:?}",
                        id,
                        messages
                            .iter()
                            .map(|m| (&m.role, m.tool_call_id.as_deref()))
                            .collect::<Vec<_>>()
                    );
                }
                _ => {
                    expected_tool_ids.clear();
                }
            }
        }
    }

    #[test]
    fn sliding_window_preserves_tool_use_result_pairs() {
        // max_retained=4: cutoff would land on tool result msg if naive
        let config = ContextManagerConfig {
            context_window_target: 32_000,
            max_retained_messages: 4,
            auto_summarize_threshold: 75,
            enable_auto_summarize: true,
            max_message_chars_for_summary: 10_000,
        };
        let manager = ContextManager::new(config, "test-model".to_string());

        // 10 messages: user, assistant+tool_calls, tool_result, user, ...
        let messages = vec![
            make_message(Role::System, "system prompt"),
            make_message(Role::User, "msg 1"),
            make_message(Role::Assistant, "reply 1"),
            make_message(Role::User, "msg 2"),
            // This is the critical group: assistant calls tool, then tool result
            Message::assistant_with_tool_calls(
                "Let me check",
                vec![make_tool_call("call-1", "read_file")],
            ),
            Message::tool("call-1", "file contents here"),
            // After tool result
            make_message(Role::User, "msg 3"),
            make_message(Role::Assistant, "final reply"),
        ];

        let window = manager.get_sliding_window(&messages, None);

        // The window must never contain an orphaned tool_result
        assert_no_orphaned_tool_results(&window);
    }

    #[test]
    fn apply_summary_preserves_tool_use_result_pairs() {
        // max_retained=3: cutoff lands right on the tool_result
        let config = ContextManagerConfig {
            context_window_target: 32_000,
            max_retained_messages: 3,
            auto_summarize_threshold: 75,
            enable_auto_summarize: true,
            max_message_chars_for_summary: 10_000,
        };
        let manager = ContextManager::new(config, "test-model".to_string());

        let messages = vec![
            make_message(Role::System, "system prompt"),
            make_message(Role::User, "msg 1"),
            make_message(Role::Assistant, "reply 1"),
            make_message(Role::User, "msg 2"),
            // Tool use group that straddles the naive cutoff
            Message::assistant_with_tool_calls(
                "Running tool",
                vec![make_tool_call("call-2", "bash")],
            ),
            Message::tool("call-2", "ok"),
            make_message(Role::User, "msg 3"),
            make_message(Role::Assistant, "done"),
        ];

        let result = manager.apply_summary(&messages, "Earlier conversation summary.");

        assert_no_orphaned_tool_results(&result);
    }

    #[test]
    fn sliding_window_preserves_multi_tool_call_group() {
        // Assistant calls 2 tools → 2 tool results. Window must keep entire group.
        let config = ContextManagerConfig {
            context_window_target: 32_000,
            max_retained_messages: 4,
            auto_summarize_threshold: 75,
            enable_auto_summarize: true,
            max_message_chars_for_summary: 10_000,
        };
        let manager = ContextManager::new(config, "test-model".to_string());

        let messages = vec![
            make_message(Role::System, "system prompt"),
            make_message(Role::User, "msg 1"),
            make_message(Role::Assistant, "reply 1"),
            make_message(Role::User, "msg 2"),
            Message::assistant_with_tool_calls(
                "Using two tools",
                vec![
                    make_tool_call("call-a", "read_file"),
                    make_tool_call("call-b", "grep"),
                ],
            ),
            Message::tool("call-a", "contents a"),
            Message::tool("call-b", "contents b"),
            make_message(Role::User, "msg 3"),
            make_message(Role::Assistant, "all done"),
        ];

        let window = manager.get_sliding_window(&messages, None);

        assert_no_orphaned_tool_results(&window);
    }

    #[test]
    fn summary_prompt_includes_conversation_content() {
        let manager =
            ContextManager::new(ContextManagerConfig::default(), "test-model".to_string());
        let messages = vec![
            make_message(Role::User, "Need help with context errors"),
            make_message(Role::Assistant, "Let's compact earlier history first."),
        ];

        let prompt = manager.summary_prompt(&messages);
        assert!(prompt.contains("Need help with context errors"));
        assert!(prompt.contains("Let's compact earlier history first."));
        assert!(prompt.contains("[1] User:"));
        assert!(prompt.contains("[2] Assistant:"));
        assert!(prompt.contains("Goals, Decisions, Files (each path touched"));
    }
}
