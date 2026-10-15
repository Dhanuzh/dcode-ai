//! Claude Code-compatible MCP server files (`.mcp.json`).
//!
//! ```json
//! { "mcpServers": {
//!     "files":  { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] },
//!     "remote": { "type": "http", "url": "https://example.com/mcp",
//!                 "headers": { "Authorization": "Bearer ${TOKEN}" } } } }
//! ```
//!
//! Scopes: `local` = `<ws>/.dcode-ai/mcp.json` (personal, not committed),
//! `project` = `<ws>/.mcp.json` (shared with the team), `user` =
//! `~/.dcode-ai/mcp.json` (every workspace).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::config::{McpServerConfig, dcode_ai_home_dir};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpScope {
    Local,
    Project,
    User,
}

impl McpScope {
    /// Precedence order when the same name appears in several files.
    pub const ALL: [McpScope; 3] = [McpScope::Local, McpScope::Project, McpScope::User];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" => Some(Self::Local),
            "project" => Some(Self::Project),
            "user" | "global" => Some(Self::User),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Project => "project",
            Self::User => "user",
        }
    }

    pub fn path(self, workspace_root: &Path) -> Option<PathBuf> {
        match self {
            Self::Local => Some(workspace_root.join(".dcode-ai/mcp.json")),
            Self::Project => Some(workspace_root.join(".mcp.json")),
            Self::User => dcode_ai_home_dir().map(|h| h.join("mcp.json")),
        }
    }
}

/// Expand `${VAR}` and `${VAR:-default}` from the environment, plus any
/// `extra` variables (e.g. `CLAUDE_PLUGIN_ROOT`). Unset variables without a
/// default expand to an empty string.
pub fn expand_vars(value: &str, extra: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let inner = &after[..end];
        let (name, default) = match inner.split_once(":-") {
            Some((n, d)) => (n, Some(d)),
            None => (inner, None),
        };
        let found = extra
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
            .filter(|v| !v.is_empty());
        out.push_str(&found.unwrap_or_else(|| default.unwrap_or("").to_string()));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Parse the `mcpServers` object of an `.mcp.json` document. `origin` is
/// recorded on each server (shown by `/mcp`); `vars` are extra `${…}`
/// substitutions.
pub fn parse_servers(
    text: &str,
    origin: &str,
    vars: &BTreeMap<String, String>,
) -> Result<Vec<McpServerConfig>, String> {
    let doc: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    servers_from_value(&doc, origin, vars)
}

/// Servers from a JSON value holding `mcpServers` (or the map itself).
pub fn servers_from_value(
    doc: &Value,
    origin: &str,
    vars: &BTreeMap<String, String>,
) -> Result<Vec<McpServerConfig>, String> {
    let map = doc
        .get("mcpServers")
        .unwrap_or(doc)
        .as_object()
        .ok_or("`mcpServers` must be an object")?;
    let x = |s: &str| expand_vars(s, vars);
    let mut out = Vec::new();
    for (name, spec) in map {
        let obj = spec
            .as_object()
            .ok_or_else(|| format!("server `{name}` must be an object"))?;
        let strings = |key: &str| -> BTreeMap<String, String> {
            obj.get(key)
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), x(v))))
                        .collect()
                })
                .unwrap_or_default()
        };
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or("stdio");
        let url = obj.get("url").and_then(Value::as_str).map(x);
        let command = obj
            .get("command")
            .and_then(Value::as_str)
            .map(x)
            .unwrap_or_default();
        if url.is_none() && command.is_empty() {
            return Err(format!("server `{name}` needs `command` or `url`"));
        }
        if url.is_some() && !matches!(kind, "http" | "sse" | "streamable-http" | "stdio") {
            return Err(format!("server `{name}`: unsupported type `{kind}`"));
        }
        out.push(McpServerConfig {
            name: name.clone(),
            command,
            args: obj
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(x).collect())
                .unwrap_or_default(),
            env: strings("env"),
            cwd: obj
                .get("cwd")
                .and_then(Value::as_str)
                .map(|c| PathBuf::from(x(c))),
            url,
            headers: strings("headers"),
            enabled: !obj
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            origin: origin.to_string(),
        });
    }
    Ok(out)
}

/// Servers declared in one scope's file; a broken file is reported and
/// skipped rather than failing startup.
pub fn load_scope(scope: McpScope, workspace_root: &Path) -> Vec<McpServerConfig> {
    let Some(path) = scope.path(workspace_root) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match parse_servers(&text, scope.label(), &BTreeMap::new()) {
        Ok(servers) => servers,
        Err(e) => {
            tracing::warn!("ignoring {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// One server entry as written by `dcode-ai mcp add`.
#[derive(Debug, Clone, Default)]
pub struct NewServer {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub env: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
}

impl NewServer {
    fn to_json(&self) -> Value {
        let mut obj = Map::new();
        if let Some(url) = &self.url {
            obj.insert("type".into(), json!("http"));
            obj.insert("url".into(), json!(url));
            if !self.headers.is_empty() {
                obj.insert("headers".into(), json!(self.headers));
            }
        } else {
            obj.insert(
                "command".into(),
                json!(self.command.clone().unwrap_or_default()),
            );
            obj.insert("args".into(), json!(self.args));
        }
        if !self.env.is_empty() {
            obj.insert("env".into(), json!(self.env));
        }
        Value::Object(obj)
    }
}

fn read_doc(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(_) => Ok(json!({ "mcpServers": {} })),
    }
}

fn write_doc(path: &Path, doc: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(doc).map_err(|e| e.to_string())?;
    std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
}

/// Add (or replace) `name` in `scope`'s file, keeping everything else in it.
pub fn add_server(
    scope: McpScope,
    workspace_root: &Path,
    name: &str,
    server: &NewServer,
) -> Result<PathBuf, String> {
    let path = scope
        .path(workspace_root)
        .ok_or("no home directory for user scope")?;
    let mut doc = read_doc(&path)?;
    if !doc.is_object() {
        doc = json!({});
    }
    let root = doc.as_object_mut().expect("object");
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("`mcpServers` must be an object")?;
    servers.insert(name.to_string(), server.to_json());
    write_doc(&path, &doc)?;
    Ok(path)
}

/// Remove `name` from `scope`'s file. Returns whether it was there.
pub fn remove_server(scope: McpScope, workspace_root: &Path, name: &str) -> Result<bool, String> {
    let Some(path) = scope.path(workspace_root) else {
        return Ok(false);
    };
    if !path.exists() {
        return Ok(false);
    }
    let mut doc = read_doc(&path)?;
    let removed = doc
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .and_then(|m| m.remove(name))
        .is_some();
    if removed {
        write_doc(&path, &doc)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stdio_and_http_servers_with_env_expansion() {
        let mut vars = BTreeMap::new();
        vars.insert("CLAUDE_PLUGIN_ROOT".to_string(), "/plug".to_string());
        let servers = parse_servers(
            r#"{"mcpServers": {
                "fs": {"command": "${CLAUDE_PLUGIN_ROOT}/bin/fs", "args": ["--root", "${MISSING_VAR_X:-.}"],
                       "env": {"LEVEL": "debug"}},
                "web": {"type": "http", "url": "https://example.com/mcp",
                        "headers": {"Authorization": "Bearer ${MISSING_VAR_Y:-none}"}},
                "off": {"command": "x", "disabled": true}
            }}"#,
            "project",
            &vars,
        )
        .expect("parse");
        let fs = servers.iter().find(|s| s.name == "fs").unwrap();
        assert_eq!(fs.command, "/plug/bin/fs");
        assert_eq!(fs.args, vec!["--root", "."]);
        assert_eq!(fs.env["LEVEL"], "debug");
        assert_eq!(fs.origin, "project");
        let web = servers.iter().find(|s| s.name == "web").unwrap();
        assert_eq!(web.url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(web.headers["Authorization"], "Bearer none");
        assert!(!servers.iter().find(|s| s.name == "off").unwrap().enabled);
    }

    #[test]
    fn rejects_servers_without_command_or_url() {
        let err = parse_servers(r#"{"mcpServers": {"x": {}}}"#, "p", &BTreeMap::new());
        assert!(err.unwrap_err().contains("needs `command` or `url`"));
    }

    #[test]
    fn add_and_remove_round_trip_keeps_other_entries() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::write(
            ws.join(".mcp.json"),
            r#"{"mcpServers": {"keep": {"command": "a"}}, "other": 1}"#,
        )
        .unwrap();
        add_server(
            McpScope::Project,
            ws,
            "web",
            &NewServer {
                url: Some("https://x/mcp".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let names: Vec<_> = load_scope(McpScope::Project, ws)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["keep", "web"]);
        assert!(remove_server(McpScope::Project, ws, "web").unwrap());
        assert!(!remove_server(McpScope::Project, ws, "web").unwrap());
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(doc["other"], 1);
    }
}
