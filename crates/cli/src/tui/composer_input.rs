//! Composer input logic: cursor math, `@`-mention completion, panel heights,
//! and styled rendering of the input line. Extracted from `tui::app`.
//!
//! Depends on [`crate::tui::slash_entries`] for the slash-panel primitives;
//! the dependency only goes one way (slash → composer never the reverse).

use dcode_ai_common::event::BusyState;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::file_mentions;
use crate::tui::slash_entries::{
    SLASH_PANEL_MAX_ROWS, SlashEntry, filter_slash_entries, slash_panel_height, slash_panel_visible,
};
use crate::tui::state::TuiSessionState;
use crate::tui::theme;

pub(crate) fn cursor_byte_index(line: &str, cursor_char_idx: usize) -> usize {
    line.char_indices()
        .nth(cursor_char_idx)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}

pub(crate) fn at_panel_height(n: usize) -> u16 {
    if n == 0 {
        return 0;
    }
    (n.min(SLASH_PANEL_MAX_ROWS) as u16).saturating_add(2)
}

pub(crate) fn at_completion_active(buffer: &str, cursor_char_idx: usize) -> bool {
    if slash_panel_visible(buffer) {
        return false;
    }
    let b = cursor_byte_index(buffer, cursor_char_idx);
    file_mentions::at_token_before_cursor(buffer, b).is_some()
}

pub(crate) fn at_completion_matches(
    workspace_files: &[String],
    buffer: &str,
    cursor_char_idx: usize,
) -> Vec<String> {
    if !at_completion_active(buffer, cursor_char_idx) {
        return Vec::new();
    }
    let b = cursor_byte_index(buffer, cursor_char_idx);
    let Some((_, prefix)) = file_mentions::at_token_before_cursor(buffer, b) else {
        return Vec::new();
    };
    file_mentions::filter_paths_prefix(workspace_files, &prefix)
}

pub(crate) fn composer_chrome_height(
    slash_entries: &[SlashEntry],
    workspace_files: &[String],
    buffer: &str,
    cursor_char_idx: usize,
) -> u16 {
    let slash_filtered = filter_slash_entries(slash_entries, buffer);
    let at_matches = at_completion_matches(workspace_files, buffer, cursor_char_idx);
    let slash_h = if slash_panel_visible(buffer) {
        slash_panel_height(slash_filtered.len())
    } else {
        0
    };
    let at_h = if !at_matches.is_empty() {
        at_panel_height(at_matches.len())
    } else {
        0
    };
    slash_h.max(at_h)
}

pub(crate) fn composer_input_height(state: &TuiSessionState, area_width: u16) -> u16 {
    let inner_w = area_width.saturating_sub(4).max(1) as usize;
    let prompt_w = 2usize; // "› "
    let raw_lines: Vec<&str> = state.input_buffer.split('\n').collect();
    let mut wrapped_input_lines = 0usize;
    for (idx, line) in raw_lines.iter().enumerate() {
        let cells = line.chars().count() + usize::from(idx == 0) * prompt_w;
        wrapped_input_lines += cells.max(1).div_ceil(inner_w);
    }
    let wrapped_input_lines = wrapped_input_lines.max(1) as u16;

    let mut content_lines = wrapped_input_lines;
    if !state.staged_image_attachments.is_empty() {
        content_lines = content_lines.saturating_add(1);
    }
    let show_hint = state.active_approval.is_some()
        || (state.active_question.is_some() && !state.question_modal_open)
        || state.busy
        || !matches!(state.current_busy_state, BusyState::Idle);
    if show_hint {
        content_lines = content_lines.saturating_add(1);
    }

    content_lines.saturating_add(2).clamp(3, 11)
}

pub(crate) fn should_hide_composer_when_scrolling(state: &TuiSessionState) -> bool {
    !state.transcript_follow_tail
        && state.input_buffer.trim().is_empty()
        && state.staged_image_attachments.is_empty()
        && state.active_approval.is_none()
        && state.active_question.is_none()
        && !state.busy
        && matches!(state.current_busy_state, BusyState::Idle)
}

/// Replace `@prefix` before cursor with `@choice` (relative path).
pub(crate) fn apply_at_completion(
    buffer: &str,
    cursor_char_idx: usize,
    choice: &str,
) -> (String, usize) {
    let b = cursor_byte_index(buffer, cursor_char_idx);
    let Some((at_byte, _prefix)) = file_mentions::at_token_before_cursor(buffer, b) else {
        return (buffer.to_string(), cursor_char_idx);
    };
    let before = &buffer[..at_byte.saturating_add(1)];
    let after = &buffer[b..];
    let new_buf = format!("{before}{choice}{after}");
    let new_byte = at_byte + 1 + choice.len();
    let new_char = new_buf[..new_byte.min(new_buf.len())].chars().count();
    (new_buf, new_char)
}

pub(crate) fn apply_selected_at_completion(
    workspace_files: &[String],
    buffer: &str,
    cursor_char_idx: usize,
    at_menu_index: usize,
    append_space: bool,
) -> Option<(String, usize)> {
    let at_matches = at_completion_matches(workspace_files, buffer, cursor_char_idx);
    if at_matches.is_empty() || !at_completion_active(buffer, cursor_char_idx) {
        return None;
    }

    let pick = at_menu_index.min(at_matches.len().saturating_sub(1));
    let choice = at_matches.get(pick)?;
    let (mut new_buf, mut new_cursor_char_idx) =
        apply_at_completion(buffer, cursor_char_idx, choice);

    if append_space {
        let insert_at = cursor_byte_index(&new_buf, new_cursor_char_idx);
        new_buf.insert(insert_at, ' ');
        new_cursor_char_idx += 1;
    }

    Some((new_buf, new_cursor_char_idx))
}

pub(crate) fn at_mention_char_ranges(buffer: &str) -> Vec<(usize, usize)> {
    file_mentions::parse_at_mentions(buffer)
        .into_iter()
        .map(|(start, end, _)| {
            let start_char = buffer[..start].chars().count();
            let end_char = buffer[..end].chars().count();
            (start_char, end_char)
        })
        .collect()
}

pub(crate) fn completed_at_mention_range_before_cursor(
    buffer: &str,
    cursor_char_idx: usize,
) -> Option<(usize, usize)> {
    let chars: Vec<char> = buffer.chars().collect();
    for (start_char, end_char) in at_mention_char_ranges(buffer) {
        if end_char == cursor_char_idx {
            return Some((start_char, end_char));
        }
        if end_char < chars.len()
            && end_char + 1 == cursor_char_idx
            && chars.get(end_char) == Some(&' ')
        {
            return Some((start_char, end_char + 1));
        }
    }
    None
}

pub(crate) fn remove_char_range(
    buffer: &str,
    start_char_idx: usize,
    end_char_idx: usize,
) -> String {
    let mut chars: Vec<char> = buffer.chars().collect();
    chars.drain(start_char_idx..end_char_idx);
    chars.into_iter().collect()
}

pub(crate) fn delete_completed_at_mention(
    buffer: &str,
    cursor_char_idx: usize,
) -> Option<(String, usize)> {
    let (start_char, end_char) = completed_at_mention_range_before_cursor(buffer, cursor_char_idx)?;
    Some((remove_char_range(buffer, start_char, end_char), start_char))
}

fn push_styled_run(
    spans: &mut Vec<Span<'static>>,
    text: &mut String,
    current_style: &mut Option<Style>,
    style: Style,
    ch: char,
) {
    if current_style.as_ref() != Some(&style) && !text.is_empty() {
        spans.push(Span::styled(
            std::mem::take(text),
            current_style.unwrap_or_default(),
        ));
    }
    *current_style = Some(style);
    text.push(ch);
}

pub(crate) fn composer_line(buffer: &str, cursor_char_idx: usize) -> Line<'static> {
    let prompt = Span::styled("› ", Style::default().fg(theme::user()).bold());
    let placeholder = "Ask anything…   /  commands   ·   ⏎ send   ·   ⇧⏎ newline";
    let chars: Vec<char> = buffer.chars().collect();
    let mention_ranges = at_mention_char_ranges(buffer);
    let cursor_char_idx = cursor_char_idx.min(chars.len());
    let mut spans = vec![prompt];
    let mut run = String::new();
    let mut run_style: Option<Style> = None;

    for idx in 0..=chars.len() {
        if idx == cursor_char_idx {
            let cursor_char = chars.get(idx).copied().unwrap_or(' ');
            let in_mention = idx < chars.len()
                && mention_ranges
                    .iter()
                    .any(|(start, end)| *start <= idx && idx < *end);
            let cursor_style = if in_mention {
                Style::default()
                    .bg(theme::user())
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .bg(theme::muted())
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            };
            push_styled_run(
                &mut spans,
                &mut run,
                &mut run_style,
                cursor_style,
                cursor_char,
            );
            if idx == chars.len() {
                break;
            }
            continue;
        }

        let Some(ch) = chars.get(idx).copied() else {
            break;
        };
        let in_mention = mention_ranges
            .iter()
            .any(|(start, end)| *start <= idx && idx < *end);
        let style = if in_mention {
            Style::default()
                .fg(theme::text())
                .bg(theme::mention_bg())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::text())
        };
        push_styled_run(&mut spans, &mut run, &mut run_style, style, ch);
    }

    if !run.is_empty() {
        spans.push(Span::styled(run, run_style.unwrap_or_default()));
    }
    if buffer.is_empty() {
        spans.push(Span::styled(
            placeholder.to_string(),
            Style::default().fg(theme::muted()),
        ));
    }

    Line::from(spans)
}
