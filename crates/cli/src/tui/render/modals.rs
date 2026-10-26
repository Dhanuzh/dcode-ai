use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Clear as ClearWidget, Paragraph, Wrap},
};

use crate::tui::connect_modal::{
    ConnectRow, build_connect_rows, selectable_row_indices, selection_pulse, title_sparkle,
};
use crate::tui::diff_hunk::extract_approval_hunks;
use crate::tui::layout::centered_rect;
use crate::tui::palette::{PaletteRow, filter_palette_rows, palette_selectable_indices};
use crate::tui::render_helpers::{popup_block, truncate_chars};
use crate::tui::slash_entries::SlashEntry;
use crate::tui::state::{ApprovalRequest, TuiSessionState};
use crate::tui::theme;

pub const COMMAND_PALETTE_WIDTH: u16 = 56;

pub(crate) fn render_slash_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    entries: &[&SlashEntry],
    selected: usize,
) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::surface())),
        area,
    );
    let inner_h = area.height.saturating_sub(2) as usize;
    let start = selected
        .saturating_sub(inner_h.saturating_sub(1))
        .min(entries.len().saturating_sub(inner_h));
    let cmd_col = entries
        .iter()
        .map(|e| e.command_str().chars().count())
        .max()
        .unwrap_or(8)
        .clamp(8, 22);
    let avail = area.width.saturating_sub(4) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for (i, e) in entries.iter().enumerate().skip(start).take(inner_h) {
        let sel = i == selected;
        let marker = if sel { "› " } else { "  " };
        let cmd = e.command_str();
        let desc = e.description_text();
        let cmd_style = if sel {
            Style::default()
                .fg(theme::on_accent())
                .bg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::accent())
        };
        let desc_style = if sel {
            Style::default().fg(theme::on_accent()).bg(theme::accent())
        } else {
            Style::default().fg(theme::muted())
        };
        let mut spans = vec![
            Span::styled(marker.to_string(), cmd_style),
            Span::styled(format!("{cmd:<cmd_col$}"), cmd_style),
        ];
        if !desc.is_empty() {
            let desc_room = avail.saturating_sub(cmd_col + 2);
            spans.push(Span::styled("  ".to_string(), desc_style));
            spans.push(Span::styled(
                truncate_chars(&desc, desc_room.max(8)),
                desc_style,
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(popup_block("commands")),
        area,
    );
}

pub(crate) fn render_at_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    matches: &[String],
    selected: usize,
) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::surface())),
        area,
    );
    let inner_h = area.height.saturating_sub(2) as usize;
    let start = selected
        .saturating_sub(inner_h.saturating_sub(1))
        .min(matches.len().saturating_sub(inner_h));
    let mut lines: Vec<Line> = Vec::new();
    for (i, m) in matches.iter().enumerate().skip(start).take(inner_h) {
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
        lines.push(Line::from(Span::styled(format!("{marker}{m}"), style)));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(popup_block("files")),
        area,
    );
}

pub(crate) fn render_command_palette(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    g: &TuiSessionState,
) {
    let filtered = filter_palette_rows(&g.command_palette_query);
    let selectable = palette_selectable_indices(&filtered);
    let pick_abs = selectable
        .get(g.palette_index.min(selectable.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0);

    let mut body: Vec<Line> = Vec::new();
    let mut sel_line = 0usize;
    for (abs_idx, row) in filtered.iter().enumerate() {
        match row {
            PaletteRow::Section(name) => {
                body.push(Line::from(Span::styled(
                    format!(" {name}"),
                    Style::default()
                        .fg(theme::muted())
                        .add_modifier(Modifier::BOLD),
                )));
            }
            PaletteRow::Entry { label, shortcut } => {
                let sel = abs_idx == pick_abs;
                if sel {
                    sel_line = body.len();
                }
                let style = if sel {
                    Style::default()
                        .fg(theme::on_accent())
                        .bg(theme::accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::text())
                };
                let marker = if sel { "› " } else { "  " };
                let mut spans = vec![Span::styled(format!("{marker}{label}"), style)];
                if !shortcut.is_empty() {
                    spans.push(Span::styled(
                        format!("   {shortcut}"),
                        if sel {
                            style
                        } else {
                            Style::default().fg(theme::muted())
                        },
                    ));
                }
                body.push(Line::from(spans));
            }
        }
    }

    if body.is_empty() {
        body.push(Line::from(Span::styled(
            "  No matching commands",
            Style::default().fg(theme::muted()),
        )));
    }

    let want_h = (body.len() as u16).saturating_add(4).clamp(8, 24);
    let popup = centered_rect(area, COMMAND_PALETTE_WIDTH, want_h);
    frame.render_widget(ClearWidget, popup);

    let body_rows = popup.height.saturating_sub(3).max(1) as usize;
    let start = sel_line
        .saturating_sub(body_rows / 2)
        .min(body.len().saturating_sub(body_rows));

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if g.command_palette_query.is_empty() {
                "type to filter…".to_string()
            } else {
                g.command_palette_query.clone()
            },
            if g.command_palette_query.is_empty() {
                Style::default().fg(theme::muted())
            } else {
                Style::default().fg(theme::text())
            },
        ),
    ]));
    for line in body.into_iter().skip(start).take(body_rows) {
        lines.push(line);
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(theme::surface()))
            .block(popup_block("commands")),
        popup,
    );
}

pub(crate) fn render_connect_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    g: &TuiSessionState,
) {
    let rows = build_connect_rows(&g.connect_search);
    let selectable = selectable_row_indices(&rows);
    let pick_abs = selectable
        .get(g.connect_menu_index.min(selectable.len().saturating_sub(1)))
        .copied()
        .unwrap_or(usize::MAX);
    let elapsed_ms = g.started.elapsed().as_millis();

    let mut body: Vec<Line> = Vec::new();
    let mut sel_line = 0usize;
    for (abs_idx, row) in rows.iter().enumerate() {
        match row {
            ConnectRow::Section { title } => {
                body.push(Line::from(Span::styled(
                    format!(" {title}"),
                    Style::default()
                        .fg(theme::muted())
                        .add_modifier(Modifier::BOLD),
                )));
            }
            ConnectRow::Provider {
                title, subtitle, ..
            } => {
                let sel = abs_idx == pick_abs;
                if sel {
                    sel_line = body.len();
                }
                let style = if sel {
                    Style::default()
                        .fg(theme::on_accent())
                        .bg(theme::accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::text())
                };
                let marker = if sel {
                    selection_pulse(elapsed_ms)
                } else {
                    "  "
                };
                body.push(Line::from(vec![
                    Span::styled(format!("{marker}{title}"), style),
                    Span::styled(
                        format!("  {subtitle}"),
                        if sel {
                            style
                        } else {
                            Style::default().fg(theme::muted())
                        },
                    ),
                ]));
            }
        }
    }

    let want_h = (body.len() as u16).saturating_add(4).clamp(8, 24);
    let popup = centered_rect(area, 60, want_h);
    frame.render_widget(ClearWidget, popup);
    let body_rows = popup.height.saturating_sub(3).max(1) as usize;
    let start = sel_line
        .saturating_sub(body_rows / 2)
        .min(body.len().saturating_sub(body_rows));

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if g.connect_search.is_empty() {
                "search providers…".to_string()
            } else {
                g.connect_search.clone()
            },
            if g.connect_search.is_empty() {
                Style::default().fg(theme::muted())
            } else {
                Style::default().fg(theme::text())
            },
        ),
    ]));
    for line in body.into_iter().skip(start).take(body_rows) {
        lines.push(line);
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(theme::surface()))
            .block(popup_block(format!(
                "{} connect a provider",
                title_sparkle(elapsed_ms)
            ))),
        popup,
    );
}

pub(crate) fn render_info_modal(frame: &mut ratatui::Frame<'_>, area: Rect, g: &TuiSessionState) {
    // Size to the content (wrapping aside), within the screen.
    let want_h = (g.info_modal_lines.len() as u16 + 2).max(8);
    let max_h = area.height.saturating_sub(2).max(8);
    let popup = centered_rect(area, 88, want_h.min(max_h));
    frame.render_widget(ClearWidget, popup);
    let inner_h = popup.height.saturating_sub(2) as usize;
    let start = g
        .info_modal_scroll
        .min(g.info_modal_lines.len().saturating_sub(1));
    let lines: Vec<Line> = g
        .info_modal_lines
        .iter()
        .skip(start)
        .take(inner_h)
        .map(|l| Line::from(Span::styled(l.clone(), Style::default().fg(theme::text()))))
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(theme::surface()))
            .block(popup_block(g.info_modal_title.as_str()))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// Per-hunk staging view for a file-edit approval (`h`).
pub(crate) fn approval_hunk_lines(
    req: &ApprovalRequest,
    hunk_selection: &[bool],
    hunk_cursor: usize,
    inner_w: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let hunks = extract_approval_hunks(&req.tool, &req.input);
    for (i, hunk) in hunks.iter().enumerate() {
        let accepted = hunk_selection.get(i).copied().unwrap_or(true);
        let is_focused = i == hunk_cursor;
        let marker = if accepted { "[✓]" } else { "[✗]" };
        let hdr_style = if is_focused {
            Style::default()
                .fg(Color::Black)
                .bg(theme::user())
                .add_modifier(Modifier::BOLD)
        } else if accepted {
            Style::default().fg(theme::success())
        } else {
            Style::default().fg(theme::error())
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} "), hdr_style),
            Span::styled(
                truncate_chars(&hunk.header, inner_w.saturating_sub(6)),
                hdr_style,
            ),
        ]));
        let max_hunk_lines = if is_focused { 6 } else { 2 };
        for (sigil, text) in hunk.lines.iter().take(max_hunk_lines) {
            let (color, bg) = match sigil {
                '+' => (
                    theme::success(),
                    Some(crate::tui::transcript::diff_add_bg()),
                ),
                '-' => (theme::error(), Some(crate::tui::transcript::diff_del_bg())),
                _ => (theme::muted(), None),
            };
            let mut style = Style::default().fg(color);
            if let Some(bg) = bg {
                style = style.bg(bg);
            }
            if !accepted && *sigil != ' ' {
                style = style.add_modifier(Modifier::DIM);
            }
            lines.push(Line::from(Span::styled(
                format!(
                    "  {sigil}{}",
                    truncate_chars(text, inner_w.saturating_sub(4))
                ),
                style,
            )));
        }
        if hunk.lines.len() > max_hunk_lines {
            lines.push(Line::from(Span::styled(
                format!("  … {} more lines", hunk.lines.len() - max_hunk_lines),
                Style::default().fg(theme::muted()),
            )));
        }
    }
    let accepted = hunk_selection.iter().filter(|&&s| s).count();
    let total = hunk_selection.len();
    lines.push(Line::from(Span::styled(
        format!("{accepted}/{total} hunks selected — ↑↓ navigate · space toggle · y/n accept/reject · Enter apply"),
        Style::default().fg(theme::muted()),
    )));
    lines
}
