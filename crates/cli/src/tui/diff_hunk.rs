//! Diff hunk parsing and selective application for interactive staging.
//!
//! Used by the approval popup to let users cherry-pick which hunks to
//! accept (git add -p style) and by the transcript to display diffs.

/// One hunk of a diff for interactive staging.
#[derive(Debug, Clone)]
pub(crate) struct DiffHunk {
    /// Human-readable label, e.g. "@@ -10,3 +10,5 @@"
    pub header: String,
    /// Lines in this hunk (with +/-/space prefix).
    pub lines: Vec<(char, String)>,
}

/// Parse a unified diff between `old` and `new` into discrete hunks.
pub(crate) fn parse_diff_hunks(old: &str, new: &str) -> Vec<DiffHunk> {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let mut hunks = Vec::new();
    for group in diff.grouped_ops(3) {
        let mut lines: Vec<(char, String)> = Vec::new();
        let mut old_start = usize::MAX;
        let mut new_start = usize::MAX;
        for op in &group {
            for change in diff.iter_changes(op) {
                if old_start == usize::MAX {
                    old_start = change.old_index().unwrap_or(0);
                    new_start = change.new_index().unwrap_or(0);
                }
                let (sigil, text) = match change.tag() {
                    ChangeTag::Insert => ('+', change.value().to_string()),
                    ChangeTag::Delete => ('-', change.value().to_string()),
                    ChangeTag::Equal => (' ', change.value().to_string()),
                };
                lines.push((sigil, text.trim_end_matches('\n').to_string()));
            }
        }
        if lines.iter().any(|(s, _)| *s != ' ') {
            let header = format!(
                "@@ -{},{} +{},{} @@",
                old_start + 1,
                lines.iter().filter(|(s, _)| *s != '+').count(),
                new_start + 1,
                lines.iter().filter(|(s, _)| *s != '-').count(),
            );
            hunks.push(DiffHunk { header, lines });
        }
    }
    hunks
}

/// Reconstruct file content by applying only selected hunks to the old text.
pub(crate) fn apply_selected_hunks(old: &str, new: &str, selected: &[bool]) -> String {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let groups: Vec<Vec<similar::DiffOp>> = diff.grouped_ops(3);
    let old_lines: Vec<&str> = old.lines().collect();

    let mut result = String::new();
    let mut old_cursor = 0usize;

    for (group_idx, group) in groups.iter().enumerate() {
        let accept = selected.get(group_idx).copied().unwrap_or(false);
        let group_old_start = group
            .first()
            .map(|op| match op {
                similar::DiffOp::Equal { old_index, .. }
                | similar::DiffOp::Delete { old_index, .. }
                | similar::DiffOp::Replace { old_index, .. }
                | similar::DiffOp::Insert { old_index, .. } => *old_index,
            })
            .unwrap_or(old_cursor);

        for i in old_cursor..group_old_start {
            if let Some(line) = old_lines.get(i) {
                result.push_str(line);
                result.push('\n');
            }
        }

        if accept {
            for op in group {
                for change in diff.iter_changes(op) {
                    match change.tag() {
                        ChangeTag::Insert | ChangeTag::Equal => {
                            result.push_str(change.value());
                        }
                        ChangeTag::Delete => {}
                    }
                }
            }
        } else {
            for op in group {
                for change in diff.iter_changes(op) {
                    match change.tag() {
                        ChangeTag::Delete | ChangeTag::Equal => {
                            result.push_str(change.value());
                        }
                        ChangeTag::Insert => {}
                    }
                }
            }
        }

        old_cursor = group
            .last()
            .map(|op| match op {
                similar::DiffOp::Equal { old_index, len, .. } => *old_index + *len,
                similar::DiffOp::Delete {
                    old_index, old_len, ..
                }
                | similar::DiffOp::Replace {
                    old_index, old_len, ..
                } => *old_index + *old_len,
                similar::DiffOp::Insert { old_index, .. } => *old_index,
            })
            .unwrap_or(old_cursor);
    }

    for i in old_cursor..old_lines.len() {
        if let Some(line) = old_lines.get(i) {
            result.push_str(line);
            result.push('\n');
        }
    }

    if !old.ends_with('\n') && !new.ends_with('\n') {
        result.truncate(result.trim_end_matches('\n').len());
    }
    result
}

/// Extract old/new content from approval request and compute diff hunks.
pub(crate) fn extract_approval_hunks(tool: &str, input_json: &str) -> Vec<DiffHunk> {
    let lower = tool.to_ascii_lowercase();
    if !lower.contains("write") && !lower.contains("edit") && !lower.contains("patch") {
        return Vec::new();
    }
    let Ok(val) = serde_json::from_str::<serde_json::Value>(input_json) else {
        return Vec::new();
    };
    let path = val
        .get("path")
        .or_else(|| val.get("file"))
        .or_else(|| val.get("filename"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new_content = val
        .get("content")
        .or_else(|| val.get("new_content"))
        .or_else(|| val.get("text"))
        .and_then(|v| v.as_str());
    let old_content = if !path.is_empty() {
        std::fs::read_to_string(path).ok()
    } else {
        None
    };
    match (old_content.as_deref(), new_content) {
        (Some(old), Some(new)) => parse_diff_hunks(old, new),
        _ => Vec::new(),
    }
}

/// Build modified tool input with only selected hunks applied.
pub(crate) fn build_hunk_modified_input(
    tool: &str,
    input_json: &str,
    selection: &[bool],
) -> Option<serde_json::Value> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(input_json) else {
        return None;
    };
    let path = val
        .get("path")
        .or_else(|| val.get("file"))
        .or_else(|| val.get("filename"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new_content = val
        .get("content")
        .or_else(|| val.get("new_content"))
        .or_else(|| val.get("text"))
        .and_then(|v| v.as_str())?;
    let old_content = std::fs::read_to_string(path).ok()?;
    let patched = apply_selected_hunks(&old_content, new_content, selection);
    let lower = tool.to_ascii_lowercase();
    if lower.contains("write") {
        let mut modified = val.clone();
        if modified.get("content").is_some() {
            modified["content"] = serde_json::Value::String(patched);
        } else if modified.get("text").is_some() {
            modified["text"] = serde_json::Value::String(patched);
        }
        Some(modified)
    } else {
        None
    }
}

pub(crate) fn parse_unified_hunk_header(line: &str) -> Option<(usize, usize)> {
    let trimmed = line.trim();
    if !trimmed.starts_with("@@") {
        return None;
    }
    let rest = trimmed.strip_prefix("@@")?.trim_start();
    let end = rest.find("@@")?;
    let body = rest[..end].trim();
    let mut parts = body.split_whitespace();
    let old = parts.next()?;
    let new = parts.next()?;
    let old_start = old
        .strip_prefix('-')?
        .split(',')
        .next()?
        .parse::<usize>()
        .ok()?;
    let new_start = new
        .strip_prefix('+')?
        .split(',')
        .next()?
        .parse::<usize>()
        .ok()?;
    Some((old_start, new_start))
}
