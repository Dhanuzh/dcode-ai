use crate::tools::ToolExecutor;
use dcode_ai_common::config::McpServerConfig;
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub fn load_mcp_tools(
    workspace_root: &Path,
    servers: &[McpServerConfig],
) -> Result<Vec<Box<dyn ToolExecutor>>, String> {
    let mut tools: Vec<Box<dyn ToolExecutor>> = Vec::new();
    for server in servers.iter().filter(|server| server.enabled) {
        let server_tools = discover_server_tools(workspace_root, server)?;
        for tool in server_tools {
            tools.push(Box::new(tool));
        }
    }
    Ok(tools)
}

#[derive(Clone)]
pub struct McpTool {
    server: McpServerConfig,
    workspace_root: PathBuf,
    tool_name: String,
    description: Option<String>,
    parameters: Value,
}

impl McpTool {
    fn prefixed_name(&self) -> String {
        format!("mcp__{}__{}", self.server.name, self.tool_name)
    }
}

#[async_trait::async_trait]
impl ToolExecutor for McpTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.prefixed_name(),
            description: self.description.clone().unwrap_or_else(|| {
                format!("MCP tool `{}` from `{}`", self.tool_name, self.server.name)
            }),
            parameters: self.parameters.clone(),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let server = self.server.clone();
        let workspace_root = self.workspace_root.clone();
        let tool_name = self.tool_name.clone();
        let input = call.input.clone();
        let call_id = call.id.clone();
        match tokio::task::spawn_blocking(move || {
            execute_mcp_call(&workspace_root, &server, &tool_name, input)
        })
        .await
        {
            Ok(Ok(output)) => ToolResult {
                call_id,
                success: true,
                output,
                error: None,
            },
            Ok(Err(error)) => ToolResult {
                call_id,
                success: false,
                output: String::new(),
                error: Some(error),
            },
            Err(error) => ToolResult {
                call_id,
                success: false,
                output: String::new(),
                error: Some(error.to_string()),
            },
        }
    }
}

fn discover_server_tools(
    workspace_root: &Path,
    server: &McpServerConfig,
) -> Result<Vec<McpTool>, String> {
    let mut client = McpClient::connect(workspace_root, server)?;
    client.initialize()?;
    let tools = client.list_tools()?;
    client.shutdown();
    Ok(tools
        .into_iter()
        .map(|tool| McpTool {
            server: server.clone(),
            workspace_root: workspace_root.to_path_buf(),
            tool_name: tool.name,
            description: tool.description,
            parameters: serde_json::json!({
                "type": "object",
                "properties": tool.input_schema.properties,
                "required": tool.input_schema.required,
            }),
        })
        .collect())
}

fn execute_mcp_call(
    workspace_root: &Path,
    server: &McpServerConfig,
    tool_name: &str,
    input: Value,
) -> Result<String, String> {
    let mut client = McpClient::connect(workspace_root, server)?;
    client.initialize()?;
    let result = client.call_tool(tool_name, input)?;
    client.shutdown();
    serde_json::to_string(&result).map_err(|err| err.to_string())
}

#[derive(Debug, serde::Deserialize)]
struct McpToolSchema {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "inputSchema", alias = "input_schema")]
    input_schema: McpInputSchema,
}

#[derive(Debug, Default, serde::Deserialize)]
struct McpInputSchema {
    #[serde(default)]
    properties: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    required: Option<Vec<String>>,
}

/// JSON-RPC plumbing shared by every MCP transport. Implementors only provide
/// `request`/`notify`/`close`; the protocol handshake and tool calls are common.
trait McpTransport {
    /// Send a JSON-RPC request and return its `result` value.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, String>;
    /// Send a fire-and-forget JSON-RPC notification.
    fn notify(&mut self, method: &str, params: Value) -> Result<(), String>;
    /// Release any underlying resources (child process, connection).
    fn close(&mut self);

    fn initialize(&mut self) -> Result<(), String> {
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
        )?;
        self.notify(
            "notifications/initialized",
            Value::Object(Default::default()),
        )?;
        Ok(())
    }

    fn list_tools(&mut self) -> Result<Vec<McpToolSchema>, String> {
        let result = self.request("tools/list", serde_json::json!({}))?;
        serde_json::from_value(
            result
                .get("tools")
                .cloned()
                .unwrap_or_else(|| Value::Array(vec![])),
        )
        .map_err(|err| format!("failed to decode MCP tool list: {err}"))
    }

    fn call_tool(&mut self, tool_name: &str, input: Value) -> Result<Value, String> {
        self.request(
            "tools/call",
            serde_json::json!({
                "name": tool_name,
                "arguments": input,
            }),
        )
    }

    fn shutdown(&mut self) {
        let _ = self.request("shutdown", Value::Null);
        let _ = self.notify("exit", Value::Null);
        self.close();
    }
}

/// Picks a transport for `server`: HTTP when `url` is set, stdio otherwise.
struct McpClient;

impl McpClient {
    fn connect(
        workspace_root: &Path,
        server: &McpServerConfig,
    ) -> Result<Box<dyn McpTransport>, String> {
        match &server.url {
            Some(url) if !url.trim().is_empty() => {
                Ok(Box::new(HttpMcpClient::connect(url, server)?))
            }
            _ => Ok(Box::new(StdioMcpClient::spawn(workspace_root, server)?)),
        }
    }
}

/// Streamable-HTTP transport: JSON-RPC over POST to a single endpoint. Accepts
/// either a JSON or an `text/event-stream` (SSE) response body, and threads the
/// optional `Mcp-Session-Id` returned at initialize through later requests.
struct HttpMcpClient {
    client: reqwest::blocking::Client,
    url: String,
    next_id: u64,
    session_id: Option<String>,
    /// Resolved extra headers (e.g. Authorization), `${ENV}` already expanded.
    headers: Vec<(String, String)>,
}

/// Expand `${ENV_VAR}` references in a header value from the process env.
/// Unset variables expand to empty so a missing secret fails loudly upstream
/// rather than leaking the literal `${...}`.
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

impl HttpMcpClient {
    fn connect(url: &str, server: &McpServerConfig) -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .build()
            .map_err(|err| {
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

    fn post(&mut self, payload: &Value) -> Result<reqwest::blocking::Response, String> {
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
            let body = resp.text().unwrap_or_default();
            return Err(format!("MCP HTTP error {status}: {body}"));
        }
        Ok(resp)
    }

    /// Extract the JSON-RPC response for `id` from a JSON or SSE response body.
    fn parse_response(body: &str, id: u64) -> Result<Value, String> {
        let matches_id = |v: &Value| v.get("id") == Some(&Value::from(id));

        // Direct JSON body.
        if let Ok(value) = serde_json::from_str::<Value>(body.trim()) {
            return Self::result_from(&value);
        }

        // SSE body: scan `data:` lines for the matching response.
        for line in body.lines() {
            let Some(data) = line.trim().strip_prefix("data:") else {
                continue;
            };
            if let Ok(value) = serde_json::from_str::<Value>(data.trim())
                && matches_id(&value)
            {
                return Self::result_from(&value);
            }
        }
        Err("MCP HTTP response contained no matching JSON-RPC result".to_string())
    }

    fn result_from(value: &Value) -> Result<Value, String> {
        if let Some(error) = value.get("error") {
            return Err(format!("MCP error: {error}"));
        }
        Ok(value
            .get("result")
            .cloned()
            .unwrap_or(Value::Object(Default::default())))
    }
}

impl McpTransport for HttpMcpClient {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let resp = self.post(&payload)?;
        let body = resp
            .text()
            .map_err(|err| format!("failed to read MCP HTTP response: {err}"))?;
        Self::parse_response(&body, id)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        // Notifications get no response body; a non-2xx is still worth surfacing.
        self.post(&payload).map(|_| ())
    }

    fn close(&mut self) {}
}

/// Stdio transport: spawns `command` and speaks newline-delimited JSON-RPC.
struct StdioMcpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl StdioMcpClient {
    fn spawn(workspace_root: &Path, server: &McpServerConfig) -> Result<Self, String> {
        if server.command.trim().is_empty() {
            return Err(format!(
                "MCP server `{}` has no `command` and no `url`",
                server.name
            ));
        }
        let mut command = Command::new(&server.command);
        command
            .args(&server.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

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

    fn write_message(&mut self, value: &Value) -> Result<(), String> {
        let line = serde_json::to_string(value).map_err(|err| err.to_string())?;
        writeln!(self.stdin, "{line}").map_err(|err| err.to_string())?;
        self.stdin.flush().map_err(|err| err.to_string())
    }

    fn read_message(&mut self) -> Result<Value, String> {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .map_err(|err| err.to_string())?;
        serde_json::from_str(&line).map_err(|err| err.to_string())
    }
}

impl McpTransport for StdioMcpClient {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let value = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&value)?;
        loop {
            let response = self.read_message()?;
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

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let value = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&value)
    }

    fn close(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn http_parse_response_reads_plain_json() {
        let body = r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#;
        let result = HttpMcpClient::parse_response(body, 7).expect("parse");
        assert_eq!(result, json!({"ok": true}));
    }

    #[test]
    fn http_parse_response_reads_sse_event() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"tools\":[]}}\n\n";
        let result = HttpMcpClient::parse_response(body, 3).expect("parse");
        assert_eq!(result, json!({"tools": []}));
    }

    #[test]
    fn env_var_expansion_in_header_values() {
        unsafe {
            std::env::set_var("DCODE_TEST_MCP_TOKEN", "secret123");
        }
        assert_eq!(
            expand_env_vars("Bearer ${DCODE_TEST_MCP_TOKEN}"),
            "Bearer secret123"
        );
        // Unmatched/empty cases.
        assert_eq!(expand_env_vars("no vars here"), "no vars here");
        assert_eq!(expand_env_vars("${UNSET_VAR_XYZ}"), "");
        assert_eq!(expand_env_vars("${unterminated"), "${unterminated");
    }

    #[test]
    fn http_parse_response_surfaces_jsonrpc_error() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#;
        let err = HttpMcpClient::parse_response(body, 1).unwrap_err();
        assert!(err.contains("MCP error"), "got: {err}");
    }

    #[tokio::test]
    async fn loads_and_executes_stdio_mcp_tool() {
        let temp = tempdir().expect("tempdir");
        let server_path = compile_mock_mcp_server(temp.path());
        let config = McpServerConfig {
            name: "mock".into(),
            command: server_path.display().to_string(),
            args: Vec::new(),
            env: Default::default(),
            cwd: None,
            url: None,
            headers: Default::default(),
            enabled: true,
        };

        let tools = load_mcp_tools(temp.path(), &[config]).expect("load MCP tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].definition().name, "mcp__mock__echo");

        let result = tools[0]
            .execute(&ToolCall {
                id: "call-1".into(),
                name: "mcp__mock__echo".into(),
                input: json!({
                    "message": "hello"
                }),
            })
            .await;

        assert!(result.success, "tool should succeed: {:?}", result.error);
        assert!(result.output.contains("\"echoed\":\"hello\""));
    }

    fn compile_mock_mcp_server(dir: &Path) -> PathBuf {
        let source_path = dir.join("mock_mcp_server.rs");
        let binary_path = dir.join("mock_mcp_server");
        std::fs::write(&source_path, mock_server_source()).expect("write mock MCP source");

        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let output = std::process::Command::new(rustc)
            .arg("--edition=2021")
            .arg(&source_path)
            .arg("-o")
            .arg(&binary_path)
            .output()
            .expect("compile mock MCP server");

        assert!(
            output.status.success(),
            "failed to compile mock MCP server: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        binary_path
    }

    fn mock_server_source() -> &'static str {
        r#"
use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let message: String = line.clone();
        let id = extract_number_field(&message, "\"id\":");
        let method = extract_string_field(&message, "\"method\":\"").unwrap_or_default();

        match method.as_str() {
            "initialize" => {
                let response = format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{{\"tools\":{{}}}},\"serverInfo\":{{\"name\":\"mock\",\"version\":\"1.0.0\"}}}}}}",
                    id.unwrap_or(1)
                );
                writeln!(stdout, "{}", response).unwrap();
                stdout.flush().unwrap();
            }
            "tools/list" => {
                let response = format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"tools\":[{{\"name\":\"echo\",\"description\":\"Echo tool\",\"inputSchema\":{{\"type\":\"object\",\"properties\":{{\"message\":{{\"type\":\"string\"}}}},\"required\":[\"message\"]}}}}]}}}}",
                    id.unwrap_or(1)
                );
                writeln!(stdout, "{}", response).unwrap();
                stdout.flush().unwrap();
            }
            "tools/call" => {
                let message = extract_string_field(&message, "\"message\":\"").unwrap_or_default();
                let response = format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"echoed\":\"{}\"}}}}",
                    id.unwrap_or(1),
                    escape_json(&message)
                );
                writeln!(stdout, "{}", response).unwrap();
                stdout.flush().unwrap();
            }
            "shutdown" => {
                let response = format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{}}}}",
                    id.unwrap_or(1)
                );
                writeln!(stdout, "{}", response).unwrap();
                stdout.flush().unwrap();
                break;
            }
            _ => {}
        }
    }
}

fn extract_number_field(input: &str, marker: &str) -> Option<u64> {
    let rest = input.split(marker).nth(1)?;
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn extract_string_field(input: &str, marker: &str) -> Option<String> {
    let rest = input.split(marker).nth(1)?;
    let mut out = String::new();
    let mut escaped = false;
    for ch in rest.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => break,
            _ => out.push(ch),
        }
    }
    Some(out)
}

fn escape_json(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}
"#
    }
}
