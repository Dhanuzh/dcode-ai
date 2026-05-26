//! Small pure rendering helpers for the transcript: tool effect/dot/status
//! styling, sub-agent phase→progress mapping, and char-window slicing.
//! No `TuiSessionState` access — extracted from `tui::app` ahead of lifting the
//! larger transcript renderer.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::tui::theme;

/// Map a sub-agent phase string to a 0–100 progress estimate.
pub(crate) fn subagent_phase_progress(phase: &str, running: bool) -> u8 {
    let p = phase.to_ascii_lowercase();
    if !running
        || p.contains("done")
        || p.contains("complete")
        || p.contains("success")
        || p.contains("finished")
    {
        return 100;
    }
    if p.contains("spawn") || p.contains("queue") {
        15
    } else if p.contains("plan") {
        30
    } else if p.contains("search") || p.contains("inspect") || p.contains("read") {
        45
    } else if p.contains("edit") || p.contains("write") || p.contains("patch") {
        70
    } else if p.contains("test") || p.contains("verify") {
        85
    } else {
        55
    }
}

/// Slice `width` chars from `s` starting at char index `start`.
pub(crate) fn char_window(s: &str, start: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    s.chars().skip(start).take(width).collect()
}

pub(crate) fn tool_effect_badge(name: &str) -> Span<'static> {
    use crate::tui::tool_classify::ToolEffect;
    let effect = crate::tui::tool_classify::classify_tool(name);
    let display = effect.display();
    let style = match effect {
        ToolEffect::ReadOnly => Style::default().fg(theme::success()),
        ToolEffect::RemoteAction => Style::default().fg(theme::assistant()),
        ToolEffect::LocalMutation => Style::default().fg(theme::warn()),
        ToolEffect::Destructive => Style::default().fg(theme::error()),
    }
    .add_modifier(Modifier::BOLD);
    Span::styled(format!("[{} {}]", display.badge, display.label), style)
}

pub(crate) fn tool_dot_style(name: &str) -> Style {
    use crate::tui::tool_classify::ToolEffect;
    match crate::tui::tool_classify::classify_tool(name) {
        ToolEffect::ReadOnly => Style::default().fg(theme::muted()),
        ToolEffect::RemoteAction => Style::default().fg(theme::assistant()),
        ToolEffect::LocalMutation => Style::default().fg(theme::warn()),
        ToolEffect::Destructive => Style::default().fg(theme::error()),
    }
}

pub(crate) fn tool_status_chip(label: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!("[{label}]"),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

/// Render a `[===···] NN%` progress bar of the given cell width.
pub(crate) fn progress_bar(percent: u8, width: usize) -> String {
    let w = width.max(8);
    let filled = (usize::from(percent) * w) / 100;
    let mut out = String::with_capacity(w + 10);
    out.push('[');
    out.push_str(&"=".repeat(filled));
    out.push_str(&"·".repeat(w.saturating_sub(filled)));
    out.push(']');
    out.push(' ');
    out.push_str(&format!("{percent:>3}%"));
    out
}

/// Word-wrap `s` to `width` columns, hard-splitting words longer than `width`.
pub(crate) fn wrap_text(s: &str, width: usize) -> Vec<String> {
    if width < 8 {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    for paragraph in s.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let word_chars = word.chars().count();
            if line.is_empty() && word_chars > width {
                for chunk in wrap_preformatted_line(word, width) {
                    out.push(chunk);
                }
                continue;
            } else if line.is_empty() {
                line = word.to_string();
            } else if line.chars().count() + 1 + word_chars <= width {
                line.push(' ');
                line.push_str(word);
            } else if word_chars > width {
                out.push(std::mem::take(&mut line));
                let chunks = wrap_preformatted_line(word, width);
                let chunk_len = chunks.len();
                for (idx, chunk) in chunks.into_iter().enumerate() {
                    if idx + 1 == chunk_len {
                        line = chunk;
                        break;
                    }
                    out.push(chunk);
                }
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    if out.is_empty() && !s.is_empty() {
        out.push(s.to_string());
    }
    out
}

/// Hard-wrap a single preformatted line to `width` columns (no word breaking).
pub(crate) fn wrap_preformatted_line(line: &str, width: usize) -> Vec<String> {
    if width < 4 || line.is_empty() {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for ch in line.chars() {
        if current_len >= width {
            out.push(current);
            current = String::new();
            current_len = 0;
        }
        current.push(ch);
        current_len += 1;
    }
    if out.is_empty() || !current.is_empty() {
        out.push(current);
    }
    out
}

/// Truncate `s` to `max` chars, appending `…` when shortened.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// A colored permission-mode pill for the composer title bar, so the active
/// mode (default / plan / accept-edits / dont-ask / bypass) is always visible.
pub(crate) fn permission_mode_pill(mode: &str) -> Span<'static> {
    let (label, color) = if mode.contains("Bypass") {
        ("BYPASS", Color::Rgb(255, 120, 140))
    } else if mode.contains("Plan") {
        ("PLAN", Color::Rgb(120, 200, 230))
    } else if mode.contains("AcceptEdits") {
        ("ACCEPT-EDITS", Color::Rgb(135, 200, 135))
    } else if mode.contains("DontAsk") {
        ("DONT-ASK", Color::Rgb(230, 190, 120))
    } else {
        ("DEFAULT", Color::Rgb(150, 150, 150))
    };
    Span::styled(
        format!(" {label} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

#[cfg(test)]
mod tests {
    use super::permission_mode_pill;

    #[test]
    fn mode_pill_labels_each_mode() {
        assert!(
            permission_mode_pill("BypassPermissions")
                .content
                .contains("BYPASS")
        );
        assert!(permission_mode_pill("Plan").content.contains("PLAN"));
        assert!(
            permission_mode_pill("AcceptEdits")
                .content
                .contains("ACCEPT-EDITS")
        );
        assert!(permission_mode_pill("Default").content.contains("DEFAULT"));
    }
}
