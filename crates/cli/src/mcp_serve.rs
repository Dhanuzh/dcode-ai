//! `dcode-ai mcp serve`: expose dcode-ai's tools as an MCP server over stdio
//! (newline-delimited JSON-RPC), so Claude Code, Copilot CLI or any MCP
//! client can read, search and (with `--allow-writes`) edit this workspace.

use std::path::Path;

use dcode_ai_common::config::DcodeAiConfig;
use dcode_ai_common::tool::ToolCall;
use dcode_ai_core::tools::ToolRegistry;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Tools that only make sense inside a dcode-ai session.
const SESSION_ONLY: [&str; 1] = ["update_plan"];

pub fn registry(config: &DcodeAiConfig, workspace_root: &Path, allow_writes: bool) -> ToolRegistry {
    let mut registry = if allow_writes {
        ToolRegistry::with_default_full_tools(workspace_root.to_path_buf(), config.web.clone())
    } else {
        ToolRegistry::with_default_readonly_tools(workspace_root.to_path_buf(), config.web.clone())
    };
    registry.retain_by_name(|n| !SESSION_ONLY.contains(&n));
    registry
}

async fn handle(registry: &ToolRegistry, msg: &Value) -> Option<Value> {
    // Notifications (no id) get no reply.
    let id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "dcode-ai", "version": env!("CARGO_PKG_VERSION") },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": registry
                .definitions()
                .into_iter()
                .map(|d| json!({
                    "name": d.name,
                    "description": d.description,
                    "inputSchema": d.parameters,
                }))
                .collect::<Vec<_>>()
        })),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or_default();
            match params.get("name").and_then(Value::as_str) {
                None => Err((-32602, "missing tool name".to_string())),
                Some(name) => {
                    let call = ToolCall {
                        id: format!("mcp-{id}"),
                        name: name.to_string(),
                        input: params.get("arguments").cloned().unwrap_or(json!({})),
                    };
                    let r = registry.execute(&call).await;
                    let text = if r.success {
                        r.output
                    } else {
                        r.error.unwrap_or_else(|| "tool failed".into())
                    };
                    Ok(json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": !r.success,
                    }))
                }
            }
        }
        other => Err((-32601, format!("method not found: {other}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    })
}

/// Serve requests from `reader` until EOF, writing replies to `writer`.
pub async fn serve<R, W>(registry: &ToolRegistry, reader: R, mut writer: W) -> anyhow::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(msg) => handle(registry, &msg).await,
            Err(e) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": format!("parse error: {e}") }
            })),
        };
        if let Some(reply) = reply {
            writer.write_all(reply.to_string().as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
    }
    Ok(())
}

pub async fn run(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    allow_writes: bool,
) -> anyhow::Result<()> {
    let registry = registry(config, workspace_root, allow_writes);
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    serve(&registry, stdin, tokio::io::stdout()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn exchange(registry: &ToolRegistry, requests: &[Value]) -> Vec<Value> {
        let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
        let mut out = Vec::new();
        serve(registry, input.as_bytes(), &mut out).await.unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn speaks_mcp_and_runs_readonly_tools() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hi there\n").unwrap();
        let config = DcodeAiConfig::default();
        let reg = registry(&config, dir.path(), false);

        let replies = exchange(
            &reg,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
                       "params":{"name":"read_file","arguments":{"path":"hello.txt"}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
                       "params":{"name":"write_file","arguments":{"path":"x","content":"y"}}}),
                json!({"jsonrpc":"2.0","id":5,"method":"nope"}),
            ],
        )
        .await;

        assert_eq!(replies.len(), 5, "notification gets no reply");
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "dcode-ai");
        let names: Vec<&str> = replies[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"read_file") && names.contains(&"search_code"));
        assert!(!names.contains(&"write_file"), "read-only by default");
        assert!(!names.contains(&"update_plan"));
        assert!(
            replies[2]["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("hi there")
        );
        assert_eq!(replies[3]["result"]["isError"], true);
        assert_eq!(replies[4]["error"]["code"], -32601);
        assert!(!dir.path().join("x").exists());
    }

    #[test]
    fn allow_writes_adds_edit_tools() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&DcodeAiConfig::default(), dir.path(), true);
        let names: Vec<String> = reg.definitions().into_iter().map(|d| d.name).collect();
        assert!(names.iter().any(|n| n == "write_file"));
        assert!(names.iter().any(|n| n == "edit_file"));
    }
}
