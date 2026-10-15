//! Best-effort secret redaction for tool outputs.
//!
//! Two layers, applied at the agent's tool-result choke point (before results
//! reach model history, UI events, hooks, or logs):
//!
//! 1. **Known values** (exact match, no false positives): values of
//!    secret-named process env vars (`*KEY*`, `*TOKEN*`, …) and everything in
//!    the credentials store. Replaced with `[redacted:<NAME>]`.
//! 2. **Token shapes** (heuristic, no regex): well-known secret prefixes
//!    (`sk-`, `ghp_`, `AKIA`, …) followed by a long run of token characters,
//!    plus PEM private-key blocks.
//!
//! Redaction is defense in depth against accidental leaks (e.g. `env` output,
//! a read of an `.env` file, an HTTP dump) — not a guarantee against a
//! determined prompt; the model could still re-encode secrets it computes.

use std::sync::OnceLock;

/// Env-var name fragments that mark the value as a secret.
const NAME_MARKERS: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL"];
/// Shorter values are too collision-prone to exact-match ("true", "1234").
const MIN_SECRET_LEN: usize = 8;

/// Well-known secret prefixes. Longer/more specific first so `sk-ant-` labels
/// win over plain `sk-`.
const TOKEN_PREFIXES: &[&str] = &[
    "sk-ant-",
    "sk-",
    "github_pat_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
    "glpat-",
    "npm_",
    "AKIA",
    "AIza",
];
/// A prefix only counts as a token when the whole run is at least this long.
const MIN_TOKEN_LEN: usize = 20;

/// Redact secrets from arbitrary text (typically a tool's output or error).
pub fn redact_secrets(text: &str) -> String {
    let with_known = redact_known(text, known_secrets());
    redact_token_shapes(&redact_pem_blocks(&with_known))
}

/// Convenience: redact both output and error of a tool result in place.
pub fn redact_tool_result(result: &mut crate::tool::ToolResult) {
    if !result.output.is_empty() {
        result.output = redact_secrets(&result.output);
    }
    if let Some(error) = result.error.take() {
        result.error = Some(redact_secrets(&error));
    }
}

/// Snapshot of (label, value) secrets known to this process: secret-named env
/// vars plus the credentials store. Taken once — secrets don't change mid-run.
fn known_secrets() -> &'static [(String, String)] {
    static SECRETS: OnceLock<Vec<(String, String)>> = OnceLock::new();
    SECRETS.get_or_init(|| {
        let mut out: Vec<(String, String)> = Vec::new();
        for (name, value) in std::env::vars() {
            let upper = name.to_ascii_uppercase();
            if NAME_MARKERS.iter().any(|marker| upper.contains(marker))
                && value.trim().len() >= MIN_SECRET_LEN
            {
                out.push((name, value));
            }
        }
        for (name, value) in crate::credentials::all() {
            if value.trim().len() >= MIN_SECRET_LEN {
                out.push((name, value));
            }
        }
        // Longest value first so overlapping secrets redact fully.
        out.sort_by_key(|(_, value)| std::cmp::Reverse(value.len()));
        out
    })
}

/// Exact-match replacement of known secret values (pure, for testability).
fn redact_known(text: &str, secrets: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (name, value) in secrets {
        if out.contains(value.as_str()) {
            out = out.replace(value.as_str(), &format!("[redacted:{name}]"));
        }
    }
    out
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Replace `prefix + long token run` shapes with `[redacted:<prefix>…]`.
fn redact_token_shapes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        // A token can't start mid-word ("task-…" must not match "sk-").
        let boundary_ok = i == 0 || !token_char(chars[i - 1]);
        let matched = if boundary_ok {
            TOKEN_PREFIXES.iter().find(|prefix| {
                prefix
                    .chars()
                    .enumerate()
                    .all(|(k, pc)| chars.get(i + k) == Some(&pc))
            })
        } else {
            None
        };
        if let Some(prefix) = matched {
            let mut j = i;
            while j < chars.len() && token_char(chars[j]) {
                j += 1;
            }
            if j - i >= MIN_TOKEN_LEN {
                out.push_str("[redacted:");
                out.push_str(prefix);
                out.push_str("…]");
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Drop the body of PEM private-key blocks (BEGIN…END … PRIVATE KEY-----).
/// Public keys and certificates are left alone.
fn redact_pem_blocks(text: &str) -> String {
    if !text.contains("PRIVATE KEY-----") {
        return text.to_string();
    }
    let mut out: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if !in_block && trimmed.starts_with("-----BEGIN") && trimmed.ends_with("PRIVATE KEY-----") {
            in_block = true;
            out.push("[redacted:private-key]");
            continue;
        }
        if in_block {
            if trimmed.starts_with("-----END") && trimmed.ends_with("PRIVATE KEY-----") {
                in_block = false;
            }
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values_are_replaced_with_labels() {
        let secrets = vec![(
            "OPENAI_API_KEY".to_string(),
            "supersecretvalue123".to_string(),
        )];
        let out = redact_known("token is supersecretvalue123 here", &secrets);
        assert_eq!(out, "token is [redacted:OPENAI_API_KEY] here");
        // Short/absent values never fire.
        assert_eq!(redact_known("nothing here", &secrets), "nothing here");
    }

    #[test]
    fn token_shapes_are_redacted() {
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
        let out = redact_secrets(&format!("auth: {key} end"));
        assert_eq!(out, "auth: [redacted:sk-ant-…] end");

        let gh = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        assert_eq!(redact_secrets(gh), "[redacted:ghp_…]");
    }

    #[test]
    fn short_runs_and_mid_word_prefixes_are_left_alone() {
        // "task-runner" contains "sk-" mid-word; "sk-12" is far too short.
        let text = "the task-runner uses sk-12 as a label";
        assert_eq!(redact_secrets(text), text);
    }

    #[test]
    fn pem_private_key_blocks_are_dropped() {
        let text = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow…\nsecretline\n-----END RSA PRIVATE KEY-----\nafter";
        let out = redact_secrets(text);
        assert_eq!(out, "before\n[redacted:private-key]\nafter");
        // Public material passes through untouched.
        let pub_block = "-----BEGIN PUBLIC KEY-----\nabc\n-----END PUBLIC KEY-----";
        assert_eq!(redact_secrets(pub_block), pub_block);
    }
}
