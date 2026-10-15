use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;
use crate::code_map::build_repo_map;

/// Returns a ranked map of the most-referenced source files in the workspace,
/// a quick way to orient in an unfamiliar codebase before reading specific files.
pub struct RepoMapTool {
    workspace_root: std::path::PathBuf,
}

impl RepoMapTool {
    pub fn new(workspace_root: std::path::PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for RepoMapTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "repo_map".into(),
            description:
                "Rank the workspace's source files by how many other files reference them \
                (a reference-graph heuristic). Use to find the hub/central files when orienting in \
                an unfamiliar codebase. Returns paths with reference scores."
                    .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "max_files": {
                        "type": "integer",
                        "description": "Max files to return (default 30)."
                    }
                }
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let max_files = call.input["max_files"].as_u64().unwrap_or(30).clamp(1, 200) as usize;
        let root = self.workspace_root.clone();

        let ranked = tokio::task::spawn_blocking(move || build_repo_map(&root, max_files)).await;

        match ranked {
            Ok(ranked) => {
                if ranked.is_empty() {
                    return ToolResult {
                        call_id: call.id.clone(),
                        success: true,
                        output: "No source files found to map.".into(),
                        error: None,
                    };
                }
                let mut out = String::from("Repo map (file — inbound references):\n");
                for f in &ranked {
                    out.push_str(&format!("{:>4}  {}\n", f.score, f.path));
                }
                ToolResult {
                    call_id: call.id.clone(),
                    success: true,
                    output: out,
                    error: None,
                }
            }
            Err(err) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("repo_map failed: {err}")),
            },
        }
    }
}
