//! `dcode-ai ci`: one non-interactive task for pipelines.
//!
//! Prints a JSON result to stdout (and optionally a file),
//! appends a Markdown summary to `$GITHUB_STEP_SUMMARY` when set, and maps
//! the outcome to exit codes: 0 success, 1 failure, 2 budget reached.

use std::path::{Path, PathBuf};

use clap::Args;
use dcode_ai_common::config::{DcodeAiConfig, PermissionMode};
use dcode_ai_common::event::EndReason;
use serde::Serialize;

use crate::runner::build_session_runtime;
use crate::stream::{StreamMode, spawn_stream_task};

#[derive(Debug, Args)]
pub struct CiArgs {
    /// The task (or use --prompt-file)
    #[arg(long, short = 'p')]
    pub prompt: Option<String>,
    /// Read the task from a file (`-` for stdin)
    #[arg(long)]
    pub prompt_file: Option<PathBuf>,
    #[arg(long)]
    pub model: Option<String>,
    /// Let the agent edit files (default: read-only review)
    #[arg(long)]
    pub allow_writes: bool,
    /// Let the agent run shell commands too (implies --allow-writes)
    #[arg(long)]
    pub allow_shell: bool,
    /// Stop when the estimated cost reaches this many USD
    #[arg(long)]
    pub max_cost: Option<f64>,
    /// Also write the JSON result to this file
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// Live progress on stdout before the JSON result: off (default), human, ndjson
    #[arg(long, value_enum, default_value_t = StreamMode::Off)]
    pub stream: StreamMode,
    /// Run the security review of this branch's changes against BASE
    /// (e.g. origin/main) instead of a task; findings are verified or
    /// marked unverified
    #[arg(long, value_name = "BASE")]
    pub security_review: Option<String>,
    /// Write the review's findings as SARIF 2.1.0 (GitHub code scanning)
    #[arg(long)]
    pub sarif: Option<PathBuf>,
    /// Exit 3 when a verified finding is at least this severe
    #[arg(long, value_enum)]
    pub fail_on: Option<crate::security_findings::Severity>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct CiResult {
    pub status: &'static str,
    pub session_id: String,
    pub model: String,
    pub output: String,
    pub error: Option<String>,
    pub changed_files: Vec<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost_usd: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<crate::security_findings::Finding>,
}

impl CiResult {
    pub fn exit_code(&self) -> u8 {
        match self.status {
            "completed" => 0,
            "findings" => 3,
            "budget_exceeded" => 2,
            _ => 1,
        }
    }

    /// Markdown for `$GITHUB_STEP_SUMMARY`.
    pub fn markdown(&self) -> String {
        let icon = match self.status {
            "completed" => "✅",
            "findings" => "🛡️",
            "budget_exceeded" => "💸",
            _ => "❌",
        };
        let mut md = format!(
            "### {icon} dcode-ai: {}\n\n| | |\n|---|---|\n| model | `{}` |\n| tokens | {} in / {} out |\n| est. cost | ${:.4} |\n| session | `{}` |\n",
            self.status,
            self.model,
            self.input_tokens,
            self.output_tokens,
            self.estimated_cost_usd,
            self.session_id
        );
        if !self.changed_files.is_empty() {
            md.push_str("\n**Changed files**\n\n");
            for f in &self.changed_files {
                md.push_str(&format!("- `{f}`\n"));
            }
        }
        if let Some(e) = &self.error {
            md.push_str(&format!("\n**Error**\n\n```\n{e}\n```\n"));
        }
        if !self.output.trim().is_empty() {
            md.push_str(&format!("\n**Result**\n\n{}\n", self.output.trim()));
        }
        md
    }
}

fn read_prompt(args: &CiArgs) -> anyhow::Result<String> {
    if let Some(base) = &args.security_review {
        let def = dcode_ai_core::agent_defs::security_reviewer();
        return Ok(format!(
            "{}\n\nTask: review the changes on this branch against `{base}` \
             (git diff {base}...HEAD).",
            def.prompt
        ));
    }
    let text = match (&args.prompt, &args.prompt_file) {
        (Some(p), None) => p.clone(),
        (None, Some(path)) if path.as_os_str() == "-" => std::io::read_to_string(std::io::stdin())?,
        (None, Some(path)) => {
            std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?
        }
        (Some(_), Some(_)) => anyhow::bail!("use --prompt or --prompt-file, not both"),
        (None, None) => anyhow::bail!("a task is required: --prompt \"…\" or --prompt-file FILE"),
    };
    if text.trim().is_empty() {
        anyhow::bail!("the task is empty");
    }
    Ok(text)
}

/// Permission mode for the run: read-only unless writes/shell are allowed.
pub fn ci_permission_mode(allow_writes: bool, allow_shell: bool) -> PermissionMode {
    if allow_shell {
        PermissionMode::BypassPermissions
    } else if allow_writes {
        PermissionMode::AcceptEdits
    } else {
        PermissionMode::DontAsk
    }
}

fn changed_files(workspace: &Path) -> Vec<String> {
    std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.get(3..).map(str::to_string))
                .filter(|p| !p.starts_with(".dcode-ai/"))
                .collect()
        })
        .unwrap_or_default()
}

pub async fn run(
    mut config: DcodeAiConfig,
    workspace_root: &Path,
    args: CiArgs,
) -> anyhow::Result<u8> {
    let prompt = read_prompt(&args)?;
    if let Some(model) = &args.model {
        config.apply_model_override(model);
    }
    config.permissions.mode = ci_permission_mode(args.allow_writes, args.allow_shell);
    config.permissions.startup_approve_all = false;
    if let Some(max) = args.max_cost {
        config.session.max_cost_usd = max.max(0.0);
    }
    let before: Vec<String> = changed_files(workspace_root);

    let mut runtime = build_session_runtime(
        config.clone(),
        workspace_root,
        false,
        false,
        None,
        None,
        None,
    )
    .await
    .map_err(anyhow::Error::msg)?;

    let stream_task = runtime.take_event_rx().map(|rx| {
        let ipc = runtime.take_ipc_handle();
        let pending = runtime.take_ipc_approval_pending();
        spawn_stream_task(
            rx,
            args.stream,
            runtime.event_log_path(),
            ipc,
            pending,
            runtime.question_pending(),
            None,
        )
    });
    let spawn_task = runtime.take_spawn_rx().map(|spawn_rx| {
        dcode_ai_runtime::supervisor::spawn_subagent_consumer(
            spawn_rx,
            runtime.session_id().to_string(),
            runtime.workspace_root().to_path_buf(),
            config.clone(),
            runtime.messages().to_vec(),
            None,
        )
    });

    let outcome = runtime.run_turn(&prompt).await;
    let (status, output, error) = match outcome {
        Ok(text) => {
            runtime.finish(EndReason::Completed).await;
            ("completed", text, None)
        }
        Err(e) => {
            runtime.finish(EndReason::Error).await;
            let msg = e.to_string();
            let status = if msg.contains("cost budget reached") {
                "budget_exceeded"
            } else {
                "failed"
            };
            (status, String::new(), Some(msg))
        }
    };
    if let Some(t) = stream_task {
        t.abort();
    }
    if let Some(t) = spawn_task {
        t.abort();
    }

    let findings = crate::security_findings::parse(&output);
    let status = match args.fail_on {
        Some(t) if status == "completed" && crate::security_findings::fails(&findings, t) => {
            "findings"
        }
        _ => status,
    };
    if let Some(path) = &args.sarif {
        std::fs::write(
            path,
            serde_json::to_string_pretty(&crate::security_findings::sarif(&findings))?,
        )?;
    }

    let snap = runtime.snapshot();
    let result = CiResult {
        findings,
        status,
        session_id: snap.id.clone(),
        model: snap.model.clone(),
        output,
        error,
        changed_files: changed_files(workspace_root)
            .into_iter()
            .filter(|f| !before.contains(f))
            .collect(),
        input_tokens: snap.total_input_tokens,
        output_tokens: snap.total_output_tokens,
        estimated_cost_usd: snap.estimated_cost_usd,
    };
    let json = serde_json::to_string_pretty(&result)?;
    println!("{json}");
    if let Some(path) = &args.output {
        std::fs::write(path, &json)?;
    }
    if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY")
        && !summary.is_empty()
    {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&summary)
        {
            let _ = writeln!(f, "{}", result.markdown());
        }
    }
    Ok(result.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(status: &'static str) -> CiResult {
        CiResult {
            status,
            session_id: "s1".into(),
            model: "m".into(),
            output: "Fixed it.".into(),
            error: None,
            changed_files: vec!["src/a.rs".into()],
            input_tokens: 10,
            output_tokens: 5,
            estimated_cost_usd: 0.0123,
            findings: Vec::new(),
        }
    }

    #[test]
    fn verified_findings_get_their_own_exit_code() {
        assert_eq!(result("findings").exit_code(), 3);
    }

    #[test]
    fn exit_codes_and_permissions() {
        assert_eq!(result("completed").exit_code(), 0);
        assert_eq!(result("failed").exit_code(), 1);
        assert_eq!(result("budget_exceeded").exit_code(), 2);
        assert_eq!(ci_permission_mode(false, false), PermissionMode::DontAsk);
        assert_eq!(ci_permission_mode(true, false), PermissionMode::AcceptEdits);
        assert_eq!(
            ci_permission_mode(false, true),
            PermissionMode::BypassPermissions
        );
    }

    #[test]
    fn markdown_summary_lists_changes_and_result() {
        let md = result("completed").markdown();
        assert!(md.starts_with("### ✅ dcode-ai: completed"));
        assert!(md.contains("- `src/a.rs`"));
        assert!(md.contains("$0.0123"));
        assert!(md.contains("Fixed it."));
    }
}
