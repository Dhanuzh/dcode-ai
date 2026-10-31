//! Sub-agent spawning and cross-session summarization: child session lifecycle,
//! parent context inheritance, and compaction fallback summaries.

use crate::context_manager::ContextManager;
use crate::session_store::SessionStore;
use dcode_ai_common::config::DcodeAiConfig;
use dcode_ai_common::event::{AgentEvent, EndReason};
use dcode_ai_core::approval::ApprovalHandler;
use dcode_ai_core::hooks::{HookEventKind, HookRunner};
use dcode_ai_core::tools::spawn_subagent::SpawnRequest;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

use super::{AutoDenyHandler, Supervisor, SupervisorConfig, spawn_event_fanout};

pub fn truncate_child_detail(s: &str, max_chars: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max_chars {
        t.to_string()
    } else {
        format!(
            "{}…",
            t.chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    }
}

pub fn summarize_child_error_message(s: &str, max_chars: usize) -> String {
    let one_line = s
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown child error");
    truncate_child_detail(one_line, max_chars)
}

pub fn tool_input_one_line(input: &serde_json::Value) -> String {
    if let Some(s) = input.as_str() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
            return tool_input_one_line(&v);
        }
        return truncate_child_detail(s, 120);
    }
    if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
        return truncate_child_detail(cmd, 120);
    }
    if let Some(p) = input
        .get("path")
        .or_else(|| input.get("file_path"))
        .and_then(|v| v.as_str())
    {
        return truncate_child_detail(p, 120);
    }
    let s = serde_json::to_string(input).unwrap_or_default();
    truncate_child_detail(&s, 120)
}

/// Maps a child session event to a parent-visible activity line (sidebar + transcript).
pub fn map_child_event_for_parent_broadcast(
    child_session_id: &str,
    event: &AgentEvent,
) -> Option<AgentEvent> {
    match event {
        AgentEvent::ToolCallStarted { tool, input, .. } => Some(AgentEvent::ChildSessionActivity {
            child_session_id: child_session_id.to_string(),
            phase: tool.clone(),
            detail: tool_input_one_line(input),
        }),
        AgentEvent::Checkpoint { phase, detail, .. } => Some(AgentEvent::ChildSessionActivity {
            child_session_id: child_session_id.to_string(),
            phase: phase.clone(),
            detail: truncate_child_detail(detail, 120),
        }),
        AgentEvent::ChildSessionSpawned { task, .. } => Some(AgentEvent::ChildSessionActivity {
            child_session_id: child_session_id.to_string(),
            phase: "nested_subagent".to_string(),
            detail: truncate_child_detail(task, 120),
        }),
        AgentEvent::Error { message } => Some(AgentEvent::ChildSessionActivity {
            child_session_id: child_session_id.to_string(),
            phase: "error".to_string(),
            detail: truncate_child_detail(message, 160),
        }),
        _ => None,
    }
}

/// Spawns a background task that consumes spawn requests from the sub-agent tool
/// and runs child sessions. Each child session inherits parent context.
pub fn spawn_subagent_consumer(
    mut spawn_rx: mpsc::Receiver<SpawnRequest>,
    parent_session_id: String,
    workspace_root: PathBuf,
    config: DcodeAiConfig,
    parent_messages: Vec<dcode_ai_common::message::Message>,
    event_tx: Option<tokio::sync::mpsc::Sender<AgentEvent>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let parent_sessions_dir = workspace_root.join(&config.session.history_dir);
        let parent_summary = build_parent_summary(&parent_messages);

        while let Some(req) = spawn_rx.recv().await {
            let parent_session_id = parent_session_id.clone();
            let workspace_root = workspace_root.clone();
            let config = config.clone();
            let event_tx = event_tx.clone();
            let parent_store = SessionStore::new(parent_sessions_dir.clone());
            let parent_summary = parent_summary.clone();

            let agent = match req.agent.as_deref() {
                None => None,
                Some(name) => {
                    let defs = dcode_ai_core::agent_defs::discover(
                        &workspace_root,
                        &dcode_ai_common::plugins::enabled_plugins(),
                    );
                    match dcode_ai_core::agent_defs::find(&defs, name) {
                        Some(def) => Some(def.clone()),
                        None => {
                            let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
                            let _ = req.reply.send(
                                dcode_ai_core::tools::spawn_subagent::SpawnResponse {
                                    child_session_id: String::new(),
                                    status: "error".into(),
                                    output: format!(
                                        "unknown agent `{name}`; available: {}",
                                        if names.is_empty() {
                                            "none".to_string()
                                        } else {
                                            names.join(", ")
                                        }
                                    ),
                                    workspace: workspace_root.display().to_string(),
                                    branch: None,
                                    worktree_path: None,
                                },
                            );
                            continue;
                        }
                    }
                }
            };
            let child_cfg = ChildSessionConfig {
                parent_session_id: parent_session_id.clone(),
                task: req.task.clone(),
                agent,
                workspace_root: workspace_root.clone(),
                config,
                parent_summary,
                use_worktree: req.use_worktree,
                focus_files: req.focus_files,
                explore: req.explore,
                parent_mode: req.parent_mode,
                child_id: None,
                approval: req.approval.clone(),
            };
            let mut child_cfg = child_cfg;
            // Background: answer the tool at once with the agent's id; the
            // result is delivered as a notification when it settles.
            let mut reply = Some(req.reply);
            let background = if req.background {
                let id = super::session::generate_session_id();
                child_cfg.child_id = Some(id.clone());
                if let Ok(mut agents) = req.agents.lock() {
                    agents.insert(
                        id.clone(),
                        dcode_ai_core::tools::spawn_subagent::AgentRecord {
                            task: req.task.clone(),
                            status: "running".into(),
                            output: String::new(),
                            started: std::time::Instant::now(),
                            finished: None,
                            abort: None,
                        },
                    );
                }
                if let Some(r) = reply.take() {
                    let _ = r.send(dcode_ai_core::tools::spawn_subagent::SpawnResponse {
                        child_session_id: id.clone(),
                        status: "running".into(),
                        output: req.task.clone(),
                        workspace: workspace_root.display().to_string(),
                        branch: None,
                        worktree_path: None,
                    });
                }
                Some((req.agents.clone(), id, req.task.clone()))
            } else {
                None
            };
            let registry_for_abort = background
                .as_ref()
                .map(|(a, id, _)| (a.clone(), id.clone()));

            let handle = tokio::spawn(async move {
                let hook_runner = {
                    let runner = HookRunner::new(child_cfg.config.hooks.clone());
                    runner.has_any().then_some(runner)
                };
                if let Some(hooks) = &hook_runner {
                    hooks
                        .run_best_effort(
                            HookEventKind::SubagentStart,
                            None,
                            &json!({
                                "parent_session_id": parent_session_id.clone(),
                                "task": child_cfg.task.clone(),
                                "workspace": child_cfg.workspace_root.clone(),
                            }),
                        )
                        .await;
                }
                let result = spawn_child_session(child_cfg, event_tx.clone()).await;
                match result {
                    Ok(res) => {
                        let completion_status = if res.status == "error" {
                            format!("error: {}", summarize_child_error_message(&res.output, 140))
                        } else {
                            res.status.clone()
                        };
                        append_child_to_parent(
                            &parent_store,
                            &parent_session_id,
                            &res.child_session_id,
                        )
                        .await;

                        if let Some(ref tx) = event_tx {
                            let _ = tx
                                .send(AgentEvent::ChildSessionCompleted {
                                    parent_session_id: parent_session_id.clone(),
                                    child_session_id: res.child_session_id.clone(),
                                    status: completion_status,
                                })
                                .await;
                        }
                        if let Some(hooks) = &hook_runner {
                            hooks
                                .run_best_effort(
                                    HookEventKind::SubagentStop,
                                    None,
                                    &json!({
                                        "parent_session_id": parent_session_id.clone(),
                                        "child_session_id": res.child_session_id.clone(),
                                        "status": res.status.clone(),
                                    }),
                                )
                                .await;
                        }
                        let response = dcode_ai_core::tools::spawn_subagent::SpawnResponse {
                            child_session_id: res.child_session_id,
                            status: res.status,
                            output: res.output,
                            workspace: res.workspace,
                            branch: res.branch,
                            worktree_path: res.worktree_path,
                        };
                        deliver(response, reply, background, &event_tx).await;
                    }
                    Err(e) => {
                        if let Some(hooks) = &hook_runner {
                            hooks
                                .run_best_effort(
                                    HookEventKind::SubagentStop,
                                    None,
                                    &json!({
                                        "parent_session_id": parent_session_id.clone(),
                                        "status": "error",
                                        "error": e.clone(),
                                    }),
                                )
                                .await;
                        }
                        if let Some(ref tx) = event_tx {
                            let _ = tx
                                .send(AgentEvent::Error {
                                    message: format!("Failed to spawn child session: {e}"),
                                })
                                .await;
                        }
                        let response = dcode_ai_core::tools::spawn_subagent::SpawnResponse {
                            child_session_id: String::new(),
                            status: "error".into(),
                            output: e,
                            workspace: workspace_root.display().to_string(),
                            branch: None,
                            worktree_path: None,
                        };
                        deliver(response, reply, background, &event_tx).await;
                    }
                }
            });
            if let Some((agents, id)) = registry_for_abort
                && let Ok(mut agents) = agents.lock()
                && let Some(r) = agents.get_mut(&id)
            {
                r.abort = Some(handle.abort_handle());
            }
        }
    })
}

/// Hand a finished child's result to whoever waits for it: the blocked tool
/// call, or — for a background agent — the registry and a notification.
async fn deliver(
    response: dcode_ai_core::tools::spawn_subagent::SpawnResponse,
    reply: Option<
        tokio::sync::oneshot::Sender<dcode_ai_core::tools::spawn_subagent::SpawnResponse>,
    >,
    background: Option<(
        dcode_ai_core::tools::spawn_subagent::BackgroundAgents,
        String,
        String,
    )>,
    event_tx: &Option<tokio::sync::mpsc::Sender<AgentEvent>>,
) {
    let Some((agents, id, task)) = background else {
        if let Some(r) = reply {
            let _ = r.send(response);
        }
        return;
    };
    let status = if response.status == "completed" {
        "completed".to_string()
    } else {
        "error".to_string()
    };
    if let Ok(mut map) = agents.lock()
        && let Some(r) = map.get_mut(&id)
    {
        // A kill already settled it.
        if r.status != "running" {
            return;
        }
        r.status = status.clone();
        r.output = response.output.clone();
        r.finished = Some(std::time::Instant::now());
        r.abort = None;
    }
    if let Some(tx) = event_tx {
        let _ = tx
            .send(AgentEvent::BackgroundAgentFinished {
                agent_id: id,
                task,
                status,
                output: response.output,
            })
            .await;
    }
}

/// Point the child at an agent's `model:`. Claude aliases (`sonnet`,
/// `opus`, …) only mean something for the Claude provider; elsewhere a full
/// model id is used as given and an alias falls back to the parent's model.
fn apply_agent_model(config: &mut DcodeAiConfig, model: &str) {
    use dcode_ai_common::config::ProviderKind;
    let is_alias = matches!(
        model.to_ascii_lowercase().as_str(),
        "sonnet" | "opus" | "haiku" | "fable"
    );
    match config.provider.default {
        ProviderKind::Anthropic => config.apply_model_override(model),
        _ if !is_alias => config.apply_model_override(model),
        _ => {}
    }
}

/// Append a child session ID to the parent session's metadata on disk.
async fn append_child_to_parent(store: &SessionStore, parent_id: &str, child_id: &str) {
    if let Ok(mut parent) = store.load(parent_id).await
        && !parent
            .meta
            .child_session_ids
            .contains(&child_id.to_string())
    {
        parent.meta.child_session_ids.push(child_id.to_string());
        let _ = store.save(&parent).await;
    }
}

/// Build a concise summary of the parent conversation for context inheritance.
pub fn build_parent_summary(messages: &[dcode_ai_common::message::Message]) -> String {
    use dcode_ai_common::message::Role;

    let mut summary = String::new();
    // The child builds its own system prompt; repeating the parent's would
    // only burn tokens. Tool-call-only assistant turns carry no text.
    let recent: Vec<_> = messages
        .iter()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant))
        .filter(|m| !m.content.event_preview().trim().is_empty())
        .collect();

    let window = if recent.len() > 10 {
        &recent[recent.len() - 10..]
    } else {
        &recent
    };

    for msg in window {
        let role = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
            Role::Tool => continue,
        };
        let body = msg.content.event_preview();
        let content = if body.len() > 500 {
            let truncated: String = body.chars().take(500).collect();
            format!("{truncated}...")
        } else {
            body
        };
        summary.push_str(&format!("[{role}]: {content}\n\n"));
    }

    summary
}

pub fn build_compaction_fallback_summary(messages: &[dcode_ai_common::message::Message]) -> String {
    // Semantic compaction: a plain conversation digest loses the concrete state
    // an agent needs to continue (which files it touched, what it ran, what
    // broke). Pull those artifacts out of *all* messages — including tool
    // results, which the conversation digest skips — and pin them above the digest.
    let artifacts = extract_session_artifacts(messages);
    let convo = build_parent_summary(messages);

    let mut out = String::new();
    if !artifacts.is_empty() {
        out.push_str("## Preserved Artifacts\n");
        out.push_str(&artifacts);
        out.push('\n');
    }
    if convo.trim().is_empty() {
        if out.is_empty() {
            out.push_str("Earlier conversation context was compacted due to token limits.");
        }
    } else {
        out.push_str("## Recent Conversation\n");
        out.push_str(&convo);
    }
    out
}

pub fn build_compaction_preview_summary(
    context_manager: &ContextManager,
    messages: &[dcode_ai_common::message::Message],
) -> String {
    if !messages.iter().any(|message| {
        matches!(
            message.role,
            dcode_ai_common::message::Role::User
                | dcode_ai_common::message::Role::Assistant
                | dcode_ai_common::message::Role::Tool
        )
    }) {
        return build_compaction_fallback_summary(&[]);
    }

    let messages_to_summarize = context_manager.get_messages_to_summarize(messages);
    if messages_to_summarize.is_empty() {
        build_compaction_fallback_summary(messages)
    } else {
        build_compaction_fallback_summary(&messages_to_summarize)
    }
}

/// Scan every message (tool results included) for the durable state worth
/// preserving across compaction: files touched, shell commands run, and error
/// lines. Dependency-free heuristics; capped so the summary stays small.
pub fn extract_session_artifacts(messages: &[dcode_ai_common::message::Message]) -> String {
    use std::collections::BTreeSet;

    let mut files: Vec<String> = Vec::new();
    let mut seen_files: BTreeSet<String> = BTreeSet::new();
    let mut commands: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    let mut push_file = |path: &str| {
        let p = path
            .trim()
            .trim_matches(|c| matches!(c, '"' | '\'' | '`' | ',' | ';' | ')' | '('));
        if looks_like_path(p) && seen_files.insert(p.to_string()) && files.len() < 20 {
            files.push(p.to_string());
        }
    };

    for msg in messages {
        // Shell commands and edited paths from tool calls.
        if let Some(calls) = &msg.tool_calls {
            for call in calls {
                if matches!(call.name.as_str(), "execute_bash" | "bash")
                    && let Some(cmd) = call.arguments.get("command").and_then(|v| v.as_str())
                    && commands.len() < 10
                {
                    commands.push(cmd.lines().next().unwrap_or(cmd).trim().to_string());
                }
                if let Some(path) = call.arguments.get("path").and_then(|v| v.as_str()) {
                    push_file(path);
                }
            }
        }

        let text = msg.content.to_summary_text();
        for token in text.split(|c: char| c.is_whitespace()) {
            if token.contains('/') {
                push_file(token);
            }
        }
        for line in text.lines() {
            let lower = line.to_ascii_lowercase();
            if (lower.contains("error") || lower.contains("failed") || lower.contains("panic"))
                && errors.len() < 8
            {
                let trimmed = line.trim();
                let clipped: String = trimmed.chars().take(160).collect();
                if !clipped.is_empty() {
                    errors.push(clipped);
                }
            }
        }
    }

    let mut out = String::new();
    if !files.is_empty() {
        out.push_str("Files: ");
        out.push_str(&files.join(", "));
        out.push('\n');
    }
    if !commands.is_empty() {
        out.push_str("Commands run:\n");
        for c in &commands {
            out.push_str(&format!("- {c}\n"));
        }
    }
    if !errors.is_empty() {
        out.push_str("Errors/failures seen:\n");
        for e in &errors {
            out.push_str(&format!("- {e}\n"));
        }
    }
    out
}

/// Heuristic: does `token` look like a workspace file path worth keeping?
pub fn looks_like_path(token: &str) -> bool {
    if token.len() < 3 || token.len() > 120 {
        return false;
    }
    if token.starts_with("http://") || token.starts_with("https://") {
        return false;
    }
    // Either a path with a separator, or a bare filename with a code-ish extension.
    let has_sep = token.contains('/');
    let has_ext = token.rsplit('.').next().is_some_and(|ext| {
        (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
    });
    has_sep && has_ext
}

/// Configuration for spawning a child session.
pub struct ChildSessionConfig {
    pub parent_session_id: String,
    pub task: String,
    /// Custom agent definition the child runs as.
    pub agent: Option<dcode_ai_core::agent_defs::AgentDef>,
    pub workspace_root: PathBuf,
    pub config: DcodeAiConfig,
    pub parent_summary: String,
    pub use_worktree: bool,
    pub focus_files: Vec<String>,
    /// Read-only research on the cheap utility model (plan mode).
    pub explore: bool,
    /// The parent's permission mode when it spawned the child (falls back to
    /// the configured mode).
    pub parent_mode: Option<dcode_ai_common::config::PermissionMode>,
    /// The child's session id, when the caller needs it up front.
    pub child_id: Option<String>,
    /// The parent's approval handler; the child's prompts reach the user
    /// through it. `None` denies them.
    pub approval: Option<Arc<dyn ApprovalHandler>>,
}

/// A child's approval prompts, shown in the parent session: announced on the
/// parent's event stream and answered through the parent's handler.
struct BridgedApproval {
    parent: Arc<dyn ApprovalHandler>,
    events: Option<tokio::sync::mpsc::Sender<AgentEvent>>,
    task: String,
}

#[async_trait::async_trait]
impl ApprovalHandler for BridgedApproval {
    async fn resolve(
        &self,
        call: &dcode_ai_common::tool::ToolCall,
        description: &str,
    ) -> dcode_ai_core::approval::ApprovalVerdict {
        if let Some(tx) = &self.events {
            let _ = tx
                .send(AgentEvent::ApprovalRequested {
                    call_id: call.id.clone(),
                    tool: call.name.clone(),
                    description: format!(
                        "Sub-agent ({}) wants to run {}: {}",
                        truncate_child_detail(&self.task, 60),
                        call.name,
                        truncate_child_detail(&tool_input_one_line(&call.input), 160)
                    ),
                })
                .await;
        }
        let verdict = self.parent.resolve(call, description).await;
        // Clear the prompt row in the parent's transcript.
        if let Some(tx) = &self.events {
            let _ = tx
                .send(AgentEvent::ApprovalResolved {
                    call_id: call.id.clone(),
                    approved: !matches!(verdict, dcode_ai_core::approval::ApprovalVerdict::Denied),
                })
                .await;
        }
        verdict
    }
}

/// The mode a sub-agent runs in, given its parent's. A child can't ask the
/// user, so the interactive modes become auto mode (edits and ordinary
/// commands run, risky ones are denied); plan, don't-ask and bypass carry
/// over unchanged.
pub fn child_permission_mode(
    parent: dcode_ai_common::config::PermissionMode,
) -> dcode_ai_common::config::PermissionMode {
    use dcode_ai_common::config::PermissionMode as M;
    match parent {
        M::Plan => M::Plan,
        M::DontAsk => M::DontAsk,
        M::BypassPermissions => M::BypassPermissions,
        M::Default | M::AcceptEdits | M::Auto => M::Auto,
    }
}

/// The child's permission mode and model. Explorers only read and take the
/// cheap utility model (Haiku on Claude); an agent definition's own model
/// wins, then `[model] subagent_model`.
fn child_mode_and_model(
    cfg: &ChildSessionConfig,
) -> (dcode_ai_common::config::PermissionMode, Option<String>) {
    let mode = if cfg.explore {
        dcode_ai_common::config::PermissionMode::Plan
    } else {
        child_permission_mode(cfg.parent_mode.unwrap_or(cfg.config.permissions.mode))
    };
    let model = cfg
        .agent
        .as_ref()
        .and_then(|a| a.model.clone())
        .or_else(|| {
            if cfg.explore {
                super::utility_model(&cfg.config)
            } else {
                None
            }
        })
        .or_else(|| cfg.config.model.subagent_model.clone());
    (mode, model)
}

/// Result of a spawned child session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChildSessionResult {
    pub child_session_id: String,
    pub status: String,
    pub output: String,
    pub workspace: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
}

/// Spawn a child session that inherits parent context and runs to completion.
/// Returns the result of the child run. This is a blocking async call.
pub async fn spawn_child_session(
    cfg: ChildSessionConfig,
    event_tx: Option<tokio::sync::mpsc::Sender<AgentEvent>>,
) -> Result<ChildSessionResult, String> {
    let mut child_config = cfg.config.clone();
    let (mode, routed) = child_mode_and_model(&cfg);
    child_config.permissions.mode = mode;
    if let Some(model) = routed {
        apply_agent_model(&mut child_config, &model);
    }
    // The child's session record stays with the parent's, even when it works
    // in a worktree.
    child_config.session.history_dir = cfg.workspace_root.join(&cfg.config.session.history_dir);

    // Create the worktree before the session so every tool (files, shell,
    // sandbox) is rooted inside it, not in the parent's checkout.
    let child_id = cfg
        .child_id
        .clone()
        .unwrap_or_else(super::session::generate_session_id);
    let mut worktree = None;
    if cfg.use_worktree {
        let wt_mgr = crate::worktree::WorktreeManager::new(&cfg.workspace_root);
        if wt_mgr.is_git_repo() {
            match wt_mgr.create_worktree(&child_id) {
                Ok(info) => worktree = Some(info),
                Err(e) => {
                    tracing::warn!("Failed to create worktree for child session: {e}");
                }
            }
        }
    }
    let root = worktree
        .as_ref()
        .map(|w| w.worktree_path.clone())
        .unwrap_or_else(|| cfg.workspace_root.clone());

    let mut sup = Supervisor::create(SupervisorConfig {
        config: child_config,
        workspace_root: root,
        safe_mode: false,
        interactive_approvals: false,
        session_id: Some(child_id.clone()),
        approval_handler: Some(match cfg.approval.clone() {
            Some(parent) => Arc::new(BridgedApproval {
                parent,
                events: event_tx.clone(),
                task: cfg.task.clone(),
            }) as Arc<dyn ApprovalHandler>,
            None => Arc::new(AutoDenyHandler) as Arc<dyn ApprovalHandler>,
        }),
        orchestration_context: None,
    })
    .await
    .map_err(|e| e.to_string())?;
    // Nobody can answer a prompt here: deny that one call and let the child
    // carry on (and report it) rather than ending its run.
    sup.agent_mut().approval.set_fail_on_ask(false);
    // Sub-agents don't nest (as in Alter Zero): nothing would receive a
    // child's spawn requests, so a call to it used to wait forever.
    sup.agent_mut().tools.retain_by_name(|name| {
        !matches!(
            name,
            "spawn_subagent" | "agent_output" | "agent_kill" | "update_goal"
        )
    });

    if let Some(allowed) = cfg.agent.as_ref().and_then(|a| a.tools.clone()) {
        sup.agent_mut()
            .tools
            .retain_by_name(|name| allowed.iter().any(|a| a == name));
    }

    sup.set_parent(
        cfg.parent_session_id.clone(),
        Some(cfg.parent_summary.clone()),
        Some(cfg.task.clone()),
    );
    if let Some(info) = worktree {
        sup.set_worktree_info(
            info.worktree_path.clone(),
            info.branch_name.clone(),
            info.base_branch.clone(),
        );
    }

    if let Some(ref tx) = event_tx {
        let _ = tx
            .send(AgentEvent::ChildSessionSpawned {
                parent_session_id: cfg.parent_session_id.clone(),
                child_session_id: child_id.clone(),
                task: cfg.task.clone(),
                workspace: sup.workspace_root.clone(),
                branch: sup.branch.clone(),
            })
            .await;
    }

    let role = match &cfg.agent {
        Some(agent) => format!(
            "You are the `{}` agent, spawned by a parent session.\n\n## Agent Instructions\n{}",
            agent.name, agent.prompt
        ),
        None => {
            "You are a sub-agent spawned by a parent session to handle a specific task.".to_string()
        }
    };
    let mut context_prompt = format!(
        "{role}\n\n\
         ## Parent Context\n{}\n\n\
         ## Your Task\n{}",
        cfg.parent_summary, cfg.task
    );

    if !cfg.focus_files.is_empty() {
        context_prompt.push_str("\n\n## Focus Files\n");
        for f in &cfg.focus_files {
            context_prompt.push_str(&format!("- {f}\n"));
        }
    }

    let mut handle = sup.take_handle();
    let event_rx = handle.take_event_rx();
    let log_path = handle.event_log_path.clone();

    let parent_forward = event_tx.clone().map(|tx| (child_id.clone(), tx));
    let fanout = event_rx.map(|rx| spawn_event_fanout(rx, log_path, None, None, parent_forward));

    let result = sup.run_turn(&context_prompt).await;

    let (status, output) = match result {
        Ok(text) => {
            sup.finish(EndReason::Completed).await;
            ("completed".to_string(), text)
        }
        Err(e) => {
            sup.finish(EndReason::Error).await;
            ("error".to_string(), e.to_string())
        }
    };

    if let Some(f) = fanout {
        f.abort();
    }

    let mut branch = sup.branch.clone();
    let mut wt_path = sup.worktree_path.clone().map(|p| p.display().to_string());
    let mut output = output;
    // A worktree the child left untouched is removed with its branch; one
    // with work stays, and the parent is told how to merge it.
    if let (Some(path), Some(base)) = (sup.worktree_path.clone(), sup.base_branch.clone()) {
        let mgr = crate::worktree::WorktreeManager::new(&cfg.workspace_root);
        if mgr.has_work(&path, &base) {
            // Commit it on the sub-agent's branch so nothing is lost and the
            // user can merge it in one step.
            let task_line: String = cfg
                .task
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect();
            if let Err(e) = mgr.commit_pending(&path, &format!("dcode-ai sub-agent: {task_line}")) {
                tracing::warn!("could not commit sub-agent work: {e}");
            }
            let (files, insertions, deletions) = mgr.branch_summary(&path, &base);
            output.push_str(&format!(
                "\n\n[worktree] The changes are on branch {} ({} file(s), +{insertions} -{deletions}); \
                 the user is asked whether to merge them. Don't merge them yourself.",
                branch.as_deref().unwrap_or("?"),
                files.len(),
            ));
            if let (Some(tx), Some(b)) = (&event_tx, branch.clone()) {
                let _ = tx
                    .send(AgentEvent::SubagentChanges {
                        child_session_id: child_id.clone(),
                        task: cfg.task.clone(),
                        branch: b,
                        base_branch: base.clone(),
                        worktree_path: path.clone(),
                        files,
                        insertions,
                        deletions,
                    })
                    .await;
            }
        } else if mgr.remove_worktree(&child_id, true).is_ok() {
            wt_path = None;
            branch = None;
        }
    }

    Ok(ChildSessionResult {
        child_session_id: child_id,
        status,
        output,
        workspace: sup.workspace_root.display().to_string(),
        branch,
        worktree_path: wt_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_manager::ContextManager;
    use dcode_ai_common::message::{Message, MessageToolCall, Role};

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

    #[test]
    fn empty_messages_fallback_uses_default_message() {
        let fallback = build_compaction_fallback_summary(&[]);
        assert_eq!(
            fallback,
            "Earlier conversation context was compacted due to token limits."
        );
    }

    #[test]
    fn artifacts_capture_files_commands_and_errors() {
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
    fn explorers_read_only_on_the_cheap_model() {
        let mut config = DcodeAiConfig::default();
        config.provider.default = dcode_ai_common::config::ProviderKind::Anthropic;
        config.model.subagent_model = Some("sonnet".into());
        let cfg = |explore| ChildSessionConfig {
            parent_session_id: "p".into(),
            task: "find it".into(),
            agent: None,
            workspace_root: PathBuf::from("/w"),
            config: config.clone(),
            parent_summary: String::new(),
            use_worktree: false,
            focus_files: Vec::new(),
            explore,
            parent_mode: Some(dcode_ai_common::config::PermissionMode::Auto),
            child_id: None,
            approval: None,
        };
        let (mode, model) = child_mode_and_model(&cfg(true));
        assert_eq!(mode, dcode_ai_common::config::PermissionMode::Plan);
        assert!(model.unwrap().contains("haiku"));
        let (mode, model) = child_mode_and_model(&cfg(false));
        assert_eq!(mode, dcode_ai_common::config::PermissionMode::Auto);
        assert_eq!(model.as_deref(), Some("sonnet"));
    }

    #[test]
    fn children_follow_the_parent_mode_without_prompts() {
        use dcode_ai_common::config::PermissionMode as M;
        assert_eq!(child_permission_mode(M::Plan), M::Plan);
        assert_eq!(child_permission_mode(M::Default), M::Auto);
        assert_eq!(child_permission_mode(M::AcceptEdits), M::Auto);
        assert_eq!(child_permission_mode(M::Auto), M::Auto);
        assert_eq!(child_permission_mode(M::DontAsk), M::DontAsk);
        assert_eq!(
            child_permission_mode(M::BypassPermissions),
            M::BypassPermissions
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
}
