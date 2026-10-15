use crate::pty::{PtyError, PtyManager, PtyOutput};
use dcode_ai_common::event::AgentEvent;
use dcode_ai_common::event::BackgroundJobStatus;
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use dcode_ai_core::tools::ToolExecutor;
use dcode_ai_core::tools::background_exec::BackgroundJobs;
use std::sync::Arc;

/// Runtime-backed bash tool that executes shell commands via PTY.
/// When an event sender is provided, stdout/stderr lines are streamed
/// to the TUI as `ToolOutputDelta` events for live feedback.
pub struct RuntimeBashTool {
    pty: Arc<PtyManager>,
    event_tx: Option<tokio::sync::mpsc::Sender<AgentEvent>>,
    /// Set for the interactive session: Ctrl+B moves a running command here.
    jobs: Option<BackgroundJobs>,
}

/// Ctrl+B: "move the running shell command to the background". Only the
/// interactive session's shell tool listens (sub-agents never do).
pub fn background_signal() -> Arc<tokio::sync::Notify> {
    static SIGNAL: std::sync::OnceLock<Arc<tokio::sync::Notify>> = std::sync::OnceLock::new();
    SIGNAL
        .get_or_init(|| Arc::new(tokio::sync::Notify::new()))
        .clone()
}

/// Longest a command may run once it can be moved to the background; the
/// per-call `timeout_secs` applies while it is still in the foreground.
const BACKGROUND_MAX_SECS: u64 = 24 * 60 * 60;

/// Aborts the command task when the tool call is dropped (turn cancelled),
/// unless the task was handed to the background.
struct AbortOnDrop(Option<tokio::task::JoinHandle<Result<PtyOutput, PtyError>>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            h.abort();
        }
    }
}

impl RuntimeBashTool {
    pub fn new(pty: Arc<PtyManager>) -> Self {
        Self {
            pty,
            event_tx: None,
            jobs: None,
        }
    }

    pub fn with_event_tx(
        pty: Arc<PtyManager>,
        event_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> Self {
        Self {
            pty,
            event_tx: Some(event_tx),
            jobs: None,
        }
    }

    /// Let Ctrl+B move this tool's running command into `jobs`.
    pub fn with_background_jobs(mut self, jobs: BackgroundJobs) -> Self {
        self.jobs = Some(jobs);
        self
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

        let result = match &self.jobs {
            None => {
                self.pty
                    .exec_streaming(command, timeout_secs, line_tx)
                    .await
            }
            Some(jobs) => {
                // Run as a task so Ctrl+B can hand it to the background
                // while it keeps running; the foreground timeout is ours.
                let pty = self.pty.clone();
                let cmd = command.to_string();
                let mut guard = AbortOnDrop(Some(tokio::spawn(async move {
                    pty.exec_streaming(&cmd, BACKGROUND_MAX_SECS, line_tx).await
                })));
                let signal = background_signal();
                let handle = guard.0.as_mut().expect("task");
                tokio::select! {
                    joined = handle => {
                        guard.0 = None;
                        joined.unwrap_or_else(|e| Err(PtyError::SpawnFailed(e.to_string())))
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs)) => {
                        Err(PtyError::Timeout(timeout_secs))
                    }
                    _ = signal.notified() => {
                        let task = guard.0.take().expect("task");
                        let id = jobs
                            .adopt(
                                command.to_string(),
                                self.event_tx.clone(),
                                Box::pin(async move {
                                    match task.await {
                                        Ok(Ok(out)) => (
                                            if out.exit_code == 0 {
                                                BackgroundJobStatus::Completed
                                            } else {
                                                BackgroundJobStatus::Failed
                                            },
                                            Some(out.exit_code),
                                            out.stdout,
                                        ),
                                        Ok(Err(e)) => {
                                            (BackgroundJobStatus::Failed, None, e.to_string())
                                        }
                                        Err(e) => {
                                            (BackgroundJobStatus::Failed, None, e.to_string())
                                        }
                                    }
                                }),
                            )
                            .await;
                        return ToolResult {
                            call_id: call.id.clone(),
                            success: true,
                            output: format!(
                                "The user moved this command to the background (job {id} · \
                /bashes to manage). It is still running; you will get a <task-notification> when it finishes. \
                Do not wait for it — continue with other work or end your turn."
                            ),
                            error: None,
                        };
                    }
                }
            }
        };
        match result {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn ctrl_b_moves_a_running_command_to_the_background() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let jobs = BackgroundJobs::default();
        let tool = RuntimeBashTool::with_event_tx(Arc::new(PtyManager::new(dir.path())), tx)
            .with_background_jobs(jobs.clone());
        let call = ToolCall {
            id: "c1".into(),
            name: "execute_bash".into(),
            input: json!({"command": "sleep 1; echo bg-done"}),
        };
        let run = tokio::spawn(async move { tool.execute(&call).await });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        background_signal().notify_waiters();
        let res = tokio::time::timeout(std::time::Duration::from_millis(500), run)
            .await
            .expect("tool returns as soon as it is backgrounded")
            .unwrap();
        assert!(
            res.output
                .contains("moved this command to the background (job 1"),
            "{}",
            res.output
        );

        let ended = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(AgentEvent::BackgroundJob {
                    status,
                    output_tail,
                    ..
                }) = rx.recv().await
                    && !status.is_running()
                {
                    return (status, output_tail);
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(ended.0, BackgroundJobStatus::Completed);
        assert!(ended.1.contains("bg-done"), "{}", ended.1);
        assert_eq!(jobs.list()[0].exit_code, Some(0));
    }
}
