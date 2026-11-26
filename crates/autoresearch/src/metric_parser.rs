//! Metric extraction from experiment output
//!
//! Parses various metrics from experiment output including val_bpb, memory usage,
//! training time, and other experiment statistics.

use std::collections::HashMap;

use anyhow::Result;
use regex::Regex;

/// Parsed metrics from experiment output
#[derive(Debug, Clone, Default)]
pub struct ParsedMetrics {
    /// Validation bits per byte (karpathy's primary metric)
    pub val_bpb: Option<f64>,
    /// Peak VRAM in MB
    pub peak_vram_mb: Option<f64>,
    /// Training time in seconds
    pub training_seconds: Option<f64>,
    /// Total experiment time
    pub total_seconds: Option<f64>,
    /// Model FLOPs utilization percentage
    pub mfu_percent: Option<f64>,
    /// Total tokens in millions
    pub total_tokens_m: Option<f64>,
    /// Number of training steps
    pub num_steps: Option<u64>,
    /// Number of parameters in millions
    pub num_params_m: Option<f64>,
    /// Depth (for transformer models)
    pub depth: Option<u32>,
    /// Batch size
    pub batch_size: Option<u64>,
    /// Learning rate
    pub learning_rate: Option<f64>,
    /// Additional metrics
    pub extra: HashMap<String, f64>,
}

impl ParsedMetrics {
    /// Get val_bpb or default
    pub fn val_bpb(&self) -> f64 {
        self.val_bpb.unwrap_or(0.0)
    }

    /// Get peak memory in GB
    pub fn memory_gb(&self) -> f64 {
        self.peak_vram_mb.unwrap_or(0.0) / 1024.0
    }
}

/// Utility for extracting metrics from text output
#[derive(Debug, Clone)]
pub struct MetricParser {
    /// Regex patterns for common metrics
    patterns: HashMap<String, Regex>,
}

/// The metrics found by default: (name, pattern whose first group is the
/// value). Karpathy nanochat/nanoGPT names first, then common ones.
const DEFAULT_PATTERNS: &[(&str, &str)] = &[
    ("val_bpb", r"val_bpb:\s*([\d.]+)"),
    ("peak_vram_mb", r"peak_vram_mb:\s*([\d.]+)"),
    ("training_seconds", r"training_seconds:\s*([\d.]+)"),
    ("total_seconds", r"total_seconds:\s*([\d.]+)"),
    ("mfu_percent", r"mfu_percent:\s*([\d.]+)"),
    ("total_tokens_m", r"total_tokens_M:\s*([\d.]+)"),
    ("num_steps", r"num_steps:\s*(\d+)"),
    ("num_params_m", r"num_params_M:\s*([\d.]+)"),
    ("depth", r"depth:\s*(\d+)"),
    ("batch_size", r"batch_size:\s*(\d+)"),
    ("learning_rate", r"(?:learning_rate|lr):\s*([\d.e-]+)"),
    (
        "val_loss",
        r"(?:val_loss|validation_loss|validation error):\s*([\d.]+)",
    ),
    (
        "train_loss",
        r"(?:train_loss|training_loss|training error):\s*([\d.]+)",
    ),
    ("accuracy", r"(?:accuracy|acc):\s*([\d.]+)"),
    ("perplexity", r"perplexity:\s*([\d.]+)"),
    ("memory_mb", r"(?:peak_)?memory(?:_mb)?:\s*([\d.]+)"),
    ("peak_memory", r"peak_memory:\s*([\d.]+)"),
    ("elapsed", r"(?:elapsed|wall.?clock|time):\s*([\d.]+)"),
];

impl Default for MetricParser {
    fn default() -> Self {
        // A pattern that doesn't compile is skipped (the test below keeps
        // them all valid) — never a panic in the middle of a run.
        let patterns = DEFAULT_PATTERNS
            .iter()
            .filter_map(|(name, pattern)| Some((name.to_string(), Regex::new(pattern).ok()?)))
            .collect();
        Self { patterns }
    }
}

impl MetricParser {
    /// Create a new metric parser with default patterns
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a custom pattern
    pub fn add_pattern(&mut self, name: &str, pattern: &str) -> Result<()> {
        let regex = Regex::new(pattern)?;
        self.patterns.insert(name.to_string(), regex);
        Ok(())
    }

    /// Extract all known metrics from output
    pub fn extract_all(&self, output: &str) -> Option<ParsedMetrics> {
        let mut metrics = ParsedMetrics::default();

        for (name, regex) in &self.patterns {
            if let Some(captures) = regex.captures(output)
                && let Some(value) = captures.get(1)
            {
                let value_str = value.as_str();
                match name.as_str() {
                    "num_steps" | "depth" | "batch_size" => {
                        if let Ok(n) = value_str.parse::<u64>() {
                            match name.as_str() {
                                "num_steps" => metrics.num_steps = Some(n),
                                "depth" => metrics.depth = Some(n as u32),
                                "batch_size" => metrics.batch_size = Some(n),
                                _ => {}
                            }
                        }
                    }
                    _ => {
                        if let Ok(f) = value_str.parse::<f64>() {
                            match name.as_str() {
                                "val_bpb" => metrics.val_bpb = Some(f),
                                "peak_vram_mb" => metrics.peak_vram_mb = Some(f),
                                "training_seconds" => metrics.training_seconds = Some(f),
                                "total_seconds" => metrics.total_seconds = Some(f),
                                "mfu_percent" => metrics.mfu_percent = Some(f),
                                "total_tokens_m" => metrics.total_tokens_m = Some(f),
                                "num_params_m" => metrics.num_params_m = Some(f),
                                "learning_rate" => metrics.learning_rate = Some(f),
                                _ => {
                                    metrics.extra.insert(name.clone(), f);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Return None only if we found no metrics
        if metrics.val_bpb.is_none()
            && metrics.peak_vram_mb.is_none()
            && metrics.training_seconds.is_none()
            && metrics.extra.is_empty()
        {
            None
        } else {
            Some(metrics)
        }
    }

    /// Extract a specific metric using a regex pattern
    pub fn extract_with_regex(&self, output: &str, regex_pattern: &str) -> Option<f64> {
        let regex = Regex::new(regex_pattern).ok()?;
        let caps = regex.captures(output)?;
        let value = caps.get(1)?.as_str();
        value.parse().ok()
    }

    /// Extract val_bpb specifically (karpathy's primary metric)
    pub fn extract_val_bpb(&self, output: &str) -> Option<f64> {
        self.extract_with_regex(output, r"val_bpb:\s*([\d.]+)")
    }

    /// Extract peak memory in GB
    pub fn extract_memory_gb(&self, output: &str) -> Option<f64> {
        self.extract_with_regex(output, r"peak_vram_mb:\s*([\d.]+)")
            .map(|mb| mb / 1024.0)
            .or_else(|| self.extract_with_regex(output, r"peak_memory:\s*([\d.]+)"))
    }

    /// Extract training time in seconds
    pub fn extract_training_time(&self, output: &str) -> Option<f64> {
        self.extract_with_regex(output, r"training_seconds:\s*([\d.]+)")
            .or_else(|| self.extract_with_regex(output, r"train_time:\s*([\d.]+)"))
    }

    /// Print a summary of extracted metrics
    pub fn print_summary(&self, output: &str) {
        if let Some(metrics) = self.extract_all(output) {
            println!("\n--- Extracted Metrics ---");
            if let Some(val_bpb) = metrics.val_bpb {
                println!("val_bpb: {:.6}", val_bpb);
            }
            if let Some(peak_vram) = metrics.peak_vram_mb {
                println!("peak_vram_mb: {:.1}", peak_vram);
            }
            if let Some(training_time) = metrics.training_seconds {
                println!("training_seconds: {:.1}", training_time);
            }
            if let Some(total_time) = metrics.total_seconds {
                println!("total_seconds: {:.1}", total_time);
            }
            if let Some(mfu) = metrics.mfu_percent {
                println!("mfu_percent: {:.2}", mfu);
            }
            if let Some(tokens) = metrics.total_tokens_m {
                println!("total_tokens_M: {:.1}", tokens);
            }
            if let Some(steps) = metrics.num_steps {
                println!("num_steps: {}", steps);
            }
            if let Some(params) = metrics.num_params_m {
                println!("num_params_M: {:.1}", params);
            }
            if let Some(depth) = metrics.depth {
                println!("depth: {}", depth);
            }
            if !metrics.extra.is_empty() {
                println!("\n--- Additional Metrics ---");
                for (name, value) in &metrics.extra {
                    println!("{}: {:.6}", name, value);
                }
            }
            println!("------------------------\n");
        } else {
            println!("No metrics found in output");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_pattern_compiles() {
        for (name, pattern) in DEFAULT_PATTERNS {
            assert!(Regex::new(pattern).is_ok(), "{name}: {pattern}");
        }
        assert_eq!(MetricParser::new().patterns.len(), DEFAULT_PATTERNS.len());
    }

    #[test]
    fn test_parse_val_bpb() {
        let parser = MetricParser::default();
        let output = "val_bpb: 0.997900\nother stuff";

        let result = parser.extract_val_bpb(output);
        assert!(result.is_some());
        assert!((result.unwrap() - 0.997900).abs() < 0.000001);
    }

    #[test]
    fn test_parse_multiple_metrics() {
        let parser = MetricParser::default();
        let output = r#"val_bpb: 0.997900
training_seconds: 300.1
total_seconds: 325.9
peak_vram_mb: 45060.2
mfu_percent: 39.80
total_tokens_M: 499.6
num_steps: 953
num_params_M: 50.3
depth: 8"#;

        let metrics = parser.extract_all(output).unwrap();

        assert!((metrics.val_bpb.unwrap() - 0.997900).abs() < 0.000001);
        assert!((metrics.peak_vram_mb.unwrap() - 45060.2).abs() < 0.1);
        assert_eq!(metrics.num_steps.unwrap(), 953);
        assert_eq!(metrics.depth.unwrap(), 8);
    }

    #[test]
    fn test_custom_regex() {
        let mut parser = MetricParser::new();
        parser
            .add_pattern("custom_metric", r"my_metric:\s*([\d.]+)")
            .unwrap();

        let output = "my_metric: 42.5\nother stuff";
        let result = parser.extract_with_regex(output, r"my_metric:\s*([\d.]+)");

        assert!(result.is_some());
        assert!((result.unwrap() - 42.5).abs() < 0.001);
    }

    #[test]
    fn test_memory_gb() {
        let parser = MetricParser::default();
        let output = "peak_vram_mb: 45060.2";

        let mb = parser
            .extract_with_regex(output, r"peak_vram_mb:\s*([\d.]+)")
            .unwrap();
        let gb = mb / 1024.0;

        assert!((gb - 44.0).abs() < 0.1);
    }
}
