use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};

use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;

/// One detached shell job tracked by the background-exec registry.
struct BgJob {
    id: u32,
    cmd: String,
    output: Arc<Mutex<String>>,
    done: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct BgRegistry {
    jobs: Vec<BgJob>,
    next_id: u32,
}

/// Lets the agent run shell commands in the background (non-blocking) and poll
/// or stop them later — the equivalent of Codex's background terminals.
/// Actions: `start`, `list`, `output`, `stop`.
pub struct BackgroundExecTool {
    workspace_root: PathBuf,
    registry: Arc<Mutex<BgRegistry>>,
}

impl BackgroundExecTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            registry: Arc::new(Mutex::new(BgRegistry {
                jobs: Vec::new(),
                next_id: 1,
            })),
        }
    }

    fn ok(call: &ToolCall, output: String) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output,
            error: None,
        }
    }

    fn err(call: &ToolCall, msg: impl Into<String>) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(msg.into()),
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for BackgroundExecTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_background".into(),
            description: "Run a shell command in the BACKGROUND (non-blocking) so you can keep \
working while it runs — e.g. dev servers, watchers, long builds/tests. Actions: \
`start` (spawn `command`, returns a job id), `list` (show jobs + status), \
`output` (read a job's captured output, needs `id`), `stop` (kill a job, needs `id`). \
For commands you need the result of right now, use the normal blocking shell tool instead."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["start", "list", "output", "stop"]
                    },
                    "command": { "type": "string", "description": "Shell command (for action=start)." },
                    "id": { "type": "integer", "description": "Job id (for action=output/stop)." }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let action = call.input["action"].as_str().unwrap_or("list");
        match action {
            "start" => {
                let Some(cmd) = call.input["command"]
                    .as_str()
                    .filter(|c| !c.trim().is_empty())
                else {
                    return Self::err(call, "action=start requires a non-empty 'command'");
                };
                let output = Arc::new(Mutex::new(String::new()));
                let done = Arc::new(AtomicBool::new(false));
                let out_clone = output.clone();
                let done_clone = done.clone();
                let cmd_str = cmd.to_string();
                let ws = self.workspace_root.clone();
                let handle = tokio::spawn(async move {
                    // Stream stdout/stderr incrementally so `output` reflects
                    // progress live (like Codex's background terminals) instead
                    // of only appearing once the process exits. `kill_on_drop`
                    // ensures `stop` (which aborts this task) also kills the child.
                    let spawned = dcode_ai_common::provider_runtime::system_shell_command(&cmd_str)
                        .current_dir(&ws)
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true)
                        .spawn();
                    let mut child = match spawned {
                        Ok(c) => c,
                        Err(e) => {
                            if let Ok(mut buf) = out_clone.lock() {
                                buf.push_str(&format!("error: {e}"));
                            }
                            done_clone.store(true, Ordering::SeqCst);
                            return;
                        }
                    };

                    // Reader tasks: append each line to the shared buffer as it
                    // arrives, for both stdout and stderr.
                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();
                    let out_a = out_clone.clone();
                    let t_out = tokio::spawn(async move {
                        let Some(r) = stdout else { return };
                        let mut lines = BufReader::new(r).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            if let Ok(mut buf) = out_a.lock() {
                                buf.push_str(&line);
                                buf.push('\n');
                            }
                        }
                    });
                    let out_b = out_clone.clone();
                    let t_err = tokio::spawn(async move {
                        let Some(r) = stderr else { return };
                        let mut lines = BufReader::new(r).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            if let Ok(mut buf) = out_b.lock() {
                                buf.push_str(&line);
                                buf.push('\n');
                            }
                        }
                    });

                    let _ = child.wait().await;
                    let _ = t_out.await;
                    let _ = t_err.await;
                    done_clone.store(true, Ordering::SeqCst);
                });
                let id = {
                    let Ok(mut reg) = self.registry.lock() else {
                        return Self::err(call, "registry lock poisoned");
                    };
                    if reg.next_id == 0 {
                        reg.next_id = 1;
                    }
                    let id = reg.next_id;
                    reg.next_id += 1;
                    reg.jobs.push(BgJob {
                        id,
                        cmd: cmd.to_string(),
                        output,
                        done,
                        handle,
                    });
                    id
                };
                Self::ok(call, format!("Started background job {id}: {cmd}"))
            }
            "list" => {
                let Ok(reg) = self.registry.lock() else {
                    return Self::err(call, "registry lock poisoned");
                };
                if reg.jobs.is_empty() {
                    return Self::ok(call, "No background jobs.".into());
                }
                let mut out = String::from("Background jobs:\n");
                for job in &reg.jobs {
                    let status = if job.done.load(Ordering::SeqCst) {
                        "done"
                    } else {
                        "running"
                    };
                    out.push_str(&format!("[{}] {status}  $ {}\n", job.id, job.cmd));
                }
                Self::ok(call, out)
            }
            "output" => {
                let Some(id) = call.input["id"].as_u64() else {
                    return Self::err(call, "action=output requires 'id'");
                };
                let id = id as u32;
                let Ok(reg) = self.registry.lock() else {
                    return Self::err(call, "registry lock poisoned");
                };
                match reg.jobs.iter().find(|j| j.id == id) {
                    Some(job) => {
                        let status = if job.done.load(Ordering::SeqCst) {
                            "done"
                        } else {
                            "still running"
                        };
                        let body = job.output.lock().map(|b| b.clone()).unwrap_or_default();
                        Self::ok(
                            call,
                            format!(
                                "Job {id} ({status}):\n{}",
                                if body.is_empty() {
                                    "(no output yet)".to_string()
                                } else {
                                    body
                                }
                            ),
                        )
                    }
                    None => Self::err(call, format!("no background job with id {id}")),
                }
            }
            "stop" => {
                let Some(id) = call.input["id"].as_u64() else {
                    return Self::err(call, "action=stop requires 'id'");
                };
                let id = id as u32;
                let Ok(mut reg) = self.registry.lock() else {
                    return Self::err(call, "registry lock poisoned");
                };
                match reg.jobs.iter().position(|j| j.id == id) {
                    Some(pos) => {
                        let job = reg.jobs.remove(pos);
                        job.handle.abort();
                        Self::ok(call, format!("Stopped background job {id}"))
                    }
                    None => Self::err(call, format!("no background job with id {id}")),
                }
            }
            other => Self::err(call, format!("unknown action '{other}'")),
        }
    }
}
