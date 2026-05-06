//! Tool-effect classification for the koda-style TUI surfaces.
//!
//! Ported from koda-core/src/tools/mod.rs (MIT, koda project) so the
//! koda-derived rendering modules (`koda_theme`, `tool_header`, …) can
//! classify tool calls without an upstream koda dependency.
//!
//! Two-axis model: what does the tool touch (local vs. remote), and how
//! severe are its effects (read vs. mutate vs. destroy)?

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    /// No side-effects: file reads, grep, git status.
    ReadOnly,
    /// Side-effects on remote services only: GitHub API, WebFetch POST.
    RemoteAction,
    /// Mutates local filesystem or state: Write, Edit, Delete, MemoryWrite.
    LocalMutation,
    /// Irreversible or high-blast-radius: rm -rf, git push --force.
    Destructive,
}

/// Classify a built-in tool by name. Unknown tools default to
/// `LocalMutation` (conservative).
pub fn classify_tool(name: &str) -> ToolEffect {
    match name {
        "Read" | "List" | "Grep" | "Glob" | "MemoryRead" | "ListAgents" | "ListSkills"
        | "ActivateSkill" | "RecallContext" | "AskUser" | "TodoWrite" => ToolEffect::ReadOnly,
        "read_file" | "list_directory" | "search" | "search_code" | "code_intel"
        | "code_intel_tool" | "git_status" | "git_diff" => ToolEffect::ReadOnly,

        "WebFetch" | "WebSearch" | "InvokeAgent" => ToolEffect::ReadOnly,
        "fetch_url" | "web_search" => ToolEffect::ReadOnly,

        "ListBackgroundTasks" | "CancelTask" | "WaitTask" => ToolEffect::ReadOnly,
        "list_background_tasks" | "cancel_task" | "wait_task" => ToolEffect::ReadOnly,

        "Write" | "Edit" | "MemoryWrite" => ToolEffect::LocalMutation,
        "write_file" | "edit_file" | "replace_match" | "apply_patch" | "rename_path"
        | "move_path" | "copy_path" | "create_directory" | "invoke_skill" => {
            ToolEffect::LocalMutation
        }

        "Bash" => ToolEffect::LocalMutation,
        "bash" | "execute_bash" | "shell" | "run_shell" | "run_validation" => {
            ToolEffect::LocalMutation
        }

        "Delete" => ToolEffect::Destructive,
        "delete_path" => ToolEffect::Destructive,

        // MCP tools (names containing `__`) — remote.
        n if n.contains("__") => ToolEffect::RemoteAction,

        _ => ToolEffect::LocalMutation,
    }
}

/// Display metadata for a tool effect level.
#[derive(Debug, Clone, Copy)]
pub struct ToolEffectDisplay {
    pub badge: &'static str,
    pub label: &'static str,
}

impl ToolEffect {
    /// Return display-friendly badge and label for the effect level.
    pub fn display(&self) -> ToolEffectDisplay {
        match self {
            ToolEffect::ReadOnly => ToolEffectDisplay {
                badge: "R",
                label: "read",
            },
            ToolEffect::RemoteAction => ToolEffectDisplay {
                badge: "~",
                label: "remote",
            },
            ToolEffect::LocalMutation => ToolEffectDisplay {
                badge: "W",
                label: "write",
            },
            ToolEffect::Destructive => ToolEffectDisplay {
                badge: "!",
                label: "danger",
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_family() {
        assert_eq!(classify_tool("Read"), ToolEffect::ReadOnly);
        assert_eq!(classify_tool("Grep"), ToolEffect::ReadOnly);
        assert_eq!(classify_tool("Glob"), ToolEffect::ReadOnly);
        assert_eq!(classify_tool("List"), ToolEffect::ReadOnly);
    }

    #[test]
    fn mutating_family() {
        assert_eq!(classify_tool("Write"), ToolEffect::LocalMutation);
        assert_eq!(classify_tool("Edit"), ToolEffect::LocalMutation);
        assert_eq!(classify_tool("Bash"), ToolEffect::LocalMutation);
    }

    #[test]
    fn destructive_family() {
        assert_eq!(classify_tool("Delete"), ToolEffect::Destructive);
    }

    #[test]
    fn mcp_remote() {
        assert_eq!(classify_tool("server__do_thing"), ToolEffect::RemoteAction);
    }

    #[test]
    fn unknown_defaults_to_mutation() {
        assert_eq!(classify_tool("WhoKnows"), ToolEffect::LocalMutation);
    }
}
