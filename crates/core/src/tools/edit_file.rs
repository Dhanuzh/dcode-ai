use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;
use super::freshness::{FileFreshness, Freshness};

fn format_diff(old: &str, new: &str, path: &std::path::Path) -> String {
    let label = path.display().to_string();
    let diff = similar::TextDiff::from_lines(old, new);
    diff.unified_diff()
        .context_radius(3)
        .header(&format!("before/{label}"), &format!("after/{label}"))
        .to_string()
}

pub struct EditFileTool {
    workspace_root: std::path::PathBuf,
    freshness: FileFreshness,
}

impl EditFileTool {
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
impl ToolExecutor for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".into(),
            description: "PREFERRED for editing files: replace one exact string in an existing file (set replace_all to change every occurrence). Use apply_patch instead when making several edits to the same file at once; use replace_match only when the target string is ambiguous and you have line/column coordinates from a search.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_text": { "type": "string" },
                    "new_text": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_text", "new_text"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let path = call.input["path"].as_str().unwrap_or("");
        let old_text = call.input["old_text"].as_str().unwrap_or("");
        let new_text = call.input["new_text"].as_str().unwrap_or("");
        let replace_all = call.input["replace_all"].as_bool().unwrap_or(false);

        let workspace_root = dcode_ai_common::config::canonicalize_simplified(&self.workspace_root)
            .unwrap_or_else(|_| self.workspace_root.clone());
        let full_path = self.workspace_root.join(path);
        let canonical = match dcode_ai_common::config::canonicalize_simplified(&full_path) {
            Ok(canonical) if canonical.starts_with(&workspace_root) => canonical,
            _ => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some("Path is outside the workspace".into()),
                };
            }
        };

        if self.freshness.check(&canonical) == Freshness::StaleExternalEdit {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(FileFreshness::stale_error(&canonical)),
            };
        }

        let content = match tokio::fs::read_to_string(&canonical).await {
            Ok(content) => content,
            Err(err) => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read file: {err}")),
                };
            }
        };

        if old_text.is_empty() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("old_text must not be empty".into()),
            };
        }

        let occurrence_count = content.matches(old_text).count();
        if occurrence_count == 0 {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("old_text was not found".into()),
            };
        }

        let updated = if replace_all {
            content.replace(old_text, new_text)
        } else if occurrence_count > 1 {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!(
                    "old_text matched {occurrence_count} occurrences; use replace_all or replace_match for a precise edit"
                )),
            };
        } else if let Some(index) = content.find(old_text) {
            let mut updated = content.clone();
            updated.replace_range(index..index + old_text.len(), new_text);
            updated
        } else {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("old_text was not found".into()),
            };
        };

        let diff = format_diff(&content, &updated, &canonical);
        match tokio::fs::write(&canonical, &updated).await {
            Ok(()) => {
                self.freshness.note(&canonical);
                ToolResult {
                    call_id: call.id.clone(),
                    success: true,
                    output: if diff.trim().is_empty() {
                        format!(
                            "Edited {} (replaced {} occurrence{}, no changes)",
                            canonical.display(),
                            occurrence_count,
                            if occurrence_count == 1 { "" } else { "s" }
                        )
                    } else {
                        format!(
                            "Edited {} (replaced {} occurrence{})",
                            canonical.display(),
                            occurrence_count,
                            if occurrence_count == 1 { "" } else { "s" }
                        ) + &format!("\n\n{diff}")
                    },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_call(input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "edit_file".into(),
            input,
        }
    }

    #[tokio::test]
    async fn edit_file_rejects_ambiguous_single_replacements() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "alpha\nalpha\n").unwrap();

        let tool = EditFileTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(&make_call(serde_json::json!({
                "path": "main.rs",
                "old_text": "alpha",
                "new_text": "beta"
            })))
            .await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("replace_match"));
    }

    #[tokio::test]
    async fn edit_file_refuses_stale_target_after_external_edit() {
        use super::super::freshness::FileFreshness;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");
        std::fs::write(&path, "alpha\n").unwrap();

        let fresh = FileFreshness::new();
        let canonical = dcode_ai_common::config::canonicalize_simplified(&path).unwrap();
        fresh.note(&canonical);

        // External edit with a strictly newer mtime.
        std::fs::write(&path, "alpha // user changed this\n").unwrap();
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(newer)).unwrap();

        let tool = EditFileTool::with_freshness(dir.path().to_path_buf(), fresh);
        let result = tool
            .execute(&make_call(serde_json::json!({
                "path": "main.rs",
                "old_text": "alpha",
                "new_text": "beta"
            })))
            .await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("changed on disk"));
        // File content untouched.
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "alpha // user changed this\n");
    }

    #[tokio::test]
    async fn edit_file_replace_all_reports_replacement_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "alpha\nalpha\n").unwrap();

        let tool = EditFileTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(&make_call(serde_json::json!({
                "path": "main.rs",
                "old_text": "alpha",
                "new_text": "beta",
                "replace_all": true
            })))
            .await;

        assert!(result.success, "{result:?}");
        assert!(result.output.contains("replaced 2 occurrences"));
        let updated = std::fs::read_to_string(dir.path().join("main.rs")).unwrap();
        assert_eq!(updated, "beta\nbeta\n");
    }
}
