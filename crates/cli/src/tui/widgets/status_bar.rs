#![allow(dead_code)]

use crate::tui::theme;
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
    pub context_tokens: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f64,
    pub permission_bypass: bool,
    pub last_turn_latency_ms: Option<u64>,
    pub turn_output_tokens: u64,
    pub context_compacted: bool,
    pub notification_count: u16,
    pub effort_label: &'a str,
    pub compaction_in_progress: bool,
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        for x in area.x..area.x + area.width {
            for y in area.y..area.y + area.height {
                buf[(x, y)].set_style(Style::default().bg(theme::surface()));
            }
        }

        let dot_sep = Span::styled(
            " · ",
            Style::default().fg(theme::border()).bg(theme::surface()),
        );

        let (status_icon, status_color) = busy_badge(self.busy_label);
        let model_display = truncate_chars(self.model, 28);
        let version = env!("CARGO_PKG_VERSION");

        // ── Line 1: Status + Model + Agent + Live Info ──
        let mut line1 = vec![
            Span::styled(
                format!(" {status_icon} "),
                Style::default()
                    .fg(status_color)
                    .bg(theme::surface())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                self.busy_label.trim().to_ascii_uppercase(),
                Style::default()
                    .fg(status_color)
                    .bg(theme::surface())
                    .add_modifier(Modifier::BOLD),
            ),
            dot_sep.clone(),
            Span::styled(
                model_display,
                Style::default()
                    .fg(provider_model_color(self.model))
                    .bg(theme::surface()),
            ),
        ];

        if !self.effort_label.is_empty() {
            line1.push(Span::styled(
                format!(" [{}]", self.effort_label),
                Style::default()
                    .fg(theme::warn())
                    .bg(theme::surface())
                    .add_modifier(Modifier::BOLD),
            ));
        }

        if !self.agent.is_empty() && self.agent != "default" {
            line1.push(dot_sep.clone());
            line1.push(Span::styled(
                truncate_chars(self.agent, 16),
                Style::default().fg(theme::assistant()).bg(theme::surface()),
            ));
        }

        if self.turn_output_tokens > 0 && !self.busy_label.eq_ignore_ascii_case("idle") {
            line1.push(dot_sep.clone());
            line1.push(Span::styled(
                format!("~{}t", self.turn_output_tokens),
                Style::default().fg(theme::accent()).bg(theme::surface()),
            ));
        } else if let Some(latency_ms) = self.last_turn_latency_ms {
            line1.push(dot_sep.clone());
            let label = if latency_ms < 1000 {
                format!("{}ms", latency_ms)
            } else {
                format!("{:.1}s", latency_ms as f64 / 1000.0)
            };
            line1.push(Span::styled(
                format!("↩ {label}"),
                Style::default().fg(theme::muted()).bg(theme::surface()),
            ));
        }

        if self.mcp_servers > 0 {
            line1.push(dot_sep.clone());
            line1.push(Span::styled(
                format!("◇{}", self.mcp_servers),
                Style::default().fg(theme::tool()).bg(theme::surface()),
            ));
        }

        if let Some(sandboxed) = self.sandbox_status {
            line1.push(dot_sep.clone());
            let (icon, color) = if sandboxed {
                ("▣", theme::success())
            } else {
                ("△", theme::warn())
            };
            line1.push(Span::styled(
                icon,
                Style::default()
                    .fg(color)
                    .bg(theme::surface())
                    .add_modifier(Modifier::BOLD),
            ));
        }

        // Right-align: elapsed time
        {
            let elapsed = format!(" {}s ", self.elapsed_secs);
            let used_w: usize = line1.iter().map(|s| s.content.len()).sum();
            let remaining = (area.width as usize).saturating_sub(used_w + elapsed.len());
            if remaining > 0 {
                line1.push(Span::styled(
                    " ".repeat(remaining),
                    Style::default().bg(theme::surface()),
                ));
            }
            line1.push(Span::styled(
                elapsed,
                Style::default().fg(theme::muted()).bg(theme::surface()),
            ));
        }

        // ── Line 2: Tokens + Cost + Context gauge + Badges ──
        let mut line2 = Vec::new();

        if self.compaction_in_progress {
            line2.push(Span::styled(
                " ⟳ compacting… ",
                Style::default()
                    .fg(theme::warn())
                    .bg(theme::surface())
                    .add_modifier(Modifier::BOLD),
            ));
            line2.push(dot_sep.clone());
        }

        if self.context_tokens > 0 {
            line2.extend(context_gauge_spans(self.context_tokens, self.model));
        }

        if self.tokens_in > 0 || self.tokens_out > 0 {
            line2.push(Span::styled(
                format!(
                    " {}↓ {}↑ · ${:.4}",
                    format_tokens(self.tokens_in),
                    format_tokens(self.tokens_out),
                    self.cost_usd
                ),
                Style::default().fg(theme::muted()).bg(theme::surface()),
            ));
        }

        if self.context_compacted {
            line2.push(dot_sep.clone());
            line2.push(Span::styled(
                "compacted",
                Style::default()
                    .fg(theme::warn())
                    .bg(theme::surface())
                    .add_modifier(Modifier::BOLD),
            ));
        }

        if self.notification_count > 0 {
            line2.push(dot_sep.clone());
            line2.push(Span::styled(
                format!("↓{} new", self.notification_count),
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::warn())
                    .add_modifier(Modifier::BOLD),
            ));
        }

        if self.permission_bypass {
            line2.push(dot_sep.clone());
            line2.push(Span::styled(
                " BYPASS ",
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::error())
                    .add_modifier(Modifier::BOLD),
            ));
        }

        // Right-align: version
        {
            let ver = format!(" v{version} ");
            let used_w: usize = line2.iter().map(|s| s.content.len()).sum();
            let remaining = (area.width as usize).saturating_sub(used_w + ver.len());
            if remaining > 0 {
                line2.push(Span::styled(
                    " ".repeat(remaining),
                    Style::default().bg(theme::surface()),
                ));
            }
            line2.push(Span::styled(
                ver,
                Style::default().fg(theme::muted()).bg(theme::surface()),
            ));
        }

        if area.height >= 2 {
            let row1 = Rect::new(area.x, area.y, area.width, 1);
            let row2 = Rect::new(area.x, area.y + 1, area.width, 1);
            Line::from(line1).render(row1, buf);
            Line::from(line2).render(row2, buf);
        } else {
            line1.extend(line2);
            Line::from(line1).render(area, buf);
        }
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub fn context_gauge_spans(used_tokens: u64, model: &str) -> Vec<Span<'static>> {
    let window = dcode_ai_runtime::model_limits::detect_context_window(model) as u64;
    if window == 0 {
        return Vec::new();
    }
    let pct = ((used_tokens.min(window) as f64 / window as f64) * 100.0).round() as u64;
    const SEGS: usize = 8;
    let filled = ((pct as usize * SEGS) / 100).min(SEGS);
    let bar: String = "█".repeat(filled) + &"░".repeat(SEGS - filled);
    let color = if pct >= 90 {
        theme::error()
    } else if pct >= 70 {
        theme::warn()
    } else {
        theme::success()
    };
    let dot_sep = Span::styled(
        " · ",
        Style::default().fg(theme::border()).bg(theme::surface()),
    );
    vec![
        Span::styled(
            format!(" {bar} {pct}%"),
            Style::default().fg(color).bg(theme::surface()),
        ),
        dot_sep,
    ]
}

fn provider_model_color(model: &str) -> Color {
    let m = model.to_ascii_lowercase();
    if m.contains("claude") || m.contains("fable") {
        Color::Rgb(204, 139, 72)
    } else if m.starts_with("gpt")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
    {
        Color::Rgb(116, 184, 134)
    } else if m.contains("gemini") {
        Color::Rgb(102, 153, 255)
    } else if m.contains("deepseek") {
        Color::Rgb(85, 170, 255)
    } else if m.contains("minimax") {
        Color::Rgb(255, 153, 51)
    } else if m.contains("mistral") || m.contains("codestral") {
        Color::Rgb(255, 119, 51)
    } else if m.contains("llama") || m.contains("meta") {
        Color::Rgb(51, 153, 255)
    } else {
        theme::text()
    }
}

fn busy_badge(label: &str) -> (&'static str, Color) {
    let lower = label.to_ascii_lowercase();
    if lower.contains("error") {
        ("✖", theme::error())
    } else if lower.contains("idle") {
        ("●", theme::muted())
    } else if lower.contains("wait") || lower.contains("tool") {
        ("◐", theme::warn())
    } else if lower.contains("stream") {
        ("◉", theme::success())
    } else if lower.contains("think") {
        ("◎", theme::accent())
    } else {
        ("●", theme::success())
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
        let empty = gauge_text(0, "gpt-4o");
        assert!(empty.contains("0%"), "got: {empty}");

        let full = gauge_text(128_000, "gpt-4o");
        assert!(full.contains("100%"), "got: {full}");
    }

    #[test]
    fn context_gauge_clamps_overflow() {
        let over = gauge_text(10_000_000, "gpt-4o");
        assert!(over.contains("100%"), "got: {over}");
    }
}
