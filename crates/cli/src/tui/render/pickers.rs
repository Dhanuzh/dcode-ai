//! Centered list pickers (model/theme/agent/project/session) and their labels.

use crate::tui::layout::centered_rect;
use crate::tui::render_helpers::{popup_block, truncate_chars};
use crate::tui::state::TuiSessionState;
use crate::tui::theme;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Clear as ClearWidget, Paragraph},
};

/// Generic centered list picker (model/theme/agent/project/session).
pub(crate) fn render_list_picker(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    items: &[String],
    selected: usize,
    search: &str,
) {
    let has_search = !search.is_empty();
    let rows = (items.len() as u16)
        .saturating_add(if has_search { 3 } else { 2 })
        .clamp(6, 22);
    let popup = centered_rect(area, 56, rows);
    frame.render_widget(ClearWidget, popup);
    let mut lines: Vec<Line> = Vec::new();

    // Search/filter line at the top so the user sees what they typed.
    lines.push(Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if search.is_empty() {
                "type to filter…".to_string()
            } else {
                search.to_string()
            },
            if search.is_empty() {
                Style::default().fg(theme::muted())
            } else {
                Style::default().fg(theme::text())
            },
        ),
    ]));

    let inner_h = popup.height.saturating_sub(3) as usize;
    let start = selected
        .saturating_sub(inner_h.saturating_sub(1))
        .min(items.len().saturating_sub(inner_h));
    if items.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no matches)",
            Style::default().fg(theme::muted()),
        )));
    }
    for (i, item) in items.iter().enumerate().skip(start).take(inner_h) {
        let sel = i == selected;
        let style = if sel {
            Style::default()
                .fg(theme::on_accent())
                .bg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::text())
        };
        let marker = if sel { "› " } else { "  " };
        lines.push(Line::from(Span::styled(
            format!(
                "{marker}{}",
                truncate_chars(item, popup.width.saturating_sub(3) as usize)
            ),
            style,
        )));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(theme::surface()))
            .block(popup_block(title)),
        popup,
    );
}

pub(crate) fn model_picker_labels_and_index(g: &TuiSessionState) -> (Vec<String>, usize) {
    let filter = g.model_picker_search.to_ascii_lowercase();
    let mut labels = Vec::new();
    let mut visual_index = 0; // falls back to 0 if nothing selected

    // Count how many non-header items are visible before the selected one.
    let mut non_header_before_selected = 0;
    let mut found_selected = false;

    for e in &g.model_picker_entries {
        let visible = if e.is_header {
            filter.is_empty() || e.label.to_ascii_lowercase().contains(&filter)
        } else {
            filter.is_empty()
                || e.label.to_ascii_lowercase().contains(&filter)
                || e.detail.to_ascii_lowercase().contains(&filter)
        };
        if !visible {
            continue;
        }
        if e.is_header {
            labels.push(format!("── {} ──", e.label));
        } else {
            if non_header_before_selected == g.model_picker_index {
                visual_index = labels.len();
                found_selected = true;
            }
            let item = if e.detail.is_empty() {
                e.label.clone()
            } else {
                format!("{}  ·  {}", e.label, e.detail)
            };
            labels.push(item);
            non_header_before_selected += 1;
        }
    }
    // Clamp in case the index is out of range (shouldn't happen, but defensive).
    if !found_selected && !labels.is_empty() {
        visual_index = labels.len().saturating_sub(1);
    }
    (labels, visual_index)
}

pub(crate) fn agent_picker_labels() -> Vec<String> {
    vec![
        "@build   — full-access development".into(),
        "@plan    — read-only analysis".into(),
        "@review  — focused code review".into(),
        "@fix     — bug diagnosis & fixes".into(),
        "@test    — testing & validation".into(),
    ]
}

pub(crate) fn project_picker_labels(g: &TuiSessionState) -> Vec<String> {
    g.connected_projects
        .iter()
        .map(|p| {
            let dot = if p.active { "● " } else { "○ " };
            format!("{dot}{}  ·  {}", p.name, p.path.display())
        })
        .collect()
}

/// Session picker entries that match the search, in order.
fn session_picker_matches(
    g: &TuiSessionState,
) -> Vec<(&crate::tui::state::SessionPickerEntry, Option<String>)> {
    g.session_picker_entries
        .iter()
        .filter_map(|e| e.search(&g.session_picker_search).map(|snip| (e, snip)))
        .collect()
}

/// Rows the `/resume` panel needs: title, search, two rows per session (at
/// most `max_rows` in all), the hint, and the two rules.
pub(crate) fn session_picker_height(g: &TuiSessionState, max_rows: u16) -> u16 {
    let shown = session_picker_matches(g).len().max(1) as u16;
    (5 + shown * 2).min(max_rows).max(7)
}

/// The `/resume` panel, drawn in the composer's place (Claude Code style):
///
/// ```text
/// ─────────────────────────────────────────────
///  Resume a session
///  ⌕ type to search
/// ❯ fix the login bug
///   3h ago · opus · main · 12.4k tokens
///   add sharepoint sync
///   2d ago · sonnet · 3.1k tokens
///  ↑↓ choose · enter resume · esc cancel · 2 of 9
/// ─────────────────────────────────────────────
/// ```
pub(crate) fn render_session_picker(
    frame: &mut ratatui::Frame<'_>,
    rect: Rect,
    g: &TuiSessionState,
) {
    use ratatui::widgets::{Block, Borders};
    frame.render_widget(ClearWidget, rect);
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(theme::accent()));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let matches = session_picker_matches(g);
    let muted = Style::default().fg(theme::muted());
    let width = inner.width.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            " Resume a session",
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(" ⌕ ", muted),
            if g.session_picker_search.is_empty() {
                Span::styled("type to search titles and what was said", muted)
            } else {
                Span::styled(
                    format!("{}▏", g.session_picker_search),
                    Style::default().fg(theme::text()),
                )
            },
        ]),
    ];
    let slots = (inner.height.saturating_sub(3) / 2).max(1) as usize;
    let selected = g.session_picker_index.min(matches.len().saturating_sub(1));
    let start = selected.saturating_sub(slots - 1);
    if matches.is_empty() {
        lines.push(Line::from(Span::styled("   no sessions match", muted)));
    }
    for (i, (entry, snippet)) in matches.iter().enumerate().skip(start).take(slots) {
        let sel = i == selected;
        let title_style = if sel {
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::text())
        };
        lines.push(Line::from(vec![
            Span::styled(if sel { " ❯ " } else { "   " }, title_style),
            Span::styled(truncate_chars(&entry.label, width), title_style),
        ]));
        // A match inside the conversation shows where it matched.
        let second = snippet.as_deref().unwrap_or(&entry.meta);
        lines.push(Line::from(Span::styled(
            format!("   {}", truncate_chars(second, width)),
            if snippet.is_some() {
                Style::default().fg(theme::accent())
            } else {
                muted
            },
        )));
    }
    // The hint sits on the last row whatever the list length.
    let used = lines.len() as u16;
    for _ in used..inner.height.saturating_sub(1) {
        lines.push(Line::default());
    }
    let position = if matches.is_empty() {
        String::new()
    } else {
        format!(" · {} of {}", selected + 1, matches.len())
    };
    lines.push(Line::from(Span::styled(
        format!(" ↑↓ choose · enter resume · esc cancel{position}"),
        muted,
    )));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(theme::bg())),
        inner,
    );
}
