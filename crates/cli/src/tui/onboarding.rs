//! OAuth-first onboarding TUI shown before runtime startup when provider auth is missing.

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use dcode_ai_common::config::{DcodeAiConfig, ProviderKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::oauth_login::OAuthProvider;

use super::terminal::{restore_terminal, setup_terminal};

#[derive(Clone, Copy)]
enum ChoiceAction {
    OAuth(OAuthProvider),
    /// "Use a Google Cloud project": Vertex AI via gcloud ADC — prompts for
    /// the project id instead of an OAuth flow.
    VertexProject,
}

#[derive(Clone, Copy)]
struct OAuthChoice {
    title: &'static str,
    subtitle: &'static str,
    action: ChoiceAction,
    runtime_provider: Option<ProviderKind>,
}

/// Vertex first (premium Google Cloud accounts), then alphabetical. No
/// provider is implied as the default — the user picks.
const OAUTH_CHOICES: &[OAuthChoice] = &[
    OAuthChoice {
        title: "Antigravity Vertex",
        subtitle: "Your Google Cloud project via gcloud ADC (Vertex AI)",
        action: ChoiceAction::VertexProject,
        runtime_provider: Some(ProviderKind::Antigravity),
    },
    OAuthChoice {
        title: "Anthropic",
        subtitle: "OAuth PKCE login",
        action: ChoiceAction::OAuth(OAuthProvider::Anthropic),
        runtime_provider: Some(ProviderKind::Anthropic),
    },
    OAuthChoice {
        title: "Antigravity",
        subtitle: "Gemini 3 Pro via Google Cloud Code Assist (OAuth)",
        action: ChoiceAction::OAuth(OAuthProvider::Antigravity),
        runtime_provider: Some(ProviderKind::Antigravity),
    },
    OAuthChoice {
        title: "Copilot",
        subtitle: "GitHub device login",
        action: ChoiceAction::OAuth(OAuthProvider::Copilot),
        runtime_provider: Some(ProviderKind::OpenAi),
    },
    OAuthChoice {
        title: "MiniMax (OpenCode Zen)",
        subtitle: "MiniMax M2.5, Kimi, GLM models",
        action: ChoiceAction::OAuth(OAuthProvider::Opencodezen),
        runtime_provider: Some(ProviderKind::OpenCodeZen),
    },
    OAuthChoice {
        title: "OpenAI",
        subtitle: "OAuth device login",
        action: ChoiceAction::OAuth(OAuthProvider::Openai),
        runtime_provider: Some(ProviderKind::OpenAi),
    },
];

pub async fn run_onboarding(mut config: DcodeAiConfig) -> anyhow::Result<DcodeAiConfig> {
    // In tmux, keep mouse capture on by default so wheel scrolling is reliable.
    let mouse_capture = config.ui.mouse_capture || std::env::var_os("TMUX").is_some();
    let mut terminal = setup_terminal(mouse_capture)?;
    let mut selected: usize = 0;
    let mut status: Option<String> = None;
    // Inline text prompt for the Vertex project id (no OAuth flow).
    let mut project_input: Option<String> = None;

    loop {
        terminal.draw(|f| render(f, selected, status.as_deref(), project_input.as_deref()))?;

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

        // Project-id input mode (after picking "Antigravity Vertex").
        if let Some(input) = project_input.as_mut() {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    project_input = None;
                    status = None;
                }
                (KeyCode::Backspace, _) => {
                    input.pop();
                }
                (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                    input.push(c);
                }
                (KeyCode::Enter, _) => {
                    let project = input.trim().to_string();
                    if project.is_empty() {
                        status = Some("Enter your GCP project id (Esc to go back).".into());
                        continue;
                    }
                    status = Some("Checking gcloud Application Default Credentials…".into());
                    terminal.draw(|f| {
                        render(f, selected, status.as_deref(), project_input.as_deref())
                    })?;
                    let probe = tokio::task::spawn_blocking(
                        dcode_ai_core::provider::antigravity::adc_access_token,
                    )
                    .await;
                    match probe {
                        Ok(Ok(_token)) => {
                            let mut store =
                                dcode_ai_common::auth::AuthStore::load().unwrap_or_default();
                            store.vertex = Some(dcode_ai_common::auth::VertexAuth {
                                project_id: project,
                                location: dcode_ai_common::auth::default_vertex_location(),
                            });
                            store.preferred_provider =
                                Some(dcode_ai_common::auth::LoggedProvider::Antigravity);
                            if let Err(e) = store.save() {
                                status = Some(format!("Failed to save auth: {e}"));
                                continue;
                            }
                            config.set_default_provider(ProviderKind::Antigravity);
                            if !config
                                .provider
                                .openai
                                .model
                                .to_ascii_lowercase()
                                .contains("gemini")
                            {
                                config.provider.openai.model = "gemini-2.5-flash".to_string();
                            }
                            config.sync_default_model_from_provider();
                            config.ui.onboarding_completed = true;
                            if let Err(e) = config.save_global() {
                                status = Some(format!("Connected, but saving config failed: {e}"));
                                continue;
                            }
                            restore_terminal(mouse_capture);
                            return Ok(config);
                        }
                        Ok(Err(e)) => {
                            status = Some(format!("{e}"));
                        }
                        Err(e) => {
                            status = Some(format!("ADC probe failed: {e}"));
                        }
                    }
                }
                _ => {}
            }
            continue;
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
                let oauth = match choice.action {
                    ChoiceAction::VertexProject => {
                        project_input = Some(String::new());
                        status = None;
                        continue;
                    }
                    ChoiceAction::OAuth(o) => o,
                };
                restore_terminal(mouse_capture);
                let res = crate::oauth_login::login(oauth).await;
                terminal = setup_terminal(mouse_capture)?;

                match res {
                    Ok(()) => {
                        if let Some(p) = choice.runtime_provider {
                            config.set_default_provider(p);
                            if matches!(oauth, OAuthProvider::Copilot) {
                                config.provider.openai.base_url =
                                    "https://api.githubcopilot.com".to_string();
                            }
                            // Antigravity shares the `openai` model slot; default
                            // to a Gemini model so the Google backend accepts it.
                            if matches!(oauth, OAuthProvider::Antigravity)
                                && !config
                                    .provider
                                    .openai
                                    .model
                                    .to_ascii_lowercase()
                                    .contains("gemini")
                            {
                                config.provider.openai.model = "gemini-3-pro".to_string();
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

fn render(f: &mut Frame, selected: usize, status: Option<&str>, project_input: Option<&str>) {
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

    if let Some(input) = project_input {
        lines.push(Line::from(Span::styled(
            "Google Cloud project (Vertex AI)",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "Requires: gcloud CLI + `gcloud auth application-default login`",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Project id: ", Style::default().fg(Color::White)),
            Span::styled(
                format!("{input}▏"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    } else {
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
                format!("{prefix}{:<22} — {}", c.title, c.subtitle),
                style,
            )));
        }
    }

    lines.push(Line::from(""));
    if let Some(s) = status {
        lines.push(Line::from(Span::styled(
            s,
            Style::default().fg(Color::Yellow),
        )));
    } else {
        let hint = if project_input.is_some() {
            "Enter connect · Esc back"
        } else {
            "↑↓ select · Enter login · Esc cancel"
        };
        lines.push(Line::from(Span::styled(
            hint,
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
