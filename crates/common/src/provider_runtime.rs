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

/// The platform shell for running a one-liner: `sh -c` on Unix, `cmd /C` on
/// Windows. Used by hooks, validation, background exec, and inline `!cmd`.
pub fn system_shell_command(command_line: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.arg("/C").arg(command_line);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(command_line);
        cmd
    }
}

/// Blocking flavor of [`system_shell_command`].
pub fn system_shell_command_blocking(command_line: &str) -> std::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("cmd");
        cmd.arg("/C").arg(command_line);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(command_line);
        cmd
    }
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
