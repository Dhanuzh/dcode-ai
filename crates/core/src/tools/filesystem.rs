use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;
use super::freshness::FileFreshness;

pub struct ReadFileTool {
    workspace_root: std::path::PathBuf,
    freshness: FileFreshness,
}

impl ReadFileTool {
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
impl ToolExecutor for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read the contents of a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file, relative to workspace root"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let path = call.input["path"].as_str().unwrap_or("");
        let full_path = self.workspace_root.join(path);

        // Verify the path stays inside the workspace
        match full_path.canonicalize() {
            Ok(canonical) if canonical.starts_with(&self.workspace_root) => {
                match tokio::fs::read_to_string(&canonical).await {
                    Ok(content) => {
                        self.freshness.note(&canonical);
                        ToolResult {
                            call_id: call.id.clone(),
                            success: true,
                            output: content,
                            error: None,
                        }
                    }
                    Err(e) => ToolResult {
                        call_id: call.id.clone(),
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to read file: {e}")),
                    },
                }
            }
            _ => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("Path is outside the workspace".into()),
            },
        }
    }
}
