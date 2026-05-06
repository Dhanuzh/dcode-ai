use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

pub struct QueuePreview<'a> {
    items: &'a [String],
    total: usize,
}

impl<'a> QueuePreview<'a> {
    pub const MAX_VISIBLE: usize = 3;

    pub fn new(items: &'a [String], total: usize) -> Self {
        Self { items, total }
    }

    pub fn height_for(total: usize) -> u16 {
        if total == 0 {
            return 0;
        }
        let rows = total.min(Self::MAX_VISIBLE) as u16;
        // rows + hint footer
        rows + 1
    }
}

impl Widget for QueuePreview<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if self.total == 0 || area.height == 0 || area.width == 0 {
            return;
        }

        let max_text_w = area.width.saturating_sub(8) as usize; // "  📋 N  "
        let visible = self
            .items
            .iter()
            .take(Self::MAX_VISIBLE)
            .collect::<Vec<_>>();
        for (row, item) in visible.iter().enumerate() {
            let y = area.y + row as u16;
            if y >= area.y + area.height {
                break;
            }

            let flat = item.replace('\n', " ");
            let preview = truncate_with_ellipsis(&flat, max_text_w);
            let line = Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled("📋 ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("{}  ", row + 1),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(preview, Style::default().fg(Color::Gray)),
            ]);
            line.render(Rect::new(area.x, y, area.width, 1), buf);
        }

        let overflow = self.total.saturating_sub(Self::MAX_VISIBLE);
        let hint_y = area.y + visible.len() as u16;
        if hint_y >= area.y + area.height {
            return;
        }
        if overflow > 0 {
            let line = Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    format!("+ {overflow} more"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    "  ·  ↑ pop  ·  Ctrl+U clear",
                    Style::default().fg(Color::Rgb(80, 80, 80)),
                ),
            ]);
            line.render(Rect::new(area.x, hint_y, area.width, 1), buf);
        } else {
            let line = Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    "↑ pop  ·  Ctrl+U clear",
                    Style::default().fg(Color::Rgb(80, 80, 80)),
                ),
            ]);
            line.render(Rect::new(area.x, hint_y, area.width, 1), buf);
        }
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
