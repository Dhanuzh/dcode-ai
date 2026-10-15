//! Claude-style decision prompts drawn in place of the composer: tool
//! permission, `/rewind` (message, then scope) and plan → build.
//!
//! ```text
//! ╭ Edit file ─────────────────────────────────╮
//! │  a.txt | +1 -1                              │
//! │  - line 11                                  │
//! │  + line ELEVEN                              │
//! │                                             │
//! │ Do you want to make this edit to a.txt?     │
//! │ ❯ 1. Yes                                    │
//! │   2. Yes, and don't ask again for edit:*    │
//! │   3. No, and tell dcode-ai what to do (esc) │
//! ╰─────────────────────────────────────────────╯
//! ```

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear as ClearWidget, Padding, Paragraph},
};

use crate::tool_ui;
use crate::tui::render::modals::approval_hunk_lines;
use crate::tui::render::tool_detail::{approval_diff_preview, tool_input_path, tool_input_preview};
use crate::tui::render_helpers::truncate_chars;
use crate::tui::state::{ApprovalRequest, RewindScope, TuiSessionState};
use crate::tui::theme;

/// Options shown when a plan-mode turn finishes.
pub(crate) const PLAN_EXIT_OPTIONS: [&str; 3] = [
    "Yes, and auto-accept edits",
    "Yes, and manually approve edits",
    "No, keep planning",
];

pub(crate) struct PromptBox {
    pub title: String,
    pub color: Color,
    pub lines: Vec<Line<'static>>,
}

/// The prompt that currently owns the composer area, if any.
pub(crate) fn active_prompt_box(g: &TuiSessionState, inner_w: usize) -> Option<PromptBox> {
    if let Some(req) = &g.active_approval {
        return Some(approval_box(g, req, inner_w));
    }
    if g.theme_picker_open {
        return Some(theme_box(g, inner_w));
    }
    if let Some(pending) = &g.rewind_pending {
        let mut lines = vec![
            muted("Restore the code and/or conversation to before:"),
            Line::from(Span::styled(
                format!(
                    "> {}",
                    truncate_chars(one_line(&pending.text).as_str(), inner_w.saturating_sub(4))
                ),
                Style::default().fg(theme::text()),
            )),
            Line::default(),
        ];
        let labels: Vec<String> = RewindScope::ALL
            .iter()
            .map(|s| s.label().to_string())
            .collect();
        lines.extend(numbered_options(&labels, pending.index, inner_w));
        lines.push(Line::default());
        lines.push(muted("Enter or 1-3 to choose · Esc cancel"));
        return Some(PromptBox {
            title: "Rewind".into(),
            color: theme::accent(),
            lines,
        });
    }
    if g.backtrack_open {
        let labels = g.backtrack_labels();
        let mut lines = vec![
            muted("Pick a message to rewind to (newest first):"),
            Line::default(),
        ];
        const VISIBLE: usize = 8;
        let start = g
            .backtrack_index
            .saturating_sub(VISIBLE - 1)
            .min(labels.len().saturating_sub(VISIBLE));
        for (i, label) in labels.iter().enumerate().skip(start).take(VISIBLE) {
            lines.push(option_line(
                i == g.backtrack_index,
                &format!("> {label}"),
                inner_w,
            ));
        }
        if labels.len() > VISIBLE {
            lines.push(muted(format!("  {} messages", labels.len())));
        }
        lines.push(Line::default());
        lines.push(muted("↑↓ select · Enter choose · Esc cancel"));
        return Some(PromptBox {
            title: "Rewind".into(),
            color: theme::accent(),
            lines,
        });
    }
    if let Some(index) = g.plan_exit_index {
        let mut lines = vec![
            muted("dcode-ai has finished planning. Start coding?"),
            Line::default(),
        ];
        let labels: Vec<String> = PLAN_EXIT_OPTIONS.iter().map(|s| s.to_string()).collect();
        lines.extend(numbered_options(&labels, index, inner_w));
        lines.push(Line::default());
        lines.push(muted("Enter or 1-3 to choose · Esc keep planning"));
        return Some(PromptBox {
            title: "Ready to code?".into(),
            color: theme::accent(),
            lines,
        });
    }
    None
}

/// `/theme`: Claude-style list with a live preview of the selected theme.
fn theme_box(g: &TuiSessionState, inner_w: usize) -> PromptBox {
    let saved = g.theme_picker_original.as_deref().unwrap_or("");
    let mut lines = vec![Line::from(Span::styled(
        "Choose the text style that looks best with your terminal",
        Style::default().fg(theme::text()),
    ))];
    for (i, name) in g.theme_picker_entries.iter().enumerate() {
        let t = theme::resolve(Some(name));
        let mut label = format!("{}. {}", i + 1, t.label);
        if t.name == saved {
            label.push_str(" ✔");
        }
        lines.push(option_line(i == g.theme_picker_index, &label, inner_w));
    }
    lines.extend(theme_preview(inner_w));
    lines.push(muted(
        "↑↓ preview · Enter select · Esc cancel · /theme <name> for more",
    ));
    PromptBox {
        title: "Theme".into(),
        color: theme::accent(),
        lines,
    }
}

/// A small diff and status sample rendered in the active theme.
fn theme_preview(inner_w: usize) -> Vec<Line<'static>> {
    let w = inner_w.saturating_sub(2).max(20);
    let row = |num: &str, sign: &str, text: &str, fg: Color, bg: Color| {
        let body = format!(" {num} {sign} {text}");
        let pad = w.saturating_sub(body.chars().count());
        Line::from(Span::styled(
            format!("{body}{}", " ".repeat(pad)),
            Style::default().fg(fg).bg(bg),
        ))
    };
    vec![
        row("1", " ", "fn greet() {", theme::muted(), Color::Reset),
        row(
            "2",
            "-",
            "    println!(\"Hello, World!\");",
            theme::error(),
            theme::diff_del(),
        ),
        row(
            "2",
            "+",
            "    println!(\"Hello, dcode-ai!\");",
            theme::success(),
            theme::diff_add(),
        ),
        row("3", " ", "}", theme::muted(), Color::Reset),
    ]
}

fn approval_box(g: &TuiSessionState, req: &ApprovalRequest, inner_w: usize) -> PromptBox {
    let ui = tool_ui::metadata(&req.tool);
    let path = tool_input_path(&req.tool, &req.input);
    let is_edit = matches!(ui.family, "edit");
    let creating = req.tool == "write_file"
        && path
            .as_deref()
            .is_some_and(|p| !g.workspace_root.join(p).exists());
    let title = match (ui.family, req.tool.as_str()) {
        ("shell", _) => "Bash command".to_string(),
        (_, "write_file") if creating => "Create file".to_string(),
        ("edit", _) => "Edit file".to_string(),
        _ => ui.label.clone(),
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    if g.approval_hunk_mode && !g.approval_hunk_selection.is_empty() {
        lines.extend(approval_hunk_lines(
            req,
            &g.approval_hunk_selection,
            g.approval_hunk_cursor,
            inner_w,
        ));
        return PromptBox {
            title: format!("{title} · choose hunks"),
            color: theme::warn(),
            lines,
        };
    }

    let diff = approval_diff_preview(&req.tool, &req.input, inner_w.saturating_sub(2));
    if !diff.is_empty() {
        for l in diff {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(l.spans);
            lines.push(Line::from(spans));
        }
    } else if ui.family == "shell" {
        let cmd = tool_input_preview(&req.tool, &req.input);
        let cmd = cmd.strip_prefix("cmd: ").unwrap_or(&cmd).to_string();
        lines.push(Line::from(Span::styled(
            format!("  {}", truncate_chars(&cmd, inner_w.saturating_sub(4))),
            Style::default().fg(theme::text()),
        )));
        if !req.description.trim().is_empty() && req.description.trim() != cmd.trim() {
            lines.push(muted(format!(
                "  {}",
                truncate_chars(req.description.trim(), inner_w.saturating_sub(4))
            )));
        }
    } else {
        let preview = tool_input_preview(&req.tool, &req.input);
        let text = if preview.is_empty() {
            req.description.clone()
        } else {
            preview
        };
        lines.push(Line::from(Span::styled(
            format!(
                "  {}",
                truncate_chars(text.trim(), inner_w.saturating_sub(4))
            ),
            Style::default().fg(theme::text()),
        )));
    }
    lines.push(Line::default());

    let file = path.unwrap_or_else(|| "this file".into());
    let question = if creating {
        format!("Do you want to create {file}?")
    } else if is_edit {
        format!("Do you want to make this edit to {file}?")
    } else {
        "Do you want to proceed?".to_string()
    };
    lines.push(Line::from(Span::styled(
        truncate_chars(&question, inner_w),
        Style::default().fg(theme::text()),
    )));
    let options = vec![
        "Yes".to_string(),
        format!("Yes, and don't ask again for {}", req.allow_pattern()),
        "No, and tell dcode-ai what to do differently (esc)".to_string(),
    ];
    lines.extend(numbered_options(
        &options,
        g.approval_option_index.min(2),
        inner_w,
    ));
    if is_edit {
        lines.push(muted("h review hunks"));
    }
    PromptBox {
        title,
        color: theme::warn(),
        lines,
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn muted(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(theme::muted()),
    ))
}

fn option_line(selected: bool, label: &str, inner_w: usize) -> Line<'static> {
    let text = truncate_chars(label, inner_w.saturating_sub(3));
    if selected {
        Line::from(vec![
            Span::styled("❯ ", Style::default().fg(theme::accent())),
            Span::styled(
                text,
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    } else {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(text, Style::default().fg(theme::text())),
        ])
    }
}

fn numbered_options(labels: &[String], selected: usize, inner_w: usize) -> Vec<Line<'static>> {
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| option_line(i == selected, &format!("{}. {l}", i + 1), inner_w))
        .collect()
}

/// Height the box needs for `pb` (content + borders), capped at `max`.
pub(crate) fn prompt_box_height(pb: &PromptBox, max: u16) -> u16 {
    (pb.lines.len() as u16 + 1).min(max)
}

pub(crate) fn render_prompt_box(frame: &mut ratatui::Frame<'_>, area: Rect, pb: PromptBox) {
    frame.render_widget(ClearWidget, area);
    let block = Block::default()
        // Claude's permission prompts: one rule on top, no box.
        .borders(Borders::TOP)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(pb.color))
        .title(Span::styled(
            format!(" {} ", pb.title),
            Style::default().fg(pb.color).add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1))
        .style(Style::default().bg(theme::bg()));
    frame.render_widget(Paragraph::new(Text::from(pb.lines)).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::{BacktrackKey, DisplayBlock};

    fn state() -> TuiSessionState {
        TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "Default".into(),
            std::path::PathBuf::from("/nonexistent-ws"),
            false,
        )
    }

    fn text(pb: &PromptBox) -> String {
        pb.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn edit_approval_reads_like_claude() {
        let mut g = state();
        g.active_approval = Some(ApprovalRequest {
            call_id: "c1".into(),
            tool: "edit_file".into(),
            description: String::new(),
            input: r#"{"path":"a.txt","old_string":"line 11","new_string":"line ELEVEN"}"#.into(),
        });
        let pb = active_prompt_box(&g, 70).expect("prompt");
        let t = text(&pb);
        assert_eq!(pb.title, "Edit file");
        assert!(
            t.contains("- line 11") && t.contains("+ line ELEVEN"),
            "{t}"
        );
        assert!(t.contains("Do you want to make this edit to a.txt?"), "{t}");
        assert!(t.contains("❯ 1. Yes"), "{t}");
        assert!(t.contains("3. No, and tell dcode-ai what to do differently (esc)"));
    }

    #[test]
    fn bash_approval_shows_the_command() {
        let mut g = state();
        g.active_approval = Some(ApprovalRequest {
            call_id: "c1".into(),
            tool: "execute_bash".into(),
            description: "Run the tests".into(),
            input: r#"{"command":"cargo test"}"#.into(),
        });
        g.approval_option_index = 1;
        let pb = active_prompt_box(&g, 70).expect("prompt");
        let t = text(&pb);
        assert_eq!(pb.title, "Bash command");
        assert!(t.contains("cargo test"), "{t}");
        assert!(t.contains("Do you want to proceed?"));
        assert!(t.contains("❯ 2. Yes, and don't ask again"), "{t}");
    }

    #[test]
    fn rewind_flow_lists_messages_then_scopes() {
        let mut g = state();
        g.blocks.push(DisplayBlock::User("first ask".into()));
        g.blocks.push(DisplayBlock::Assistant("ok".into()));
        g.blocks.push(DisplayBlock::User("second ask".into()));
        assert!(g.open_backtrack());
        let t = text(&active_prompt_box(&g, 70).expect("picker"));
        assert!(
            t.contains("❯ > second ask") && t.contains("> first ask"),
            "{t}"
        );

        g.apply_backtrack_key(BacktrackKey::Accept);
        let pb = active_prompt_box(&g, 70).expect("scope");
        let t = text(&pb);
        assert_eq!(pb.title, "Rewind");
        assert!(t.contains("❯ 1. Restore code and conversation"), "{t}");
        assert!(t.contains("3. Restore code"));
    }

    #[test]
    fn plan_exit_offers_build_modes() {
        let mut g = state();
        g.plan_exit_index = Some(0);
        let pb = active_prompt_box(&g, 70).expect("plan exit");
        let t = text(&pb);
        assert_eq!(pb.title, "Ready to code?");
        assert!(t.contains("❯ 1. Yes, and auto-accept edits"));
        assert!(t.contains("3. No, keep planning"));
    }

    #[test]
    fn approval_outranks_other_prompts() {
        let mut g = state();
        g.plan_exit_index = Some(0);
        g.active_approval = Some(ApprovalRequest {
            call_id: "c1".into(),
            tool: "fetch_url".into(),
            description: "fetch".into(),
            input: r#"{"url":"https://example.com"}"#.into(),
        });
        assert_eq!(active_prompt_box(&g, 70).unwrap().title, "Fetch");
    }

    #[test]
    fn theme_picker_lists_claude_themes_with_preview() {
        let mut g = state();
        let names: Vec<String> = theme::CLAUDE_THEMES
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        g.open_theme_picker(names, 0);
        let t = text(&active_prompt_box(&g, 70).expect("theme box"));
        assert!(t.contains("❯ 1. Dark mode"), "{t}");
        assert!(t.contains("2. Light mode"));
        assert!(t.contains("Dark mode (colorblind-friendly)"));
        assert!(t.contains("Light mode (ANSI colors only)"));
        assert!(t.contains("Hello, dcode-ai!"), "preview: {t}");
    }
}
