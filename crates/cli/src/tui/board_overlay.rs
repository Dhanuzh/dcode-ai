//! `/board`: the project's task board full screen, as a kanban:
//! Backlog · To do · In progress · Review · Done. Cards move with
//! Shift+←/→ (or `<` `>`); `n` adds, `e` renames, `p` priority, `g` review
//! gate, `b` blockers, `x` twice deletes, Enter shows a card in full, `r` asks
//! the agent to run the board (the head agent), `q` closes.
//!
//! The board lives in `.dcode-ai/board/board.json`; it's re-read twice a second,
//! so the agent's moves show up live.

use crate::tui::app::TuiCmd;
use crate::tui::state::TuiSessionState;
use crate::tui::theme;
use crossterm::{
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind, poll, read},
    execute,
};
use dcode_ai_common::board::{Board, Card, NewCard, Priority, Status};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap},
};
use std::io::stdout;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

/// What the bottom line is typing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Typing {
    NewCard,
    Rename(String),
    Blockers(String),
}

/// The board view's state while it's open.
#[derive(Debug, Default)]
pub struct BoardUi {
    col: usize,
    row: usize,
    typing: Option<(Typing, String)>,
    /// `x` pressed once on this card: `x` again deletes.
    confirm_delete: Option<String>,
    detail: bool,
    message: Option<String>,
}

fn priority_mark(p: Priority) -> (&'static str, Color) {
    match p {
        Priority::Urgent => ("⏫", theme::error()),
        Priority::High => ("▲", theme::warn()),
        Priority::Medium => ("■", theme::muted()),
        Priority::Low => ("▽", theme::muted()),
    }
}

fn status_color(s: Status) -> Color {
    match s {
        Status::Backlog => theme::muted(),
        Status::Todo => theme::text(),
        Status::InProgress => theme::accent(),
        Status::Review => theme::warn(),
        Status::Done => theme::success(),
    }
}

fn truncate(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(w.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// A card's lines inside a column `w` wide.
fn card_lines(board: &Board, c: &Card, w: usize, selected: bool) -> Vec<Line<'static>> {
    let blockers = board.open_blockers(c);
    let blocked = !blockers.is_empty() && c.status != Status::Done;
    let (mark, mark_color) = priority_mark(c.priority);
    let base = if blocked {
        Style::default().fg(theme::muted())
    } else {
        Style::default().fg(theme::text())
    };
    let sel = if selected {
        Style::default()
            .bg(theme::surface())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let bar = Span::styled(
        if selected { "▌" } else { " " },
        Style::default().fg(theme::accent()),
    );
    let mut lines = Vec::new();
    let mut head = vec![
        bar.clone(),
        Span::styled(format!("{mark} "), Style::default().fg(mark_color)),
        Span::styled(
            c.key.clone(),
            Style::default().fg(theme::accent()).patch(sel),
        ),
    ];
    if c.gate {
        head.push(Span::styled(" ◆", Style::default().fg(theme::warn())));
    }
    if let Some(a) = &c.assignee {
        head.push(Span::styled(
            format!(" @{}", truncate(a, 10)),
            Style::default().fg(theme::muted()),
        ));
    }
    lines.push(Line::from(head));
    // The title, wrapped to two lines.
    let room = w.saturating_sub(2).max(4);
    let words: Vec<&str> = c.title.split_whitespace().collect();
    let mut rows: Vec<String> = vec![String::new()];
    for word in words {
        let n = rows.len();
        let cur = &mut rows[n - 1];
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + word.chars().count() <= room || n >= 2 {
            cur.push(' ');
            cur.push_str(word);
        } else {
            rows.push(word.to_string());
        }
    }
    for r in rows {
        lines.push(Line::from(vec![
            bar.clone(),
            Span::styled(format!(" {}", truncate(&r, room)), base.patch(sel)),
        ]));
    }
    let mut tags: Vec<Span<'static>> = vec![bar];
    if blocked {
        tags.push(Span::styled(
            format!(
                " ⛓ {}",
                truncate(&blockers.join(","), room.saturating_sub(3))
            ),
            Style::default().fg(theme::error()),
        ));
    }
    if let Some(m) = c
        .milestone
        .as_deref()
        .and_then(|m| board.milestone_title(m))
    {
        tags.push(Span::styled(
            format!(" ⚑ {}", truncate(m, 14)),
            Style::default().fg(theme::muted()),
        ));
    }
    if let Some(b) = &c.branch {
        tags.push(Span::styled(
            format!(" ⎇ {}", truncate(b, 14)),
            Style::default().fg(theme::muted()),
        ));
    }
    if tags.len() > 1 {
        lines.push(Line::from(tags));
    }
    lines
}

fn workspace(g: &TuiSessionState) -> PathBuf {
    g.workspace_root.clone()
}

/// Run the board until closed. `r` sends the head-agent prompt through
/// `cmd_tx`.
pub(crate) fn run_board_overlay(
    state: &Arc<Mutex<TuiSessionState>>,
    cmd_tx: &UnboundedSender<TuiCmd>,
) -> anyhow::Result<()> {
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    execute!(
        stdout(),
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut term = ratatui::Terminal::new(backend)?;
    term.clear()?;
    let ws = state
        .lock()
        .map(|g| workspace(&g))
        .unwrap_or_else(|_| PathBuf::from("."));
    let mut board = Board::load(&ws);
    let mut loaded = Instant::now();
    // Start on To do: where new cards go and work is picked from.
    let mut ui = BoardUi {
        col: Status::Todo.index(),
        ..Default::default()
    };
    let mut col_rects: Vec<Rect> = Vec::new();
    // Per column: (first row, height, card index) of each card drawn, for clicks.
    let mut card_rows: Vec<Vec<(u16, u16, usize)>> = Vec::new();

    let res: anyhow::Result<()> = (|| {
        loop {
            if loaded.elapsed() > Duration::from_millis(500) && ui.typing.is_none() {
                board = Board::load(&ws);
                loaded = Instant::now();
            }
            if !state.lock().map(|g| g.board_open).unwrap_or(false) {
                return Ok(());
            }
            let columns: Vec<Vec<Card>> = Status::ALL
                .iter()
                .map(|s| board.column(*s).into_iter().cloned().collect())
                .collect();
            ui.col = ui.col.min(Status::ALL.len() - 1);
            ui.row = ui.row.min(columns[ui.col].len().saturating_sub(1));
            let selected: Option<Card> = columns[ui.col].get(ui.row).cloned();

            let size = term.size()?;
            let area = Rect::new(0, 0, size.width, size.height);
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(4),
                Constraint::Length(1),
            ])
            .split(area);
            // Narrow terminals show three columns around the selected one.
            let visible: Vec<usize> = if area.width >= 90 {
                (0..Status::ALL.len()).collect()
            } else {
                let start = ui.col.saturating_sub(1).min(Status::ALL.len() - 3);
                (start..start + 3).collect()
            };
            let cols = Layout::horizontal(
                visible
                    .iter()
                    .map(|_| Constraint::Ratio(1, visible.len() as u32))
                    .collect::<Vec<_>>(),
            )
            .split(rows[1]);
            col_rects = vec![Rect::default(); Status::ALL.len()];
            card_rows = vec![Vec::new(); Status::ALL.len()];

            let total = board.cards.len();
            let done = board.column(Status::Done).len();
            let ready = board.ready().len();
            let title = format!(
                " {} board · {total} cards · {done} done · {ready} ready ",
                board.prefix
            );
            let footer = match (&ui.typing, &ui.message, &ui.confirm_delete) {
                (Some((Typing::NewCard, buf)), _, _) => format!(" New card title: {buf}▏  (Enter adds · Esc cancels)"),
                (Some((Typing::Rename(k), buf)), _, _) => format!(" Rename {k}: {buf}▏"),
                (Some((Typing::Blockers(k), buf)), _, _) => {
                    format!(" {k} is blocked by (keys or numbers, comma separated): {buf}▏")
                }
                (None, _, Some(k)) => format!(" Delete {k}? press x again (any other key keeps it)"),
                (None, Some(m), None) => format!(" {m}"),
                _ => " ←→↑↓ move · Shift+←/→ or < > move card · n new · e rename · p priority · g gate · b blockers · x delete · Enter details · r run the board · q close".into(),
            };

            term.draw(|frame| {
                frame.render_widget(
                    Block::default().style(Style::default().bg(theme::bg())),
                    area,
                );
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        title.clone(),
                        Style::default()
                            .fg(theme::accent())
                            .add_modifier(Modifier::BOLD),
                    )))
                    .style(Style::default().bg(theme::surface())),
                    rows[0],
                );
                for (slot, &ci) in visible.iter().enumerate() {
                    let rect = cols[slot];
                    col_rects[ci] = rect;
                    let status = Status::ALL[ci];
                    let cards = &columns[ci];
                    let focused = ci == ui.col;
                    let inner_w = rect.width.saturating_sub(4) as usize;
                    let mut lines: Vec<Line> = Vec::new();
                    let mut starts = Vec::new();
                    // Keep the selected card in view.
                    let mut card_blocks: Vec<Vec<Line>> = cards
                        .iter()
                        .enumerate()
                        .map(|(i, c)| card_lines(&board, c, inner_w, focused && i == ui.row))
                        .collect();
                    let view_h = rect.height.saturating_sub(2) as usize;
                    let mut skip = 0usize;
                    if focused {
                        let mut h = 0;
                        for (i, b) in card_blocks.iter().enumerate() {
                            if i == ui.row {
                                let end = h + b.len() + 1;
                                if end > view_h {
                                    skip = i.min(card_blocks.len());
                                }
                                break;
                            }
                            h += b.len() + 1;
                        }
                    }
                    if skip > 0 {
                        lines.push(Line::from(Span::styled(
                            format!(" ↑ {skip} more"),
                            Style::default().fg(theme::muted()),
                        )));
                    }
                    for (i, b) in card_blocks.drain(..).enumerate().skip(skip) {
                        starts.push((rect.y + 1 + lines.len() as u16, b.len() as u16, i));
                        lines.extend(b);
                        lines.push(Line::default());
                    }
                    if cards.is_empty() {
                        lines.push(Line::from(Span::styled(
                            if ci == Status::Todo.index() && board.cards.is_empty() {
                                " empty — n adds a card, or /board plan <goal>"
                            } else {
                                " —"
                            },
                            Style::default().fg(theme::muted()),
                        )));
                    }
                    card_rows[ci] = starts;
                    let border = if focused {
                        theme::accent()
                    } else {
                        theme::border()
                    };
                    frame.render_widget(
                        Paragraph::new(Text::from(lines)).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Rounded)
                                .border_style(Style::default().fg(border))
                                .title(Span::styled(
                                    format!(" {} {} ", status.label(), cards.len()),
                                    Style::default()
                                        .fg(status_color(status))
                                        .add_modifier(Modifier::BOLD),
                                )),
                        ),
                        rect,
                    );
                }
                if ui.detail
                    && let Some(c) = &selected
                {
                    let w = (area.width * 3 / 5).max(40).min(area.width);
                    let h = (area.height * 3 / 4).max(10).min(rows[1].height);
                    let r = Rect::new(
                        area.width.saturating_sub(w) / 2,
                        rows[1].y + rows[1].height.saturating_sub(h) / 2,
                        w,
                        h,
                    );
                    frame.render_widget(Clear, r);
                    frame.render_widget(
                        Paragraph::new(board.render_card(c))
                            .wrap(Wrap { trim: false })
                            .style(Style::default().fg(theme::text()).bg(theme::bg()))
                            .block(
                                Block::default()
                                    .borders(Borders::ALL)
                                    .border_type(BorderType::Rounded)
                                    .border_style(Style::default().fg(theme::accent()))
                                    .title(Span::styled(
                                        format!(" {} · Esc closes ", c.key),
                                        Style::default().fg(theme::accent()),
                                    ))
                                    .padding(Padding::horizontal(1)),
                            ),
                        r,
                    );
                }
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        footer.clone(),
                        Style::default().fg(
                            if ui.typing.is_some() || ui.confirm_delete.is_some() {
                                theme::text()
                            } else {
                                theme::muted()
                            },
                        ),
                    )))
                    .style(Style::default().bg(theme::surface())),
                    rows[2],
                );
            })?;

            if !poll(Duration::from_millis(250))? {
                continue;
            }
            let ev = read()?;
            match ev {
                Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                    for (ci, r) in col_rects.iter().enumerate() {
                        if r.width > 0
                            && m.column >= r.x
                            && m.column < r.x + r.width
                            && m.row >= r.y
                            && m.row < r.y + r.height
                        {
                            ui.col = ci;
                            if let Some((_, _, i)) = card_rows[ci]
                                .iter()
                                .find(|(y, h, _)| m.row >= *y && m.row < y + h)
                            {
                                ui.row = *i;
                            }
                        }
                    }
                }
                Event::Mouse(m) if m.kind == MouseEventKind::ScrollDown => {
                    ui.row = (ui.row + 1).min(columns[ui.col].len().saturating_sub(1));
                }
                Event::Mouse(m) if m.kind == MouseEventKind::ScrollUp => {
                    ui.row = ui.row.saturating_sub(1);
                }
                Event::Key(key) if !matches!(key.kind, KeyEventKind::Release) => {
                    ui.message = None;
                    // Typing on the bottom line.
                    if let Some((what, mut buf)) = ui.typing.take() {
                        match key.code {
                            KeyCode::Esc => {}
                            KeyCode::Enter => {
                                let text = buf.trim().to_string();
                                let result = match &what {
                                    Typing::NewCard if !text.is_empty() => {
                                        let status = Status::ALL[ui.col];
                                        Board::update(&ws, |b| {
                                            b.add(
                                                NewCard {
                                                    title: text.clone(),
                                                    status: Some(if status == Status::Done {
                                                        Status::Todo
                                                    } else {
                                                        status
                                                    }),
                                                    ..Default::default()
                                                },
                                                "you",
                                            )
                                        })
                                        .map(|k| format!("added {k}"))
                                    }
                                    Typing::Rename(k) if !text.is_empty() => {
                                        Board::update(&ws, |b| {
                                            if let Some(c) = b.card_mut(k) {
                                                c.title = text.chars().take(200).collect();
                                                c.updated = chrono::Utc::now();
                                            }
                                        })
                                        .map(|_| format!("renamed {k}"))
                                    }
                                    Typing::Blockers(k) => Board::update(&ws, |b| {
                                        let keys: Vec<String> = text
                                            .split([',', ' '])
                                            .filter(|x| !x.trim().is_empty())
                                            .filter_map(|x| b.resolve_key(x))
                                            .filter(|x| x != k)
                                            .collect();
                                        if let Some(c) = b.card_mut(k) {
                                            c.blocked_by = keys.clone();
                                            c.updated = chrono::Utc::now();
                                        }
                                        keys
                                    })
                                    .map(|keys| {
                                        if keys.is_empty() {
                                            format!("{k} isn't blocked")
                                        } else {
                                            format!("{k} is blocked by {}", keys.join(", "))
                                        }
                                    }),
                                    _ => Ok(String::new()),
                                };
                                ui.message = Some(match result {
                                    Ok(m) if m.is_empty() => "nothing changed".into(),
                                    Ok(m) => m,
                                    Err(e) => format!("could not save the board: {e}"),
                                });
                                board = Board::load(&ws);
                            }
                            KeyCode::Backspace => {
                                buf.pop();
                                ui.typing = Some((what, buf));
                            }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if buf.chars().count() < 200 {
                                    buf.push(c);
                                }
                                ui.typing = Some((what, buf));
                            }
                            _ => ui.typing = Some((what, buf)),
                        }
                        continue;
                    }
                    if let Some(k) = ui.confirm_delete.take() {
                        if key.code == KeyCode::Char('x') {
                            let _ = Board::update(&ws, |b| b.remove(&k));
                            board = Board::load(&ws);
                            ui.message = Some(format!("deleted {k}"));
                        }
                        continue;
                    }
                    if ui.detail {
                        if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                            ui.detail = false;
                        }
                        continue;
                    }
                    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                    let move_card = |step: i32, board: &mut Board, ui: &mut BoardUi| {
                        let Some(c) = &selected else { return };
                        let to = c.status.step(step);
                        if to == c.status {
                            return;
                        }
                        let key = c.key.clone();
                        let _ = Board::update(&ws, |b| {
                            if let Some(c) = b.card_mut(&key) {
                                c.set_status(to, "you");
                            }
                        });
                        *board = Board::load(&ws);
                        ui.col = to.index();
                        ui.row = board
                            .column(to)
                            .iter()
                            .position(|c| c.key == key)
                            .unwrap_or(0);
                        ui.message = Some(format!("{key} → {}", to.label()));
                    };
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => {
                            if let Ok(mut g) = state.lock() {
                                g.board_open = false;
                            }
                            return Ok(());
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if let Ok(mut g) = state.lock() {
                                g.board_open = false;
                            }
                            return Ok(());
                        }
                        KeyCode::Right if shift => move_card(1, &mut board, &mut ui),
                        KeyCode::Left if shift => move_card(-1, &mut board, &mut ui),
                        KeyCode::Char('>') | KeyCode::Char('.') => {
                            move_card(1, &mut board, &mut ui)
                        }
                        KeyCode::Char('<') | KeyCode::Char(',') => {
                            move_card(-1, &mut board, &mut ui)
                        }
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                            ui.col = (ui.col + 1).min(Status::ALL.len() - 1);
                            ui.row = 0;
                        }
                        KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                            ui.col = ui.col.saturating_sub(1);
                            ui.row = 0;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            ui.row = (ui.row + 1).min(columns[ui.col].len().saturating_sub(1))
                        }
                        KeyCode::Up | KeyCode::Char('k') => ui.row = ui.row.saturating_sub(1),
                        KeyCode::Char('n') | KeyCode::Char('a') => {
                            ui.typing = Some((Typing::NewCard, String::new()))
                        }
                        KeyCode::Char('e') => {
                            if let Some(c) = &selected {
                                ui.typing = Some((Typing::Rename(c.key.clone()), c.title.clone()));
                            }
                        }
                        KeyCode::Char('b') => {
                            if let Some(c) = &selected {
                                ui.typing = Some((
                                    Typing::Blockers(c.key.clone()),
                                    c.blocked_by.join(", "),
                                ));
                            }
                        }
                        KeyCode::Char('p') => {
                            if let Some(c) = &selected {
                                let key = c.key.clone();
                                let _ = Board::update(&ws, |b| {
                                    if let Some(c) = b.card_mut(&key) {
                                        c.priority = c.priority.next();
                                        c.updated = chrono::Utc::now();
                                    }
                                });
                                board = Board::load(&ws);
                            }
                        }
                        KeyCode::Char('g') => {
                            if let Some(c) = &selected {
                                let key = c.key.clone();
                                let _ = Board::update(&ws, |b| {
                                    if let Some(c) = b.card_mut(&key) {
                                        c.gate = !c.gate;
                                        c.updated = chrono::Utc::now();
                                    }
                                });
                                board = Board::load(&ws);
                                ui.message = Some(match board.card(&key).map(|c| c.gate) {
                                    Some(true) => format!(
                                        "{key} has a review gate: it waits in Review for you"
                                    ),
                                    _ => format!("{key}: no review gate"),
                                });
                            }
                        }
                        KeyCode::Char('x') | KeyCode::Delete => {
                            if let Some(c) = &selected {
                                ui.confirm_delete = Some(c.key.clone());
                            }
                        }
                        KeyCode::Enter => ui.detail = selected.is_some(),
                        KeyCode::Char('r') => {
                            let _ = cmd_tx.send(TuiCmd::Submit("/board run".into()));
                            if let Ok(mut g) = state.lock() {
                                g.board_open = false;
                            }
                            return Ok(());
                        }
                        _ => {}
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_shows_its_key_blockers_and_gate() {
        let mut b = Board::new("DCO");
        b.add(
            NewCard {
                title: "API".into(),
                ..Default::default()
            },
            "",
        );
        b.add(
            NewCard {
                title: "Checkout page with Stripe and a long title that wraps".into(),
                blocked_by: vec!["DCO-1".into()],
                gate: true,
                milestone: Some("Launch".into()),
                ..Default::default()
            },
            "",
        );
        let c = b.card("DCO-2").unwrap().clone();
        let text: String = card_lines(&b, &c, 24, true)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(text.contains("DCO-2"), "{text}");
        assert!(text.contains("◆"));
        assert!(text.contains("⛓ DCO-1"));
        assert!(text.contains("⚑ Launch"));
        assert!(text.lines().count() <= 4, "{text}");
    }
}
