//! Tool-call presentation helpers: header detail spans, input previews,
//! diff counts, and approval diff previews.

use crate::tool_ui;
use crate::tui::render_helpers::truncate_chars;
use crate::tui::theme;
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

pub(crate) fn tool_header_detail_spans(name: &str, input: &str) -> Vec<Span<'static>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        let preview = tool_input_preview(name, input);
        return if preview.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(
                truncate_chars(&preview, 100),
                Style::default().fg(theme::muted()),
            )]
        };
    };

    use crate::tui::tool_summary::{ToolCallKind, ToolCallSummary};
    let summary = ToolCallSummary::from_call(name, &value);
    match summary.kind {
        ToolCallKind::Bash { command } => {
            if command.is_empty() {
                Vec::new()
            } else {
                vec![Span::styled(
                    truncate_chars(&command, 100),
                    Style::default().fg(theme::warn()),
                )]
            }
        }
        ToolCallKind::Path { path } => {
            if path.is_empty() {
                Vec::new()
            } else {
                vec![Span::styled(
                    truncate_chars(&path, 100),
                    Style::default()
                        .fg(theme::assistant())
                        .add_modifier(Modifier::UNDERLINED),
                )]
            }
        }
        ToolCallKind::Grep { pattern, dir } => vec![
            Span::styled(
                format!("\"{}\"", truncate_chars(&pattern, 48)),
                Style::default()
                    .fg(theme::warn())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" in ", Style::default().fg(theme::muted())),
            Span::styled(
                truncate_chars(&dir, 44),
                Style::default()
                    .fg(theme::assistant())
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ],
        ToolCallKind::Glob { pattern, base } => {
            let mut spans = vec![Span::styled(
                truncate_chars(&pattern, 56),
                Style::default().fg(theme::assistant()),
            )];
            if let Some(base) = base {
                spans.push(Span::styled(" in ", Style::default().fg(theme::muted())));
                spans.push(Span::styled(
                    truncate_chars(&base, 40),
                    Style::default()
                        .fg(theme::assistant())
                        .add_modifier(Modifier::UNDERLINED),
                ));
            }
            spans
        }
        ToolCallKind::List { dir } => vec![Span::styled(
            truncate_chars(&dir, 100),
            Style::default()
                .fg(theme::assistant())
                .add_modifier(Modifier::UNDERLINED),
        )],
        ToolCallKind::WebFetch { url } => vec![Span::styled(
            truncate_chars(&url, 100),
            Style::default()
                .fg(theme::assistant())
                .add_modifier(Modifier::UNDERLINED),
        )],
        ToolCallKind::Generic { value } => value
            .filter(|v| !v.trim().is_empty())
            .map(|v| {
                vec![Span::styled(
                    truncate_chars(&v, 100),
                    Style::default().fg(theme::muted()),
                )]
            })
            .unwrap_or_default(),
    }
}

/// Extract a compact single-line preview of a tool call input for the transcript.
/// Tries common argument keys (path/file_path/command/pattern) from JSON first.
pub(crate) fn tool_input_preview(name: &str, input: &str) -> String {
    let trimmed = input.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        use crate::tui::tool_summary::{ToolCallKind, ToolCallSummary};
        let summary = ToolCallSummary::from_call(name, &value);
        let out = match summary.kind {
            ToolCallKind::Bash { command } => {
                if command.is_empty() {
                    String::new()
                } else {
                    format!("cmd: {}", truncate_chars(&command, 110))
                }
            }
            ToolCallKind::Path { path } => {
                if path.is_empty() {
                    String::new()
                } else {
                    format!("path: {path}")
                }
            }
            ToolCallKind::Grep { pattern, dir } => {
                format!(
                    "grep `{}` in {}",
                    truncate_chars(&pattern, 60),
                    truncate_chars(&dir, 40)
                )
            }
            ToolCallKind::Glob { pattern, base } => {
                if let Some(base) = base {
                    format!(
                        "glob `{}` in {}",
                        truncate_chars(&pattern, 60),
                        truncate_chars(&base, 40)
                    )
                } else {
                    format!("glob `{}`", truncate_chars(&pattern, 60))
                }
            }
            ToolCallKind::List { dir } => format!("list {dir}"),
            ToolCallKind::WebFetch { url } => format!("url: {}", truncate_chars(&url, 110)),
            ToolCallKind::Generic { value } => value.unwrap_or_default(),
        };
        if !out.trim().is_empty() {
            return out;
        }
    }
    tool_ui::preview_from_display_input(name, input)
}

/// Count added/deleted lines in a unified diff (ignoring `+++`/`---` headers).
pub(crate) fn diff_change_counts(diff: &str) -> (usize, usize) {
    let mut adds = 0usize;
    let mut dels = 0usize;
    for l in diff.lines() {
        if l.starts_with('+') && !l.starts_with("+++") {
            adds += 1;
        } else if l.starts_with('-') && !l.starts_with("---") {
            dels += 1;
        }
    }
    (adds, dels)
}

// Diff hunk logic extracted to tui/diff_hunk.rs

/// Build a compact colorized diff preview for file-write/edit tool approvals.
/// Returns up to ~6 colored `Line`s showing +/- changes, or empty if not applicable.
/// The proposed edit as a diff of the real file, with old/new line numbers
/// (like the full-screen diff view). `None` when it can't be worked out
/// (file missing, text not found, not an edit) — the plain preview is used.
pub(crate) fn approval_numbered_diff(
    tool: &str,
    input_json: &str,
    workspace_root: &std::path::Path,
    width: usize,
    max_lines: usize,
) -> Option<Vec<Line<'static>>> {
    let val: serde_json::Value = serde_json::from_str(input_json).ok()?;
    let path = val
        .get("path")
        .or_else(|| val.get("file_path"))
        .and_then(|v| v.as_str())?;
    let full = workspace_root.join(path);
    let old = std::fs::read_to_string(&full).ok();
    let new = match tool {
        "write_file" => val.get("content")?.as_str()?.to_string(),
        "edit_file" | "replace_match" => {
            let mut text = old.clone()?;
            let edits: Vec<(String, String, bool)> =
                match val.get("edits").and_then(|e| e.as_array()) {
                    Some(list) => list
                        .iter()
                        .filter_map(|e| {
                            Some((
                                e.get("old_string")?.as_str()?.to_string(),
                                e.get("new_string")?.as_str()?.to_string(),
                                e.get("replace_all")
                                    .and_then(|b| b.as_bool())
                                    .unwrap_or(false),
                            ))
                        })
                        .collect(),
                    None => vec![(
                        val.get("old_string")?.as_str()?.to_string(),
                        val.get("new_string")?.as_str()?.to_string(),
                        val.get("replace_all")
                            .and_then(|b| b.as_bool())
                            .unwrap_or(false),
                    )],
                };
            for (from, to, all) in edits {
                if from.is_empty() || !text.contains(&from) {
                    return None;
                }
                text = if all {
                    text.replace(&from, &to)
                } else {
                    text.replacen(&from, &to, 1)
                };
            }
            text
        }
        _ => return None,
    };
    let old = old.unwrap_or_default();
    let hunks = crate::tui::diff_hunk::parse_diff_hunks(&old, &new);
    if hunks.is_empty() {
        return None;
    }
    let (adds, dels) = hunks
        .iter()
        .flat_map(|h| &h.lines)
        .fold((0, 0), |(a, d), (s, _)| match s {
            '+' => (a + 1, d),
            '-' => (a, d + 1),
            _ => (a, d),
        });
    let (body, _) = crate::tui::diff_overlay::diff_lines(&hunks, width);
    let mut out = vec![Line::from(vec![
        Span::styled(
            truncate_chars(path, width.saturating_sub(16)),
            Style::default()
                .fg(theme::text())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  +{adds}"), Style::default().fg(theme::success())),
        Span::styled(format!(" −{dels}"), Style::default().fg(theme::error())),
    ])];
    let total = body.len();
    out.extend(body.into_iter().take(max_lines));
    if total > max_lines {
        out.push(Line::from(Span::styled(
            format!("  … {} more lines (h reviews hunks)", total - max_lines),
            Style::default().fg(theme::muted()),
        )));
    }
    Some(out)
}

pub(crate) fn approval_diff_preview(
    tool: &str,
    input_json: &str,
    max_cols: usize,
) -> Vec<Line<'static>> {
    let lower = tool.to_ascii_lowercase();
    if !lower.contains("write")
        && !lower.contains("edit")
        && !lower.contains("patch")
        && !lower.contains("replace")
    {
        return Vec::new();
    }
    let Ok(val) = serde_json::from_str::<serde_json::Value>(input_json) else {
        return Vec::new();
    };

    let path = val
        .get("path")
        .or_else(|| val.get("file"))
        .or_else(|| val.get("file_path"))
        .or_else(|| val.get("filename"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // For edit_file / replace_match: compute diff from old_string → new_string.
    let (old_str_owned, new_str_owned);
    let (old_str, new_str) = if lower.contains("edit") || lower.contains("replace") {
        let old_s = val
            .get("old_string")
            .or_else(|| val.get("old_text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new_s = val
            .get("new_string")
            .or_else(|| val.get("new_text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if old_s.is_empty() && new_s.is_empty() {
            return Vec::new();
        }
        (old_s, new_s)
    } else {
        // write_file: diff between current file and proposed content.
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
        match (old_content, new_content) {
            (Some(old), Some(new)) => {
                old_str_owned = old;
                new_str_owned = new.to_string();
                (old_str_owned.as_str(), new_str_owned.as_str())
            }
            _ => return Vec::new(),
        }
    };

    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old_str, new_str);
    let mut out: Vec<Line<'static>> = Vec::new();

    // File path header.
    if !path.is_empty() {
        let (adds, dels) = {
            let mut a = 0usize;
            let mut d = 0usize;
            for change in diff.iter_all_changes() {
                match change.tag() {
                    ChangeTag::Insert => a += 1,
                    ChangeTag::Delete => d += 1,
                    ChangeTag::Equal => {}
                }
            }
            (a, d)
        };
        out.push(Line::from(vec![
            Span::styled(
                truncate_chars(path, max_cols.saturating_sub(16)),
                Style::default()
                    .fg(theme::text())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" | ", Style::default().fg(theme::muted())),
            Span::styled(format!("+{adds}"), Style::default().fg(theme::success())),
            Span::styled(" ", Style::default()),
            Span::styled(format!("-{dels}"), Style::default().fg(theme::error())),
        ]));
    }

    // Show up to 12 changed lines with 1 line of context around each change.
    let max_lines = 12_usize;
    let mut prev_was_context = false;
    for change in diff.iter_all_changes().take(80) {
        if out.len() > max_lines {
            out.push(Line::from(Span::styled(
                "  …",
                Style::default().fg(theme::muted()),
            )));
            break;
        }
        let (sigil, color) = match change.tag() {
            ChangeTag::Insert => ("+", theme::success()),
            ChangeTag::Delete => ("-", theme::error()),
            ChangeTag::Equal => {
                if !prev_was_context && out.len() > 1 {
                    prev_was_context = true;
                    let text = format!(
                        "  {}",
                        truncate_chars(
                            change.value().trim_end_matches('\n'),
                            max_cols.saturating_sub(4)
                        )
                    );
                    out.push(Line::from(Span::styled(
                        text,
                        Style::default().fg(theme::muted()),
                    )));
                }
                continue;
            }
        };
        prev_was_context = false;
        let text = format!(
            "{sigil} {}",
            truncate_chars(
                change.value().trim_end_matches('\n'),
                max_cols.saturating_sub(2)
            )
        );
        out.push(Line::from(Span::styled(text, Style::default().fg(color))));
    }
    out
}

// ── Codex-style popup/panel renderers ──────────────────────────────────

/// The file a tool call targets, from its JSON input (`path`, `file_path`, …).
pub(crate) fn tool_input_path(_name: &str, input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input.trim()).ok()?;
    ["path", "file_path", "file", "filename", "target"]
        .iter()
        .find_map(|k| value.get(*k).and_then(|v| v.as_str()))
        .map(str::to_string)
        .filter(|p| !p.trim().is_empty())
}
