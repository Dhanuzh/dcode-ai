use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

#[derive(Debug, Clone)]
pub struct ActivityRow {
    pub id: String,
    pub phase: String,
    pub detail: String,
    pub running: bool,
}

pub struct ChildActivityOverlay<'a> {
    rows: &'a [ActivityRow],
    total: usize,
}

impl<'a> ChildActivityOverlay<'a> {
    pub const MAX_VISIBLE: usize = 5;

    pub fn new(rows: &'a [ActivityRow], total: usize) -> Self {
        Self { rows, total }
    }

    pub fn height_for(total: usize) -> u16 {
        if total == 0 {
            0
        } else {
            total.min(Self::MAX_VISIBLE) as u16 + 1
        }
    }
}

impl Widget for ChildActivityOverlay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let visible = self.rows.iter().take(Self::MAX_VISIBLE).collect::<Vec<_>>();
        let max_activity_w = area.width.saturating_sub(28) as usize;
        for (row_idx, row) in visible.iter().enumerate() {
            let y = area.y + row_idx as u16;
            if y >= area.y + area.height {
                break;
            }

            let icon = if row.running { "🤖" } else { "○" };
            let icon_color = if row.running {
                Color::Yellow
            } else {
                Color::DarkGray
            };
            let id8 = short_id(&row.id, 8);
            let phase = truncate_with_ellipsis(&row.phase.replace('\n', " "), 12);
            let detail = truncate_with_ellipsis(&row.detail.replace('\n', " "), max_activity_w);

            let mut spans: Vec<Span<'static>> = vec![
                Span::raw("  "),
                Span::styled(format!("{icon} "), Style::default().fg(icon_color)),
                Span::styled(
                    format!("{id8:<8}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {:<12}", phase),
                    Style::default().fg(Color::Rgb(160, 160, 160)),
                ),
            ];
            if !detail.is_empty() {
                spans.push(Span::styled(
                    " · ".to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
                spans.push(Span::styled(detail, Style::default().fg(Color::Gray)));
            }
            Line::from(spans).render(Rect::new(area.x, y, area.width, 1), buf);
        }

        let hint_y = area.y + visible.len() as u16;
        if hint_y >= area.y + area.height {
            return;
        }
        let overflow = self.total.saturating_sub(visible.len());
        let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
        if overflow > 0 {
            spans.push(Span::styled(
                format!("+ {overflow} more"),
                Style::default().fg(Color::DarkGray),
            ));
            spans.push(Span::styled(
                "  ·  ",
                Style::default().fg(Color::Rgb(80, 80, 80)),
            ));
        }
        spans.push(Span::styled(
            "Ctrl+G details  ·  Esc cancel turn",
            Style::default().fg(Color::Rgb(80, 80, 80)),
        ));
        Line::from(spans).render(Rect::new(area.x, hint_y, area.width, 1), buf);
    }
}

fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out = s.chars().take(max.saturating_sub(1)).collect::<String>();
        out.push('…');
        out
    }
}

fn short_id(id: &str, max: usize) -> String {
    id.chars().take(max).collect()
}
