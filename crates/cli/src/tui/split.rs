//! The split workspace: next to the conversation, the dashboard can show up
//! to three more panes — a sub-agent at work, or a side chat (`/btw`
//! questions kept as a thread). The one message box writes to the focused
//! pane: the conversation, the agent (it reads the message at its next
//! step), or the side chat. Ctrl+X then `|` splits right, `-` below, `o` or
//! the arrows move focus, `a` changes what a pane shows, `x` closes it.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap};
use serde::{Deserialize, Serialize};

use crate::tui::state::SubagentRow;
use crate::tui::theme;

/// Panes beside the conversation, at most.
pub const MAX_PANES: usize = 3;

/// What a split key or `/split` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAction {
    Right,
    Below,
    Close,
    Next,
    Prev,
    Show,
}

impl SplitAction {
    /// `/split <word>`.
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word.trim().to_ascii_lowercase().as_str() {
            "" | "right" | "r" | "|" => Self::Right,
            "below" | "down" | "b" | "-" => Self::Below,
            "close" | "x" | "off" => Self::Close,
            "next" | "n" | "o" => Self::Next,
            "prev" | "previous" | "p" => Self::Prev,
            "show" | "a" | "change" => Self::Show,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneKind {
    /// A sub-agent's live activity (by id).
    Agent(String),
    /// The side chat.
    Side,
}

#[derive(Debug, Clone, Default)]
pub struct SplitWorkspace {
    pub panes: Vec<PaneKind>,
    /// Panes go below the conversation instead of to its right.
    pub below: bool,
    /// 0 = the conversation; `i + 1` = `panes[i]`.
    pub focus: usize,
    /// The side chat so far: (question, answer).
    pub side: Vec<(String, String)>,
}

/// What's kept between runs (`~/.dcode-ai/tui.json`): how many panes of
/// each kind, and where.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SplitPrefs {
    #[serde(default)]
    pub agent_panes: u8,
    #[serde(default)]
    pub side_pane: bool,
    #[serde(default)]
    pub below: bool,
}

impl SplitWorkspace {
    pub fn is_split(&self) -> bool {
        !self.panes.is_empty()
    }

    /// The focused pane (None = the conversation).
    pub fn focused(&self) -> Option<&PaneKind> {
        self.focus.checked_sub(1).and_then(|i| self.panes.get(i))
    }

    /// What a new pane shows: a sub-agent not shown yet (running ones
    /// first, newest first), else the side chat if it isn't open, else the
    /// newest agent.
    pub fn next_kind(&self, agents: &[SubagentRow]) -> PaneKind {
        let shown = |id: &str| {
            self.panes
                .iter()
                .any(|p| matches!(p, PaneKind::Agent(a) if a == id))
        };
        let pick = agents
            .iter()
            .rev()
            .filter(|a| !shown(&a.id))
            .max_by_key(|a| a.running)
            .map(|a| PaneKind::Agent(a.id.clone()));
        pick.unwrap_or_else(|| {
            if self.panes.contains(&PaneKind::Side) {
                agents
                    .last()
                    .map(|a| PaneKind::Agent(a.id.clone()))
                    .unwrap_or(PaneKind::Side)
            } else {
                PaneKind::Side
            }
        })
    }

    /// Adds a pane (right, or below) and focuses it. False when full.
    pub fn split(&mut self, below: bool, agents: &[SubagentRow]) -> bool {
        if self.panes.len() >= MAX_PANES {
            return false;
        }
        let kind = self.next_kind(agents);
        self.below = below;
        self.panes.push(kind);
        self.focus = self.panes.len();
        true
    }

    /// Closes the focused pane (the conversation can't be closed).
    pub fn close_focused(&mut self) -> bool {
        let Some(i) = self.focus.checked_sub(1) else {
            return false;
        };
        if i >= self.panes.len() {
            return false;
        }
        self.panes.remove(i);
        self.focus = self.focus.min(self.panes.len());
        true
    }

    /// Moves focus by `step` (wrapping over the conversation and panes).
    pub fn move_focus(&mut self, step: i32) {
        let n = self.panes.len() as i32 + 1;
        self.focus = (self.focus as i32 + step).rem_euclid(n) as usize;
    }

    /// The focused pane shows the next thing: the next sub-agent, then the
    /// side chat, round again.
    pub fn cycle_focused(&mut self, agents: &[SubagentRow]) {
        let Some(i) = self.focus.checked_sub(1) else {
            return;
        };
        let Some(cur) = self.panes.get(i).cloned() else {
            return;
        };
        let mut choices: Vec<PaneKind> = agents
            .iter()
            .map(|a| PaneKind::Agent(a.id.clone()))
            .collect();
        choices.push(PaneKind::Side);
        let at = choices.iter().position(|c| *c == cur).unwrap_or(0);
        self.panes[i] = choices[(at + 1) % choices.len()].clone();
    }

    pub fn prefs(&self) -> SplitPrefs {
        SplitPrefs {
            agent_panes: self
                .panes
                .iter()
                .filter(|p| matches!(p, PaneKind::Agent(_)))
                .count() as u8,
            side_pane: self.panes.contains(&PaneKind::Side),
            below: self.below,
        }
    }

    /// The layout from last time (agent panes fill in as agents start).
    pub fn restore(p: &SplitPrefs) -> Self {
        let mut ws = SplitWorkspace {
            below: p.below,
            ..Default::default()
        };
        if p.side_pane {
            ws.panes.push(PaneKind::Side);
        }
        for _ in 0..p.agent_panes.min(MAX_PANES as u8) {
            if ws.panes.len() < MAX_PANES {
                ws.panes.push(PaneKind::Agent(String::new()));
            }
        }
        ws
    }

    /// Fills placeholder agent panes (restored, id unknown) with agents not
    /// shown yet.
    pub fn fill_placeholders(&mut self, agents: &[SubagentRow]) {
        for i in 0..self.panes.len() {
            if self.panes[i] == PaneKind::Agent(String::new()) {
                let next = self.next_kind(agents);
                if let PaneKind::Agent(id) = next
                    && !id.is_empty()
                {
                    self.panes[i] = PaneKind::Agent(id);
                }
            }
        }
    }
}

/// The conversation's area and each pane's, for `area`.
pub fn layout(area: Rect, ws: &SplitWorkspace) -> (Rect, Vec<Rect>) {
    if ws.panes.is_empty() {
        return (area, Vec::new());
    }
    let dir = if ws.below {
        Direction::Vertical
    } else {
        Direction::Horizontal
    };
    // The conversation keeps the larger share.
    let [main, rest] = Layout::default()
        .direction(dir)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .areas(area);
    let n = ws.panes.len() as u32;
    let panes = Layout::default()
        .direction(if ws.below {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints((0..n).map(|_| Constraint::Ratio(1, n)))
        .split(rest)
        .to_vec();
    (main, panes)
}

fn pane_block(title: String, focused: bool) -> Block<'static> {
    let color = if focused {
        theme::accent()
    } else {
        theme::border()
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1))
        .style(Style::default().bg(theme::bg()))
}

fn short(id: &str) -> String {
    let seg = id
        .split('-')
        .filter(|s| s.chars().any(|c| c.is_ascii_digit()))
        .max_by_key(|s| s.len())
        .unwrap_or(id);
    let n = seg.chars().count();
    seg.chars().skip(n.saturating_sub(8)).collect()
}

/// A pane's lines: the agent's task, state and recent activity, or the side
/// chat thread with the answer being written.
pub fn pane_lines(
    kind: &PaneKind,
    agents: &[SubagentRow],
    side: &[(String, String)],
    live: Option<(&str, &str)>,
    focused: bool,
) -> (String, Vec<Line<'static>>) {
    let muted = |t: String| Line::from(Span::styled(t, Style::default().fg(theme::muted())));
    match kind {
        PaneKind::Agent(id) => {
            let Some(a) = agents.iter().find(|a| &a.id == id) else {
                return (
                    "agent".into(),
                    vec![
                        muted("waiting for a sub-agent…".into()),
                        muted("Ctrl+X a shows something else".into()),
                    ],
                );
            };
            let state = if a.running {
                "● working"
            } else {
                "○ finished"
            };
            let mut lines = vec![
                Line::from(Span::styled(
                    a.task.clone(),
                    Style::default()
                        .fg(theme::text())
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(vec![
                    Span::styled(
                        state.to_string(),
                        Style::default().fg(if a.running {
                            theme::accent()
                        } else {
                            theme::success()
                        }),
                    ),
                    Span::styled(
                        format!("  {} {}", a.phase, a.detail),
                        Style::default().fg(theme::muted()),
                    ),
                ]),
                Line::from(""),
            ];
            lines.extend(a.log.iter().map(|l| Line::from(format!("· {l}"))));
            if focused {
                lines.push(Line::from(""));
                lines.push(muted(if a.running {
                    "type to message this agent".into()
                } else {
                    "finished · /agents to continue it".into()
                }));
            }
            (format!("◇ {}", short(id)), lines)
        }
        PaneKind::Side => {
            let mut lines = Vec::new();
            if side.is_empty() && live.is_none() {
                lines.push(muted("side chat — questions about the work,".into()));
                lines.push(muted("answered without interrupting it".into()));
            }
            let mut qa = |q: &str, a: &str| {
                lines.push(Line::from(Span::styled(
                    format!("> {q}"),
                    Style::default().fg(theme::user()),
                )));
                for l in a.lines() {
                    lines.push(Line::from(l.to_string()));
                }
                lines.push(Line::from(""));
            };
            for (q, a) in side {
                qa(q, a);
            }
            if let Some((q, a)) = live {
                qa(q, if a.is_empty() { "…" } else { a });
            }
            if focused {
                lines.push(muted("type a side question".into()));
            }
            ("side chat".into(), lines)
        }
    }
}

/// Draws pane `kind` in `rect`, showing its newest lines.
pub fn render_pane(
    frame: &mut ratatui::Frame<'_>,
    rect: Rect,
    kind: &PaneKind,
    agents: &[SubagentRow],
    side: &[(String, String)],
    live: Option<(&str, &str)>,
    focused: bool,
) {
    let (title, lines) = pane_lines(kind, agents, side, live, focused);
    let block = pane_block(title, focused);
    let inner = block.inner(rect);
    // Keep the bottom in view: estimate wrapped rows and scroll past the rest.
    let w = inner.width.max(1) as usize;
    let rows: usize = lines.iter().map(|l| l.width().max(1).div_ceil(w)).sum();
    let scroll = rows.saturating_sub(inner.height as usize) as u16;
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, running: bool) -> SubagentRow {
        SubagentRow {
            id: id.into(),
            task: format!("task {id}"),
            phase: String::new(),
            detail: String::new(),
            running,
            skill: None,
            log: vec!["read_file src/a.rs".into()],
        }
    }

    #[test]
    fn split_words_parse() {
        assert_eq!(SplitAction::parse(""), Some(SplitAction::Right));
        assert_eq!(SplitAction::parse("below"), Some(SplitAction::Below));
        assert_eq!(SplitAction::parse("CLOSE"), Some(SplitAction::Close));
        assert_eq!(SplitAction::parse("next"), Some(SplitAction::Next));
        assert_eq!(SplitAction::parse("show"), Some(SplitAction::Show));
        assert_eq!(SplitAction::parse("sideways"), None);
    }

    #[test]
    fn splits_pick_agents_then_the_side_chat_and_stop_at_three() {
        let agents = vec![agent("a", false), agent("b", true)];
        let mut ws = SplitWorkspace::default();
        assert!(ws.split(false, &agents));
        assert_eq!(ws.panes[0], PaneKind::Agent("b".into()), "running first");
        assert!(ws.split(false, &agents));
        assert_eq!(ws.panes[1], PaneKind::Agent("a".into()));
        assert!(ws.split(true, &agents));
        assert_eq!(ws.panes[2], PaneKind::Side);
        assert!(ws.below);
        assert_eq!(ws.focus, 3);
        assert!(!ws.split(false, &agents), "at most three");
    }

    #[test]
    fn focus_moves_wraps_and_closing_keeps_it_valid() {
        let mut ws = SplitWorkspace::default();
        ws.split(false, &[]);
        ws.split(false, &[agent("a", true)]);
        assert_eq!(ws.focus, 2);
        ws.move_focus(1);
        assert_eq!(ws.focus, 0, "wraps to the conversation");
        assert!(!ws.close_focused(), "the conversation stays");
        ws.move_focus(-1);
        assert!(ws.close_focused());
        assert_eq!(ws.panes, vec![PaneKind::Side]);
        assert_eq!(ws.focus, 1);
        assert_eq!(ws.focused(), Some(&PaneKind::Side));
    }

    #[test]
    fn a_pane_cycles_through_agents_and_the_side_chat() {
        let agents = vec![agent("a", true), agent("b", true)];
        let mut ws = SplitWorkspace {
            panes: vec![PaneKind::Agent("a".into())],
            focus: 1,
            ..Default::default()
        };
        ws.cycle_focused(&agents);
        assert_eq!(ws.panes[0], PaneKind::Agent("b".into()));
        ws.cycle_focused(&agents);
        assert_eq!(ws.panes[0], PaneKind::Side);
        ws.cycle_focused(&agents);
        assert_eq!(ws.panes[0], PaneKind::Agent("a".into()));
    }

    #[test]
    fn the_layout_is_kept_and_restored() {
        let mut ws = SplitWorkspace::default();
        ws.split(true, &[agent("a", true)]);
        ws.split(true, &[agent("a", true)]);
        let p = ws.prefs();
        assert_eq!(
            p,
            SplitPrefs {
                agent_panes: 1,
                side_pane: true,
                below: true
            }
        );
        let mut back = SplitWorkspace::restore(&p);
        assert_eq!(back.panes.len(), 2);
        back.fill_placeholders(&[agent("z", true)]);
        assert!(back.panes.contains(&PaneKind::Agent("z".into())));
    }

    #[test]
    fn panes_share_the_area_with_the_conversation() {
        let area = Rect::new(0, 0, 100, 40);
        let mut ws = SplitWorkspace::default();
        assert_eq!(layout(area, &ws).0, area);
        ws.split(false, &[]);
        ws.split(false, &[agent("a", true)]);
        let (main, panes) = layout(area, &ws);
        assert_eq!(panes.len(), 2);
        assert!(main.width >= 50 && main.height == 40);
        assert_eq!(panes[0].x, main.width);
        assert!(panes[1].y > panes[0].y);
    }

    #[test]
    fn an_agent_pane_shows_its_task_and_activity() {
        let (title, lines) = pane_lines(
            &PaneKind::Agent("session-1791887456044093-0".into()),
            &[agent("session-1791887456044093-0", true)],
            &[],
            None,
            true,
        );
        assert_eq!(title, "◇ 56044093");
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert!(text.iter().any(|l| l.contains("read_file src/a.rs")));
        assert!(
            text.iter()
                .any(|l| l.contains("type to message this agent"))
        );
    }
}
