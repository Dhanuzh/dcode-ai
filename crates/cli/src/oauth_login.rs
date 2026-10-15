//! Sign-in flows. dcode-ai supports two providers:
//! - Claude: the user's own Claude Code login (`claude auth login`), or
//!   Anthropic Console OAuth via the official `ant` CLI for the API backend.
//! - GitHub Copilot: GitHub device-flow login, token stored in `auth.json`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use dcode_ai_common::auth::{AuthStore, CopilotAuth, LoggedProvider};

/// Default GitHub OAuth app for the Copilot device login. Override with
/// `[provider.copilot] oauth_client_id` or `DCODE_AI_COPILOT_CLIENT_ID` to
/// sign in through your own GitHub OAuth app.
const DEFAULT_COPILOT_CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";

fn copilot_client_id() -> String {
    std::env::var("DCODE_AI_COPILOT_CLIENT_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            dcode_ai_common::config::DcodeAiConfig::load()
                .ok()
                .and_then(|c| c.provider.copilot.oauth_client_id)
        })
        .unwrap_or_else(|| DEFAULT_COPILOT_CLIENT_ID.to_string())
}
const COPILOT_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const COPILOT_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

/// Anthropic sign-in is delegated to Anthropic's official `ant` CLI
/// (`ant auth login`), which owns the OAuth client, PKCE, token storage and
/// refresh. dcode-ai only drives it and reads short-lived access tokens.
const ANT_INSTALL_HINT: &str = "Anthropic sign-in needs Anthropic's `ant` CLI: https://github.com/anthropics/anthropic-cli/releases (macOS: `brew install anthropics/tap/ant`)";

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum OAuthProvider {
    Anthropic,
    Copilot,
}

#[derive(Debug, Clone)]
pub struct AnthropicLoginPrompt {
    pub authorization_url: String,
    pub code_verifier: String,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum LogoutTarget {
    Anthropic,
    Copilot,
    All,
}

pub fn show_auth_status() -> anyhow::Result<()> {
    let store = AuthStore::load().unwrap_or_default();
    let mark = |ok: bool| {
        if ok {
            "✓ signed in"
        } else {
            "✗ not signed in"
        }
    };
    let claude_via = if anthropic_uses_claude_code() {
        "Claude account (Claude Code)"
    } else {
        "API (ant OAuth)"
    };
    println!();
    println!("  ┌──────────────┬──────────────────────────────┐");
    println!("  │ Provider     │ Status                       │");
    println!("  ├──────────────┼──────────────────────────────┤");
    println!(
        "  │ claude       │ {} │",
        pad_status(mark(anthropic_ready()))
    );
    println!("  │   via        │ {} │", pad_status(claude_via));
    println!(
        "  │ copilot      │ {} │",
        pad_status(mark(store.copilot.is_some()))
    );
    println!("  └──────────────┴──────────────────────────────┘");
    println!();
    Ok(())
}

fn pad_status(s: &str) -> String {
    let width = 28usize;
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - len))
    }
}

pub fn logout(target: LogoutTarget) -> anyhow::Result<()> {
    logout_with_output(target, |line| println!("{line}"))
}

pub fn logout_with_output<F>(target: LogoutTarget, mut emit: F) -> anyhow::Result<()>
where
    F: FnMut(&str),
{
    let mut store = AuthStore::load().unwrap_or_default();
    if matches!(target, LogoutTarget::Anthropic | LogoutTarget::All) {
        if anthropic_uses_claude_code() {
            emit(
                "dcode-ai no longer uses Claude. Your Claude Code login is untouched (`claude auth logout` signs it out).",
            );
        } else {
            ant_logout(&mut emit);
        }
        store.anthropic = None;
        if matches!(store.preferred_provider, Some(LoggedProvider::Anthropic)) {
            store.preferred_provider = None;
        }
    }
    if matches!(target, LogoutTarget::Copilot | LogoutTarget::All) {
        store.copilot = None;
        if matches!(store.preferred_provider, Some(LoggedProvider::Copilot)) {
            store.preferred_provider = None;
        }
    }
    store.save()?;
    emit("Logged out.");
    Ok(())
}

/// Sign out of the `ant` OAuth profile dcode-ai uses for Anthropic.
fn ant_logout<F: FnMut(&str)>(emit: &mut F) {
    if !dcode_ai_common::provider_runtime::has_ant_cli() {
        return;
    }
    let ok = dcode_ai_common::provider_runtime::windows_compat_command("ant")
        .args(["auth", "logout"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        emit("warning: `ant auth logout` failed; run it manually to clear the Anthropic profile");
    }
}

pub async fn login(provider: OAuthProvider) -> anyhow::Result<()> {
    login_with_output(provider, |line| println!("{line}")).await
}

pub async fn login_with_output<F>(provider: OAuthProvider, mut emit: F) -> anyhow::Result<()>
where
    F: FnMut(&str),
{
    let already = match provider {
        OAuthProvider::Anthropic => anthropic_ready(),
        OAuthProvider::Copilot => AuthStore::load().unwrap_or_default().copilot.is_some(),
    };
    if already {
        emit("Already logged in. Use `dcode-ai logout ...` first if you want to re-login.");
        return Ok(());
    }
    match provider {
        OAuthProvider::Anthropic => login_anthropic(&mut emit).await,
        OAuthProvider::Copilot => login_copilot(&mut emit).await,
    }
}

fn anthropic_config() -> dcode_ai_common::config::AnthropicConfig {
    dcode_ai_common::config::DcodeAiConfig::load()
        .unwrap_or_default()
        .provider
        .anthropic
}

/// Signed in for the configured Claude backend (Claude Code account or `ant`).
pub fn anthropic_ready() -> bool {
    dcode_ai_common::provider_runtime::anthropic_signed_in(&anthropic_config())
}

pub fn anthropic_uses_claude_code() -> bool {
    anthropic_config().backend == dcode_ai_common::config::AnthropicBackend::ClaudeCode
}

async fn login_anthropic<F>(emit: &mut F) -> anyhow::Result<()>
where
    F: FnMut(&str),
{
    if anthropic_uses_claude_code() {
        return login_claude_code(emit).await;
    }
    login_anthropic_ant(emit).await
}

/// Claude account sign-in, owned entirely by Anthropic's own `claude` binary.
/// dcode-ai never sees the account token; it only runs `claude` later.
async fn login_claude_code<F>(emit: &mut F) -> anyhow::Result<()>
where
    F: FnMut(&str),
{
    if !dcode_ai_common::provider_runtime::has_claude_cli() {
        bail!(
            "Claude account sign-in needs Claude Code (`claude`) installed: https://claude.com/claude-code"
        );
    }
    emit("");
    emit("Claude account login (via `claude auth login`)");
    let mut cmd = dcode_ai_common::provider_runtime::windows_compat_command_tokio("claude");
    cmd.args(["auth", "login", "--claudeai"]);
    let status = cmd
        .status()
        .await
        .context("failed to run `claude auth login`")?;
    if !status.success() || !dcode_ai_common::provider_runtime::claude_code_signed_in() {
        bail!("`claude auth login` did not complete");
    }
    mark_anthropic_preferred()?;
    emit("Logged in to Claude ✓");
    Ok(())
}

async fn login_anthropic_ant<F>(emit: &mut F) -> anyhow::Result<()>
where
    F: FnMut(&str),
{
    if !dcode_ai_common::provider_runtime::has_ant_cli() {
        bail!("{ANT_INSTALL_HINT}");
    }
    emit("");
    emit("Anthropic OAuth login (via `ant auth login`)");
    // Interactive terminal: let `ant` own the whole flow (browser + callback).
    let mut cmd = dcode_ai_common::provider_runtime::windows_compat_command_tokio("ant");
    cmd.args(["auth", "login"]);
    let status = cmd
        .status()
        .await
        .context("failed to run `ant auth login`")?;
    if !status.success() {
        bail!("`ant auth login` failed ({status})");
    }
    mark_anthropic_preferred()?;
    emit("Logged in to Anthropic ✓");
    Ok(())
}

fn mark_anthropic_preferred() -> anyhow::Result<()> {
    let mut store = AuthStore::load().unwrap_or_default();
    // Any legacy minted API key is superseded by the OAuth profile.
    store.anthropic = None;
    store.preferred_provider = Some(LoggedProvider::Anthropic);
    store.save()?;
    Ok(())
}

/// A running `ant auth login --no-browser`, waiting for the user to paste the
/// code shown on the authorize page. Held between the TUI opening its login
/// modal and the user submitting the code. `kill_on_drop` ends an abandoned
/// attempt when a new one replaces it.
struct PendingAntLogin {
    child: tokio::process::Child,
    stdout: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
}

static PENDING_ANT_LOGIN: std::sync::Mutex<Option<PendingAntLogin>> = std::sync::Mutex::new(None);

/// Start `ant auth login --no-browser` and return the authorize URL it prints.
/// The process keeps running until [`finish_anthropic_login`] feeds it the code.
pub async fn begin_anthropic_login_prompt() -> anyhow::Result<AnthropicLoginPrompt> {
    use tokio::io::AsyncBufReadExt;

    if !dcode_ai_common::provider_runtime::has_ant_cli() {
        bail!("{ANT_INSTALL_HINT}");
    }
    let mut cmd = dcode_ai_common::provider_runtime::windows_compat_command_tokio("ant");
    cmd.args(["auth", "login", "--no-browser"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("failed to start `ant auth login`")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("`ant auth login` has no stdout"))?;
    let mut lines = tokio::io::BufReader::new(stdout).lines();

    let url = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(line) = lines.next_line().await? {
            if let Some(start) = line.find("https://") {
                return Ok::<_, std::io::Error>(Some(line[start..].trim().to_string()));
            }
        }
        Ok(None)
    })
    .await
    .context("timed out waiting for `ant auth login` to print the authorize URL")??
    .ok_or_else(|| anyhow::anyhow!("`ant auth login` exited without an authorize URL"))?;

    *PENDING_ANT_LOGIN.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingAntLogin {
        child,
        stdout: lines,
    });
    Ok(AnthropicLoginPrompt {
        authorization_url: url,
        code_verifier: String::new(),
    })
}

/// Feed the pasted authorization code to the pending `ant auth login`.
pub async fn finish_anthropic_login(_code_verifier: &str, raw_code: &str) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let code = raw_code.trim();
    if code.is_empty() {
        bail!("No code provided");
    }
    let pending = PENDING_ANT_LOGIN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let Some(PendingAntLogin {
        mut child,
        mut stdout,
    }) = pending
    else {
        bail!("no Anthropic login in progress; run /login anthropic again");
    };
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(code.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    }
    let mut transcript = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        while let Ok(Some(line)) = stdout.next_line().await {
            transcript.push(line);
        }
        child.wait_with_output().await
    })
    .await
    .context("timed out waiting for `ant auth login` to finish")??;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            transcript.join(" ")
        } else {
            stderr
        };
        bail!("Anthropic login failed: {detail}");
    }
    mark_anthropic_preferred()
}

async fn login_copilot<F>(emit: &mut F) -> anyhow::Result<()>
where
    F: FnMut(&str),
{
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

    let client_id = copilot_client_id();
    let client = reqwest::Client::new();
    let start = client
        .post(COPILOT_DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .json(&serde_json::json!({"client_id": client_id, "scope": "read:user"}))
        .send()
        .await?;
    if !start.status().is_success() {
        bail!("Copilot device-code failed: {}", start.status());
    }
    let d: DeviceResp = start.json().await?;
    emit("");
    emit("GitHub Copilot login");
    emit(&format!("  1. Open:  {}", d.verification_uri));
    emit(&format!("  2. Enter: {}", d.user_code));
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
                "client_id": client_id,
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
    emit("Logged in to Copilot ✓");
    Ok(())
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

pub fn try_open_browser(url: &str) -> bool {
    open_browser(url)
}
