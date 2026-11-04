//! `/board`: the project's task board full screen, as a kanban:
//! Backlog · To do · In progress · Review · Done, each card in its own box.
//!
//! Two spaces, kept apart: **Personal** (this computer only) and **Jira**
//! (the linked project's tickets, synced through its MCP server). `t` or Tab
//! switches. Cards move with Shift+←/→ (or `<` `>`); `n` adds to the space
//! you're in, `e` renames, `p` priority, `g` review gate, `b` blockers, `x`
//! twice deletes, Enter shows a card in full, `o` opens its Jira ticket, `s`
//! syncs now, `r` asks the agent to run the board (the head agent), `q`
//! closes.
//!
//! The board lives in `.dcode-ai/board/board.json`; it's re-read twice a
//! second, so the agent's and Jira's changes show up live.

use crate::tui::app::TuiCmd;
use crate::tui::state::TuiSessionState;
use crate::tui::theme;
use crossterm::{
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind, poll, read},
    execute,
};
use dcode_ai_common::board::{Board, Card, NewCard, Priority, Push, Space, Status};
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
#[derive(Debug)]
pub struct BoardUi {
    space: Space,
    col: usize,
    row: usize,
    typing: Option<(Typing, String)>,
    /// `x` pressed once on this card: `x` again deletes.
    confirm_delete: Option<String>,
    detail: bool,
    message: Option<String>,
}

impl Default for BoardUi {
    fn default() -> Self {
        Self {
            space: Space::Personal,
            // Start on To do: where new cards go and work is picked from.
            col: Status::Todo.index(),
            row: 0,
            typing: None,
            confirm_delete: None,
            detail: false,
            message: None,
        }
    }
}

fn priority_chip(p: Priority) -> Span<'static> {
    let (text, color) = match p {
        Priority::Urgent => ("⏫ urgent", theme::error()),
        Priority::High => ("▲ high", theme::warn()),
        Priority::Medium => ("■ med", theme::muted()),
        Priority::Low => ("▽ low", theme::muted()),
    };
    Span::styled(text, Style::default().fg(color))
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

/// The title in at most two lines of `w`.
fn title_rows(title: &str, w: usize) -> Vec<String> {
    let w = w.max(4);
    let mut rows: Vec<String> = vec![String::new()];
    for word in title.split_whitespace() {
        let n = rows.len();
        let cur = &mut rows[n - 1];
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + word.chars().count() <= w || n >= 2 {
            cur.push(' ');
            cur.push_str(word);
        } else {
            rows.push(word.to_string());
        }
    }
    rows.into_iter().map(|r| truncate(&r, w)).collect()
}

/// A card's inside lines (its box adds the border), `w` wide.
fn card_lines(board: &Board, c: &Card, w: usize) -> Vec<Line<'static>> {
    let blockers = board.open_blockers(c);
    let blocked = !blockers.is_empty() && c.status != Status::Done;
    let key = c.shown_key().to_string();
    let chip = priority_chip(c.priority);
    let pad = w.saturating_sub(key.chars().count() + chip.content.chars().count());
    let mut lines = vec![Line::from(vec![
        Span::styled(
            key,
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(pad)),
        chip,
    ])];
    let title_style = if blocked {
        Style::default().fg(theme::muted())
    } else {
        Style::default().fg(theme::text())
    };
    for r in title_rows(&c.title, w) {
        lines.push(Line::from(Span::styled(r, title_style)));
    }
    // The details line: who, where it stands, what holds it.
    let mut meta: Vec<Span<'static>> = Vec::new();
    let add = |s: Span<'static>, meta: &mut Vec<Span<'static>>| {
        if !meta.is_empty() {
            meta.push(Span::styled(" · ", Style::default().fg(theme::border())));
        }
        meta.push(s);
    };
    match c.push {
        Some(Push::Create) => add(
            Span::styled("⟳ making ticket", Style::default().fg(theme::warn())),
            &mut meta,
        ),
        Some(Push::Status) => add(
            Span::styled("⟳ moving", Style::default().fg(theme::warn())),
            &mut meta,
        ),
        None => {}
    }
    if let Some(a) = &c.assignee {
        add(
            Span::styled(
                format!("@{}", truncate(a, 12)),
                Style::default().fg(theme::muted()),
            ),
            &mut meta,
        );
    }
    if let Some(r) = &c.remote
        && !r.status.is_empty()
        && !r.status.eq_ignore_ascii_case(c.status.label())
    {
        add(
            Span::styled(truncate(&r.status, 14), Style::default().fg(theme::muted())),
            &mut meta,
        );
    }
    if blocked {
        add(
            Span::styled(
                format!("⛓ {}", truncate(&blockers.join(","), 14)),
                Style::default().fg(theme::error()),
            ),
            &mut meta,
        );
    }
    if c.gate {
        add(
            Span::styled("◆ gate", Style::default().fg(theme::warn())),
            &mut meta,
        );
    }
    if let Some(m) = c
        .milestone
        .as_deref()
        .and_then(|m| board.milestone_title(m))
    {
        add(
            Span::styled(
                format!("⚑ {}", truncate(m, 12)),
                Style::default().fg(theme::muted()),
            ),
            &mut meta,
        );
    }
    if !meta.is_empty() {
        lines.push(Line::from(meta));
    }
    lines
}

/// The header's sync note.
fn sync_line(board: &Board, g: &TuiSessionState) -> String {
    let Some(link) = &board.jira else {
        return "Jira not linked · /board jira <PROJECT>".into();
    };
    let status = g.board_syncer.as_ref().map(|s| s.status());
    let when = link
        .last_sync
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string());
    match (status, when) {
        (Some(s), _) if s.starts_with("syncing") => "⇄ syncing…".into(),
        (Some(s), Some(w)) if s.contains('⚠') => format!("⇄ {w} · {s}"),
        (_, Some(w)) => format!("⇄ synced {w}"),
        (Some(s), None) => format!("⇄ {s}"),
        (None, None) => "⇄ not synced yet · s syncs".into(),
    }
}

/// `o`: the selected card's Jira ticket in the browser.
fn open_remote(selected: &Option<Card>, ui: &mut BoardUi) {
    ui.message = Some(match selected.as_ref().and_then(|c| c.remote.as_ref()) {
        Some(r) if !r.url.is_empty() => match crate::tui::app::open_link_in_system(&r.url) {
            Ok(()) => format!("opened {}", r.key),
            Err(e) => format!("couldn't open {}: {e}", r.url),
        },
        Some(r) => format!("{} has no link yet", r.key),
        None => "this card isn't a Jira ticket".into(),
    });
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
        .map(|g| g.workspace_root.clone())
        .unwrap_or_else(|_| PathBuf::from("."));
    let mut board = Board::load(&ws);
    let mut loaded = Instant::now();
    let mut ui = BoardUi::default();
    // A linked board with no personal cards opens on Jira.
    if board.jira.is_some() && board.in_space(Space::Personal).next().is_none() {
        ui.space = Space::Jira;
    }
    let kick = |state: &Arc<Mutex<TuiSessionState>>| {
        if let Ok(g) = state.lock()
            && let Some(s) = &g.board_syncer
        {
            s.kick();
        }
    };
    // Where each card was drawn: (rect, column, index), for clicks.
    let mut hits: Vec<(Rect, usize, usize)> = Vec::new();
    let mut col_rects: Vec<Rect> = Vec::new();

    let res: anyhow::Result<()> = (|| {
        loop {
            if loaded.elapsed() > Duration::from_millis(500) && ui.typing.is_none() {
                board = Board::load(&ws);
                loaded = Instant::now();
            }
            let (open, sync_text) = state
                .lock()
                .map(|g| (g.board_open, sync_line(&board, &g)))
                .unwrap_or((false, String::new()));
            if !open {
                return Ok(());
            }
            let space = ui.space;
            let columns: Vec<Vec<Card>> = Status::ALL
                .iter()
                .map(|s| {
                    board
                        .column(*s)
                        .into_iter()
                        .filter(|c| c.space == space)
                        .cloned()
                        .collect()
                })
                .collect();
            ui.col = ui.col.min(Status::ALL.len() - 1);
            ui.row = ui.row.min(columns[ui.col].len().saturating_sub(1));
            let selected: Option<Card> = columns[ui.col].get(ui.row).cloned();

            let size = term.size()?;
            let area = Rect::new(0, 0, size.width, size.height);
            let rows = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(4),
                Constraint::Length(1),
            ])
            .split(area);
            // Narrow terminals show three columns around the selected one.
            let visible: Vec<usize> = if area.width >= 100 {
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
            .spacing(1)
            .split(rows[1]);
            hits.clear();
            col_rects = vec![Rect::default(); Status::ALL.len()];

            // The header: the two spaces as tabs, counts, the sync note.
            let personal = board.in_space(Space::Personal).count();
            let jira_n = board.in_space(Space::Jira).count();
            let tab = |label: String, on: bool| -> Span<'static> {
                if on {
                    Span::styled(
                        format!(" {label} "),
                        Style::default()
                            .fg(theme::on_accent())
                            .bg(theme::accent())
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(format!(" {label} "), Style::default().fg(theme::muted()))
                }
            };
            let jira_label = match &board.jira {
                Some(l) => format!("Jira {} · {jira_n}", l.project),
                None => "Jira".into(),
            };
            let total: usize = columns.iter().map(Vec::len).sum();
            let ready = board.ready().iter().filter(|c| c.space == space).count();
            let done = columns[Status::Done.index()].len();
            let mut head = vec![
                Span::styled(
                    " ▦ Board  ",
                    Style::default()
                        .fg(theme::text())
                        .add_modifier(Modifier::BOLD),
                ),
                tab(format!("Personal · {personal}"), space == Space::Personal),
                Span::raw(" "),
                tab(jira_label, space == Space::Jira),
                Span::styled(
                    format!("   {total} cards · {ready} ready · {done} done"),
                    Style::default().fg(theme::muted()),
                ),
            ];
            let right = format!("{sync_text}  ");
            let used: usize = head.iter().map(|s| s.content.chars().count()).sum();
            let pad = (area.width as usize).saturating_sub(used + right.chars().count());
            head.push(Span::raw(" ".repeat(pad)));
            head.push(Span::styled(
                right,
                Style::default().fg(if sync_text.contains('⚠') {
                    theme::warn()
                } else {
                    theme::muted()
                }),
            ));

            let footer = match (&ui.typing, &ui.message, &ui.confirm_delete) {
                (Some((Typing::NewCard, buf)), _, _) => format!(
                    " New {} card in {}: {buf}▏   Enter adds · Esc cancels",
                    if space == Space::Jira {
                        "Jira"
                    } else {
                        "personal"
                    },
                    Status::ALL[ui.col].label()
                ),
                (Some((Typing::Rename(k), buf)), _, _) => format!(" Rename {k}: {buf}▏"),
                (Some((Typing::Blockers(k), buf)), _, _) => {
                    format!(" {k} is blocked by (keys or numbers, comma separated): {buf}▏")
                }
                (None, _, Some(k)) => {
                    format!(" Delete {k}? x again deletes, any other key keeps it")
                }
                (None, Some(m), None) => format!(" {m}"),
                _ => " t Personal/Jira · ←→↑↓ · ⇧←/→ move card · n new · e edit · p priority · b blockers · g gate · ⏎ details · o open in Jira · s sync · r run · q close".into(),
            };

            term.draw(|frame| {
                frame.render_widget(
                    Block::default().style(Style::default().bg(theme::bg())),
                    area,
                );
                frame.render_widget(
                    Paragraph::new(vec![Line::from(head.clone()), Line::default()]),
                    rows[0],
                );
                for (slot, &ci) in visible.iter().enumerate() {
                    let rect = cols[slot];
                    col_rects[ci] = rect;
                    let status = Status::ALL[ci];
                    let cards = &columns[ci];
                    let focused = ci == ui.col;
                    let col_block = Block::default()
                        .borders(Borders::TOP)
                        .border_type(BorderType::Thick)
                        .border_style(Style::default().fg(if focused {
                            status_color(status)
                        } else {
                            theme::border()
                        }))
                        .title(Line::from(vec![
                            Span::styled(
                                format!(" {} ", status.label()),
                                Style::default()
                                    .fg(status_color(status))
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!("{} ", cards.len()),
                                Style::default().fg(theme::muted()),
                            ),
                        ]));
                    let inner = col_block.inner(rect);
                    frame.render_widget(col_block, rect);
                    if cards.is_empty() {
                        let hint = if ci == Status::Todo.index() && total == 0 {
                            match space {
                                Space::Personal => " n adds a card · /board plan <goal>",
                                Space::Jira if board.jira.is_none() => {
                                    " /board jira <PROJECT> links Jira"
                                }
                                Space::Jira => " n makes a Jira ticket · s syncs",
                            }
                        } else {
                            ""
                        };
                        frame.render_widget(
                            Paragraph::new(Span::styled(hint, Style::default().fg(theme::muted())))
                                .wrap(Wrap { trim: true }),
                            inner,
                        );
                        continue;
                    }
                    let w = inner.width.saturating_sub(4) as usize;
                    let blocks: Vec<Vec<Line>> =
                        cards.iter().map(|c| card_lines(&board, c, w)).collect();
                    // Scroll so the selected card shows.
                    let height = |i: usize| blocks[i].len() as u16 + 2;
                    let mut start = 0usize;
                    if focused {
                        let last = ui.row.min(blocks.len() - 1);
                        while start < last
                            && (start..=last).map(height).sum::<u16>() + 1 > inner.height
                        {
                            start += 1;
                        }
                    }
                    let bottom = inner.y + inner.height;
                    let mut y = inner.y;
                    if start > 0 {
                        frame.render_widget(
                            Paragraph::new(Span::styled(
                                format!(" ↑ {start} more"),
                                Style::default().fg(theme::muted()),
                            )),
                            Rect::new(inner.x, y, inner.width, 1),
                        );
                        y += 1;
                    }
                    for (i, lines) in blocks.into_iter().enumerate().skip(start) {
                        let h = lines.len() as u16 + 2;
                        if y + h > bottom {
                            if y < bottom {
                                frame.render_widget(
                                    Paragraph::new(Span::styled(
                                        format!(" ↓ {} more", cards.len() - i),
                                        Style::default().fg(theme::muted()),
                                    )),
                                    Rect::new(inner.x, y, inner.width, 1),
                                );
                            }
                            break;
                        }
                        let r = Rect::new(inner.x, y, inner.width, h);
                        let sel = focused && i == ui.row;
                        let border = if sel {
                            theme::accent()
                        } else if cards[i].status == Status::Done {
                            theme::success()
                        } else {
                            theme::border()
                        };
                        let mut b = Block::default()
                            .borders(Borders::ALL)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(border))
                            .padding(Padding::horizontal(1));
                        if sel {
                            b = b.style(Style::default().bg(theme::surface()));
                        }
                        frame.render_widget(Paragraph::new(Text::from(lines)).block(b), r);
                        hits.push((r, ci, i));
                        y += h;
                    }
                }
                if ui.detail
                    && let Some(c) = &selected
                {
                    let w = (area.width * 3 / 5).max(44).min(area.width);
                    let h = (area.height * 3 / 4).max(12).min(rows[1].height);
                    let r = Rect::new(
                        area.width.saturating_sub(w) / 2,
                        rows[1].y + rows[1].height.saturating_sub(h) / 2,
                        w,
                        h,
                    );
                    let mut text = board.render_card(c);
                    text.push_str(&format!("\n\nspace: {}", c.space.label()));
                    if let Some(rm) = &c.remote {
                        text.push_str(&format!("\nJira: {} {}", rm.key, rm.url));
                        if !rm.status.is_empty() {
                            text.push_str(&format!(" ({})", rm.status));
                        }
                    }
                    frame.render_widget(Clear, r);
                    frame.render_widget(
                        Paragraph::new(text)
                            .wrap(Wrap { trim: false })
                            .style(Style::default().fg(theme::text()).bg(theme::bg()))
                            .block(
                                Block::default()
                                    .borders(Borders::ALL)
                                    .border_type(BorderType::Rounded)
                                    .border_style(Style::default().fg(theme::accent()))
                                    .title(Span::styled(
                                        format!(
                                            " {} · Esc closes{} ",
                                            c.shown_key(),
                                            if c.remote.is_some() {
                                                " · o opens in Jira"
                                            } else {
                                                ""
                                            }
                                        ),
                                        Style::default().fg(theme::accent()),
                                    ))
                                    .padding(Padding::uniform(1)),
                            ),
                        r,
                    );
                }
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        truncate(&footer, area.width as usize),
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
                    let inside = |r: &Rect| {
                        m.column >= r.x
                            && m.column < r.x + r.width
                            && m.row >= r.y
                            && m.row < r.y + r.height
                    };
                    if let Some((_, ci, i)) = hits.iter().find(|(r, _, _)| inside(r)) {
                        ui.col = *ci;
                        ui.row = *i;
                    } else if let Some(ci) = col_rects.iter().position(|r| r.width > 0 && inside(r))
                    {
                        ui.col = ci;
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
                                        let r = Board::update(&ws, |b| {
                                            b.add(
                                                NewCard {
                                                    title: text.clone(),
                                                    status: Some(if status == Status::Done {
                                                        Status::Todo
                                                    } else {
                                                        status
                                                    }),
                                                    space,
                                                    ..Default::default()
                                                },
                                                "you",
                                            )
                                        });
                                        if space == Space::Jira {
                                            kick(state);
                                        }
                                        r.map(|k| match space {
                                            Space::Jira => {
                                                format!("added {k} — making its Jira ticket…")
                                            }
                                            Space::Personal => format!("added {k}"),
                                        })
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
                            ui.message = Some(if space == Space::Jira {
                                format!("removed {k} from the board — the Jira ticket itself stays")
                            } else {
                                format!("deleted {k}")
                            });
                        }
                        continue;
                    }
                    if ui.detail {
                        match key.code {
                            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => ui.detail = false,
                            KeyCode::Char('o') => open_remote(&selected, &mut ui),
                            _ => {}
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
                            .filter(|c| c.space == ui.space)
                            .position(|c| c.key == key)
                            .unwrap_or(0);
                        ui.message = Some(format!("{key} → {}", to.label()));
                    };
                    let step = match key.code {
                        KeyCode::Right if shift => Some(1),
                        KeyCode::Left if shift => Some(-1),
                        KeyCode::Char('>') | KeyCode::Char('.') => Some(1),
                        KeyCode::Char('<') | KeyCode::Char(',') => Some(-1),
                        _ => None,
                    };
                    if let Some(step) = step {
                        move_card(step, &mut board, &mut ui);
                        if space == Space::Jira {
                            kick(state);
                        }
                        continue;
                    }
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
                        KeyCode::Char('t') | KeyCode::Tab | KeyCode::BackTab => {
                            ui.space = match ui.space {
                                Space::Personal => Space::Jira,
                                Space::Jira => Space::Personal,
                            };
                            ui.row = 0;
                            if ui.space == Space::Jira && board.jira.is_none() {
                                ui.message = Some(
                                    "Jira isn't linked yet — close the board and run /board jira <PROJECT-KEY>"
                                        .into(),
                                );
                            }
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            ui.col = (ui.col + 1).min(Status::ALL.len() - 1);
                            ui.row = 0;
                        }
                        KeyCode::Left | KeyCode::Char('h') => {
                            ui.col = ui.col.saturating_sub(1);
                            ui.row = 0;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            ui.row = (ui.row + 1).min(columns[ui.col].len().saturating_sub(1))
                        }
                        KeyCode::Up | KeyCode::Char('k') => ui.row = ui.row.saturating_sub(1),
                        KeyCode::Char('n') | KeyCode::Char('a') => {
                            if space == Space::Jira && board.jira.is_none() {
                                ui.message =
                                    Some("link Jira first: /board jira <PROJECT-KEY>".into());
                            } else {
                                ui.typing = Some((Typing::NewCard, String::new()));
                            }
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
                        KeyCode::Char('o') => open_remote(&selected, &mut ui),
                        KeyCode::Char('s') => {
                            if board.jira.is_some() {
                                kick(state);
                                ui.message = Some("syncing with Jira…".into());
                            } else {
                                ui.message = Some(
                                    "nothing to sync: /board jira <PROJECT-KEY> links Jira".into(),
                                );
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

    fn text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }

    #[test]
    fn a_card_shows_its_key_priority_blockers_and_gate() {
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
                priority: Some(Priority::High),
                ..Default::default()
            },
            "",
        );
        let c = b.card("DCO-2").unwrap().clone();
        let t = text(&card_lines(&b, &c, 26));
        assert!(t.starts_with("DCO-2"), "{t}");
        assert!(t.lines().next().unwrap().ends_with("▲ high"), "{t}");
        assert!(t.contains("◆ gate") && t.contains("⛓ DCO-1") && t.contains("⚑ Launch"));
        assert!(t.lines().count() <= 4, "{t}");
    }

    #[test]
    fn a_jira_card_waiting_for_its_ticket_says_so() {
        let mut b = Board::new("DCO");
        b.add(
            NewCard {
                title: "Apple Pay".into(),
                space: Space::Jira,
                ..Default::default()
            },
            "",
        );
        let c = b.card("DCO-1").unwrap().clone();
        assert!(text(&card_lines(&b, &c, 30)).contains("⟳ making ticket"));
    }
}
