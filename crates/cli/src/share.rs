//! `/export` and `/share`: the conversation as clean Markdown with secrets
//! masked, saved to a file or posted as a secret GitHub gist (`gh`).

use std::sync::LazyLock;

use dcode_ai_common::message::{Message, Role};
use regex::Regex;

/// Secrets the general redactor can miss in a transcript: bearer tokens,
/// `key=value` style secrets and long hex keys in URLs.
static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9._~+/=\-]{12,}").expect("regex"),
            "${1}[redacted]",
        ),
        (
            Regex::new(
                r#"(?i)((?:api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|token|secret|password|passwd)["']?\s*[:=]\s*["']?)[^\s"'&,;]{6,}"#,
            )
            .expect("regex"),
            "${1}[redacted]",
        ),
        (
            Regex::new(r"(?i)([/=])[0-9a-f]{24,}").expect("regex"),
            "${1}[redacted]",
        ),
    ]
});

/// Mask secrets and the home folder in text that leaves this machine.
pub fn mask(text: &str) -> String {
    let mut out = dcode_ai_common::redact::redact_secrets(text);
    for (re, with) in PATTERNS.iter() {
        out = re.replace_all(&out, *with).into_owned();
    }
    if let Some(home) = dcode_ai_common::config::home_dir() {
        let home = home.display().to_string();
        if home.len() > 1 {
            out = out.replace(&home, "~");
        }
    }
    out
}

fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

/// The conversation as Markdown: your messages, the replies, and one line
/// per tool call. System prompt and tool output are left out.
pub fn markdown(title: &str, meta: &str, messages: &[Message]) -> String {
    let mut out = format!("# {}\n\n_{}_\n", mask(title), mask(meta));
    for m in messages {
        let text = m.content.to_summary_text();
        let text = text.trim();
        match m.role {
            Role::System | Role::Tool => {}
            Role::User => {
                if !text.is_empty() {
                    out.push_str(&format!("\n## You\n\n{}\n", mask(text)));
                }
            }
            Role::Assistant => {
                let calls = m.tool_calls.as_deref().unwrap_or_default();
                if text.is_empty() && calls.is_empty() {
                    continue;
                }
                out.push_str("\n## dcode-ai\n\n");
                if !text.is_empty() {
                    out.push_str(&mask(text));
                    out.push('\n');
                }
                for call in calls {
                    let args = call
                        .arguments
                        .get("command")
                        .or_else(|| call.arguments.get("path"))
                        .or_else(|| call.arguments.get("pattern"))
                        .and_then(|v| v.as_str())
                        .map(|s| one_line(s, 100))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "\n> ran `{}`{}\n",
                        call.name,
                        if args.is_empty() {
                            String::new()
                        } else {
                            format!(" — `{}`", mask(&args).replace('`', "'"))
                        }
                    ));
                }
            }
        }
    }
    out
}

/// Post `file` as a secret gist with `gh`; returns its URL.
pub fn gist(file: &std::path::Path, description: &str) -> Result<String, String> {
    let out = std::process::Command::new("gh")
        .args(["gist", "create", "--desc", description])
        .arg(file)
        .output()
        .map_err(|e| format!("could not run gh ({e}); install GitHub CLI and `gh auth login`"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let url = String::from_utf8_lossy(&out.stdout)
        .lines()
        .rev()
        .find(|l| l.starts_with("https://"))
        .unwrap_or_default()
        .trim()
        .to_string();
    if url.is_empty() {
        Err("gh did not return a gist URL".into())
    } else {
        Ok(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_masked() {
        let t = mask(
            "curl -H 'Authorization: Bearer abcdefghijklmnop1234' \
             https://zoho.example.com/mcp/8e7c7033177ff44819ca5aebfa4d1279/message \
             api_key=supersecretvalue password: hunter2hunter",
        );
        assert!(!t.contains("abcdefghijklmnop1234"), "{t}");
        assert!(!t.contains("8e7c7033177ff44819ca5aebfa4d1279"), "{t}");
        assert!(!t.contains("supersecretvalue"), "{t}");
        assert!(!t.contains("hunter2hunter"), "{t}");
        assert!(
            t.contains("https://zoho.example.com/mcp/[redacted]/message"),
            "{t}"
        );
    }

    #[test]
    fn markdown_has_turns_and_tool_lines_but_no_system_prompt() {
        let msgs = vec![
            Message::system("secret system prompt"),
            Message::user("fix the login bug"),
            Message::assistant_with_tool_calls(
                "Looking at it.",
                vec![dcode_ai_common::message::MessageToolCall {
                    id: "c1".into(),
                    name: "execute_bash".into(),
                    arguments: serde_json::json!({"command": "cargo test"}),
                }],
            ),
            Message::tool("c1", "huge output"),
            Message::assistant("Fixed."),
        ];
        let md = markdown("Fix login", "dcode-ai session", &msgs);
        assert!(md.starts_with("# Fix login"));
        assert!(md.contains("## You\n\nfix the login bug"));
        assert!(md.contains("> ran `execute_bash` — `cargo test`"));
        assert!(md.contains("Fixed."));
        assert!(!md.contains("secret system prompt") && !md.contains("huge output"));
    }
}
