use std::process::Stdio;
use std::sync::OnceLock;

static CLAUDE_CLI_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Whether local `claude` CLI is available in PATH.
///
/// This is cached for the process lifetime to avoid repeated subprocess probes.
pub fn has_claude_cli() -> bool {
    *CLAUDE_CLI_AVAILABLE.get_or_init(detect_claude_cli)
}

fn detect_claude_cli() -> bool {
    std::process::Command::new("claude")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
