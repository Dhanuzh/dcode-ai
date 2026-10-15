use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn started(tool: &str, input: &Value) -> Option<String> {
    crate::tool_ui::started_message(tool, input)
}

pub fn completed(
    tool: &str,
    success: bool,
    output: &str,
    workspace_root: Option<&Path>,
) -> Option<String> {
    crate::tool_ui::completed_message(tool, success, output, workspace_root)
}

pub(crate) fn format_write_summary_for_tool_ui(
    output: &str,
    workspace_root: Option<&Path>,
) -> String {
    format_write_summary(output, workspace_root)
}

pub(crate) fn format_edit_summary_for_tool_ui(
    output: &str,
    workspace_root: Option<&Path>,
) -> String {
    format_edit_summary(output, workspace_root)
}

fn format_write_summary(output: &str, workspace_root: Option<&Path>) -> String {
    let first_line = output.lines().next().unwrap_or("").trim();
    let path = first_line
        .strip_prefix("Wrote ")
        .unwrap_or(first_line)
        .trim()
        .trim_end_matches(" (no changes)")
        .trim();
    let display_path = relativize(path, workspace_root);
    let (added, removed) = diff_line_counts(output);
    if added > 0 || removed > 0 {
        format!("Edited {display_path} (+{added} -{removed})")
    } else if first_line.ends_with("(no changes)") {
        format!("Checked {display_path} (no changes)")
    } else {
        format!("Edited {display_path}")
    }
}

fn format_edit_summary(output: &str, workspace_root: Option<&Path>) -> String {
    let first_line = output.lines().next().unwrap_or("").trim();
    let path = if let Some(rest) = first_line.strip_prefix("Edited ") {
        rest.split(" (").next().unwrap_or(rest).trim()
    } else if let Some(rest) = first_line.strip_prefix("Patched ") {
        rest.trim()
    } else if let Some(rest) = first_line.strip_prefix("Replaced match at ") {
        rest.split(':').next().unwrap_or(rest).trim()
    } else {
        ""
    };

    if path.is_empty() {
        "Edited files".to_string()
    } else {
        format!("Edited {}", relativize(path, workspace_root))
    }
}

fn diff_line_counts(text: &str) -> (usize, usize) {
    text.lines().fold((0usize, 0usize), |(add, del), line| {
        if line.starts_with("+++") || line.starts_with("---") {
            return (add, del);
        }
        if line.starts_with('+') {
            return (add + 1, del);
        }
        if line.starts_with('-') {
            return (add, del + 1);
        }
        (add, del)
    })
}

fn relativize(path: &str, workspace_root: Option<&Path>) -> String {
    if path.is_empty() {
        return "files".to_string();
    }
    let Some(root) = workspace_root else {
        return path.to_string();
    };
    let file = PathBuf::from(path);
    match file.strip_prefix(root) {
        Ok(rel) => rel.display().to_string(),
        Err(_) => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn started_web_search_includes_query() {
        let msg = started("web_search", &serde_json::json!({"query":"rust tokio"}));
        assert_eq!(msg.as_deref(), Some("Using web context: rust tokio"));
    }

    #[test]
    fn completed_write_file_reports_diff_counts() {
        let output = "Wrote /repo/src/main.rs\n\n--- before\n+++ after\n@@\n-old\n+new\n+extra\n";
        let msg = completed("write_file", true, output, Some(Path::new("/repo")));
        assert_eq!(msg.as_deref(), Some("Edited src/main.rs (+2 -1)"));
    }

    #[test]
    fn completed_edit_file_relativizes_path() {
        let output = "Edited /repo/src/lib.rs (replaced 1 occurrence)";
        let msg = completed("edit_file", true, output, Some(Path::new("/repo")));
        assert_eq!(msg.as_deref(), Some("Edited src/lib.rs"));
    }
}
