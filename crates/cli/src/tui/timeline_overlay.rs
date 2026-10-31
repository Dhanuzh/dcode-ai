//! `/timeline`: the session as steps you can scrub. Steps on the left (time,
//! icon, what happened, ✓/✗), the selected step's detail on the right.
//! Enter on one of your messages opens rewind at it. Alt-screen with its own
//! input loop, like the diff view.

use crate::tui::activity::{Step, StepKind};
use crate::tui::render_helpers::truncate_chars;
use crate::tui::state::TuiSessionState;
use crate::tui::theme;
use crossterm::{
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind, poll, read},
    execute,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap},
};
use std::io::stdout;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn kind_color(kind: StepKind) -> ratatui::style::Color {
    match kind {
        StepKind::User => theme::accent(),
        StepKind::Tool => theme::muted(),
        StepKind::Reply => theme::text(),
        StepKind::Compaction => theme::warn(),
        StepKind::Agent => theme::accent(),
        StepKind::Error => theme::error(),
    }
}

/// One row per step: `12:04  ⚙ edit_file src/a.rs  ✓`.
pub(crate) fn step_rows(steps: &[Step], selected: usize, width: usize) -> Vec<Line<'static>> {
    steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mark = match s.ok {
                Some(true) => " ✓",
                Some(false) => " ✗",
                None => "",
            };
            let time = s.at.format("%H:%M").to_string();
            let room = width.saturating_sub(time.len() + 4 + mark.len());
            let mut label_style = Style::default().fg(kind_color(s.kind));
            if i == selected {
                label_style = label_style
                    .add_modifier(Modifier::BOLD)
                    .bg(theme::surface());
            }
            Line::from(vec![
                Span::styled(
                    if i == selected { "❯ " } else { "  " },
                    Style::default().fg(theme::accent()),
                ),
                Span::styled(format!("{time} "), Style::default().fg(theme::muted())),
                Span::styled(
                    format!("{} {}", s.kind.icon(), truncate_chars(&s.label, room)),
                    label_style,
                ),
                Span::styled(
                    mark,
                    Style::default().fg(if s.ok == Some(false) {
                        theme::error()
                    } else {
                        theme::success()
                    }),
                ),
            ])
        })
        .collect()
}

/// A bar of the whole session: one cell per step bucket, the cursor's
/// position marked — the "scrubber".
pub(crate) fn scrubber(steps: &[Step], selected: usize, width: usize) -> Line<'static> {
    let n = steps.len().max(1);
    let cells = width.clamp(1, 120);
    // First step at the left edge, last at the right.
    let at = if n <= 1 {
        0
    } else {
        selected.min(n - 1) * (cells - 1) / (n - 1)
    };
    let spans: Vec<Span> = (0..cells)
        .map(|c| {
            let i = (c * n / cells).min(n - 1);
            let kind = steps.get(i).map(|s| s.kind).unwrap_or(StepKind::Tool);
            let glyph = match kind {
                StepKind::User => "▌",
                StepKind::Compaction => "┃",
                StepKind::Error => "╳",
                _ => "▁",
            };
            if c == at {
                Span::styled("◆", Style::default().fg(theme::accent()))
            } else {
                Span::styled(glyph, Style::default().fg(kind_color(kind)))
            }
        })
        .collect();
    Line::from(spans)
}

/// Run the timeline until closed.
pub(crate) fn run_timeline_overlay(state: &Arc<Mutex<TuiSessionState>>) -> anyhow::Result<()> {
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    execute!(
        stdout(),
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut term = ratatui::Terminal::new(backend)?;
    // Start on the newest step.
    let mut selected = state
        .lock()
        .map(|g| g.timeline.steps.len().saturating_sub(1))
        .unwrap_or(0);
    let mut detail_scroll: u16 = 0;

    let res: anyhow::Result<()> = (|| {
        loop {
            let size = term.size()?;
            let area = Rect::new(0, 0, size.width, size.height);
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(area);
            let list_w = (area.width * 2 / 5).clamp(30, 70);
            let cols = Layout::horizontal([Constraint::Length(list_w), Constraint::Min(20)])
                .split(rows[1]);
            let view_h = cols[0].height.saturating_sub(2) as usize;

            let (list, bar, title, detail, count) = {
                let g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                if !g.timeline_open {
                    return Ok(());
                }
                let steps = &g.timeline.steps;
                selected = selected.min(steps.len().saturating_sub(1));
                let rows = step_rows(steps, selected, list_w.saturating_sub(4) as usize);
                // Keep the selection in view.
                let start = selected.saturating_sub(view_h.saturating_sub(1) / 2);
                let start = start.min(rows.len().saturating_sub(view_h));
                let list: Vec<Line> = rows.into_iter().skip(start).take(view_h).collect();
                let bar = scrubber(steps, selected, area.width.saturating_sub(2) as usize);
                let (title, detail) = match steps.get(selected) {
                    Some(s) => (
                        format!(
                            "{} {} · {}",
                            s.kind.icon(),
                            match s.kind {
                                StepKind::User => "your message",
                                StepKind::Tool => "tool call",
                                StepKind::Reply => "reply",
                                StepKind::Compaction => "compaction",
                                StepKind::Agent => "sub-agent",
                                StepKind::Error => "error",
                            },
                            s.at.format("%H:%M:%S")
                        ),
                        s.detail.clone(),
                    ),
                    None => ("timeline".into(), "Nothing has happened yet.".into()),
                };
                (list, bar, title, detail, steps.len())
            };

            term.draw(|frame| {
                frame.render_widget(
                    Block::default().style(Style::default().bg(theme::bg())),
                    area,
                );
                frame.render_widget(Paragraph::new(bar), rows[0].inner(ratatui::layout::Margin::new(1, 0)));
                let pane = |t: String| {
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(theme::border()))
                        .title(Span::styled(
                            format!(" {t} "),
                            Style::default()
                                .fg(theme::accent())
                                .add_modifier(Modifier::BOLD),
                        ))
                        .padding(Padding::horizontal(1))
                };
                frame.render_widget(
                    Paragraph::new(Text::from(list))
                        .block(pane(format!("{} / {count} steps", selected + 1))),
                    cols[0],
                );
                frame.render_widget(
                    Paragraph::new(detail)
                        .wrap(Wrap { trim: false })
                        .scroll((detail_scroll, 0))
                        .block(pane(title)),
                    cols[1],
                );
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        " ←/→ or ↑/↓ scrub · PgUp/PgDn jump · Home/End · J/K scroll detail · Enter on your message: rewind there · q close",
                        Style::default().fg(theme::muted()),
                    )))
                    .style(Style::default().bg(theme::surface())),
                    rows[2],
                );
            })?;

            if !poll(Duration::from_millis(250))? {
                continue;
            }
            let ev = read()?;
            let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            let last = g.timeline.steps.len().saturating_sub(1);
            let mut moved = true;
            match ev {
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollUp => selected = selected.saturating_sub(1),
                    MouseEventKind::ScrollDown => selected = (selected + 1).min(last),
                    _ => moved = false,
                },
                Event::Key(key) if !matches!(key.kind, KeyEventKind::Release) => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        g.timeline_open = false;
                        return Ok(());
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        g.timeline_open = false;
                        return Ok(());
                    }
                    KeyCode::Left | KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('h') => {
                        selected = selected.saturating_sub(1)
                    }
                    KeyCode::Right | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('l') => {
                        selected = (selected + 1).min(last)
                    }
                    KeyCode::PageUp => selected = selected.saturating_sub(view_h.max(1)),
                    KeyCode::PageDown => selected = (selected + view_h.max(1)).min(last),
                    KeyCode::Home => selected = 0,
                    KeyCode::End => selected = last,
                    KeyCode::Char('K') => {
                        detail_scroll = detail_scroll.saturating_sub(3);
                        moved = false;
                    }
                    KeyCode::Char('J') => {
                        detail_scroll = detail_scroll.saturating_add(3);
                        moved = false;
                    }
                    KeyCode::Enter => {
                        let user_index = g
                            .timeline
                            .steps
                            .get(selected)
                            .filter(|s| s.kind == StepKind::User)
                            .and_then(|s| s.user_index);
                        if let Some(idx) = user_index
                            && g.open_backtrack()
                        {
                            let count = g.user_block_positions().len();
                            g.backtrack_index = idx.min(count.saturating_sub(1));
                            g.timeline_open = false;
                            return Ok(());
                        }
                        moved = false;
                    }
                    _ => moved = false,
                },
                _ => moved = false,
            }
            if moved {
                detail_scroll = 0;
            }
        }
    })();

    let _ = execute!(
        stdout(),
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(kind: StepKind, label: &str) -> Step {
        Step {
            kind,
            label: label.into(),
            detail: String::new(),
            at: chrono::Local::now(),
            user_index: None,
            ok: Some(true),
        }
    }

    #[test]
    fn rows_and_scrubber_mark_the_selection() {
        let steps = vec![
            step(StepKind::User, "fix it"),
            step(StepKind::Tool, "edit_file a.rs"),
            step(StepKind::Reply, "done"),
        ];
        let rows = step_rows(&steps, 1, 60);
        let text: Vec<String> = rows
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(text[1].starts_with("❯ ") && text[1].contains("⚙ edit_file a.rs ✓"));
        assert!(text[0].starts_with("  "));
        let bar: String = scrubber(&steps, 2, 9)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(bar.chars().filter(|c| *c == '◆').count(), 1);
        assert!(bar.ends_with('◆'));
    }
}
