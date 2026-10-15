//! `dcode-ai mcp …`: list, inspect, add and remove MCP servers.

use std::collections::BTreeMap;
use std::path::Path;

use clap::Subcommand;
use dcode_ai_common::config::{DcodeAiConfig, McpServerConfig};
use dcode_ai_common::mcp_json::{McpScope, NewServer, add_server, remove_server};

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// List configured servers and where each comes from
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one server's configuration (secrets masked)
    Get { name: String },
    /// Add a server: `mcp add files -- npx -y @modelcontextprotocol/server-filesystem .`
    /// or `mcp add docs --transport http https://example.com/mcp`
    Add {
        name: String,
        /// local (.dcode-ai/mcp.json), project (.mcp.json) or user (~/.dcode-ai/mcp.json)
        #[arg(long, short = 's', default_value = "local")]
        scope: String,
        /// stdio (default) or http
        #[arg(long, short = 't', default_value = "stdio")]
        transport: String,
        /// Environment variable for the server process, `KEY=value` (repeatable)
        #[arg(long = "env", short = 'e')]
        env: Vec<String>,
        /// HTTP header, `Name: value` (repeatable; values may use ${ENV_VAR})
        #[arg(long = "header", short = 'H')]
        header: Vec<String>,
        /// Command and arguments (stdio) or the URL (http)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        target: Vec<String>,
    },
    /// Run dcode-ai as an MCP server over stdio, exposing its tools for this
    /// directory (read-only unless --allow-writes)
    Serve {
        /// Also expose file-editing tools (write, edit, patch, move, delete)
        #[arg(long)]
        allow_writes: bool,
    },
    /// Remove a server from a scope's file (every scope when omitted)
    Remove {
        name: String,
        #[arg(long, short = 's')]
        scope: Option<String>,
    },
}

fn parse_scope(s: &str) -> anyhow::Result<McpScope> {
    McpScope::parse(s).ok_or_else(|| anyhow::anyhow!("unknown scope `{s}` (local, project, user)"))
}

fn origin_label(server: &McpServerConfig) -> String {
    if server.origin.is_empty() {
        "config.toml".to_string()
    } else {
        server.origin.clone()
    }
}

fn target_label(server: &McpServerConfig) -> String {
    match &server.url {
        Some(url) => format!("http  {url}"),
        None => format!("stdio {} {}", server.command, server.args.join(" "))
            .trim_end()
            .to_string(),
    }
}

/// Mask values that look like secrets (tokens, keys, auth headers).
fn mask(key: &str, value: &str) -> String {
    let k = key.to_ascii_lowercase();
    let secret = ["token", "key", "secret", "auth", "password"]
        .iter()
        .any(|w| k.contains(w));
    if secret && !value.is_empty() {
        "••••".to_string()
    } else {
        value.to_string()
    }
}

pub fn list(config: &DcodeAiConfig, json: bool) -> anyhow::Result<()> {
    if json {
        let rows: Vec<_> = config
            .mcp
            .servers
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "origin": origin_label(s),
                    "enabled": s.enabled,
                    "command": s.command,
                    "args": s.args,
                    "url": s.url,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if config.mcp.servers.is_empty() {
        println!("No MCP servers configured.");
        println!("Add one: dcode-ai mcp add <name> -- <command> [args…]");
        return Ok(());
    }
    let width = config
        .mcp
        .servers
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(0);
    for s in &config.mcp.servers {
        let dot = if s.enabled { "●" } else { "○" };
        println!(
            "{dot} {:<width$}  {:<12}  {}",
            s.name,
            origin_label(s),
            target_label(s)
        );
    }
    Ok(())
}

pub fn get(config: &DcodeAiConfig, name: &str) -> anyhow::Result<()> {
    let s = config
        .mcp
        .servers
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow::anyhow!("no MCP server named `{name}`"))?;
    println!("{}", s.name);
    println!("  source:  {}", origin_label(s));
    println!("  enabled: {}", s.enabled);
    match &s.url {
        Some(url) => {
            println!("  type:    http");
            println!("  url:     {url}");
        }
        None => {
            println!("  type:    stdio");
            println!("  command: {}", s.command);
            if !s.args.is_empty() {
                println!("  args:    {}", s.args.join(" "));
            }
        }
    }
    for (k, v) in &s.env {
        println!("  env:     {k}={}", mask(k, v));
    }
    for (k, v) in &s.headers {
        println!("  header:  {k}: {}", mask(k, v));
    }
    Ok(())
}

pub fn add(
    workspace_root: &Path,
    name: &str,
    scope: &str,
    transport: &str,
    env: &[String],
    header: &[String],
    target: &[String],
) -> anyhow::Result<()> {
    let scope = parse_scope(scope)?;
    let mut env_map = BTreeMap::new();
    for e in env {
        let (k, v) = e
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--env expects KEY=value, got `{e}`"))?;
        env_map.insert(k.trim().to_string(), v.to_string());
    }
    let mut headers = BTreeMap::new();
    for h in header {
        let (k, v) = h
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("--header expects `Name: value`, got `{h}`"))?;
        headers.insert(k.trim().to_string(), v.trim().to_string());
    }
    let server = match transport.to_ascii_lowercase().as_str() {
        "http" | "sse" | "streamable-http" => {
            let [url] = target else {
                anyhow::bail!("http transport takes exactly one URL");
            };
            NewServer {
                url: Some(url.clone()),
                env: env_map,
                headers,
                ..Default::default()
            }
        }
        "stdio" => {
            if !headers.is_empty() {
                anyhow::bail!("--header only applies to --transport http");
            }
            let (command, args) = target.split_first().expect("clap requires target");
            NewServer {
                command: Some(command.clone()),
                args: args.to_vec(),
                env: env_map,
                ..Default::default()
            }
        }
        other => anyhow::bail!("unknown transport `{other}` (stdio, http)"),
    };
    let path = add_server(scope, workspace_root, name, &server).map_err(anyhow::Error::msg)?;
    println!(
        "Added MCP server `{name}` ({} scope) to {}",
        scope.label(),
        path.display()
    );
    if scope == McpScope::Project {
        println!("Commit .mcp.json to share it with your team.");
    }
    Ok(())
}

pub fn remove(workspace_root: &Path, name: &str, scope: Option<&str>) -> anyhow::Result<()> {
    let scopes = match scope {
        Some(s) => vec![parse_scope(s)?],
        None => McpScope::ALL.to_vec(),
    };
    let mut removed = Vec::new();
    for scope in scopes {
        if remove_server(scope, workspace_root, name).map_err(anyhow::Error::msg)? {
            removed.push(scope.label());
        }
    }
    if removed.is_empty() {
        anyhow::bail!(
            "`{name}` is not in any mcp.json scope (servers in config.toml are edited there)"
        );
    }
    println!(
        "Removed MCP server `{name}` from {} scope",
        removed.join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_masked() {
        assert_eq!(mask("GITHUB_TOKEN", "abc"), "••••");
        assert_eq!(mask("Authorization", "Bearer x"), "••••");
        assert_eq!(mask("LOG_LEVEL", "debug"), "debug");
    }

    #[test]
    fn add_stdio_and_http_then_remove() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        add(
            ws,
            "files",
            "project",
            "stdio",
            &["ROOT=.".into()],
            &[],
            &["npx".into(), "-y".into(), "server-fs".into()],
        )
        .unwrap();
        add(
            ws,
            "docs",
            "local",
            "http",
            &[],
            &["Authorization: Bearer ${T}".into()],
            &["https://example.com/mcp".into()],
        )
        .unwrap();
        let mut config = DcodeAiConfig::default();
        config.add_mcp_json_servers(ws);
        let files = config
            .mcp
            .servers
            .iter()
            .find(|s| s.name == "files")
            .unwrap();
        assert_eq!(files.args, vec!["-y", "server-fs"]);
        assert_eq!(files.origin, "project");
        assert!(
            config
                .mcp
                .servers
                .iter()
                .any(|s| s.name == "docs" && s.url.is_some())
        );

        remove(ws, "docs", None).unwrap();
        assert!(remove(ws, "docs", None).is_err());
        assert!(add(ws, "bad", "nowhere", "stdio", &[], &[], &["x".into()]).is_err());
    }
}
