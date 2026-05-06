use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
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
    pub const MAX_VISIBLE: usize = 2;

    pub fn new(rows: &'a [ActivityRow], total: usize) -> Self {
        Self { rows, total }
    }

    pub fn height_for(total: usize) -> u16 {
        if total == 0 {
            0
        } else {
            total.min(Self::MAX_VISIBLE) as u16
        }
    }
}

impl Widget for ChildActivityOverlay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        for (i, row) in self.rows.iter().take(Self::MAX_VISIBLE).enumerate() {
            if i as u16 >= area.height {
                break;
            }
            let dot = if row.running { "●" } else { "○" };
            let text = format!(
                "  {dot} {} · {}{}",
                row.id,
                row.phase,
                if row.detail.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", row.detail)
                }
            );
            Line::from(Span::styled(
                text,
                Style::default().fg(if row.running {
                    Color::Rgb(220, 180, 90)
                } else {
                    Color::Rgb(120, 120, 120)
                }),
            ))
            .render(Rect::new(area.x, area.y + i as u16, area.width, 1), buf);
        }

        if self.total > Self::MAX_VISIBLE && area.height > 0 {
            let line = format!(
                "  + {} more background tasks",
                self.total - Self::MAX_VISIBLE
            );
            Line::from(Span::styled(
                line,
                Style::default().fg(Color::Rgb(110, 110, 110)),
            ))
            .render(
                Rect::new(area.x, area.y + area.height - 1, area.width, 1),
                buf,
            );
        }
    }
}
