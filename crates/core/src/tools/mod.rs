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
pub mod notebook;
pub mod rename_path;
pub mod replace_match;
pub mod repo_map;
pub mod run_script;
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
    /// The tool list has been sent to a model: from now on it only grows at
    /// its end, so the provider's prompt cache stays valid.
    sent: std::sync::atomic::AtomicBool,
    /// The names `tool_search`'s description lists (frozen once sent).
    search_listed: Vec<String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            deferred: Default::default(),
            activated: Default::default(),
            sent: Default::default(),
            search_listed: Vec::new(),
        }
    }

    /// How many registered tools (deferred ones stay registered) satisfy `pred`.
    pub fn count_matching(&self, pred: impl Fn(&str) -> bool) -> usize {
        self.tools
            .iter()
            .filter(|t| pred(&t.definition().name))
            .count()
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
        // One search covers everything deferred so far (built-ins and MCP).
        let all: Vec<ToolDefinition> = self
            .tools
            .iter()
            .map(|t| t.definition())
            .filter(|d| self.deferred.contains(&d.name))
            .collect();
        // Once the list has been sent, `tool_search` keeps its description
        // and its place: tools deferred later are searchable, not listed.
        let sent = self.sent.load(std::sync::atomic::Ordering::Relaxed);
        let listed: Vec<String> = if sent && !self.search_listed.is_empty() {
            self.search_listed.clone()
        } else {
            all.iter().map(|d| d.name.clone()).collect()
        };
        self.search_listed = listed.clone();
        let search = Box::new(tool_search::ToolSearchTool::with_listed(
            all,
            self.activated.clone(),
            listed,
        ));
        match self
            .tools
            .iter()
            .position(|t| t.definition().name == "tool_search")
        {
            Some(i) => self.tools[i] = search,
            None => self.register(search),
        }
        n
    }

    /// Tools that arrive after the list was sent (MCP servers that connected
    /// late) go behind `tool_search` whatever their number, so the list the
    /// provider caches doesn't change. Returns their names, to announce.
    pub fn defer_late(&mut self, pred: impl Fn(&str) -> bool) -> Vec<String> {
        let new: Vec<String> = self
            .tools
            .iter()
            .map(|t| t.definition().name)
            .filter(|n| pred(n) && !self.deferred.contains(n))
            .collect();
        if !new.is_empty() {
            let set: std::collections::HashSet<String> = new.iter().cloned().collect();
            self.defer_behind_search(|n| set.contains(n));
        }
        new
    }

    /// Load a deferred tool now (as if `tool_search` found it) — e.g. the
    /// browser once the user connects an app. False when it isn't deferred.
    pub fn activate(&self, name: &str) -> bool {
        if !self.deferred.contains(name) {
            return false;
        }
        self.activated.add(name);
        true
    }

    /// Whether the list has been sent to a model yet.
    pub fn list_sent(&self) -> bool {
        self.sent.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Built-in tools used rarely enough to load only on demand: their
    /// schemas would otherwise be sent with every request.
    pub const RARELY_USED: [&'static str; 7] = [
        "browser",
        "notebook_edit",
        "inspect_binary",
        "copy_path",
        "move_path",
        "rename_path",
        "query_symbols",
    ];

    /// Defer [`Self::RARELY_USED`] behind `tool_search`.
    pub fn defer_rarely_used(&mut self) -> usize {
        self.defer_behind_search(|name| Self::RARELY_USED.contains(&name))
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
        registry.register(Box::new(notebook::NotebookEditTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(crate::browser::BrowserTool::new(
            workspace_root.clone(),
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

    /// The tools to send: the regular ones in registration order, then the
    /// ones `tool_search` loaded, in the order it loaded them — so the list
    /// only ever grows at its end and the prompt cache keeps its prefix.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.sent.store(true, std::sync::atomic::Ordering::Relaxed);
        let all: Vec<ToolDefinition> = self.tools.iter().map(|t| t.definition()).collect();
        let mut out: Vec<ToolDefinition> = all
            .iter()
            .filter(|d| !self.deferred.contains(&d.name))
            .cloned()
            .collect();
        for name in self.activated.names() {
            if let Some(d) = all
                .iter()
                .find(|d| d.name == name && self.deferred.contains(&d.name))
            {
                out.push(d.clone());
            }
        }
        out
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
