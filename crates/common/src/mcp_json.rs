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
    expand(value, extra, false).0
}

/// Like [`expand_vars`], but a `${VAR}` that is unset (or empty) and has no
/// default is left in place verbatim, so a later [`expand_vars_strict`] can
/// report it. Used for MCP headers, which are resolved again at connect time.
pub fn expand_vars_keep_unset(value: &str, extra: &BTreeMap<String, String>) -> String {
    expand(value, extra, true).0
}

/// Like [`expand_vars`], but fails instead of substituting an empty string.
/// The error is the name of the first unset (or empty) variable that has no
/// `:-default`.
pub fn expand_vars_strict(value: &str, extra: &BTreeMap<String, String>) -> Result<String, String> {
    let (out, missing) = expand(value, extra, false);
    match missing.into_iter().next() {
        Some(name) => Err(name),
        None => Ok(out),
    }
}

/// Shared expansion. Returns the expanded text and the names of variables
/// that were unset (or empty) with no default; those are either dropped or,
/// with `keep_unset`, written back as `${…}`.
fn expand(
    value: &str,
    extra: &BTreeMap<String, String>,
    keep_unset: bool,
) -> (String, Vec<String>) {
    let mut out = String::with_capacity(value.len());
    let mut missing = Vec::new();
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
        match found.or_else(|| default.map(str::to_string)) {
            Some(v) => out.push_str(&v),
            None => {
                missing.push(name.to_string());
                if keep_unset {
                    out.push_str("${");
                    out.push_str(inner);
                    out.push('}');
                }
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    (out, missing)
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
        let strings = |key: &str, f: &dyn Fn(&str) -> String| -> BTreeMap<String, String> {
            obj.get(key)
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), f(v))))
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
            env: strings("env", &x),
            cwd: obj
                .get("cwd")
                .and_then(Value::as_str)
                .map(|c| PathBuf::from(x(c))),
            url,
            // Unset `${VAR}`s stay literal so the HTTP client can fail loudly
            // at connect time instead of sending e.g. `Authorization: Basic `.
            headers: strings("headers", &|s| expand_vars_keep_unset(s, vars)),
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

    // Never set in any environment; tests must not call `set_var` (edition 2024).
    const UNSET: &str = "DCODE_AI_TEST_UNSET_VAR_9F3A";

    #[test]
    fn strict_expansion_reports_the_unset_variable() {
        let mut extra = BTreeMap::new();
        extra.insert("USER_X".to_string(), "me".to_string());
        assert_eq!(
            expand_vars_strict("Basic ${USER_X}", &extra).as_deref(),
            Ok("Basic me")
        );
        assert_eq!(
            expand_vars_strict(&format!("Basic ${{{UNSET}}}"), &extra),
            Err(UNSET.to_string())
        );
        // A default (even an empty one) is an explicit choice, not an error.
        assert_eq!(
            expand_vars_strict(&format!("a${{{UNSET}:-b}}c"), &extra).as_deref(),
            Ok("abc")
        );
        assert_eq!(
            expand_vars_strict(&format!("a${{{UNSET}:-}}c"), &extra).as_deref(),
            Ok("ac")
        );
        // An empty value counts as unset.
        extra.insert("EMPTY_X".to_string(), String::new());
        assert_eq!(
            expand_vars_strict("${EMPTY_X}", &extra),
            Err("EMPTY_X".to_string())
        );
    }

    #[test]
    fn keep_unset_leaves_only_unresolved_placeholders() {
        let mut extra = BTreeMap::new();
        extra.insert("ROOT_X".to_string(), "/plug".to_string());
        let kept =
            expand_vars_keep_unset(&format!("${{ROOT_X}}:${{{UNSET}}}:${{{UNSET}:-d}}"), &extra);
        assert_eq!(kept, format!("/plug:${{{UNSET}}}:d"));
        // The lenient form is unchanged: unset becomes empty.
        assert_eq!(expand_vars(&format!("x${{{UNSET}}}y"), &extra), "xy");
    }

    #[test]
    fn unset_header_variables_survive_parsing_but_env_does_not() {
        let text = format!(
            r#"{{"mcpServers": {{"atl": {{"type": "http", "url": "https://x/mcp",
                "headers": {{"Authorization": "Basic ${{{UNSET}}}"}},
                "env": {{"K": "v${{{UNSET}}}"}}}}}}}}"#
        );
        let servers = parse_servers(&text, "user", &BTreeMap::new()).expect("parse");
        assert_eq!(
            servers[0].headers["Authorization"],
            format!("Basic ${{{UNSET}}}")
        );
        assert_eq!(servers[0].env["K"], "v");
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
