//! Experiment result types and logging
//!
//! Handles serialization, TSV logging, and result analysis.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

/// Experiment status in the research loop
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExperimentStatus {
    /// Metric improved, keep this commit
    Keep,
    /// Metric is worse or equal, discard
    Discard,
    /// Experiment crashed or timed out
    Crash,
}

impl std::fmt::Display for ExperimentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExperimentStatus::Keep => write!(f, "keep"),
            ExperimentStatus::Discard => write!(f, "discard"),
            ExperimentStatus::Crash => write!(f, "crash"),
        }
    }
}

/// Result of a single experiment run
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExperimentResult {
    /// Short git commit hash (7 chars)
    pub commit: String,
    /// The extracted metric value
    pub metric_value: f64,
    /// Peak memory usage in GB
    pub memory_gb: f64,
    /// Training time in seconds (excluding startup)
    pub training_seconds: f64,
    /// Total experiment time including startup/evaluation
    pub total_seconds: f64,
    /// Whether we should keep this experiment
    pub status: ExperimentStatus,
    /// Short description of what was tried
    pub description: String,
    /// When the experiment was run
    pub timestamp: DateTime<Utc>,
    /// Peak VRAM in MB (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_vram_mb: Option<f64>,
    /// Model FLOPs utilization percentage (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mfu_percent: Option<f64>,
    /// Total tokens processed in millions (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens_m: Option<f64>,
    /// Number of training steps
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_steps: Option<u64>,
    /// Number of model parameters in millions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_params_m: Option<f64>,
}

impl ExperimentResult {
    /// Create a new experiment result
    pub fn new(
        commit: String,
        metric_value: f64,
        memory_gb: f64,
        training_seconds: f64,
        total_seconds: f64,
        status: ExperimentStatus,
        description: String,
    ) -> Self {
        Self {
            commit,
            metric_value,
            memory_gb,
            training_seconds,
            total_seconds,
            status,
            description,
            timestamp: Utc::now(),
            peak_vram_mb: None,
            mfu_percent: None,
            total_tokens_m: None,
            num_steps: None,
            num_params_m: None,
        }
    }

    /// Create a crash result
    pub fn crash(commit: String, description: String) -> Self {
        Self::new(
            commit,
            0.0,
            0.0,
            0.0,
            0.0,
            ExperimentStatus::Crash,
            description,
        )
    }

    /// Format for TSV logging
    pub fn to_tsv_row(&self) -> String {
        format!(
            "{}\t{:.6}\t{:.1}\t{}\t{}",
            self.commit,
            self.metric_value,
            self.memory_gb,
            self.status,
            self.description.replace(['\t', '\n'], " ")
        )
    }

    /// Parse from a TSV row
    pub fn from_tsv_row(row: &str, timestamp: DateTime<Utc>) -> Option<Self> {
        let parts: Vec<&str> = row.split('\t').collect();
        if parts.len() < 5 {
            return None;
        }

        let commit = parts[0].to_string();
        let metric_value = parts[1].parse().ok()?;
        let memory_gb = parts[2].parse().ok()?;
        let status = match parts[3] {
            "keep" => ExperimentStatus::Keep,
            "discard" => ExperimentStatus::Discard,
            "crash" => ExperimentStatus::Crash,
            _ => return None,
        };
        let description = parts[4].to_string();

        Some(Self {
            commit,
            metric_value,
            memory_gb,
            training_seconds: 0.0,
            total_seconds: 0.0,
            status,
            description,
            timestamp,
            peak_vram_mb: None,
            mfu_percent: None,
            total_tokens_m: None,
            num_steps: None,
            num_params_m: None,
        })
    }
}

/// TSV header for results log
pub const RESULTS_TSV_HEADER: &str = "commit\tval_bpb\tmemory_gb\tstatus\tdescription";

/// Handles logging experiment results to TSV files
#[derive(Debug, Clone)]
pub struct ResultsLogger {
    path: PathBuf,
}

impl ResultsLogger {
    /// Create a new results logger
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Get the results file path
    pub fn results_path(&self) -> &Path {
        &self.path
    }

    /// Initialize the results file with header
    pub fn init(&self) -> Result<()> {
        if self.path.exists() {
            return Ok(());
        }

        let parent = self.path.parent().context("Invalid results path")?;
        std::fs::create_dir_all(parent)?;

        let mut file = File::create(&self.path)
            .with_context(|| format!("Failed to create results file: {:?}", self.path))?;

        writeln!(file, "{}", RESULTS_TSV_HEADER)
            .with_context(|| "Failed to write header to results file")?;

        Ok(())
    }

    /// Append a result to the log
    pub fn append(&self, result: &ExperimentResult) -> Result<()> {
        // Ensure file exists with header
        if !self.path.exists() {
            self.init()?;
        }

        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .with_context(|| format!("Failed to open results file: {:?}", self.path))?;

        writeln!(file, "{}", result.to_tsv_row())
            .with_context(|| "Failed to write result to results file")?;

        Ok(())
    }

    /// Load all results from the log
    pub fn load(&self) -> Result<Vec<ExperimentResult>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&self.path)
            .with_context(|| format!("Failed to open results file: {:?}", self.path))?;
        let reader = BufReader::new(file);
        let mut results = Vec::new();
        let mut lines = reader.lines();

        // Skip header
        if let Some(Ok(line)) = lines.next()
            && line.trim() != RESULTS_TSV_HEADER.trim()
        {
            anyhow::bail!("Invalid results file header");
        }

        for line in lines {
            let line = line.context("Failed to read line from results file")?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(result) = ExperimentResult::from_tsv_row(&line, Utc::now()) {
                results.push(result);
            }
        }

        Ok(results)
    }

    /// Get the best result from the log
    pub fn best(&self, minimize: bool) -> Result<Option<ExperimentResult>> {
        let results = self.load()?;

        if results.is_empty() {
            return Ok(None);
        }

        let best = results
            .into_iter()
            .filter(|r| r.status != ExperimentStatus::Crash)
            .min_by(|a, b| {
                if minimize {
                    a.metric_value
                        .partial_cmp(&b.metric_value)
                        .unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    b.metric_value
                        .partial_cmp(&a.metric_value)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }
            });

        Ok(best)
    }

    /// Get summary statistics
    pub fn summary(&self, minimize: bool) -> Result<ResultsSummary> {
        let results = self.load()?;

        let total = results.len();
        let kept = results
            .iter()
            .filter(|r| r.status == ExperimentStatus::Keep)
            .count();
        let discarded = results
            .iter()
            .filter(|r| r.status == ExperimentStatus::Discard)
            .count();
        let crashed = results
            .iter()
            .filter(|r| r.status == ExperimentStatus::Crash)
            .count();

        let best = results
            .iter()
            .filter(|r| r.status != ExperimentStatus::Crash)
            .map(|r| r.metric_value)
            .reduce(|a, b| if minimize { a.min(b) } else { a.max(b) });

        let baseline = results.first().map(|r| r.metric_value);

        Ok(ResultsSummary {
            total_experiments: total,
            kept,
            discarded,
            crashed,
            best_metric: best,
            baseline_metric: baseline,
        })
    }

    /// Print results as a formatted table
    pub fn print_table(&self) -> Result<()> {
        let results = self.load()?;

        if results.is_empty() {
            println!("No experiments recorded yet.");
            return Ok(());
        }

        println!("\n{:=<80}", "");
        println!(" Experiment Results ");
        println!("{:=<80}", "");
        println!(
            "{:<8} {:>10} {:>10} {:>8}  description",
            "commit", "val_bpb", "mem_gb", "status"
        );
        println!("{:-<80}", "");

        for result in &results {
            let status_str = match result.status {
                ExperimentStatus::Keep => "keep",
                ExperimentStatus::Discard => "discard",
                ExperimentStatus::Crash => "crash",
            };
            let desc = if result.description.len() > 45 {
                format!("{}...", &result.description[..42])
            } else {
                result.description.clone()
            };
            println!(
                "{:<8} {:>10.6} {:>10.1} {:>8}  {}",
                result.commit, result.metric_value, result.memory_gb, status_str, desc
            );
        }

        println!("{:-<80}", "");

        if let Ok(summary) = self.summary(true) {
            if let Some(baseline) = summary.baseline_metric {
                println!("Baseline: {:.6}", baseline);
            }
            if let Some(best) = summary.best_metric {
                let improvement = summary
                    .baseline_metric
                    .map(|b| (b - best) / b * 100.0)
                    .unwrap_or(0.0);
                println!("Best: {:.6} ({:.2}% improvement)", best, improvement);
            }
            println!(
                "\nSummary: {} total, {} kept, {} discarded, {} crashed",
                summary.total_experiments, summary.kept, summary.discarded, summary.crashed
            );
        }

        Ok(())
    }
}

/// Summary statistics for results
#[derive(Debug, Clone)]
pub struct ResultsSummary {
    pub total_experiments: usize,
    pub kept: usize,
    pub discarded: usize,
    pub crashed: usize,
    pub best_metric: Option<f64>,
    pub baseline_metric: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_result_to_tsv() {
        let result = ExperimentResult::new(
            "a1b2c3d".to_string(),
            0.997900,
            44.0,
            300.1,
            325.9,
            ExperimentStatus::Keep,
            "baseline".to_string(),
        );

        let row = result.to_tsv_row();
        assert!(row.starts_with("a1b2c3d\t0.997900\t44.0\tkeep\tbaseline"));
    }

    #[test]
    fn test_result_from_tsv() {
        let row = "a1b2c3d\t0.997900\t44.0\tkeep\tbaseline";
        let result = ExperimentResult::from_tsv_row(row, Utc::now()).unwrap();

        assert_eq!(result.commit, "a1b2c3d");
        assert!((result.metric_value - 0.997900).abs() < 0.000001);
        assert_eq!(result.status, ExperimentStatus::Keep);
    }

    #[test]
    fn test_results_logger() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "{}", RESULTS_TSV_HEADER).unwrap();
        writeln!(temp_file, "a1b2c3d\t0.997900\t44.0\tkeep\tbaseline").unwrap();
        temp_file.flush().unwrap();

        let logger = ResultsLogger::new(temp_file.path());
        let results = logger.load().unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].commit, "a1b2c3d");

        let summary = logger.summary(true).unwrap();
        assert_eq!(summary.total_experiments, 1);
        assert_eq!(summary.kept, 1);
    }
}
