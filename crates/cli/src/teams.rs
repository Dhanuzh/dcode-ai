//! Microsoft Teams: `@dcode-ai <question>` in a channel → an answer about
//! this project, from this machine (read-only).
//!
//! Teams can only reach a bot over public HTTPS, so this uses a Teams
//! *outgoing webhook* (a team owner adds it; no admin, no Azure) pointed at a
//! tunnel to dcode-ai (Tailscale Funnel, Cloudflare Tunnel or a Microsoft
//! dev tunnel):
//!
//! 1. Teams POSTs the mention, signed with the webhook's HMAC key; anything
//!    unsigned or wrongly signed is refused.
//! 2. Teams waits only 5 seconds, so dcode-ai replies "looking into it" at
//!    once and works on the answer.
//! 3. The answer is posted to the channel through a Teams *Workflows* webhook
//!    ("Post to a channel when a webhook request is received").
//!
//! Only the people you allow can ask. Settings in `~/.dcode-ai/teams.json`,
//! readable only by you.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub const DEFAULT_PORT: u16 = 8790;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamsConfig {
    /// The outgoing webhook's security token (base64), from Teams.
    pub hmac_token: String,
    /// The Workflows webhook URL that posts answers to the channel.
    pub post_url: String,
    /// Display names or Entra object ids of the people who may ask.
    pub allowed_users: Vec<String>,
    /// Local port the tunnel points at.
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

pub fn config_path() -> Option<PathBuf> {
    dcode_ai_common::config::home_dir().map(|h| h.join(".dcode-ai/teams.json"))
}

impl TeamsConfig {
    pub fn load() -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(config_path()?).ok()?).ok()
    }

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

    fn allows(&self, name: &str, aad: &str) -> bool {
        self.allowed_users.iter().any(|u| {
            let u = u.trim();
            (!aad.is_empty() && u.eq_ignore_ascii_case(aad))
                || (!name.is_empty() && u.eq_ignore_ascii_case(name.trim()))
        })
    }
}

/// HMAC-SHA256 (RFC 2104).
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(msg)
        .finalize();
    Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize()
        .into()
}

/// Whether `authorization` (`HMAC <base64>`) signs `body` with `token`.
pub fn verify(token_b64: &str, authorization: &str, body: &[u8]) -> bool {
    let Ok(key) = base64::engine::general_purpose::STANDARD.decode(token_b64.trim()) else {
        return false;
    };
    let Some(sig) = authorization.trim().strip_prefix("HMAC ") else {
        return false;
    };
    let Ok(sig) = base64::engine::general_purpose::STANDARD.decode(sig.trim()) else {
        return false;
    };
    let want = hmac_sha256(&key, body);
    // Constant time.
    sig.len() == want.len() && sig.iter().zip(want).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

/// The question in a Teams message: mentions and HTML removed.
pub fn question(html: &str) -> String {
    // Drop `<at>…</at>` mentions, then every other tag.
    let mut s = String::new();
    let mut rest = html;
    while let Some(i) = rest.find("<at>") {
        s.push_str(&rest[..i]);
        rest = match rest[i..].find("</at>") {
            Some(j) => &rest[i + j + 5..],
            None => "",
        };
    }
    s.push_str(rest);
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The Adaptive Card the Workflows webhook posts.
pub fn card(asker: &str, q: &str, answer: &str) -> serde_json::Value {
    let mut body = vec![
        serde_json::json!({"type": "TextBlock", "text": format!("**{asker}** asked: {q}"), "wrap": true, "weight": "Bolder"}),
    ];
    let text: String = answer.chars().take(20_000).collect();
    for para in text.split("\n\n").filter(|p| !p.trim().is_empty()) {
        body.push(serde_json::json!({"type": "TextBlock", "text": para.trim(), "wrap": true}));
    }
    body.push(serde_json::json!({"type": "TextBlock", "text": "— dcode-ai (read-only, from the code)", "isSubtle": true, "size": "Small"}));
    serde_json::json!({
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": {
                "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                "type": "AdaptiveCard",
                "version": "1.4",
                "body": body
            }
        }]
    })
}

async fn post_answer(url: &str, card: &serde_json::Value) -> Result<(), String> {
    let r = reqwest::Client::new()
        .post(url)
        .json(card)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err(format!("posting the answer: HTTP {}", r.status()))
    }
}

async fn reply(
    w: &mut tokio::net::tcp::OwnedWriteHalf,
    status: &str,
    text: &str,
) -> std::io::Result<()> {
    let body = serde_json::json!({"type": "message", "text": text}).to_string();
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    w.write_all(head.as_bytes()).await?;
    w.write_all(body.as_bytes()).await?;
    w.flush().await
}

/// What the session shows as it happens.
pub type Report = crate::slack::Report;

async fn handle(
    stream: tokio::net::TcpStream,
    cfg: Arc<TeamsConfig>,
    workspace: Arc<PathBuf>,
    report: Report,
    busy: Arc<tokio::sync::Semaphore>,
) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let method = line.split_whitespace().next().unwrap_or("").to_string();
    let mut len = 0usize;
    let mut auth = String::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).await? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap_or(0),
                "authorization" => auth = v.trim().to_string(),
                _ => {}
            }
        }
    }
    if method != "POST" || len == 0 || len > 256 * 1024 {
        return reply(&mut write, "400 Bad Request", "dcode-ai Teams webhook").await;
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    if !verify(&cfg.hmac_token, &auth, &body) {
        report("Teams: refused a request that wasn't signed by the webhook".into());
        return reply(&mut write, "401 Unauthorized", "not signed").await;
    }
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let name = v["from"]["name"].as_str().unwrap_or("").to_string();
    let aad = v["from"]["aadObjectId"].as_str().unwrap_or("").to_string();
    let q = question(v["text"].as_str().unwrap_or(""));
    if !cfg.allows(&name, &aad) {
        report(format!(
            "Teams: {name} isn't allowed — add them with `dcode-ai teams setup` (id {aad})"
        ));
        return reply(
            &mut write,
            "200 OK",
            "Sorry — I only answer the people allowed in `dcode-ai teams setup`.",
        )
        .await;
    }
    if q.is_empty() {
        return reply(
            &mut write,
            "200 OK",
            "Ask me something about the project, e.g. @dcode-ai where is the login handled?",
        )
        .await;
    }
    report(format!(
        "Teams question from {name}: {}",
        q.chars().take(80).collect::<String>()
    ));
    reply(
        &mut write,
        "200 OK",
        "👀 Looking into it — the answer will be posted in this channel.",
    )
    .await?;
    tokio::spawn(async move {
        let _permit = busy.acquire().await;
        let answer = crate::slack::answer(&workspace, &q).await;
        match post_answer(&cfg.post_url, &card(&name, &q, &answer)).await {
            Ok(()) => report("Teams: answered".into()),
            Err(e) => report(format!(
                "Teams: {e} — check the Workflows URL in `dcode-ai teams setup`"
            )),
        }
    });
    Ok(())
}

/// Listens on `127.0.0.1:<port>` (the tunnel's target) until dropped.
pub async fn serve(cfg: TeamsConfig, workspace: PathBuf, report: Report) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", cfg.port))
        .await
        .map_err(|e| format!("port {}: {e}", cfg.port))?;
    report(format!(
        "listening on http://127.0.0.1:{} — your tunnel must point here (see `dcode-ai teams setup`)",
        cfg.port
    ));
    let (cfg, workspace) = (Arc::new(cfg), Arc::new(workspace));
    let busy = Arc::new(tokio::sync::Semaphore::new(2));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let (cfg, workspace, report, busy) =
            (cfg.clone(), workspace.clone(), report.clone(), busy.clone());
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(10),
                handle(stream, cfg, workspace, report, busy),
            )
            .await;
        });
    }
}

fn ask(label: &str) -> String {
    use std::io::Write;
    print!("{label}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s.trim().to_string()
}

/// `dcode-ai teams setup`.
pub async fn setup() -> anyhow::Result<()> {
    let port = DEFAULT_PORT;
    println!("Teams reaches dcode-ai through an outgoing webhook and a tunnel to this machine.\n");
    println!("1. A tunnel to http://127.0.0.1:{port} with a stable https address — one of:");
    println!(
        "     tailscale funnel {port}                         (https://<machine>.<tailnet>.ts.net)"
    );
    println!(
        "     devtunnel host -p {port} --allow-anonymous      (Microsoft dev tunnels; create a persistent tunnel first)"
    );
    println!("     cloudflared tunnel run …                      (a named Cloudflare tunnel)");
    println!(
        "2. In Teams, as a team owner: the team's ⋯ → Manage team → Apps → Create an outgoing webhook."
    );
    println!(
        "   Name: dcode-ai · Callback URL: your tunnel's https address · then copy the security token it shows."
    );
    println!(
        "3. In the channel for answers: ⋯ → Workflows → \"Post to a channel when a webhook request is received\","
    );
    println!("   and copy the URL it gives.\n");
    let hmac_token = ask("Outgoing webhook security token: ");
    if base64::engine::general_purpose::STANDARD
        .decode(hmac_token.trim())
        .is_err()
    {
        anyhow::bail!("that doesn't look like the webhook's security token (base64)");
    }
    let post_url = ask("Workflows webhook URL: ");
    if !post_url.starts_with("https://") {
        anyhow::bail!("the Workflows URL starts with https://");
    }
    let users = ask("Who may ask — Teams display names or Entra object ids, comma-separated: ");
    let allowed_users: Vec<String> = users
        .split(',')
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .collect();
    if allowed_users.is_empty() {
        anyhow::bail!("at least one person is needed — nobody else can ask");
    }
    TeamsConfig {
        hmac_token,
        post_url,
        allowed_users,
        port,
    }
    .save()?;
    println!(
        "✓ saved (only you can read it). Start the tunnel, then in a session: /teams on — or run `dcode-ai teams serve`."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231() {
        // RFC 4231 test case 2.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn only_signed_requests_pass() {
        let key = b"0123456789abcdef0123456789abcdef";
        let token = base64::engine::general_purpose::STANDARD.encode(key);
        let body = br#"{"text":"<at>dcode-ai</at> hi"}"#;
        let sig = base64::engine::general_purpose::STANDARD.encode(hmac_sha256(key, body));
        assert!(verify(&token, &format!("HMAC {sig}"), body));
        assert!(!verify(&token, &format!("HMAC {sig}"), b"tampered"));
        assert!(!verify(&token, "Bearer x", body));
        assert!(!verify(&token, "", body));
    }

    #[test]
    fn questions_lose_mentions_and_html() {
        assert_eq!(
            question("<at>dcode-ai</at>&nbsp;where is <b>auth</b> handled?<br>"),
            "where is auth handled?"
        );
    }

    #[test]
    fn people_are_matched_by_name_or_id() {
        let c = TeamsConfig {
            allowed_users: vec!["Priya Rao".into(), "11111111-2222".into()],
            ..Default::default()
        };
        assert!(c.allows("priya rao", ""));
        assert!(c.allows("Someone", "11111111-2222"));
        assert!(!c.allows("Mallory", "999"));
    }

    #[test]
    fn answers_become_an_adaptive_card() {
        let c = card(
            "Priya",
            "where is auth?",
            "In `src/auth.rs`.\n\nIt checks tokens.",
        );
        let body = &c["attachments"][0]["content"]["body"];
        assert_eq!(body.as_array().unwrap().len(), 4);
        assert!(body[0]["text"].as_str().unwrap().contains("Priya"));
    }
}
