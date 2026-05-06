//! Mouse text selection for full-screen TUI.
//!
//! Ported from koda's koda-cli/src/mouse_select.rs (MIT).
//!
//! Click-drag selection in the history panel with automatic clipboard
//! copy on release. Selection coordinates are in **buffer space**
//! (absolute visual rows across the entire scroll buffer) so anchors
//! stay stable as the buffer grows during inference.

use ratatui::{
    style::{Color, Modifier, Style},
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
/// Returns modified lines with inverted styles on selected rows.
/// Selection coordinates are in buffer space (absolute visual rows).
pub fn apply_selection_highlight<'a>(
    lines: Vec<Line<'a>>,
    selection: &Selection,
    viewport_width: usize,
) -> Vec<Line<'a>> {
    let highlight = Style::default()
        .bg(Color::Rgb(68, 68, 120))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let mut row_idx: u16 = 0;
    let mut result = Vec::with_capacity(lines.len());

    for line in lines {
        let _ = viewport_width;
        let in_selection = selection.contains_row(row_idx);

        if in_selection {
            let highlighted_spans: Vec<Span<'a>> = line
                .spans
                .into_iter()
                .map(|s| Span::styled(s.content, highlight))
                .collect();
            result.push(Line::from(highlighted_spans));
        } else {
            result.push(line);
        }

        row_idx = row_idx.saturating_add(1);
    }

    result
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
