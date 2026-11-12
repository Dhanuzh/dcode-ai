use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use tokio::sync::{mpsc, oneshot};

use super::ToolExecutor;
use crate::approval::ApprovalHandler;

/// A sub-agent this session started, for `agent_output` / `agent_kill`.
#[derive(Debug)]
pub struct AgentRecord {
    pub task: String,
    /// `running`, `completed`, `error` or `stopped`.
    pub status: String,
    /// The agent's final report (or error) once it settles.
    pub output: String,
    pub started: Instant,
    pub finished: Option<Instant>,
    /// Stops a background agent.
    pub abort: Option<tokio::task::AbortHandle>,
    /// Its custom agent, if any (kept when it's continued).
    pub agent: Option<String>,
    /// A read-only explorer (kept when it's continued).
    pub explore: bool,
    /// Messages for it while it runs (`send_message`); read at its next step.
    pub steer: Option<crate::agent::SteerInbox>,
}

impl AgentRecord {
    pub fn new(task: &str) -> Self {
        Self {
            task: task.to_string(),
            status: "running".into(),
            output: String::new(),
            started: Instant::now(),
            finished: None,
            abort: None,
            agent: None,
            explore: false,
            steer: None,
        }
    }
}

/// The parent's conversation as of its latest tool batch that spawns or
/// messages an agent — so a sub-agent sees the conversation as it is now.
pub type MessagesMirror = Arc<Mutex<Vec<dcode_ai_common::message::Message>>>;

/// Every sub-agent of this session, by id.
pub type BackgroundAgents = Arc<Mutex<HashMap<String, AgentRecord>>>;

/// Request sent from the tool to the runtime to spawn a child session.
#[derive(Debug)]
pub struct SpawnRequest {
    pub task: String,
    /// Custom agent definition to run (`.dcode-ai/agents/<name>.md`).
    pub agent: Option<String>,
    pub focus_files: Vec<String>,
    pub use_worktree: bool,
    /// Read-only research: plan mode, no worktree, the cheap utility model.
    pub explore: bool,
    /// The parent's permission mode when the call was made.
    pub parent_mode: Option<dcode_ai_common::config::PermissionMode>,
    /// Return at once and report completion with a notification.
    pub background: bool,
    /// A `browser_task`: the child gets only the browser and read-only file
    /// tools, and reports back briefly.
    pub browser_task: bool,
    /// Continue this existing sub-agent (its id) with `task` as its next
    /// message, instead of starting a new one.
    pub resume: Option<String>,
    /// Start with a copy of the parent's conversation, not a summary.
    pub fork: bool,
    /// The parent's conversation now (for the summary, or the fork).
    pub parent_messages: Option<Vec<dcode_ai_common::message::Message>>,
    pub agents: BackgroundAgents,
    /// The parent's approval handler: the child's prompts go to the user
    /// through it instead of being denied.
    pub approval: Option<Arc<dyn ApprovalHandler>>,
    pub reply: oneshot::Sender<SpawnResponse>,
}

impl std::fmt::Debug for dyn ApprovalHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApprovalHandler")
    }
}

/// Response from the runtime after spawning a child session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpawnResponse {
    pub child_session_id: String,
    pub status: String,
    pub output: String,
    pub workspace: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
}

pub struct SpawnSubagentTool {
    spawn_tx: mpsc::Sender<SpawnRequest>,
    /// `(name, description)` of custom agents the model may pick.
    agents: Vec<(String, String)>,
    parent_mode: Option<crate::approval::ModeCell>,
    registry: BackgroundAgents,
    /// `run_in_background` when the model doesn't say; on only where a UI
    /// delivers completion notifications (the TUI).
    background_default: Arc<std::sync::atomic::AtomicBool>,
    approval: Option<Arc<dyn ApprovalHandler>>,
    mirror: Option<MessagesMirror>,
}

impl SpawnSubagentTool {
    pub fn new(spawn_tx: mpsc::Sender<SpawnRequest>) -> Self {
        Self {
            spawn_tx,
            agents: Vec::new(),
            parent_mode: None,
            registry: Default::default(),
            background_default: Default::default(),
            approval: None,
            mirror: None,
        }
    }

    /// Read the parent's current conversation from `mirror`.
    pub fn with_mirror(mut self, mirror: MessagesMirror) -> Self {
        self.mirror = Some(mirror);
        self
    }

    /// Share the session's agent registry (for `agent_output` / `agent_kill`).
    pub fn with_registry(mut self, registry: BackgroundAgents) -> Self {
        self.registry = registry;
        self
    }

    /// Share the switch for launching in the background by default.
    pub fn with_background_default(mut self, switch: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.background_default = switch;
        self
    }

    /// Route sub-agents' approval prompts to the user through `handler`.
    pub fn with_approval(mut self, handler: Option<Arc<dyn ApprovalHandler>>) -> Self {
        self.approval = handler;
        self
    }

    /// Pass the parent's live permission mode to each child.
    pub fn with_parent_mode(mut self, cell: crate::approval::ModeCell) -> Self {
        self.parent_mode = Some(cell);
        self
    }

    /// Offer custom agents through the `agent` parameter.
    pub fn with_agents(mut self, agents: Vec<(String, String)>) -> Self {
        self.agents = agents;
        self
    }
}

fn summarize_subagent_error(output: &str, max_chars: usize) -> String {
    let one_line = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown child error");
    if one_line.chars().count() <= max_chars {
        one_line.to_string()
    } else {
        format!(
            "{}…",
            one_line
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    }
}

#[async_trait::async_trait]
impl ToolExecutor for SpawnSubagentTool {
    fn definition(&self) -> ToolDefinition {
        let mut def = self.base_definition();
        if !self.agents.is_empty() {
            let catalog = self
                .agents
                .iter()
                .map(|(n, d)| {
                    if d.is_empty() {
                        format!("- {n}")
                    } else {
                        format!("- {n}: {d}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            def.description.push_str(&format!(
                "\n\nSpecialised agents (pass `agent` to use one; omit for a general agent):\n{catalog}"
            ));
            def.parameters["properties"]["agent"] = serde_json::json!({
                "type": "string",
                "enum": self.agents.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
                "description": "Name of a specialised agent to run the task."
            });
        }
        def
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        self.run(call).await
    }
}

impl SpawnSubagentTool {
    fn base_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "spawn_subagent".into(),
            description: "Spawn a sub-agent that runs as a separate session to handle a specific \
                task in parallel. The sub-agent inherits your conversation context and workspace. \
                Use this to delegate independent tasks (e.g. creating files, running builds) \
                to child agents that work in isolated git worktrees. The result includes the \
                agent's id: `send_message` continues it later with its history intact (better \
                than re-explaining to a new agent)."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "A clear, self-contained description of what the sub-agent should do."
                    },
                    "focus_files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of file paths the sub-agent should focus on."
                    },
                    "explore": {
                        "type": "boolean",
                        "description": "Read-only research (find code, read, summarize): runs on a fast, cheap model with read-only tools and no worktree. Use it for lookups; not for changes."
                    },
                    "use_worktree": {
                        "type": "boolean",
                        "description": "If true, the sub-agent runs in an isolated git worktree branch. Defaults to true."
                    },
                    "run_in_background": {
                        "type": "boolean",
                        "description": "Start the agent and return at once with its id; you get a <task-notification> when it finishes (agent_output shows progress, agent_kill stops it). Use false when you need the result before your next step."
                    },
                    "fork": {
                        "type": "boolean",
                        "description": "Start the agent with a copy of this whole conversation instead of a summary — for side work that needs everything discussed so far. Costs more; use only when a summary won't do."
                    }
                },
                "required": ["task"]
            }),
        }
    }

    async fn run(&self, call: &ToolCall) -> ToolResult {
        let task = call.input["task"].as_str().unwrap_or("").to_string();
        let agent = call.input["agent"]
            .as_str()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string);

        if task.is_empty() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("task is required".into()),
            };
        }

        let focus_files: Vec<String> = call.input["focus_files"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let explore = call.input["explore"].as_bool().unwrap_or(false);
        let fork = !explore && call.input["fork"].as_bool().unwrap_or(false);
        // Explorers only read: no worktree to create and clean up.
        let use_worktree = !explore && call.input["use_worktree"].as_bool().unwrap_or(true);
        let background = call.input["run_in_background"]
            .as_bool()
            .unwrap_or_else(|| {
                self.background_default
                    .load(std::sync::atomic::Ordering::Relaxed)
            });

        let (reply_tx, reply_rx) = oneshot::channel();

        let req = SpawnRequest {
            task,
            agent,
            focus_files,
            use_worktree,
            explore,
            parent_mode: self
                .parent_mode
                .as_ref()
                .and_then(|c| c.read().ok().map(|m| *m)),
            background,
            browser_task: false,
            resume: None,
            fork,
            parent_messages: self
                .mirror
                .as_ref()
                .and_then(|m| m.lock().ok().map(|v| v.clone())),
            agents: self.registry.clone(),
            approval: self.approval.clone(),
            reply: reply_tx,
        };

        send_and_wait(&self.spawn_tx, req, reply_rx, call).await
    }
}

/// Sends a spawn (or continue) request and turns the answer into the tool's
/// result.
async fn send_and_wait(
    spawn_tx: &mpsc::Sender<SpawnRequest>,
    req: SpawnRequest,
    reply_rx: oneshot::Receiver<SpawnResponse>,
    call: &ToolCall,
) -> ToolResult {
    if spawn_tx.send(req).await.is_err() {
        return ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some("Sub-agent spawner is not available".into()),
        };
    }

    match tokio::time::timeout(std::time::Duration::from_secs(600), reply_rx).await {
        Ok(Ok(response)) if response.status == "running" => ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: format!(
                "Agent {id} started in the background on: {task}\n\
You get a <task-notification> when it finishes — don't poll. Use agent_output with \
agent_id {id} for its progress (wait=<seconds> to block until it finishes), agent_kill to stop it.",
                id = response.child_session_id,
                task = response.output,
            ),
            error: None,
        },
        Ok(Ok(response)) => {
            let output = serde_json::to_string_pretty(&response).unwrap_or_default();
            let success = response.status == "completed";
            ToolResult {
                call_id: call.id.clone(),
                success,
                output,
                error: if success {
                    None
                } else {
                    let detail = summarize_subagent_error(&response.output, 180);
                    Some(format!(
                        "Sub-agent finished with status: {} ({detail})",
                        response.status,
                    ))
                },
            }
        }
        Ok(Err(_)) => ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some("Sub-agent spawner dropped the reply channel".into()),
        },
        Err(_) => ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some("Sub-agent timed out after 600 seconds".into()),
        },
    }
}

fn elapsed(r: &AgentRecord) -> String {
    let d = r.finished.unwrap_or_else(Instant::now) - r.started;
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// `agent_output`: a sub-agent's state and, once settled, its final report.
pub struct AgentOutputTool {
    registry: BackgroundAgents,
}

impl AgentOutputTool {
    pub fn new(registry: BackgroundAgents) -> Self {
        Self { registry }
    }

    fn report(&self, id: &str) -> Option<(bool, String)> {
        let map = self.registry.lock().ok()?;
        let r = map.get(id)?;
        let settled = r.status != "running";
        let mut out = format!("{} (agent {id} · {} · {})", r.status, r.task, elapsed(r));
        if settled {
            out.push_str("\n\nFinal report:\n");
            out.push_str(r.output.trim());
        }
        Some((settled, out))
    }
}

#[async_trait::async_trait]
impl ToolExecutor for AgentOutputTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "agent_output".into(),
            description: "State of a sub-agent you started (by agent_id), and its final report \
once it has finished. `wait` (seconds, up to 600) blocks until it finishes. Only wait when you \
need the result now; otherwise a <task-notification> arrives when it ends."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": { "type": "string" },
                    "wait": { "type": "integer", "description": "Seconds to wait for it to finish (0-600)." }
                },
                "required": ["agent_id"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let id = call.input["agent_id"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        let wait = call.input["wait"].as_u64().unwrap_or(0).min(600);
        let deadline = Instant::now() + std::time::Duration::from_secs(wait);
        loop {
            let Some((settled, report)) = self.report(&id) else {
                let known: Vec<String> = self
                    .registry
                    .lock()
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "no agent `{id}`; this session's agents: {}",
                        if known.is_empty() {
                            "none".into()
                        } else {
                            known.join(", ")
                        }
                    )),
                };
            };
            if settled || Instant::now() >= deadline {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: true,
                    output: report,
                    error: None,
                };
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

/// `browser_task`: a multi-step job in the browser or a connected desktop app,
/// done by a sub-agent so the page snapshots and screenshots stay in its
/// context — only its short report comes back.
pub struct BrowserTaskTool {
    spawn_tx: mpsc::Sender<SpawnRequest>,
    parent_mode: Option<crate::approval::ModeCell>,
    registry: BackgroundAgents,
    approval: Option<Arc<dyn ApprovalHandler>>,
}

impl BrowserTaskTool {
    pub fn new(
        spawn_tx: mpsc::Sender<SpawnRequest>,
        parent_mode: Option<crate::approval::ModeCell>,
        registry: BackgroundAgents,
        approval: Option<Arc<dyn ApprovalHandler>>,
    ) -> Self {
        Self {
            spawn_tx,
            parent_mode,
            registry,
            approval,
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for BrowserTaskTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser_task".into(),
            description: "Do a multi-step job in the web browser or a connected desktop app \
(VS Code, Slack, Teams, Outlook, …) through a helper agent: logging in, filling forms, \
collecting data across pages, testing a flow. Its page snapshots and screenshots stay in its own \
context; you get back a short report — what it did, the data you asked for, what's on screen \
now. Prefer this over many `browser` calls; use `browser` directly for one quick look or click. \
The page or app stays open afterwards."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "goal": {"type": "string", "description": "What to get done, and what to report back (the data you need)"},
                    "target": {"type": "string", "description": "A connected desktop app (e.g. \"vscode\") instead of the browser"},
                    "url": {"type": "string", "description": "Page to start from"}
                },
                "required": ["goal"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let fail = |e: String| ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(e),
        };
        let goal = call.input["goal"].as_str().unwrap_or("").trim().to_string();
        if goal.is_empty() {
            return fail("goal is required".into());
        }
        let mut task = goal;
        if let Some(t) = call.input["target"]
            .as_str()
            .filter(|t| !t.trim().is_empty())
        {
            task.push_str(&format!(
                "\n\nWork in the desktop app `{t}`: pass target: \"{t}\" to the browser tool."
            ));
        }
        if let Some(u) = call.input["url"].as_str().filter(|u| !u.trim().is_empty()) {
            task.push_str(&format!("\n\nStart at {u}."));
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = SpawnRequest {
            task,
            agent: None,
            focus_files: Vec::new(),
            use_worktree: false,
            explore: false,
            parent_mode: self
                .parent_mode
                .as_ref()
                .and_then(|c| c.read().ok().map(|m| *m)),
            background: false,
            browser_task: true,
            resume: None,
            fork: false,
            parent_messages: None,
            agents: self.registry.clone(),
            approval: self.approval.clone(),
            reply: reply_tx,
        };
        if self.spawn_tx.send(req).await.is_err() {
            return fail("Sub-agent spawner is not available".into());
        }
        match tokio::time::timeout(std::time::Duration::from_secs(1800), reply_rx).await {
            Ok(Ok(r)) => ToolResult {
                call_id: call.id.clone(),
                success: r.status == "completed",
                output: r.output.clone(),
                error: (r.status != "completed").then(|| format!("browser task {}", r.status)),
            },
            Ok(Err(_)) => fail("the browser task ended without a report".into()),
            Err(_) => fail("the browser task took more than 30 minutes".into()),
        }
    }
}

/// Stops a running background sub-agent (`agent_kill`, or the user from the
/// TUI). Returns whether the agent exists, and what happened in words.
pub fn stop_agent(registry: &BackgroundAgents, id: &str, why: &str) -> (bool, String) {
    let mut map = registry.lock().unwrap_or_else(|e| e.into_inner());
    match map.get_mut(id) {
        Some(r) if r.status == "running" => {
            if let Some(a) = r.abort.take() {
                a.abort();
            }
            r.status = "stopped".into();
            r.finished = Some(Instant::now());
            r.output = why.to_string();
            (true, format!("Agent {id} stopped."))
        }
        Some(r) => (true, format!("Agent {id} already {}.", r.status)),
        None => (false, format!("no agent `{id}`")),
    }
}

/// `agent_kill`: stop a running sub-agent.
pub struct AgentKillTool {
    registry: BackgroundAgents,
}

impl AgentKillTool {
    pub fn new(registry: BackgroundAgents) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for AgentKillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "agent_kill".into(),
            description: "Stop a running sub-agent you started (by agent_id).".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "agent_id": { "type": "string" } },
                "required": ["agent_id"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let id = call.input["agent_id"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        let (success, text) = stop_agent(&self.registry, &id, "Stopped before it finished.");
        ToolResult {
            call_id: call.id.clone(),
            success,
            output: if success { text.clone() } else { String::new() },
            error: (!success).then_some(text),
        }
    }
}

/// `send_message`: continue a sub-agent this session started. A running one
/// gets the message at its next step; a finished one is resumed with its
/// history and works the message as its next turn.
pub struct SendMessageTool {
    spawn_tx: mpsc::Sender<SpawnRequest>,
    registry: BackgroundAgents,
    parent_mode: Option<crate::approval::ModeCell>,
    background_default: Arc<std::sync::atomic::AtomicBool>,
    approval: Option<Arc<dyn ApprovalHandler>>,
}

impl SendMessageTool {
    pub fn new(
        spawn_tx: mpsc::Sender<SpawnRequest>,
        registry: BackgroundAgents,
        parent_mode: Option<crate::approval::ModeCell>,
        background_default: Arc<std::sync::atomic::AtomicBool>,
        approval: Option<Arc<dyn ApprovalHandler>>,
    ) -> Self {
        Self {
            spawn_tx,
            registry,
            parent_mode,
            background_default,
            approval,
        }
    }
}

/// What `send_message` does with an agent, from its record.
#[derive(Debug, PartialEq)]
enum Delivery {
    /// It's running: the message went to its inbox.
    Steered,
    /// It was stopped on purpose; not continued.
    Stopped,
    /// Finished (or unknown here): resume it.
    Resume,
}

fn deliver_to_running(registry: &BackgroundAgents, id: &str, message: &str) -> Delivery {
    let Ok(map) = registry.lock() else {
        return Delivery::Resume;
    };
    match map.get(id) {
        Some(r) if r.status == "running" => match &r.steer {
            Some(inbox) => {
                if let Ok(mut q) = inbox.lock() {
                    q.push(format!("Message from the parent agent: {message}"));
                }
                Delivery::Steered
            }
            // Running but not reachable yet (just starting): resume would
            // race it, so queue nothing and say so.
            None => Delivery::Stopped,
        },
        Some(r) if r.status == "stopped" => Delivery::Stopped,
        _ => Delivery::Resume,
    }
}

#[async_trait::async_trait]
impl ToolExecutor for SendMessageTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "send_message".into(),
            description: "Continue a sub-agent you started (by its agent_id / child_session_id), \
with its whole history: a follow-up question, a fix to its work, the next step. A running agent \
reads the message at its next step; a finished one picks up where it left off (same worktree, \
same tools) and replies. Cheaper and better than starting a new agent and re-explaining."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": {"type": "string"},
                    "message": {"type": "string", "description": "What to tell it"},
                    "run_in_background": {"type": "boolean", "description": "For a finished agent: return at once and get a <task-notification> when it replies."}
                },
                "required": ["agent_id", "message"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let fail = |e: String| ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(e),
        };
        let id = call.input["agent_id"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        let message = call.input["message"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if id.is_empty() || message.is_empty() {
            return fail("agent_id and message are required".into());
        }
        match deliver_to_running(&self.registry, &id, &message) {
            Delivery::Steered => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: true,
                    output: format!(
                        "Sent to running agent {id}; it reads it at its next step. Its reply comes with its final report."
                    ),
                    error: None,
                };
            }
            Delivery::Stopped => {
                return fail(format!(
                    "agent {id} was stopped (or is just starting) — start a new agent instead, or try again in a moment"
                ));
            }
            Delivery::Resume => {}
        }
        let (agent, explore) = self
            .registry
            .lock()
            .ok()
            .and_then(|m| m.get(&id).map(|r| (r.agent.clone(), r.explore)))
            .unwrap_or((None, false));
        let background = call.input["run_in_background"]
            .as_bool()
            .unwrap_or_else(|| {
                self.background_default
                    .load(std::sync::atomic::Ordering::Relaxed)
            });
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = SpawnRequest {
            task: message,
            agent,
            focus_files: Vec::new(),
            use_worktree: false,
            explore,
            parent_mode: self
                .parent_mode
                .as_ref()
                .and_then(|c| c.read().ok().map(|m| *m)),
            background,
            browser_task: false,
            resume: Some(id),
            fork: false,
            parent_messages: None,
            agents: self.registry.clone(),
            approval: self.approval.clone(),
            reply: reply_tx,
        };
        send_and_wait(&self.spawn_tx, req, reply_rx, call).await
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    fn record(status: &str) -> AgentRecord {
        AgentRecord {
            status: status.into(),
            output: "report".into(),
            ..AgentRecord::new("t")
        }
    }

    #[tokio::test]
    async fn output_waits_for_a_finish_and_kill_stops_a_running_agent() {
        let reg: BackgroundAgents = Default::default();
        reg.lock().unwrap().insert("a1".into(), record("running"));
        let out = AgentOutputTool::new(reg.clone());
        let call = |id: &str, wait: u64| ToolCall {
            id: "c".into(),
            name: "agent_output".into(),
            input: serde_json::json!({"agent_id": id, "wait": wait}),
        };
        let r = out.execute(&call("a1", 0)).await;
        assert!(r.output.starts_with("running"), "{}", r.output);
        let reg2 = reg.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let mut m = reg2.lock().unwrap();
            let r = m.get_mut("a1").unwrap();
            r.status = "completed".into();
            r.finished = Some(Instant::now());
        });
        let r = out.execute(&call("a1", 5)).await;
        assert!(r.output.contains("Final report:\nreport"), "{}", r.output);
        assert!(!out.execute(&call("zz", 0)).await.success);

        reg.lock().unwrap().insert("a2".into(), record("running"));
        let kill = AgentKillTool::new(reg.clone());
        let k = kill
            .execute(&ToolCall {
                id: "k".into(),
                name: "agent_kill".into(),
                input: serde_json::json!({"agent_id": "a2"}),
            })
            .await;
        assert!(k.success);
        assert_eq!(reg.lock().unwrap()["a2"].status, "stopped");
    }

    #[test]
    fn messages_reach_a_running_agent_and_never_a_stopped_one() {
        let reg: BackgroundAgents = Default::default();
        let inbox: crate::agent::SteerInbox = Default::default();
        let mut running = record("running");
        running.steer = Some(inbox.clone());
        reg.lock().unwrap().insert("r".into(), running);
        reg.lock().unwrap().insert("s".into(), record("stopped"));
        reg.lock().unwrap().insert("c".into(), record("completed"));
        assert_eq!(
            deliver_to_running(&reg, "r", "also check X"),
            Delivery::Steered
        );
        assert_eq!(inbox.lock().unwrap().len(), 1);
        assert!(inbox.lock().unwrap()[0].ends_with("also check X"));
        assert_eq!(deliver_to_running(&reg, "s", "x"), Delivery::Stopped);
        assert_eq!(deliver_to_running(&reg, "c", "x"), Delivery::Resume);
        assert_eq!(deliver_to_running(&reg, "unknown", "x"), Delivery::Resume);
    }
}
