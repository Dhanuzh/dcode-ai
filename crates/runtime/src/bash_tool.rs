use crate::pty::PtyManager;
use dcode_ai_common::event::AgentEvent;
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use dcode_ai_core::tools::ToolExecutor;
use std::sync::Arc;

/// Runtime-backed bash tool that executes shell commands via PTY.
/// When an event sender is provided, stdout/stderr lines are streamed
/// to the TUI as `ToolOutputDelta` events for live feedback.
pub struct RuntimeBashTool {
    pty: Arc<PtyManager>,
    event_tx: Option<tokio::sync::mpsc::Sender<AgentEvent>>,
}

impl RuntimeBashTool {
    pub fn new(pty: Arc<PtyManager>) -> Self {
        Self {
            pty,
            event_tx: None,
        }
    }

    pub fn with_event_tx(
        pty: Arc<PtyManager>,
        event_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> Self {
        Self {
            pty,
            event_tx: Some(event_tx),
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for RuntimeBashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "execute_bash".into(),
            description: "Execute a shell command in the workspace".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute"
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Command timeout in seconds (default: 30)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let command = call.input["command"].as_str().unwrap_or("");
        let timeout_secs = call.input["timeout_secs"].as_u64().unwrap_or(30);

        // Set up a line-streaming channel if we have an event sender.
        let line_tx = if let Some(event_tx) = &self.event_tx {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(64);
            let etx = event_tx.clone();
            let cid = call.id.clone();
            tokio::spawn(async move {
                while let Some(line) = rx.recv().await {
                    let _ = etx.try_send(AgentEvent::ToolOutputDelta {
                        call_id: cid.clone(),
                        delta: line,
                    });
                }
            });
            Some(tx)
        } else {
            None
        };

        match self
            .pty
            .exec_streaming(command, timeout_secs, line_tx)
            .await
        {
            Ok(out) => ToolResult {
                call_id: call.id.clone(),
                success: out.exit_code == 0,
                output: if out.stdout.is_empty() {
                    format!("Command exited with status {}", out.exit_code)
                } else {
                    out.stdout
                },
                error: None,
            },
            Err(err) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(err.to_string()),
            },
        }
    }
}
