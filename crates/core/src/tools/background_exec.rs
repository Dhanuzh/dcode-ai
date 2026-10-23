use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use dcode_ai_common::event::{AgentEvent, BackgroundJobStatus};
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;

/// Lines of output kept in the end-of-job notification.
const TAIL_LINES: usize = 20;
/// Characters of output kept in the end-of-job notification.
const TAIL_CHARS: usize = 2_000;

/// One detached shell job tracked by the background-exec registry.
struct BgJob {
    id: u32,
    cmd: String,
    output: Arc<Mutex<String>>,
    status: Arc<Mutex<(BackgroundJobStatus, Option<i32>)>>,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct BgRegistry {
    jobs: Vec<BgJob>,
    next_id: u32,
}

/// A running command's outcome: final status, exit code and output.
pub type JobRun = std::pin::Pin<
    Box<dyn std::future::Future<Output = (BackgroundJobStatus, Option<i32>, String)> + Send>,
>;

/// A snapshot of one job, for listings (`/bashes`).
#[derive(Debug, Clone, PartialEq)]
pub struct JobInfo {
    pub id: u32,
    pub command: String,
    pub status: BackgroundJobStatus,
    pub exit_code: Option<i32>,
    pub output: String,
}

/// Shared view of a session's background jobs, for the UI.
#[derive(Clone, Default)]
pub struct BackgroundJobs(Arc<Mutex<BgRegistry>>);

impl BackgroundJobs {
    pub fn list(&self) -> Vec<JobInfo> {
        let Ok(reg) = self.0.lock() else {
            return Vec::new();
        };
        reg.jobs
            .iter()
            .map(|j| {
                let (status, exit_code) = j
                    .status
                    .lock()
                    .map(|s| *s)
                    .unwrap_or((BackgroundJobStatus::Running, None));
                JobInfo {
                    id: j.id,
                    command: j.cmd.clone(),
                    status,
                    exit_code,
                    output: j.output.lock().map(|b| b.clone()).unwrap_or_default(),
                }
            })
            .collect()
    }

    /// Stop a job; `false` when there is no such job.
    pub fn stop(&self, id: u32) -> bool {
        let Ok(mut reg) = self.0.lock() else {
            return false;
        };
        let Some(pos) = reg.jobs.iter().position(|j| j.id == id) else {
            return false;
        };
        let job = reg.jobs.remove(pos);
        job.handle.abort();
        true
    }

    /// Take over a command that is already running (Ctrl+B on a foreground
    /// shell command). `run` resolves to the final status, exit code and
    /// output; start and end are reported on `events` like `start` jobs.
    pub async fn adopt(
        &self,
        command: String,
        events: Option<mpsc::Sender<AgentEvent>>,
        run: JobRun,
    ) -> u32 {
        let id = match self.0.lock() {
            Ok(mut reg) => {
                if reg.next_id == 0 {
                    reg.next_id = 1;
                }
                let id = reg.next_id;
                reg.next_id += 1;
                id
            }
            Err(_) => 0,
        };
        if let Some(tx) = &events {
            let _ = tx
                .send(AgentEvent::BackgroundJob {
                    job_id: id,
                    command: command.clone(),
                    status: BackgroundJobStatus::Running,
                    exit_code: None,
                    output_tail: String::new(),
                })
                .await;
        }
        let output = Arc::new(Mutex::new(String::new()));
        let status = Arc::new(Mutex::new((BackgroundJobStatus::Running, None)));
        let (out_c, status_c, cmd_c) = (output.clone(), status.clone(), command.clone());
        let handle = tokio::spawn(async move {
            let (final_status, code, text) = run.await;
            if let Ok(mut b) = out_c.lock() {
                *b = text.clone();
            }
            if let Ok(mut s) = status_c.lock() {
                *s = (final_status, code);
            }
            if let Some(tx) = events {
                let _ = tx
                    .send(AgentEvent::BackgroundJob {
                        job_id: id,
                        command: cmd_c,
                        status: final_status,
                        exit_code: code,
                        output_tail: output_tail(&text),
                    })
                    .await;
            }
        });
        if let Ok(mut reg) = self.0.lock() {
            reg.jobs.push(BgJob {
                id,
                cmd: command,
                output,
                status,
                handle,
            });
        }
        id
    }

    /// Stop every job (session end).
    pub fn stop_all(&self) {
        if let Ok(mut reg) = self.0.lock() {
            for job in reg.jobs.drain(..) {
                job.handle.abort();
            }
        }
    }
}

/// Last lines of `output`, bounded for notifications.
pub fn output_tail(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    let chars = tail.chars().count();
    if chars <= TAIL_CHARS {
        tail
    } else {
        tail.chars().skip(chars - TAIL_CHARS).collect()
    }
}

/// Lets the agent run shell commands in the background (non-blocking) and poll
/// or stop them later, like Claude Code's background shells. When a job ends
/// the session gets a `BackgroundJob` event, so the agent is told without
/// polling. Actions: `start`, `list`, `output`, `stop`.
pub struct BackgroundExecTool {
    workspace_root: PathBuf,
    jobs: BackgroundJobs,
    events: Option<mpsc::Sender<AgentEvent>>,
}

impl BackgroundExecTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            jobs: BackgroundJobs::default(),
            events: None,
        }
    }

    /// A tool that reports job starts and ends on the session's event channel.
    pub fn with_events(workspace_root: PathBuf, events: mpsc::Sender<AgentEvent>) -> Self {
        Self {
            events: Some(events),
            ..Self::new(workspace_root)
        }
    }

    /// The shared job list, for `/bashes`.
    pub fn handle(&self) -> BackgroundJobs {
        self.jobs.clone()
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

fn status_word(status: BackgroundJobStatus) -> &'static str {
    match status {
        BackgroundJobStatus::Running => "running",
        BackgroundJobStatus::Completed => "completed",
        BackgroundJobStatus::Failed => "failed",
        BackgroundJobStatus::Stopped => "stopped",
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
You are notified automatically when a job finishes (a <task-notification> message with \
its exit code and last output), so do not poll in a loop; carry on or end your turn. \
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
                let id = {
                    let Ok(mut reg) = self.jobs.0.lock() else {
                        return Self::err(call, "registry lock poisoned");
                    };
                    if reg.next_id == 0 {
                        reg.next_id = 1;
                    }
                    let id = reg.next_id;
                    reg.next_id += 1;
                    id
                };
                let output = Arc::new(Mutex::new(String::new()));
                let status = Arc::new(Mutex::new((BackgroundJobStatus::Running, None)));
                let out_clone = output.clone();
                let status_clone = status.clone();
                let cmd_str = cmd.to_string();
                let ws = self.workspace_root.clone();
                let events = self.events.clone();
                if let Some(tx) = &events {
                    let _ = tx
                        .send(AgentEvent::BackgroundJob {
                            job_id: id,
                            command: cmd_str.clone(),
                            status: BackgroundJobStatus::Running,
                            exit_code: None,
                            output_tail: String::new(),
                        })
                        .await;
                }
                let handle = tokio::spawn(async move {
                    // Stream stdout/stderr incrementally so `output` reflects
                    // progress live instead of only appearing once the process
                    // exits. `kill_on_drop` ensures `stop` (which aborts this
                    // task) also kills the child.
                    let mut command =
                        dcode_ai_common::provider_runtime::system_shell_command(&cmd_str);
                    dcode_ai_common::shell::prepare_agent_command(&mut command);
                    let spawned = command
                        .current_dir(&ws)
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true)
                        .spawn();
                    let (final_status, code) = match spawned {
                        Err(e) => {
                            if let Ok(mut buf) = out_clone.lock() {
                                buf.push_str(&format!("error: {e}"));
                            }
                            (BackgroundJobStatus::Failed, None)
                        }
                        Ok(mut child) => {
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
                            let exit = child.wait().await;
                            let _ = t_out.await;
                            let _ = t_err.await;
                            match exit {
                                Ok(st) if st.success() => {
                                    (BackgroundJobStatus::Completed, st.code())
                                }
                                Ok(st) => (BackgroundJobStatus::Failed, st.code()),
                                Err(_) => (BackgroundJobStatus::Failed, None),
                            }
                        }
                    };
                    if let Ok(mut s) = status_clone.lock() {
                        *s = (final_status, code);
                    }
                    if let Some(tx) = events {
                        let tail = out_clone
                            .lock()
                            .map(|b| output_tail(&b))
                            .unwrap_or_default();
                        let _ = tx
                            .send(AgentEvent::BackgroundJob {
                                job_id: id,
                                command: cmd_str,
                                status: final_status,
                                exit_code: code,
                                output_tail: tail,
                            })
                            .await;
                    }
                });
                if let Ok(mut reg) = self.jobs.0.lock() {
                    reg.jobs.push(BgJob {
                        id,
                        cmd: cmd.to_string(),
                        output,
                        status,
                        handle,
                    });
                }
                Self::ok(
                    call,
                    format!(
                        "Running in the background (job {id} · /bashes to manage): {cmd}\n\
You will get a <task-notification> when it finishes."
                    ),
                )
            }
            "list" => {
                let jobs = self.jobs.list();
                if jobs.is_empty() {
                    return Self::ok(call, "No background jobs.".into());
                }
                let mut out = String::from("Background jobs:\n");
                for job in &jobs {
                    out.push_str(&format!(
                        "[{}] {}  $ {}\n",
                        job.id,
                        status_word(job.status),
                        job.command
                    ));
                }
                Self::ok(call, out)
            }
            "output" => {
                let Some(id) = call.input["id"].as_u64() else {
                    return Self::err(call, "action=output requires 'id'");
                };
                let id = id as u32;
                match self.jobs.list().into_iter().find(|j| j.id == id) {
                    Some(job) => {
                        let status = match (job.status, job.exit_code) {
                            (BackgroundJobStatus::Running, _) => "still running".to_string(),
                            (s, Some(code)) => format!("{}, exit code {code}", status_word(s)),
                            (s, None) => status_word(s).to_string(),
                        };
                        Self::ok(
                            call,
                            format!(
                                "Job {id} ({status}):\n{}",
                                if job.output.is_empty() {
                                    "(no output yet)".to_string()
                                } else {
                                    job.output
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
                let command = self
                    .jobs
                    .list()
                    .into_iter()
                    .find(|j| j.id == id)
                    .map(|j| j.command);
                if !self.jobs.stop(id) {
                    return Self::err(call, format!("no background job with id {id}"));
                }
                if let (Some(tx), Some(command)) = (&self.events, command) {
                    let _ = tx
                        .send(AgentEvent::BackgroundJob {
                            job_id: id,
                            command,
                            status: BackgroundJobStatus::Stopped,
                            exit_code: None,
                            output_tail: String::new(),
                        })
                        .await;
                }
                Self::ok(call, format!("Stopped background job {id}"))
            }
            other => Self::err(call, format!("unknown action '{other}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: "run_background".into(),
            input,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn job_end_is_reported_with_exit_code_and_tail() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let tool = BackgroundExecTool::with_events(dir.path().to_path_buf(), tx);
        let res = tool
            .execute(&call(
                json!({"action": "start", "command": "echo hi; exit 3"}),
            ))
            .await;
        assert!(
            res.output.starts_with("Running in the background (job 1"),
            "{}",
            res.output
        );

        let started = rx.recv().await.unwrap();
        assert!(matches!(
            started,
            AgentEvent::BackgroundJob {
                status: BackgroundJobStatus::Running,
                ..
            }
        ));
        let ended = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let AgentEvent::BackgroundJob {
            job_id,
            status,
            exit_code,
            output_tail,
            ..
        } = ended
        else {
            panic!("expected job event");
        };
        assert_eq!(
            (job_id, status, exit_code, output_tail.as_str()),
            (1, BackgroundJobStatus::Failed, Some(3), "hi")
        );
        let listed = tool.handle().list();
        assert_eq!(listed[0].exit_code, Some(3));
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        let out: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let tail = output_tail(&out);
        assert!(
            tail.starts_with("line 11") && tail.ends_with("line 30"),
            "{tail}"
        );
    }
}
