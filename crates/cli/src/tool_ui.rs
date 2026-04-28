use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ToolUi {
    pub icon: &'static str,
    pub label: String,
    pub family: &'static str,
}

pub fn metadata(name: &str) -> ToolUi {
    if let Some((server, tool)) = mcp_parts(name) {
        return ToolUi {
            icon: "◆",
            label: format!("MCP {server}/{tool}"),
            family: "mcp",
        };
    }

    let (icon, label, family) = match name {
        "bash" | "execute_bash" | "shell" | "run_shell" => ("$", "Bash", "shell"),
        "read_file" => ("R", "Read file", "file"),
        "list_directory" => ("L", "List directory", "file"),
        "search" | "search_code" => ("S", "File search", "search"),
        "fetch_url" => ("F", "Fetch URL", "web"),
        "web_search" => ("W", "Web search", "web"),
        "write_file" => ("W", "Update file", "edit"),
        "edit_file" => ("E", "Edit file", "edit"),
        "apply_patch" => ("P", "Patch file", "edit"),
        "replace_match" => ("R", "Replace match", "edit"),
        "create_directory" => ("D", "Create directory", "file"),
        "rename_path" => ("R", "Rename path", "file"),
        "move_path" => ("M", "Move path", "file"),
        "copy_path" => ("C", "Copy path", "file"),
        "delete_path" => ("D", "Delete path", "file"),
        "git_status" => ("G", "Git status", "git"),
        "git_diff" => ("G", "Git diff", "git"),
        "run_validation" => ("V", "Run validation", "check"),
        "code_intel" | "code_intel_tool" => ("I", "Code intel", "code"),
        "spawn_subagent" => ("A", "Sub-agent", "agent"),
        "ask_question" => ("?", "Question", "input"),
        "invoke_skill" => ("K", "Skill", "skill"),
        _ => ("T", "", "tool"),
    };

    ToolUi {
        icon,
        label: if label.is_empty() {
            humanize_tool_name(name)
        } else {
            label.to_string()
        },
        family,
    }
}

pub fn preview_from_value(tool: &str, value: &Value) -> String {
    if tool == "spawn_subagent" {
        return spawn_subagent_preview(value);
    }

    let value = parse_embedded_json(value);
    if let Some(obj) = value.as_object() {
        for key in preview_keys(tool) {
            if let Some(text) = obj.get(*key).and_then(value_to_preview) {
                return text;
            }
        }

        for (key, value) in obj {
            if key == "input" || key == "arguments" {
                continue;
            }
            if let Some(text) = value_to_preview(value) {
                return format!("{key}: {text}");
            }
        }
    }

    value_to_preview(&value).unwrap_or_default()
}

pub fn preview_from_display_input(tool: &str, input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if tool == "spawn_subagent"
        && let Some(task) = trimmed
            .lines()
            .find_map(|line| line.strip_prefix("task:").map(str::trim))
            .filter(|s| !s.is_empty())
    {
        return task.to_string();
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return preview_from_value(tool, &value);
    }

    trimmed.lines().next().unwrap_or("").trim().to_string()
}

pub fn format_input_for_display(tool: &str, value: &Value) -> String {
    if tool == "spawn_subagent" {
        return format_spawn_subagent_input(value);
    }
    let value = parse_embedded_json(value);
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
}

pub fn started_message(tool: &str, input: &Value) -> Option<String> {
    let preview = preview_from_value(tool, input);
    let ui = metadata(tool);
    let action = match ui.family {
        "shell" => "Running command",
        "web" => "Using web context",
        "search" => "Searching workspace",
        "edit" => "Editing workspace",
        "file" => "Inspecting files",
        "git" => "Inspecting git state",
        "check" => "Running validation",
        "agent" => "Starting sub-agent",
        "skill" => "Loading skill",
        "input" => "Asking a question",
        "mcp" => "Calling MCP tool",
        _ => "Running tool",
    };

    Some(with_preview(action, &preview))
}

pub fn completed_message(
    tool: &str,
    success: bool,
    output: &str,
    workspace_root: Option<&std::path::Path>,
) -> Option<String> {
    if !success {
        return None;
    }

    match tool {
        "write_file" => Some(crate::activity::format_write_summary_for_tool_ui(
            output,
            workspace_root,
        )),
        "edit_file" | "apply_patch" | "replace_match" => Some(
            crate::activity::format_edit_summary_for_tool_ui(output, workspace_root),
        ),
        _ => {
            let ui = metadata(tool);
            let action = match ui.family {
                "shell" => "Command finished",
                "web" => "Web context ready",
                "search" => "Search complete",
                "file" => "File context ready",
                "git" => "Git context ready",
                "check" => "Validation complete",
                "agent" => "Sub-agent updated",
                "skill" => "Skill loaded",
                "input" => "Question handled",
                "mcp" => "MCP tool finished",
                _ => "Tool finished",
            };
            Some(action.to_string())
        }
    }
}

fn with_preview(action: &str, preview: &str) -> String {
    if preview.is_empty() {
        action.to_string()
    } else {
        format!("{action}: {}", truncate_chars(preview, 120))
    }
}

fn preview_keys(tool: &str) -> &'static [&'static str] {
    match tool {
        "bash" | "execute_bash" | "shell" | "run_shell" | "run_validation" => &["command", "cmd"],
        "web_search" => &["query", "q"],
        "fetch_url" => &["url"],
        "search" | "search_code" => &["pattern", "query", "path"],
        "read_file" | "list_directory" | "write_file" | "edit_file" | "replace_match"
        | "create_directory" | "rename_path" | "move_path" | "copy_path" | "delete_path"
        | "git_diff" => &[
            "file_path",
            "path",
            "target_file",
            "old_path",
            "new_path",
            "from",
            "to",
        ],
        "invoke_skill" => &["skill", "name", "skill_name"],
        "ask_question" => &["prompt", "question"],
        _ => &[
            "file_path",
            "path",
            "command",
            "cmd",
            "pattern",
            "query",
            "url",
            "name",
            "prompt",
        ],
    }
}

fn parse_embedded_json(value: &Value) -> Value {
    if let Some(raw) = value.as_str()
        && let Ok(parsed) = serde_json::from_str::<Value>(raw)
    {
        return parsed;
    }
    value.clone()
}

fn value_to_preview(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => {
            let s = normalize_preview(s);
            (!s.is_empty()).then_some(s)
        }
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(items) => Some(format!("{} item{}", items.len(), plural(items.len()))),
        Value::Object(obj) => Some(format!("{} field{}", obj.len(), plural(obj.len()))),
        Value::Null => None,
    }
}

fn spawn_subagent_preview(value: &Value) -> String {
    let task = value
        .get("task")
        .and_then(Value::as_str)
        .map(normalize_preview)
        .unwrap_or_default();
    let worktree = value
        .get("use_worktree")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let focus = value
        .get("focus_files")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let mut parts = Vec::new();
    if !task.is_empty() {
        parts.push(task);
    }
    parts.push(if worktree {
        "worktree".to_string()
    } else {
        "shared workspace".to_string()
    });
    if focus > 0 {
        parts.push(format!("{focus} focus file{}", plural(focus)));
    }
    parts.join(" · ")
}

fn format_spawn_subagent_input(v: &Value) -> String {
    let task = v.get("task").and_then(Value::as_str).unwrap_or("").trim();
    let wt = v
        .get("use_worktree")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let n_focus = v
        .get("focus_files")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    format!(
        "task:\n{}\nworktree: {} · focus_files: {}",
        truncate_chars(task, 500),
        wt,
        n_focus
    )
}

fn mcp_parts(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("mcp__")?;
    let mut parts = rest.splitn(2, "__");
    let server = parts.next()?.trim();
    let tool = parts.next()?.trim();
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((humanize_tool_name(server), humanize_tool_name(tool)))
}

fn humanize_tool_name(name: &str) -> String {
    let normalized = name.replace("__", " / ").replace(['_', '-'], " ");
    let mut chars = normalized.chars();
    let Some(first) = chars.next() else {
        return "Tool".to_string();
    };
    format!("{}{}", first.to_uppercase(), chars.collect::<String>())
}

fn normalize_preview(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}...",
            s.chars().take(max.saturating_sub(3)).collect::<String>()
        )
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_handles_mcp_tools() {
        let ui = metadata("mcp__filesystem__read_file");
        assert_eq!(ui.family, "mcp");
        assert_eq!(ui.label, "MCP Filesystem/Read file");
    }

    #[test]
    fn preview_prefers_tool_specific_keys() {
        let v = serde_json::json!({"query":"rust tui","path":"src"});
        assert_eq!(preview_from_value("web_search", &v), "rust tui");
    }

    #[test]
    fn preview_summarizes_spawn_subagent() {
        let v = serde_json::json!({
            "task":"Fix tests",
            "use_worktree": true,
            "focus_files": ["src/lib.rs"]
        });
        assert_eq!(
            preview_from_value("spawn_subagent", &v),
            "Fix tests · worktree · 1 focus file"
        );
    }
}
