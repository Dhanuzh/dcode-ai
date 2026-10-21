//! `dcode-ai acp`: an Agent Client Protocol server over stdio, so editors
//! that speak ACP (e.g. Zed) can use dcode-ai as their coding agent.
//!
//! Newline-delimited JSON-RPC 2.0. Client → agent: `initialize`,
//! `session/new`, `session/load` (resume a saved session, replaying its
//! conversation), `session/prompt` (text, images, resources),
//! `session/cancel` (notification).
//! Agent → client: `session/update` notifications (message / thought
//! chunks, tool calls, plan) and `session/request_permission` requests for
//! tool approvals.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use dcode_ai_common::config::DcodeAiConfig;
use dcode_ai_common::event::AgentEvent;
use dcode_ai_common::tool::ToolCall;
use dcode_ai_core::approval::{ApprovalHandler, ApprovalVerdict};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::runner::{SessionRuntime, build_session_runtime};

const PROTOCOL_VERSION: u64 = 1;

/// Outgoing messages plus the agent's own pending requests to the client.
pub struct Outbox {
    tx: mpsc::UnboundedSender<Value>,
    next_id: AtomicU64,
    pending: std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>,
}

impl Outbox {
    fn new(tx: mpsc::UnboundedSender<Value>) -> Self {
        Self {
            tx,
            next_id: AtomicU64::new(1),
            pending: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn send(&self, v: Value) {
        let _ = self.tx.send(v);
    }

    fn notify(&self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn update(&self, session_id: &str, update: Value) {
        self.notify(
            "session/update",
            json!({ "sessionId": session_id, "update": update }),
        );
    }

    /// Send a request to the client and wait for its result.
    async fn request(&self, method: &str, params: Value) -> Option<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().ok()?.insert(id, tx);
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        rx.await.ok()
    }

    /// A response from the client to one of our requests.
    fn resolve(&self, id: u64, result: Value) {
        if let Some(tx) = self.pending.lock().ok().and_then(|mut p| p.remove(&id)) {
            let _ = tx.send(result);
        }
    }
}

/// ACP tool kind for a dcode-ai tool.
pub fn tool_kind(tool: &str) -> &'static str {
    match tool {
        "read_file" | "list_directory" | "git_status" | "git_diff" | "repo_map"
        | "inspect_binary" => "read",
        "write_file" | "edit_file" | "replace_match" | "apply_patch" | "create_directory"
        | "copy_path" | "rename_path" => "edit",
        "delete_path" => "delete",
        "move_path" => "move",
        "search_code" | "query_symbols" | "code_intel" | "tool_search" => "search",
        "execute_bash" | "run_background" | "run_validation" | "interactive_exec" => "execute",
        "fetch_url" | "web_search" => "fetch",
        "update_plan" => "think",
        _ => "other",
    }
}

fn tool_title(tool: &str, input: &Value) -> String {
    let ui = crate::tool_ui::metadata(tool);
    let preview = crate::tool_ui::preview_from_value(tool, input);
    if preview.is_empty() {
        ui.label
    } else {
        format!(
            "{} {}",
            ui.label,
            crate::tui::render_helpers::truncate_chars(&preview, 80)
        )
    }
}

/// `update_plan` output (`[x] a`, `[~] b`, `[ ] c`) as ACP plan entries.
pub fn plan_entries(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let (status, rest) = if let Some(r) = l.strip_prefix("[x]") {
                ("completed", r)
            } else if let Some(r) = l.strip_prefix("[~]") {
                ("in_progress", r)
            } else {
                ("pending", l.strip_prefix("[ ]")?)
            };
            Some(json!({ "content": rest.trim(), "priority": "medium", "status": status }))
        })
        .collect()
}

/// Map one agent event to an ACP `session/update` payload.
pub fn event_update(ev: &AgentEvent, tools: &mut HashMap<String, String>) -> Vec<Value> {
    match ev {
        AgentEvent::TokensStreamed { delta } => vec![json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": delta },
        })],
        AgentEvent::ThinkingDelta { delta } => vec![json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": { "type": "text", "text": delta },
        })],
        AgentEvent::ToolCallStarted {
            call_id,
            tool,
            input,
        } => {
            tools.insert(call_id.clone(), tool.clone());
            vec![json!({
                "sessionUpdate": "tool_call",
                "toolCallId": call_id,
                "title": tool_title(tool, input),
                "kind": tool_kind(tool),
                "status": "in_progress",
                "rawInput": input,
            })]
        }
        AgentEvent::ToolCallCompleted { call_id, output } => {
            let text = if output.success {
                output.output.clone()
            } else {
                output.error.clone().unwrap_or_else(|| "failed".into())
            };
            let shown: String = text.chars().take(4_000).collect();
            let mut out = vec![json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": call_id,
                "status": if output.success { "completed" } else { "failed" },
                "content": [{ "type": "content", "content": { "type": "text", "text": shown } }],
            })];
            if tools.remove(call_id).as_deref() == Some("update_plan") && output.success {
                let entries = plan_entries(&output.output);
                if !entries.is_empty() {
                    out.push(json!({ "sessionUpdate": "plan", "entries": entries }));
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// Tool approvals become `session/request_permission` requests.
struct AcpApproval {
    out: Arc<Outbox>,
    session_id: String,
}

#[async_trait::async_trait]
impl ApprovalHandler for AcpApproval {
    async fn resolve(&self, call: &ToolCall, _description: &str) -> ApprovalVerdict {
        let params = json!({
            "sessionId": self.session_id,
            "toolCall": {
                "toolCallId": call.id,
                "title": tool_title(&call.name, &call.input),
                "kind": tool_kind(&call.name),
                "status": "pending",
                "rawInput": call.input,
            },
            "options": [
                { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                { "optionId": "always", "name": "Always allow", "kind": "allow_always" },
                { "optionId": "reject", "name": "Reject", "kind": "reject_once" },
            ],
        });
        let Some(result) = self.out.request("session/request_permission", params).await else {
            return ApprovalVerdict::Denied;
        };
        match result.pointer("/outcome/optionId").and_then(Value::as_str) {
            Some("allow") => ApprovalVerdict::Approved,
            Some("always") => ApprovalVerdict::AllowPattern(
                dcode_ai_core::approval::suggest_allow_pattern(&call.name, &call.input),
            ),
            _ => ApprovalVerdict::Denied,
        }
    }
}

struct Session {
    runtime: Mutex<SessionRuntime>,
    cancel: Arc<AtomicBool>,
}

struct Server {
    config: DcodeAiConfig,
    out: Arc<Outbox>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// What the editor said it can do in `initialize`.
    client: std::sync::Mutex<ClientCaps>,
}

/// Editor capabilities from `initialize.clientCapabilities`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientCaps {
    pub read_text_file: bool,
    pub write_text_file: bool,
    pub terminal: bool,
}

impl ClientCaps {
    pub fn from_initialize(params: &Value) -> Self {
        let c = &params["clientCapabilities"];
        Self {
            read_text_file: c["fs"]["readTextFile"].as_bool().unwrap_or(false),
            write_text_file: c["fs"]["writeTextFile"].as_bool().unwrap_or(false),
            terminal: c["terminal"].as_bool().unwrap_or(false),
        }
    }
}

type RpcResult = Result<Value, (i64, String)>;

impl Server {
    async fn new_session(&self, params: &Value) -> RpcResult {
        let cwd = session_cwd(params)?;
        let session_id = dcode_ai_runtime::supervisor::session::generate_session_id();
        self.open_session(&cwd, &session_id, false).await?;
        Ok(json!({ "sessionId": session_id }))
    }

    /// `session/load`: resume a saved session in `cwd` and replay its
    /// conversation to the client before answering.
    async fn load_session(&self, params: &Value) -> RpcResult {
        let cwd = session_cwd(params)?;
        let sid = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or((-32602, "missing sessionId".to_string()))?
            .to_string();
        let session = self.open_session(&cwd, &sid, true).await?;
        let runtime = session.runtime.lock().await;
        for update in history_updates(runtime.messages()) {
            self.out.update(&sid, update);
        }
        Ok(Value::Null)
    }

    async fn open_session(
        &self,
        cwd: &std::path::Path,
        session_id: &str,
        resume: bool,
    ) -> Result<Arc<Session>, (i64, String)> {
        let session_id = session_id.to_string();
        let handler: Arc<dyn ApprovalHandler> = Arc::new(AcpApproval {
            out: Arc::clone(&self.out),
            session_id: session_id.clone(),
        });
        let mut config = dcode_ai_common::config::DcodeAiConfig::load_for_workspace(cwd)
            .unwrap_or_else(|_| self.config.clone());
        config.permissions.startup_approve_all = false;
        let built = if resume {
            crate::runner::build_resumed_session_runtime(
                config.clone(),
                cwd,
                false,
                true,
                &session_id,
                Some(handler),
            )
            .await
        } else {
            build_session_runtime(
                config.clone(),
                cwd,
                false,
                true,
                Some(session_id.clone()),
                Some(handler),
                None,
            )
            .await
        };
        let mut runtime = built.map_err(|e| (-32603, e.to_string()))?;
        // Use the editor's buffers and terminals when it offers them.
        let caps = self.client.lock().map(|c| *c).unwrap_or_default();
        let editor = EditorLink {
            out: Arc::clone(&self.out),
            session_id: session_id.clone(),
            root: runtime.workspace_root().to_path_buf(),
        };
        if caps.read_text_file {
            runtime.replace_tool(Box::new(EditorReadFile(editor.clone())));
        }
        if caps.write_text_file {
            runtime.replace_tool(Box::new(EditorWriteFile(editor.clone())));
        }
        if caps.terminal {
            runtime.replace_tool(Box::new(EditorTerminal(editor)));
        }

        // Stream this session's events to the client.
        if let Some(mut rx) = runtime.take_event_rx() {
            let out = Arc::clone(&self.out);
            let sid = session_id.clone();
            tokio::spawn(async move {
                let mut tools = HashMap::new();
                while let Some(ev) = rx.recv().await {
                    for update in event_update(&ev, &mut tools) {
                        out.update(&sid, update);
                    }
                }
            });
        }
        if let Some(spawn_rx) = runtime.take_spawn_rx() {
            dcode_ai_runtime::supervisor::spawn_subagent_consumer(
                spawn_rx,
                runtime.session_id().to_string(),
                runtime.workspace_root().to_path_buf(),
                config,
                runtime.messages().to_vec(),
                None,
            );
        }
        let cancel = runtime.cancel_handle();
        let session = Arc::new(Session {
            runtime: Mutex::new(runtime),
            cancel,
        });
        self.sessions
            .lock()
            .await
            .insert(session_id, Arc::clone(&session));
        Ok(session)
    }

    async fn prompt(&self, params: &Value) -> RpcResult {
        let sid = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or((-32602, "missing sessionId".to_string()))?;
        let session = self
            .sessions
            .lock()
            .await
            .get(sid)
            .cloned()
            .ok_or((-32602, format!("unknown session `{sid}`")))?;
        let text = prompt_text(params.get("prompt"));
        let images = prompt_images(params.get("prompt"));
        if text.trim().is_empty() && images.is_empty() {
            return Err((-32602, "empty prompt".into()));
        }
        session.cancel.store(false, Ordering::SeqCst);
        let mut runtime = session.runtime.lock().await;
        let mut attachments = Vec::new();
        for (mime, bytes) in &images {
            let att = crate::image_attach::save_image_bytes(
                runtime.workspace_root(),
                runtime.session_id(),
                mime,
                bytes,
            )
            .map_err(|e| (-32602, e))?;
            attachments.push(att);
        }
        let turn = if attachments.is_empty() {
            runtime.run_turn(&text).await
        } else {
            runtime.run_turn_with_images(&text, attachments).await
        };
        match turn {
            Ok(_) => Ok(json!({ "stopReason": "end_turn" })),
            Err(_) if session.cancel.load(Ordering::SeqCst) => {
                Ok(json!({ "stopReason": "cancelled" }))
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("turn budget exceeded") {
                    Ok(json!({ "stopReason": "max_turn_requests" }))
                } else {
                    Err((-32603, msg))
                }
            }
        }
    }

    async fn cancel(&self, params: &Value) {
        if let Some(sid) = params.get("sessionId").and_then(Value::as_str)
            && let Some(s) = self.sessions.lock().await.get(sid)
        {
            s.cancel.store(true, Ordering::SeqCst);
        }
    }

    async fn handle(self: Arc<Self>, msg: Value) {
        let method = msg.get("method").and_then(Value::as_str);
        let id = msg.get("id").cloned();
        // A response to one of our own requests.
        if method.is_none() {
            if let Some(n) = id.as_ref().and_then(Value::as_u64) {
                let result = match (msg.get("result"), msg.get("error")) {
                    (Some(r), _) => r.clone(),
                    // Client-side failure (e.g. fs/read_text_file on a
                    // missing file): callers fall back to local tools.
                    (None, Some(e)) => json!({ "__error": e }),
                    (None, None) => json!({ "outcome": { "outcome": "cancelled" } }),
                };
                self.out.resolve(n, result);
            }
            return;
        }
        let params = msg.get("params").cloned().unwrap_or_default();
        let result: RpcResult = match method.unwrap_or("") {
            "initialize" => {
                if let Ok(mut caps) = self.client.lock() {
                    *caps = ClientCaps::from_initialize(&params);
                }
                Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "agentCapabilities": {
                    "loadSession": true,
                    "promptCapabilities": { "image": true, "audio": false, "embeddedContext": true },
                },
                "authMethods": [],
                "agentInfo": { "name": "dcode-ai", "version": env!("CARGO_PKG_VERSION") },
                }))
            }
            "authenticate" => Ok(json!({})),
            "session/new" => self.new_session(&params).await,
            "session/load" => self.load_session(&params).await,
            "session/prompt" => self.prompt(&params).await,
            "session/cancel" => {
                self.cancel(&params).await;
                return;
            }
            other => Err((-32601, format!("method not found: {other}"))),
        };
        let Some(id) = id else {
            return; // notification
        };
        self.out.send(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        });
    }
}

/// How editor-backed tools reach the editor for one session.
#[derive(Clone)]
struct EditorLink {
    out: Arc<Outbox>,
    session_id: String,
    root: PathBuf,
}

impl EditorLink {
    /// Absolute path inside the workspace (or an `/add-dir` folder).
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let p = self.root.join(path);
        let parent = p.parent().unwrap_or(&p);
        let canonical_parent = dcode_ai_common::config::canonicalize_simplified(parent)
            .map_err(|e| format!("{}: {e}", parent.display()))?;
        let root = dcode_ai_common::config::canonicalize_simplified(&self.root)
            .unwrap_or_else(|_| self.root.clone());
        if !dcode_ai_common::workspace_scope::allows(&root, &canonical_parent) {
            return Err("Path is outside the workspace".into());
        }
        Ok(canonical_parent.join(p.file_name().unwrap_or_default()))
    }

    /// Request to the editor; `None` when it failed (callers fall back).
    async fn call(&self, method: &str, mut params: Value) -> Option<Value> {
        params["sessionId"] = json!(self.session_id);
        let result = self.out.request(method, params).await?;
        (result.get("__error").is_none()).then_some(result)
    }
}

fn tool_ok(call: &ToolCall, output: String) -> dcode_ai_common::tool::ToolResult {
    dcode_ai_common::tool::ToolResult {
        call_id: call.id.clone(),
        success: true,
        output,
        error: None,
    }
}

/// `read_file` through `fs/read_text_file`, so unsaved editor changes are
/// seen. Falls back to the disk.
struct EditorReadFile(EditorLink);

#[async_trait::async_trait]
impl dcode_ai_core::tools::ToolExecutor for EditorReadFile {
    fn definition(&self) -> dcode_ai_common::tool::ToolDefinition {
        dcode_ai_core::tools::filesystem::ReadFileTool::new(self.0.root.clone()).definition()
    }

    async fn execute(&self, call: &ToolCall) -> dcode_ai_common::tool::ToolResult {
        let local = dcode_ai_core::tools::filesystem::ReadFileTool::new(self.0.root.clone());
        let path = call.input["path"].as_str().unwrap_or("");
        let Ok(abs) = self.0.resolve(path) else {
            return local.execute(call).await;
        };
        match self
            .0
            .call(
                "fs/read_text_file",
                json!({ "path": abs.display().to_string() }),
            )
            .await
            .and_then(|r| r["content"].as_str().map(str::to_string))
        {
            Some(content) => tool_ok(call, content),
            None => local.execute(call).await,
        }
    }
}

/// `write_file` through `fs/write_text_file`, so the editor sees the change
/// (and can show or undo it). Falls back to the disk.
struct EditorWriteFile(EditorLink);

#[async_trait::async_trait]
impl dcode_ai_core::tools::ToolExecutor for EditorWriteFile {
    fn definition(&self) -> dcode_ai_common::tool::ToolDefinition {
        dcode_ai_core::tools::write_file::WriteFileTool::new(self.0.root.clone()).definition()
    }

    async fn execute(&self, call: &ToolCall) -> dcode_ai_common::tool::ToolResult {
        let local = dcode_ai_core::tools::write_file::WriteFileTool::new(self.0.root.clone());
        let path = call.input["path"].as_str().unwrap_or("");
        let content = call.input["content"].as_str().unwrap_or("");
        let Ok(abs) = self.0.resolve(path) else {
            return local.execute(call).await;
        };
        match self
            .0
            .call(
                "fs/write_text_file",
                json!({ "path": abs.display().to_string(), "content": content }),
            )
            .await
        {
            Some(_) => tool_ok(
                call,
                format!("Wrote {} ({} lines)", path, content.lines().count()),
            ),
            None => local.execute(call).await,
        }
    }
}

/// `execute_bash` in an editor terminal (`terminal/*`), shown live in the
/// editor's tool-call view. Falls back to the built-in shell.
struct EditorTerminal(EditorLink);

#[async_trait::async_trait]
impl dcode_ai_core::tools::ToolExecutor for EditorTerminal {
    fn definition(&self) -> dcode_ai_common::tool::ToolDefinition {
        dcode_ai_common::tool::ToolDefinition {
            name: "execute_bash".into(),
            description: "Run a non-interactive shell command in the workspace (in the \
editor's terminal). Use write_file / edit_file to change files, not shell redirection."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Command to run" },
                    "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 120)" }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> dcode_ai_common::tool::ToolResult {
        let command = call.input["command"].as_str().unwrap_or("").to_string();
        let timeout = call.input["timeout_secs"].as_u64().unwrap_or(120);
        let fail = |e: String| dcode_ai_common::tool::ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(e),
        };
        let (shell, flag) = if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };
        let Some(created) = self
            .0
            .call(
                "terminal/create",
                json!({
                    "command": shell,
                    "args": [flag, command],
                    "cwd": self.0.root.display().to_string(),
                    "outputByteLimit": 1_000_000,
                }),
            )
            .await
        else {
            return fail("the editor could not start a terminal".into());
        };
        let Some(tid) = created["terminalId"].as_str().map(str::to_string) else {
            return fail("the editor returned no terminal id".into());
        };
        // Show the live terminal in the tool call.
        self.0.out.update(
            &self.0.session_id,
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": call.id,
                "content": [{ "type": "terminal", "terminalId": tid }],
            }),
        );
        let exit = tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            self.0
                .call("terminal/wait_for_exit", json!({ "terminalId": tid })),
        )
        .await;
        if exit.is_err() {
            let _ = self
                .0
                .call("terminal/kill", json!({ "terminalId": tid }))
                .await;
        }
        let output = self
            .0
            .call("terminal/output", json!({ "terminalId": tid }))
            .await
            .unwrap_or_default();
        let _ = self
            .0
            .call("terminal/release", json!({ "terminalId": tid }))
            .await;
        let text = output["output"].as_str().unwrap_or("").to_string();
        let code = match &exit {
            Ok(Some(v)) => v["exitCode"].as_i64(),
            _ => output["exitStatus"]["exitCode"].as_i64(),
        };
        match (exit.is_err(), code) {
            (true, _) => fail(format!("Command timed out after {timeout}s\n{text}")),
            (false, Some(0)) | (false, None) => tool_ok(
                call,
                if text.is_empty() {
                    "Command exited with status 0".into()
                } else {
                    text
                },
            ),
            (false, Some(c)) => dcode_ai_common::tool::ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: format!("{text}\nCommand exited with status {c}"),
                error: None,
            },
        }
    }
}

fn session_cwd(params: &Value) -> Result<PathBuf, (i64, String)> {
    params
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .ok_or((-32602, "`cwd` must be an existing directory".to_string()))
}

/// A saved conversation as `session/update` messages (user and assistant
/// text; tool traffic and system prompts are left out).
pub fn history_updates(messages: &[dcode_ai_common::message::Message]) -> Vec<Value> {
    use dcode_ai_common::message::Role;
    messages
        .iter()
        .filter_map(|m| {
            let kind = match m.role {
                Role::User if m.tool_call_id.is_none() => "user_message_chunk",
                Role::Assistant => "agent_message_chunk",
                _ => return None,
            };
            let text = m.content.event_preview();
            (!text.trim().is_empty()).then(
                || json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } }),
            )
        })
        .collect()
}

/// Image blocks of an ACP prompt as `(mime type, bytes)`.
pub fn prompt_images(prompt: Option<&Value>) -> Vec<(String, Vec<u8>)> {
    use base64::Engine;
    prompt
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("image"))
        .filter_map(|b| {
            let mime = b.get("mimeType").and_then(Value::as_str)?.to_string();
            let data = b.get("data").and_then(Value::as_str)?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.trim())
                .ok()?;
            Some((mime, bytes))
        })
        .collect()
}

/// Text of an ACP prompt: text blocks, plus embedded resources inlined and
/// resource links as `@path` mentions.
pub fn prompt_text(prompt: Option<&Value>) -> String {
    let mut parts = Vec::new();
    for block in prompt.and_then(Value::as_array).into_iter().flatten() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    parts.push(t.to_string());
                }
            }
            Some("resource_link") => {
                if let Some(uri) = block.get("uri").and_then(Value::as_str) {
                    parts.push(format!("@{}", uri.trim_start_matches("file://")));
                }
            }
            Some("resource") => {
                let r = block.get("resource").cloned().unwrap_or_default();
                let uri = r.get("uri").and_then(Value::as_str).unwrap_or("resource");
                if let Some(t) = r.get("text").and_then(Value::as_str) {
                    parts.push(format!("<context uri=\"{uri}\">\n{t}\n</context>"));
                }
            }
            _ => {}
        }
    }
    parts.join("\n\n")
}

pub async fn run(config: DcodeAiConfig) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(v) = rx.recv().await {
            let line = format!("{v}\n");
            if stdout.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    });
    let server = Arc::new(Server {
        config,
        out: Arc::new(Outbox::new(tx)),
        sessions: Mutex::new(HashMap::new()),
        client: std::sync::Mutex::new(ClientCaps::default()),
    });
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            // Each message runs concurrently: a prompt must not block the
            // permission responses and cancels that arrive meanwhile.
            Ok(msg) => {
                tokio::spawn(Arc::clone(&server).handle(msg));
            }
            Err(e) => server.out.send(json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": format!("parse error: {e}") }
            })),
        }
    }
    drop(server);
    let _ = writer.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcode_ai_common::tool::ToolResult;

    #[test]
    fn events_map_to_session_updates() {
        let mut tools = HashMap::new();
        let u = event_update(
            &AgentEvent::TokensStreamed { delta: "hi".into() },
            &mut tools,
        );
        assert_eq!(u[0]["sessionUpdate"], "agent_message_chunk");
        let started = event_update(
            &AgentEvent::ToolCallStarted {
                call_id: "c1".into(),
                tool: "update_plan".into(),
                input: json!({}),
            },
            &mut tools,
        );
        assert_eq!(started[0]["kind"], "think");
        let done = event_update(
            &AgentEvent::ToolCallCompleted {
                call_id: "c1".into(),
                output: ToolResult {
                    call_id: "c1".into(),
                    success: true,
                    output: "[x] read\n[~] fix\n[ ] test".into(),
                    error: None,
                },
            },
            &mut tools,
        );
        assert_eq!(done[0]["status"], "completed");
        assert_eq!(done[1]["sessionUpdate"], "plan");
        assert_eq!(done[1]["entries"][1]["status"], "in_progress");
    }

    #[test]
    fn image_blocks_decode_and_history_replays() {
        use dcode_ai_common::message::{Message, MessageContent, Role};
        let p = json!([
            { "type": "text", "text": "what is this?" },
            { "type": "image", "mimeType": "image/png", "data": "iVBORw0KGgo=" },
        ]);
        let imgs = prompt_images(Some(&p));
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].0, "image/png");
        assert_eq!(&imgs[0].1[..4], b"\x89PNG");

        let mut user = Message::user_with_parts(vec![]);
        user.content = MessageContent::text("fix it");
        let mut asst = user.clone();
        asst.role = Role::Assistant;
        asst.content = MessageContent::text("done");
        let mut sys = user.clone();
        sys.role = Role::System;
        let u = history_updates(&[sys, user, asst]);
        assert_eq!(u.len(), 2);
        assert_eq!(u[0]["sessionUpdate"], "user_message_chunk");
        assert_eq!(u[1]["content"]["text"], "done");
    }

    #[test]
    fn client_capabilities_are_read_from_initialize() {
        let caps = ClientCaps::from_initialize(&json!({"clientCapabilities": {
            "fs": {"readTextFile": true, "writeTextFile": false}, "terminal": true}}));
        assert_eq!(
            caps,
            ClientCaps {
                read_text_file: true,
                write_text_file: false,
                terminal: true
            }
        );
        assert_eq!(
            ClientCaps::from_initialize(&json!({})),
            ClientCaps::default()
        );
    }

    #[tokio::test]
    async fn read_file_goes_through_the_editor_and_falls_back() {
        use dcode_ai_core::tools::ToolExecutor;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "on disk").unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let out = Arc::new(Outbox::new(tx));
        let link = EditorLink {
            out: Arc::clone(&out),
            session_id: "s1".into(),
            root: dir.path().to_path_buf(),
        };
        // Fake editor: first read returns the unsaved buffer, second errors.
        let editor = Arc::clone(&out);
        tokio::spawn(async move {
            let mut n = 0;
            while let Some(req) = rx.recv().await {
                let id = req["id"].as_u64().unwrap();
                assert_eq!(req["method"], "fs/read_text_file");
                assert_eq!(req["params"]["sessionId"], "s1");
                n += 1;
                let reply = if n == 1 {
                    json!({"content": "unsaved buffer"})
                } else {
                    json!({"__error": {"code": -32000}})
                };
                editor.resolve(id, reply);
            }
        });
        let tool = EditorReadFile(link);
        let call = ToolCall {
            id: "c".into(),
            name: "read_file".into(),
            input: json!({"path": "a.txt"}),
        };
        assert_eq!(tool.execute(&call).await.output, "unsaved buffer");
        assert!(
            tool.execute(&call).await.output.contains("on disk"),
            "falls back to disk"
        );
    }

    #[test]
    fn prompt_blocks_become_text() {
        let p = json!([
            { "type": "text", "text": "fix this" },
            { "type": "resource_link", "uri": "file:///repo/src/a.rs", "name": "a.rs" },
            { "type": "resource", "resource": { "uri": "file:///repo/b.rs", "text": "fn b() {}" } },
        ]);
        let t = prompt_text(Some(&p));
        assert!(t.starts_with("fix this"));
        assert!(t.contains("@/repo/src/a.rs"));
        assert!(t.contains("<context uri=\"file:///repo/b.rs\">\nfn b() {}"));
        assert_eq!(tool_kind("execute_bash"), "execute");
        assert_eq!(tool_kind("edit_file"), "edit");
    }
}
