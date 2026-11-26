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
    /// The sign-in to run (`None`: nothing to sign in to — a local model).
    login: Option<OAuthProvider>,
    provider: ProviderKind,
}

/// Every choice is a sign-in flow; no API keys are pasted.
const OAUTH_CHOICES: &[OAuthChoice] = &[
    OAuthChoice {
        title: "Claude",
        subtitle: "Your Claude account via Claude Code (`claude`)",
        login: Some(OAuthProvider::Anthropic),
        provider: ProviderKind::Anthropic,
    },
    OAuthChoice {
        title: "GitHub Copilot",
        subtitle: "GitHub device login",
        login: Some(OAuthProvider::Copilot),
        provider: ProviderKind::Copilot,
    },
    OAuthChoice {
        title: "Local model",
        subtitle: "Free, on this machine: Ollama or LM Studio",
        login: None,
        provider: ProviderKind::Local,
    },
];

/// What a row can do right now, checked when the picker opens and after
/// every attempt.
#[derive(Clone, Debug, PartialEq)]
enum RowState {
    /// Signed in (or, for a local model, a server answers): Enter uses it.
    SignedIn,
    /// Enter starts the sign-in.
    Ready,
    /// Can't work yet; the text says how to fix it.
    Blocked(String),
}

impl RowState {
    fn label(&self, local: bool) -> &'static str {
        match self {
            RowState::SignedIn if local => "✓ running",
            RowState::SignedIn => "✓ signed in",
            RowState::Ready => "sign in",
            RowState::Blocked(_) => "not set up",
        }
    }
}

async fn row_states() -> Vec<RowState> {
    let mut out = Vec::with_capacity(OAUTH_CHOICES.len());
    for c in OAUTH_CHOICES {
        out.push(match c.login {
            Some(OAuthProvider::Anthropic) => {
                if crate::oauth_login::anthropic_ready() {
                    RowState::SignedIn
                } else if let Some(why) = crate::oauth_login::claude_login_blocker() {
                    RowState::Blocked(why.to_string())
                } else {
                    RowState::Ready
                }
            }
            Some(OAuthProvider::Copilot) => {
                if crate::oauth_login::copilot_signed_in() {
                    RowState::SignedIn
                } else {
                    RowState::Ready
                }
            }
            None => {
                if dcode_ai_core::provider::local::local_server_running().await {
                    RowState::SignedIn
                } else {
                    RowState::Blocked(LOCAL_HINT.to_string())
                }
            }
        });
    }
    out
}

const LOCAL_HINT: &str = "no Ollama or LM Studio running — install Ollama (ollama.com), run `ollama pull qwen3-coder`, then choose this again";

/// The row the cursor starts on: one already signed in, else one that can
/// sign in now — never a row that can only fail when another would work.
fn first_usable(states: &[RowState]) -> usize {
    states
        .iter()
        .position(|s| *s == RowState::SignedIn)
        .or_else(|| states.iter().position(|s| *s == RowState::Ready))
        .unwrap_or(0)
}

pub async fn run_onboarding(mut config: DcodeAiConfig) -> anyhow::Result<DcodeAiConfig> {
    // Onboarding can run before the main REPL initializes the theme from
    // config, so apply the user's configured theme now.
    super::theme::set_by_name(config.ui.theme.as_deref());
    // In tmux, keep mouse capture on by default so wheel scrolling is reliable.
    let mouse_capture = config.ui.mouse_capture || std::env::var_os("TMUX").is_some();
    let mut terminal = setup_terminal(mouse_capture)?;
    let mut states = row_states().await;
    let mut selected = first_usable(&states);
    let mut status: Option<String> = None;

    loop {
        terminal.draw(|f| render(f, selected, &states, status.as_deref()))?;

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
            (KeyCode::Char(d @ '1'..='9'), _)
                if (d as usize - '1' as usize) < OAUTH_CHOICES.len() =>
            {
                selected = d as usize - '1' as usize;
                status = None;
            }
            (KeyCode::Enter, _) => {
                let choice = OAUTH_CHOICES[selected];
                let res = match choice.login {
                    // Nothing can start: say how to fix it, stay in the picker.
                    Some(_) if matches!(states[selected], RowState::Blocked(_)) => {
                        // Re-check: the user may have installed it meanwhile.
                        states = row_states().await;
                        match &states[selected] {
                            RowState::Blocked(why) => Err(anyhow::anyhow!("{why}")),
                            _ => continue,
                        }
                    }
                    Some(login) => {
                        restore_terminal(mouse_capture);
                        let r = crate::oauth_login::login(login).await;
                        terminal = setup_terminal(mouse_capture)?;
                        r
                    }
                    // Nothing to sign in to: is a server running?
                    None if dcode_ai_core::provider::local::local_server_running().await => Ok(()),
                    None => Err(anyhow::anyhow!(LOCAL_HINT)),
                };
                match res {
                    Ok(()) => {
                        config.set_default_provider(choice.provider);
                        config.ui.onboarding_completed = true;
                        let provider = choice.provider;
                        if let Err(e) = DcodeAiConfig::update_global(|g| {
                            g.set_default_provider(provider);
                            g.ui.onboarding_completed = true;
                        }) {
                            status =
                                Some(format!("Login succeeded, but saving config failed: {e}"));
                            continue;
                        }
                        restore_terminal(mouse_capture);
                        return Ok(config);
                    }
                    Err(e) => {
                        let blocked = matches!(states[selected], RowState::Blocked(_));
                        status = Some(if choice.login.is_some() && !blocked {
                            format!("Sign-in failed: {e}")
                        } else {
                            e.to_string()
                        });
                        states = row_states().await;
                    }
                }
            }
            _ => {}
        }
    }
}

fn render(f: &mut Frame, selected: usize, states: &[RowState], status: Option<&str>) {
    let area = f.area();
    let popup = centered_rect(84, 10, area);
    f.render_widget(Clear, popup);

    let block = super::render_helpers::popup_block("Sign in to dcode-ai");
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let muted = Style::default().fg(theme::muted());
    let mut lines = vec![
        Line::from(Span::styled(
            "Choose where the model runs. You can switch any time with /model.",
            Style::default().fg(theme::text()),
        )),
        Line::from(""),
    ];

    for (idx, c) in OAUTH_CHOICES.iter().enumerate() {
        let active = idx == selected;
        let state = states.get(idx).cloned().unwrap_or(RowState::Ready);
        let title = if active {
            Style::default()
                .fg(theme::on_accent())
                .bg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::text())
                .add_modifier(Modifier::BOLD)
        };
        let state_style = match state {
            RowState::SignedIn => Style::default().fg(theme::success()),
            RowState::Ready => Style::default().fg(theme::accent()),
            RowState::Blocked(_) => Style::default().fg(theme::warn()),
        };
        let prefix = if active { "›" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(format!("{prefix} {} {:<16}", idx + 1, c.title), title),
            Span::raw("  "),
            Span::styled(
                format!("{:<12}", state.label(c.login.is_none())),
                state_style,
            ),
            Span::styled(c.subtitle, muted),
        ]));
    }

    lines.push(Line::from(""));
    // The selected row's fix, else the last attempt's result, else keys.
    let note = match (status, states.get(selected)) {
        (Some(s), _) => Some((s.to_string(), theme::warn())),
        (None, Some(RowState::Blocked(why))) => Some((why.clone(), theme::warn())),
        _ => None,
    };
    if let Some((text, color)) = note {
        lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        "↑↓ or 1–3 select · Enter continue · Esc cancel",
        muted,
    )));

    f.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .style(Style::default().bg(theme::surface())),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cursor_starts_on_a_row_that_can_work() {
        let b = || RowState::Blocked("x".into());
        // Fresh machine without `claude`: Copilot, not a Claude row that fails.
        assert_eq!(first_usable(&[b(), RowState::Ready, b()]), 1);
        // Already signed in somewhere: that one.
        assert_eq!(
            first_usable(&[RowState::Ready, RowState::Ready, RowState::SignedIn]),
            2
        );
        assert_eq!(first_usable(&[b(), b(), b()]), 0);
    }
}
