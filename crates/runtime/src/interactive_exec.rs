//! Interactive, PTY-backed command execution (Codex `unified_exec` analog).
//!
//! Unlike the one-shot `execute_bash` tool, this runs a command inside a real
//! pseudo-terminal so interactive prompts work: the agent can `start` a
//! command, `read` its output (including a prompt like a `sudo` password
//! request or a `[y/N]` confirmation), `write` a line to its stdin to answer,
//! and `stop` it. Sessions persist across tool calls until they exit or are
//! stopped.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use dcode_ai_core::tools::ToolExecutor;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Max characters retained per session buffer (older output is dropped).
const MAX_BUFFER: usize = 64 * 1024;
/// Characters of trailing output returned to the model per read/write.
const TAIL_CHARS: usize = 4000;
/// How long to let output settle after start/write before snapshotting.
const SETTLE: Duration = Duration::from_millis(400);

struct PtySession {
    cmd: String,
    writer: Box<dyn Write + Send>,
    buffer: Arc<Mutex<String>>,
    done: Arc<AtomicBool>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    // Master handle must outlive the reader/writer; kept alive here.
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

#[derive(Default)]
struct Registry {
    sessions: HashMap<u32, PtySession>,
    next_id: u32,
}

pub struct InteractiveExecTool {
    workspace_root: std::path::PathBuf,
    registry: Arc<Mutex<Registry>>,
}

/// Cloneable handle to the interactive-exec sessions, so the CLI can write to a
/// running session's stdin *locally* (e.g. the user typing a password via
/// `/input`) without the input ever passing through the model/transcript.
#[derive(Clone)]
pub struct InteractiveExecHandle {
    registry: Arc<Mutex<Registry>>,
}

impl InteractiveExecHandle {
    /// Send a line to a session's stdin (a newline is appended if missing).
    /// The text is written straight to the PTY — it is never sent to the model.
    pub fn write_line(&self, id: u32, text: &str) -> Result<(), String> {
        let mut reg = self.registry.lock().map_err(|_| "registry lock poisoned")?;
        let sess = reg
            .sessions
            .get_mut(&id)
            .ok_or_else(|| format!("no interactive session with id {id}"))?;
        let data = if text.ends_with('\n') {
            text.to_string()
        } else {
            format!("{text}\n")
        };
        sess.writer
            .write_all(data.as_bytes())
            .map_err(|e| e.to_string())?;
        sess.writer.flush().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// List active sessions as `(id, running, command)`.
    pub fn sessions(&self) -> Vec<(u32, bool, String)> {
        let Ok(reg) = self.registry.lock() else {
            return Vec::new();
        };
        reg.sessions
            .iter()
            .map(|(id, s)| (*id, !s.done.load(Ordering::SeqCst), s.cmd.clone()))
            .collect()
    }
}

impl InteractiveExecTool {
    pub fn new(workspace_root: std::path::PathBuf) -> Self {
        Self {
            workspace_root,
            registry: Arc::new(Mutex::new(Registry {
                sessions: HashMap::new(),
                next_id: 1,
            })),
        }
    }

    /// A cloneable handle for writing to sessions from outside the tool
    /// (e.g. the CLI's `/input` command).
    pub fn handle(&self) -> InteractiveExecHandle {
        InteractiveExecHandle {
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

    /// A human-readable snapshot of a session: status + trailing output.
    fn snapshot(&self, id: u32) -> String {
        let Ok(reg) = self.registry.lock() else {
            return "[session registry unavailable]".into();
        };
        match reg.sessions.get(&id) {
            Some(s) => {
                let status = if s.done.load(Ordering::SeqCst) {
                    "exited"
                } else {
                    "running"
                };
                let body = s.buffer.lock().map(|b| b.clone()).unwrap_or_default();
                // Last TAIL_CHARS characters, on a char boundary.
                let tail: String = body
                    .chars()
                    .rev()
                    .take(TAIL_CHARS)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("[session {id} · {status}]\n{tail}")
            }
            None => format!("[session {id} not found]"),
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for InteractiveExecTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "interactive_exec".into(),
            description:
                "Run a shell command in a real PTY so you can answer interactive prompts. \
Use for commands that ask for input (a `sudo` password, a `[y/N]` confirmation, an installer \
wizard). Actions: `start` (spawn `command`, returns a session id + initial output), `read` \
(get the latest output, needs `id`), `write` (send `input` + newline to the command's stdin to \
answer a prompt, needs `id`), `stop` (kill a session, needs `id`). For non-interactive commands, \
use the normal execute_bash tool."
                    .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["start", "read", "write", "stop"] },
                    "command": { "type": "string", "description": "Shell command (action=start)." },
                    "id": { "type": "integer", "description": "Session id (read/write/stop)." },
                    "input": { "type": "string", "description": "Line to send to stdin (action=write)." }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let action = call.input["action"].as_str().unwrap_or("");
        match action {
            "start" => {
                let Some(command) = call.input["command"]
                    .as_str()
                    .filter(|c| !c.trim().is_empty())
                else {
                    return Self::err(call, "action=start requires a non-empty 'command'");
                };

                let pair = match native_pty_system().openpty(PtySize {
                    rows: 30,
                    cols: 120,
                    pixel_width: 0,
                    pixel_height: 0,
                }) {
                    Ok(p) => p,
                    Err(e) => return Self::err(call, format!("openpty failed: {e}")),
                };

                // ConPTY on Windows has no `sh`; run through cmd.
                let mut builder = if cfg!(windows) {
                    let mut b = CommandBuilder::new("cmd");
                    b.arg("/C");
                    b
                } else {
                    let mut b = CommandBuilder::new("sh");
                    b.arg("-lc");
                    b
                };
                builder.arg(command);
                builder.cwd(self.workspace_root.as_os_str());

                let child = match pair.slave.spawn_command(builder) {
                    Ok(c) => c,
                    Err(e) => return Self::err(call, format!("spawn failed: {e}")),
                };
                // Drop the slave so the reader sees EOF when the child exits.
                drop(pair.slave);

                let reader = match pair.master.try_clone_reader() {
                    Ok(r) => r,
                    Err(e) => return Self::err(call, format!("reader failed: {e}")),
                };
                let writer = match pair.master.take_writer() {
                    Ok(w) => w,
                    Err(e) => return Self::err(call, format!("writer failed: {e}")),
                };

                let buffer = Arc::new(Mutex::new(String::new()));
                let done = Arc::new(AtomicBool::new(false));
                let buf_r = buffer.clone();
                let done_r = done.clone();
                // Blocking reader thread: append PTY output to the buffer.
                std::thread::spawn(move || {
                    let mut reader = reader;
                    let mut chunk = [0u8; 4096];
                    loop {
                        match reader.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if let Ok(mut b) = buf_r.lock() {
                                    b.push_str(&String::from_utf8_lossy(&chunk[..n]));
                                    if b.len() > MAX_BUFFER {
                                        let cut = b.len() - MAX_BUFFER;
                                        // Trim to a char boundary.
                                        let mut idx = cut;
                                        while idx < b.len() && !b.is_char_boundary(idx) {
                                            idx += 1;
                                        }
                                        *b = b[idx..].to_string();
                                    }
                                }
                            }
                        }
                    }
                    done_r.store(true, Ordering::SeqCst);
                });

                let id = {
                    let mut reg = match self.registry.lock() {
                        Ok(r) => r,
                        Err(_) => return Self::err(call, "registry lock poisoned"),
                    };
                    if reg.next_id == 0 {
                        reg.next_id = 1;
                    }
                    let id = reg.next_id;
                    reg.next_id += 1;
                    reg.sessions.insert(
                        id,
                        PtySession {
                            cmd: command.to_string(),
                            writer,
                            buffer,
                            done,
                            child,
                            _master: pair.master,
                        },
                    );
                    id
                };

                tokio::time::sleep(SETTLE).await;
                Self::ok(
                    call,
                    format!(
                        "Started interactive session {id}: {command}\n{}\n\n\
⌨  If this is waiting for input (a password, a [y/N], …), the user can answer it \
securely by typing:  /input {id} <text>   (sent straight to the command, never to the AI).",
                        self.snapshot(id)
                    ),
                )
            }
            "write" => {
                let Some(id) = call.input["id"].as_u64().map(|v| v as u32) else {
                    return Self::err(call, "action=write requires 'id'");
                };
                let input = call.input["input"].as_str().unwrap_or("");
                {
                    let mut reg = match self.registry.lock() {
                        Ok(r) => r,
                        Err(_) => return Self::err(call, "registry lock poisoned"),
                    };
                    let Some(sess) = reg.sessions.get_mut(&id) else {
                        return Self::err(call, format!("no interactive session with id {id}"));
                    };
                    let data = if input.ends_with('\n') {
                        input.to_string()
                    } else {
                        format!("{input}\n")
                    };
                    if let Err(e) = sess.writer.write_all(data.as_bytes()) {
                        return Self::err(call, format!("write failed: {e}"));
                    }
                    let _ = sess.writer.flush();
                }
                tokio::time::sleep(SETTLE).await;
                Self::ok(call, self.snapshot(id))
            }
            "read" => {
                let Some(id) = call.input["id"].as_u64().map(|v| v as u32) else {
                    return Self::err(call, "action=read requires 'id'");
                };
                Self::ok(call, self.snapshot(id))
            }
            "stop" => {
                let Some(id) = call.input["id"].as_u64().map(|v| v as u32) else {
                    return Self::err(call, "action=stop requires 'id'");
                };
                let mut reg = match self.registry.lock() {
                    Ok(r) => r,
                    Err(_) => return Self::err(call, "registry lock poisoned"),
                };
                match reg.sessions.remove(&id) {
                    Some(mut sess) => {
                        let _ = sess.child.kill();
                        Self::ok(
                            call,
                            format!("Stopped interactive session {id} ({})", sess.cmd),
                        )
                    }
                    None => Self::err(call, format!("no interactive session with id {id}")),
                }
            }
            other => Self::err(call, format!("unknown action '{other}'")),
        }
    }
}
