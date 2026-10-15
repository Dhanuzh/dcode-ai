//! Research program definition and parsing
//!
//! A research program defines the parameters for autonomous research:
//! - Which files can be edited
//! - Which files are fixed (read-only)
//! - How to extract the metric from experiment output
//! - Time budget and constraints

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Metric optimization goal
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MetricGoal {
    /// Lower is better (e.g., val_bpb, loss)
    #[default]
    Minimize,
    /// Higher is better (e.g., accuracy)
    Maximize,
}

impl std::fmt::Display for MetricGoal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetricGoal::Minimize => write!(f, "minimize"),
            MetricGoal::Maximize => write!(f, "maximize"),
        }
    }
}

/// Command to extract metric from experiment output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricCommand {
    /// The shell command to run (e.g., `grep "^val_bpb:" run.log`)
    pub command: String,
    /// Regex with one capture group to extract the numeric value
    pub parse_regex: String,
}

/// File that the agent is allowed to edit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditableFile {
    pub path: PathBuf,
    pub description: Option<String>,
}

impl EditableFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            description: None,
        }
    }

    pub fn with_description(path: impl Into<PathBuf>, description: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            description: Some(description.into()),
        }
    }
}

/// File that is fixed (read-only) for the experiment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixedFile {
    pub path: PathBuf,
    pub reason: String,
}

/// Research program definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchProgram {
    /// Human-readable name for the program
    pub name: String,
    /// Detailed description of the research goal
    pub description: String,
    /// Files the agent can modify
    pub editable_files: Vec<EditableFile>,
    /// Files that are fixed (read-only)
    pub fixed_files: Vec<FixedFile>,
    /// How to extract the metric from output
    pub metric_command: MetricCommand,
    /// Whether to minimize or maximize the metric
    pub metric_goal: MetricGoal,
    /// Time budget per experiment in seconds
    pub time_budget_seconds: u64,
    /// Additional constraints (e.g., "max_memory_gb: 50")
    pub extra_constraints: Vec<String>,
    /// Maximum memory usage in GB (soft constraint)
    pub max_memory_gb: Option<f64>,
    /// Instructions for the agent
    pub instructions: String,
}

impl ResearchProgram {
    /// Parse a research program from a markdown file
    ///
    /// The format follows karpathy/autoresearch's `program.md`:
    /// ```markdown
    /// # My Research Program
    ///
    /// ## Files
    /// - Editable: `train.py` — model, optimizer, training loop
    /// - Fixed: `prepare.py` — data prep, tokenizer (do not modify)
    ///
    /// ## Metric
    /// - Command: `grep "^val_bpb:" run.log | cut -d: -f2`
    /// - Goal: minimize
    /// - Baseline: 0.997900
    ///
    /// ## Constraints
    /// - Time budget: 300 seconds
    /// - Max memory: 50GB
    ///
    /// ## Instructions
    /// You are an autonomous researcher...
    /// ```
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read program file: {:?}", path.as_ref()))?;

        Self::from_markdown(&content)
    }

    /// Parse a research program from markdown content
    pub fn from_markdown(content: &str) -> Result<Self> {
        let mut name = String::new();
        let mut description = String::new();
        let mut editable_files = Vec::new();
        let mut fixed_files = Vec::new();
        let mut metric_command = MetricCommand {
            command: String::new(),
            parse_regex: String::new(),
        };
        let mut metric_goal = MetricGoal::Minimize;
        let mut time_budget_seconds = 300u64;
        let mut extra_constraints = Vec::new();
        let mut max_memory_gb = None;
        let mut instructions = String::new();
        let mut instructions_mode = false;

        let lines: Vec<&str> = content.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i].trim();

            // Check for section headers
            if line.starts_with("# ") {
                name = line.trim_start_matches("# ").trim().to_string();
            } else if line.starts_with("## ") {
                let section = line.trim_start_matches("## ").trim().to_lowercase();
                match section.as_str() {
                    "files" | "file configuration" => {
                        // Parse file list
                        i += 1;
                        while i < lines.len()
                            && !lines[i].trim().is_empty()
                            && !lines[i].trim().starts_with("## ")
                        {
                            let file_line = lines[i].trim();
                            if file_line.starts_with('-') || file_line.starts_with('*') {
                                let file_content = file_line
                                    .trim_start_matches('-')
                                    .trim_start_matches('*')
                                    .trim();
                                if file_content.to_lowercase().contains("editable") {
                                    if let Some(path) = extract_file_path(file_content) {
                                        editable_files.push(EditableFile::new(path));
                                    }
                                } else if file_content.to_lowercase().contains("fixed") {
                                    if let Some(path) = extract_file_path(file_content) {
                                        fixed_files.push(FixedFile {
                                            path,
                                            reason: "Fixed by research program".to_string(),
                                        });
                                    }
                                } else if let Some(path) = extract_file_path(file_content) {
                                    // Default to editable if not specified
                                    editable_files.push(EditableFile::new(path));
                                }
                            }
                            i += 1;
                        }
                        continue;
                    }
                    "metric" | "metrics" => {
                        // Parse metric configuration
                        i += 1;
                        while i < lines.len()
                            && !lines[i].trim().is_empty()
                            && !lines[i].trim().starts_with("## ")
                        {
                            let metric_line = lines[i].trim().to_lowercase();
                            if metric_line.contains("command:") || metric_line.contains("cmd:") {
                                let cmd = extract_value(lines[i], vec!["command:", "cmd:"]);
                                if let Some(cmd) = cmd {
                                    metric_command.command = cmd.trim().to_string();
                                }
                            } else if metric_line.contains("regex:")
                                || metric_line.contains("parse:")
                            {
                                let regex = extract_value(lines[i], vec!["regex:", "parse:"]);
                                if let Some(regex) = regex {
                                    // Strip backticks from regex value
                                    let regex = regex.trim().trim_matches('`');
                                    metric_command.parse_regex = regex.to_string();
                                }
                            } else if metric_line.contains("goal:") {
                                if metric_line.contains("minimize") || metric_line.contains("lower")
                                {
                                    metric_goal = MetricGoal::Minimize;
                                } else if metric_line.contains("maximize")
                                    || metric_line.contains("higher")
                                {
                                    metric_goal = MetricGoal::Maximize;
                                }
                            }
                            i += 1;
                        }
                        continue;
                    }
                    "constraints" | "settings" | "config" => {
                        i += 1;
                        while i < lines.len()
                            && !lines[i].trim().is_empty()
                            && !lines[i].trim().starts_with("## ")
                        {
                            let constraint_line = lines[i].trim().to_lowercase();
                            if constraint_line.contains("time budget")
                                || constraint_line.contains("timeout")
                            {
                                if let Some(secs) = extract_number(lines[i]) {
                                    time_budget_seconds = secs;
                                }
                            } else if constraint_line.contains("max memory")
                                || constraint_line.contains("memory limit")
                            {
                                if let Some(mem) = extract_float(lines[i]) {
                                    max_memory_gb = Some(mem);
                                }
                            } else if !lines[i].trim().starts_with('-')
                                && !lines[i].trim().starts_with('*')
                            {
                                extra_constraints.push(lines[i].trim().to_string());
                            }
                            i += 1;
                        }
                        continue;
                    }
                    "instructions" | "research instructions" => {
                        instructions_mode = true;
                        i += 1;
                        continue;
                    }
                    _ => {}
                }
            } else if instructions_mode {
                if line.starts_with("## ") || (line.starts_with('#') && !instructions_mode) {
                    instructions_mode = false;
                } else if !line.is_empty() {
                    if !instructions.is_empty() {
                        instructions.push('\n');
                    }
                    instructions.push_str(line);
                }
            } else if !line.is_empty()
                && !line.starts_with('#')
                && !line.starts_with('-')
                && !line.starts_with('*')
            {
                // Accumulate as description
                if !description.is_empty() {
                    description.push(' ');
                }
                description.push_str(line);
            }

            i += 1;
        }

        // Validate required fields
        if name.is_empty() {
            name = "Unnamed Research".to_string();
        }
        if metric_command.command.is_empty() {
            return Err(anyhow::anyhow!(
                "Missing metric command in research program"
            ));
        }
        if metric_command.parse_regex.is_empty() {
            // Try to use a sensible default
            metric_command.parse_regex = r"([\d.]+)".to_string();
        }

        Ok(Self {
            name,
            description,
            editable_files,
            fixed_files,
            metric_command,
            metric_goal,
            time_budget_seconds,
            extra_constraints,
            max_memory_gb,
            instructions,
        })
    }

    /// Get default instructions based on the program
    pub fn default_instructions(&self) -> String {
        let mut instructions = String::new();

        instructions.push_str(&format!("# Research Program: {}\n\n", self.name));
        instructions.push_str(&format!("{}\n\n", self.description));

        instructions.push_str("## Files\n");
        instructions.push_str("- **Editable files**: You may modify these:\n");
        for file in &self.editable_files {
            instructions.push_str(&format!("  - `{}`", file.path.display()));
            if let Some(desc) = &file.description {
                instructions.push_str(&format!(" — {}", desc));
            }
            instructions.push('\n');
        }
        instructions.push_str("- **Fixed files**: Do NOT modify:\n");
        for file in &self.fixed_files {
            instructions.push_str(&format!("  - `{}`", file.path.display()));
            if !file.reason.is_empty() {
                instructions.push_str(&format!(" — {}", file.reason));
            }
            instructions.push('\n');
        }

        instructions.push_str("\n## Metric\n");
        instructions.push_str(&format!(
            "- **Command**: `{}`\n",
            self.metric_command.command
        ));
        instructions.push_str(&format!("- **Goal**: {}\n", self.metric_goal));
        instructions.push_str(&format!(
            "- **Time budget**: {} seconds\n",
            self.time_budget_seconds
        ));

        if let Some(max_mem) = self.max_memory_gb {
            instructions.push_str(&format!("- **Max memory**: {} GB\n", max_mem));
        }

        if !self.extra_constraints.is_empty() {
            instructions.push_str("\n## Constraints\n");
            for constraint in &self.extra_constraints {
                instructions.push_str(&format!("- {}\n", constraint));
            }
        }

        if !self.instructions.is_empty() {
            instructions.push_str(&format!(
                "\n## Research Instructions\n{}\n",
                self.instructions
            ));
        }

        instructions
    }

    /// Generate a summary for the agent prompt
    pub fn to_prompt_section(&self) -> String {
        let goal_text = match self.metric_goal {
            MetricGoal::Minimize => "lower is better",
            MetricGoal::Maximize => "higher is better",
        };

        let mut section = format!(
            r#"# Autonomous Research Mode

You are running in **autonomous research mode**. Your goal is to improve the metric `{metric}` where {goal_text}.

## Current State
- Research program: {name}
- Experiments run: {{experiment_count}}
- Current best metric: {{best_metric}}
- Baseline metric: {{baseline_metric}}

## Files You May Edit
"#,
            metric = self.metric_command.command,
            goal_text = goal_text,
            name = self.name
        );

        for file in &self.editable_files {
            section.push_str(&format!("- `{}`\n", file.path.display()));
        }

        section.push_str("\n## Files You Must NOT Edit\n");
        for file in &self.fixed_files {
            section.push_str(&format!("- `{}`\n", file.path.display()));
        }

        section.push_str(&format!(
            "\n## Experiment Configuration
- **Time budget**: {} seconds per experiment
- **Metric extraction**: `{}`
- **Goal**: {}
",
            self.time_budget_seconds, self.metric_command.command, goal_text
        ));

        if let Some(max_mem) = self.max_memory_gb {
            section.push_str(&format!("- **Max memory**: {} GB\n", max_mem));
        }

        if !self.extra_constraints.is_empty() {
            section.push_str("\n## Additional Constraints\n");
            for constraint in &self.extra_constraints {
                section.push_str(&format!("- {}\n", constraint));
            }
        }

        if !self.instructions.is_empty() {
            section.push_str(&format!(
                "\n## Research Instructions\n{}\n",
                self.instructions
            ));
        }

        section.push_str(
            r#"
## The Experiment Loop

1. Make a modification to an editable file
2. Commit your change with a descriptive message
3. Run the experiment: `dcode-ai-autoresearch run`
4. Extract the metric from output
5. If metric improved → keep the commit
6. If metric is worse or equal → revert the commit
7. If crashed → fix and retry, or skip with "crash" status
8. Repeat

## Results Log
Record each experiment in `results.tsv` (tab-separated):
```
commit	val_bpb	memory_gb	status	description
```

## Important Rules
- Do NOT modify fixed files
- Do NOT ask for permission to continue after setup
- Run experiments autonomously until interrupted
- If out of ideas, think harder or try combining previous approaches
"#,
        );

        section
    }
}

/// Extract file path from a line like "- Editable: `train.py`" or "- `train.py`"
fn extract_file_path(line: &str) -> Option<PathBuf> {
    // Try to find backtick-quoted path
    if let Some(start) = line.find('`')
        && let Some(end) = line[start + 1..].find('`')
    {
        return Some(PathBuf::from(&line[start + 1..start + 1 + end]));
    }
    // Try to find quoted path
    if let Some(start) = line.find('"')
        && let Some(end) = line[start + 1..].find('"')
    {
        return Some(PathBuf::from(&line[start + 1..start + 1 + end]));
    }
    // Try to extract the first word-like token that looks like a file path.
    // This handles cases like "train.py — model code" where em-dash isn't whitespace.
    for token in line.split(|c: char| {
        c.is_ascii_whitespace()
            || c == '—'
            || c == '–'
            || c == '-'
            || c == ':'
            || c == '('
            || c == ')'
            || c == '['
            || c == ']'
    }) {
        let t = token.trim();
        // Skip empty or punctuation-only
        if t.is_empty() {
            continue;
        }
        // Skip if it starts with common non-path markers
        if t.starts_with('#') || t.starts_with('-') || t.starts_with('*') {
            continue;
        }
        // Accept if it looks like a file (has a known extension, or looks like a path segment)
        if t.contains('.') || t.starts_with("./") || t.starts_with("../") {
            // Trim trailing punctuation
            let clean = t.trim_end_matches(|c: char| {
                c == '.' || c == ',' || c == ';' || c == '"' || c == '\''
            });
            if !clean.is_empty() && clean != "-" && clean != "—" {
                return Some(PathBuf::from(clean));
            }
        }
    }
    // Fallback: last space-separated word
    let parts: Vec<&str> = line.split_whitespace().collect();
    if let Some(last) = parts.last() {
        let last = last.trim_matches(|c| c == '`' || c == ',' || c == '.');
        if !last.is_empty() && !last.starts_with('-') && !last.starts_with('*') {
            return Some(PathBuf::from(last));
        }
    }
    None
}

/// Extract value after a key (handles various formats)
fn extract_value<'a>(line: &'a str, keys: Vec<&str>) -> Option<&'a str> {
    let line_lower = line.to_lowercase();
    for key in keys {
        if let Some(pos) = line_lower.find(key) {
            let _after = line[pos..]
                .trim_start_matches(|c: char| !c.is_alphanumeric() && c != ':' && c != '_');
            // Find the actual value after the key
            let key_len = key.len();
            let remaining = &line[pos + key_len..];
            let value = remaining.trim_start_matches(|c: char| {
                c == ':' || c == ' ' || c == '`' || c == '"' || c == '\''
            });
            if !value.is_empty() {
                return Some(value.trim_end_matches(['`', '"', '\'']));
            }
        }
    }
    None
}

/// Extract a number from a line (for time budgets)
fn extract_number(line: &str) -> Option<u64> {
    let re = regex::Regex::new(r"(\d+)").ok()?;
    if let Some(caps) = re.captures(line)
        && let Some(num) = caps.get(1)
    {
        return num.as_str().parse().ok();
    }
    None
}

/// Extract a float from a line (for memory values)
fn extract_float(line: &str) -> Option<f64> {
    let re = regex::Regex::new(r"([\d.]+)").ok()?;
    if let Some(caps) = re.captures(line)
        && let Some(num) = caps.get(1)
    {
        return num.as_str().parse().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_program() {
        let content = r#"# Test Program

## Metric
- Command: `grep "val_bpb:" run.log`
- Regex: `val_bpb:\s*([\d.]+)`
- Goal: minimize

## Instructions
Run experiments to improve the model.
"#;

        let program = ResearchProgram::from_markdown(content).unwrap();
        assert_eq!(program.name, "Test Program");
        assert_eq!(program.metric_command.command, "grep \"val_bpb:\" run.log");
        assert_eq!(program.metric_goal, MetricGoal::Minimize);
        assert_eq!(program.time_budget_seconds, 300);
    }

    #[test]
    fn test_parse_full_program() {
        let content = r#"# NanoGPT Autoresearch

This is an experiment to have the LLM do its own research on nanoGPT training.

## Files
- Editable: `train.py` — model architecture, optimizer, training loop
- Fixed: `prepare.py` — data prep, tokenizer (do not modify)

## Metric
- Command: `grep "^val_bpb:" run.log`
- Regex: `val_bpb:\s*([\d.]+)`
- Goal: minimize

## Constraints
- Time budget: 300 seconds
- Max memory: 50GB

## Instructions
Modify train.py to improve val_bpb. Changes are kept if metric improves.
"#;

        let program = ResearchProgram::from_markdown(content).unwrap();
        assert_eq!(program.name, "NanoGPT Autoresearch");
        assert!(!program.editable_files.is_empty());
        assert!(!program.fixed_files.is_empty());
        assert_eq!(program.time_budget_seconds, 300);
        assert_eq!(program.max_memory_gb, Some(50.0));
    }

    #[test]
    fn test_default_instructions() {
        let program = ResearchProgram {
            name: "Test".to_string(),
            description: "Test description".to_string(),
            editable_files: vec![EditableFile::new("train.py")],
            fixed_files: vec![FixedFile {
                path: PathBuf::from("prepare.py"),
                reason: "Fixed".to_string(),
            }],
            metric_command: MetricCommand {
                command: "grep val".to_string(),
                parse_regex: r"([\d.]+)".to_string(),
            },
            metric_goal: MetricGoal::Minimize,
            time_budget_seconds: 300,
            extra_constraints: vec![],
            max_memory_gb: None,
            instructions: String::new(),
        };

        let instructions = program.default_instructions();
        assert!(instructions.contains("Research Program: Test"));
        assert!(instructions.contains("Editable files"));
        assert!(instructions.contains("train.py"));
        assert!(instructions.contains("Fixed files"));
        assert!(instructions.contains("prepare.py"));
    }

    #[test]
    fn test_extract_file_path() {
        assert_eq!(
            extract_file_path("- Editable: `train.py`"),
            Some(PathBuf::from("train.py"))
        );
        assert_eq!(
            extract_file_path("- Fixed: `prepare.py`"),
            Some(PathBuf::from("prepare.py"))
        );
        assert_eq!(
            extract_file_path("train.py — model code"),
            Some(PathBuf::from("train.py"))
        );
    }
}
