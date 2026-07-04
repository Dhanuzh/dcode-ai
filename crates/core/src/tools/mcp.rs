use crate::tools::ToolExecutor;
use dcode_ai_common::config::McpServerConfig;
use dcode_ai_common::event::{AgentEvent, McpStartupStatus};
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, mpsc};

pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(300);

/// Aggregates and manages long-lived MCP server connections.
pub struct McpConnectionManager {
    workspace_root: PathBuf,
    clients: Mutex<HashMap<String, Arc<AsyncManagedClient>>>,
    event_tx: Option<mpsc::UnboundedSender<AgentEvent>>,
}

impl McpConnectionManager {
    pub fn new(
        workspace_root: PathBuf,
        event_tx: Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> Self {
        Self {
            workspace_root,
            clients: Mutex::new(HashMap::new()),
            event_tx,
        }
    }

    pub async fn get_or_connect(
        self: &Arc<Self>,
        server_config: &McpServerConfig,
    ) -> Result<Arc<AsyncManagedClient>, String> {
        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.get(&server_config.name) {
            return Ok(Arc::clone(client));
        }

        let client = Arc::new(AsyncManagedClient::new(
            server_config.clone(),
            self.workspace_root.clone(),
            self.event_tx.clone(),
        ));
        clients.insert(server_config.name.clone(), Arc::clone(&client));
        Ok(client)
    }

    pub async fn shutdown_all(&self) {
        let mut clients = self.clients.lock().await;
        for (_, client) in clients.drain() {
            client.shutdown().await;
        }
    }
}

/// A handle to a potentially-connecting MCP client.
/// Replicates the codex `AsyncManagedClient` pattern using a shared initialization future.
pub struct AsyncManagedClient {
    #[allow(clippy::type_complexity)]
    init_future: Shared<BoxFuture<'static, Result<Arc<Mutex<Box<dyn McpTransport>>>, String>>>,
    shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl AsyncManagedClient {
    pub fn new(
        config: McpServerConfig,
        workspace_root: PathBuf,
        event_tx: Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> Self {
        let config_clone = config.clone();
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

        let init_task = async move {
            let server_name = config_clone.name.clone();
            if let Some(ref tx) = event_tx {
                let _ = tx.send(AgentEvent::McpStartupUpdate {
                    server: server_name.clone(),
                    status: McpStartupStatus::Starting,
                });
            }

            let connect_res = async {
                let mut transport: Box<dyn McpTransport> = match &config_clone.url {
                    Some(url) if !url.trim().is_empty() => {
                        Box::new(HttpMcpClient::connect(url, &config_clone)?)
                    }
                    _ => Box::new(StdioMcpClient::spawn(&workspace_root, &config_clone)?),
                };

                if let Some(ref tx) = event_tx {
                    let _ = tx.send(AgentEvent::McpStartupUpdate {
                        server: server_name.clone(),
                        status: McpStartupStatus::Initializing,
                    });
                }

                transport.initialize().await?;

                if let Some(ref tx) = event_tx {
                    let _ = tx.send(AgentEvent::McpStartupUpdate {
                        server: server_name.clone(),
                        status: McpStartupStatus::Ready,
                    });
                }

                Ok(Arc::new(Mutex::new(transport)))
            };

            tokio::select! {
                res = connect_res => res,
                _ = &mut shutdown_rx => Err(format!("MCP server `{}` shutdown before initialization completed", server_name)),
                _ = tokio::time::sleep(DEFAULT_STARTUP_TIMEOUT) => {
                    let err = format!("MCP server `{}` timed out during startup", server_name);
                    if let Some(ref tx) = event_tx {
                        let _ = tx.send(AgentEvent::McpStartupUpdate {
                            server: server_name.clone(),
                            status: McpStartupStatus::Failed { message: err.clone() },
                        });
                    }
                    Err(err)
                }
            }
        }
        .boxed()
        .shared();

        Self {
            init_future: init_task,
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
        }
    }

    pub async fn execute_tool(&self, tool_name: &str, input: Value) -> Result<Value, String> {
        let transport_mutex = self.init_future.clone().await?;
        let mut transport = transport_mutex.lock().await;

        tokio::select! {
            res = transport.call_tool(tool_name, input) => res,
            _ = tokio::time::sleep(DEFAULT_TOOL_TIMEOUT) => Err(format!("MCP tool `{}` timed out after {}s", tool_name, DEFAULT_TOOL_TIMEOUT.as_secs())),
        }
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolSchema>, String> {
        let transport_mutex = self.init_future.clone().await?;
        let mut transport = transport_mutex.lock().await;
        transport.list_tools().await
    }

    pub async fn shutdown(&self) {
        if let Some(tx) = self.shutdown_tx.lock().await.take() {
            let _ = tx.send(());
        }
        if let Ok(transport_mutex) = self.init_future.clone().await {
            let mut transport = transport_mutex.lock().await;
            transport.shutdown().await;
        }
    }
}

pub struct McpTool {
    manager: Arc<McpConnectionManager>,
    server_config: McpServerConfig,
    tool_name: String,
    definition: ToolDefinition,
}

impl McpTool {
    pub fn new(
        manager: Arc<McpConnectionManager>,
        server_config: McpServerConfig,
        tool_schema: McpToolSchema,
    ) -> Self {
        let prefixed_name = format!("mcp__{}__{}", server_config.name, tool_schema.name);
        let definition = ToolDefinition {
            name: prefixed_name,
            description: tool_schema.description.unwrap_or_else(|| {
                format!(
                    "MCP tool `{}` from `{}`",
                    tool_schema.name, server_config.name
                )
            }),
            parameters: serde_json::json!({
                "type": "object",
                "properties": tool_schema.input_schema.properties,
                "required": tool_schema.input_schema.required,
            }),
        };

        Self {
            manager,
            server_config,
            tool_name: tool_schema.name,
            definition,
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for McpTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let client = match self.manager.get_or_connect(&self.server_config).await {
            Ok(client) => client,
            Err(err) => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(err),
                };
            }
        };

        match client
            .execute_tool(&self.tool_name, call.input.clone())
            .await
        {
            Ok(output_val) => {
                let mcp_res: Result<McpCallToolResult, _> =
                    serde_json::from_value(output_val.clone());
                match mcp_res {
                    Ok(res) => ToolResult {
                        call_id: call.id.clone(),
                        success: !res.is_error,
                        output: res.format_output(),
                        error: None,
                    },
                    Err(_) => {
                        // Fallback for non-spec compliant servers
                        let output = if output_val.is_string() {
                            output_val.as_str().unwrap().to_string()
                        } else {
                            serde_json::to_string(&output_val).unwrap_or_default()
                        };
                        ToolResult {
                            call_id: call.id.clone(),
                            success: true,
                            output,
                            error: None,
                        }
                    }
                }
            }
            Err(err) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(err),
            },
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct McpToolSchema {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema", alias = "input_schema")]
    pub input_schema: McpInputSchema,
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct McpInputSchema {
    #[serde(default)]
    pub properties: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub required: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCallToolResult {
    pub content: Vec<McpContent>,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpContent {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Resource {
        resource: Value,
    },
}

impl McpCallToolResult {
    pub fn format_output(&self) -> String {
        let mut out = String::new();
        for item in &self.content {
            match item {
                McpContent::Text { text } => {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str(text);
                }
                McpContent::Image { mime_type, .. } => {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str(&format!("[Image: {}]", mime_type));
                }
                McpContent::Resource { .. } => {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str("[Resource]");
                }
            }
        }
        out
    }
}

#[async_trait::async_trait]
trait McpTransport: Send + Sync {
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String>;
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String>;
    async fn shutdown(&mut self);

    async fn initialize(&mut self) -> Result<(), String> {
        self.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "dcode-ai",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        )
        .await?;
        self.notify(
            "notifications/initialized",
            Value::Object(Default::default()),
        )
        .await?;
        Ok(())
    }

    async fn list_tools(&mut self) -> Result<Vec<McpToolSchema>, String> {
        let result = self.request("tools/list", serde_json::json!({})).await?;
        serde_json::from_value(
            result
                .get("tools")
                .cloned()
                .unwrap_or_else(|| Value::Array(vec![])),
        )
        .map_err(|err| format!("failed to decode MCP tool list: {err}"))
    }

    async fn call_tool(&mut self, tool_name: &str, input: Value) -> Result<Value, String> {
        self.request(
            "tools/call",
            serde_json::json!({
                "name": tool_name,
                "arguments": input,
            }),
        )
        .await
    }
}

struct StdioMcpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl StdioMcpClient {
    fn spawn(workspace_root: &Path, server: &McpServerConfig) -> Result<Self, String> {
        let mut command = Command::new(&server.command);
        command
            .args(&server.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let working_directory = server
            .cwd
            .clone()
            .unwrap_or_else(|| workspace_root.to_path_buf());
        command.current_dir(working_directory);
        for (key, value) in &server.env {
            command.env(key, value);
        }

        let mut child = command
            .spawn()
            .map_err(|err| format!("failed to start MCP server `{}`: {err}", server.name))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("missing stdin for MCP server `{}`", server.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("missing stdout for MCP server `{}`", server.name))?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }
}

#[async_trait::async_trait]
impl McpTransport for StdioMcpClient {
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let line = serde_json::to_string(&payload).map_err(|err| err.to_string())?;
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|err| err.to_string())?;
        self.stdin.flush().await.map_err(|err| err.to_string())?;

        loop {
            let mut response_line = String::new();
            self.stdout
                .read_line(&mut response_line)
                .await
                .map_err(|err| err.to_string())?;
            if response_line.trim().is_empty() {
                continue;
            }
            let response: Value =
                serde_json::from_str(&response_line).map_err(|err| err.to_string())?;
            if response.get("id") == Some(&Value::from(id)) {
                if let Some(error) = response.get("error") {
                    return Err(format!("MCP error: {}", error));
                }
                return Ok(response
                    .get("result")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default())));
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let line = serde_json::to_string(&payload).map_err(|err| err.to_string())?;
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|err| err.to_string())?;
        self.stdin.flush().await.map_err(|err| err.to_string())
    }

    async fn shutdown(&mut self) {
        let _ = self.request("shutdown", Value::Null).await;
        let _ = self.notify("exit", Value::Null).await;
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

struct HttpMcpClient {
    client: reqwest::Client,
    url: String,
    next_id: u64,
    session_id: Option<String>,
    headers: Vec<(String, String)>,
}

impl HttpMcpClient {
    fn connect(url: &str, server: &McpServerConfig) -> Result<Self, String> {
        let client = reqwest::Client::builder().build().map_err(|err| {
            format!(
                "failed to build HTTP client for MCP `{}`: {err}",
                server.name
            )
        })?;
        let headers = server
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), expand_env_vars(v)))
            .collect();
        Ok(Self {
            client,
            url: url.to_string(),
            next_id: 1,
            session_id: None,
            headers,
        })
    }

    async fn post(&mut self, payload: &Value) -> Result<reqwest::Response, String> {
        let mut req = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (key, value) in &self.headers {
            req = req.header(key, value);
        }
        if let Some(sid) = &self.session_id {
            req = req.header("mcp-session-id", sid.clone());
        }
        let resp = req
            .json(payload)
            .send()
            .await
            .map_err(|err| format!("MCP HTTP request failed: {err}"))?;

        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(sid.to_string());
        }

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("MCP HTTP error {status}: {body}"));
        }
        Ok(resp)
    }
}

#[async_trait::async_trait]
impl McpTransport for HttpMcpClient {
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let resp = self.post(&payload).await?;
        let body = resp
            .text()
            .await
            .map_err(|err| format!("failed to read MCP HTTP response: {err}"))?;

        // Direct JSON body.
        if let Ok(value) = serde_json::from_str::<Value>(body.trim()) {
            return self.result_from(&value);
        }

        // SSE body: scan `data:` lines for the matching response.
        for line in body.lines() {
            let Some(data) = line.trim().strip_prefix("data:") else {
                continue;
            };
            if let Ok(value) = serde_json::from_str::<Value>(data.trim())
                && value.get("id") == Some(&Value::from(id))
            {
                return self.result_from(&value);
            }
        }
        Err("MCP HTTP response contained no matching JSON-RPC result".to_string())
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.post(&payload).await.map(|_| ())
    }

    async fn shutdown(&mut self) {
        let _ = self.request("shutdown", Value::Null).await;
        let _ = self.notify("exit", Value::Null).await;
    }
}

impl HttpMcpClient {
    fn result_from(&self, value: &Value) -> Result<Value, String> {
        if let Some(error) = value.get("error") {
            return Err(format!("MCP error: {error}"));
        }
        Ok(value
            .get("result")
            .cloned()
            .unwrap_or(Value::Object(Default::default())))
    }
}

fn expand_env_vars(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find('}') {
            let var = &after[..end];
            out.push_str(&std::env::var(var).unwrap_or_default());
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

pub async fn load_mcp_tools(
    manager: &Arc<McpConnectionManager>,
    servers: &[McpServerConfig],
) -> Result<Vec<Box<dyn ToolExecutor>>, String> {
    let mut tools: Vec<Box<dyn ToolExecutor>> = Vec::new();
    for server in servers.iter().filter(|server| server.enabled) {
        let client = manager.get_or_connect(server).await?;
        let server_tools = client.list_tools().await?;
        for tool_schema in server_tools {
            tools.push(Box::new(McpTool::new(
                Arc::clone(manager),
                server.clone(),
                tool_schema,
            )));
        }
    }
    Ok(tools)
}
