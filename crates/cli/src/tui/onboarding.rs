//! Sign-in onboarding TUI shown before runtime startup when no provider is ready.

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use dcode_ai_common::config::{DcodeAiConfig, ProviderKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::oauth_login::OAuthProvider;

use super::terminal::{restore_terminal, setup_terminal};
use super::theme;

#[derive(Clone, Copy)]
struct OAuthChoice {
    title: &'static str,
    subtitle: &'static str,
    login: OAuthProvider,
    provider: ProviderKind,
}

/// Every choice is a sign-in flow; no API keys are pasted.
const OAUTH_CHOICES: &[OAuthChoice] = &[
    OAuthChoice {
        title: "Claude",
        subtitle: "Your Claude account via Claude Code (`claude`)",
        login: OAuthProvider::Anthropic,
        provider: ProviderKind::Anthropic,
    },
    OAuthChoice {
        title: "GitHub Copilot",
        subtitle: "GitHub device login",
        login: OAuthProvider::Copilot,
        provider: ProviderKind::Copilot,
    },
];

pub async fn run_onboarding(mut config: DcodeAiConfig) -> anyhow::Result<DcodeAiConfig> {
    // Onboarding can run before the main REPL initializes the theme from
    // config, so apply the user's configured theme now.
    super::theme::set_by_name(config.ui.theme.as_deref());
    // In tmux, keep mouse capture on by default so wheel scrolling is reliable.
    let mouse_capture = config.ui.mouse_capture || std::env::var_os("TMUX").is_some();
    let mut terminal = setup_terminal(mouse_capture)?;
    let mut selected: usize = 0;
    let mut status: Option<String> = None;

    loop {
        terminal.draw(|f| render(f, selected, status.as_deref()))?;

        if !event::poll(Duration::from_millis(60))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        // Windows delivers both Press and Release key events; without this
        // filter every typed character appears twice.
        if matches!(key.kind, KeyEventKind::Release) {
            continue;
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            restore_terminal(mouse_capture);
            anyhow::bail!("onboarding cancelled by user");
        }

        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                restore_terminal(mouse_capture);
                anyhow::bail!("onboarding cancelled by user");
            }
            (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                selected = selected.saturating_sub(1);
            }
            (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                selected = (selected + 1).min(OAUTH_CHOICES.len().saturating_sub(1));
            }
            (KeyCode::Enter, _) => {
                let choice = OAUTH_CHOICES[selected];
                restore_terminal(mouse_capture);
                let res = crate::oauth_login::login(choice.login).await;
                terminal = setup_terminal(mouse_capture)?;
                match res {
                    Ok(()) => {
                        config.set_default_provider(choice.provider);
                        config.ui.onboarding_completed = true;
                        if let Err(e) = config.save_global() {
                            status =
                                Some(format!("Login succeeded, but saving config failed: {e}"));
                            continue;
                        }
                        restore_terminal(mouse_capture);
                        return Ok(config);
                    }
                    Err(e) => {
                        status = Some(format!("Login failed: {e}"));
                    }
                }
            }
            _ => {}
        }
    }
}

fn render(f: &mut Frame, selected: usize, status: Option<&str>) {
    let area = f.area();
    let popup = centered_rect(74, 12, area);
    f.render_widget(Clear, popup);

    let block = super::render_helpers::popup_block("Sign in");
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines = vec![
        Line::from(Span::styled(
            "Select a provider and press Enter to sign in.",
            Style::default().fg(theme::text()),
        )),
        Line::from(Span::styled(
            "Shown because no provider is signed in yet.",
            Style::default().fg(theme::muted()),
        )),
        Line::from(""),
    ];

    for (idx, c) in OAUTH_CHOICES.iter().enumerate() {
        let active = idx == selected;
        let style = if active {
            Style::default()
                .fg(theme::on_accent())
                .bg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::text())
        };
        let prefix = if active { "> " } else { "  " };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{:<16} — {}", c.title, c.subtitle),
            style,
        )));
    }

    lines.push(Line::from(""));
    match status {
        Some(s) => lines.push(Line::from(Span::styled(
            s,
            Style::default().fg(theme::warn()),
        ))),
        None => lines.push(Line::from(Span::styled(
            "↑↓ select · Enter sign in · Esc cancel",
            Style::default().fg(theme::muted()),
        ))),
    }

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::surface())),
        inner,
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    const POPUP_W_PAD: u16 = 10;
    const POPUP_H_PAD: u16 = 3;
    let width = width.saturating_add(POPUP_W_PAD);
    let height = height.saturating_add(POPUP_H_PAD);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}
