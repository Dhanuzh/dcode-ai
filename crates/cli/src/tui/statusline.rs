//! `[ui] statusline_command`: a user command whose first output line is
//! shown at the right of the footer (like Claude Code's `statusLine`).
//!
//! The command gets session JSON on stdin and is re-run at most once per
//! second, off the UI thread, with a 2-second timeout.
// Driven from the binary's REPL; the library build only compiles it.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use super::state::TuiSessionState;

const INTERVAL: Duration = Duration::from_secs(1);
const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CHARS: usize = 120;

/// What the command receives on stdin.
pub(crate) fn session_json(g: &TuiSessionState) -> Value {
    json!({
        "session_id": g.session_id,
        "cwd": g.workspace_root,
        "model": { "id": g.model },
        "permission_mode": g.permission_mode,
        "agent": g.agent_profile,
        "busy": g.busy,
        "tokens": { "input": g.input_tokens, "output": g.output_tokens, "context": g.context_tokens },
        "cost_usd": g.cost_usd,
    })
}

/// First non-empty output line, trimmed and length-capped.
pub(crate) fn first_line(stdout: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stdout);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(MAX_CHARS).collect())
}

async fn run_once(command: &str, input: Value) -> Option<String> {
    let mut cmd = dcode_ai_common::provider_runtime::system_shell_command(command);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.to_string().as_bytes()).await;
    }
    let out = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    first_line(&out.stdout)
}

/// Keep `statusline_text` fresh while the session runs.
pub(crate) fn spawn_poller(state: Arc<Mutex<TuiSessionState>>) {
    tokio::spawn(async move {
        loop {
            let snapshot = match state.lock() {
                Ok(g) if !g.should_exit => g
                    .statusline_command
                    .clone()
                    .map(|cmd| (cmd, session_json(&g))),
                _ => return,
            };
            let Some((command, input)) = snapshot else {
                return;
            };
            let text = run_once(&command, input).await;
            if let Ok(mut g) = state.lock() {
                g.statusline_text = text;
            }
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_skips_blanks_and_caps_length() {
        assert_eq!(
            first_line(b"\n  main ~ 12% \nmore"),
            Some("main ~ 12%".into())
        );
        assert_eq!(first_line(b"   \n"), None);
        assert_eq!(first_line(&[b'x'; 300]).unwrap().len(), MAX_CHARS);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_reads_session_json_from_stdin() {
        let out = run_once(
            "python3 -c 'import json,sys; print(json.load(sys.stdin)[\"model\"][\"id\"])'",
            json!({"model": {"id": "claude-x"}}),
        )
        .await;
        assert_eq!(out.as_deref(), Some("claude-x"));
    }
}
