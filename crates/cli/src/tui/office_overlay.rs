//! `/office`: the Agent Office full screen. The room with every agent on
//! top, the selected agent's recent activity below. ←/→ (or Tab) picks an
//! agent; Enter on an agent waiting for you goes back to answer it.

use crate::tui::office::{Activity, OfficeView, office_layout};
use crate::tui::state::TuiSessionState;
use crate::tui::theme;
use crossterm::{
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind, poll, read},
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

fn activity_label(a: Activity) -> &'static str {
    match a {
        Activity::Idle => "idle",
        Activity::Thinking => "thinking",
        Activity::Typing => "writing",
        Activity::Reading => "reading",
        Activity::Shell => "running a command",
        Activity::Browsing => "using the browser",
        Activity::WaitingApproval => "waiting for your approval",
        Activity::Asking => "asking you something",
        Activity::Tidying => "compacting",
        Activity::Delegating => "handing work to a sub-agent",
        Activity::Done => "done",
        Activity::Failed => "stopped",
    }
}

/// Recent activity of agent `i`: the main agent's last steps, or a
/// sub-agent's log.
fn agent_log(g: &TuiSessionState, i: usize) -> Vec<String> {
    let Some(agent) = g.office.agents.get(i) else {
        return Vec::new();
    };
    if i == 0 {
        g.timeline
            .steps
            .iter()
            .rev()
            .take(12)
            .map(|s| format!("{} {}", s.kind.icon(), s.label))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    } else {
        g.subagents
            .iter()
            .find(|r| r.id == agent.id)
            .map(|r| r.log.iter().rev().take(12).rev().cloned().collect())
            .unwrap_or_default()
    }
}

/// Run the office until closed.
pub(crate) fn run_office_overlay(state: &Arc<Mutex<TuiSessionState>>) -> anyhow::Result<()> {
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    execute!(
        stdout(),
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut term = ratatui::Terminal::new(backend)?;
    // The alternate screen may still hold what was drawn before (the
    // dashboard); start from a blank one so nothing shows through.
    term.clear()?;
    let mut room_rect = Rect::default();
    let mut room_scale: u16 = 1;

    let res: anyhow::Result<()> = (|| {
        loop {
            let size = term.size()?;
            let area = Rect::new(0, 0, size.width, size.height);
            {
                let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                if !g.office_open {
                    return Ok(());
                }
                g.office.tidy();
                let n = g.office.agents.len();
                g.office_selected = g.office_selected.min(n.saturating_sub(1));
                let (scale, room_h) = office_layout(area.width, area.height.saturating_sub(5), n);
                room_scale = scale;
                let rows = Layout::vertical([
                    Constraint::Length(room_h),
                    Constraint::Min(3),
                    Constraint::Length(1),
                ])
                .split(area);
                room_rect = rows[0];
                let sel = g.office_selected;
                let agent = g.office.agents.get(sel).cloned();
                let log = agent_log(&g, sel);
                let model = g.model.clone();
                let tick = crate::tui::app::office_tick();
                term.draw(|frame| {
                    frame.render_widget(
                        Block::default().style(Style::default().bg(theme::bg())),
                        area,
                    );
                    frame.render_widget(
                        OfficeView {
                            office: &g.office,
                            tick,
                            selected: Some(sel),
                        },
                        rows[0],
                    );
                    let (title, mut lines) = match &agent {
                        Some(a) => {
                            let who = if sel == 0 {
                                format!("{} · {model}", a.name)
                            } else {
                                format!("sub-agent · {}", a.name)
                            };
                            (
                                who,
                                vec![Line::from(vec![
                                    Span::styled(
                                        activity_label(a.activity),
                                        Style::default()
                                            .fg(theme::accent())
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(
                                        format!(
                                            " · {} · for {}s",
                                            a.bubble,
                                            a.since.elapsed().as_secs()
                                        ),
                                        Style::default().fg(theme::muted()),
                                    ),
                                ])],
                            )
                        }
                        None => ("office".into(), Vec::new()),
                    };
                    lines.push(Line::default());
                    if log.is_empty() {
                        lines.push(Line::from(Span::styled(
                            "nothing yet",
                            Style::default().fg(theme::muted()),
                        )));
                    }
                    for l in log {
                        lines.push(Line::from(Span::styled(
                            l,
                            Style::default().fg(theme::text()),
                        )));
                    }
                    frame.render_widget(
                        Paragraph::new(Text::from(lines)).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Rounded)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(
                                    format!(" {title} "),
                                    Style::default()
                                        .fg(theme::accent())
                                        .add_modifier(Modifier::BOLD),
                                ))
                                .padding(Padding::horizontal(1)),
                        ),
                        rows[1],
                    );
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled(
                            " ←/→ or Tab pick an agent · click one · Enter on one waiting for you: answer it · q close",
                            Style::default().fg(theme::muted()),
                        )))
                        .style(Style::default().bg(theme::surface())),
                        rows[2],
                    );
                })?;
            }

            // Redraw ~8 times a second for the animation.
            if !poll(Duration::from_millis(120))? {
                continue;
            }
            let ev = read()?;
            let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            let n = g.office.agents.len().max(1);
            match ev {
                Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                    // Which desk was clicked.
                    let sc = room_scale.max(1) as usize;
                    let lw = room_rect.width / room_scale.max(1);
                    let per_row = crate::tui::office::desks_per_row(lw);
                    let used = per_row.min(n) * crate::tui::office::STATION_W;
                    let left = (lw as usize).saturating_sub(used) / 2;
                    let x = (m.column as usize / sc).saturating_sub(left);
                    let y = ((m.row.saturating_sub(room_rect.y) as usize) / sc).saturating_sub(3);
                    let i = (y / crate::tui::office::STATION_ROWS as usize) * per_row
                        + x / crate::tui::office::STATION_W;
                    if i < n {
                        g.office_selected = i;
                    }
                }
                Event::Key(key) if !matches!(key.kind, KeyEventKind::Release) => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        g.office_open = false;
                        return Ok(());
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        g.office_open = false;
                        return Ok(());
                    }
                    KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
                        g.office_selected = (g.office_selected + 1) % n
                    }
                    KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => {
                        g.office_selected = (g.office_selected + n - 1) % n
                    }
                    KeyCode::Enter => {
                        let waiting = g.office.agents.get(g.office_selected).is_some_and(|a| {
                            matches!(a.activity, Activity::WaitingApproval | Activity::Asking)
                        });
                        if waiting {
                            g.office_open = false;
                            return Ok(());
                        }
                    }
                    _ => {}
                },
                _ => {}
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
