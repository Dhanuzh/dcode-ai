//! `/mcp`: the MCP server panel (Claude Code style). A list of servers with
//! their status; Enter opens a server's menu (Authenticate, Reconnect, Sign
//! out, Enable/Disable); Authenticate shows the sign-in URL with "c to copy"
//! and takes a pasted redirect URL when the browser cannot reach localhost.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dcode_ai_common::event::McpStartupStatus;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};

use super::theme;

#[derive(Debug, Clone)]
pub struct McpRow {
    pub name: String,
    /// The server URL for remote (http) servers.
    pub url: Option<String>,
    /// Where it is configured (`.mcp.json`, `plugin:x`, …).
    pub origin: String,
    pub enabled: bool,
    pub signed_in: bool,
    /// Tools this session has from the server.
    pub tools: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Authenticate,
    Reconnect,
    SignOut,
    Disable,
    Enable,
}

impl MenuItem {
    fn label(self) -> &'static str {
        match self {
            Self::Authenticate => "Authenticate",
            Self::Reconnect => "Reconnect",
            Self::SignOut => "Sign out",
            Self::Disable => "Disable",
            Self::Enable => "Enable",
        }
    }
}

/// What the session should do for the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpAction {
    Authenticate(String),
    /// A redirect URL the user pasted for the sign-in in progress.
    PastedRedirect(String),
    CancelAuth,
    Reconnect(String),
    SignOut(String),
    SetEnabled(String, bool),
}

pub enum McpKey {
    Handled,
    Close,
    Run(McpAction),
}

/// The sign-in screen.
#[derive(Debug, Default)]
pub struct AuthView {
    pub server: String,
    /// Set once the authorization URL is known.
    pub url: Option<String>,
    /// A pasted redirect URL.
    pub input: String,
    /// Latest status line, and whether it is an error.
    pub note: Option<(String, bool)>,
    pub done: bool,
}

enum View {
    List,
    Menu { row: usize, sel: usize },
    Auth(AuthView),
}

pub struct McpPanel {
    pub rows: Vec<McpRow>,
    selected: usize,
    view: View,
    /// A finished action's message, shown under the list.
    pub status: Option<(String, bool)>,
    pub busy: Option<String>,
}

impl McpPanel {
    pub fn new(rows: Vec<McpRow>) -> Self {
        Self {
            rows,
            selected: 0,
            view: View::List,
            status: None,
            busy: None,
        }
    }

    pub fn auth_mut(&mut self) -> Option<&mut AuthView> {
        match &mut self.view {
            View::Auth(a) => Some(a),
            _ => None,
        }
    }

    pub fn in_auth(&self) -> bool {
        matches!(self.view, View::Auth(_))
    }

    /// Text pasted while the sign-in screen is open.
    pub fn paste(&mut self, text: &str) {
        if let Some(a) = self.auth_mut() {
            a.input.push_str(text.trim());
        }
    }

    /// Back to the list after a sign-in finished.
    pub fn finish_auth(&mut self, message: String, error: bool) {
        if let Some(a) = self.auth_mut() {
            a.done = true;
            a.note = Some((message.clone(), error));
        }
        self.status = Some((message, error));
    }

    fn menu_items(row: &McpRow) -> Vec<MenuItem> {
        let mut items = Vec::new();
        if row.enabled {
            if row.url.is_some() {
                items.push(MenuItem::Authenticate);
            }
            items.push(MenuItem::Reconnect);
            if row.signed_in {
                items.push(MenuItem::SignOut);
            }
            items.push(MenuItem::Disable);
        } else {
            items.push(MenuItem::Enable);
        }
        items
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> McpKey {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match &mut self.view {
            View::List => match key.code {
                KeyCode::Esc => McpKey::Close,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                    McpKey::Handled
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.selected + 1 < self.rows.len() {
                        self.selected += 1;
                    }
                    McpKey::Handled
                }
                KeyCode::Enter if !self.rows.is_empty() && self.busy.is_none() => {
                    self.view = View::Menu {
                        row: self.selected,
                        sel: 0,
                    };
                    McpKey::Handled
                }
                _ => McpKey::Handled,
            },
            View::Menu { row, sel } => {
                let row_i = *row;
                let items = Self::menu_items(&self.rows[row_i]);
                match key.code {
                    KeyCode::Esc | KeyCode::Left => {
                        self.view = View::List;
                        McpKey::Handled
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *sel = sel.saturating_sub(1);
                        McpKey::Handled
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if *sel + 1 < items.len() {
                            *sel += 1;
                        }
                        McpKey::Handled
                    }
                    KeyCode::Enter => {
                        let name = self.rows[row_i].name.clone();
                        let item = items[(*sel).min(items.len() - 1)];
                        self.status = None;
                        let action = match item {
                            MenuItem::Authenticate => {
                                self.view = View::Auth(AuthView {
                                    server: name.clone(),
                                    note: Some(("Starting sign-in…".into(), false)),
                                    ..AuthView::default()
                                });
                                return McpKey::Run(McpAction::Authenticate(name));
                            }
                            MenuItem::Reconnect => McpAction::Reconnect(name),
                            MenuItem::SignOut => McpAction::SignOut(name),
                            MenuItem::Disable => McpAction::SetEnabled(name, false),
                            MenuItem::Enable => McpAction::SetEnabled(name, true),
                        };
                        self.busy = Some(format!("{}…", item.label()));
                        self.view = View::List;
                        McpKey::Run(action)
                    }
                    _ => McpKey::Handled,
                }
            }
            View::Auth(auth) => match key.code {
                KeyCode::Esc => {
                    let running = !auth.done;
                    self.view = View::List;
                    if running {
                        McpKey::Run(McpAction::CancelAuth)
                    } else {
                        McpKey::Handled
                    }
                }
                KeyCode::Enter if auth.done => {
                    self.view = View::List;
                    McpKey::Handled
                }
                KeyCode::Enter if !auth.input.trim().is_empty() => {
                    let pasted = std::mem::take(&mut auth.input);
                    auth.note = Some(("Checking the pasted URL…".into(), false));
                    McpKey::Run(McpAction::PastedRedirect(pasted))
                }
                // `c` copies the sign-in URL (when nothing is being typed).
                KeyCode::Char('c') if !ctrl && auth.input.is_empty() => {
                    if let Some(url) = &auth.url {
                        auth.note = Some(match super::clipboard::copy_to_clipboard(url) {
                            Ok(_) => ("Copied the sign-in URL.".into(), false),
                            Err(e) => (format!("Could not copy: {e}"), true),
                        });
                    }
                    McpKey::Handled
                }
                KeyCode::Backspace => {
                    auth.input.pop();
                    McpKey::Handled
                }
                KeyCode::Char('u') if ctrl => {
                    auth.input.clear();
                    McpKey::Handled
                }
                KeyCode::Char(ch) if !ctrl && !auth.done => {
                    auth.input.push(ch);
                    McpKey::Handled
                }
                _ => McpKey::Handled,
            },
        }
    }

    pub fn render(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        statuses: &HashMap<String, McpStartupStatus>,
    ) {
        frame.render_widget(Clear, area);
        let title = match &self.view {
            View::Auth(a) => format!(" Authenticate {} ", a.server),
            View::Menu { row, .. } => format!(" MCP · {} ", self.rows[*row].name),
            View::List => " MCP servers ".to_string(),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::accent()))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ))
            .padding(Padding::horizontal(1))
            .style(Style::default().bg(theme::bg()));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let lines = self.lines(inner.width as usize, statuses);
        frame.render_widget(
            Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
            inner,
        );
    }

    pub fn lines(
        &self,
        width: usize,
        statuses: &HashMap<String, McpStartupStatus>,
    ) -> Vec<Line<'static>> {
        let muted = Style::default().fg(theme::muted());
        let text = Style::default().fg(theme::text());
        let accent = Style::default().fg(theme::accent());
        let mut out: Vec<Line<'static>> = Vec::new();
        match &self.view {
            View::List => {
                if self.rows.is_empty() {
                    out.push(Line::from(Span::styled("No MCP servers configured.", text)));
                    out.push(Line::default());
                    out.push(Line::from(Span::styled(
                        "Add one: dcode-ai mcp add docs --transport http https://example.com/mcp",
                        muted,
                    )));
                    out.push(Line::from(Span::styled(
                        "or install a plugin that ships one (/plugins).",
                        muted,
                    )));
                }
                for (i, row) in self.rows.iter().enumerate() {
                    let sel = i == self.selected;
                    let (glyph, state, style) = row_state(row, statuses.get(&row.name));
                    let name_style = if sel {
                        accent.add_modifier(Modifier::BOLD)
                    } else {
                        text
                    };
                    out.push(Line::from(vec![
                        Span::styled(if sel { "❯ " } else { "  " }, accent),
                        Span::styled(format!("{glyph} "), style),
                        Span::styled(row.name.clone(), name_style),
                        Span::styled(format!(" · {state}"), style),
                    ]));
                    let kind = if row.url.is_some() { "http" } else { "stdio" };
                    out.push(Line::from(Span::styled(
                        format!("    {kind} · {}", row.origin),
                        muted,
                    )));
                }
                out.push(Line::default());
                if let Some(busy) = &self.busy {
                    out.push(Line::from(Span::styled(busy.clone(), accent)));
                } else if let Some((msg, err)) = &self.status {
                    out.push(Line::from(Span::styled(
                        msg.clone(),
                        if *err {
                            Style::default().fg(theme::error())
                        } else {
                            Style::default().fg(theme::success())
                        },
                    )));
                }
                out.push(Line::from(Span::styled(
                    "↑↓ choose · enter options · esc close",
                    muted,
                )));
            }
            View::Menu { row, sel } => {
                let r = &self.rows[*row];
                let (glyph, state, style) = row_state(r, statuses.get(&r.name));
                out.push(Line::from(vec![
                    Span::styled(format!("{glyph} "), style),
                    Span::styled(r.name.clone(), text.add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" · {state}"), style),
                ]));
                if let Some(url) = &r.url {
                    out.push(Line::from(Span::styled(format!("URL: {url}"), muted)));
                }
                out.push(Line::from(Span::styled(
                    format!("From: {}", r.origin),
                    muted,
                )));
                out.push(Line::default());
                for (i, item) in Self::menu_items(r).iter().enumerate() {
                    let on = i == *sel;
                    out.push(Line::from(vec![
                        Span::styled(if on { "❯ " } else { "  " }, accent),
                        Span::styled(
                            format!("{}. {}", i + 1, item.label()),
                            if on { accent } else { text },
                        ),
                    ]));
                }
                out.push(Line::default());
                out.push(Line::from(Span::styled(
                    "↑↓ choose · enter select · esc back",
                    muted,
                )));
            }
            View::Auth(a) => {
                if let Some(url) = &a.url {
                    {
                        out.push(Line::from(Span::styled(
                            "A browser window opens for sign-in. If it doesn't, open this URL:",
                            text,
                        )));
                        out.push(Line::default());
                        out.push(Line::from(Span::styled(url.clone(), accent)));
                        out.push(Line::default());
                        out.push(Line::from(Span::styled(
                            "If the browser shows a connection error after you approve (common on WSL),",
                            muted,
                        )));
                        out.push(Line::from(Span::styled(
                            "copy the address from the address bar and paste it here:",
                            muted,
                        )));
                        let shown = if a.input.is_empty() {
                            Span::styled("paste redirect URL", muted)
                        } else {
                            Span::styled(clip_left(&a.input, width.saturating_sub(4)), text)
                        };
                        out.push(Line::from(vec![Span::styled("› ", accent), shown]));
                    }
                }
                if let Some((note, err)) = &a.note {
                    out.push(Line::default());
                    out.push(Line::from(Span::styled(
                        note.clone(),
                        if *err {
                            Style::default().fg(theme::error())
                        } else if a.done {
                            Style::default().fg(theme::success())
                        } else {
                            accent
                        },
                    )));
                }
                out.push(Line::default());
                out.push(Line::from(Span::styled(
                    if a.done {
                        "enter / esc back"
                    } else if a.url.is_some() {
                        "c copy URL · enter submit pasted URL · esc cancel"
                    } else {
                        "esc cancel"
                    },
                    muted,
                )));
            }
        }
        out
    }
}

/// `(glyph, words, style)` for a server's state.
fn row_state(row: &McpRow, status: Option<&McpStartupStatus>) -> (&'static str, String, Style) {
    let ok = Style::default().fg(theme::success());
    let warn = Style::default().fg(theme::warn());
    let bad = Style::default().fg(theme::error());
    let muted = Style::default().fg(theme::muted());
    if !row.enabled {
        return ("○", "disabled".into(), muted);
    }
    match status {
        Some(McpStartupStatus::Ready) | None if row.tools > 0 => (
            "✔",
            format!(
                "connected · {} tool{}",
                row.tools,
                if row.tools == 1 { "" } else { "s" }
            ),
            ok,
        ),
        Some(McpStartupStatus::Starting | McpStartupStatus::Initializing) => {
            ("…", "connecting".into(), muted)
        }
        Some(McpStartupStatus::Failed { message })
            if message.contains("sign-in") || message.contains("401") =>
        {
            ("!", "needs sign-in · enter to authenticate".into(), warn)
        }
        Some(McpStartupStatus::Failed { message }) => {
            let first = message.lines().next().unwrap_or("").to_string();
            ("✗", format!("failed: {first}"), bad)
        }
        Some(McpStartupStatus::Ready) => ("✔", "connected".into(), ok),
        None => ("…", "not connected".into(), muted),
    }
}

/// The end of `s` that fits `width` (a long pasted URL shows its tail).
fn clip_left(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - width.saturating_sub(1)).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn panel() -> McpPanel {
        McpPanel::new(vec![
            McpRow {
                name: "catalyst".into(),
                url: Some("https://example.com/mcp".into()),
                origin: "plugin:catalyst".into(),
                enabled: true,
                signed_in: false,
                tools: 0,
            },
            McpRow {
                name: "files".into(),
                url: None,
                origin: ".mcp.json".into(),
                enabled: false,
                signed_in: false,
                tools: 0,
            },
        ])
    }

    #[test]
    fn enter_authenticate_opens_sign_in_and_paste_submits() {
        let mut p = panel();
        assert!(matches!(p.handle_key(key(KeyCode::Enter)), McpKey::Handled));
        match p.handle_key(key(KeyCode::Enter)) {
            McpKey::Run(McpAction::Authenticate(name)) => assert_eq!(name, "catalyst"),
            _ => panic!("expected authenticate"),
        }
        assert!(p.in_auth());
        p.auth_mut().unwrap().url = Some("https://auth/x".into());
        p.paste("http://127.0.0.1:1/callback?code=a&state=b\n");
        match p.handle_key(key(KeyCode::Enter)) {
            McpKey::Run(McpAction::PastedRedirect(u)) => {
                assert_eq!(u, "http://127.0.0.1:1/callback?code=a&state=b")
            }
            _ => panic!("expected pasted redirect"),
        }
        assert!(matches!(
            p.handle_key(key(KeyCode::Esc)),
            McpKey::Run(McpAction::CancelAuth)
        ));
        assert!(!p.in_auth());
    }

    #[test]
    fn statuses_and_menus_follow_the_server() {
        let mut statuses = HashMap::new();
        statuses.insert(
            "catalyst".to_string(),
            McpStartupStatus::Failed {
                message: "needs sign-in: run …".into(),
            },
        );
        let p = panel();
        let text: String = p
            .lines(80, &statuses)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("needs sign-in"));
        assert!(text.contains("disabled"));
        assert_eq!(
            McpPanel::menu_items(&p.rows[0]),
            vec![
                MenuItem::Authenticate,
                MenuItem::Reconnect,
                MenuItem::Disable
            ]
        );
        assert_eq!(McpPanel::menu_items(&p.rows[1]), vec![MenuItem::Enable]);
    }
}
