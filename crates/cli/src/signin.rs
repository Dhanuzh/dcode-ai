//! Sign-in expiry: warn before a sign-in lapses, so a scheduled run or a
//! morning session doesn't fail on it.
//!
//! - Claude through your `claude` (the default backend): `claude auth status
//!   --json` reports `loginExpiresAt`.
//! - GitHub Copilot: the stored GitHub token is checked against GitHub
//!   (valid, or revoked/expired).
//! - The `api` backend's `ant` refreshes its own tokens: not checked.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Warn this many days ahead.
pub const WARN_DAYS: i64 = 10;

/// One sign-in's state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signin {
    /// `Claude` or `GitHub Copilot`.
    pub provider: String,
    /// When it lapses, if known.
    pub expires: Option<DateTime<Utc>>,
    /// Set when it's already unusable (expired, revoked, signed out).
    pub problem: Option<String>,
    /// The command that renews it.
    pub renew: String,
}

/// `loginExpiresAt` from `claude auth status --json` output.
pub fn parse_claude_status(json: &str) -> Option<(bool, Option<DateTime<Utc>>)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let logged_in = v["loggedIn"].as_bool().unwrap_or(false);
    let expires = v["loginExpiresAt"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    Some((logged_in, expires))
}

/// Runs `claude auth status --json` (at most 10 s). `None` when `claude`
/// isn't there.
fn claude_status() -> Option<String> {
    let mut child = dcode_ai_common::provider_runtime::windows_compat_command("claude")
        .args(["auth", "status", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    use std::io::Read;
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

/// Claude's sign-in, through the `claude` binary.
pub fn claude() -> Option<Signin> {
    let raw = claude_status()?;
    let (logged_in, expires) = parse_claude_status(&raw)?;
    Some(Signin {
        provider: "Claude".into(),
        expires,
        problem: (!logged_in).then(|| "not signed in".to_string()),
        renew: "claude auth login".into(),
    })
}

/// Copilot's sign-in: is the stored GitHub token still accepted?
pub async fn copilot() -> Option<Signin> {
    let store = dcode_ai_common::auth::AuthStore::load().ok()?;
    let token = store.copilot?.github_token;
    let resp = reqwest::Client::new()
        .get("https://api.github.com/user")
        .header("Authorization", format!("token {token}"))
        .header("User-Agent", "dcode-ai")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    let problem = match resp.status().as_u16() {
        401 => Some("the GitHub sign-in was revoked or expired".to_string()),
        _ => None,
    };
    Some(Signin {
        provider: "GitHub Copilot".into(),
        expires: None,
        problem,
        renew: "dcode-ai login copilot".into(),
    })
}

/// The warning for a sign-in, if it needs one now.
pub fn warning(s: &Signin, now: DateTime<Utc>) -> Option<String> {
    if let Some(p) = &s.problem {
        return Some(format!("⚠ {} sign-in: {p} — run `{}`", s.provider, s.renew));
    }
    let exp = s.expires?;
    let left = exp - now;
    if left.num_seconds() <= 0 {
        return Some(format!(
            "⚠ {} sign-in expired on {} — run `{}`",
            s.provider,
            exp.with_timezone(&chrono::Local).format("%b %-d"),
            s.renew
        ));
    }
    if left.num_days() >= WARN_DAYS {
        return None;
    }
    let when = match left.num_days() {
        0 => "within a day".to_string(),
        1 => "tomorrow".to_string(),
        d => format!("in {d} days"),
    };
    Some(format!(
        "⚠ {} sign-in expires {when} ({}) — run `{}` to renew, so scheduled runs keep working",
        s.provider,
        exp.with_timezone(&chrono::Local).format("%b %-d %H:%M"),
        s.renew
    ))
}

/// A line for `dcode-ai doctor`.
pub fn doctor_line(s: &Signin, now: DateTime<Utc>) -> String {
    match (&s.problem, s.expires) {
        (Some(p), _) => format!("{}: {p} — `{}`", s.provider, s.renew),
        (None, Some(exp)) => {
            let days = (exp - now).num_days();
            format!(
                "{}: expires {} ({})",
                s.provider,
                exp.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"),
                if exp <= now {
                    "expired".to_string()
                } else if days == 0 {
                    "today".to_string()
                } else {
                    format!("in {days} days")
                }
            )
        }
        (None, None) => format!("{}: valid", s.provider),
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    checked: Option<DateTime<Utc>>,
    signins: Vec<Signin>,
}

fn cache_path() -> Option<PathBuf> {
    dcode_ai_common::config::home_dir().map(|h| h.join(".dcode-ai/signin-check.json"))
}

/// Every sign-in in use (Claude when the `claude-code` backend is, Copilot
/// when signed in), checked at most once a day unless `fresh`.
pub async fn check(config: &dcode_ai_common::config::DcodeAiConfig, fresh: bool) -> Vec<Signin> {
    let uses_claude =
        config.provider.anthropic.backend == dcode_ai_common::config::AnthropicBackend::ClaudeCode;
    let now = Utc::now();
    let path = cache_path();
    if !fresh
        && let Some(p) = &path
        && let Ok(raw) = std::fs::read_to_string(p)
        && let Ok(c) = serde_json::from_str::<Cache>(&raw)
        && c.checked
            .is_some_and(|t| now - t < chrono::Duration::hours(20))
    {
        return c.signins;
    }
    let mut out = Vec::new();
    if uses_claude && let Ok(Some(c)) = tokio::task::spawn_blocking(claude).await {
        out.push(c);
    }
    if let Some(c) = copilot().await {
        out.push(c);
    }
    if let Some(p) = path {
        let c = Cache {
            checked: Some(now),
            signins: out.clone(),
        };
        if let Ok(s) = serde_json::to_string(&c) {
            let _ = std::fs::write(p, s);
        }
    }
    out
}

/// The warnings to show now.
pub async fn warnings(config: &dcode_ai_common::config::DcodeAiConfig, fresh: bool) -> Vec<String> {
    let now = Utc::now();
    check(config, fresh)
        .await
        .iter()
        .filter_map(|s| warning(s, now))
        .collect()
}

/// From the last check (no waiting): a sign-in that can't work now — a
/// scheduled run is skipped with this instead of failing.
pub fn known_problem() -> Option<String> {
    let now = Utc::now();
    let raw = std::fs::read_to_string(cache_path()?).ok()?;
    let c: Cache = serde_json::from_str(&raw).ok()?;
    c.signins.iter().find_map(|s| {
        let dead = s.problem.is_some() || s.expires.is_some_and(|e| e <= now);
        dead.then(|| warning(s, now).unwrap_or_default())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn reads_claudes_status() {
        let (on, exp) = parse_claude_status(
            r#"{"loggedIn": true, "authMethod": "claude.ai", "loginExpiresAt": "2026-11-17T19:33:43.069Z"}"#,
        )
        .unwrap();
        assert!(on);
        assert_eq!(exp, Some(at("2026-11-17T19:33:43.069Z")));
        assert_eq!(
            parse_claude_status(r#"{"loggedIn": false}"#),
            Some((false, None))
        );
        assert_eq!(parse_claude_status("not json"), None);
    }

    #[test]
    fn warns_only_inside_ten_days() {
        let s = |exp: &str| Signin {
            provider: "Claude".into(),
            expires: Some(at(exp)),
            problem: None,
            renew: "claude auth login".into(),
        };
        let now = at("2026-11-12T10:00:00Z");
        assert!(warning(&s("2026-12-30T00:00:00Z"), now).is_none());
        let w = warning(&s("2026-11-17T19:33:43Z"), now).unwrap();
        assert!(
            w.contains("in 5 days") && w.contains("claude auth login"),
            "{w}"
        );
        assert!(
            warning(&s("2026-11-13T09:00:00Z"), now)
                .unwrap()
                .contains("within a day")
        );
        assert!(
            warning(&s("2026-11-01T00:00:00Z"), now)
                .unwrap()
                .contains("expired")
        );
        let revoked = Signin {
            provider: "GitHub Copilot".into(),
            expires: None,
            problem: Some("revoked".into()),
            renew: "dcode-ai login copilot".into(),
        };
        assert!(
            warning(&revoked, now)
                .unwrap()
                .contains("dcode-ai login copilot")
        );
        assert!(doctor_line(&s("2026-11-17T19:33:43Z"), now).contains("in 5 days"));
    }
}
