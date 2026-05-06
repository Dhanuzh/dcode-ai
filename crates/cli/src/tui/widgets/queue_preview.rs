use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
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

        let visible = self
            .items
            .iter()
            .take(Self::MAX_VISIBLE)
            .collect::<Vec<_>>();
        for (i, item) in visible.iter().enumerate() {
            if i as u16 >= area.height.saturating_sub(1) {
                break;
            }
            let text = format!("  📋 {}  {}", i + 1, item);
            Line::from(Span::styled(
                text,
                Style::default().fg(Color::Rgb(140, 140, 140)),
            ))
            .render(Rect::new(area.x, area.y + i as u16, area.width, 1), buf);
        }

        let mut hint = String::from("  ↑ pop · Ctrl+U clear");
        if self.total > Self::MAX_VISIBLE {
            hint.push_str(&format!(" · +{} more", self.total - Self::MAX_VISIBLE));
        }
        Line::from(Span::styled(
            hint,
            Style::default().fg(Color::Rgb(110, 110, 110)),
        ))
        .render(
            Rect::new(
                area.x,
                area.y + area.height.saturating_sub(1),
                area.width,
                1,
            ),
            buf,
        );
    }
}
