//! `/diagram` (Ctrl+]): the Mermaid diagrams in the conversation, drawn full
//! screen — newest first; Tab / Shift+Tab between them, arrows to scroll,
//! `c` copies the source, Esc closes.

use crate::tui::state::{DisplayBlock, TuiSessionState};
use crate::tui::theme;
use crossterm::{
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind, poll, read},
    execute,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Padding, Paragraph},
};
use std::io::stdout;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Every Mermaid block in the replies so far, oldest first.
pub fn diagrams_in(state: &TuiSessionState) -> Vec<String> {
    state
        .blocks
        .iter()
        .filter_map(|b| match b {
            DisplayBlock::Assistant(t) => Some(crate::diagram::extract(t)),
            _ => None,
        })
        .flatten()
        .collect()
}

pub fn run_diagram_overlay(state: &Arc<Mutex<TuiSessionState>>) -> anyhow::Result<()> {
    let diagrams = state.lock().map(|g| diagrams_in(&g)).unwrap_or_default();
    if diagrams.is_empty() {
        if let Ok(mut g) = state.lock() {
            g.diagram_open = false;
            g.push_block(DisplayBlock::System(
                "No diagrams yet — ask for one (\"draw the request flow as a mermaid diagram\")."
                    .into(),
            ));
            g.touch_transcript();
        }
        return Ok(());
    }
    let rendered: Vec<(Vec<String>, Option<String>)> = diagrams
        .iter()
        .map(|src| match crate::diagram::render(src) {
            Ok(lines) => (lines, None),
            Err(why) => (src.lines().map(str::to_string).collect(), Some(why)),
        })
        .collect();

    execute!(
        stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut term = ratatui::Terminal::new(backend)?;
    term.clear()?;
    let mut idx = rendered.len() - 1;
    let (mut sx, mut sy) = (0usize, 0usize);
    let mut note = String::new();

    let res: anyhow::Result<()> = (|| {
        loop {
            let size = term.size()?;
            let area = Rect::new(0, 0, size.width, size.height);
            let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
            let (lines, why) = &rendered[idx];
            let view_h = rows[0].height.saturating_sub(2) as usize;
            let view_w = rows[0].width.saturating_sub(4) as usize;
            let max_w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
            sy = sy.min(lines.len().saturating_sub(view_h));
            sx = sx.min(max_w.saturating_sub(view_w));
            let mut body: Vec<Line> = Vec::new();
            if let Some(w) = why {
                body.push(Line::from(Span::styled(
                    w.clone(),
                    Style::default().fg(theme::warn()),
                )));
                body.push(Line::default());
            }
            body.extend(lines.iter().skip(sy).take(view_h).map(|l| {
                Line::from(Span::styled(
                    l.chars().skip(sx).collect::<String>(),
                    Style::default().fg(if why.is_some() {
                        theme::muted()
                    } else {
                        theme::text()
                    }),
                ))
            }));
            let title = format!(" diagram {} / {} ", idx + 1, rendered.len());
            let hint = if note.is_empty() {
                " Tab / Shift+Tab next · ↑↓←→ PgUp PgDn scroll · c copy source · Esc close"
                    .to_string()
            } else {
                format!(" {note}")
            };
            term.draw(|frame| {
                frame.render_widget(
                    Block::default().style(Style::default().bg(theme::bg())),
                    area,
                );
                frame.render_widget(
                    Paragraph::new(Text::from(body)).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(theme::border()))
                            .title(Span::styled(
                                title,
                                Style::default()
                                    .fg(theme::accent())
                                    .add_modifier(Modifier::BOLD),
                            ))
                            .padding(Padding::horizontal(1)),
                    ),
                    rows[0],
                );
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        hint,
                        Style::default().fg(theme::muted()),
                    )))
                    .style(Style::default().bg(theme::surface())),
                    rows[1],
                );
            })?;

            if !poll(Duration::from_millis(250))? {
                continue;
            }
            let ev = read()?;
            note.clear();
            match ev {
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollUp => sy = sy.saturating_sub(3),
                    MouseEventKind::ScrollDown => sy += 3,
                    _ => {}
                },
                Event::Key(key) if !matches!(key.kind, KeyEventKind::Release) => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char(']') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Tab | KeyCode::Char('n') => {
                        idx = (idx + 1) % rendered.len();
                        (sx, sy) = (0, 0);
                    }
                    KeyCode::BackTab | KeyCode::Char('p') => {
                        idx = (idx + rendered.len() - 1) % rendered.len();
                        (sx, sy) = (0, 0);
                    }
                    KeyCode::Up | KeyCode::Char('k') => sy = sy.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => sy += 1,
                    KeyCode::Left | KeyCode::Char('h') => sx = sx.saturating_sub(4),
                    KeyCode::Right | KeyCode::Char('l') => sx += 4,
                    KeyCode::PageUp => sy = sy.saturating_sub(view_h.max(1)),
                    KeyCode::PageDown => sy += view_h.max(1),
                    KeyCode::Home => (sx, sy) = (0, 0),
                    KeyCode::Char('c') => {
                        note = match crate::tui::clipboard::copy_to_clipboard(&diagrams[idx]) {
                            Ok(_) => "copied the Mermaid source".into(),
                            Err(e) => format!("couldn't copy: {e}"),
                        };
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(())
    })();

    if let Ok(mut g) = state.lock() {
        g.diagram_open = false;
    }
    let _ = execute!(
        stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    );
    res
}
