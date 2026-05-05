//! OAuth-first onboarding TUI shown before runtime startup when provider auth is missing.

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use dcode_ai_common::config::{DcodeAiConfig, ProviderKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::oauth_login::OAuthProvider;

use super::app::{restore_terminal, setup_terminal};

#[derive(Clone, Copy)]
struct OAuthChoice {
    title: &'static str,
    subtitle: &'static str,
    oauth: OAuthProvider,
    runtime_provider: Option<ProviderKind>,
}

const OAUTH_CHOICES: &[OAuthChoice] = &[
    OAuthChoice {
        title: "OpenAI",
        subtitle: "OAuth device login",
        oauth: OAuthProvider::Openai,
        runtime_provider: Some(ProviderKind::OpenAi),
    },
    OAuthChoice {
        title: "Anthropic",
        subtitle: "OAuth PKCE login",
        oauth: OAuthProvider::Anthropic,
        runtime_provider: Some(ProviderKind::Anthropic),
    },
    OAuthChoice {
        title: "Copilot",
        subtitle: "GitHub device login",
        oauth: OAuthProvider::Copilot,
        runtime_provider: Some(ProviderKind::OpenAi),
    },
    OAuthChoice {
        title: "Antigravity",
        subtitle: "Google OAuth login (login only)",
        oauth: OAuthProvider::Antigravity,
        runtime_provider: None,
    },
    OAuthChoice {
        title: "OpenCode Zen",
        subtitle: "Big Pickle, Kimi, GLM models (free)",
        oauth: OAuthProvider::Opencodezen,
        runtime_provider: Some(ProviderKind::OpenCodeZen),
    },
];

pub async fn run_onboarding(mut config: DcodeAiConfig) -> anyhow::Result<DcodeAiConfig> {
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

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            restore_terminal(mouse_capture);
            anyhow::bail!("onboarding cancelled by user");
        }

        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                restore_terminal(mouse_capture);
                anyhow::bail!("onboarding cancelled by user");
            }
            (KeyCode::Up, _) => {
                selected = selected.saturating_sub(1);
            }
            (KeyCode::Down, _) => {
                selected = (selected + 1).min(OAUTH_CHOICES.len().saturating_sub(1));
            }
            (KeyCode::Enter, _) => {
                let choice = OAUTH_CHOICES[selected];
                restore_terminal(mouse_capture);
                let res = crate::oauth_login::login(choice.oauth).await;
                terminal = setup_terminal(mouse_capture)?;

                match res {
                    Ok(()) => {
                        if let Some(p) = choice.runtime_provider {
                            config.set_default_provider(p);
                            if matches!(choice.oauth, OAuthProvider::Copilot) {
                                config.provider.openai.base_url =
                                    "https://api.githubcopilot.com".to_string();
                                if config.provider.openai.model.trim().is_empty() {
                                    config.provider.openai.model = "gpt-4o".to_string();
                                }
                            }
                            config.ui.onboarding_completed = true;
                            if let Err(e) = config.save_global() {
                                status =
                                    Some(format!("Login succeeded, but saving config failed: {e}"));
                                continue;
                            }
                            restore_terminal(mouse_capture);
                            return Ok(config);
                        }
                        status = Some(
                            "Login saved. Runtime provider can be set with /provider.".to_string(),
                        );
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
    let popup = centered_rect(74, 18, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Connect Provider (OAuth) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines = vec![
        Line::from(Span::styled(
            "Select a provider and press Enter to login.",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "This popup appears because no active provider auth is configured.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];

    for (idx, c) in OAUTH_CHOICES.iter().enumerate() {
        let active = idx == selected;
        let style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let prefix = if active { "> " } else { "  " };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{:<12} — {}", c.title, c.subtitle),
            style,
        )));
    }

    lines.push(Line::from(""));
    if let Some(s) = status {
        lines.push(Line::from(Span::styled(
            s,
            Style::default().fg(Color::Yellow),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "↑↓ select · Enter login · Esc cancel",
            Style::default().fg(Color::DarkGray),
        )));
    }

    f.render_widget(Paragraph::new(lines), inner);
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
