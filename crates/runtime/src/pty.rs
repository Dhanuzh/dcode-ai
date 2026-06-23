use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::{Duration, timeout};

/// Manages PTY sessions for sandboxed command execution.
pub struct PtyManager {
    workspace_root: std::path::PathBuf,
}

impl PtyManager {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
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
        cmd.current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| PtyError::SpawnFailed(e.to_string()))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stdout_tx = line_tx.clone();
        let stdout_task = tokio::spawn(async move {
            let mut lines = Vec::new();
            if let Some(stdout) = stdout {
                let reader = BufReader::new(stdout);
                let mut line_reader = reader.lines();
                while let Ok(Some(line)) = line_reader.next_line().await {
                    if let Some(ref tx) = stdout_tx {
                        let _ = tx.try_send(line.clone());
                    }
                    lines.push(line);
                }
            }
            lines
        });

        let stderr_tx = line_tx;
        let stderr_task = tokio::spawn(async move {
            let mut lines = Vec::new();
            if let Some(stderr) = stderr {
                let reader = BufReader::new(stderr);
                let mut line_reader = reader.lines();
                while let Ok(Some(line)) = line_reader.next_line().await {
                    if let Some(ref tx) = stderr_tx {
                        let _ = tx.try_send(line.clone());
                    }
                    lines.push(line);
                }
            }
            lines
        });

        let result = timeout(Duration::from_secs(timeout_secs), async {
            let (stdout_lines, stderr_lines) = tokio::join!(stdout_task, stderr_task);
            let stdout_lines = stdout_lines.unwrap_or_default();
            let stderr_lines = stderr_lines.unwrap_or_default();
            let status = child.wait().await;
            (stdout_lines, stderr_lines, status)
        })
        .await
        .map_err(|_| {
            let _ = child.start_kill();
            PtyError::Timeout(timeout_secs)
        })?;

        let (stdout_lines, stderr_lines, status) = result;
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
}

#[cfg(windows)]
fn shell_command(command: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("cmd");
    cmd.arg("/C").arg(command);
    cmd
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-lc").arg(command);
    cmd
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
