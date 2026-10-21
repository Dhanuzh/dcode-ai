//! OAuth 2.1 discovery for remote (Streamable-HTTP) MCP servers.
//!
//! Follows the MCP authorization spec. A server that needs sign-in answers
//! `401` with `WWW-Authenticate: Bearer resource_metadata="…"`. The client
//! reads that protected-resource metadata (RFC 9728) to find the
//! authorization server, then reads the authorization server's own metadata
//! (RFC 8414). Login, token exchange and refresh build on [`Discovery`]; see
//! `docs/plans/2026-10-16-mcp-oauth.md`.

use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;

const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// The `Bearer` challenge of a `WWW-Authenticate` header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BearerChallenge {
    /// URL of the server's protected-resource metadata.
    pub resource_metadata: Option<String>,
    /// Space-separated scopes the server asks for, if it says.
    pub scope: Option<String>,
    pub error: Option<String>,
}

/// Parse a `WWW-Authenticate` value. It may hold several challenges (e.g.
/// `Basic realm="x", Bearer error="invalid_token"`) and quoted values may
/// contain commas. `None` when there is no `Bearer` challenge.
pub fn parse_www_authenticate(header: &str) -> Option<BearerChallenge> {
    let chars: Vec<char> = header.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut in_bearer = false;
    let mut found: Option<BearerChallenge> = None;
    while i < n {
        while i < n && (chars[i].is_whitespace() || chars[i] == ',') {
            i += 1;
        }
        let start = i;
        while i < n && !chars[i].is_whitespace() && chars[i] != ',' && chars[i] != '=' {
            i += 1;
        }
        let token: String = chars[start..i].iter().collect();
        if token.is_empty() {
            // A stray `=` (e.g. token68 padding); step over it.
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && chars[j].is_whitespace() {
            j += 1;
        }
        if j < n && chars[j] == '=' {
            // `name=value` auth-param of the current challenge.
            i = j + 1;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            let value = if i < n && chars[i] == '"' {
                i += 1;
                let mut v = String::new();
                while i < n && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < n {
                        i += 1;
                    }
                    v.push(chars[i]);
                    i += 1;
                }
                i += 1; // closing quote
                v
            } else {
                let s = i;
                while i < n && chars[i] != ',' && !chars[i].is_whitespace() {
                    i += 1;
                }
                chars[s..i].iter().collect()
            };
            if in_bearer && let Some(c) = found.as_mut() {
                match token.to_ascii_lowercase().as_str() {
                    "resource_metadata" => c.resource_metadata = Some(value),
                    "scope" => c.scope = Some(value),
                    "error" => c.error = Some(value),
                    _ => {}
                }
            }
        } else {
            // A bare token starts a new challenge (`Bearer`, `Basic`, …).
            in_bearer = token.eq_ignore_ascii_case("bearer");
            if in_bearer && found.is_none() {
                found = Some(BearerChallenge::default());
            }
        }
    }
    found
}

/// RFC 9728 protected-resource metadata (the fields dcode-ai uses).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResourceMetadata {
    pub resource: String,
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
}

/// RFC 8414 authorization-server metadata (the fields dcode-ai uses).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AuthServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
    #[serde(default)]
    pub grant_types_supported: Vec<String>,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Vec<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
}

impl AuthServerMetadata {
    /// Checks before trusting the metadata: it is the issuer we asked for
    /// (RFC 8414 §3.3), its endpoints are https, and it supports PKCE S256
    /// (the MCP spec requires refusing servers that do not advertise it).
    pub fn validate(&self, expected_issuer: &str) -> Result<(), String> {
        if self.issuer.trim_end_matches('/') != expected_issuer.trim_end_matches('/') {
            return Err(format!(
                "authorization-server metadata is for issuer `{}`, expected `{expected_issuer}`; refusing to sign in",
                self.issuer
            ));
        }
        check_secure_url(&self.authorization_endpoint, "authorization_endpoint")?;
        check_secure_url(&self.token_endpoint, "token_endpoint")?;
        for (url, what) in [
            (&self.registration_endpoint, "registration_endpoint"),
            (&self.revocation_endpoint, "revocation_endpoint"),
        ] {
            if let Some(url) = url {
                check_secure_url(url, what)?;
            }
        }
        if !self
            .code_challenge_methods_supported
            .iter()
            .any(|m| m == "S256")
        {
            return Err(format!(
                "authorization server `{}` does not advertise PKCE S256; refusing to sign in",
                self.issuer
            ));
        }
        Ok(())
    }
}

/// Everything needed to start a sign-in for one MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub server_url: String,
    pub resource: ResourceMetadata,
    pub auth_server: AuthServerMetadata,
    /// Scope the server asked for in its `401` challenge, if any.
    pub challenge_scope: Option<String>,
}

/// Find the authorization server for `server_url`. Uses the challenge's
/// `resource_metadata` URL first (when the caller has a `401`), then the
/// RFC 9728 well-known locations.
pub async fn discover(
    http: &reqwest::Client,
    server_url: &str,
    challenge: Option<&BearerChallenge>,
) -> Result<Discovery, String> {
    let mut urls: Vec<String> = challenge
        .and_then(|c| c.resource_metadata.clone())
        .into_iter()
        .collect();
    for url in resource_metadata_urls(server_url) {
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    if urls.is_empty() {
        return Err(format!("not an http(s) URL: {server_url}"));
    }
    let resource: ResourceMetadata = fetch_first(http, &urls)
        .await
        .map_err(|e| format!("could not read protected-resource metadata for {server_url}: {e}"))?;
    if !resource_matches(&resource.resource, server_url) {
        return Err(format!(
            "protected-resource metadata is for `{}`, not {server_url}; refusing to sign in",
            resource.resource
        ));
    }
    let issuer = resource
        .authorization_servers
        .first()
        .cloned()
        .ok_or_else(|| {
            format!("protected-resource metadata for {server_url} lists no authorization_servers")
        })?;
    check_secure_url(&issuer, "authorization server")?;
    let auth_server: AuthServerMetadata = fetch_first(http, &auth_server_metadata_urls(&issuer))
        .await
        .map_err(|e| format!("could not read authorization-server metadata for {issuer}: {e}"))?;
    auth_server.validate(&issuer)?;
    Ok(Discovery {
        server_url: server_url.to_string(),
        resource,
        auth_server,
        challenge_scope: challenge.and_then(|c| c.scope.clone()),
    })
}

/// RFC 9728 well-known locations for `server_url`'s metadata: the
/// path-specific one first, then the origin's.
pub fn resource_metadata_urls(server_url: &str) -> Vec<String> {
    let Some((origin, path)) = split_url(server_url) else {
        return Vec::new();
    };
    let base = format!("{origin}/.well-known/oauth-protected-resource");
    if path.is_empty() {
        vec![base]
    } else {
        vec![format!("{base}{path}"), base]
    }
}

/// Metadata locations for `issuer`, in the order the MCP spec gives: RFC 8414
/// (path inserted), OpenID Connect (inserted, then appended), and the
/// appended RFC 8414 form some servers still use.
pub fn auth_server_metadata_urls(issuer: &str) -> Vec<String> {
    let Some((origin, path)) = split_url(issuer) else {
        return Vec::new();
    };
    if path.is_empty() {
        return vec![
            format!("{origin}/.well-known/oauth-authorization-server"),
            format!("{origin}/.well-known/openid-configuration"),
        ];
    }
    vec![
        format!("{origin}/.well-known/oauth-authorization-server{path}"),
        format!("{origin}/.well-known/openid-configuration{path}"),
        format!("{origin}{path}/.well-known/openid-configuration"),
        format!("{origin}{path}/.well-known/oauth-authorization-server"),
    ]
}

/// Whether metadata advertising `resource` describes `server_url`: the same
/// URL or a parent path of it. Query strings (e.g. `?tools=all`), fragments
/// and trailing slashes are ignored; scheme and host are case-insensitive.
pub fn resource_matches(resource: &str, server_url: &str) -> bool {
    match (canonical(resource), canonical(server_url)) {
        (Some(r), Some(s)) => {
            s == r || s.strip_prefix(&r).is_some_and(|rest| rest.starts_with('/'))
        }
        _ => false,
    }
}

/// Sign-in endpoints must use https. Plain http is allowed only for
/// loopback hosts (local servers and tests).
pub fn check_secure_url(url: &str, what: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("http://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        // Drop userinfo so `http://localhost@evil.example` is not loopback.
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let hostname = match host.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or(""),
            None => host.split(':').next().unwrap_or(""),
        };
        if matches!(hostname, "127.0.0.1" | "localhost" | "::1") {
            return Ok(());
        }
    }
    Err(format!("{what} must use https: {url}"))
}

/// `(origin, path)` of an http(s) URL; the path has no query, fragment or
/// trailing `/`. `None` when there is no scheme or host.
fn split_url(url: &str) -> Option<(&str, &str)> {
    let scheme_end = url.find("://")? + 3;
    let host_end = url[scheme_end..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| scheme_end + i);
    if host_end == scheme_end {
        return None;
    }
    let after = &url[host_end..];
    let path_end = after.find(['?', '#']).unwrap_or(after.len());
    Some((&url[..host_end], after[..path_end].trim_end_matches('/')))
}

fn canonical(url: &str) -> Option<String> {
    let (origin, path) = split_url(url)?;
    Some(format!("{}{path}", origin.to_ascii_lowercase()))
}

/// First of `urls` that returns metadata; the error lists every attempt.
async fn fetch_first<T: DeserializeOwned>(
    http: &reqwest::Client,
    urls: &[String],
) -> Result<T, String> {
    let mut errors = Vec::new();
    for url in urls {
        match fetch_json(http, url).await {
            Ok(value) => return Ok(value),
            Err(e) => errors.push(e),
        }
    }
    Err(errors.join("; "))
}

async fn fetch_json<T: DeserializeOwned>(http: &reqwest::Client, url: &str) -> Result<T, String> {
    check_secure_url(url, "metadata URL")?;
    let resp = http
        .get(url)
        .header("accept", "application/json")
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("GET {url}: HTTP {status}"));
    }
    let text = resp.text().await.map_err(|e| format!("GET {url}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("GET {url}: invalid metadata JSON: {e}"))
}

// ── Login: PKCE, client registration, authorize URL, tokens ─────────────

/// PKCE pair (RFC 7636, S256).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

impl Pkce {
    pub fn from_verifier(verifier: &str) -> Self {
        use sha2::{Digest, Sha256};
        Self {
            verifier: verifier.to_string(),
            challenge: b64url(&Sha256::digest(verifier.as_bytes())),
        }
    }

    pub fn new() -> Self {
        Self::from_verifier(&random_token())
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

/// 32 random bytes, base64url (PKCE verifier, `state`).
pub fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    b64url(&bytes)
}

/// Register dcode-ai as a public client (RFC 7591) for `redirect_uri`.
pub async fn register_client(
    http: &reqwest::Client,
    auth_server: &AuthServerMetadata,
    redirect_uri: &str,
) -> Result<dcode_ai_common::mcp_auth::McpOAuthClient, String> {
    let endpoint = auth_server
        .registration_endpoint
        .as_deref()
        .ok_or_else(|| {
            format!(
                "authorization server `{}` does not offer client registration; \
             configure an Authorization header for this MCP server instead",
                auth_server.issuer
            )
        })?;
    let resp = http
        .post(endpoint)
        .timeout(FETCH_TIMEOUT)
        .json(&serde_json::json!({
            "client_name": "dcode-ai",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .await
        .map_err(|e| format!("client registration failed: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("client registration: bad response ({status}): {e}"))?;
    if !status.is_success() {
        return Err(format!("client registration rejected ({status}): {body}"));
    }
    let client_id = body["client_id"]
        .as_str()
        .ok_or("client registration returned no client_id")?
        .to_string();
    Ok(dcode_ai_common::mcp_auth::McpOAuthClient {
        client_id,
        client_secret: body["client_secret"].as_str().map(str::to_string),
        token_endpoint_auth_method: body["token_endpoint_auth_method"]
            .as_str()
            .unwrap_or("none")
            .to_string(),
        redirect_uri: redirect_uri.to_string(),
    })
}

/// The browser URL for the authorization-code + PKCE flow.
pub fn authorize_url(
    discovery: &Discovery,
    client: &dcode_ai_common::mcp_auth::McpOAuthClient,
    pkce: &Pkce,
    state: &str,
) -> String {
    let scope = discovery
        .challenge_scope
        .clone()
        .or_else(|| {
            let s = &discovery.resource.scopes_supported;
            (!s.is_empty()).then(|| s.join(" "))
        })
        .map(|mut s| {
            let refreshable = discovery
                .auth_server
                .scopes_supported
                .iter()
                .any(|x| x == "offline_access");
            if refreshable && !s.split(' ').any(|x| x == "offline_access") {
                s.push_str(" offline_access");
            }
            s
        });
    let mut params = vec![
        ("response_type", "code".to_string()),
        ("client_id", client.client_id.clone()),
        ("redirect_uri", client.redirect_uri.clone()),
        ("code_challenge", pkce.challenge.clone()),
        ("code_challenge_method", "S256".to_string()),
        ("state", state.to_string()),
        ("resource", discovery.resource.resource.clone()),
    ];
    if let Some(scope) = scope {
        params.push(("scope", scope));
    }
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect();
    let sep = if discovery.auth_server.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    format!(
        "{}{sep}{}",
        discovery.auth_server.authorization_endpoint,
        query.join("&")
    )
}

/// A token-endpoint response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    pub scope: Option<String>,
}

async fn token_request(
    http: &reqwest::Client,
    token_endpoint: &str,
    client: &dcode_ai_common::mcp_auth::McpOAuthClient,
    mut form: Vec<(&str, String)>,
) -> Result<Tokens, String> {
    let mut req = http.post(token_endpoint).timeout(FETCH_TIMEOUT);
    match (
        client.token_endpoint_auth_method.as_str(),
        &client.client_secret,
    ) {
        ("client_secret_basic", Some(secret)) => {
            req = req.basic_auth(&client.client_id, Some(secret));
        }
        ("client_secret_post", Some(secret)) => {
            form.push(("client_id", client.client_id.clone()));
            form.push(("client_secret", secret.clone()));
        }
        _ => form.push(("client_id", client.client_id.clone())),
    }
    let body: String = form
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let resp = req
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("token request failed: {e}"))?;
    let status = resp.status();
    let value: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("token response ({status}) is not JSON: {e}"))?;
    if !status.is_success() || value.get("error").is_some() {
        let err = value["error"].as_str().unwrap_or("unknown_error");
        let desc = value["error_description"].as_str().unwrap_or("");
        return Err(format!("token request rejected ({status}): {err} {desc}")
            .trim()
            .to_string());
    }
    let access_token = value["access_token"]
        .as_str()
        .ok_or("token response has no access_token")?
        .to_string();
    let now = chrono::Utc::now().timestamp();
    Ok(Tokens {
        access_token,
        refresh_token: value["refresh_token"].as_str().map(str::to_string),
        expires_at: value["expires_in"].as_i64().map(|s| now + s),
        scope: value["scope"].as_str().map(str::to_string),
    })
}

/// Exchange the authorization code from the redirect for tokens.
pub async fn exchange_code(
    http: &reqwest::Client,
    discovery: &Discovery,
    client: &dcode_ai_common::mcp_auth::McpOAuthClient,
    code: &str,
    pkce: &Pkce,
) -> Result<Tokens, String> {
    token_request(
        http,
        &discovery.auth_server.token_endpoint,
        client,
        vec![
            ("grant_type", "authorization_code".into()),
            ("code", code.to_string()),
            ("redirect_uri", client.redirect_uri.clone()),
            ("code_verifier", pkce.verifier.clone()),
            ("resource", discovery.resource.resource.clone()),
        ],
    )
    .await
}

/// New tokens from a refresh token. A response without a new refresh token
/// keeps the old one.
pub async fn refresh(
    http: &reqwest::Client,
    login: &dcode_ai_common::mcp_auth::McpServerLogin,
    resource: &str,
) -> Result<Tokens, String> {
    let refresh_token = login
        .refresh_token
        .clone()
        .ok_or("no refresh token stored")?;
    let mut tokens = token_request(
        http,
        &login.token_endpoint,
        &login.client,
        vec![
            ("grant_type", "refresh_token".into()),
            ("refresh_token", refresh_token.clone()),
            ("resource", resource.to_string()),
        ],
    )
    .await?;
    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(refresh_token);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use tiny_http::{Header, Response, Server, StatusCode};

    const ATLASSIAN_MCP: &str = "https://mcp.atlassian.com/v2/mcp";
    const ATLASSIAN_ISSUER: &str = "https://auth.atlassian.com/VCeDsk8ZHncYF1g234fKtc4lNipbBhu3";

    // Real responses fetched 2026-10-16 (resource scopes trimmed).
    const ATLASSIAN_RESOURCE_JSON: &str = r#"{"resource":"https://mcp.atlassian.com/v2/mcp","authorization_servers":["https://auth.atlassian.com/VCeDsk8ZHncYF1g234fKtc4lNipbBhu3"],"bearer_methods_supported":["header"],"scopes_supported":["read:me","offline_access","read:jira:agent-interface","read:confluence:agent-interface"],"resource_documentation":"https://www.atlassian.com/platform/remote-mcp-server"}"#;
    const ATLASSIAN_AS_JSON: &str = r#"{"authorization_endpoint":"https://auth.atlassian.com/authorize","client_id_metadata_document_supported":true,"code_challenge_methods_supported":["S256"],"grant_types_supported":["authorization_code","client_credentials","refresh_token","urn:ietf:params:oauth:grant-type:token-revoke","urn:ietf:params:oauth:grant-type:jwt-bearer","urn:ietf:params:oauth:grant-type:token-exchange"],"issuer":"https://auth.atlassian.com/VCeDsk8ZHncYF1g234fKtc4lNipbBhu3","jwks_uri":"https://auth.atlassian.com/.well-known/jwks.json","pushed_authorization_request_endpoint":"https://auth.atlassian.com/oauth/par","registration_endpoint":"https://auth.atlassian.com/VCeDsk8ZHncYF1g234fKtc4lNipbBhu3/dcr/register","response_types_supported":["code","token"],"revocation_endpoint":"https://auth.atlassian.com/oauth/revoke","token_endpoint":"https://auth.atlassian.com/oauth/token","token_endpoint_auth_methods_supported":["none","client_secret_post","client_secret_basic","private_key_jwt"]}"#;

    #[test]
    fn parses_the_atlassian_challenge() {
        let c = parse_www_authenticate(
            r#"Bearer resource_metadata="https://mcp.atlassian.com/.well-known/oauth-protected-resource/v2/mcp""#,
        )
        .expect("bearer challenge");
        assert_eq!(
            c.resource_metadata.as_deref(),
            Some("https://mcp.atlassian.com/.well-known/oauth-protected-resource/v2/mcp")
        );
        assert_eq!(c.scope, None);
    }

    #[test]
    fn parses_bearer_among_other_challenges_with_quoted_commas() {
        let c = parse_www_authenticate(
            r#"Basic realm="a, b", Negotiate abc==, bearer error="invalid_token", scope="read write", resource_metadata="https://x/.well-known/oauth-protected-resource/mcp", realm="q\"t""#,
        )
        .expect("bearer challenge");
        assert_eq!(c.error.as_deref(), Some("invalid_token"));
        assert_eq!(c.scope.as_deref(), Some("read write"));
        assert_eq!(
            c.resource_metadata.as_deref(),
            Some("https://x/.well-known/oauth-protected-resource/mcp")
        );
        // Unquoted values and parameters after another scheme are not mixed in.
        let c = parse_www_authenticate("Bearer error=insufficient_scope, Basic scope=nope")
            .expect("bearer challenge");
        assert_eq!(c.error.as_deref(), Some("insufficient_scope"));
        assert_eq!(c.scope, None);
    }

    #[test]
    fn no_bearer_challenge_is_none() {
        assert_eq!(parse_www_authenticate(r#"Basic realm="x""#), None);
        assert_eq!(parse_www_authenticate(""), None);
    }

    #[test]
    fn resource_metadata_urls_match_what_atlassian_advertises() {
        let urls = resource_metadata_urls("https://mcp.atlassian.com/v2/mcp?tools=all");
        assert_eq!(
            urls,
            vec![
                "https://mcp.atlassian.com/.well-known/oauth-protected-resource/v2/mcp",
                "https://mcp.atlassian.com/.well-known/oauth-protected-resource",
            ]
        );
        assert_eq!(
            resource_metadata_urls("https://example.com/"),
            vec!["https://example.com/.well-known/oauth-protected-resource"]
        );
        assert!(resource_metadata_urls("not a url").is_empty());
    }

    #[test]
    fn auth_server_metadata_urls_follow_the_spec_order() {
        let urls = auth_server_metadata_urls(ATLASSIAN_ISSUER);
        // The first one is the URL Atlassian answered with 200.
        assert_eq!(
            urls[0],
            "https://auth.atlassian.com/.well-known/oauth-authorization-server/VCeDsk8ZHncYF1g234fKtc4lNipbBhu3"
        );
        assert_eq!(urls.len(), 4);
        assert_eq!(
            urls[3],
            format!("{ATLASSIAN_ISSUER}/.well-known/oauth-authorization-server")
        );
        assert_eq!(
            auth_server_metadata_urls("https://as.example"),
            vec![
                "https://as.example/.well-known/oauth-authorization-server",
                "https://as.example/.well-known/openid-configuration",
            ]
        );
    }

    #[test]
    fn resource_must_be_the_server_or_a_parent_of_it() {
        assert!(resource_matches(
            ATLASSIAN_MCP,
            "https://mcp.atlassian.com/v2/mcp?tools=all"
        ));
        assert!(resource_matches(
            "https://MCP.atlassian.com/v2/mcp/",
            ATLASSIAN_MCP
        ));
        assert!(resource_matches("https://mcp.atlassian.com", ATLASSIAN_MCP));
        assert!(!resource_matches(
            "https://mcp.atlassian.com/v2/mc",
            ATLASSIAN_MCP
        ));
        assert!(!resource_matches(
            "https://evil.example/v2/mcp",
            ATLASSIAN_MCP
        ));
        assert!(!resource_matches(
            "http://mcp.atlassian.com/v2/mcp",
            ATLASSIAN_MCP
        ));
    }

    #[test]
    fn only_https_or_loopback_http_is_accepted() {
        assert!(check_secure_url("https://auth.atlassian.com/authorize", "x").is_ok());
        assert!(check_secure_url("http://127.0.0.1:8080/t", "x").is_ok());
        assert!(check_secure_url("http://localhost/t", "x").is_ok());
        assert!(check_secure_url("http://[::1]:9/t", "x").is_ok());
        assert!(check_secure_url("http://auth.example/t", "x").is_err());
        assert!(check_secure_url("http://localhost@evil.example/t", "x").is_err());
        assert!(check_secure_url("http://localhost.evil.example/t", "x").is_err());
        assert!(check_secure_url("ftp://auth.example", "x").is_err());
    }

    #[test]
    fn real_atlassian_metadata_parses_and_validates() {
        let resource: ResourceMetadata = serde_json::from_str(ATLASSIAN_RESOURCE_JSON).unwrap();
        assert!(resource_matches(&resource.resource, ATLASSIAN_MCP));
        assert_eq!(resource.authorization_servers, vec![ATLASSIAN_ISSUER]);
        assert!(
            resource
                .scopes_supported
                .iter()
                .any(|s| s == "offline_access")
        );

        let auth: AuthServerMetadata = serde_json::from_str(ATLASSIAN_AS_JSON).unwrap();
        auth.validate(ATLASSIAN_ISSUER).expect("valid");
        assert_eq!(
            auth.token_endpoint,
            "https://auth.atlassian.com/oauth/token"
        );
        assert!(auth.registration_endpoint.is_some());
        assert!(
            auth.grant_types_supported
                .iter()
                .any(|g| g == "refresh_token")
        );
    }

    #[test]
    fn metadata_for_another_issuer_or_without_s256_is_rejected() {
        let auth: AuthServerMetadata = serde_json::from_str(ATLASSIAN_AS_JSON).unwrap();
        let err = auth
            .validate("https://auth.atlassian.com/other")
            .unwrap_err();
        assert!(err.contains("expected"), "{err}");

        let mut no_pkce = auth.clone();
        no_pkce.code_challenge_methods_supported = vec!["plain".into()];
        let err = no_pkce.validate(ATLASSIAN_ISSUER).unwrap_err();
        assert!(err.contains("S256"), "{err}");

        let mut insecure = auth;
        insecure.token_endpoint = "http://auth.atlassian.com/oauth/token".into();
        let err = insecure.validate(ATLASSIAN_ISSUER).unwrap_err();
        assert!(err.contains("token_endpoint"), "{err}");
    }

    /// Serves `routes` (path → JSON body; anything else is 404) until idle
    /// for 5 s. Returns the log of requested paths, in order.
    fn serve(server: Server, routes: Vec<(String, String)>) -> Arc<Mutex<Vec<String>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        thread::spawn(move || {
            while let Ok(Some(request)) = server.recv_timeout(Duration::from_secs(5)) {
                let path = request.url().to_string();
                log.lock().unwrap().push(path.clone());
                let response = match routes.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => Response::from_string(body.clone()).with_header(
                        Header::from_bytes("Content-Type", "application/json").unwrap(),
                    ),
                    None => Response::from_string("not found").with_status_code(StatusCode(404)),
                };
                let _ = request.respond(response);
            }
        });
        seen
    }

    fn mock_server() -> (Server, String) {
        let server = Server::http("127.0.0.1:0").expect("start mock server");
        let base = match server.server_addr() {
            tiny_http::ListenAddr::IP(addr) => format!("http://{addr}"),
            other => panic!("unsupported listen addr: {other:?}"),
        };
        (server, base)
    }

    fn as_json(base: &str, issuer: &str) -> String {
        serde_json::json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "registration_endpoint": format!("{base}/register"),
            "code_challenge_methods_supported": ["S256"],
        })
        .to_string()
    }

    #[tokio::test]
    async fn discover_follows_the_challenge_then_rfc8414() {
        let (server, base) = mock_server();
        let issuer = format!("{base}/tenant");
        let resource = serde_json::json!({
            "resource": format!("{base}/mcp"),
            "authorization_servers": [issuer],
        })
        .to_string();
        let seen = serve(
            server,
            vec![
                ("/custom/prm".into(), resource),
                (
                    "/.well-known/oauth-authorization-server/tenant".into(),
                    as_json(&base, &issuer),
                ),
            ],
        );
        let challenge = BearerChallenge {
            resource_metadata: Some(format!("{base}/custom/prm")),
            scope: Some("read".into()),
            error: None,
        };
        let found = discover(
            &reqwest::Client::new(),
            &format!("{base}/mcp"),
            Some(&challenge),
        )
        .await
        .expect("discovery");
        assert_eq!(found.auth_server.issuer, issuer);
        assert_eq!(found.auth_server.token_endpoint, format!("{base}/token"));
        assert_eq!(found.challenge_scope.as_deref(), Some("read"));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                "/custom/prm",
                "/.well-known/oauth-authorization-server/tenant"
            ]
        );
    }

    #[tokio::test]
    async fn discover_without_a_challenge_falls_back_through_well_known_urls() {
        let (server, base) = mock_server();
        let issuer = format!("{base}/tenant");
        let resource = serde_json::json!({
            "resource": format!("{base}/v2/mcp"),
            "authorization_servers": [issuer],
        })
        .to_string();
        // Only the last-resort locations exist, so every earlier one is tried.
        let seen = serve(
            server,
            vec![
                ("/.well-known/oauth-protected-resource".into(), resource),
                (
                    "/tenant/.well-known/oauth-authorization-server".into(),
                    as_json(&base, &issuer),
                ),
            ],
        );
        let found = discover(
            &reqwest::Client::new(),
            &format!("{base}/v2/mcp?tools=all"),
            None,
        )
        .await
        .expect("discovery");
        assert_eq!(found.auth_server.issuer, issuer);
        assert_eq!(found.challenge_scope, None);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                "/.well-known/oauth-protected-resource/v2/mcp",
                "/.well-known/oauth-protected-resource",
                "/.well-known/oauth-authorization-server/tenant",
                "/.well-known/openid-configuration/tenant",
                "/tenant/.well-known/openid-configuration",
                "/tenant/.well-known/oauth-authorization-server",
            ]
        );
    }

    #[tokio::test]
    async fn discover_rejects_metadata_for_another_resource() {
        let (server, base) = mock_server();
        let resource = serde_json::json!({
            "resource": "https://other.example/mcp",
            "authorization_servers": [format!("{base}/tenant")],
        })
        .to_string();
        let seen = serve(
            server,
            vec![("/.well-known/oauth-protected-resource/mcp".into(), resource)],
        );
        let err = discover(&reqwest::Client::new(), &format!("{base}/mcp"), None)
            .await
            .unwrap_err();
        assert!(err.contains("other.example"), "{err}");
        // It stopped before asking any authorization server.
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    /// Live check against the real server; network, so never run by default:
    /// `cargo test -p dcode-ai-core --lib live_atlassian -- --ignored`.
    #[tokio::test]
    #[ignore = "network: talks to mcp.atlassian.com"]
    async fn live_atlassian_discovery() {
        let http = reqwest::Client::new();
        let resp = http
            .post(ATLASSIAN_MCP)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
            .send()
            .await
            .expect("reach mcp.atlassian.com");
        assert_eq!(resp.status(), 401);
        let header = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .expect("WWW-Authenticate header")
            .to_string();
        let challenge = parse_www_authenticate(&header).expect("bearer challenge");
        let found = discover(&http, ATLASSIAN_MCP, Some(&challenge))
            .await
            .expect("discovery");
        assert_eq!(found.auth_server.issuer, ATLASSIAN_ISSUER);
        assert!(found.auth_server.registration_endpoint.is_some());
    }

    #[tokio::test]
    async fn discover_reports_every_location_it_tried() {
        let (server, base) = mock_server();
        let _seen = serve(server, Vec::new());
        let err = discover(&reqwest::Client::new(), &format!("{base}/mcp"), None)
            .await
            .unwrap_err();
        assert!(
            err.contains("/.well-known/oauth-protected-resource/mcp"),
            "{err}"
        );
        assert!(err.contains("404"), "{err}");
    }

    #[test]
    fn pkce_challenge_is_base64url_sha256() {
        // Expected value computed independently with Python's hashlib.
        let p = Pkce::from_verifier("dcode-ai-pkce-test-verifier");
        assert_eq!(p.challenge, "mqH2hcKXfi-p4g629P9WOj0-gmekml4CiS-dC_uyNYA");
        assert_ne!(Pkce::new().verifier, Pkce::new().verifier);
    }

    #[tokio::test]
    async fn exchange_and_refresh_against_a_local_token_endpoint() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Two requests: code exchange, then refresh (no new refresh token).
        let server = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for reply in [
                r#"{"access_token":"at1","refresh_token":"rt1","expires_in":3600}"#,
                r#"{"access_token":"at2","expires_in":3600}"#,
            ] {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap();
                bodies.push(String::from_utf8_lossy(&buf[..n]).to_string());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
            bodies
        });
        let token_endpoint = format!("http://{addr}/token");
        let discovery = Discovery {
            server_url: "https://mcp.example.com/mcp".into(),
            resource: ResourceMetadata {
                resource: "https://mcp.example.com/mcp".into(),
                authorization_servers: vec![],
                scopes_supported: vec![],
            },
            auth_server: AuthServerMetadata {
                issuer: "https://auth.example.com".into(),
                authorization_endpoint: "https://auth.example.com/authorize".into(),
                token_endpoint: token_endpoint.clone(),
                registration_endpoint: None,
                revocation_endpoint: None,
                code_challenge_methods_supported: vec!["S256".into()],
                grant_types_supported: vec![],
                token_endpoint_auth_methods_supported: vec![],
                scopes_supported: vec![],
            },
            challenge_scope: Some("read".into()),
        };
        let client = dcode_ai_common::mcp_auth::McpOAuthClient {
            client_id: "cid".into(),
            client_secret: None,
            token_endpoint_auth_method: "none".into(),
            redirect_uri: "http://127.0.0.1:53682/callback".into(),
        };
        let pkce = Pkce::from_verifier("verifier-123");
        let url = authorize_url(&discovery, &client, &pkce, "st8");
        assert!(
            url.starts_with("https://auth.example.com/authorize?response_type=code"),
            "{url}"
        );
        assert!(url.contains("code_challenge_method=S256") && url.contains("state=st8"));
        assert!(url.contains("scope=read"), "{url}");

        let http = reqwest::Client::new();
        let t = exchange_code(&http, &discovery, &client, "the-code", &pkce)
            .await
            .unwrap();
        assert_eq!(t.access_token, "at1");
        let login = dcode_ai_common::mcp_auth::McpServerLogin {
            issuer: "https://auth.example.com".into(),
            token_endpoint,
            client,
            access_token: t.access_token,
            refresh_token: t.refresh_token,
            expires_at: t.expires_at,
            scope: None,
        };
        let t2 = refresh(&http, &login, "https://mcp.example.com/mcp")
            .await
            .unwrap();
        assert_eq!(t2.access_token, "at2");
        assert_eq!(
            t2.refresh_token.as_deref(),
            Some("rt1"),
            "old refresh token kept"
        );

        let bodies = server.await.unwrap();
        assert!(bodies[0].contains("grant_type=authorization_code"));
        assert!(bodies[0].contains("code_verifier=verifier-123"));
        assert!(bodies[0].contains("client_id=cid"));
        assert!(bodies[1].contains("grant_type=refresh_token"));
        assert!(bodies[1].contains("refresh_token=rt1"));
    }
}
