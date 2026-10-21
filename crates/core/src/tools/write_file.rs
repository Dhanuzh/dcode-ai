use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;
use super::freshness::{FileFreshness, Freshness};

fn format_diff(old: &str, new: &str) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    diff.unified_diff()
        .context_radius(3)
        .header("before", "after")
        .to_string()
}

pub struct WriteFileTool {
    workspace_root: std::path::PathBuf,
    freshness: FileFreshness,
}

impl WriteFileTool {
    pub fn new(workspace_root: std::path::PathBuf) -> Self {
        Self::with_freshness(workspace_root, FileFreshness::new())
    }

    pub fn with_freshness(workspace_root: std::path::PathBuf, freshness: FileFreshness) -> Self {
        Self {
            workspace_root,
            freshness,
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            description: "Create or overwrite a file inside the workspace. This replaces the ENTIRE file with content — for changing part of an existing file, prefer edit_file or apply_patch instead so unrelated content is not lost.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file, relative to the workspace root. Parent directories are created automatically."
                    },
                    "content": {
                        "type": "string",
                        "description": "The full contents to write. This is the entire file after the write — any existing content not included here is lost."
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let path = call.input["path"].as_str().unwrap_or("");
        let content = call.input["content"].as_str().unwrap_or("");
        let full_path = self.workspace_root.join(path);

        let parent = match full_path.parent() {
            Some(parent) => parent,
            None => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some("Invalid write path".into()),
                };
            }
        };

        if let Err(err) = tokio::fs::create_dir_all(parent).await {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("Failed to create parent directories: {err}")),
            };
        }

        let canonical_parent = match dcode_ai_common::config::canonicalize_simplified(parent) {
            Ok(path) => path,
            Err(err) => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve parent path: {err}")),
                };
            }
        };

        if !dcode_ai_common::workspace_scope::allows(&self.workspace_root, &canonical_parent) {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("Path is outside the workspace".into()),
            };
        }

        // Freshness is keyed by canonical path (read_file notes it that way);
        // resolve through the canonical parent so `./`/symlinked spellings of
        // the same file can't dodge the stale check.
        let canonical_target = full_path
            .file_name()
            .map(|name| canonical_parent.join(name))
            .unwrap_or_else(|| full_path.clone());

        if self.freshness.check(&canonical_target) == Freshness::StaleExternalEdit {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(FileFreshness::stale_error(&canonical_target)),
            };
        }

        let old = tokio::fs::read_to_string(&full_path)
            .await
            .unwrap_or_default();

        match tokio::fs::write(&full_path, content).await {
            Ok(()) => {
                self.freshness.note(&canonical_target);
                let diff = format_diff(&old, content);
                // Keep output machine- and human-friendly. The CLI can render this as-is.
                let output = if diff.trim().is_empty() {
                    format!("Wrote {} (no changes)", full_path.display())
                } else {
                    format!("Wrote {}\n\n{diff}", full_path.display())
                };

                ToolResult {
                    call_id: call.id.clone(),
                    success: true,
                    output,
                    error: None,
                }
            }
            Err(err) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("Failed to write file: {err}")),
            },
        }
    }
}
