//! Sign-ins for OAuth-protected remote MCP servers:
//! `~/.dcode-ai/mcp-auth.json` (owner-only), keyed by the server URL so a
//! renamed server or the same server from a plugin shares one login.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A client dcode-ai registered with an authorization server (dynamic
/// client registration). Reused for later logins with the same redirect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpOAuthClient {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// `none`, `client_secret_post` or `client_secret_basic`.
    #[serde(default = "default_auth_method")]
    pub token_endpoint_auth_method: String,
    pub redirect_uri: String,
}

fn default_auth_method() -> String {
    "none".into()
}

/// Tokens for one MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerLogin {
    pub issuer: String,
    pub token_endpoint: String,
    pub client: McpOAuthClient,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl McpServerLogin {
    /// Expired, or within a minute of expiring.
    pub fn is_expired(&self, now_unix: i64) -> bool {
        self.expires_at.is_some_and(|t| t <= now_unix + 60)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpAuthStore {
    /// Server URL key (see [`key`]) → login.
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerLogin>,
    /// `issuer redirect_uri` → registered client, so a new login reuses it.
    #[serde(default)]
    pub clients: BTreeMap<String, McpOAuthClient>,
}

/// Store key for a server URL: scheme and host lowercased, no trailing `/`.
pub fn key(server_url: &str) -> String {
    let url = server_url.trim().trim_end_matches('/');
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            let mut k = format!(
                "{}://{}",
                scheme.to_ascii_lowercase(),
                host.to_ascii_lowercase()
            );
            if !path.is_empty() {
                k.push('/');
                k.push_str(path);
            }
            k
        }
        None => url.to_string(),
    }
}

pub fn client_key(issuer: &str, redirect_uri: &str) -> String {
    format!("{} {redirect_uri}", issuer.trim_end_matches('/'))
}

pub fn store_path() -> Option<PathBuf> {
    crate::config::dcode_ai_home_dir().map(|h| h.join("mcp-auth.json"))
}

impl McpAuthStore {
    pub fn load() -> Self {
        store_path()
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = store_path().ok_or("no home directory")?;
        self.save_to(&path)
    }

    /// Atomic write, readable by the owner only.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn login_for(&self, server_url: &str) -> Option<&McpServerLogin> {
        self.servers.get(&key(server_url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login() -> McpServerLogin {
        McpServerLogin {
            issuer: "https://auth.example.com".into(),
            token_endpoint: "https://auth.example.com/token".into(),
            client: McpOAuthClient {
                client_id: "c1".into(),
                client_secret: None,
                token_endpoint_auth_method: "none".into(),
                redirect_uri: "http://127.0.0.1:53682/callback".into(),
            },
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            expires_at: Some(1_000),
            scope: None,
        }
    }

    #[test]
    fn keys_ignore_case_of_host_and_trailing_slash() {
        assert_eq!(
            key("HTTPS://MCP.Atlassian.com/v2/mcp/"),
            "https://mcp.atlassian.com/v2/mcp"
        );
        assert_eq!(key("https://x.io"), "https://x.io");
    }

    #[test]
    fn round_trips_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-auth.json");
        let mut store = McpAuthStore::default();
        store
            .servers
            .insert(key("https://mcp.example.com/mcp"), login());
        store.save_to(&path).unwrap();
        let back = McpAuthStore::load_from(&path);
        assert_eq!(back, store);
        assert!(back.login_for("https://MCP.example.com/mcp/").is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(login().is_expired(990));
        assert!(!login().is_expired(100));
    }
}
