//! A masked password popup for actions that need sudo (installing an update
//! into /usr/local/bin). The password is only kept here until Enter, never
//! shown, logged or sent to the model, and wiped when the popup closes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};

use super::theme;

pub struct SudoPrompt {
    /// What the password is for.
    message: String,
    /// Shown after a rejected password.
    error: Option<String>,
    input: String,
    /// What Enter does, for the hint ("install", "run").
    action: &'static str,
}

pub enum SudoKey {
    Handled,
    Cancel,
    /// The typed password.
    Submit(Secret),
}

/// A typed password: prints as `<hidden>` and is wiped when dropped.
pub struct Secret(String);

impl Secret {
    /// The bytes, moved out (the caller wipes them after use).
    pub fn into_bytes(mut self) -> Vec<u8> {
        std::mem::take(&mut self.0).into_bytes()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<hidden>")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Zero a string's bytes before it is freed.
pub fn wipe(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    bytes.fill(0);
}

impl SudoPrompt {
    pub fn new(message: impl Into<String>, error: Option<String>) -> Self {
        Self {
            message: message.into(),
            error,
            input: String::new(),
            action: "install",
        }
    }

    /// Name what Enter does in the hint.
    pub fn action(mut self, action: &'static str) -> Self {
        self.action = action;
        self
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SudoKey {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return SudoKey::Cancel,
            KeyCode::Enter if !self.input.is_empty() => {
                return SudoKey::Submit(Secret(std::mem::take(&mut self.input)));
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char('u') if ctrl => wipe(&mut self.input),
            KeyCode::Char(c) if !ctrl => self.input.push(c),
            _ => {}
        }
        SudoKey::Handled
    }

    pub fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let width = area.width.min(72);
        let rect = super::layout::centered_rect(area, width, 9.min(area.height));
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::accent()))
            .title(Span::styled(
                " sudo password ",
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(
                format!(" Enter {} · Esc cancel ", self.action),
                Style::default().fg(theme::muted()),
            ))
            .padding(Padding::horizontal(1))
            .style(Style::default().bg(theme::bg()));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let muted = Style::default().fg(theme::muted());
        let mut lines = vec![
            Line::from(Span::styled(
                self.message.clone(),
                Style::default().fg(theme::text()),
            )),
            Line::default(),
            Line::from(vec![
                Span::styled("Password: ", muted),
                Span::styled(
                    "•".repeat(self.input.chars().count()),
                    Style::default().fg(theme::text()),
                ),
                Span::styled(" ", Style::default().bg(theme::muted())),
            ]),
        ];
        if let Some(err) = &self.error {
            lines.push(Line::from(Span::styled(
                err.clone(),
                Style::default().fg(theme::error()),
            )));
        }
        lines.push(Line::from(Span::styled(
            "Used once for sudo; never stored, logged or shown to the model.",
            muted,
        )));
        frame.render_widget(
            Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
            inner,
        );
    }
}

impl Drop for SudoPrompt {
    fn drop(&mut self) {
        wipe(&mut self.input);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn typing_then_enter_submits_and_esc_cancels() {
        let mut p = SudoPrompt::new("x", None);
        assert!(matches!(
            p.handle_key(key(KeyCode::Enter)),
            SudoKey::Handled
        ));
        for c in "pw1".chars() {
            p.handle_key(key(KeyCode::Char(c)));
        }
        p.handle_key(key(KeyCode::Backspace));
        match p.handle_key(key(KeyCode::Enter)) {
            SudoKey::Submit(pw) => {
                assert_eq!(format!("{pw:?}"), "<hidden>");
                assert_eq!(pw.into_bytes(), b"pw");
            }
            _ => panic!("expected submit"),
        }
        assert!(matches!(p.handle_key(key(KeyCode::Esc)), SudoKey::Cancel));
    }
}
