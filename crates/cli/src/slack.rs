//! Slack: `@dcode-ai <question>` in a channel → an answer about this
//! project in the thread, from this machine.
//!
//! - Socket Mode: dcode-ai opens a websocket out to Slack, so nothing needs
//!   a public URL or an open port.
//! - Each question runs `dcode-ai --safe -p` (read-only: plan mode, no edits,
//!   no commands that change anything) in the project folder; secrets are
//!   masked in tool output as everywhere.
//! - Only the Slack users you allow can ask; anyone else is told no.
//! - Tokens live in `~/.dcode-ai/slack.json`, readable only by you.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SlackConfig {
    /// Bot token (`xoxb-…`): posts the answers.
    pub bot_token: String,
    /// App-level token (`xapp-…`, scope `connections:write`): Socket Mode.
    pub app_token: String,
    /// Slack member ids allowed to ask (`U…`).
    pub allowed_users: Vec<String>,
    /// The bot's own id (its mention is stripped from questions).
    #[serde(default)]
    pub bot_user_id: String,
}

pub fn config_path() -> Option<PathBuf> {
    dcode_ai_common::config::home_dir().map(|h| h.join(".dcode-ai/slack.json"))
}

impl SlackConfig {
    pub fn load() -> Option<Self> {
        let raw = std::fs::read_to_string(config_path()?).ok()?;
        serde_json::from_str(&raw).ok()
    }

    /// Saves it readable only by you.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path().ok_or_else(|| anyhow::anyhow!("no home folder"))?;
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            use std::io::Write;
            o.open(&tmp)?
                .write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        }
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

async fn api(
    token: &str,
    method: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("https://slack.com/api/{method}"))
        .bearer_auth(token)
        .json(&body)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    if v["ok"].as_bool() == Some(true) {
        Ok(v)
    } else {
        Err(format!(
            "slack {method}: {}",
            v["error"].as_str().unwrap_or("failed")
        ))
    }
}

/// Checks the bot token; returns (bot user id, workspace name).
pub async fn check_bot(token: &str) -> Result<(String, String), String> {
    let v = api(token, "auth.test", serde_json::json!({})).await?;
    Ok((
        v["user_id"].as_str().unwrap_or("").to_string(),
        v["team"].as_str().unwrap_or("").to_string(),
    ))
}

/// A question from a mention: the bot's mention removed, trimmed.
pub fn question(text: &str, bot_user_id: &str) -> String {
    let mention = format!("<@{bot_user_id}>");
    let mut t = text.replace(&mention, " ");
    // Any other leading mention of the bot (another id format).
    while let Some(start) = t.find("<@") {
        let Some(end) = t[start..].find('>') else {
            break;
        };
        if start > 0 {
            break;
        }
        t.replace_range(start..start + end + 1, " ");
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Splits an answer into Slack-sized messages.
pub fn chunks(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if cur.len() + line.len() + 1 > max && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        let mut line = line;
        while line.len() > max {
            let cut = line
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= max)
                .last()
                .unwrap_or(max);
            out.push(line[..cut].to_string());
            line = &line[cut..];
        }
        cur.push_str(line);
        cur.push('\n');
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Answers one question read-only in `workspace` with this binary.
pub async fn answer(workspace: &Path, q: &str) -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("dcode-ai"));
    let prompt = format!(
        "A teammate asks this about the project in Slack. Answer it by reading the code — briefly, in Slack-friendly Markdown (short paragraphs, bullets, `path:line` for code). Don't paste secrets or whole files.\n\nQuestion: {q}"
    );
    let out = tokio::process::Command::new(exe)
        .args(["--safe", "-p", &prompt])
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(600), out).await {
        Ok(Ok(o)) if o.status.success() => {
            let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if t.is_empty() {
                "(no answer)".into()
            } else {
                t
            }
        }
        Ok(Ok(o)) => format!(
            "Sorry — that failed: {}",
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .last()
                .unwrap_or("error")
        ),
        Ok(Err(e)) => format!("Sorry — couldn't start: {e}"),
        Err(_) => "Sorry — that took more than 10 minutes.".into(),
    }
}

/// Something to show in the session as it happens.
pub type Report = Arc<dyn Fn(String) + Send + Sync>;

/// Serves mentions until the task is dropped; reconnects when Slack asks.
pub async fn serve(cfg: SlackConfig, workspace: PathBuf, report: Report) -> Result<(), String> {
    let busy = Arc::new(tokio::sync::Semaphore::new(2));
    loop {
        let open = api(
            &cfg.app_token,
            "apps.connections.open",
            serde_json::json!({}),
        )
        .await?;
        let url = open["url"].as_str().ok_or("no socket url")?.to_string();
        let (ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .map_err(|e| format!("slack socket: {e}"))?;
        let (mut sink, mut stream) = ws.split();
        report("connected — mention the app in a channel it's in".into());
        while let Some(msg) = stream.next().await {
            let text = match msg {
                Ok(Message::Text(t)) => t.to_string(),
                Ok(Message::Ping(p)) => {
                    let _ = sink.send(Message::Pong(p)).await;
                    continue;
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => continue,
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            if let Some(id) = v["envelope_id"].as_str() {
                let _ = sink
                    .send(Message::Text(
                        serde_json::json!({"envelope_id": id}).to_string().into(),
                    ))
                    .await;
            }
            if v["type"] == "disconnect" {
                break;
            }
            let ev = &v["payload"]["event"];
            if v["type"] != "events_api" || ev["type"] != "app_mention" {
                continue;
            }
            let user = ev["user"].as_str().unwrap_or("").to_string();
            let channel = ev["channel"].as_str().unwrap_or("").to_string();
            let thread = ev["thread_ts"]
                .as_str()
                .or(ev["ts"].as_str())
                .unwrap_or("")
                .to_string();
            let q = question(ev["text"].as_str().unwrap_or(""), &cfg.bot_user_id);
            let (cfg, ws_root, report, busy) =
                (cfg.clone(), workspace.clone(), report.clone(), busy.clone());
            tokio::spawn(async move {
                let post = |text: String| {
                    let token = cfg.bot_token.clone();
                    let (channel, thread) = (channel.clone(), thread.clone());
                    async move {
                        let _ = api(
                            &token,
                            "chat.postMessage",
                            serde_json::json!({"channel": channel, "thread_ts": thread, "text": text}),
                        )
                        .await;
                    }
                };
                if !cfg.allowed_users.contains(&user) {
                    report(format!(
                        "Slack: a question from {user} (not allowed) was refused"
                    ));
                    post(
                        "Sorry — I only answer the people allowed in `dcode-ai slack setup`."
                            .into(),
                    )
                    .await;
                    return;
                }
                if q.is_empty() {
                    post("Ask me something about the project, e.g. `@dcode-ai where is the login handled?`".into()).await;
                    return;
                }
                report(format!(
                    "Slack question: {}",
                    q.chars().take(80).collect::<String>()
                ));
                post("👀 looking into it…".into()).await;
                let _permit = busy.acquire().await;
                let a = answer(&ws_root, &q).await;
                for part in chunks(&a, 3500) {
                    post(part).await;
                }
                report("Slack: answered".into());
            });
        }
        report("Slack connection closed — reconnecting".into());
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// The app manifest to paste at api.slack.com/apps → "Create New App" →
/// "From a manifest".
pub const MANIFEST: &str = r#"{
  "display_information": { "name": "dcode-ai" },
  "features": { "bot_user": { "display_name": "dcode-ai", "always_online": false } },
  "oauth_config": { "scopes": { "bot": ["app_mentions:read", "chat:write"] } },
  "settings": {
    "event_subscriptions": { "bot_events": ["app_mention"] },
    "socket_mode_enabled": true,
    "org_deploy_enabled": false
  }
}"#;

fn ask(label: &str) -> String {
    use std::io::Write;
    print!("{label}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s.trim().to_string()
}

/// `dcode-ai slack setup`: create the app, paste its tokens, say who may ask.
pub async fn setup() -> anyhow::Result<()> {
    println!(
        "1. Open https://api.slack.com/apps → Create New App → From a manifest, pick your workspace, paste:\n\n{MANIFEST}\n"
    );
    println!(
        "2. Install it to the workspace (Install App), then copy the Bot User OAuth Token (xoxb-…)."
    );
    println!(
        "3. Basic Information → App-Level Tokens → Generate, scope connections:write; copy it (xapp-…)."
    );
    println!("4. Invite the app to a channel: /invite @dcode-ai\n");
    let bot_token = ask("Bot token (xoxb-…): ");
    let app_token = ask("App-level token (xapp-…): ");
    if !bot_token.starts_with("xoxb-") || !app_token.starts_with("xapp-") {
        anyhow::bail!("the bot token starts with xoxb- and the app token with xapp-");
    }
    let (bot_user_id, team) = check_bot(&bot_token)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("✓ connected to {team}");
    let users = ask(
        "Slack member ids allowed to ask, comma-separated (your profile → ⋮ → Copy member ID): ",
    );
    let allowed_users: Vec<String> = users
        .split(',')
        .map(|u| u.trim().to_string())
        .filter(|u| u.starts_with('U') || u.starts_with('W'))
        .collect();
    if allowed_users.is_empty() {
        anyhow::bail!("at least one member id (U…) is needed — nobody else can ask");
    }
    SlackConfig {
        bot_token,
        app_token,
        allowed_users,
        bot_user_id,
    }
    .save()?;
    println!(
        "✓ saved (only you can read it). In a session: /slack on — or run `dcode-ai slack serve` in the project."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TLS websocket works (no crypto-provider panic). Network: --ignored.
    #[tokio::test]
    #[ignore]
    async fn secure_socket_connects() {
        let (mut ws, _) = tokio_tungstenite::connect_async("wss://ws.postman-echo.com/raw")
            .await
            .unwrap();
        ws.send(Message::Text("hi".into())).await.unwrap();
        let back = ws.next().await.unwrap().unwrap();
        assert_eq!(back.to_text().unwrap(), "hi");
    }

    #[test]
    fn mentions_become_questions() {
        assert_eq!(
            question("<@U0BOT> where is auth?", "U0BOT"),
            "where is auth?"
        );
        assert_eq!(
            question("hey <@U0BOT>  what does X do", "U0BOT"),
            "hey what does X do"
        );
        assert_eq!(question("<@UOTHER> explain", "U0BOT"), "explain");
    }

    #[test]
    fn long_answers_are_split() {
        let text = "a".repeat(5000) + "\nshort";
        let parts = chunks(&text, 3500);
        assert!(
            parts.iter().all(|p| p.len() <= 3501),
            "{:?}",
            parts.iter().map(|p| p.len()).collect::<Vec<_>>()
        );
        assert_eq!(parts.concat().replace('\n', ""), text.replace('\n', ""));
    }
}
