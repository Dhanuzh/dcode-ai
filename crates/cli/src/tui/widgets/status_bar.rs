use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

#[derive(Debug, Clone, Default)]
pub struct TurnStats {
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub elapsed_secs: u64,
}

pub struct StatusBar<'a> {
    pub model: &'a str,
    pub agent: &'a str,
    pub branch: &'a str,
    pub busy_label: &'a str,
    pub context_pct: u32,
    pub elapsed_secs: u64,
    pub mcp_servers: usize,
    pub sandbox_status: Option<bool>,
    pub last_turn: Option<&'a TurnStats>,
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let _ = (self.context_pct, self.mcp_servers, self.sandbox_status);

        let mut spans = vec![
            Span::styled(
                format!(" {} ", self.busy_label),
                Style::default().fg(Color::Rgb(120, 220, 140)),
            ),
            Span::raw(" │ "),
            Span::styled(self.model, Style::default().fg(Color::Rgb(120, 200, 255))),
            Span::raw(" │ "),
            Span::styled(self.agent, Style::default().fg(Color::Rgb(200, 150, 255))),
        ];

        if !self.branch.is_empty() {
            spans.push(Span::raw(" │ "));
            spans.push(Span::styled(
                format!("⎇ {}", self.branch),
                Style::default()
                    .fg(Color::Rgb(120, 220, 220))
                    .add_modifier(Modifier::UNDERLINED),
            ));
        }

        if let Some(turn) = self.last_turn {
            spans.extend([
                Span::raw(" │ "),
                Span::styled(
                    format!(
                        "last in:{} out:{} {}s",
                        turn.tokens_in, turn.tokens_out, turn.elapsed_secs
                    ),
                    Style::default().fg(Color::Rgb(140, 140, 140)),
                ),
            ]);
        }

        spans.extend([
            Span::raw(" │ "),
            Span::styled(
                format!(
                    "{:02}:{:02}",
                    self.elapsed_secs / 60,
                    self.elapsed_secs % 60
                ),
                Style::default().fg(Color::Rgb(130, 130, 130)),
            ),
        ]);

        Line::from(spans).render(area, buf);
    }
}
