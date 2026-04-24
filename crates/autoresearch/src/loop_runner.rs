//! Main autonomous research loop
//!
//! Orchestrates the full autonomous research workflow: execute experiments,
//! evaluate results, and decide whether to keep or discard changes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::Result;
use tokio::sync::RwLock;

pub use crate::experiment::{ExperimentConfig, ExperimentOutput, ExperimentRunner};
pub use crate::git_integration::GitManager;
pub use crate::metric_parser::{MetricParser, ParsedMetrics};
pub use crate::program::{MetricGoal, ResearchProgram};
pub use crate::result::{ExperimentResult, ExperimentStatus, ResultsLogger};

use crate::AutoResearchSession;

/// Configuration for the autonomous research loop
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Maximum number of experiments (0 = unlimited)
    pub max_experiments: u64,
    /// Maximum total time in seconds (0 = unlimited)
    pub max_time_seconds: u64,
    /// Whether to auto-commit improvements
    pub auto_commit: bool,
    /// Commit message template
    pub commit_template: String,
    /// Whether to print progress
    pub verbose: bool,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_experiments: 0, // Unlimited
            max_time_seconds: 0,
            auto_commit: true,
            commit_template: "experiment: {description}".to_string(),
            verbose: true,
        }
    }
}

/// State of the research loop
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoopState {
    /// Loop is running
    Running,
    /// Loop is paused/waiting
    Paused,
    /// Loop has completed
    Completed,
    /// Loop was interrupted
    Interrupted,
    /// Loop encountered an error
    Error,
}

/// Event emitted by the research loop
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LoopEvent {
    /// Loop state changed
    StateChanged { from: LoopState, to: LoopState },
    /// Experiment started
    ExperimentStarted { number: u64 },
    /// Experiment completed
    ExperimentCompleted {
        number: u64,
        result: ExperimentResult,
    },
    /// Experiment crashed
    ExperimentCrashed { number: u64, error: String },
    /// Metric improved
    MetricImproved {
        number: u64,
        old_metric: f64,
        new_metric: f64,
    },
    /// Metric regressed
    MetricRegressed {
        number: u64,
        old_metric: f64,
        new_metric: f64,
    },
    /// Commit created
    Committed { commit: String },
    /// Reset performed
    Reset { to_commit: String },
    /// Loop completed
    Completed { total_experiments: u64 },
    /// Progress update
    Progress {
        experiments_per_hour: f64,
        eta_minutes: Option<f64>,
    },
}

/// Main autonomous research loop
pub struct AutoResearchLoop {
    config: LoopConfig,
    program: ResearchProgram,
    experiment_config: ExperimentConfig,
    git: GitManager,
    logger: ResultsLogger,
    #[allow(dead_code)]
    session: Arc<RwLock<Option<AutoResearchSession>>>,
    state: Arc<RwLock<LoopState>>,
    should_stop: Arc<AtomicBool>,
    start_time: Option<Instant>,
    experiment_count: u64,
    baseline_metric: Option<f64>,
    baseline_commit: Option<String>,
    current_commit: Option<String>,
}

impl AutoResearchLoop {
    /// Create a new autonomous research loop
    pub fn new(
        program: ResearchProgram,
        workspace: impl AsRef<Path>,
        experiment_command: &str,
        experiment_args: Vec<String>,
    ) -> Result<Self> {
        let workspace = workspace.as_ref();
        let results_path = workspace.join("results.tsv");

        // Create git manager for the main repo
        let git = GitManager::new(workspace);

        // Initialize results logger
        let logger = ResultsLogger::new(&results_path);
        logger.init()?;

        // Configure experiment runner
        let experiment_config = ExperimentConfig {
            working_dir: workspace.to_path_buf(),
            command: experiment_command.to_string(),
            args: experiment_args,
            time_budget_seconds: program.time_budget_seconds,
            log_file: Some(PathBuf::from("run.log")),
            memory_limit_gb: program.max_memory_gb,
            kill_timeout_factor: 2,
        };

        Ok(Self {
            config: LoopConfig::default(),
            program,
            experiment_config,
            git,
            logger,
            session: Arc::new(RwLock::new(None)),
            state: Arc::new(RwLock::new(LoopState::Paused)),
            should_stop: Arc::new(AtomicBool::new(false)),
            start_time: None,
            experiment_count: 0,
            baseline_metric: None,
            baseline_commit: None,
            current_commit: None,
        })
    }

    /// Configure the loop
    pub fn with_config(mut self, config: LoopConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the working directory (for the git repo)
    pub fn with_workspace(mut self, workspace: impl AsRef<Path>) -> Self {
        let workspace = workspace.as_ref();
        self.experiment_config.working_dir = workspace.to_path_buf();
        self.git = GitManager::new(workspace);
        self
    }

    /// Set up the research branch
    pub async fn setup(&mut self, tag: &str) -> Result<()> {
        // Create the autoresearch branch
        let branch_name = format!("autoresearch/{}", tag);

        // Check if branch already exists
        if let Ok(current) = self.git.current_branch().await {
            if current == branch_name {
                tracing::info!("Already on branch {}", branch_name);
            } else {
                // Check if we should create or switch
                let output = std::process::Command::new("git")
                    .current_dir(&self.experiment_config.working_dir)
                    .args(["rev-parse", "--verify", &format!("origin/{}", branch_name)])
                    .output();

                match output {
                    Ok(out) if out.status.success() => {
                        self.git.checkout(&branch_name).await?;
                    }
                    _ => {
                        self.git.create_and_switch(&branch_name).await?;
                    }
                }
            }
        }

        // Get the starting commit
        self.current_commit = Some(self.git.current_commit().await?);

        // Run baseline experiment if no results exist
        if self.logger.load()?.is_empty() {
            tracing::info!("No previous results found. Running baseline experiment...");
            self.run_baseline().await?;
        }

        // Set initial baseline
        if let Some(results) = self
            .logger
            .best(self.program.metric_goal == MetricGoal::Minimize)?
        {
            self.baseline_metric = Some(results.metric_value);
            self.baseline_commit = Some(results.commit);
        }

        tracing::info!(
            "Setup complete. Baseline metric: {:?}",
            self.baseline_metric
        );
        Ok(())
    }

    /// Run the baseline experiment
    pub async fn run_baseline(&mut self) -> Result<()> {
        self.experiment_count += 1;

        if self.config.verbose {
            println!("\n{:=<60}", "");
            println!(" Running Baseline Experiment ({})", self.experiment_count);
            println!("{:=<60}", "");
        }

        let commit = self.git.current_commit().await?;
        let runner = ExperimentRunner::new(self.experiment_config.clone());
        let output = runner.run().await?;

        // Parse the metric
        let metric_value = self.parse_metric(&output)?;
        let status = if output.status == ExperimentStatus::Crash {
            ExperimentStatus::Crash
        } else {
            ExperimentStatus::Keep
        };

        let result = ExperimentResult::new(
            commit,
            metric_value,
            output.memory_gb,
            output.training_seconds,
            output.elapsed_seconds,
            status,
            "baseline".to_string(),
        );

        // Update baseline
        if status == ExperimentStatus::Keep && self.baseline_metric.is_none() {
            self.baseline_metric = Some(metric_value);
            self.baseline_commit = Some(result.commit.clone());
        }

        self.logger.append(&result)?;

        if self.config.verbose {
            self.print_result(&result);
        }

        Ok(())
    }

    /// Run a single experiment
    pub async fn run_experiment(&mut self, description: &str) -> Result<ExperimentResult> {
        self.experiment_count += 1;
        self.set_state(LoopState::Running);

        if self.config.verbose {
            println!("\n{:=<60}", "");
            println!(" Experiment #{}: {}", self.experiment_count, description);
            println!("{:=<60}", "");
        }

        // Commit the changes first
        let commit = match self.git.is_dirty().await? {
            true => {
                let msg = self
                    .config
                    .commit_template
                    .replace("{description}", description);
                match self.git.commit(&msg).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Failed to commit: {}. Using current commit.", e);
                        self.git.current_commit().await?
                    }
                }
            }
            false => self.git.current_commit().await?,
        };

        // Run the experiment
        let runner = ExperimentRunner::new(self.experiment_config.clone());
        let output = runner.run().await?;

        // Parse the metric
        let metric_value = self.parse_metric(&output)?;

        // Determine if this is an improvement
        let baseline = self.baseline_metric.unwrap_or(f64::INFINITY);
        let is_improvement = match self.program.metric_goal {
            MetricGoal::Minimize => metric_value < baseline,
            MetricGoal::Maximize => metric_value > baseline,
        };

        let status = if output.status == ExperimentStatus::Crash {
            ExperimentStatus::Crash
        } else if is_improvement {
            ExperimentStatus::Keep
        } else {
            ExperimentStatus::Discard
        };

        let result = ExperimentResult::new(
            commit.clone(),
            metric_value,
            output.memory_gb,
            output.training_seconds,
            output.elapsed_seconds,
            status,
            description.to_string(),
        );

        // Handle based on status
        match status {
            ExperimentStatus::Keep => {
                // Keep this experiment - update baseline
                self.baseline_metric = Some(metric_value);
                self.baseline_commit = Some(commit.clone());
                self.current_commit = Some(commit.clone());
            }
            ExperimentStatus::Discard => {
                // Revert to baseline
                if let Some(ref baseline_commit) = self.baseline_commit {
                    self.git.reset_hard(baseline_commit).await?;
                    self.current_commit = Some(baseline_commit.clone());
                    tracing::info!("Reverted to commit: {}", baseline_commit);
                }
            }
            ExperimentStatus::Crash => {
                // Try to fix and revert
                if let Some(ref baseline_commit) = self.baseline_commit {
                    self.git.reset_hard(baseline_commit).await?;
                    self.current_commit = Some(baseline_commit.clone());
                }
            }
        }

        // Log the result
        self.logger.append(&result)?;

        if self.config.verbose {
            self.print_result(&result);
        }

        self.set_state(LoopState::Paused);
        Ok(result)
    }

    /// Run the autonomous loop until stopped
    pub async fn run_loop<F>(&mut self, mut iteration_fn: F) -> Result<()>
    where
        F: FnMut(u64, Option<f64>, Option<f64>) -> Option<String>,
    {
        self.set_state(LoopState::Running);
        self.start_time = Some(Instant::now());

        tracing::info!("Starting autonomous research loop");

        loop {
            // Check stop conditions
            if self.should_stop.load(Ordering::SeqCst) {
                tracing::info!("Loop stopped by user");
                self.set_state(LoopState::Interrupted);
                break;
            }

            // Check experiment limit
            if self.config.max_experiments > 0
                && self.experiment_count >= self.config.max_experiments
            {
                tracing::info!("Reached experiment limit ({})", self.config.max_experiments);
                self.set_state(LoopState::Completed);
                break;
            }

            // Check time limit
            if let Some(start) = self.start_time
                && self.config.max_time_seconds > 0
            {
                let elapsed = start.elapsed().as_secs();
                if elapsed >= self.config.max_time_seconds {
                    tracing::info!(
                        "Reached time limit ({} seconds)",
                        self.config.max_time_seconds
                    );
                    self.set_state(LoopState::Completed);
                    break;
                }
            }

            // Get the next experiment description from the agent
            let description = iteration_fn(
                self.experiment_count + 1,
                self.baseline_metric,
                self.baseline_metric,
            );

            match description {
                Some(desc) => {
                    self.run_experiment(&desc).await?;
                }
                None => {
                    tracing::info!("No more experiments (agent returned None)");
                    self.set_state(LoopState::Completed);
                    break;
                }
            }
        }

        // Print final summary
        if self.config.verbose {
            self.print_summary();
        }

        Ok(())
    }

    /// Request the loop to stop
    pub fn stop(&self) {
        self.should_stop.store(true, Ordering::SeqCst);
        self.set_state(LoopState::Interrupted);
    }

    /// Get current state
    pub async fn state(&self) -> LoopState {
        *self.state.read().await
    }

    /// Get progress info
    pub fn progress(&self) -> (u64, Option<f64>) {
        let elapsed = self
            .start_time
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);

        let experiments_per_hour = if elapsed > 0.0 {
            self.experiment_count as f64 / (elapsed / 3600.0)
        } else {
            0.0
        };

        (self.experiment_count, Some(experiments_per_hour))
    }

    /// Parse metric from experiment output
    fn parse_metric(&self, output: &ExperimentOutput) -> Result<f64> {
        let parser = MetricParser::new();

        // Try val_bpb first
        if let Some(val_bpb) = parser.extract_val_bpb(&output.output) {
            return Ok(val_bpb);
        }

        // Fall back to generic extraction
        if let Some(metrics) = parser.extract_all(&output.output)
            && let Some(val_bpb) = metrics.val_bpb
        {
            return Ok(val_bpb);
        }

        // If we have an explicit regex from the program, use that
        let regex = &self.program.metric_command.parse_regex;
        if let Some(value) = parser.extract_with_regex(&output.output, regex) {
            return Ok(value);
        }

        Err(anyhow::anyhow!("Failed to extract metric from output"))
    }

    fn set_state(&self, state: LoopState) {
        let current = *self.state.blocking_read();
        if current != state {
            *self.state.blocking_write() = state;
            tracing::debug!("Loop state: {:?} -> {:?}", current, state);
        }
    }

    fn print_result(&self, result: &ExperimentResult) {
        let improvement = self
            .baseline_metric
            .map(|b| (b - result.metric_value) / b * 100.0)
            .unwrap_or(0.0);

        let status_icon = match result.status {
            ExperimentStatus::Keep => "✓",
            ExperimentStatus::Discard => "✗",
            ExperimentStatus::Crash => "!",
        };

        println!("\n{}", "-".repeat(60));
        println!(" {} {}", status_icon, result.description);
        println!("{}", "-".repeat(60));
        println!("  commit: {}", result.commit);
        println!("  metric: {:.6}", result.metric_value);
        println!("  memory: {:.1} GB", result.memory_gb);
        println!(
            "  time: {:.1}s / {:.1}s",
            result.training_seconds, result.total_seconds
        );

        if result.status == ExperimentStatus::Keep {
            println!("  improvement: {:.2}%", improvement);
        }

        println!("  status: {}", result.status);
        println!("{}", "-".repeat(60));
    }

    fn print_summary(&self) {
        println!("\n{}", "=".repeat(60));
        println!(" AUTONOMOUS RESEARCH COMPLETE");
        println!("{}", "=".repeat(60));

        if let Ok(summary) = self
            .logger
            .summary(self.program.metric_goal == MetricGoal::Minimize)
        {
            println!("\nTotal experiments: {}", summary.total_experiments);
            println!("  Kept: {}", summary.kept);
            println!("  Discarded: {}", summary.discarded);
            println!("  Crashed: {}", summary.crashed);

            if let Some(baseline) = summary.baseline_metric {
                println!("\nBaseline metric: {:.6}", baseline);
            }
            if let Some(best) = summary.best_metric {
                let improvement = summary
                    .baseline_metric
                    .map(|b| (b - best) / b * 100.0)
                    .unwrap_or(0.0);
                println!("Best metric: {:.6} ({:.2}% improvement)", best, improvement);
            }
        }

        if let Some(start) = self.start_time {
            let elapsed = start.elapsed();
            println!("\nTotal time: {:?}", elapsed);
            if elapsed.as_secs() > 0 {
                println!(
                    "Experiments per hour: {:.1}",
                    self.experiment_count as f64 / (elapsed.as_secs_f64() / 3600.0)
                );
            }
        }

        println!("\nResults saved to: {:?}", self.logger.results_path());
        println!("{}", "=".repeat(60));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loop_config_defaults() {
        let config = LoopConfig::default();
        assert_eq!(config.max_experiments, 0);
        assert_eq!(config.max_time_seconds, 0);
        assert!(config.auto_commit);
    }
}
