//! Detection of the shell used to run agent and system one-liner commands.
//!
//! On Unix this is always `sh -c`. On Windows the historical `cmd /C` choice
//! made the `execute_bash` tool a trap: models write POSIX commands, `cmd`
//! mangles them, and the fallback habit (`echo … >> file`) corrupts files.
//! Windows now prefers, in order:
//!
//! 1. `DCODE_AI_SHELL` — explicit override (absolute path to a shell binary;
//!    kind inferred from the file name).
//! 2. Git Bash (`bash.exe` from a Git for Windows install) — POSIX commands
//!    work as-is, matching the tool's name and the model's training bias.
//! 3. PowerShell (`pwsh.exe`, then `powershell.exe`).
//! 4. `cmd.exe` as the last resort.
//!
//! WSL's `C:\Windows\System32\bash.exe` is deliberately excluded: it boots a
//! Linux VM with a different filesystem view, which is never what a workspace
//! command wants.

#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// POSIX `sh` (Unix).
    Sh,
    /// Git for Windows bash.
    GitBash,
    /// PowerShell 7 (`pwsh`) or Windows PowerShell 5.1.
    PowerShell,
    /// Legacy `cmd.exe`.
    Cmd,
}

#[derive(Debug, Clone)]
pub struct ShellInfo {
    pub kind: ShellKind,
    /// Program to invoke (absolute path on Windows when detected, otherwise a
    /// PATH-resolved name).
    pub program: PathBuf,
}

impl ShellInfo {
    /// Human-readable name for status lines and the system prompt.
    pub fn display_name(&self) -> &'static str {
        match self.kind {
            ShellKind::Sh => "sh",
            ShellKind::GitBash => "bash (Git Bash)",
            ShellKind::PowerShell => "PowerShell",
            ShellKind::Cmd => "cmd.exe",
        }
    }

    /// System-prompt guidance for writing commands against this shell.
    pub fn prompt_hint(&self) -> &'static str {
        match self.kind {
            ShellKind::Sh => "",
            ShellKind::GitBash => {
                "\n- The shell is Git Bash on Windows: standard Unix commands (ls, cat, grep, \
                 sed, find) work. Use forward slashes in paths; `C:\\foo` is `/c/foo`."
            }
            ShellKind::PowerShell => {
                "\n- The shell is PowerShell: use PowerShell syntax — `Get-ChildItem` (ls), \
                 `Get-Content` (cat), `Select-String` (grep), `$env:VAR` (not `$VAR` or \
                 `%VAR%`). Do NOT use bash/sh syntax or heredocs."
            }
            ShellKind::Cmd => {
                "\n- The shell is cmd.exe: use Windows commands — `dir` not `ls`, `type` not \
                 `cat`, `findstr` not `grep`, `%VAR%` not `$VAR`."
            }
        }
    }
}

/// The detected workspace shell, probed once per process.
pub fn workspace_shell() -> &'static ShellInfo {
    static SHELL: OnceLock<ShellInfo> = OnceLock::new();
    SHELL.get_or_init(detect_shell)
}

/// Build a tokio command running `command_line` in the detected shell.
pub fn shell_command(command_line: &str) -> tokio::process::Command {
    let shell = workspace_shell();
    let mut cmd = tokio::process::Command::new(&shell.program);
    apply_shell_args(shell.kind, command_line, |a| {
        cmd.arg(a);
    });
    cmd
}

/// Blocking flavor of [`shell_command`].
pub fn shell_command_blocking(command_line: &str) -> std::process::Command {
    let shell = workspace_shell();
    let mut cmd = std::process::Command::new(&shell.program);
    apply_shell_args(shell.kind, command_line, |a| {
        cmd.arg(a);
    });
    cmd
}

fn apply_shell_args(kind: ShellKind, command_line: &str, mut push: impl FnMut(&str)) {
    match kind {
        ShellKind::Sh => {
            push("-c");
            push(command_line);
        }
        ShellKind::GitBash => {
            // Plain `-c` (not `-lc`): login shells source user profiles, which
            // is slow and can chdir or print banners into tool output.
            push("-c");
            push(command_line);
        }
        ShellKind::PowerShell => {
            push("-NoProfile");
            push("-NonInteractive");
            push("-Command");
            // PowerShell's `-Command` exit code reflects script errors, not the
            // last native command's status; propagate it explicitly.
            // `exit $LASTEXITCODE` coerces $null to 0 when no native ran.
            push(&format!("{command_line}\nexit $LASTEXITCODE"));
        }
        ShellKind::Cmd => {
            push("/C");
            push(command_line);
        }
    }
}

#[cfg(not(windows))]
fn detect_shell() -> ShellInfo {
    if let Some(overridden) = shell_override() {
        return overridden;
    }
    ShellInfo {
        kind: ShellKind::Sh,
        program: PathBuf::from("sh"),
    }
}

#[cfg(windows)]
fn detect_shell() -> ShellInfo {
    if let Some(overridden) = shell_override() {
        return overridden;
    }
    if let Some(bash) = find_git_bash() {
        return ShellInfo {
            kind: ShellKind::GitBash,
            program: bash,
        };
    }
    if let Some(ps) = find_in_path("pwsh.exe").or_else(|| find_in_path("powershell.exe")) {
        return ShellInfo {
            kind: ShellKind::PowerShell,
            program: ps,
        };
    }
    ShellInfo {
        kind: ShellKind::Cmd,
        program: PathBuf::from("cmd"),
    }
}

/// `DCODE_AI_SHELL=<path>` override; the kind is inferred from the file name.
fn shell_override() -> Option<ShellInfo> {
    let raw = std::env::var("DCODE_AI_SHELL").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let program = PathBuf::from(raw);
    let name = program
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = match name.as_str() {
        "bash" => {
            if cfg!(windows) {
                ShellKind::GitBash
            } else {
                ShellKind::Sh
            }
        }
        "pwsh" | "powershell" => ShellKind::PowerShell,
        "cmd" => ShellKind::Cmd,
        _ => ShellKind::Sh,
    };
    Some(ShellInfo { kind, program })
}

/// Locate Git for Windows bash: derived from `git.exe` on PATH first, then
/// well-known install directories. Never returns WSL's System32 bash.
#[cfg(windows)]
fn find_git_bash() -> Option<PathBuf> {
    // Standard layout: <root>\cmd\git.exe (or <root>\bin\git.exe) with
    // <root>\bin\bash.exe alongside.
    if let Some(git) = find_in_path("git.exe")
        && let Some(parent) = git.parent()
        && let Some(root) = parent.parent()
    {
        let bash = root.join("bin").join("bash.exe");
        if is_usable_bash(&bash) {
            return Some(bash);
        }
        let bash = root.join("usr").join("bin").join("bash.exe");
        if is_usable_bash(&bash) {
            return Some(bash);
        }
    }

    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(p) = std::env::var(var) {
            roots.push(Path::new(&p).join("Git"));
        }
    }
    if let Ok(p) = std::env::var("LOCALAPPDATA") {
        roots.push(Path::new(&p).join("Programs").join("Git"));
    }
    for root in roots {
        let bash = root.join("bin").join("bash.exe");
        if is_usable_bash(&bash) {
            return Some(bash);
        }
    }
    None
}

/// A bash we're willing to use: exists and is not the WSL launcher in
/// System32 (which runs commands inside a Linux distro, not this workspace).
#[cfg(windows)]
fn is_usable_bash(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let lower = path.to_string_lossy().to_ascii_lowercase();
    !lower.contains("\\system32\\") && !lower.contains("\\windows\\")
}

/// Minimal PATH search for an executable name (no PATHEXT expansion — callers
/// pass the full file name).
#[cfg(windows)]
fn find_in_path(file_name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_args_propagate_native_exit_code() {
        let mut args: Vec<String> = Vec::new();
        apply_shell_args(ShellKind::PowerShell, "cargo build", |a| {
            args.push(a.to_string());
        });
        assert_eq!(args[0], "-NoProfile");
        assert_eq!(args[1], "-NonInteractive");
        assert_eq!(args[2], "-Command");
        assert!(args[3].ends_with("exit $LASTEXITCODE"));
        assert!(args[3].starts_with("cargo build"));
    }

    #[test]
    fn sh_and_git_bash_use_dash_c() {
        for kind in [ShellKind::Sh, ShellKind::GitBash] {
            let mut args: Vec<String> = Vec::new();
            apply_shell_args(kind, "ls -la", |a| {
                args.push(a.to_string());
            });
            assert_eq!(args, vec!["-c".to_string(), "ls -la".to_string()]);
        }
    }

    #[test]
    fn cmd_uses_slash_c() {
        let mut args: Vec<String> = Vec::new();
        apply_shell_args(ShellKind::Cmd, "dir", |a| {
            args.push(a.to_string());
        });
        assert_eq!(args, vec!["/C".to_string(), "dir".to_string()]);
    }
}
