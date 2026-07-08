use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::{Duration, timeout};

/// Manages PTY sessions for sandboxed command execution.
pub struct PtyManager {
    workspace_root: std::path::PathBuf,
    /// Landlock-confine children (Linux): writes only beneath the workspace
    /// and scratch dirs. Set from `[permissions] sandbox_bash`. Only the
    /// unix `pre_exec` path reads these; other platforms carry them unused.
    #[cfg_attr(not(unix), allow(dead_code))]
    sandbox: bool,
    /// Extra writable roots inside the sandbox (already tilde-expanded),
    /// from `[permissions] sandbox_writable_roots`.
    #[cfg_attr(not(unix), allow(dead_code))]
    sandbox_writable_roots: Vec<std::path::PathBuf>,
}

impl PtyManager {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self::with_sandbox(workspace_root, false, Vec::new())
    }

    pub fn with_sandbox(
        workspace_root: impl AsRef<Path>,
        sandbox: bool,
        sandbox_writable_roots: Vec<std::path::PathBuf>,
    ) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            sandbox,
            sandbox_writable_roots,
        }
    }

    /// Spawn a command, capture output, and return it (non-streaming).
    pub async fn exec(&self, command: &str, timeout_secs: u64) -> Result<PtyOutput, PtyError> {
        self.exec_streaming(command, timeout_secs, None).await
    }

    /// Spawn a command with optional line-by-line streaming.
    /// Each line of stdout/stderr is sent through `line_tx` as it arrives.
    /// The full output is still collected for the ToolResult.
    pub async fn exec_streaming(
        &self,
        command: &str,
        timeout_secs: u64,
        line_tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> Result<PtyOutput, PtyError> {
        let mut cmd = shell_command(command);
        // Use process cwd if it differs from the stored root (e.g. after /cd).
        let effective_root =
            std::env::current_dir().unwrap_or_else(|_| self.workspace_root.clone());
        cmd.current_dir(&effective_root)
            // Detach stdin so an interactive command (e.g. `sudo`) fails fast
            // instead of inheriting the TUI's terminal and hanging until timeout.
            // Truly interactive commands should use the `interactive_exec` tool.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Kill the child if this future is dropped (turn cancelled) so the
            // command doesn't keep running orphaned.
            .kill_on_drop(true);

        #[cfg(unix)]
        if self.sandbox {
            let ws = effective_root.clone();
            let extra = self.sandbox_writable_roots.clone();
            // Applied between fork and exec: confines only the child.
            unsafe {
                cmd.pre_exec(move || crate::sandbox::apply_workspace_sandbox(&ws, &extra));
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| PtyError::SpawnFailed(e.to_string()))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Create a cancel token so we can abort the reader tasks on timeout.
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_stdout = cancel.clone();
        let cancel_stderr = cancel.clone();

        let stdout_tx = line_tx.clone();
        let mut stdout_task = tokio::spawn(async move {
            let mut lines = Vec::new();
            if let Some(stdout) = stdout {
                let reader = BufReader::new(stdout);
                let mut line_reader = reader.lines();
                loop {
                    tokio::select! {
                        result = line_reader.next_line() => {
                            match result {
                                Ok(Some(line)) => {
                                    if let Some(ref tx) = stdout_tx {
                                        let _ = tx.try_send(line.clone());
                                    }
                                    lines.push(line);
                                }
                                _ => break,
                            }
                        }
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {
                            if cancel_stdout.load(std::sync::atomic::Ordering::Relaxed) {
                                break;
                            }
                        }
                    }
                }
            }
            lines
        });

        let stderr_tx = line_tx;
        let mut stderr_task = tokio::spawn(async move {
            let mut lines = Vec::new();
            if let Some(stderr) = stderr {
                let reader = BufReader::new(stderr);
                let mut line_reader = reader.lines();
                loop {
                    tokio::select! {
                        result = line_reader.next_line() => {
                            match result {
                                Ok(Some(line)) => {
                                    if let Some(ref tx) = stderr_tx {
                                        let _ = tx.try_send(line.clone());
                                    }
                                    lines.push(line);
                                }
                                _ => break,
                            }
                        }
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {
                            if cancel_stderr.load(std::sync::atomic::Ordering::Relaxed) {
                                break;
                            }
                        }
                    }
                }
            }
            lines
        });

        let result = timeout(Duration::from_secs(timeout_secs), async {
            let (stdout_lines, stderr_lines) = tokio::join!(&mut stdout_task, &mut stderr_task);
            let stdout_lines = stdout_lines.unwrap_or_default();
            let stderr_lines = stderr_lines.unwrap_or_default();
            let status = child.wait().await;
            (stdout_lines, stderr_lines, status)
        })
        .await;

        match result {
            Ok(inner) => {
                let (stdout_lines, stderr_lines, status) = inner;
                let exit_code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);

                let mut text = stdout_lines.join("\n");
                if !stderr_lines.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&stderr_lines.join("\n"));
                }

                Ok(PtyOutput {
                    stdout: text,
                    exit_code,
                })
            }
            Err(_elapsed) => {
                // Signal reader tasks to abort.
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);

                // Kill the child process group aggressively.
                if let Some(pid) = child.id() {
                    kill_process_group(pid);
                }

                // Wait for the child so it doesn't become a zombie.
                let _ = child.wait().await;

                // Abort the spawned reader tasks so they don't leak.
                stdout_task.abort();
                stderr_task.abort();

                Err(PtyError::Timeout(timeout_secs))
            }
        }
    }
}

#[cfg(windows)]
fn shell_command(command: &str) -> tokio::process::Command {
    // Git Bash → PowerShell → cmd, matching what the system prompt tells the
    // model (see dcode_ai_common::shell).
    dcode_ai_common::shell::shell_command(command)
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-lc").arg(command);
    cmd
}

/// Kill a process and its entire process group. Used when a command times out.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // Send SIGKILL to the process group (negative PID = PGID).
    // This ensures sudo and its children are killed, not just the outer shell.
    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
    // Also try the process itself in case process-group kill didn't work.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

#[cfg(not(unix))]
fn kill_process_group(pid: u32) {
    // On non-Unix, use taskkill with /T to kill the process tree.
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .output();
}

#[derive(Debug)]
pub struct PtyOutput {
    pub stdout: String,
    pub exit_code: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("Command timed out after {0}s")]
    Timeout(u64),
    #[error("Spawn failed: {0}")]
    SpawnFailed(String),
}
