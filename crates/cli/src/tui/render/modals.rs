use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear as ClearWidget, Paragraph, Wrap},
};

use crate::tool_ui;
use crate::tui::connect_modal::{ConnectRow, build_connect_rows, selectable_row_indices};
use crate::tui::diff_hunk::extract_approval_hunks;
use crate::tui::layout::centered_rect;
use crate::tui::palette::{PaletteRow, filter_palette_rows, palette_selectable_indices};
use crate::tui::render_helpers::{popup_block, truncate_chars, wrap_text};
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
                let marker = if sel { "› " } else { "  " };
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
            .block(popup_block("connect a provider")),
        popup,
    );
}

pub(crate) fn render_api_key_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    g: &TuiSessionState,
) {
    use crate::tui::state::OnboardingValidation;
    let provider = g
        .api_key_target_provider
        .map(|p| p.display_name().to_string())
        .unwrap_or_else(|| "provider".into());
    let masked: String = "•".repeat(g.api_key_input.chars().count().min(48));

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("Enter API key for {provider}"),
        Style::default()
            .fg(theme::text())
            .add_modifier(Modifier::BOLD),
    )));
    if g.api_key_target_has_existing {
        lines.push(Line::from(Span::styled(
            "(a key is already saved — entering a new one replaces it)",
            Style::default().fg(theme::muted()),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        if masked.is_empty() {
            Span::styled("paste your key…", Style::default().fg(theme::muted()))
        } else {
            Span::styled(masked, Style::default().fg(theme::text()))
        },
    ]));
    lines.push(Line::default());
    match &g.validation_status {
        Some(OnboardingValidation::Validating) => lines.push(Line::from(Span::styled(
            "  validating…",
            Style::default().fg(theme::warn()),
        ))),
        Some(OnboardingValidation::Valid) => lines.push(Line::from(Span::styled(
            "  ✓ valid",
            Style::default().fg(theme::success()),
        ))),
        Some(OnboardingValidation::Failed(msg)) => lines.push(Line::from(Span::styled(
            format!("  ✗ {}", truncate_chars(msg, 50)),
            Style::default().fg(theme::error()),
        ))),
        None => {}
    }
    lines.push(Line::from(Span::styled(
        "Enter save · Esc cancel",
        Style::default().fg(theme::muted()),
    )));

    let h = (lines.len() as u16).saturating_add(2).clamp(7, 14);
    let popup = centered_rect(area, 60, h);
    frame.render_widget(ClearWidget, popup);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(theme::surface()))
            .block(popup_block("api key"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

pub(crate) fn render_info_modal(frame: &mut ratatui::Frame<'_>, area: Rect, g: &TuiSessionState) {
    let popup = centered_rect(area, 80, 22);
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

pub(crate) fn render_question_modal(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    g: &TuiSessionState,
) {
    let Some(q) = &g.active_question else { return };
    let suggested_label = q
        .options
        .iter()
        .find(|o| o.id == q.suggested_answer)
        .map(|o| o.label.clone())
        .unwrap_or_else(|| q.suggested_answer.clone());
    let mut items: Vec<String> = vec![format!("{suggested_label}  (recommended)")];
    for o in &q.options {
        if o.id == q.suggested_answer {
            continue;
        }
        items.push(o.label.clone());
    }

    let mut lines: Vec<Line> = Vec::new();
    for pl in wrap_text(&q.prompt, 60) {
        lines.push(Line::from(Span::styled(
            pl,
            Style::default().fg(theme::text()),
        )));
    }
    lines.push(Line::default());
    for (i, label) in items.iter().enumerate() {
        let sel = i == g.question_modal_index;
        let style = if sel {
            Style::default()
                .fg(theme::on_accent())
                .bg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::text())
        };
        let marker = if sel { "› " } else { "  " };
        lines.push(Line::from(Span::styled(format!("{marker}{label}"), style)));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Enter select · ↑↓ move · Esc cancel",
        Style::default().fg(theme::muted()),
    )));

    let h = (lines.len() as u16).saturating_add(2).clamp(6, 20);
    let popup = centered_rect(area, 64, h);
    frame.render_widget(ClearWidget, popup);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(theme::surface()))
            .block(popup_block("approval"))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

pub(crate) fn render_approval_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    req: &ApprovalRequest,
    selected: usize,
    hunk_mode: bool,
    hunk_selection: &[bool],
    hunk_cursor: usize,
) {
    let max_w = area.width.saturating_sub(2);
    let popup_w = max_w.min(84).max(max_w.min(56));
    let max_h = area.height.saturating_sub(2);
    let has_diff = !crate::tui::app::approval_diff_preview(
        req.tool.as_str(),
        req.input.as_str(),
        (popup_w as usize).saturating_sub(4),
    )
    .is_empty();
    let popup_h = if hunk_mode {
        max_h.min(30).max(max_h.min(18))
    } else if has_diff {
        max_h.min(20).max(max_h.min(13))
    } else {
        max_h.min(13).max(max_h.min(9))
    };
    let popup_area = centered_rect(area, popup_w, popup_h);

    let selected = selected.min(2);
    let inner_w = popup_area.width.saturating_sub(4) as usize;
    let ui = tool_ui::metadata(&req.tool);
    let preview = crate::tui::app::tool_input_preview(&req.tool, &req.input);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("◆ ", Style::default().fg(theme::success())),
        Span::styled(
            ui.label,
            Style::default()
                .fg(theme::success())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" requires approval", Style::default().fg(theme::muted())),
    ]));
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("name:", Style::default().fg(theme::success())),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("\"{}\"", req.tool),
            Style::default().fg(theme::text()),
        ),
    ]));
    lines.push(Line::default());

    let option_row = |idx: usize, label: &str| -> Line<'static> {
        let active = selected == idx;
        let marker = if active { "● " } else { "○ " };
        let mut style = Style::default().fg(theme::muted());
        if active {
            style = Style::default()
                .fg(theme::success())
                .add_modifier(Modifier::BOLD);
        }
        Line::from(vec![
            Span::styled(marker, style),
            Span::styled(label.to_string(), style),
        ])
    };
    lines.push(option_row(0, "Approve (y)"));
    lines.push(option_row(1, "Always approve (a)"));
    lines.push(option_row(2, "Deny (n)"));
    lines.push(Line::default());

    if hunk_mode && !hunk_selection.is_empty() {
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
                let color = match sigil {
                    '+' => theme::success(),
                    '-' => theme::error(),
                    _ => theme::muted(),
                };
                let dimmed = if !accepted && *sigil != ' ' {
                    Style::default().fg(color).add_modifier(Modifier::DIM)
                } else {
                    Style::default().fg(color)
                };
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {sigil}{}",
                        truncate_chars(text, inner_w.saturating_sub(4))
                    ),
                    dimmed,
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
    } else {
        let diff_preview = crate::tui::app::approval_diff_preview(&req.tool, &req.input, inner_w);
        if !diff_preview.is_empty() {
            lines.push(Line::from(Span::styled(
                "diff preview (h = per-hunk staging):",
                Style::default().fg(theme::muted()),
            )));
            for dl in diff_preview {
                lines.push(dl);
            }
        } else if !preview.is_empty() {
            lines.push(Line::from(Span::styled(
                truncate_chars(
                    &format!("input: {preview}"),
                    inner_w.saturating_sub(2).max(24),
                ),
                Style::default().fg(theme::muted()),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                truncate_chars(&req.description, inner_w.saturating_sub(2).max(24)),
                Style::default().fg(theme::muted()),
            )));
        }
        lines.push(Line::from(Span::styled(
            "↑↓ select  Enter confirm  Esc cancel  h hunks",
            Style::default().fg(theme::muted()),
        )));
    }

    frame.render_widget(ClearWidget, popup_area);
    let popup = Paragraph::new(Text::from(lines)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::warn()))
            .title(Span::styled(
                " Tool Approval ",
                Style::default()
                    .fg(theme::warn())
                    .add_modifier(Modifier::BOLD),
            )),
    );
    frame.render_widget(popup, popup_area);
}
