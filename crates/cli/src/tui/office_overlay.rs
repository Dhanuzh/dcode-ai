//! `/office`: the Agent Office full screen. The room with every agent on
//! top, the selected agent's recent activity below. ←/→ (or Tab) picks an
//! agent; Enter on an agent waiting for you goes back to answer it.

use crate::tui::office::{Activity, OfficeView, PersonRole, Who, office_layout};
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

/// Title, status line and recent activity of the `i`-th one in the room.
fn details(g: &TuiSessionState, i: usize) -> (String, String, Vec<String>) {
    let stations = g.office.stations();
    let Some(who) = stations.get(i) else {
        return ("office".into(), String::new(), Vec::new());
    };
    match who {
        Who::Person(p) => {
            let role = match p.role {
                PersonRole::Owner => "you · running this session",
                PersonRole::Joined => "on dcode-ai join · can send messages",
                PersonRole::Watching => "on the /watch page · watching",
            };
            let said = if p.said.is_empty() {
                "hasn't said anything yet".to_string()
            } else {
                format!("last said: {}", p.said)
            };
            (p.name.clone(), role.to_string(), vec![said])
        }
        Who::Agent(a) => {
            let main = a.id == "main";
            let title = if main {
                format!("{} · {}", a.name, g.model)
            } else {
                format!("sub-agent · {}", a.name)
            };
            let status = format!(
                "{} · {} · for {}s",
                activity_label(a.activity),
                a.bubble,
                a.since.elapsed().as_secs()
            );
            let log = if main {
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
                    .find(|r| r.id == a.id)
                    .map(|r| r.log.iter().rev().take(12).rev().cloned().collect())
                    .unwrap_or_default()
            };
            (title, status, log)
        }
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
                let n = g.office.stations().len();
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
                let (title, status, log) = details(&g, sel);
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
                    let mut lines = vec![Line::from(Span::styled(
                        status.clone(),
                        Style::default()
                            .fg(theme::accent())
                            .add_modifier(Modifier::BOLD),
                    ))];
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
            let n = g.office.stations().len().max(1);
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
                        let waiting = matches!(
                            g.office.stations().get(g.office_selected),
                            Some(Who::Agent(a))
                                if matches!(a.activity, Activity::WaitingApproval | Activity::Asking)
                        );
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
