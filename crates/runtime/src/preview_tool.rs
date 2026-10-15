//! Agent-driven dev-server preview: start a persistent process (e.g. `npm
//! run dev`), stream its output live to the web chat UI, and auto-detect its
//! listening URL for the preview pane. One job per session — starting a new
//! one stops any existing one first (keeps the state model simple; see
//! `background_exec`'s `list` action if multiple concurrent jobs are wanted
//! later).
//!
//! Combines three existing patterns rather than inventing a fourth: the
//! non-blocking spawn/registry/stop shape of `background_exec`
//! (`crates/core/src/tools/background_exec.rs`), the bounded-buffer safety
//! of `interactive_exec` (this crate), and the `event_tx`-driven live
//! streaming of `bash_tool` (this crate) — see each for the originals this
//! mirrors.
//!
//! Unlike `execute_bash`/`run_validation`, this deliberately has **no hard
//! runtime timeout**: a dev server is meant to run indefinitely. Safety
//! instead comes from the bounded output buffer (a chatty dev server can't
//! grow memory unboundedly) and explicit `stop` — including a best-effort
//! stop when the owning session ends, wired via [`PreviewHandle`] from
//! `Supervisor::finish`.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dcode_ai_common::event::AgentEvent;
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use dcode_ai_core::tools::ToolExecutor;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Max characters retained in the running job's output buffer (older output
/// is dropped), matching `interactive_exec`'s bound.
const MAX_BUFFER: usize = 64 * 1024;
/// Characters of trailing output returned by the `status` action.
const TAIL_CHARS: usize = 4000;

struct PreviewJob {
    id: u32,
    command: String,
    output: Arc<Mutex<String>>,
    url: Arc<Mutex<Option<String>>>,
    done: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct Registry {
    job: Option<PreviewJob>,
    next_id: u32,
}

/// Lets the agent start a dev server / static-preview process, watch its
/// output, and stop it. Actions: `start`, `status`, `stop`.
pub struct PreviewTool {
    workspace_root: std::path::PathBuf,
    event_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    registry: Arc<Mutex<Registry>>,
}

/// Cloneable handle so the owning session can stop the running preview job
/// when it ends (`Supervisor::finish`), without reaching through the
/// type-erased tool registry — same shape as `InteractiveExecHandle`.
#[derive(Clone)]
pub struct PreviewHandle {
    registry: Arc<Mutex<Registry>>,
}

impl PreviewHandle {
    /// Best-effort stop of any running job. A no-op if nothing is running.
    pub fn stop(&self) {
        if let Ok(mut reg) = self.registry.lock()
            && let Some(job) = reg.job.take()
        {
            job.handle.abort();
        }
    }
}

impl PreviewTool {
    pub fn new(
        workspace_root: std::path::PathBuf,
        event_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> Self {
        Self {
            workspace_root,
            event_tx,
            registry: Arc::new(Mutex::new(Registry::default())),
        }
    }

    pub fn handle(&self) -> PreviewHandle {
        PreviewHandle {
            registry: self.registry.clone(),
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
impl ToolExecutor for PreviewTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_preview".into(),
            description: "Start a dev server or static preview for a web project (e.g. `npm run \
dev`, `vite`, `python -m http.server`) so the user can see it running live in the web chat's \
Preview pane. Non-blocking — returns immediately, runs until stopped. The listening URL is \
auto-detected from the process's own startup output when possible. Actions: `start` (spawn \
`command`, stops any previously running preview first), `status` (job state + recent output + \
detected URL), `stop` (kill the running job). Only meaningful in the web chat UI — has no effect \
in the terminal. For a one-off command whose result you need right now, use the normal shell \
tool instead."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["start", "status", "stop"]
                    },
                    "command": {
                        "type": "string",
                        "description": "Shell command to run (for action=start), e.g. `npm run dev`."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory relative to the workspace root (for action=start). Defaults to the workspace root."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let action = call.input["action"].as_str().unwrap_or("status");
        match action {
            "start" => {
                let Some(cmd) = call.input["command"]
                    .as_str()
                    .filter(|c| !c.trim().is_empty())
                else {
                    return Self::err(call, "action=start requires a non-empty 'command'");
                };
                let cwd = call.input["cwd"]
                    .as_str()
                    .filter(|c| !c.trim().is_empty())
                    .map(|c| self.workspace_root.join(c))
                    .unwrap_or_else(|| self.workspace_root.clone());

                // Starting a new preview replaces any running one — one job
                // per session keeps the UI/state model simple.
                self.handle().stop();

                let output = Arc::new(Mutex::new(String::new()));
                let url = Arc::new(Mutex::new(None));
                let done = Arc::new(AtomicBool::new(false));
                let id = {
                    let Ok(mut reg) = self.registry.lock() else {
                        return Self::err(call, "registry lock poisoned");
                    };
                    if reg.next_id == 0 {
                        reg.next_id = 1;
                    }
                    reg.next_id
                };

                let out_clone = output.clone();
                let url_clone = url.clone();
                let done_clone = done.clone();
                let cmd_str = cmd.to_string();
                let event_tx = self.event_tx.clone();

                let handle = tokio::spawn(async move {
                    let _ = event_tx
                        .send(AgentEvent::PreviewStarted {
                            job_id: id,
                            command: cmd_str.clone(),
                        })
                        .await;

                    let spawned = dcode_ai_common::provider_runtime::system_shell_command(&cmd_str)
                        .current_dir(&cwd)
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true)
                        .spawn();
                    let mut child = match spawned {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = event_tx
                                .send(AgentEvent::PreviewStopped {
                                    job_id: id,
                                    reason: format!("failed to start: {e}"),
                                })
                                .await;
                            done_clone.store(true, Ordering::SeqCst);
                            return;
                        }
                    };

                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();

                    let out_a = out_clone.clone();
                    let url_a = url_clone.clone();
                    let etx_a = event_tx.clone();
                    let t_out = tokio::spawn(async move {
                        let Some(r) = stdout else { return };
                        let mut lines = BufReader::new(r).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            append_bounded(&out_a, &line);
                            let _ = etx_a
                                .send(AgentEvent::PreviewLog {
                                    job_id: id,
                                    delta: line.clone(),
                                })
                                .await;
                            maybe_report_url(&url_a, &etx_a, id, &line).await;
                        }
                    });
                    let out_b = out_clone.clone();
                    let url_b = url_clone.clone();
                    let etx_b = event_tx.clone();
                    let t_err = tokio::spawn(async move {
                        let Some(r) = stderr else { return };
                        let mut lines = BufReader::new(r).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            append_bounded(&out_b, &line);
                            let _ = etx_b
                                .send(AgentEvent::PreviewLog {
                                    job_id: id,
                                    delta: line.clone(),
                                })
                                .await;
                            maybe_report_url(&url_b, &etx_b, id, &line).await;
                        }
                    });

                    let status = child.wait().await;
                    let _ = t_out.await;
                    let _ = t_err.await;
                    done_clone.store(true, Ordering::SeqCst);
                    let reason = match status {
                        Ok(s) if s.success() => "process exited".to_string(),
                        Ok(s) => format!("process exited: {s}"),
                        Err(e) => format!("wait failed: {e}"),
                    };
                    let _ = event_tx
                        .send(AgentEvent::PreviewStopped { job_id: id, reason })
                        .await;
                });

                {
                    let Ok(mut reg) = self.registry.lock() else {
                        return Self::err(call, "registry lock poisoned");
                    };
                    reg.next_id = id + 1;
                    reg.job = Some(PreviewJob {
                        id,
                        command: cmd.to_string(),
                        output,
                        url,
                        done,
                        handle,
                    });
                }
                Self::ok(call, format!("Started preview job {id}: {cmd}"))
            }
            "status" => {
                let Ok(reg) = self.registry.lock() else {
                    return Self::err(call, "registry lock poisoned");
                };
                match &reg.job {
                    None => Self::ok(call, "No preview running.".into()),
                    Some(job) => {
                        let status = if job.done.load(Ordering::SeqCst) {
                            "done"
                        } else {
                            "running"
                        };
                        let url = job.url.lock().ok().and_then(|u| u.clone());
                        let body = job.output.lock().map(|b| b.clone()).unwrap_or_default();
                        let tail = tail_chars(&body, TAIL_CHARS);
                        Self::ok(
                            call,
                            format!(
                                "Job {} ({status}) $ {}\nurl: {}\n\n{}",
                                job.id,
                                job.command,
                                url.unwrap_or_else(|| "(not detected yet)".to_string()),
                                if tail.is_empty() {
                                    "(no output yet)".to_string()
                                } else {
                                    tail
                                }
                            ),
                        )
                    }
                }
            }
            "stop" => {
                let stopped = {
                    let Ok(mut reg) = self.registry.lock() else {
                        return Self::err(call, "registry lock poisoned");
                    };
                    reg.job.take()
                };
                match stopped {
                    Some(job) => {
                        let id = job.id;
                        job.handle.abort();
                        let _ = self.event_tx.try_send(AgentEvent::PreviewStopped {
                            job_id: id,
                            reason: "stopped".to_string(),
                        });
                        Self::ok(call, format!("Stopped preview job {id}"))
                    }
                    None => Self::ok(call, "No preview running.".into()),
                }
            }
            other => Self::err(call, format!("unknown action '{other}'")),
        }
    }
}

fn append_bounded(buf: &Arc<Mutex<String>>, line: &str) {
    if let Ok(mut b) = buf.lock() {
        b.push_str(line);
        b.push('\n');
        if b.len() > MAX_BUFFER {
            let cut = b.len() - MAX_BUFFER;
            let mut idx = cut;
            while idx < b.len() && !b.is_char_boundary(idx) {
                idx += 1;
            }
            *b = b[idx..].to_string();
        }
    }
}

fn tail_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let cut = s.len() - max;
    let mut idx = cut;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    s[idx..].to_string()
}

async fn maybe_report_url(
    url_slot: &Arc<Mutex<Option<String>>>,
    event_tx: &tokio::sync::mpsc::Sender<AgentEvent>,
    job_id: u32,
    line: &str,
) {
    let Some(found) = detect_url(line) else {
        return;
    };
    let already_set = url_slot.lock().map(|u| u.is_some()).unwrap_or(true);
    if already_set {
        return;
    }
    if let Ok(mut slot) = url_slot.lock() {
        *slot = Some(found.clone());
    }
    let _ = event_tx
        .send(AgentEvent::PreviewUrlDetected { job_id, url: found })
        .await;
}

/// First `http(s)://localhost|127.0.0.1|0.0.0.0[:port][/path]` substring in
/// `line`, with `0.0.0.0` normalized to `127.0.0.1` (a dev server bound to
/// all interfaces is still reached locally via loopback). No `regex`
/// dependency — a small manual scan, since this is the only place in either
/// crate that would need one. Covers Vite's "Local:   http://localhost:5173/",
/// Next.js's "- Local: http://localhost:3000", and plain `http.server`-style
/// banners without any framework-specific parsing.
fn detect_url(line: &str) -> Option<String> {
    for scheme in ["http://", "https://"] {
        let mut search_from = 0;
        while let Some(rel) = line.get(search_from..).and_then(|s| s.find(scheme)) {
            let start = search_from + rel;
            let rest = &line[start..];
            let host = &rest[scheme.len()..];
            let is_local = host.starts_with("localhost")
                || host.starts_with("127.0.0.1")
                || host.starts_with("0.0.0.0");
            if is_local {
                let end = rest
                    .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | ']' | '>'))
                    .unwrap_or(rest.len());
                let url = &rest[..end];
                return Some(url.replacen("0.0.0.0", "127.0.0.1", 1));
            }
            search_from = start + scheme.len();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_vite_style_banner() {
        assert_eq!(
            detect_url("  ➜  Local:   http://localhost:5173/"),
            Some("http://localhost:5173/".to_string())
        );
    }

    #[test]
    fn detects_and_normalizes_0_0_0_0() {
        assert_eq!(
            detect_url("Server running at http://0.0.0.0:8000/"),
            Some("http://127.0.0.1:8000/".to_string())
        );
    }

    #[test]
    fn ignores_non_local_urls() {
        assert_eq!(detect_url("Docs: https://vitejs.dev"), None);
    }

    #[test]
    fn ignores_lines_with_no_url() {
        assert_eq!(detect_url("Compiled successfully!"), None);
    }

    #[test]
    fn trims_trailing_punctuation() {
        assert_eq!(
            detect_url("open http://localhost:3000 in your browser"),
            Some("http://localhost:3000".to_string())
        );
        assert_eq!(
            detect_url("(see http://localhost:3000)"),
            Some("http://localhost:3000".to_string())
        );
    }
}
