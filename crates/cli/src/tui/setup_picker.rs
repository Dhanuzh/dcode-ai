//! `/setup` picker: tick roles and languages, Enter installs them.

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent};
use dcode_ai_common::packs::{self, PackKind, SetupState};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};

use super::render_helpers::truncate_chars;
use super::theme;

struct Row {
    id: String,
    name: String,
    description: String,
    kind: PackKind,
    checked: bool,
    installed: bool,
    detected: bool,
}

pub struct SetupPicker {
    rows: Vec<Row>,
    cursor: usize,
}

pub enum SetupKey {
    Handled,
    Close,
    /// Install these pack ids.
    Install(Vec<String>),
}

impl SetupPicker {
    /// Roles first, then languages; languages found in the repo start ticked.
    pub fn load(workspace_root: &Path) -> Self {
        let (all, _) = packs::load(workspace_root);
        let detected: Vec<String> = packs::detect(&all, workspace_root)
            .iter()
            .map(|p| p.id.clone())
            .collect();
        let state = SetupState::load();
        let mut rows: Vec<Row> = all
            .into_iter()
            .map(|p| {
                let installed = state.packs.contains_key(&p.id);
                let found = detected.contains(&p.id);
                Row {
                    checked: found && !installed,
                    installed,
                    detected: found,
                    id: p.id,
                    name: p.name,
                    description: p.description,
                    kind: p.kind,
                }
            })
            .collect();
        rows.sort_by_key(|r| r.kind == PackKind::Language);
        Self { rows, cursor: 0 }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SetupKey {
        let n = self.rows.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return SetupKey::Close,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => {
                self.cursor = (self.cursor + n - 1) % n;
            }
            KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                self.cursor = (self.cursor + 1) % n;
            }
            // Two-column layout: jump to the same row in the other column.
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') if n > 0 => {
                let roles = self
                    .rows
                    .iter()
                    .filter(|r| r.kind == PackKind::Role)
                    .count();
                let langs = n - roles;
                self.cursor = if self.cursor < roles {
                    roles + self.cursor.min(langs.saturating_sub(1))
                } else {
                    (self.cursor - roles).min(roles.saturating_sub(1))
                };
            }
            KeyCode::Char(' ') | KeyCode::Tab => {
                if let Some(r) = self.rows.get_mut(self.cursor)
                    && !r.installed
                {
                    r.checked = !r.checked;
                }
            }
            KeyCode::Enter => {
                let ids: Vec<String> = self
                    .rows
                    .iter()
                    .filter(|r| r.checked && !r.installed)
                    .map(|r| r.id.clone())
                    .collect();
                return if ids.is_empty() {
                    SetupKey::Close
                } else {
                    SetupKey::Install(ids)
                };
            }
            _ => {}
        }
        SetupKey::Handled
    }

    pub fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let roles = self
            .rows
            .iter()
            .filter(|r| r.kind == PackKind::Role)
            .count();
        let tallest = roles.max(self.rows.len() - roles) as u16;
        let height = (tallest + 4).min(area.height);
        let width = area.width.min(96);
        let rect = super::layout::centered_rect(area, width, height);
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::accent()))
            .title(Span::styled(
                " Starter packs ",
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(
                " ↑↓←→ move · Space select · Enter install · Esc close ",
                Style::default().fg(theme::muted()),
            ))
            .padding(Padding::horizontal(1))
            .style(Style::default().bg(theme::bg()));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let lines = self.lines(inner.width as usize, inner.height as usize);
        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn lines(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        if width >= 64 {
            return self.two_column_lines(width);
        }
        self.list_lines(width, height)
    }

    /// Roles left, languages right; the selected pack's description below.
    fn two_column_lines(&self, width: usize) -> Vec<Line<'static>> {
        let muted = Style::default().fg(theme::muted());
        let text = Style::default().fg(theme::text());
        let accent = Style::default().fg(theme::accent());
        let col = width / 2;
        let cell = |i: Option<usize>| -> Vec<Span<'static>> {
            let Some(i) = i else {
                return vec![Span::raw(" ".repeat(col))];
            };
            let r = &self.rows[i];
            let selected = i == self.cursor;
            let mark = if r.installed {
                "[●]"
            } else if r.checked {
                "[✔]"
            } else {
                "[ ]"
            };
            let tag = if r.installed {
                " installed"
            } else if r.detected {
                " found"
            } else {
                ""
            };
            let head = format!("{}{mark} ", if selected { "❯ " } else { "  " });
            let name = truncate_chars(
                &r.name,
                col.saturating_sub(head.chars().count() + tag.len() + 1),
            );
            let pad = col.saturating_sub(head.chars().count() + name.chars().count() + tag.len());
            let style = if selected {
                accent.add_modifier(Modifier::BOLD)
            } else {
                text
            };
            vec![
                Span::styled(head, style),
                Span::styled(name, style),
                Span::styled(tag.to_string(), accent),
                Span::raw(" ".repeat(pad)),
            ]
        };
        let roles: Vec<usize> = (0..self.rows.len())
            .filter(|&i| self.rows[i].kind == PackKind::Role)
            .collect();
        let langs: Vec<usize> = (0..self.rows.len())
            .filter(|&i| self.rows[i].kind == PackKind::Language)
            .collect();
        let mut out = vec![Line::from(vec![
            Span::styled(
                format!("{:<col$}", "What do you work on?"),
                text.add_modifier(Modifier::BOLD),
            ),
            Span::styled("Languages", text.add_modifier(Modifier::BOLD)),
        ])];
        for k in 0..roles.len().max(langs.len()) {
            let mut spans = cell(roles.get(k).copied());
            spans.extend(cell(langs.get(k).copied()));
            out.push(Line::from(spans));
        }
        let n = self
            .rows
            .iter()
            .filter(|r| r.checked && !r.installed)
            .count();
        let selected = format!("{n} selected · ");
        let desc = self
            .rows
            .get(self.cursor)
            .map(|r| truncate_chars(&r.description, width.saturating_sub(selected.len())))
            .unwrap_or_default();
        out.push(Line::from(vec![
            Span::styled(selected, accent),
            Span::styled(desc, muted),
        ]));
        out
    }

    fn list_lines(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        let muted = Style::default().fg(theme::muted());
        let text = Style::default().fg(theme::text());
        let accent = Style::default().fg(theme::accent());
        let mut body: Vec<Line<'static>> = Vec::new();
        let mut cursor_line = 0;
        let mut last_kind = None;
        for (i, r) in self.rows.iter().enumerate() {
            if last_kind != Some(r.kind) {
                if last_kind.is_some() {
                    body.push(Line::default());
                }
                body.push(Line::from(Span::styled(
                    match r.kind {
                        PackKind::Role => "What do you work on?",
                        PackKind::Language => "Languages",
                    },
                    text.add_modifier(Modifier::BOLD),
                )));
                last_kind = Some(r.kind);
            }
            let selected = i == self.cursor;
            let pointer = if selected { "❯ " } else { "  " };
            let mark = if r.installed {
                "[●]"
            } else if r.checked {
                "[✔]"
            } else {
                "[ ]"
            };
            let tag = if r.installed {
                "  installed"
            } else if r.detected {
                "  found in repo"
            } else {
                ""
            };
            let name_style = if selected {
                accent.add_modifier(Modifier::BOLD)
            } else {
                text
            };
            let head = format!("{pointer}{mark} {:<24}", r.name);
            let room = width.saturating_sub(head.chars().count() + tag.len() + 2);
            if selected {
                cursor_line = body.len();
            }
            body.push(Line::from(vec![
                Span::styled(head, name_style),
                Span::styled(truncate_chars(&r.description, room), muted),
                Span::styled(tag.to_string(), accent),
            ]));
        }
        // Keep the cursor in view when the list is taller than the box.
        let room = height.saturating_sub(3);
        let skip = cursor_line.saturating_sub(room.saturating_sub(1));
        let mut out: Vec<Line<'static>> = body.into_iter().skip(skip).take(room).collect();
        out.push(Line::default());
        let n = self
            .rows
            .iter()
            .filter(|r| r.checked && !r.installed)
            .count();
        out.push(Line::from(vec![
            Span::styled(format!("{n} selected  "), accent),
            Span::styled("↑↓ move · Space select · Enter install · Esc close", muted),
        ]));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn space_ticks_and_enter_returns_the_ticked_packs() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = SetupPicker::load(dir.path());
        for r in &mut p.rows {
            r.installed = false;
            r.checked = false;
        }
        assert!(matches!(p.handle_key(key(KeyCode::Enter)), SetupKey::Close));
        p.handle_key(key(KeyCode::Char(' ')));
        p.handle_key(key(KeyCode::Down));
        p.handle_key(key(KeyCode::Char(' ')));
        match p.handle_key(key(KeyCode::Enter)) {
            SetupKey::Install(ids) => assert_eq!(ids, [p.rows[0].id.clone(), p.rows[1].id.clone()]),
            _ => panic!("expected install"),
        }
        assert_eq!(p.rows[0].kind, PackKind::Role);
    }
}
