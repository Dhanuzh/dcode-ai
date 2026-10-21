use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use tokio::sync::{mpsc, oneshot};

use super::ToolExecutor;

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
    pub reply: oneshot::Sender<SpawnResponse>,
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
}

impl SpawnSubagentTool {
    pub fn new(spawn_tx: mpsc::Sender<SpawnRequest>) -> Self {
        Self {
            spawn_tx,
            agents: Vec::new(),
            parent_mode: None,
        }
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
