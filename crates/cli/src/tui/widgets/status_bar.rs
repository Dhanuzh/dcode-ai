use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

pub struct StatusBar<'a> {
    pub model: &'a str,
    pub agent: &'a str,
    pub busy_label: &'a str,
    pub elapsed_secs: u64,
    pub mcp_servers: usize,
    pub sandbox_status: Option<bool>,
    /// Estimated current context-window occupancy, for the ctx gauge.
    pub context_tokens: u64,
    /// Cumulative session token counts and estimated cost.
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f64,
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
                format!(" ◇mcp {} ", self.mcp_servers),
                Style::default().fg(Color::Rgb(110, 210, 170)),
            ));
        }

        if let Some(sandboxed) = self.sandbox_status {
            spans.push(sep.clone());
            let (icon, label, color) = if sandboxed {
                ("▣", "sandboxed", Color::Green)
            } else {
                ("△", "unsandboxed", Color::Yellow)
            };
            spans.push(Span::styled(
                format!(" {icon} {label} "),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }

        if self.context_tokens > 0 {
            spans.push(sep.clone());
            // Context-window fullness vs the model's window, so the user sees
            // how close they are to compaction.
            spans.extend(context_gauge_spans(self.context_tokens, self.model));
        }

        if self.tokens_in > 0 || self.tokens_out > 0 {
            spans.push(Span::styled(
                format!(
                    " {} in / {} out · ${:.4} ",
                    self.tokens_in, self.tokens_out, self.cost_usd
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

/// A 5-segment context-window gauge (`ctx ▰▰▰▱▱ 62%`), colored by fullness:
/// green < 70%, amber < 90%, red ≥ 90%. Empty if the model window is unknown.
fn context_gauge_spans(used_tokens: u64, model: &str) -> Vec<Span<'static>> {
    let window = dcode_ai_runtime::model_limits::detect_context_window(model) as u64;
    if window == 0 {
        return Vec::new();
    }
    let pct = ((used_tokens.min(window) as f64 / window as f64) * 100.0).round() as u64;
    const SEGS: usize = 5;
    let filled = ((pct as usize * SEGS) / 100).min(SEGS);
    let bar: String = "▰".repeat(filled) + &"▱".repeat(SEGS - filled);
    let color = if pct >= 90 {
        Color::Rgb(255, 120, 120)
    } else if pct >= 70 {
        Color::Rgb(230, 190, 120)
    } else {
        Color::Rgb(135, 180, 135)
    };
    let sep = Span::styled(" │ ", Style::default().fg(Color::Rgb(70, 70, 70)));
    vec![
        Span::styled(format!(" ctx {bar} {pct}% "), Style::default().fg(color)),
        sep,
    ]
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

#[cfg(test)]
mod tests {
    use super::context_gauge_spans;

    fn gauge_text(used: u64, model: &str) -> String {
        context_gauge_spans(used, model)
            .iter()
            .map(|s| s.content.as_ref().to_string())
            .collect()
    }

    #[test]
    fn context_gauge_shows_percentage_and_label() {
        // gpt-4o has a known 128k window in model_limits.
        let empty = gauge_text(0, "gpt-4o");
        assert!(empty.contains("ctx"), "got: {empty}");
        assert!(empty.contains("0%"), "got: {empty}");

        let full = gauge_text(128_000, "gpt-4o");
        assert!(full.contains("100%"), "got: {full}");
        // Full bar uses only filled segments.
        assert!(full.contains("▰▰▰▰▰"), "got: {full}");
    }

    #[test]
    fn context_gauge_clamps_overflow() {
        // Beyond the window still caps at 100%.
        let over = gauge_text(10_000_000, "gpt-4o");
        assert!(over.contains("100%"), "got: {over}");
    }
}
