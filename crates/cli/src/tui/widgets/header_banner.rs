#![allow(dead_code)]

use crate::tui::theme;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

pub struct HeaderBanner<'a> {
    pub model: &'a str,
    pub workspace: &'a str,
    pub session_id: &'a str,
    pub current_branch: &'a str,
    pub permission_mode: &'a str,
    pub project_count: usize,
}

const LOGO_ART: [&str; 5] = [
    "     ╷╱╲╷     ",
    "    ╱╱  ╲╲    ",
    "   ╱╱ ◆◆ ╲╲   ",
    "  ╱╱______╲╲  ",
    " ╱____________╲ ",
];

pub const HEADER_HEIGHT: u16 = 7;

impl Widget for HeaderBanner<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 30 || area.height < HEADER_HEIGHT {
            return;
        }

        for x in area.x..area.x + area.width {
            for y in area.y..area.y + area.height {
                buf[(x, y)].set_style(Style::default().bg(theme::header_bg()));
            }
        }

        let version = env!("CARGO_PKG_VERSION");

        let logo_colors = [
            Style::default().fg(theme::error()).bg(theme::header_bg()),
            Style::default().fg(theme::warn()).bg(theme::header_bg()),
            Style::default().fg(theme::success()).bg(theme::header_bg()),
            Style::default().fg(theme::user()).bg(theme::header_bg()),
            Style::default()
                .fg(theme::assistant())
                .bg(theme::header_bg()),
        ];

        let logo_x = area.x + 1;
        let info_x = logo_x + 17;
        let info_w = area.width.saturating_sub(19) as usize;

        for (i, art_line) in LOGO_ART.iter().enumerate() {
            let y = area.y + i as u16 + 1;
            if y >= area.y + area.height {
                break;
            }
            let style = logo_colors[i % logo_colors.len()];
            let line = Line::from(Span::styled(*art_line, style));
            let line_area = Rect::new(logo_x, y, 16.min(area.width.saturating_sub(2)), 1);
            line.render(line_area, buf);
        }

        let title_line = Line::from(vec![Span::styled(
            format!("dcode-ai v{version}"),
            Style::default()
                .fg(theme::accent())
                .bg(theme::header_bg())
                .add_modifier(Modifier::BOLD),
        )]);

        let ws_display = if self.workspace.len() > info_w.saturating_sub(2) {
            let start = self
                .workspace
                .len()
                .saturating_sub(info_w.saturating_sub(5));
            format!("...{}", &self.workspace[start..])
        } else {
            self.workspace.to_string()
        };

        let model_line = Line::from(vec![Span::styled(
            self.model,
            Style::default().fg(theme::text()).bg(theme::header_bg()),
        )]);

        let ws_line = Line::from(vec![Span::styled(
            ws_display,
            Style::default().fg(theme::muted()).bg(theme::header_bg()),
        )]);

        let branch_mode = if !self.current_branch.is_empty() || !self.permission_mode.is_empty() {
            let mut parts = Vec::new();
            if !self.current_branch.is_empty() {
                parts.push(Span::styled(
                    format!(" {}", self.current_branch),
                    Style::default().fg(theme::success()).bg(theme::header_bg()),
                ));
            }
            if !self.permission_mode.is_empty() {
                let mode_label = if self.permission_mode.contains("Bypass") {
                    "bypass"
                } else if self.permission_mode.contains("Plan") {
                    "plan"
                } else if self.permission_mode.contains("AcceptEdits") {
                    "accept-edits"
                } else if self.permission_mode.contains("DontAsk") {
                    "dont-ask"
                } else {
                    "default"
                };
                if !parts.is_empty() {
                    parts.push(Span::styled(
                        " · ",
                        Style::default().fg(theme::border()).bg(theme::header_bg()),
                    ));
                }
                parts.push(Span::styled(
                    mode_label,
                    Style::default().fg(theme::warn()).bg(theme::header_bg()),
                ));
            }
            Some(Line::from(parts))
        } else {
            None
        };

        let project_line = if self.project_count > 1 {
            Some(Line::from(vec![Span::styled(
                format!(
                    "◆ {} projects connected · Ctrl+X p to switch",
                    self.project_count
                ),
                Style::default().fg(theme::muted()).bg(theme::header_bg()),
            )]))
        } else {
            None
        };

        let info_lines: Vec<Line> = [
            Some(title_line),
            Some(model_line),
            Some(ws_line),
            branch_mode,
            project_line,
        ]
        .into_iter()
        .flatten()
        .collect();

        for (i, line) in info_lines.iter().enumerate() {
            let y = area.y + 1 + i as u16;
            if y >= area.y + area.height {
                break;
            }
            let line_area = Rect::new(info_x, y, info_w as u16, 1);
            line.render(line_area, buf);
        }

        let sep_y = area.y + area.height - 1;
        if sep_y > area.y {
            let sep_char = "─";
            for x in area.x..area.x + area.width {
                buf[(x, sep_y)]
                    .set_symbol(sep_char)
                    .set_style(Style::default().fg(theme::border()).bg(theme::header_bg()));
            }
        }
    }
}
