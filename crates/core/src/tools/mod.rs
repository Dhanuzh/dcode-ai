pub mod apply_patch;
pub mod ask_question;
pub mod background_exec;
pub mod code_intel_tool;
pub mod copy_path;
pub mod create_directory;
pub mod delete_path;
pub mod edit_file;
pub mod fetch_url;
pub mod filesystem;
pub mod freshness;
pub mod git;
pub mod inspect_binary;
pub mod invoke_skill;
pub mod list_directory;
pub mod mcp;
pub mod move_path;
pub mod rename_path;
pub mod replace_match;
pub mod repo_map;
pub mod run_validation;
pub mod search;
pub mod spawn_subagent;
pub mod tool_search;
pub mod types;
pub mod update_plan;
pub mod web_search;
pub mod write_file;

pub use ask_question::AskQuestionTool;
pub use invoke_skill::InvokeSkillTool;

use dcode_ai_common::config::WebConfig;
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

/// Registry of available tools the agent can invoke.
pub struct ToolRegistry {
    tools: Vec<Box<dyn ToolExecutor>>,
    /// Tools left out of `definitions()` until `tool_search` activates them.
    deferred: std::collections::HashSet<String>,
    activated: tool_search::Activated,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            deferred: Default::default(),
            activated: Default::default(),
        }
    }

    /// Hide every tool whose name satisfies `pred` behind `tool_search`, and
    /// register that tool. Returns how many were deferred.
    pub fn defer_behind_search(&mut self, pred: impl Fn(&str) -> bool) -> usize {
        let defs: Vec<ToolDefinition> = self
            .tools
            .iter()
            .map(|t| t.definition())
            .filter(|d| pred(&d.name))
            .collect();
        if defs.is_empty() {
            return 0;
        }
        self.deferred.extend(defs.iter().map(|d| d.name.clone()));
        let n = defs.len();
        self.tools.retain(|t| t.definition().name != "tool_search");
        self.register(Box::new(tool_search::ToolSearchTool::new(
            defs,
            self.activated.clone(),
        )));
        n
    }

    pub fn register(&mut self, tool: Box<dyn ToolExecutor>) {
        self.tools.push(tool);
    }

    pub fn with_default_readonly_tools(
        workspace_root: std::path::PathBuf,
        web_config: WebConfig,
    ) -> Self {
        Self::with_default_readonly_tools_shared(
            workspace_root,
            web_config,
            freshness::FileFreshness::new(),
        )
    }

    fn with_default_readonly_tools_shared(
        workspace_root: std::path::PathBuf,
        web_config: WebConfig,
        fresh: freshness::FileFreshness,
    ) -> Self {
        let workspace_root = canonical_root(workspace_root);
        let mut registry = Self::new();
        registry.register(Box::new(filesystem::ReadFileTool::with_freshness(
            workspace_root.clone(),
            fresh,
        )));
        registry.register(Box::new(search::SearchCodeTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(list_directory::ListDirectoryTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(repo_map::RepoMapTool::new(workspace_root.clone())));
        registry.register(Box::new(git::GitStatusTool::new(workspace_root.clone())));
        registry.register(Box::new(git::GitDiffTool::new(workspace_root.clone())));
        registry.register(Box::new(inspect_binary::InspectBinaryTool::new(
            workspace_root,
        )));
        registry.register(Box::new(web_search::WebSearchTool::new(web_config.clone())));
        registry.register(Box::new(fetch_url::FetchUrlTool::new(web_config)));
        registry.register(Box::new(update_plan::UpdatePlanTool::new()));
        registry
    }

    pub fn with_default_full_tools(
        workspace_root: std::path::PathBuf,
        web_config: WebConfig,
    ) -> Self {
        let workspace_root = canonical_root(workspace_root);
        // One shared freshness tracker: reads note mtimes, writes verify them,
        // so external edits between a read and a write are caught.
        let fresh = freshness::FileFreshness::new();
        let mut registry = Self::with_default_readonly_tools_shared(
            workspace_root.clone(),
            web_config,
            fresh.clone(),
        );
        registry.register(Box::new(code_intel_tool::CodeIntelTool::new(
            crate::code_intel::WorkspaceCodeIntel::new(workspace_root.clone()),
        )));
        registry.register(Box::new(write_file::WriteFileTool::with_freshness(
            workspace_root.clone(),
            fresh.clone(),
        )));
        registry.register(Box::new(create_directory::CreateDirectoryTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(apply_patch::ApplyPatchTool::with_freshness(
            workspace_root.clone(),
            fresh.clone(),
        )));
        registry.register(Box::new(edit_file::EditFileTool::with_freshness(
            workspace_root.clone(),
            fresh.clone(),
        )));
        registry.register(Box::new(replace_match::ReplaceMatchTool::with_freshness(
            workspace_root.clone(),
            fresh,
        )));
        registry.register(Box::new(rename_path::RenamePathTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(move_path::MovePathTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(copy_path::CopyPathTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(delete_path::DeletePathTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(run_validation::RunValidationTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(background_exec::BackgroundExecTool::new(
            workspace_root,
        )));
        registry
    }

    /// Keep only tools whose name satisfies `keep` (custom agents with a
    /// `tools:` list).
    pub fn retain_by_name(&mut self, keep: impl Fn(&str) -> bool) {
        self.tools.retain(|t| keep(&t.definition().name));
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let active = self.activated.names();
        self.tools
            .iter()
            .map(|t| t.definition())
            .filter(|d| !self.deferred.contains(&d.name) || active.contains(&d.name))
            .collect()
    }

    pub async fn execute(&self, call: &ToolCall) -> ToolResult {
        if call.name == dcode_ai_common::tool::INVALID_TOOL_CALL {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Your tool call could not be read ({}): {}\nWrite it again on its own lines \
as <tool_call>{{\"name\": \"<tool>\", \"input\": {{...}}}}</tool_call>. If you were only \
mentioning the protocol in prose, put it in backticks.",
                    call.input["error"].as_str().unwrap_or("parse error"),
                    call.input["raw"].as_str().unwrap_or("")
                )),
            };
        }
        for tool in &self.tools {
            if tool.definition().name == call.name {
                return tool.execute(call).await;
            }
        }

        ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(format!("Unknown tool: {}", call.name)),
        }
    }
}

/// Tools check `canonical(path).starts_with(workspace_root)`, so the root must
/// be canonical too (Windows 8.3 short names like `RUNNER~1`, symlinks).
fn canonical_root(root: std::path::PathBuf) -> std::path::PathBuf {
    dcode_ai_common::config::canonicalize_simplified(&root).unwrap_or(root)
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait implemented by each tool.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, call: &ToolCall) -> ToolResult;
}
