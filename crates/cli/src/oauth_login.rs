use std::io::{self, BufRead, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use base64::Engine;
use dcode_ai_common::auth::{
    AntigravityAuth, AuthStore, CopilotAuth, LoggedProvider, OpenAiOAuth, OpenCodeZenOAuth,
    ProviderAuth,
};
use sha2::{Digest, Sha256};

pub const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_DEVICE_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const OPENAI_DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const OPENAI_CODE_EXCHANGE_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

const COPILOT_CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";
const COPILOT_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const COPILOT_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const ANTHROPIC_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const ANTHROPIC_SCOPES: &str = "org:create_api_key user:profile user:inference";
const ANTHROPIC_USER_AGENT: &str = "dcode-ai/0.1";

const ANTIGRAVITY_REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
const ANTIGRAVITY_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const ANTIGRAVITY_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const ANTIGRAVITY_DEFAULT_PROJECT_ID: &str = "rising-fact-p41fc";

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum OAuthProvider {
    Anthropic,
    Openai,
    Copilot,
    Antigravity,
    Opencodezen,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum LogoutTarget {
    Anthropic,
    Openai,
    Copilot,
    Antigravity,
    Opencodezen,
    All,
}

pub fn show_auth_status() -> anyhow::Result<()> {
    let store = AuthStore::load().unwrap_or_default();
    println!();
    println!("  ┌──────────────┬──────────────────────────────┐");
    println!("  │ Provider     │ Status                       │");
    println!("  ├──────────────┼──────────────────────────────┤");
    println!(
        "  │ anthropic    │ {} │",
        pad_status(if store.anthropic.is_some() {
            "✓ logged in"
        } else {
            "✗ not logged in"
        })
    );
    println!(
        "  │ openai       │ {} │",
        pad_status(if store.openai_oauth.is_some() {
            "✓ logged in"
        } else {
            "✗ not logged in"
        })
    );
    println!(
        "  │ copilot      │ {} │",
        pad_status(if store.copilot.is_some() {
            "✓ logged in"
        } else {
            "✗ not logged in"
        })
    );
    println!(
        "  │ antigravity  │ {} │",
        pad_status(if store.antigravity.is_some() {
            "✓ logged in"
        } else {
            "✗ not logged in"
        })
    );
    println!(
        "  │ opencodezen   │ {} │",
        pad_status(if store.opencodezen_oauth.is_some() {
            "✓ logged in"
        } else {
            "✗ not logged in"
        })
    );
    println!("  └──────────────┴──────────────────────────────┘");
    println!();
    Ok(())
}

fn pad_status(s: &str) -> String {
    let width = 28usize;
    if s.len() >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - s.len()))
    }
}

pub fn logout(target: LogoutTarget) -> anyhow::Result<()> {
    let mut store = AuthStore::load().unwrap_or_default();
    match target {
        LogoutTarget::Anthropic => {
            store.anthropic = None;
            if matches!(store.preferred_provider, Some(LoggedProvider::Anthropic)) {
                store.preferred_provider = None;
            }
        }
        LogoutTarget::Openai => {
            store.openai_oauth = None;
            if matches!(store.preferred_provider, Some(LoggedProvider::Openai)) {
                store.preferred_provider = None;
            }
        }
        LogoutTarget::Copilot => {
            store.copilot = None;
            if matches!(store.preferred_provider, Some(LoggedProvider::Copilot)) {
                store.preferred_provider = None;
            }
        }
        LogoutTarget::Antigravity => {
            store.antigravity = None;
            if matches!(store.preferred_provider, Some(LoggedProvider::Antigravity)) {
                store.preferred_provider = None;
            }
        }
        LogoutTarget::Opencodezen => {
            store.opencodezen_oauth = None;
            if matches!(store.preferred_provider, Some(LoggedProvider::Opencodezen)) {
                store.preferred_provider = None;
            }
        }
        LogoutTarget::All => {
            store.anthropic = None;
            store.openai_oauth = None;
            store.copilot = None;
            store.antigravity = None;
            store.opencodezen_oauth = None;
            store.preferred_provider = None;
        }
    }
    store.save()?;
    println!("Logged out.");
    Ok(())
}

pub async fn login(provider: OAuthProvider) -> anyhow::Result<()> {
    let store = AuthStore::load().unwrap_or_default();
    let already = match provider {
        OAuthProvider::Anthropic => store.anthropic.is_some(),
        OAuthProvider::Openai => store.openai_oauth.is_some(),
        OAuthProvider::Copilot => store.copilot.is_some(),
        OAuthProvider::Antigravity => store.antigravity.is_some(),
        OAuthProvider::Opencodezen => store.opencodezen_oauth.is_some(),
    };
    if already {
        println!("Already logged in. Use `dcode-ai logout ...` first if you want to re-login.");
        return Ok(());
    }

    match provider {
        OAuthProvider::Anthropic => login_anthropic().await,
        OAuthProvider::Openai => login_openai().await,
        OAuthProvider::Copilot => login_copilot().await,
        OAuthProvider::Antigravity => login_antigravity().await,
        OAuthProvider::Opencodezen => login_opencodezen().await,
    }
}

async fn login_anthropic() -> anyhow::Result<()> {
    let pkce = generate_pkce();
    let query = [
        ("code", "true"),
        ("response_type", "code"),
        ("client_id", ANTHROPIC_CLIENT_ID),
        ("redirect_uri", ANTHROPIC_REDIRECT_URI),
        ("scope", ANTHROPIC_SCOPES),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("state", &pkce.verifier),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", url_encode(v)))
    .collect::<Vec<_>>()
    .join("&");
    let auth_url = format!("{ANTHROPIC_AUTHORIZE_URL}?{query}");

    println!("\nAnthropic OAuth login\n");
    if !open_browser(&auth_url) {
        println!("Open this URL:\n{auth_url}\n");
    }
    print!("Paste authorization code: ");
    io::stdout().flush()?;
    let mut code = String::new();
    io::stdin().lock().read_line(&mut code)?;
    let code = code.trim();
    if code.is_empty() {
        bail!("No code provided");
    }

    let body = format!(
        "grant_type=authorization_code&code={}&code_verifier={}&client_id={}&redirect_uri={}&state={}",
        url_encode(code),
        url_encode(&pkce.verifier),
        url_encode(ANTHROPIC_CLIENT_ID),
        url_encode(ANTHROPIC_REDIRECT_URI),
        url_encode(&pkce.verifier),
    );
    let client = reqwest::Client::new();
    let resp = client
        .post(ANTHROPIC_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", ANTHROPIC_USER_AGENT)
        .body(body)
        .send()
        .await
        .context("anthropic token exchange")?;
    if !resp.status().is_success() {
        bail!(
            "Anthropic token exchange failed {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    let v: serde_json::Value = resp.json().await?;
    let token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing access_token"))?;
    let mut store = AuthStore::load().unwrap_or_default();
    store.anthropic = Some(ProviderAuth {
        token: token.to_string(),
        expires_at: None,
    });
    store.preferred_provider = Some(LoggedProvider::Anthropic);
    store.save()?;
    println!("Logged in to Anthropic ✓");
    Ok(())
}

async fn login_openai() -> anyhow::Result<()> {
    #[derive(serde::Deserialize)]
    struct DeviceCodeResp {
        device_auth_id: String,
        user_code: String,
        #[serde(default)]
        verification_uri: String,
        #[serde(default)]
        interval: Option<u64>,
    }
    #[derive(serde::Deserialize)]
    struct PollResp {
        authorization_code: Option<String>,
        code_verifier: Option<String>,
        error: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct TokenResp {
        access_token: String,
        refresh_token: String,
        expires_in: Option<u64>,
    }

    let client = reqwest::Client::new();
    let start = client
        .post(OPENAI_DEVICE_USERCODE_URL)
        .header("Accept", "application/json")
        .json(&serde_json::json!({"client_id": OPENAI_CLIENT_ID}))
        .send()
        .await
        .context("openai device start")?;
    if !start.status().is_success() {
        bail!(
            "OpenAI OAuth unavailable {}: {}",
            start.status(),
            start.text().await.unwrap_or_default()
        );
    }
    let mut data: DeviceCodeResp = start.json().await?;
    if data.verification_uri.is_empty() {
        data.verification_uri = "https://auth.openai.com/codex/device".to_string();
    }

    println!("\nOpenAI login");
    println!("  1. Open:  {}", data.verification_uri);
    println!("  2. Enter: {}", data.user_code);
    let _ = open_browser(&data.verification_uri);

    let cancel = Arc::new(tokio::sync::Notify::new());
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        cancel_clone.notify_one();
    });

    let mut interval = data.interval.unwrap_or(5).max(5);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
    let (auth_code, code_verifier) = loop {
        if tokio::time::Instant::now() > deadline {
            bail!("OpenAI login timed out");
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
            _ = cancel.notified() => bail!("Login cancelled"),
        }

        let r = client
            .post(OPENAI_DEVICE_TOKEN_URL)
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "device_auth_id": data.device_auth_id,
                "user_code": data.user_code,
            }))
            .send()
            .await?;
        if r.status().as_u16() == 403 || r.status().as_u16() == 404 {
            continue;
        }
        let p: PollResp = r.json().await?;
        if let Some(err) = p.error.as_deref() {
            match err {
                "authorization_pending" => continue,
                "slow_down" => {
                    interval += 5;
                    continue;
                }
                "expired_token" => bail!("Device code expired"),
                "access_denied" => bail!("Access denied"),
                _ => bail!("OAuth error: {err}"),
            }
        }
        if let (Some(a), Some(v)) = (p.authorization_code, p.code_verifier) {
            break (a, v);
        }
    };

    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencoding::encode(&auth_code),
        urlencoding::encode(OPENAI_REDIRECT_URI),
        urlencoding::encode(OPENAI_CLIENT_ID),
        urlencoding::encode(&code_verifier),
    );
    let ex = client
        .post(OPENAI_CODE_EXCHANGE_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .context("openai token exchange")?;
    if !ex.status().is_success() {
        bail!(
            "OpenAI token exchange failed {}: {}",
            ex.status(),
            ex.text().await.unwrap_or_default()
        );
    }
    let tok: TokenResp = ex.json().await?;
    let expires_at = tok
        .expires_in
        .map(|s| chrono::Utc::now().timestamp() + s as i64);

    let mut store = AuthStore::load().unwrap_or_default();
    store.openai_oauth = Some(OpenAiOAuth {
        access_token: tok.access_token,
        refresh_token: tok.refresh_token,
        expires_at,
    });
    store.preferred_provider = Some(LoggedProvider::Openai);
    store.save()?;
    println!("Logged in to OpenAI ✓");
    Ok(())
}

async fn login_copilot() -> anyhow::Result<()> {
    #[derive(serde::Deserialize)]
    struct DeviceResp {
        device_code: String,
        user_code: String,
        verification_uri: String,
        interval: Option<u64>,
    }
    #[derive(serde::Deserialize)]
    struct TokenResp {
        access_token: Option<String>,
        error: Option<String>,
        interval: Option<u64>,
    }

    let client = reqwest::Client::new();
    let start = client
        .post(COPILOT_DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .json(&serde_json::json!({"client_id": COPILOT_CLIENT_ID, "scope": "read:user"}))
        .send()
        .await?;
    if !start.status().is_success() {
        bail!("Copilot device-code failed: {}", start.status());
    }
    let d: DeviceResp = start.json().await?;
    println!("\nGitHub Copilot login");
    println!("  1. Open:  {}", d.verification_uri);
    println!("  2. Enter: {}", d.user_code);
    let _ = open_browser(&d.verification_uri);

    let cancel = Arc::new(tokio::sync::Notify::new());
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        cancel_clone.notify_one();
    });

    let mut secs = d.interval.unwrap_or(5);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
    let github_token = loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(secs) + Duration::from_secs(3)) => {}
            _ = cancel.notified() => bail!("Copilot login cancelled"),
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("Copilot login timed out");
        }
        let resp = client
            .post(COPILOT_ACCESS_TOKEN_URL)
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "client_id": COPILOT_CLIENT_ID,
                "device_code": d.device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("GitHub token poll failed: {}", resp.status());
        }
        let t: TokenResp = resp.json().await?;
        if let Some(token) = t.access_token {
            break token;
        }
        match t.error.as_deref() {
            Some("authorization_pending") => {
                if let Some(i) = t.interval {
                    secs = i;
                }
            }
            Some("slow_down") => secs += 5,
            Some(err) => bail!("GitHub auth error: {err}"),
            None => bail!("unexpected empty GitHub response"),
        }
    };

    let mut store = AuthStore::load().unwrap_or_default();
    store.copilot = Some(CopilotAuth {
        github_token,
        copilot_token: None,
        copilot_expires_at: None,
    });
    store.preferred_provider = Some(LoggedProvider::Copilot);
    store.save()?;
    println!("Logged in to Copilot ✓");
    Ok(())
}

async fn login_antigravity() -> anyhow::Result<()> {
    let client_id = std::env::var("DCODE_ANTIGRAVITY_CLIENT_ID")
        .context("Missing DCODE_ANTIGRAVITY_CLIENT_ID")?;
    let client_secret = std::env::var("DCODE_ANTIGRAVITY_CLIENT_SECRET")
        .context("Missing DCODE_ANTIGRAVITY_CLIENT_SECRET")?;

    let pkce = generate_pkce();
    let scopes = [
        "https://www.googleapis.com/auth/cloud-platform",
        "https://www.googleapis.com/auth/userinfo.email",
        "https://www.googleapis.com/auth/userinfo.profile",
        "https://www.googleapis.com/auth/cclog",
        "https://www.googleapis.com/auth/experimentsandconfigs",
    ]
    .join(" ");

    let auth_url = format!(
        "{ANTIGRAVITY_AUTH_URL}?client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&access_type=offline&prompt=consent",
        url_encode(&client_id),
        url_encode(ANTIGRAVITY_REDIRECT_URI),
        url_encode(&scopes),
        url_encode(&pkce.challenge),
        url_encode(&pkce.verifier)
    );

    println!("\nAntigravity login\n");
    if !open_browser(&auth_url) {
        println!("Open this URL:\n{auth_url}\n");
    }
    println!("Waiting for callback on http://localhost:51121/oauth-callback ...");

    let (code, state) = wait_for_callback().await?;
    if state != pkce.verifier {
        bail!("OAuth state mismatch");
    }

    let client = reqwest::Client::new();
    let params = [
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("code", code.as_str()),
        ("grant_type", "authorization_code"),
        ("redirect_uri", ANTIGRAVITY_REDIRECT_URI),
        ("code_verifier", pkce.verifier.as_str()),
    ];
    let resp = client
        .post(ANTIGRAVITY_TOKEN_URL)
        .form(&params)
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!(
            "Antigravity token exchange failed {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    let v: serde_json::Value = resp.json().await?;
    let access_token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing access_token"))?;
    let refresh_token = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing refresh_token"))?;
    let expires_in = v.get("expires_in").and_then(|x| x.as_i64()).unwrap_or(3600);
    let expires_at = chrono::Utc::now().timestamp() + expires_in - 300;

    let mut store = AuthStore::load().unwrap_or_default();
    store.antigravity = Some(AntigravityAuth {
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        expires_at: Some(expires_at),
        project_id: ANTIGRAVITY_DEFAULT_PROJECT_ID.to_string(),
        email: None,
    });
    store.preferred_provider = Some(LoggedProvider::Antigravity);
    store.save()?;
    println!("Logged in to Antigravity ✓");
    Ok(())
}

const OPENCODEZEN_CLIENT_ID: &str = "dcode-ai-cli";
const OPENCODEZEN_REDIRECT_URI: &str = "http://localhost:51122/oauth-callback";
const OPENCODEZEN_AUTH_URL: &str = "https://opencode.ai/auth/authorize";
const OPENCODEZEN_TOKEN_URL: &str = "https://opencode.ai/auth/token";

async fn login_opencodezen() -> anyhow::Result<()> {
    let pkce = generate_pkce();
    let auth_url = format!(
        "{OPENCODEZEN_AUTH_URL}?client_id={OPENCODEZEN_CLIENT_ID}&redirect_uri={}&response_type=code&scope=openid%20profile%20email&code_challenge={}&code_challenge_method=S256&state={}",
        url_encode(OPENCODEZEN_REDIRECT_URI),
        url_encode(&pkce.challenge),
        url_encode(&pkce.verifier)
    );

    println!("\nOpenCode Zen login");
    if !open_browser(&auth_url) {
        println!("Open this URL:\n{auth_url}\n");
    }
    println!("Waiting for callback on http://localhost:51122/oauth-callback ...");

    let (code, state) = wait_for_opencodezen_callback().await?;
    if state != pkce.verifier {
        bail!("OAuth state mismatch");
    }

    let client = reqwest::Client::new();
    let params = [
        ("client_id", OPENCODEZEN_CLIENT_ID),
        ("code", code.as_str()),
        ("grant_type", "authorization_code"),
        ("redirect_uri", OPENCODEZEN_REDIRECT_URI),
        ("code_verifier", pkce.verifier.as_str()),
    ];
    let resp = client
        .post(OPENCODEZEN_TOKEN_URL)
        .form(&params)
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!(
            "OpenCode Zen token exchange failed {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    let v: serde_json::Value = resp.json().await?;
    let access_token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing access_token"))?;
    let refresh_token = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing refresh_token"))?;
    let expires_in = v
        .get("expires_in")
        .and_then(|x| x.as_i64())
        .unwrap_or(86400);
    let expires_at = chrono::Utc::now().timestamp() + expires_in - 300;

    let mut store = AuthStore::load().unwrap_or_default();
    store.opencodezen_oauth = Some(OpenCodeZenOAuth {
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        expires_at: Some(expires_at),
    });
    store.preferred_provider = Some(LoggedProvider::Opencodezen);
    store.save()?;
    println!("Logged in to OpenCode Zen ✓");
    Ok(())
}

async fn wait_for_opencodezen_callback() -> anyhow::Result<(String, String)> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:51122").await?;
    let (mut stream, _) = listener.accept().await?;
    let (reader, mut writer) = stream.split();
    let mut buf_reader = BufReader::new(reader);
    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;

    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();
    let url = url::Url::parse(&format!("http://localhost{path}"))?;
    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| anyhow::anyhow!("missing code"))?;
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| anyhow::anyhow!("missing state"))?;

    let body =
        "<html><body><h2>Authentication complete.</h2><p>You can close this tab.</p></body></html>";
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = writer.write_all(resp.as_bytes()).await;
    Ok((code, state))
}

async fn wait_for_callback() -> anyhow::Result<(String, String)> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:51121").await?;
    let (mut stream, _) = listener.accept().await?;
    let (reader, mut writer) = stream.split();
    let mut buf_reader = BufReader::new(reader);
    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;

    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();
    let url = url::Url::parse(&format!("http://localhost{path}"))?;
    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| anyhow::anyhow!("missing code"))?;
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| anyhow::anyhow!("missing state"))?;

    let body =
        "<html><body><h2>Authentication complete.</h2><p>You can close this tab.</p></body></html>";
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = writer.write_all(resp.as_bytes()).await;
    Ok((code, state))
}

#[derive(Debug, Clone)]
struct PkcePair {
    verifier: String,
    challenge: String,
}

fn generate_pkce() -> PkcePair {
    let mut bytes = [0u8; 64];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut bytes);

    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    PkcePair {
        verifier,
        challenge,
    }
}

fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let cmd = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let cmd = std::process::Command::new("cmd")
        .args(["/c", "start", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let cmd: Result<_, _> = Err(std::io::Error::other("unsupported"));

    cmd.is_ok()
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
