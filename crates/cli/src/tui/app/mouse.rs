//! Mouse handling for the session TUI.

use super::*;

/// Handle one mouse event: wheel scroll, drag-select/copy, transcript click
/// targets (question options, collapsible blocks, links), slash/@ panel
/// clicks and status-bar chips.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_mouse_event(
    m: crossterm::event::MouseEvent,
    g: &mut TuiSessionState,
    sz: ratatui::layout::Size,
    slash_entries: &[SlashEntry],
    workspace_files: &[String],
    transcript_cache: &mut TranscriptRenderCache,
    scroll_buffer: &mut crate::tui::scroll_buffer::ScrollBuffer,
    scroll_speed: u16,
    cmd_tx: &UnboundedSender<TuiCmd>,
) {
    let area = Rect::new(0, 0, sz.width, sz.height);
    let (main_area, _) = layout_with_sidebar(area, g.sidebar_open);
    let slash_filtered = filter_slash_entries(slash_entries, &g.input_buffer);
    let at_matches = at_completion_matches(workspace_files, &g.input_buffer, g.cursor_char_idx);
    let sh = composer_chrome_height(
        slash_entries,
        workspace_files,
        &g.input_buffer,
        g.cursor_char_idx,
    );
    let input_h = if should_hide_composer_when_scrolling(g) {
        0
    } else {
        composer_input_height(g, main_area.width)
    };
    let (tr, _, slash_r, _) =
        layout_chunks(main_area, sh, input_h, g.unsent_count(), g.subagents.len());

    // Mouse scroll works anywhere on the screen, not just inside the transcript.
    if matches!(
        m.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        let inner_w = tr.width.saturating_sub(2);
        let (lines, _hits, rebuilt) = transcript_cache.get_or_rebuild(g, inner_w);
        let th = tr.height.saturating_sub(2) as usize;
        if rebuilt {
            scroll_buffer.replace_lines(lines.to_vec());
        }
        scroll_buffer.set_from_top(g.scroll_lines, th, inner_w as usize);
        let step = mouse_scroll_step(m.modifiers, th, scroll_speed as usize);
        // Scrolling clears any active mouse text selection.
        g.mouse_selection = None;
        match m.kind {
            MouseEventKind::ScrollUp => {
                scroll_buffer.scroll_up(step, inner_w as usize, th);
            }
            MouseEventKind::ScrollDown => {
                scroll_buffer.scroll_down(step);
            }
            _ => {}
        }
        let (from_top, _) = scroll_buffer.scroll_position_from_top(th, inner_w as usize);
        g.scroll_lines = from_top as usize;
        g.transcript_follow_tail = scroll_buffer.is_sticky();
        return;
    }

    // ── Mouse text selection within transcript ──────────────
    // Down(Left) anchors a selection; Drag(Left) extends it;
    // Up(Left) with a non-empty drag copies to clipboard.
    // This stays active even if drag/release leaves the panel.
    let inner_top = tr.y.saturating_add(1);
    let inner_left = tr.x.saturating_add(1);
    let inner_w = tr.width.saturating_sub(2) as usize;
    let inner_h = tr.height.saturating_sub(2) as usize;
    let in_transcript_inner = m.row >= inner_top
        && (m.row as usize) < inner_top as usize + inner_h
        && m.column >= inner_left
        && (m.column as usize) < inner_left as usize + inner_w;
    g.history_rect = Some((tr.x, tr.y, tr.width, tr.height));

    match m.kind {
        MouseEventKind::Down(MouseButton::Left) if in_transcript_inner => {
            let row_in_area = (m.row - inner_top) as usize;
            let col_in_area = (m.column - inner_left) as usize;
            let buf_row = (g.scroll_lines + row_in_area).min(u16::MAX as usize) as u16;
            g.mouse_selection = Some(crate::tui::mouse_select::Selection {
                anchor: crate::tui::mouse_select::VisualPos {
                    row: buf_row,
                    col: col_in_area.min(u16::MAX as usize) as u16,
                },
                cursor: crate::tui::mouse_select::VisualPos {
                    row: buf_row,
                    col: col_in_area.min(u16::MAX as usize) as u16,
                },
                scroll_from_top: g.scroll_lines.min(u16::MAX as usize) as u16,
            });
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if g.mouse_selection.is_some() && inner_h > 0 {
                let inner_w_eff = inner_w.max(1);
                let (lines, _hits, rebuilt) =
                    transcript_cache.get_or_rebuild(g, inner_w_eff as u16);
                if rebuilt {
                    scroll_buffer.replace_lines(lines.to_vec());
                }
                scroll_buffer.set_from_top(g.scroll_lines, inner_h, inner_w_eff);
                if m.row < inner_top {
                    scroll_buffer.scroll_up(1, inner_w_eff, inner_h);
                } else if (m.row as usize) >= inner_top as usize + inner_h {
                    scroll_buffer.scroll_down(1);
                }
                let (from_top, _) = scroll_buffer.scroll_position_from_top(inner_h, inner_w_eff);
                g.scroll_lines = from_top as usize;
                g.transcript_follow_tail = scroll_buffer.is_sticky();

                let clamped_row = if m.row < inner_top {
                    0usize
                } else if (m.row as usize) >= inner_top as usize + inner_h {
                    inner_h.saturating_sub(1)
                } else {
                    (m.row - inner_top) as usize
                };
                let clamped_col = if m.column < inner_left {
                    0usize
                } else if (m.column as usize) >= inner_left as usize + inner_w_eff {
                    inner_w_eff.saturating_sub(1)
                } else {
                    (m.column - inner_left) as usize
                };

                let scroll_from_top = g.scroll_lines.min(u16::MAX as usize) as u16;
                if let Some(ref mut sel) = g.mouse_selection {
                    sel.scroll_from_top = scroll_from_top;
                    let buf_row =
                        (sel.scroll_from_top as usize + clamped_row).min(u16::MAX as usize) as u16;
                    sel.cursor = crate::tui::mouse_select::VisualPos {
                        row: buf_row,
                        col: clamped_col.min(u16::MAX as usize) as u16,
                    };
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(sel) = g.mouse_selection.take()
                && sel.anchor != sel.cursor
                && !is_click_jitter(&sel)
            {
                let inner_w_eff = inner_w.max(1);
                let (lines, _hits, _) = transcript_cache.get_or_rebuild(g, inner_w_eff as u16);
                let gutter_widths = vec![0u16; lines.len()];
                let (rows, gutters) = crate::tui::mouse_select::build_all_visual_rows(
                    lines,
                    &gutter_widths,
                    inner_w_eff,
                );
                let text = crate::tui::mouse_select::extract_selected_text(&rows, &gutters, &sel);
                if !text.is_empty() {
                    match crate::tui::mouse_select::copy_to_clipboard(&text) {
                        Ok(_msg) => {
                            g.show_toast(
                                "✓ Copied to clipboard",
                                crate::tui::state::ToastKind::Success,
                            );
                        }
                        Err(e) => {
                            g.push_error(format!("Clipboard copy failed: {e}"));
                        }
                    }
                    // Meaningful drag copy consumed the click.
                    return;
                }
            }
            // Pure click (no drag): let click-hit logic run.
        }
        _ => {}
    }

    // Transcript click targets (left-click on question options / code blocks).
    if rect_contains(tr, m.column, m.row) {
        let inner_w = tr.width.saturating_sub(2);
        let (_lines, hits, _) = transcript_cache.get_or_rebuild(g, inner_w);
        let th = tr.height.saturating_sub(2) as usize;
        match m.kind {
            k if mouse_left_activated(k) => {
                // Inner content starts below top border (y+1).
                let inner_top = tr.y.saturating_add(1);
                if m.row >= inner_top {
                    let row_in_area = (m.row - inner_top) as usize;
                    if row_in_area < th {
                        let gline = g.scroll_lines + row_in_area;
                        let picked = if gline < hits.len() {
                            hits[gline].clone()
                        } else {
                            None
                        };
                        if let Some(hit) = picked {
                            activate_line_hit(hit, g);
                            return;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(sr) = slash_r
        && rect_contains(sr, m.column, m.row)
        && mouse_left_activated(m.kind)
    {
        let inner_y = m.row.saturating_sub(sr.y).saturating_sub(1);
        if slash_panel_visible(&g.input_buffer) && !slash_filtered.is_empty() {
            let n_show = slash_filtered.len().min(SLASH_PANEL_MAX_ROWS);
            let max_scroll = slash_filtered.len().saturating_sub(n_show);
            let list_scroll = g
                .slash_menu_index
                .saturating_sub(n_show.saturating_sub(1))
                .min(max_scroll);
            if (inner_y as usize) < n_show {
                let idx = list_scroll + inner_y as usize;
                if idx < slash_filtered.len() {
                    g.set_input_text(slash_filtered[idx].command_str());
                    g.slash_menu_index = idx;
                }
            }
        } else if !at_matches.is_empty() {
            let n_show = at_matches.len().min(SLASH_PANEL_MAX_ROWS);
            let max_scroll = at_matches.len().saturating_sub(n_show);
            let pick = g.at_menu_index.min(at_matches.len().saturating_sub(1));
            let list_scroll = pick
                .saturating_sub(n_show.saturating_sub(1))
                .min(max_scroll);
            if (inner_y as usize) < n_show {
                let idx = list_scroll + inner_y as usize;
                if let Some(choice) = at_matches.get(idx) {
                    let cur = g.cursor_char_idx;
                    let (buf, cidx) = apply_at_completion(&g.input_buffer, cur, choice);
                    g.set_input_text_with_cursor(buf, cidx);
                }
            }
        }
    }

    // Check click on branch chip in status bar.
    if let Some(bounds) = g.branch_chip_bounds
        && rect_contains(bounds, m.column, m.row)
        && mouse_left_activated(m.kind)
    {
        let _ = cmd_tx.send(TuiCmd::OpenBranchPicker);
    }
}

/// Run the action behind a clicked transcript line.
fn activate_line_hit(hit: LineClickHit, g: &mut TuiSessionState) {
    match hit {
        LineClickHit::CopyText(text) => match copy_to_clipboard(&text) {
            Ok(_) => g.show_toast(
                "✓ Copied to clipboard",
                crate::tui::state::ToastKind::Success,
            ),
            Err(e) => g.show_toast(
                format!("✗ Copy failed: {e}"),
                crate::tui::state::ToastKind::Error,
            ),
        },
        LineClickHit::ToggleThinking => {
            g.thinking_expanded = !g.thinking_expanded;
            g.touch_transcript();
        }
        LineClickHit::ToggleAssistant(block_idx) => {
            if g.collapsed_assistant_blocks.contains(&block_idx) {
                g.collapsed_assistant_blocks.remove(&block_idx);
            } else {
                g.collapsed_assistant_blocks.insert(block_idx);
            }
            g.touch_transcript();
        }
        LineClickHit::OpenLink(target) => {
            let _ = open_link_in_system(&target);
            g.show_toast(
                format!("↗ Opened: {}", &target[..target.len().min(40)]),
                crate::tui::state::ToastKind::Info,
            );
        }
    }
}

/// Dashboard mouse: wheel scrolls the transcript (or the changes list when
/// the pointer is over it); left clicks hit rail targets recorded by the
/// last draw, then transcript click targets.
pub(super) fn handle_dashboard_mouse(
    m: crossterm::event::MouseEvent,
    g: &mut TuiSessionState,
    transcript_cache: &mut TranscriptRenderCache,
) {
    use crate::tui::state::{DashTarget, RailTab};

    let over_changes = g.rail_tab == RailTab::Changes
        && g.dash_hits.targets.iter().any(|(r, t)| {
            matches!(t, DashTarget::ReviewItem(_)) && r.y <= m.row && m.column >= r.x
        })
        && g.dash_hits
            .transcript
            .is_none_or(|(tr, _)| !rect_contains(tr, m.column, m.row));
    match m.kind {
        MouseEventKind::ScrollUp if over_changes => g.diff_review.move_by(-1),
        MouseEventKind::ScrollDown if over_changes => g.diff_review.move_by(1),
        MouseEventKind::ScrollUp => {
            g.transcript_follow_tail = false;
            g.scroll_lines = g.scroll_lines.saturating_sub(3);
        }
        MouseEventKind::ScrollDown => {
            g.scroll_lines = g.scroll_lines.saturating_add(3);
        }
        k if mouse_left_activated(k) => {
            let target = g
                .dash_hits
                .targets
                .iter()
                .find(|(r, _)| rect_contains(*r, m.column, m.row))
                .map(|(_, t)| *t);
            match target {
                Some(DashTarget::Tab(tab)) => g.select_rail_tab(tab),
                Some(DashTarget::ReviewItem(item)) => {
                    g.rail_focus = true;
                    g.diff_review.cursor = item;
                    g.diff_review.confirm_revert = false;
                }
                Some(DashTarget::Subagent(idx)) => {
                    g.open_subagent_modal();
                    g.subagent_modal_index = idx.min(g.subagents.len().saturating_sub(1));
                }
                None => {
                    let Some((tr, start)) = g.dash_hits.transcript else {
                        return;
                    };
                    if !rect_contains(tr, m.column, m.row) {
                        return;
                    }
                    // Clicking the conversation returns keys to the composer.
                    g.rail_focus = false;
                    let inner_w = tr.width.saturating_sub(2).max(10);
                    let (_lines, hits, _) = transcript_cache.get_or_rebuild(g, inner_w);
                    let line = start + (m.row - tr.y) as usize;
                    if let Some(Some(hit)) = hits.get(line).cloned() {
                        activate_line_hit(hit, g);
                    }
                }
            }
        }
        _ => {}
    }
}
