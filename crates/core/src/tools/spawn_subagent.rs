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
}

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
    /// The parent's permission mode when the call was made.
    pub parent_mode: Option<dcode_ai_common::config::PermissionMode>,
    /// Return at once and report completion with a notification.
    pub background: bool,
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
        }
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
                to child agents that work in isolated git worktrees."
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
                    "use_worktree": {
                        "type": "boolean",
                        "description": "If true, the sub-agent runs in an isolated git worktree branch. Defaults to true."
                    },
                    "run_in_background": {
                        "type": "boolean",
                        "description": "Start the agent and return at once with its id; you get a <task-notification> when it finishes (agent_output shows progress, agent_kill stops it). Use false when you need the result before your next step."
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

        let use_worktree = call.input["use_worktree"].as_bool().unwrap_or(true);
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
            parent_mode: self
                .parent_mode
                .as_ref()
                .and_then(|c| c.read().ok().map(|m| *m)),
            background,
            agents: self.registry.clone(),
            approval: self.approval.clone(),
            reply: reply_tx,
        };

        if self.spawn_tx.send(req).await.is_err() {
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
        let mut map = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let (success, text) = match map.get_mut(&id) {
            Some(r) if r.status == "running" => {
                if let Some(a) = r.abort.take() {
                    a.abort();
                }
                r.status = "stopped".into();
                r.finished = Some(Instant::now());
                r.output = "Stopped before it finished.".into();
                (true, format!("Agent {id} stopped."))
            }
            Some(r) => (true, format!("Agent {id} already {}.", r.status)),
            None => (false, format!("no agent `{id}`")),
        };
        ToolResult {
            call_id: call.id.clone(),
            success,
            output: if success { text.clone() } else { String::new() },
            error: (!success).then_some(text),
        }
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    fn record(status: &str) -> AgentRecord {
        AgentRecord {
            task: "t".into(),
            status: status.into(),
            output: "report".into(),
            started: Instant::now(),
            finished: None,
            abort: None,
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
}
