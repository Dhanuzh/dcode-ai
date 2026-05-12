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
    pub busy_label: &'a str,
    pub elapsed_secs: u64,
    pub mcp_servers: usize,
    pub sandbox_status: Option<bool>,
    pub last_turn: Option<&'a TurnStats>,
    pub permission_bypass: bool,
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let sep = Span::styled(" │ ", Style::default().fg(Color::Rgb(70, 70, 70)));
        let model_display = truncate_chars(self.model, 20);
        let (busy_icon, busy_color) = busy_badge(self.busy_label);
        let version = env!("CARGO_PKG_VERSION");

        let mut spans = vec![
            Span::styled(
                format!(" {busy_icon} {} ", self.busy_label.to_ascii_uppercase()),
                Style::default().fg(busy_color).add_modifier(Modifier::BOLD),
            ),
            sep.clone(),
            Span::styled(
                format!(" /{} ", model_display),
                Style::default().fg(Color::Rgb(170, 170, 170)),
            ),
            sep.clone(),
            Span::styled(
                format!(" {} ", truncate_chars(self.agent, 16)),
                Style::default().fg(Color::Rgb(185, 150, 230)),
            ),
        ];

        spans.push(sep.clone());
        spans.push(Span::styled(
            format!(" {}s ", self.elapsed_secs),
            Style::default().fg(Color::Rgb(150, 150, 150)),
        ));

        if self.mcp_servers > 0 {
            spans.push(sep.clone());
            spans.push(Span::styled(
                format!(" ⚡{} ", self.mcp_servers),
                Style::default().fg(Color::Rgb(110, 210, 170)),
            ));
        }

        if let Some(sandboxed) = self.sandbox_status {
            spans.push(sep.clone());
            let (icon, label, color) = if sandboxed {
                ("🛡", "sandboxed", Color::Green)
            } else {
                ("⚠", "unsandboxed", Color::Yellow)
            };
            spans.push(Span::styled(
                format!(" {icon} {label} "),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }

        if let Some(turn) = self.last_turn {
            spans.push(sep.clone());
            spans.push(Span::styled(
                format!(
                    " in:{} out:{} {}s ",
                    turn.tokens_in, turn.tokens_out, turn.elapsed_secs
                ),
                Style::default().fg(Color::Rgb(135, 135, 135)),
            ));
        }

        if self.permission_bypass {
            spans.push(sep.clone());
            spans.push(Span::styled(
                " BYPASS ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(255, 120, 140))
                    .add_modifier(Modifier::BOLD),
            ));
        }

        spans.push(sep.clone());
        spans.push(Span::styled(
            format!(" v{} ", version),
            Style::default().fg(Color::Rgb(145, 145, 145)),
        ));

        Line::from(spans).render(area, buf);
    }
}

fn busy_badge(label: &str) -> (&'static str, Color) {
    let lower = label.to_ascii_lowercase();
    if lower.contains("error") {
        ("✖", Color::Red)
    } else if lower.contains("idle") {
        ("•", Color::DarkGray)
    } else if lower.contains("wait") || lower.contains("tool") {
        ("◐", Color::Yellow)
    } else {
        ("•", Color::Green)
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out = s.chars().take(max.saturating_sub(1)).collect::<String>();
        out.push('…');
        out
    }
}
