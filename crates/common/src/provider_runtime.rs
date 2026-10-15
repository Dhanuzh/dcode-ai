use std::process::Stdio;
use std::sync::OnceLock;

static CLAUDE_CLI_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Build a `std::process::Command` for a CLI tool that may be installed as a
/// `.cmd`/`.bat` shim on Windows (npm globals like `claude`, the Google Cloud
/// SDK's `gcloud`, …). `CreateProcess` can't execute those shims directly, so
/// on Windows the program runs through `cmd /C`, which resolves `PATHEXT`.
pub fn windows_compat_command(program: &str) -> std::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("cmd");
        cmd.arg("/C").arg(program);
        cmd
    }
    #[cfg(not(windows))]
    std::process::Command::new(program)
}

/// Async flavor of [`windows_compat_command`].
pub fn windows_compat_command_tokio(program: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.arg("/C").arg(program);
        cmd
    }
    #[cfg(not(windows))]
    tokio::process::Command::new(program)
}

/// The platform shell for running a one-liner: `sh -c` on Unix; on Windows
/// the best available shell (Git Bash → PowerShell → cmd, see
/// [`crate::shell`]). Used by hooks, validation, background exec, and inline
/// `!cmd`, so every command the agent or user runs goes through the same
/// shell the system prompt describes.
pub fn system_shell_command(command_line: &str) -> tokio::process::Command {
    crate::shell::shell_command(command_line)
}

/// Blocking flavor of [`system_shell_command`].
pub fn system_shell_command_blocking(command_line: &str) -> std::process::Command {
    crate::shell::shell_command_blocking(command_line)
}

/// Whether local `claude` CLI is available in PATH.
///
/// This is cached for the process lifetime to avoid repeated subprocess probes.
pub fn has_claude_cli() -> bool {
    *CLAUDE_CLI_AVAILABLE.get_or_init(detect_claude_cli)
}

fn detect_claude_cli() -> bool {
    windows_compat_command("claude")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

static ANT_CLI_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Whether Anthropic's official `ant` CLI is available in PATH. dcode-ai uses
/// it as the Anthropic OAuth credential source (`ant auth login` /
/// `ant auth print-credentials --access-token`). Cached for the process.
pub fn has_ant_cli() -> bool {
    *ANT_CLI_AVAILABLE.get_or_init(|| {
        windows_compat_command("ant")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

/// Whether the `ant` CLI has a usable Anthropic OAuth session for `profile`
/// (the active profile when `None`). Asks `ant` for a token, which also
/// refreshes it when near expiry. `false` when `ant` is missing.
pub fn ant_signed_in(profile: Option<&str>) -> bool {
    if !has_ant_cli() {
        return false;
    }
    let mut cmd = windows_compat_command("ant");
    cmd.args(["auth", "print-credentials", "--access-token"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let Some(profile) = profile {
        cmd.env("ANTHROPIC_PROFILE", profile);
    }
    cmd.output()
        .map(|out| {
            let token = String::from_utf8_lossy(&out.stdout);
            out.status.success() && !token.trim().is_empty()
        })
        .unwrap_or(false)
}

/// Whether the local `claude` (Claude Code) binary reports a signed-in
/// account (`claude auth status --json` → `"loggedIn": true`).
pub fn claude_code_signed_in() -> bool {
    if !has_claude_cli() {
        return false;
    }
    windows_compat_command("claude")
        .args(["auth", "status", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|out| serde_json::from_slice::<serde_json::Value>(&out.stdout).ok())
        .and_then(|v| v.get("loggedIn").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// Whether the configured Claude backend has a usable sign-in.
pub fn anthropic_signed_in(config: &crate::config::AnthropicConfig) -> bool {
    match config.backend {
        crate::config::AnthropicBackend::ClaudeCode => claude_code_signed_in(),
        crate::config::AnthropicBackend::Api => ant_signed_in(config.oauth_profile.as_deref()),
    }
}

/// Cheap check (cached binary probe, no sign-in check) that the configured
/// Claude backend's CLI is installed.
pub fn anthropic_backend_available(config: &crate::config::AnthropicConfig) -> bool {
    match config.backend {
        crate::config::AnthropicBackend::ClaudeCode => has_claude_cli(),
        crate::config::AnthropicBackend::Api => has_ant_cli(),
    }
}
