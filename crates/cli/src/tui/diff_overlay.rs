//! Full-screen diff view (Enter in the dashboard's changes tab, or `/diff`):
//! changed files on the left, the selected file's diff with line numbers on
//! the right. Alt-screen with its own input loop, like the transcript view.

use crate::tui::diff_hunk::DiffHunk;
use crate::tui::diff_review::DiffReview;
use crate::tui::render_helpers::truncate_chars;
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

/// `(old start, new start)` from a `@@ -10,3 +12,5 @@` header.
fn hunk_starts(header: &str) -> (usize, usize) {
    let mut old = 1;
    let mut new = 1;
    for part in header.split_whitespace() {
        let num = |p: &str| {
            p[1..]
                .split(',')
                .next()
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(1)
        };
        if part.starts_with('-') && part.len() > 1 {
            old = num(part);
        } else if part.starts_with('+') && part.len() > 1 {
            new = num(part);
        }
    }
    (old, new)
}

/// The diff of one file as lines, plus the line index where each hunk starts.
pub(crate) fn diff_lines(hunks: &[DiffHunk], width: usize) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut lines = Vec::new();
    let mut starts = Vec::new();
    let gutter = Style::default().fg(theme::muted());
    for hunk in hunks {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        starts.push(lines.len());
        lines.push(Line::from(Span::styled(
            hunk.header.clone(),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )));
        let (mut old, mut new) = hunk_starts(&hunk.header);
        for (sigil, text) in &hunk.lines {
            let (o, n) = match sigil {
                '-' => {
                    old += 1;
                    (format!("{:>5}", old - 1), "     ".to_string())
                }
                '+' => {
                    new += 1;
                    ("     ".to_string(), format!("{:>5}", new - 1))
                }
                _ => {
                    old += 1;
                    new += 1;
                    (format!("{:>5}", old - 1), format!("{:>5}", new - 1))
                }
            };
            let (fg, bg) = match sigil {
                '+' => (theme::success(), Some(theme::diff_add())),
                '-' => (theme::error(), Some(theme::diff_del())),
                _ => (theme::text(), None),
            };
            let body = truncate_chars(&format!("{sigil} {text}"), width.saturating_sub(13));
            // Fill the row so the add/remove colour runs the full width.
            let pad = width.saturating_sub(13 + body.chars().count());
            let mut style = Style::default().fg(fg);
            if let Some(bg) = bg {
                style = style.bg(bg);
            }
            lines.push(Line::from(vec![
                Span::styled(format!("{o} {n} "), gutter),
                Span::styled(format!("{body}{}", " ".repeat(pad)), style),
            ]));
        }
    }
    (lines, starts)
}

/// File list rows: name first (it says the most), folder dimmed, then
/// `+N −M`; the selected one highlighted.
fn file_rows(review: &DiffReview, selected: usize, width: usize) -> Vec<Line<'static>> {
    review
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let added = format!(" +{}", f.added);
            let removed = if f.removed > 0 {
                format!(" −{}", f.removed)
            } else {
                String::new()
            };
            let new = if f.untracked { " new" } else { "" };
            let (dir, name) = match f.path.rsplit_once('/') {
                Some((d, n)) => (format!(" {d}/"), n.to_string()),
                None => (String::new(), f.path.clone()),
            };
            let room = width
                .saturating_sub(2 + added.chars().count() + removed.chars().count() + new.len());
            let name = truncate_chars(&name, room);
            let dir = truncate_chars(&dir, room.saturating_sub(name.chars().count()));
            let sel = i == selected;
            let name_style = if sel {
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::text())
            };
            Line::from(vec![
                Span::styled(
                    if sel { "❯ " } else { "  " },
                    Style::default().fg(theme::accent()),
                ),
                Span::styled(name, name_style),
                Span::styled(dir, Style::default().fg(theme::muted())),
                Span::styled(new.to_string(), Style::default().fg(theme::warn())),
                Span::styled(added, Style::default().fg(theme::success())),
                Span::styled(removed, Style::default().fg(theme::error())),
            ])
        })
        .collect()
}

fn first_item(review: &DiffReview, fi: usize) -> usize {
    review.first_item_of(fi)
}

fn pane(title: &str, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused {
            theme::accent()
        } else {
            theme::border()
        }))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1))
}

/// Run the diff view until the user closes it.
pub(crate) fn run_diff_overlay(state: &Arc<Mutex<TuiSessionState>>) -> anyhow::Result<()> {
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    execute!(
        stdout(),
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut term = ratatui::Terminal::new(backend)?;
    {
        let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let ws = g.workspace_root.clone();
        g.diff_review.refresh(&ws);
        g.diff_overlay_scroll = 0;
    }
    // Focus: false = file list, true = diff.
    let mut diff_focus = false;
    let mut list_rect = Rect::default();
    let mut status: Option<String> = None;

    let res: anyhow::Result<()> = (|| {
        loop {
            let size = term.size()?;
            let area = Rect::new(0, 0, size.width, size.height);
            let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
            let list_w = (area.width / 3).clamp(24, 48);
            let cols = Layout::horizontal([Constraint::Length(list_w), Constraint::Min(20)])
                .split(rows[0]);
            list_rect = cols[0];
            let view_h = cols[1].height.saturating_sub(2) as usize;
            let diff_w = cols[1].width.saturating_sub(4) as usize;

            let (files, body, starts, title, max_scroll, scroll, confirm) = {
                let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                if !g.diff_overlay_open {
                    return Ok(());
                }
                let selected = g.diff_review.selected().map_or(0, |(fi, _)| fi);
                let files = file_rows(&g.diff_review, selected, list_w.saturating_sub(4) as usize);
                let (body, starts, title) = match g.diff_review.files.get(selected) {
                    Some(f) if f.untracked && f.hunks.is_empty() => {
                        (vec![Line::from("empty new file")], vec![0], f.path.clone())
                    }
                    Some(f) => {
                        let (b, s) = diff_lines(&f.hunks, diff_w);
                        (b, s, f.path.clone())
                    }
                    None => (
                        vec![Line::from(Span::styled(
                            g.diff_review
                                .error
                                .clone()
                                .unwrap_or_else(|| "no unstaged changes".into()),
                            Style::default().fg(theme::muted()),
                        ))],
                        Vec::new(),
                        "diff".into(),
                    ),
                };
                let max_scroll = body.len().saturating_sub(view_h);
                g.diff_overlay_scroll = g.diff_overlay_scroll.min(max_scroll);
                (
                    files,
                    body,
                    starts,
                    title,
                    max_scroll,
                    g.diff_overlay_scroll,
                    g.diff_review.confirm_revert,
                )
            };

            term.draw(|frame| {
                frame.render_widget(
                    Block::default().style(Style::default().bg(theme::bg())),
                    area,
                );
                let n = files.len();
                frame.render_widget(
                    Paragraph::new(Text::from(files)).block(pane(
                        &format!("{n} changed file{}", if n == 1 { "" } else { "s" }),
                        !diff_focus,
                    )),
                    cols[0],
                );
                let visible: Vec<Line> = body.into_iter().skip(scroll).take(view_h).collect();
                frame.render_widget(
                    Paragraph::new(Text::from(visible)).block(pane(&title, diff_focus)),
                    cols[1],
                );
                let hint = if confirm {
                    "press r again to revert this hunk".to_string()
                } else if let Some(s) = &status {
                    s.clone()
                } else {
                    " ↑↓ file · Tab diff · PgUp/PgDn scroll · n/p hunk · r revert hunk · g refresh · q close"
                        .into()
                };
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        hint,
                        Style::default().fg(if confirm {
                            theme::warn()
                        } else {
                            theme::muted()
                        }),
                    )))
                    .style(Style::default().bg(theme::surface())),
                    rows[1],
                );
            })?;

            if !poll(Duration::from_millis(250))? {
                continue;
            }
            let ev = read()?;
            let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            let selected_file = g.diff_review.selected().map_or(0, |(fi, _)| fi);
            let file_count = g.diff_review.files.len();
            let select_file = |g: &mut TuiSessionState, fi: usize| {
                g.diff_review.cursor = first_item(&g.diff_review, fi);
                g.diff_review.confirm_revert = false;
                g.diff_overlay_scroll = 0;
            };
            match ev {
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollUp => {
                        g.diff_overlay_scroll = g.diff_overlay_scroll.saturating_sub(3)
                    }
                    MouseEventKind::ScrollDown => {
                        g.diff_overlay_scroll = (g.diff_overlay_scroll + 3).min(max_scroll)
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if m.column < list_rect.x + list_rect.width
                            && m.row > list_rect.y
                            && m.row < list_rect.y + list_rect.height =>
                    {
                        let fi = (m.row - list_rect.y - 1) as usize;
                        if fi < file_count {
                            select_file(&mut g, fi);
                        }
                    }
                    _ => {}
                },
                Event::Key(key) if !matches!(key.kind, KeyEventKind::Release) => {
                    // Your `/keymap` remaps apply here too.
                    let Some(key) = (if g.key_bindings.is_empty() {
                        Some(key)
                    } else {
                        crate::tui::keymap::translate_key(key, &g.key_bindings)
                    }) else {
                        continue;
                    };
                    status = None;
                    let plain = !key.modifiers.contains(KeyModifiers::CONTROL);
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => {
                            g.diff_overlay_open = false;
                            return Ok(());
                        }
                        KeyCode::Char('c') if !plain => {
                            g.diff_overlay_open = false;
                            return Ok(());
                        }
                        KeyCode::Tab | KeyCode::BackTab => diff_focus = !diff_focus,
                        KeyCode::Up | KeyCode::Char('k') if !diff_focus => {
                            select_file(&mut g, selected_file.saturating_sub(1))
                        }
                        KeyCode::Down | KeyCode::Char('j') if !diff_focus => select_file(
                            &mut g,
                            (selected_file + 1).min(file_count.saturating_sub(1)),
                        ),
                        KeyCode::Up | KeyCode::Char('k') => {
                            g.diff_overlay_scroll = g.diff_overlay_scroll.saturating_sub(1)
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            g.diff_overlay_scroll = (g.diff_overlay_scroll + 1).min(max_scroll)
                        }
                        KeyCode::PageUp => {
                            g.diff_overlay_scroll = g.diff_overlay_scroll.saturating_sub(view_h)
                        }
                        KeyCode::PageDown | KeyCode::Char(' ') => {
                            g.diff_overlay_scroll = (g.diff_overlay_scroll + view_h).min(max_scroll)
                        }
                        KeyCode::Enter => diff_focus = true,
                        // Next / previous hunk of this file (also selects it for `r`).
                        KeyCode::Char('n') | KeyCode::Char('p') => {
                            let (fi, hi) = g.diff_review.selected().unwrap_or((0, 0));
                            let count = starts.len();
                            if count > 0 {
                                let hi = if key.code == KeyCode::Char('n') {
                                    (hi + 1).min(count - 1)
                                } else {
                                    hi.saturating_sub(1)
                                };
                                g.diff_review.cursor = first_item(&g.diff_review, fi) + hi;
                                g.diff_review.confirm_revert = false;
                                g.diff_overlay_scroll = starts[hi].min(max_scroll);
                                diff_focus = true;
                            }
                        }
                        KeyCode::Char('r') => {
                            let ws = g.workspace_root.clone();
                            status = g.diff_review.press_revert(&ws);
                        }
                        KeyCode::Char('g') => {
                            let ws = g.workspace_root.clone();
                            g.diff_review.refresh(&ws);
                            status = Some("refreshed".into());
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
    fn diff_lines_number_old_and_new_sides() {
        let hunks = vec![DiffHunk {
            header: "@@ -10,3 +10,3 @@".into(),
            lines: vec![
                (' ', "keep".into()),
                ('-', "old line".into()),
                ('+', "new line".into()),
            ],
        }];
        let (lines, starts) = diff_lines(&hunks, 60);
        assert_eq!(starts, vec![0]);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            text[1].starts_with("   10    10 ") && text[1].contains("  keep"),
            "{text:?}"
        );
        assert!(text[2].starts_with("   11       - old line"), "{text:?}");
        assert!(text[3].starts_with("         11 + new line"), "{text:?}");
    }

    #[test]
    fn hunk_starts_are_read_from_the_header() {
        assert_eq!(hunk_starts("@@ -10,3 +12,5 @@"), (10, 12));
        assert_eq!(hunk_starts("@@ -0,0 +1,4 @@"), (0, 1));
    }
}
