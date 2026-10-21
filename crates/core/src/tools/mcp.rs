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

    /// Shut down and forget one server (its tools must be unregistered too).
    pub async fn disconnect(&self, name: &str) {
        let client = self.clients.lock().await.remove(name);
        if let Some(client) = client {
            client.shutdown().await;
        }
    }

    pub async fn shutdown_all(&self) {
        let mut clients = self.clients.lock().await;
        for (name, client) in clients.drain() {
            // A server that never finished starting, or a request still
            // holding the transport, must not block session switch or exit.
            if tokio::time::timeout(std::time::Duration::from_secs(3), client.shutdown())
                .await
                .is_err()
            {
                tracing::warn!("MCP server `{name}` did not shut down in 3s; dropping it");
            }
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
            .stderr(std::process::Stdio::null())
            // Never leave a server running after its client is gone.
            .kill_on_drop(true);

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

    /// MCP stdio shutdown: close the server's stdin, give it a moment to
    /// exit, then kill it. (MCP has no `shutdown` request; servers that
    /// ignore unknown methods would never answer one.)
    async fn shutdown(&mut self) {
        let _ = self.stdin.shutdown().await;
        let exited = tokio::time::timeout(std::time::Duration::from_millis(500), self.child.wait())
            .await
            .is_ok();
        if !exited {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
    }
}

struct HttpMcpClient {
    client: reqwest::Client,
    url: String,
    next_id: u64,
    session_id: Option<String>,
    headers: Vec<(String, String)>,
    server_name: String,
    /// OAuth sign-in from `dcode-ai mcp login`, used when no Authorization
    /// header is configured.
    oauth: Option<dcode_ai_common::mcp_auth::McpServerLogin>,
}

impl HttpMcpClient {
    fn connect(url: &str, server: &McpServerConfig) -> Result<Self, String> {
        let client = reqwest::Client::builder().build().map_err(|err| {
            format!(
                "failed to build HTTP client for MCP `{}`: {err}",
                server.name
            )
        })?;
        let headers = resolve_headers(server)?;
        let has_auth = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"));
        let oauth = if has_auth {
            None
        } else {
            dcode_ai_common::mcp_auth::McpAuthStore::load()
                .login_for(url)
                .cloned()
        };
        Ok(Self {
            client,
            url: url.to_string(),
            next_id: 1,
            session_id: None,
            headers,
            server_name: server.name.clone(),
            oauth,
        })
    }

    /// Refresh the stored OAuth tokens and save them. `false` = no login or
    /// refresh failed.
    async fn refresh_login(&mut self) -> bool {
        let Some(login) = self.oauth.clone() else {
            return false;
        };
        match crate::mcp_oauth::refresh(&self.client, &login, &self.url).await {
            Ok(t) => {
                let mut updated = login;
                updated.access_token = t.access_token;
                updated.refresh_token = t.refresh_token;
                updated.expires_at = t.expires_at;
                let mut store = dcode_ai_common::mcp_auth::McpAuthStore::load();
                store
                    .servers
                    .insert(dcode_ai_common::mcp_auth::key(&self.url), updated.clone());
                let _ = store.save();
                self.oauth = Some(updated);
                true
            }
            Err(e) => {
                tracing::warn!("MCP `{}`: token refresh failed: {e}", self.server_name);
                false
            }
        }
    }

    fn sign_in_error(&self) -> String {
        format!(
            "MCP server `{0}` needs sign-in: run `dcode-ai mcp login {0}`",
            self.server_name
        )
    }

    async fn post(&mut self, payload: &Value) -> Result<reqwest::Response, String> {
        if self
            .oauth
            .as_ref()
            .is_some_and(|l| l.is_expired(chrono::Utc::now().timestamp()))
        {
            self.refresh_login().await;
        }
        let mut retried = false;
        let resp = loop {
            let mut req = self
                .client
                .post(&self.url)
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream");
            for (key, value) in &self.headers {
                req = req.header(key, value);
            }
            if let Some(login) = &self.oauth {
                req = req.bearer_auth(&login.access_token);
            }
            if let Some(sid) = &self.session_id {
                req = req.header("mcp-session-id", sid.clone());
            }
            let resp = req
                .json(payload)
                .send()
                .await
                .map_err(|err| format!("MCP HTTP request failed: {err}"))?;
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                let oauth_server = resp
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .and_then(crate::mcp_oauth::parse_www_authenticate)
                    .is_some();
                if !retried && self.oauth.is_some() && self.refresh_login().await {
                    retried = true;
                    continue;
                }
                if oauth_server || self.oauth.is_some() {
                    return Err(self.sign_in_error());
                }
            }
            break resp;
        };

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

    /// Nothing to tear down for streamable HTTP: the server owns its
    /// lifetime, and MCP defines no `shutdown` request.
    async fn shutdown(&mut self) {}
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

/// Expand `${VAR}`s in a server's HTTP headers. An unset variable is an
/// error naming the server, header and variable: sending e.g.
/// `Authorization: Basic ` would only come back as an unexplained 401.
fn resolve_headers(server: &McpServerConfig) -> Result<Vec<(String, String)>, String> {
    let no_extra = std::collections::BTreeMap::new();
    server
        .headers
        .iter()
        .map(|(key, value)| {
            dcode_ai_common::mcp_json::expand_vars_strict(value, &no_extra)
                .map(|v| (key.clone(), v))
                .map_err(|var| {
                    format!(
                        "MCP server `{}`: header `{key}` uses ${{{var}}}, which is not set; \
                         export {var} before starting dcode-ai",
                        server.name
                    )
                })
        })
        .collect()
}

/// Like [`load_mcp_tools`] but one failing server does not hide the others:
/// returns every tool that loaded plus `(server, error)` for each failure.
pub async fn load_mcp_tools_tolerant(
    manager: &Arc<McpConnectionManager>,
    servers: &[McpServerConfig],
) -> (Vec<Box<dyn ToolExecutor>>, Vec<(String, String)>) {
    let mut tools = Vec::new();
    let mut errors = Vec::new();
    for server in servers.iter().filter(|s| s.enabled) {
        match load_mcp_tools(manager, std::slice::from_ref(server)).await {
            Ok(t) => tools.extend(t),
            Err(e) => errors.push((server.name.clone(), e)),
        }
    }
    (tools, errors)
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

#[cfg(test)]
mod tests {
    use super::*;

    // Never set anywhere; tests must not call `set_var` (edition 2024).
    const UNSET: &str = "DCODE_AI_TEST_UNSET_MCP_HEADER_4C1E";

    fn http_server(auth: &str) -> McpServerConfig {
        let doc = serde_json::json!({ "mcpServers": { "atl": {
            "type": "http",
            "url": "https://mcp.example.com/mcp",
            "headers": { "Authorization": auth },
        }}});
        // Same path as a real `.mcp.json`: parse first, then connect.
        dcode_ai_common::mcp_json::servers_from_value(&doc, "user", &Default::default())
            .expect("parse")
            .remove(0)
    }

    #[test]
    fn unset_header_variable_fails_connect_with_a_clear_error() {
        let server = http_server(&format!("Basic ${{{UNSET}}}"));
        let err = match HttpMcpClient::connect(server.url.as_deref().unwrap(), &server) {
            Ok(_) => panic!("connect must fail when a header variable is unset"),
            Err(e) => e,
        };
        assert!(err.contains("`atl`"), "{err}");
        assert!(err.contains("`Authorization`"), "{err}");
        assert!(err.contains(UNSET), "{err}");
    }

    #[test]
    fn resolved_and_defaulted_headers_connect() {
        let server = http_server(&format!("Bearer ${{{UNSET}:-fallback}}"));
        let client = HttpMcpClient::connect(server.url.as_deref().unwrap(), &server)
            .expect("a default makes the header resolvable");
        assert_eq!(
            client.headers,
            vec![("Authorization".to_string(), "Bearer fallback".to_string())]
        );
    }

    #[tokio::test]
    async fn oauth_challenge_without_login_says_how_to_sign_in() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nwww-authenticate: Bearer resource_metadata=\"https://x/.well-known/oauth-protected-resource\"\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
        });
        let doc = serde_json::json!({ "mcpServers": { "remote": {
            "type": "http", "url": format!("http://{addr}/mcp") }}});
        let server =
            dcode_ai_common::mcp_json::servers_from_value(&doc, "user", &Default::default())
                .unwrap()
                .remove(0);
        let mut client = HttpMcpClient::connect(server.url.as_deref().unwrap(), &server).unwrap();
        client.oauth = None; // ignore any real login on this machine
        let err = client
            .request("initialize", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.contains("dcode-ai mcp login remote"), "{err}");
    }
}
