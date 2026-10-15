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
        let shell = dcode_ai_common::shell::workspace_shell();
        ToolDefinition {
            name: "execute_bash".into(),
            description: format!(
                "Execute a NON-INTERACTIVE command in the workspace via {shell_name}. Stdin is \
detached, so any command that prompts for input — a `sudo` password, a `[y/N]` confirmation, an \
installer wizard, a REPL — will fail or time out here. For those, use the `interactive_exec` \
tool instead (it runs in a real PTY and lets the user answer prompts). Do NOT create or modify \
files with this tool (no `echo … >> file` or heredocs) — use write_file / edit_file instead.",
                shell_name = shell.display_name()
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": format!("Command to execute ({} syntax)", shell.display_name())
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
                // Steer models away from building files line-by-line with echo
                // redirection (slow, encoding-hostile, and easy to corrupt).
                if writes_file_via_redirection(command) {
                    output.push_str(
                        "\n\n[hint] Do not write file contents through shell redirection. Use \
the `write_file` tool to create the whole file in one call (or `edit_file` to change part of \
it).",
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

/// Heuristic: is this an `echo`/`printf`/`type`/`Add-Content`-style command
/// that writes file contents via redirection? Those should go through the
/// write_file/edit_file tools instead.
fn writes_file_via_redirection(command: &str) -> bool {
    let trimmed = command.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let starts_with_writer = [
        "echo ",
        "echo.",
        "printf ",
        "cat ",
        "type ",
        "add-content ",
        "set-content ",
    ]
    .iter()
    .any(|p| lower.starts_with(p));
    starts_with_writer && (trimmed.contains(">>") || trimmed.contains('>'))
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
