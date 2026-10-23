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
    /// Sign in to an OAuth-protected remote server (opens the browser)
    Login { name: String },
    /// Forget the sign-in for a remote server
    Logout { name: String },
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

/// `  [signed in]` etc. for remote servers.
fn auth_label(
    server: &McpServerConfig,
    logins: &dcode_ai_common::mcp_auth::McpAuthStore,
) -> &'static str {
    let Some(url) = &server.url else {
        return "";
    };
    if server
        .headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("authorization"))
    {
        "  [header auth]"
    } else if logins.login_for(url).is_some() {
        "  [signed in]"
    } else {
        ""
    }
}

fn find_remote<'a>(
    config: &'a DcodeAiConfig,
    name: &str,
) -> anyhow::Result<(&'a McpServerConfig, String)> {
    let server = config
        .mcp
        .servers
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow::anyhow!("no MCP server named `{name}` (see `dcode-ai mcp list`)"))?;
    let url = server.url.clone().ok_or_else(|| {
        anyhow::anyhow!("`{name}` is a local (stdio) server; only remote servers sign in")
    })?;
    Ok((server, url))
}

/// `dcode-ai mcp login <name>`: OAuth 2.1 authorization code + PKCE with a
/// loopback redirect (the MCP authorization spec).
pub async fn login(config: &DcodeAiConfig, name: &str) -> anyhow::Result<()> {
    let flow = match begin_login(config, name).await? {
        Some(flow) => flow,
        None => {
            println!("`{name}` does not ask for sign-in; nothing to do.");
            return Ok(());
        }
    };
    println!(
        "Sign in to `{name}` in your browser:\n\n  {}\n",
        flow.auth_url
    );
    if !crate::oauth_login::try_open_browser(&flow.auth_url) {
        println!("(Open the link above yourself.)");
    }
    println!("Waiting for the sign-in to finish (5 minutes)…");
    let code = tokio::time::timeout(std::time::Duration::from_secs(300), flow.wait_browser())
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for the browser sign-in"))??;
    flow.finish(&code).await?;
    println!(
        "Signed in to `{name}`. Its tools load in new sessions (or `/reload` in a running one)."
    );
    Ok(())
}

/// A sign-in in progress: the URL to open, and the loopback listener that
/// receives the browser's redirect.
pub struct LoginFlow {
    pub auth_url: String,
    url: String,
    http: reqwest::Client,
    discovery: dcode_ai_core::mcp_oauth::Discovery,
    client: dcode_ai_common::mcp_auth::McpOAuthClient,
    pkce: dcode_ai_core::mcp_oauth::Pkce,
    state: String,
    listener: tokio::net::TcpListener,
}

/// Start signing in to `name`: discover its authorization server, register
/// dcode-ai as a client (once per server), and build the authorization URL.
/// `None` when the server does not ask for sign-in.
pub async fn begin_login(config: &DcodeAiConfig, name: &str) -> anyhow::Result<Option<LoginFlow>> {
    use dcode_ai_common::mcp_auth::{McpAuthStore, client_key};
    use dcode_ai_core::mcp_oauth;
    let (_server, url) = find_remote(config, name)?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    // An unauthenticated request returns the 401 challenge that names the
    // server's metadata.
    let probe = http
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(
            &serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "dcode-ai", "version": env!("CARGO_PKG_VERSION")}}}),
        )
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("could not reach {url}: {e}"))?;
    let challenge = probe
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .and_then(mcp_oauth::parse_www_authenticate);
    if probe.status().is_success() && challenge.is_none() {
        return Ok(None);
    }
    let discovery = mcp_oauth::discover(&http, &url, challenge.as_ref())
        .await
        .map_err(anyhow::Error::msg)?;

    let listener = match tokio::net::TcpListener::bind("127.0.0.1:53682").await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind("127.0.0.1:0").await?,
    };
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let mut store = McpAuthStore::load();
    let ckey = client_key(&discovery.auth_server.issuer, &redirect_uri);
    let client = match store.clients.get(&ckey) {
        Some(c) => c.clone(),
        None => {
            let c = mcp_oauth::register_client(&http, &discovery.auth_server, &redirect_uri)
                .await
                .map_err(anyhow::Error::msg)?;
            store.clients.insert(ckey, c.clone());
            store.save().map_err(anyhow::Error::msg)?;
            c
        }
    };
    let pkce = mcp_oauth::Pkce::new();
    let state = mcp_oauth::random_token();
    let auth_url = mcp_oauth::authorize_url(&discovery, &client, &pkce, &state);
    Ok(Some(LoginFlow {
        auth_url,
        url,
        http,
        discovery,
        client,
        pkce,
        state,
        listener,
    }))
}

impl LoginFlow {
    /// Wait for the browser's redirect to the loopback listener.
    pub async fn wait_browser(&self) -> anyhow::Result<String> {
        wait_for_callback(&self.listener, &self.state).await
    }

    /// The code from a redirect URL the user pasted (when the browser
    /// could not reach localhost, e.g. on WSL).
    pub fn code_from_pasted(&self, pasted: &str) -> anyhow::Result<String> {
        code_from_redirect(pasted, &self.state)
    }

    /// Exchange the code for tokens and save the sign-in.
    pub async fn finish(self, code: &str) -> anyhow::Result<()> {
        use dcode_ai_common::mcp_auth::{McpAuthStore, McpServerLogin, key};
        let tokens = dcode_ai_core::mcp_oauth::exchange_code(
            &self.http,
            &self.discovery,
            &self.client,
            code,
            &self.pkce,
        )
        .await
        .map_err(anyhow::Error::msg)?;
        let mut store = McpAuthStore::load();
        store.servers.insert(
            key(&self.url),
            McpServerLogin {
                issuer: self.discovery.auth_server.issuer.clone(),
                token_endpoint: self.discovery.auth_server.token_endpoint.clone(),
                client: self.client,
                access_token: tokens.access_token,
                refresh_token: tokens.refresh_token,
                expires_at: tokens.expires_at,
                scope: tokens.scope,
            },
        );
        store.save().map_err(anyhow::Error::msg)
    }
}

/// `code` from a pasted redirect URL whose `state` matches.
fn code_from_redirect(pasted: &str, state: &str) -> anyhow::Result<String> {
    let url = url::Url::parse(pasted.trim()).map_err(|_| {
        anyhow::anyhow!("that is not a URL; paste the whole address from the browser")
    })?;
    let get = |k: &str| {
        url.query_pairs()
            .find(|(name, _)| name == k)
            .map(|(_, v)| v.to_string())
    };
    if let Some(err) = get("error") {
        anyhow::bail!(
            "sign-in failed: {err} {}",
            get("error_description").unwrap_or_default()
        );
    }
    if get("state").as_deref() != Some(state) {
        anyhow::bail!("that URL is from a different sign-in (state does not match)");
    }
    get("code").ok_or_else(|| anyhow::anyhow!("no code in that URL"))
}

/// Serve one redirect on the loopback listener; return the `code` once the
/// `state` matches.
async fn wait_for_callback(
    listener: &tokio::net::TcpListener,
    state: &str,
) -> anyhow::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
        let parsed = url::Url::parse(&format!("http://127.0.0.1{path}")).ok();
        let get = |k: &str| {
            parsed.as_ref().and_then(|u| {
                u.query_pairs()
                    .find(|(name, _)| name == k)
                    .map(|(_, v)| v.to_string())
            })
        };
        if !path.starts_with("/callback") {
            let _ = sock
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let (ok, message) = if let Some(err) = get("error") {
            (
                false,
                format!(
                    "Sign-in failed: {err} {}",
                    get("error_description").unwrap_or_default()
                ),
            )
        } else if get("state").as_deref() != Some(state) {
            (
                false,
                "Sign-in failed: state mismatch (possible CSRF); try again.".to_string(),
            )
        } else if get("code").is_some() {
            (
                true,
                "Signed in to dcode-ai. You can close this tab.".to_string(),
            )
        } else {
            (
                false,
                "Sign-in failed: no code in the redirect.".to_string(),
            )
        };
        let body = format!(
            "<html><body style=\"font-family:sans-serif\"><h3>{message}</h3></body></html>"
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        return if ok {
            Ok(get("code").unwrap_or_default())
        } else {
            Err(anyhow::anyhow!(message))
        };
    }
}

/// `dcode-ai mcp logout <name>`.
pub fn logout(config: &DcodeAiConfig, name: &str) -> anyhow::Result<()> {
    if sign_out(config, name)? {
        println!("Signed out of `{name}`.");
    } else {
        println!("`{name}` was not signed in.");
    }
    Ok(())
}

/// Forget the saved sign-in for `name`. False when there was none.
pub fn sign_out(config: &DcodeAiConfig, name: &str) -> anyhow::Result<bool> {
    let (_server, url) = find_remote(config, name)?;
    let mut store = dcode_ai_common::mcp_auth::McpAuthStore::load();
    if store
        .servers
        .remove(&dcode_ai_common::mcp_auth::key(&url))
        .is_none()
    {
        return Ok(false);
    }
    store.save().map_err(anyhow::Error::msg)?;
    Ok(true)
}

/// Whether a remote server at `url` has a saved sign-in.
pub fn signed_in(url: &str) -> bool {
    dcode_ai_common::mcp_auth::McpAuthStore::load()
        .servers
        .contains_key(&dcode_ai_common::mcp_auth::key(url))
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
    let logins = dcode_ai_common::mcp_auth::McpAuthStore::load();
    for s in &config.mcp.servers {
        let dot = if s.enabled { "●" } else { "○" };
        println!(
            "{dot} {:<width$}  {:<12}  {}{}",
            s.name,
            origin_label(s),
            target_label(s),
            auth_label(s, &logins)
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
    fn pasted_redirect_gives_the_code_only_for_this_sign_in() {
        let ok = "http://127.0.0.1:53682/callback?code=abc&state=s1";
        assert_eq!(code_from_redirect(ok, "s1").unwrap(), "abc");
        assert!(code_from_redirect(ok, "other").is_err());
        assert!(code_from_redirect("not a url", "s1").is_err());
        assert!(
            code_from_redirect(
                "http://localhost/callback?error=access_denied&state=s1",
                "s1"
            )
            .unwrap_err()
            .to_string()
            .contains("access_denied")
        );
    }

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
