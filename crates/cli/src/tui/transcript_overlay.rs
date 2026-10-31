//! Full-screen transcript overlay (`/transcript`): alt-screen, scrollable,
//! reflowing view of every block with a raw copy mode.

use crate::tui::state::TuiSessionState;
use crate::tui::theme;
use crossterm::{
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind, poll, read},
    execute,
};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Padding, Paragraph},
};
use std::io::stdout;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Full-screen transcript overlay (Codex `/transcript`). Renders ALL blocks at
/// the current terminal width — so it expands/collapses and reflows freely,
/// unlike the frozen native scrollback. Scrollable; supports a raw copy mode.
/// Enters the alternate screen, runs its own input loop, then restores.
pub(crate) fn run_transcript_overlay(state: &Arc<Mutex<TuiSessionState>>) -> anyhow::Result<()> {
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut term = ratatui::Terminal::new(backend)?;
    // The alternate screen may still hold what was drawn before (the
    // dashboard); start from a blank one so nothing shows through.
    term.clear()?;

    let res: anyhow::Result<()> = (|| {
        loop {
            let size = term.size()?;
            let width = size.width.saturating_sub(2).max(20);
            let view_h = size.height.saturating_sub(2).max(1) as usize;

            // Render all blocks at the current width (reflows on resize).
            let (mut lines, raw, msg_count) = {
                let g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                if !g.transcript_overlay_open {
                    return Ok(());
                }
                let n = g.blocks.len();
                let mut ls = crate::tui::transcript::render_blocks_range(&g, 0, n, width);
                if g.transcript_overlay_raw {
                    // Flatten to plain text for copy-friendly selection.
                    ls = ls
                        .into_iter()
                        .map(|l| {
                            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                            Line::from(text)
                        })
                        .collect();
                }
                (ls, g.transcript_overlay_raw, n)
            };

            let total = lines.len();
            let max_scroll = total.saturating_sub(view_h);
            let scroll = {
                let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                g.transcript_overlay_scroll = g.transcript_overlay_scroll.min(max_scroll);
                g.transcript_overlay_scroll
            };
            let end = (scroll + view_h).min(total);
            let visible: Vec<Line> = if scroll < end {
                lines.drain(scroll..end).collect()
            } else {
                Vec::new()
            };

            term.draw(|frame| {
                let area = frame.area();
                if !raw {
                    frame.render_widget(
                        Block::default().style(Style::default().bg(theme::bg())),
                        area,
                    );
                }
                let rows = Layout::vertical([
                    Constraint::Length(1),
                    Constraint::Min(1),
                    Constraint::Length(1),
                ])
                .split(area);

                // Header
                let header = Line::from(vec![
                    Span::styled(
                        " transcript ",
                        Style::default()
                            .fg(theme::on_accent())
                            .bg(theme::accent())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {msg_count} blocks"),
                        Style::default().fg(theme::muted()),
                    ),
                ]);
                frame.render_widget(
                    Paragraph::new(header).style(Style::default().bg(theme::surface())),
                    rows[0],
                );

                frame.render_widget(
                    Paragraph::new(Text::from(visible))
                        .block(Block::default().padding(Padding::horizontal(1))),
                    rows[1],
                );

                // Footer with keybinds + scroll position.
                let pct = (scroll * 100).checked_div(max_scroll).unwrap_or(100).min(100);
                let footer = Line::from(vec![
                    Span::styled(
                        " ↑↓/PgUp/PgDn scroll · t thinking · o tools · r raw · g/G top/bottom · q close ",
                        Style::default().fg(theme::muted()),
                    ),
                    Span::styled(
                        format!("  {pct}%"),
                        Style::default().fg(theme::muted()),
                    ),
                ]);
                frame.render_widget(
                    Paragraph::new(footer).style(Style::default().bg(theme::surface())),
                    rows[2],
                );
            })?;

            if !poll(Duration::from_millis(200))? {
                continue;
            }
            let ev = read()?;
            if let Event::Mouse(m) = &ev {
                let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                match m.kind {
                    MouseEventKind::ScrollUp => {
                        g.transcript_overlay_scroll = g.transcript_overlay_scroll.saturating_sub(3);
                    }
                    MouseEventKind::ScrollDown => {
                        g.transcript_overlay_scroll =
                            (g.transcript_overlay_scroll + 3).min(max_scroll);
                    }
                    _ => {}
                }
                continue;
            }
            if let Event::Key(key) = ev
                && !matches!(key.kind, KeyEventKind::Release)
            {
                let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _)
                    | (KeyCode::Char('q'), _)
                    | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        g.transcript_overlay_open = false;
                        return Ok(());
                    }
                    (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                        g.transcript_overlay_scroll = g.transcript_overlay_scroll.saturating_sub(1);
                    }
                    (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                        g.transcript_overlay_scroll =
                            (g.transcript_overlay_scroll + 1).min(max_scroll);
                    }
                    (KeyCode::PageUp, _) => {
                        g.transcript_overlay_scroll =
                            g.transcript_overlay_scroll.saturating_sub(view_h);
                    }
                    (KeyCode::PageDown, _) | (KeyCode::Char(' '), _) => {
                        g.transcript_overlay_scroll =
                            (g.transcript_overlay_scroll + view_h).min(max_scroll);
                    }
                    (KeyCode::Home, _) | (KeyCode::Char('g'), KeyModifiers::NONE) => {
                        g.transcript_overlay_scroll = 0;
                    }
                    (KeyCode::End, _) | (KeyCode::Char('G'), _) => {
                        g.transcript_overlay_scroll = max_scroll;
                    }
                    (KeyCode::Char('t'), KeyModifiers::NONE) => {
                        g.thinking_expanded = !g.thinking_expanded;
                    }
                    (KeyCode::Char('o'), KeyModifiers::NONE) => {
                        g.toggle_all_tool_blocks();
                    }
                    (KeyCode::Char('r'), KeyModifiers::NONE) => {
                        g.transcript_overlay_raw = !g.transcript_overlay_raw;
                    }
                    _ => {}
                }
            }
        }
    })();

    let _ = execute!(stdout(), LeaveAlternateScreen);
    res
}
