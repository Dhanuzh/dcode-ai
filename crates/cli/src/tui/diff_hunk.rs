//! Diff hunk parsing and selective application for interactive staging.
//!
//! This module is the canonical home for diff logic. The same functions
//! still exist in `app.rs` for now; new code should import from here.
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

/// The file an approval would change, as `(path, current, proposed)`:
/// `write_file` (full content) and `edit_file` (`old_text` → `new_text`,
/// optionally `replace_all`). Paths are relative to `root`. `None` for other
/// tools, or when the proposal can't be worked out (then hunks aren't
/// offered and the edit is approved or denied whole).
fn approval_change(
    tool: &str,
    input_json: &str,
    root: &std::path::Path,
) -> Option<(String, String, String)> {
    let val = serde_json::from_str::<serde_json::Value>(input_json).ok()?;
    let field = |names: &[&str]| names.iter().find_map(|n| val.get(*n)?.as_str());
    let path = field(&["path", "file", "filename", "file_path"])?.to_string();
    let full = if std::path::Path::new(&path).is_absolute() {
        std::path::PathBuf::from(&path)
    } else {
        root.join(&path)
    };
    let tool = tool.to_ascii_lowercase();
    if tool == "write_file" {
        let new = field(&["content", "new_content", "text"])?.to_string();
        let old = std::fs::read_to_string(&full).unwrap_or_default();
        return Some((path, old, new));
    }
    if tool == "edit_file" {
        let old_text = field(&["old_text", "old_string"])?;
        let new_text = field(&["new_text", "new_string"])?;
        let old = std::fs::read_to_string(&full).ok()?;
        if old_text.is_empty() || !old.contains(old_text) {
            return None;
        }
        let all = val.get("replace_all").and_then(|v| v.as_bool()) == Some(true);
        let new = if all {
            old.replace(old_text, new_text)
        } else {
            old.replacen(old_text, new_text, 1)
        };
        return Some((path, old, new));
    }
    None
}

/// Diff hunks of a file-edit approval (empty when hunks can't be offered).
pub(crate) fn extract_approval_hunks(
    tool: &str,
    input_json: &str,
    root: &std::path::Path,
) -> Vec<DiffHunk> {
    approval_change(tool, input_json, root)
        .map(|(_, old, new)| parse_diff_hunks(&old, &new))
        .unwrap_or_default()
}

/// Marker in a modified approval input: run the call as `write_file` with
/// this content (a partial `edit_file` can't be expressed as an edit).
pub const PARTIAL_WRITE_MARKER: &str = "__dcode_partial_write";

/// Tool input for applying only the selected hunks: `{path, content}` with
/// the old file plus the chosen hunks, marked to run as `write_file`.
pub(crate) fn build_hunk_modified_input(
    tool: &str,
    input_json: &str,
    selection: &[bool],
    root: &std::path::Path,
) -> Option<serde_json::Value> {
    let (path, old, new) = approval_change(tool, input_json, root)?;
    let patched = apply_selected_hunks(&old, &new, selection);
    Some(serde_json::json!({
        "path": path,
        "content": patched,
        PARTIAL_WRITE_MARKER: true,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_file_hunks_apply_only_the_chosen_part() {
        let dir = tempfile::tempdir().unwrap();
        let old: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("a.txt"), &old).unwrap();
        // One edit spanning two far-apart changes → two hunks.
        let old_text: String = (2..=30).map(|i| format!("line {i}\n")).collect();
        let new_text = old_text
            .replace("line 2\n", "line TWO\n")
            .replace("line 30\n", "line THIRTY\n");
        let input =
            serde_json::json!({"path": "a.txt", "old_text": old_text, "new_text": new_text})
                .to_string();
        let hunks = extract_approval_hunks("edit_file", &input, dir.path());
        assert_eq!(hunks.len(), 2);
        let modified =
            build_hunk_modified_input("edit_file", &input, &[true, false], dir.path()).unwrap();
        let content = modified["content"].as_str().unwrap();
        assert!(content.contains("line TWO") && content.contains("line 30\n"));
        assert!(!content.contains("THIRTY"));
        assert_eq!(modified[PARTIAL_WRITE_MARKER], true);
    }

    #[test]
    fn unknown_tools_offer_no_hunks() {
        let dir = tempfile::tempdir().unwrap();
        assert!(extract_approval_hunks("apply_patch", "{}", dir.path()).is_empty());
        let missing = serde_json::json!({"path": "nope.txt", "old_text": "a", "new_text": "b"});
        assert!(extract_approval_hunks("edit_file", &missing.to_string(), dir.path()).is_empty());
    }
}
