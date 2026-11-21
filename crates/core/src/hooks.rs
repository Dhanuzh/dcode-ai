use dcode_ai_common::config::{HookCommand, HookConfig};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// Longest a hook may run; a hung hook mustn't hang the agent.
pub const HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Most output kept from a hook (a hook that prints without end is cut off).
const HOOK_OUTPUT_MAX: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct HookRunner {
    config: HookConfig,
}

#[derive(Debug, Clone, Copy)]
pub enum HookEventKind {
    SessionStart,
    SessionEnd,
    PreToolUse,
    PostToolUse,
    PostToolFailure,
    ApprovalRequested,
    SubagentStart,
    SubagentStop,
    UserPromptSubmit,
    Stop,
    PreCompact,
}

impl HookRunner {
    pub fn new(config: HookConfig) -> Self {
        Self { config }
    }

    pub fn has_any(&self) -> bool {
        !self.config.session_start.is_empty()
            || !self.config.session_end.is_empty()
            || !self.config.pre_tool_use.is_empty()
            || !self.config.post_tool_use.is_empty()
            || !self.config.post_tool_failure.is_empty()
            || !self.config.approval_requested.is_empty()
            || !self.config.subagent_start.is_empty()
            || !self.config.subagent_stop.is_empty()
            || !self.config.user_prompt_submit.is_empty()
            || !self.config.stop.is_empty()
            || !self.config.pre_compact.is_empty()
    }

    pub async fn run(
        &self,
        event: HookEventKind,
        matcher_value: Option<&str>,
        payload: &Value,
    ) -> Result<(), String> {
        for hook in self.matching_hooks(event, matcher_value) {
            run_hook_command(hook, payload).await?;
        }
        Ok(())
    }

    pub async fn run_best_effort(
        &self,
        event: HookEventKind,
        matcher_value: Option<&str>,
        payload: &Value,
    ) {
        if let Err(error) = self.run(event, matcher_value, payload).await {
            tracing::warn!("hook {:?} failed: {}", event, error);
        }
    }

    fn matching_hooks(
        &self,
        event: HookEventKind,
        matcher_value: Option<&str>,
    ) -> Vec<&HookCommand> {
        hooks_for_event(&self.config, event)
            .iter()
            .filter(|hook| hook_matches(hook.matcher.as_deref(), matcher_value))
            .collect()
    }
}

fn hooks_for_event(config: &HookConfig, event: HookEventKind) -> &[HookCommand] {
    match event {
        HookEventKind::SessionStart => &config.session_start,
        HookEventKind::SessionEnd => &config.session_end,
        HookEventKind::PreToolUse => &config.pre_tool_use,
        HookEventKind::PostToolUse => &config.post_tool_use,
        HookEventKind::PostToolFailure => &config.post_tool_failure,
        HookEventKind::ApprovalRequested => &config.approval_requested,
        HookEventKind::SubagentStart => &config.subagent_start,
        HookEventKind::SubagentStop => &config.subagent_stop,
        HookEventKind::UserPromptSubmit => &config.user_prompt_submit,
        HookEventKind::Stop => &config.stop,
        HookEventKind::PreCompact => &config.pre_compact,
    }
}

fn hook_matches(matcher: Option<&str>, value: Option<&str>) -> bool {
    match matcher.map(str::trim) {
        None | Some("") | Some("*") => true,
        Some(matcher) => value
            .map(|value| value == matcher || value.contains(matcher))
            .unwrap_or(false),
    }
}

async fn read_capped(mut r: impl AsyncRead + Unpin) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            // Keep reading past the cap (so the hook isn't blocked on a full
            // pipe) but keep only the start.
            Ok(n) if out.len() < HOOK_OUTPUT_MAX => {
                let take = n.min(HOOK_OUTPUT_MAX - out.len());
                out.extend_from_slice(&chunk[..take]);
            }
            Ok(_) => {}
        }
    }
    out
}

/// Runs one hook. Only a *blocking* hook can stop what it guards, and only
/// by exiting non-zero: one that fails to start, hangs past
/// [`HOOK_TIMEOUT`] or errors without `blocking` is logged and skipped.
async fn run_hook_command(hook: &HookCommand, payload: &Value) -> Result<(), String> {
    run_hook_command_with(hook, payload, HOOK_TIMEOUT).await
}

async fn run_hook_command_with(
    hook: &HookCommand,
    payload: &Value,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let fail = |reason: String| {
        if hook.blocking {
            Err(reason)
        } else {
            tracing::warn!("hook `{}`: {reason}", hook.command);
            Ok(())
        }
    };
    let mut command = dcode_ai_common::provider_runtime::system_shell_command(&hook.command);
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(err) => return fail(format!("couldn't start: {err}")),
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let input = serde_json::to_vec(payload).unwrap_or_default();
    let run = async {
        let write = async {
            if let Some(mut stdin) = stdin {
                // A hook may exit without reading its input (`exit 1`,
                // `true`); the broken pipe is not a failure — its exit
                // status decides.
                let _ = stdin.write_all(&input).await;
            }
        };
        let out = async {
            match stdout {
                Some(r) => read_capped(r).await,
                None => Vec::new(),
            }
        };
        let err = async {
            match stderr {
                Some(r) => read_capped(r).await,
                None => Vec::new(),
            }
        };
        let ((), out, err) = tokio::join!(write, out, err);
        (child.wait().await, out, err)
    };
    let (status, stdout, stderr) = match tokio::time::timeout(timeout, run).await {
        Ok(r) => r,
        Err(_) => return fail(format!("timed out after {}s", timeout.as_secs())),
    };
    let status = match status {
        Ok(s) => s,
        Err(err) => return fail(err.to_string()),
    };
    if status.success() || !hook.blocking {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&stdout).trim().to_string();
    let reason = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("hook command `{}` failed", hook.command)
    };
    Err(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hook(command: &str, blocking: bool) -> HookCommand {
        HookCommand {
            command: command.into(),
            matcher: None,
            blocking,
            origin: String::new(),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn blocking_prompt_hook_can_stop_a_turn() {
        let runner = HookRunner::new(HookConfig {
            user_prompt_submit: vec![hook("echo 'no secrets please' >&2; exit 2", true)],
            stop: vec![hook("exit 1", false)],
            ..Default::default()
        });
        assert!(runner.has_any());
        let err = runner
            .run(
                HookEventKind::UserPromptSubmit,
                None,
                &json!({"prompt": "x"}),
            )
            .await
            .unwrap_err();
        assert_eq!(err, "no secrets please");
        // Non-blocking hooks never fail the caller.
        assert!(
            runner
                .run(HookEventKind::Stop, None, &json!({}))
                .await
                .is_ok()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn broken_or_hung_hooks_dont_stop_the_agent() {
        let payload = json!({});
        let short = std::time::Duration::from_millis(300);
        // Non-blocking: a hung or missing hook is skipped.
        assert!(
            run_hook_command_with(&hook("sleep 5", false), &payload, short)
                .await
                .is_ok()
        );
        assert!(
            run_hook_command_with(&hook("/no/such/hook", false), &payload, short)
                .await
                .is_ok()
        );
        // Blocking: a hung hook fails (and is stopped) instead of hanging.
        let started = std::time::Instant::now();
        let err = run_hook_command_with(&hook("sleep 5", true), &payload, short)
            .await
            .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        // Endless output is read but not kept without limit.
        assert!(
            run_hook_command_with(&hook("yes | head -c 5000000", false), &payload, short * 10)
                .await
                .is_ok()
        );
    }
}
