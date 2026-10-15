//! Experiment execution with fixed time budget
//!
//! Runs experiments with a hard timeout, captures output, and extracts metrics.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

pub use crate::metric_parser::MetricParser;
pub use crate::program::{MetricGoal, ResearchProgram};
pub use crate::result::{ExperimentResult, ExperimentStatus};

/// Configuration for experiment execution
#[derive(Debug, Clone)]
pub struct ExperimentConfig {
    /// Working directory for the experiment
    pub working_dir: PathBuf,
    /// Command to run
    pub command: String,
    /// Arguments for the command
    pub args: Vec<String>,
    /// Time budget in seconds
    pub time_budget_seconds: u64,
    /// Log file path
    pub log_file: Option<PathBuf>,
    /// Memory limit in GB (soft constraint)
    pub memory_limit_gb: Option<f64>,
    /// Kill timeout factor (how many times the budget before SIGKILL)
    pub kill_timeout_factor: u64,
}

impl Default for ExperimentConfig {
    fn default() -> Self {
        Self {
            working_dir: PathBuf::from("."),
            command: "python".to_string(),
            args: vec!["train.py".to_string()],
            time_budget_seconds: 300,
            log_file: None,
            memory_limit_gb: None,
            kill_timeout_factor: 2,
        }
    }
}

impl ExperimentConfig {
    /// Run a training script (like karpathy's train.py)
    pub fn training_script(script: impl AsRef<Path>, time_budget_seconds: u64) -> Self {
        let script_path = script.as_ref();
        let script_name = script_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("train.py");

        Self {
            working_dir: script_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")),
            command: "python".to_string(),
            args: vec![script_name.to_string()],
            time_budget_seconds,
            log_file: Some(PathBuf::from("run.log")),
            memory_limit_gb: None,
            kill_timeout_factor: 2,
        }
    }
}

/// Experiment runner that executes runs with timeout and metric extraction
pub struct ExperimentRunner {
    config: ExperimentConfig,
    metric_parser: MetricParser,
}

impl ExperimentRunner {
    /// Create a new experiment runner
    pub fn new(config: ExperimentConfig) -> Self {
        Self {
            config,
            metric_parser: MetricParser::default(),
        }
    }

    /// Set a custom metric parser
    pub fn with_metric_parser(mut self, parser: MetricParser) -> Self {
        self.metric_parser = parser;
        self
    }

    /// Run a single experiment with the configured time budget
    ///
    /// Returns the experiment result with parsed metrics.
    pub async fn run(&self) -> Result<ExperimentOutput> {
        self.run_with_description(String::new()).await
    }

    /// Run an experiment with a description of what was changed
    pub async fn run_with_description(&self, description: String) -> Result<ExperimentOutput> {
        let start_time = Instant::now();

        // Set up log file
        let log_path = self.config.log_file.as_ref().map(|p| {
            if p.is_relative() {
                self.config.working_dir.join(p)
            } else {
                p.clone()
            }
        });

        // Clear/create log file
        if let Some(ref log_path) = log_path {
            tokio::fs::create_dir_all(log_path.parent().unwrap_or(&self.config.working_dir))
                .await?;
            tokio::fs::write(log_path, b"").await?;
        }

        // Spawn the process
        let mut child = Command::new(&self.config.command)
            .args(&self.config.args)
            .current_dir(&self.config.working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn: {} {:?}",
                    self.config.command, self.config.args
                )
            })?;

        // Set up output capture
        let stdout = child.stdout.take().expect("stdout captured");
        let stderr = child.stderr.take().expect("stderr captured");

        // Use Arc for shared state between tasks
        let output_arc = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let output_arc_clone1 = output_arc.clone();
        let output_arc_clone2 = output_arc.clone();

        // Stream stdout to log file and channel
        let log_path_clone = log_path.clone();
        let stdout_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut log_file: Option<tokio::fs::File> = if let Some(ref path) = log_path_clone {
                tokio::fs::File::create(path).await.ok()
            } else {
                None
            };

            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                // Write to log file
                if let Some(ref mut file) = log_file {
                    let _ = file.write_all(line.as_bytes()).await;
                }
                // Collect output
                {
                    let mut output = output_arc_clone1.lock().await;
                    output.push(line.clone());
                }
                line.clear();
            }
        });

        // Stream stderr
        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                {
                    let mut output = output_arc_clone2.lock().await;
                    output.push(format!("[stderr] {}", line.trim()));
                }
                line.clear();
            }
        });

        // Wait for process with timeout
        let timeout_duration =
            Duration::from_secs(self.config.time_budget_seconds * self.config.kill_timeout_factor);

        let wait_result = timeout(timeout_duration, child.wait()).await;

        // Check if we timed out
        let exit_status = match wait_result {
            Ok(Ok(status)) => {
                // Process exited normally before timeout
                Some(status)
            }
            Ok(Err(e)) => {
                // Process spawn failed
                return Err(anyhow::anyhow!("Process wait failed: {}", e));
            }
            Err(_) => {
                // Timeout - kill the process
                tracing::warn!(
                    "Experiment timed out after {} seconds, killing process",
                    self.config.time_budget_seconds
                );

                // Kill synchronously
                let _ = child.kill().await;

                // Wait a moment for cleanup
                let _ = timeout(Duration::from_secs(5), child.wait()).await;

                None // Indicates timeout
            }
        };

        // Wait for output streams to finish
        let _ = stdout_task.await;
        let _ = stderr_task.await;

        // Collect output
        let output = {
            let output = output_arc.lock().await;
            output.join("\n")
        };

        let elapsed = start_time.elapsed();

        // Determine success and extract metrics
        let (status, parsed_metrics) = if let Some(code) = exit_status.and_then(|s| s.code()) {
            if code == 0 {
                (
                    ExperimentStatus::Keep,
                    self.metric_parser.extract_all(&output),
                )
            } else {
                (ExperimentStatus::Crash, None)
            }
        } else {
            (ExperimentStatus::Crash, None)
        };

        // Extract values from parsed_metrics
        let peak_vram_mb = parsed_metrics.as_ref().and_then(|m| m.peak_vram_mb);
        let mfu_percent = parsed_metrics.as_ref().and_then(|m| m.mfu_percent);
        let total_tokens_m = parsed_metrics.as_ref().and_then(|m| m.total_tokens_m);
        let num_steps = parsed_metrics.as_ref().and_then(|m| m.num_steps);
        let num_params_m = parsed_metrics.as_ref().and_then(|m| m.num_params_m);
        let training_seconds = parsed_metrics
            .as_ref()
            .and_then(|m| m.training_seconds)
            .unwrap_or(elapsed.as_secs_f64());

        Ok(ExperimentOutput {
            status,
            output,
            elapsed_seconds: elapsed.as_secs_f64(),
            training_seconds,
            peak_vram_mb,
            mfu_percent,
            total_tokens_m,
            num_steps,
            num_params_m,
            memory_gb: peak_vram_mb.map(|mb| mb / 1024.0).unwrap_or(0.0),
            description,
            log_path,
        })
    }

    /// Run with a specific metric extraction
    pub async fn run_with_metric(
        &self,
        _metric_command: &str,
        regex: &str,
    ) -> Result<(ExperimentOutput, Option<f64>)> {
        let output = self.run().await?;
        let metric_value = self.metric_parser.extract_with_regex(&output.output, regex);
        Ok((output, metric_value))
    }
}

/// Output from an experiment run
#[derive(Debug, Clone)]
pub struct ExperimentOutput {
    /// Experiment status
    pub status: ExperimentStatus,
    /// Captured stdout/stderr
    pub output: String,
    /// Total elapsed time
    pub elapsed_seconds: f64,
    /// Training time (may be extracted from output)
    pub training_seconds: f64,
    /// Peak VRAM in MB
    pub peak_vram_mb: Option<f64>,
    /// Model FLOPs utilization
    pub mfu_percent: Option<f64>,
    /// Total tokens in millions
    pub total_tokens_m: Option<f64>,
    /// Number of training steps
    pub num_steps: Option<u64>,
    /// Number of parameters in millions
    pub num_params_m: Option<f64>,
    /// Memory usage in GB
    pub memory_gb: f64,
    /// Description of the experiment
    pub description: String,
    /// Path to log file
    pub log_path: Option<PathBuf>,
}

impl ExperimentOutput {
    /// Convert to ExperimentResult for logging
    pub fn to_result(&self, commit: &str) -> ExperimentResult {
        let mut result = ExperimentResult::new(
            commit.to_string(),
            0.0, // Will be set by the caller
            self.memory_gb,
            self.training_seconds,
            self.elapsed_seconds,
            self.status,
            self.description.clone(),
        );
        result.peak_vram_mb = self.peak_vram_mb;
        result.mfu_percent = self.mfu_percent;
        result.total_tokens_m = self.total_tokens_m;
        result.num_steps = self.num_steps;
        result.num_params_m = self.num_params_m;
        result
    }
}

/// Run an experiment and extract metrics in one go
pub async fn run_experiment(
    config: ExperimentConfig,
    metric_regex: &str,
) -> Result<(ExperimentOutput, Option<f64>)> {
    let runner = ExperimentRunner::new(config);
    let output = runner.run().await?;
    let metric_value = MetricParser::default().extract_with_regex(&output.output, metric_regex);
    Ok((output, metric_value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = ExperimentConfig::default();
        assert_eq!(config.time_budget_seconds, 300);
        assert_eq!(config.command, "python");
    }

    #[tokio::test]
    async fn test_successful_experiment() {
        let config = ExperimentConfig {
            command: "echo".to_string(),
            args: vec!["val_bpb: 1.234\npeak_vram_mb: 4096.0".to_string()],
            time_budget_seconds: 5,
            ..Default::default()
        };

        let runner = ExperimentRunner::new(config);
        let output = runner.run().await.unwrap();

        assert_eq!(output.status, ExperimentStatus::Keep);
        assert!(output.output.contains("val_bpb"));
    }

    #[tokio::test]
    async fn test_failed_experiment() {
        let config = ExperimentConfig {
            command: "bash".to_string(),
            args: vec!["-c".to_string(), "exit 1".to_string()],
            time_budget_seconds: 5,
            ..Default::default()
        };

        let runner = ExperimentRunner::new(config);
        let output = runner.run().await.unwrap();

        assert_eq!(output.status, ExperimentStatus::Crash);
    }

    #[tokio::test]
    async fn test_timeout() {
        let config = ExperimentConfig {
            command: "sleep".to_string(),
            args: vec!["10".to_string()],
            time_budget_seconds: 1,
            kill_timeout_factor: 1, // Aggressive kill
            ..Default::default()
        };

        let runner = ExperimentRunner::new(config);
        let start = Instant::now();
        let output = runner.run().await.unwrap();
        let elapsed = start.elapsed();

        // Should timeout well before 10 seconds
        assert!(elapsed < Duration::from_secs(5));
        assert_eq!(output.status, ExperimentStatus::Crash);
    }
}
