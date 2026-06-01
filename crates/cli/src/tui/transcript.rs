//! Transcript rendering: turns the session's `DisplayBlock` history into the
//! scrollable, styled lines shown in the TUI, along with per-line click targets
//! (`LineAnswerHit`) used by mouse handling. Extracted from `tui::app`.
//!
//! This is the single largest renderer; its pure sub-helpers live in
//! `render_helpers` and `markdown`, while a few line-assembly helpers it shares
//! with the rest of `app` are imported back from there.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use dcode_ai_common::event::QuestionSelection;

use crate::tool_ui;
use crate::tui::app::{
    LineAnswerHit, LineClickHit, prefixed_line, push_section_gap, push_tool_detail_lines,
    push_transcript_line, tool_header_detail_spans,
};
use crate::tui::markdown::render_markdown_lines_with_hits;
use crate::tui::render_helpers::{
    tool_dot_style, tool_effect_badge, tool_status_chip, truncate_chars, wrap_text,
};
use crate::tui::state::{DisplayBlock, TuiSessionState};
use crate::tui::theme;

/// Build scrollable transcript lines + optional mouse/click targets per line.
pub(crate) fn transcript_lines_and_hits(
    state: &TuiSessionState,
    width: u16,
) -> (Vec<Line<'static>>, Vec<LineAnswerHit>) {
    let w = width.max(20) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineAnswerHit> = Vec::new();

    if !state.pinned_notes.is_empty() {
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(vec![
                Span::styled(
                    " pinned ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(theme::warn())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        " {} item(s) · Ctrl+O list · Ctrl+K pin last",
                        state.pinned_notes.len()
                    ),
                    Style::default().fg(theme::muted()),
                ),
            ]),
            None,
        );
        for (idx, note) in state.pinned_notes.iter().enumerate().take(4) {
            let preview = truncate_chars(note.body.trim(), w.saturating_sub(24).max(24));
            push_transcript_line(
                &mut lines,
                &mut hits,
                Line::from(vec![
                    Span::styled(
                        format!("  [{}] ", idx + 1),
                        Style::default().fg(theme::warn()),
                    ),
                    Span::styled(
                        format!("{:<18}", truncate_chars(&note.title, 18)),
                        Style::default().fg(theme::assistant()),
                    ),
                    Span::styled(" · ", Style::default().fg(theme::muted())),
                    Span::styled(preview, Style::default().fg(theme::text())),
                    Span::styled(
                        "  copy",
                        Style::default()
                            .fg(theme::tool())
                            .add_modifier(Modifier::UNDERLINED),
                    ),
                ]),
                Some(LineClickHit::CopyText(note.body.clone())),
            );
        }
        if state.pinned_notes.len() > 4 {
            push_transcript_line(
                &mut lines,
                &mut hits,
                Line::from(Span::styled(
                    format!("  … +{} more pinned", state.pinned_notes.len() - 4),
                    Style::default().fg(theme::muted()),
                )),
                None,
            );
        }
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
    }

    for block in &state.blocks {
        match block {
            DisplayBlock::User(content) => {
                push_section_gap(&mut lines, &mut hits);
                let user_chip = format!(" {:<9} ", "user");
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![Span::styled(
                        user_chip,
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::user())
                            .add_modifier(Modifier::BOLD),
                    )]),
                    None,
                );
                for text_line in wrap_text(content, w) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        prefixed_line(
                            Span::styled("▏ ", Style::default().fg(theme::user())),
                            Line::from(Span::styled(text_line, Style::default().fg(theme::text()))),
                        ),
                        None,
                    );
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::Assistant(content) => {
                push_section_gap(&mut lines, &mut hits);
                let assistant_chip = format!(" {:<9} ", "assistant");
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(
                            assistant_chip,
                            Style::default()
                                .fg(Color::Black)
                                .bg(theme::assistant())
                                .bold(),
                        ),
                        Span::styled(
                            " response ",
                            Style::default()
                                .fg(theme::assistant())
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Some(LineClickHit::CopyText(content.clone())),
                );
                let (md_lines, md_hits) =
                    render_markdown_lines_with_hits(content, state.code_line_numbers);
                for (md_line, md_hit) in md_lines.into_iter().zip(md_hits) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        prefixed_line(
                            Span::styled("▏ ", Style::default().fg(theme::assistant())),
                            md_line,
                        ),
                        md_hit,
                    );
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ToolRunning {
                name,
                input,
                call_id,
            } => {
                push_section_gap(&mut lines, &mut hits);
                let ui = tool_ui::metadata(name);
                let collapsed = state.is_tool_block_collapsed(call_id);
                let fold_indicator = if collapsed { "▸" } else { "▾" };
                let mut header_spans = vec![
                    Span::styled(
                        format!("{fold_indicator} "),
                        Style::default().fg(theme::muted()),
                    ),
                    Span::styled("◉ ", tool_dot_style(name)),
                    Span::styled(
                        ui.label,
                        Style::default()
                            .fg(theme::tool())
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                let detail_spans = tool_header_detail_spans(name, input);
                if !detail_spans.is_empty() {
                    header_spans.push(Span::raw(" · "));
                    header_spans.extend(detail_spans);
                }
                header_spans.push(Span::raw("  "));
                header_spans.push(tool_effect_badge(name));
                header_spans.push(Span::raw(" "));
                header_spans.push(tool_status_chip("RUNNING", theme::warn()));
                push_transcript_line(&mut lines, &mut hits, Line::from(header_spans), None);
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ApprovalPending(req) => {
                // Full approval UI lives in a popup overlay; transcript only shows a
                // compact placeholder so the user has visual continuity in scrollback.
                push_section_gap(&mut lines, &mut hits);
                let ui = tool_ui::metadata(&req.tool);
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(
                            format!(" {} ", ui.icon),
                            Style::default().fg(Color::Black).bg(theme::warn()).bold(),
                        ),
                        Span::styled(" ", Style::default()),
                        Span::styled(
                            ui.label,
                            Style::default()
                                .fg(theme::warn())
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" ", Style::default()),
                        tool_effect_badge(&req.tool),
                        Span::styled(
                            "  awaiting approval — see popup",
                            Style::default().fg(theme::muted()),
                        ),
                    ]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
                let _ = w; // width still consumed in popup renderer
            }
            DisplayBlock::ApprovalResolved { tool, approved } => {
                push_section_gap(&mut lines, &mut hits);
                let ui = tool_ui::metadata(tool);
                let (label, style) = if *approved {
                    (
                        " approved ",
                        Style::default().fg(Color::Black).bg(theme::success()),
                    )
                } else {
                    (
                        " denied ",
                        Style::default().fg(Color::Black).bg(theme::error()),
                    )
                };
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(label, style.add_modifier(Modifier::BOLD)),
                        Span::styled(format!(" {tool}"), Style::default().fg(theme::text())),
                        Span::styled(
                            format!("  {}", ui.label),
                            Style::default().fg(theme::muted()),
                        ),
                    ]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ToolDone {
                name,
                call_id,
                ok,
                detail,
                duration_ms,
            } => {
                push_section_gap(&mut lines, &mut hits);
                let ui = tool_ui::metadata(name);
                let collapsed = state.is_tool_block_collapsed(call_id);
                let fold_indicator = if collapsed { "▸" } else { "▾" };
                let status_chip = if *ok {
                    tool_status_chip("DONE", theme::success())
                } else {
                    tool_status_chip("FAILED", theme::error())
                };
                let mut header = vec![
                    Span::styled(
                        format!("{fold_indicator} "),
                        Style::default().fg(theme::muted()),
                    ),
                    Span::styled("◉ ", tool_dot_style(name)),
                    Span::styled(
                        ui.label,
                        Style::default()
                            .fg(theme::tool())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    tool_effect_badge(name),
                    Span::raw(" "),
                    status_chip,
                ];
                if let Some(ms) = duration_ms {
                    header.push(Span::raw(" "));
                    header.push(Span::styled(
                        format_duration_badge(*ms),
                        Style::default().fg(theme::muted()),
                    ));
                }
                push_transcript_line(&mut lines, &mut hits, Line::from(header), None);
                if !collapsed && !detail.trim().is_empty() {
                    push_tool_detail_lines(&mut lines, &mut hits, detail, w.saturating_sub(6), 80);
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::System(s) => {
                // Multiline system blocks (e.g. startup logo) render as raw rows
                // without bullet prefixes so ASCII art alignment is preserved.
                if s.contains('\n') {
                    for part in s.split('\n') {
                        push_transcript_line(
                            &mut lines,
                            &mut hits,
                            Line::from(Span::styled(
                                part.to_string(),
                                Style::default().fg(theme::muted()),
                            )),
                            None,
                        );
                    }
                } else if s.is_empty() {
                    push_transcript_line(&mut lines, &mut hits, Line::default(), None);
                } else {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(Span::styled(
                            format!("  • {s}"),
                            Style::default().fg(theme::muted()),
                        )),
                        None,
                    );
                }
            }
            DisplayBlock::Question(q) => {
                let selected_answer = state.answered_questions.get(&q.question_id);
                push_section_gap(&mut lines, &mut hits);
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(
                            " ? ",
                            Style::default().fg(Color::Black).bg(theme::warn()).bold(),
                        ),
                        Span::styled(
                            " question ",
                            Style::default()
                                .fg(theme::warn())
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
                for text_line in wrap_text(&q.prompt, w.saturating_sub(2)) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(vec![
                            Span::styled("  ", Style::default().fg(theme::muted())),
                            Span::styled(text_line, Style::default().fg(theme::text())),
                        ]),
                        None,
                    );
                }
                // When the modal is open, skip inline options — the popup handles selection.
                if !state.question_modal_open {
                    let suggested_selected =
                        matches!(selected_answer, Some(QuestionSelection::Suggested));
                    for (idx, row) in wrap_text(
                        &format!("suggested: {}", q.suggested_answer),
                        w.saturating_sub(12).max(20),
                    )
                    .into_iter()
                    .enumerate()
                    {
                        let row_style = if suggested_selected {
                            Style::default()
                                .fg(Color::Black)
                                .bg(theme::success())
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme::success())
                        };
                        let prefix_style = if suggested_selected {
                            row_style
                        } else {
                            Style::default()
                                .fg(theme::success())
                                .add_modifier(Modifier::BOLD)
                        };
                        let line = if idx == 0 {
                            Line::from(vec![
                                Span::styled("  [0] ", prefix_style),
                                Span::styled(row, row_style),
                                Span::styled(
                                    "  (click)",
                                    if suggested_selected {
                                        row_style
                                    } else {
                                        Style::default().fg(theme::muted())
                                    },
                                ),
                            ])
                        } else {
                            Line::from(vec![
                                Span::styled("      ", Style::default().fg(theme::muted())),
                                Span::styled(row, row_style),
                            ])
                        };
                        push_transcript_line(
                            &mut lines,
                            &mut hits,
                            line,
                            Some(LineClickHit::Question(QuestionSelection::Suggested)),
                        );
                    }
                    for (i, o) in q.options.iter().enumerate() {
                        let opt_selected = matches!(
                            selected_answer,
                            Some(QuestionSelection::Option { option_id }) if option_id == &o.id
                        );
                        let choice_text = format!("({}) {}", o.id, o.label);
                        for (idx, row) in wrap_text(&choice_text, w.saturating_sub(12).max(20))
                            .into_iter()
                            .enumerate()
                        {
                            let row_style = if opt_selected {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::user())
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::text())
                            };
                            let prefix_style = if opt_selected {
                                row_style
                            } else {
                                Style::default()
                                    .fg(theme::assistant())
                                    .add_modifier(Modifier::BOLD)
                            };
                            let line = if idx == 0 {
                                Line::from(vec![
                                    Span::styled(format!("  [{}] ", i + 1), prefix_style),
                                    Span::styled(row, row_style),
                                    Span::styled(
                                        "  (click)",
                                        if opt_selected {
                                            row_style
                                        } else {
                                            Style::default().fg(theme::muted())
                                        },
                                    ),
                                ])
                            } else {
                                Line::from(vec![
                                    Span::styled("      ", Style::default().fg(theme::muted())),
                                    Span::styled(row, row_style),
                                ])
                            };
                            push_transcript_line(
                                &mut lines,
                                &mut hits,
                                line,
                                Some(LineClickHit::Question(QuestionSelection::Option {
                                    option_id: o.id.clone(),
                                })),
                            );
                        }
                    }
                    if q.allow_custom {
                        let custom_selected =
                            matches!(selected_answer, Some(QuestionSelection::Custom { .. }));
                        push_transcript_line(
                            &mut lines,
                            &mut hits,
                            Line::from(Span::styled(
                                "  [c] type your own answer below, then Enter",
                                if custom_selected {
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(theme::assistant())
                                        .add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(theme::muted())
                                },
                            )),
                            None,
                        );
                        if let Some(QuestionSelection::Custom { text }) = selected_answer {
                            push_transcript_line(
                                &mut lines,
                                &mut hits,
                                Line::from(Span::styled(
                                    format!("      selected: {}", truncate_chars(text, 80)),
                                    Style::default().fg(theme::assistant()),
                                )),
                                None,
                            );
                        }
                    }
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(Span::styled(
                            "  Tip: /auto-answer or Enter on empty = suggested · click an option above",
                            Style::default().fg(theme::muted()),
                        )),
                        None,
                    );
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::Thinking(content) => {
                push_section_gap(&mut lines, &mut hits);
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(
                            " ✦ thinking ",
                            Style::default()
                                .fg(Color::Black)
                                .bg(theme::muted())
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("", Style::default()),
                    ]),
                    None,
                );
                let wrapped = wrap_text(content, w.saturating_sub(2));
                let shown = wrapped.len().min(10);
                for text_line in wrapped.iter().take(shown) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(vec![
                            Span::styled(" │ ", Style::default().fg(theme::muted())),
                            Span::styled(
                                text_line.clone(),
                                Style::default()
                                    .fg(theme::muted())
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        ]),
                        None,
                    );
                }
                if wrapped.len() > shown {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(Span::styled(
                            format!(" … +{} more thinking lines", wrapped.len() - shown),
                            Style::default().fg(theme::muted()),
                        )),
                        None,
                    );
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ErrorLine(s) => {
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(Span::styled(
                        format!(" ✗ {s}"),
                        Style::default().fg(theme::error()),
                    )),
                    None,
                );
            }
        }
    }

    if let Some(thinking) = &state.streaming_thinking
        && !thinking.is_empty()
    {
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(vec![Span::styled(
                " ✦ thinking… ",
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::muted())
                    .add_modifier(Modifier::BOLD),
            )]),
            None,
        );
        let wrapped = wrap_text(thinking, w.saturating_sub(2));
        let shown = wrapped.len().min(10);
        for text_line in wrapped.iter().take(shown) {
            push_transcript_line(
                &mut lines,
                &mut hits,
                Line::from(vec![
                    Span::styled(" │ ", Style::default().fg(theme::muted())),
                    Span::styled(
                        text_line.clone(),
                        Style::default()
                            .fg(theme::muted())
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]),
                None,
            );
        }
        if wrapped.len() > shown {
            push_transcript_line(
                &mut lines,
                &mut hits,
                Line::from(Span::styled(
                    format!(" … +{} more thinking lines", wrapped.len() - shown),
                    Style::default().fg(theme::muted()),
                )),
                None,
            );
        }
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
    }

    if let Some(stream) = &state.streaming_assistant
        && !stream.is_empty()
    {
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(vec![
                Span::styled(
                    " dcode-ai ",
                    Style::default().fg(Color::Black).bg(theme::assistant()),
                ),
                Span::styled(" streaming ", Style::default().fg(theme::muted())),
            ]),
            Some(LineClickHit::CopyText(stream.clone())),
        );
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
        let (md_lines, md_hits) = render_markdown_lines_with_hits(stream, state.code_line_numbers);
        for (md_line, md_hit) in md_lines.into_iter().zip(md_hits) {
            push_transcript_line(
                &mut lines,
                &mut hits,
                prefixed_line(
                    Span::styled("▏ ", Style::default().fg(theme::assistant())),
                    md_line,
                ),
                md_hit,
            );
        }
    }

    if lines.is_empty() {
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(vec![
                Span::styled(
                    "dcode-ai",
                    Style::default()
                        .fg(theme::assistant())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" — session ready", Style::default().fg(theme::muted())),
            ]),
            None,
        );
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
        // Context chips: workspace, branch, model, agent — so a fresh session
        // shows where you are and what you're driving without running /status.
        let mut ctx_spans: Vec<Span<'static>> = Vec::new();
        let push_chip = |spans: &mut Vec<Span<'static>>, label: &str, value: &str| {
            if value.trim().is_empty() {
                return;
            }
            if !spans.is_empty() {
                spans.push(Span::styled("  ·  ", Style::default().fg(theme::muted())));
            }
            spans.push(Span::styled(
                format!("{label} "),
                Style::default().fg(theme::muted()),
            ));
            spans.push(Span::styled(
                value.to_string(),
                Style::default().fg(theme::text()),
            ));
        };
        push_chip(
            &mut ctx_spans,
            "cwd",
            &truncate_chars(&state.workspace_display, 40),
        );
        push_chip(&mut ctx_spans, "branch", &state.current_branch);
        push_chip(&mut ctx_spans, "model", &truncate_chars(&state.model, 24));
        push_chip(&mut ctx_spans, "agent", &state.agent_profile);
        if !ctx_spans.is_empty() {
            push_transcript_line(&mut lines, &mut hits, Line::from(ctx_spans), None);
            push_transcript_line(&mut lines, &mut hits, Line::default(), None);
        }
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(Span::styled(
                "Tab agent · Shift+Enter/Ctrl+I/Ctrl+J newline · Ctrl+P palette · /keymaps · Ctrl+V image",
                Style::default().fg(theme::muted()),
            )),
            None,
        );
    }

    (lines, hits)
}

/// Format a tool duration as a compact badge: `120ms` under 1s, else `1.2s`.
fn format_duration_badge(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::format_duration_badge;

    #[test]
    fn duration_badge_formats_ms_and_seconds() {
        assert_eq!(format_duration_badge(0), "0ms");
        assert_eq!(format_duration_badge(120), "120ms");
        assert_eq!(format_duration_badge(999), "999ms");
        assert_eq!(format_duration_badge(1200), "1.2s");
        assert_eq!(format_duration_badge(60_000), "60.0s");
    }
}
