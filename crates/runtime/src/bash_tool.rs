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
            description: "Execute a NON-INTERACTIVE shell command in the workspace. Stdin is \
detached, so any command that prompts for input — a `sudo` password, a `[y/N]` confirmation, an \
installer wizard, a REPL — will fail or time out here. For those, use the `interactive_exec` \
tool instead (it runs in a real PTY and lets the user answer prompts)."
                .into(),
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
            Ok(out) => {
                let success = out.exit_code == 0;
                let mut output = if out.stdout.is_empty() {
                    format!("Command exited with status {}", out.exit_code)
                } else {
                    out.stdout
                };
                // If the command failed because it needed interactive input
                // (stdin is detached here), point the model at the PTY-backed
                // interactive_exec tool that can answer such prompts.
                if !success && looks_interactive(&output) {
                    output.push_str(
                        "\n\n[hint] This command appears to need interactive input (e.g. a \
password or confirmation). Re-run it with the `interactive_exec` tool, which runs in a real \
terminal and lets you answer prompts.",
                    );
                }
                ToolResult {
                    call_id: call.id.clone(),
                    success,
                    output,
                    error: None,
                }
            }
            Err(err) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(err.to_string()),
            },
        }
    }
}

/// Heuristic: does this command output indicate it failed because it needed an
/// interactive terminal (so the agent should retry via `interactive_exec`)?
fn looks_interactive(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    [
        "a terminal is required",
        "no tty",
        "askpass",
        "a password is required",
        "sudo: a password",
        "must be run from a terminal",
        "not a terminal",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}
