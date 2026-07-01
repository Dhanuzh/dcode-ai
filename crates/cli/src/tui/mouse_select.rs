//! Mouse text selection for full-screen TUI.
//!
//! Ported from koda's koda-cli/src/mouse_select.rs (MIT).
//!
//! Click-drag selection in the history panel with automatic clipboard
//! copy on release. Selection coordinates are in **buffer space**
//! (absolute visual rows across the entire scroll buffer) so anchors
//! stay stable as the buffer grows during inference.

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualPos {
    pub row: u16,
    pub col: u16,
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub anchor: VisualPos,
    pub cursor: VisualPos,
    /// Scroll-from-top captured at MouseDown time. Used to map screen
    /// rows → buffer rows consistently for the lifetime of the drag.
    pub scroll_from_top: u16,
}

impl Selection {
    pub fn ordered(&self) -> (VisualPos, VisualPos) {
        if self.anchor.row < self.cursor.row
            || (self.anchor.row == self.cursor.row && self.anchor.col <= self.cursor.col)
        {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    #[allow(dead_code)]
    pub fn contains_row(&self, row: u16) -> bool {
        let (start, end) = self.ordered();
        row >= start.row && row <= end.row
    }
}

/// Build selectable rows from transcript lines.
///
/// Returns one String per visual row plus a parallel vec of gutter widths
/// (cells excluded from text extraction, e.g. line-number columns in diffs).
pub fn build_all_visual_rows(
    lines: &[Line<'_>],
    gutter_widths: &[u16],
    viewport_width: usize,
) -> (Vec<String>, Vec<u16>) {
    let mut visual_rows: Vec<String> = Vec::new();
    let mut visual_gutters: Vec<u16> = Vec::new();
    let _ = viewport_width;

    for (i, line) in lines.iter().enumerate() {
        let gw = gutter_widths.get(i).copied().unwrap_or(0);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        visual_rows.push(text);
        visual_gutters.push(gw);
    }

    (visual_rows, visual_gutters)
}

/// Extract selected text from visual rows, skipping gutter columns.
pub fn extract_selected_text(rows: &[String], gutters: &[u16], selection: &Selection) -> String {
    let (start, end) = selection.ordered();
    let mut result = String::new();

    for row in start.row..=end.row {
        let idx = row as usize;
        if idx >= rows.len() {
            break;
        }
        let line = &rows[idx];
        let gutter_w = gutters.get(idx).copied().unwrap_or(0) as usize;
        let chars: Vec<char> = line.chars().collect();

        let col_start = if row == start.row {
            start.col as usize
        } else {
            0
        };
        let col_end = if row == end.row {
            (end.col as usize + 1).min(chars.len())
        } else {
            chars.len()
        };

        let effective_start = col_start.max(gutter_w);

        if effective_start < chars.len() && effective_start < col_end {
            let selected: String = chars[effective_start..col_end.min(chars.len())]
                .iter()
                .collect();
            result.push_str(&selected);
        }
        if row < end.row {
            result.push('\n');
        }
    }

    result
}

/// Apply selection highlight to lines being rendered.
///
/// Only the selected character range is highlighted:
/// - middle rows get fully highlighted
/// - the start row is highlighted from `start.col` to end-of-line
/// - the end row is highlighted from column 0 to `end.col` (inclusive)
///
/// Selection coordinates are in buffer space (absolute visual rows).
#[allow(dead_code)]
pub fn apply_selection_highlight<'a>(
    lines: Vec<Line<'a>>,
    selection: &Selection,
    viewport_width: usize,
) -> Vec<Line<'a>> {
    let _ = viewport_width;
    let highlight = Style::default()
        .bg(crate::tui::theme::mention_bg())
        .fg(crate::tui::theme::text())
        .add_modifier(Modifier::BOLD);

    let (start, end) = selection.ordered();
    let mut row_idx: u16 = 0;
    let mut result = Vec::with_capacity(lines.len());

    for line in lines {
        if !selection.contains_row(row_idx) {
            result.push(line);
            row_idx = row_idx.saturating_add(1);
            continue;
        }

        // Determine which column range to highlight on this row.
        let col_start: usize = if row_idx == start.row {
            start.col as usize
        } else {
            0
        };
        let col_end: usize = if row_idx == end.row {
            end.col as usize + 1 // inclusive
        } else {
            usize::MAX // whole line
        };

        // Split each span at the highlight boundaries.
        let new_spans = highlight_span_range(line.spans, col_start, col_end, highlight);
        result.push(Line::from(new_spans));

        row_idx = row_idx.saturating_add(1);
    }

    result
}

/// Rebuild a span list, applying `highlight` style only to characters in
/// `[col_start, col_end)` (column indices into the concatenated text).
#[allow(dead_code)]
fn highlight_span_range<'a>(
    spans: Vec<Span<'a>>,
    col_start: usize,
    col_end: usize,
    highlight: Style,
) -> Vec<Span<'a>> {
    let mut out: Vec<Span<'a>> = Vec::new();
    let mut pos: usize = 0; // current column (char index)

    for span in spans {
        let text: Vec<char> = span.content.chars().collect();
        let span_end = pos + text.len();

        if span_end <= col_start || pos >= col_end {
            // Entirely outside selection — keep as-is.
            out.push(span);
        } else if pos >= col_start && span_end <= col_end {
            // Entirely inside selection — fully highlight.
            out.push(Span::styled(span.content, highlight));
        } else {
            // Partially overlapping — split into up to three segments.
            let orig = span.style;
            let content = span.content;

            // Before the selection
            let pre_end = col_start.saturating_sub(pos).min(text.len());
            if pre_end > 0 {
                out.push(Span::styled(
                    text[..pre_end].iter().collect::<String>(),
                    orig,
                ));
            }

            // Inside the selection
            let sel_start = col_start.saturating_sub(pos).min(text.len());
            let sel_end = col_end.saturating_sub(pos).min(text.len());
            if sel_start < sel_end {
                out.push(Span::styled(
                    text[sel_start..sel_end].iter().collect::<String>(),
                    highlight,
                ));
            }

            // After the selection
            if sel_end < text.len() {
                out.push(Span::styled(
                    text[sel_end..].iter().collect::<String>(),
                    orig,
                ));
            }

            let _ = content; // ownership consumed via `text`
        }

        pos = span_end;
    }

    out
}

/// Copy text to the system clipboard.
pub fn copy_to_clipboard(text: &str) -> Result<String, String> {
    super::clipboard::copy_to_clipboard(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_line(text: &str) -> Line<'static> {
        Line::from(text.to_string())
    }

    fn no_gutters(n: usize) -> Vec<u16> {
        vec![0; n]
    }

    #[test]
    fn selection_ordered_swaps_when_anchor_below_cursor() {
        let sel = Selection {
            anchor: VisualPos { row: 5, col: 10 },
            cursor: VisualPos { row: 2, col: 3 },
            scroll_from_top: 0,
        };
        let (start, end) = sel.ordered();
        assert_eq!(start.row, 2);
        assert_eq!(end.row, 5);
    }

    #[test]
    fn extract_single_line() {
        let rows = vec!["hello world".to_string()];
        let sel = Selection {
            anchor: VisualPos { row: 0, col: 6 },
            cursor: VisualPos { row: 0, col: 10 },
            scroll_from_top: 0,
        };
        let text = extract_selected_text(&rows, &no_gutters(1), &sel);
        assert_eq!(text, "world");
    }

    #[test]
    fn extract_multi_line() {
        let rows = vec![
            "first line".to_string(),
            "second line".to_string(),
            "third line".to_string(),
        ];
        let sel = Selection {
            anchor: VisualPos { row: 0, col: 6 },
            cursor: VisualPos { row: 2, col: 4 },
            scroll_from_top: 0,
        };
        let text = extract_selected_text(&rows, &no_gutters(3), &sel);
        assert_eq!(text, "line\nsecond line\nthird");
    }

    #[test]
    fn build_all_visual_rows_preserves_one_row_per_line() {
        let lines = vec![make_line("abcdefghij12345")];
        let (rows, _) = build_all_visual_rows(&lines, &no_gutters(1), 10);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], "abcdefghij12345");
    }

    #[test]
    fn cross_page_selection_captures_all_rows() {
        let lines: Vec<Line<'_>> = (0..20).map(|i| make_line(&format!("line {i}"))).collect();
        let (all_rows, all_gutters) = build_all_visual_rows(&lines, &no_gutters(20), 80);
        let sel = Selection {
            anchor: VisualPos { row: 2, col: 0 },
            cursor: VisualPos { row: 8, col: 5 },
            scroll_from_top: 0,
        };
        let text = extract_selected_text(&all_rows, &all_gutters, &sel);
        assert!(text.contains("line 2"));
        assert!(text.contains("line 8"));
        assert_eq!(text.lines().count(), 7);
    }

    #[test]
    fn highlight_partial_start_row_only_from_col() {
        // Selecting from col 6 on row 0 to end of row 1: row 0 should only
        // highlight "world", not "hello ".
        let lines = vec![Line::from("hello world"), Line::from("second line")];
        let sel = Selection {
            anchor: VisualPos { row: 0, col: 6 },
            cursor: VisualPos { row: 1, col: 5 },
            scroll_from_top: 0,
        };
        let result = apply_selection_highlight(lines, &sel, 80);

        // Row 0: "hello " unhighlighted, "world" highlighted.
        let row0_plain: String = result[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier != Modifier::BOLD)
            .map(|s| s.content.as_ref())
            .collect();
        assert!(row0_plain.contains("hello "), "got: {row0_plain}");

        let row0_hl: String = result[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect();
        assert!(row0_hl.contains("world"), "highlighted: {row0_hl}");
    }

    #[test]
    fn highlight_partial_end_row_only_up_to_col() {
        // Selecting row 0 fully, row 1 only up to col 5 ("second").
        let lines = vec![Line::from("first line"), Line::from("second line")];
        let sel = Selection {
            anchor: VisualPos { row: 0, col: 0 },
            cursor: VisualPos { row: 1, col: 5 },
            scroll_from_top: 0,
        };
        let result = apply_selection_highlight(lines, &sel, 80);

        // Row 1: "second" highlighted, " line" not.
        let row1_plain: String = result[1]
            .spans
            .iter()
            .filter(|s| !s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            row1_plain.contains(" line"),
            "unhighlighted tail: {row1_plain}"
        );
    }

    #[test]
    fn gutter_columns_skipped() {
        let rows = vec![
            "   1   fn main() {".to_string(),
            "   2 - println!(\"hello\");".to_string(),
        ];
        let gutters = vec![7u16, 7];
        let sel = Selection {
            anchor: VisualPos { row: 0, col: 0 },
            cursor: VisualPos { row: 1, col: 30 },
            scroll_from_top: 0,
        };
        let text = extract_selected_text(&rows, &gutters, &sel);
        assert!(!text.contains("   1"));
        assert!(text.contains("fn main()"));
    }
}
