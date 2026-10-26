//! Per-frame rendering of the inline session view.

use super::*;

/// Draw one frame of the inline (scrollback-preserving) view: live content,
/// slash/@ panels, composer, status bar, and any open overlay.
pub(super) fn draw_inline_frame(
    frame: &mut ratatui::Frame<'_>,
    g: &mut TuiSessionState,
    slash_filtered: &[&SlashEntry],
    at_matches: &[String],
    chrome_h: u16,
    transcript_cache: &mut TranscriptRenderCache,
) {
    let area = frame.area();
    draw_session_pane(
        frame,
        area,
        g,
        slash_filtered,
        at_matches,
        chrome_h,
        transcript_cache,
    );
}

/// The session pane — transcript, slash/@ panels, composer, status bar and
/// overlays — drawn into `area`. The inline view gives it the whole viewport;
/// the dashboard gives it the centre column.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_session_pane(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    g: &mut TuiSessionState,
    slash_filtered: &[&SlashEntry],
    at_matches: &[String],
    chrome_h: u16,
    transcript_cache: &mut TranscriptRenderCache,
) {
    // Clear the whole viewport each frame so moving content (the
    // input box shifts as live content grows/shrinks) never leaves
    // ghost copies behind. `Clear` resets cells to default — it
    // wipes stale rows without painting a dark block over the
    // terminal wallpaper. Content below paints its own background.
    frame.render_widget(ClearWidget, area);

    // ── Layout: transcript · separator · input ──
    // Tight input height: 1 line, growing with multiline content
    // (no border chrome — clean Codex look).
    let input_h = {
        // Use the SAME char-wrap as the render so the box height
        // exactly fits the wrapped input (no clipped cursor row).
        let inner_w = area.width.saturating_sub(4).max(1) as usize;
        let line = composer_line(&g.input_buffer, g.cursor_char_idx);
        let rows = wrap_composer_line(&line, inner_w).len();
        (rows.max(1) as u16).min(8)
    };
    // A pending decision (permission, rewind, plan → build) takes the
    // composer's place, Claude-style.
    let prompt =
        crate::tui::render::prompt_box::active_prompt_box(g, area.width.saturating_sub(4) as usize);
    let slash_h = if prompt.is_some() {
        0
    } else if slash_panel_visible(&g.input_buffer) && !slash_filtered.is_empty() {
        slash_panel_height(slash_filtered.len())
    } else if !at_matches.is_empty() {
        (at_matches.len().min(8) as u16) + 2
    } else {
        0
    };
    let _ = chrome_h;
    // Messages typed during the turn, waiting to be sent (shown above the
    // composer, Claude-style).
    let queued = if prompt.is_some() {
        Vec::new()
    } else {
        super::chrome::queued_lines(g, area.width.saturating_sub(2) as usize)
    };
    let queue_h = queued.len() as u16;

    // ── Live content (unflushed blocks + streaming) ──
    // Follows the tail by default; respects user scroll position
    // when `transcript_follow_tail` is false.
    let inner_w = area.width.saturating_sub(2).max(10);
    let (lines, _hits, _rebuilt) = transcript_cache.get_or_rebuild(g, inner_w);

    // Input box height = input lines + 2 border rows (or the prompt box).
    let box_h = match &prompt {
        Some(pb) => crate::tui::render::prompt_box::prompt_box_height(
            pb,
            area.height.saturating_sub(4).max(5),
        ),
        // `/resume` takes the composer's place.
        None if g.session_picker_open => crate::tui::render::pickers::session_picker_height(
            g,
            area.height.saturating_sub(3).max(7),
        ),
        None => input_h + 2,
    };
    // Rows available for live content = viewport − spacer − slash
    // − input box − status.
    let avail_for_live = area
        .height
        .saturating_sub(1 + queue_h + slash_h + box_h + 1) as usize;
    let total = lines.len();
    // Top-align when it fits; show the tail when at default scroll
    // state, or preserve user's manual scroll position.
    let start = if g.transcript_follow_tail {
        total.saturating_sub(avail_for_live)
    } else {
        g.scroll_lines.min(total.saturating_sub(avail_for_live))
    };
    g.scroll_lines = start;
    let dashboard = g.view_mode == crate::tui::state::ViewMode::Dashboard;
    if dashboard && !g.transcript_follow_tail && start >= total.saturating_sub(avail_for_live) {
        // Scrolled back to the bottom: resume following new output.
        g.transcript_follow_tail = true;
    }
    let mut visible: Vec<Line> = if dashboard {
        // The dashboard pane is full height; never hand the layout more rows
        // than fit (a scrolled-up transcript can be thousands of lines).
        lines[start..]
            .iter()
            .take(avail_for_live)
            .cloned()
            .collect()
    } else {
        lines[start..].to_vec()
    };
    // Paint the in-progress drag highlight. `mouse_selection`'s
    // coordinates are absolute buffer rows (see mouse_select.rs);
    // shift them into this window's local row space before
    // applying — without this a user sees no feedback while
    // dragging, only the copied result after release.
    if let Some(sel) = &g.mouse_selection {
        let shift = start.min(u16::MAX as usize) as u16;
        let local_sel = crate::tui::mouse_select::shift_selection_rows(sel, shift);
        visible = crate::tui::mouse_select::apply_selection_highlight(
            visible,
            &local_sel,
            inner_w as usize,
        );
    }
    let live_h = visible.len() as u16;

    // Contiguous block from the top:
    // [live][spacer][queued][slash][input-box][status][bg].
    // Inline: content-hugging stack from the top of the viewport. Dashboard:
    // the transcript fills the pane and the composer is pinned to the bottom.
    let live_constraint = if dashboard {
        Constraint::Min(0)
    } else {
        Constraint::Length(live_h)
    };
    let tail_constraint = if dashboard {
        Constraint::Length(0)
    } else {
        Constraint::Min(0)
    };
    let chunks = Layout::vertical([
        live_constraint,
        Constraint::Length(1),
        Constraint::Length(queue_h),
        Constraint::Length(slash_h),
        Constraint::Length(box_h),
        Constraint::Length(1),
        tail_constraint,
    ])
    .split(area);
    let tr = chunks[0];
    // chunks[1]: the working spinner while a turn runs, else breathing room.
    if let Some(line) = super::chrome::spinner_line(g) {
        frame.render_widget(Paragraph::new(line), chunks[1]);
    }
    if queue_h > 0 {
        frame.render_widget(Paragraph::new(Text::from(queued)), chunks[2]);
    }
    let panel_r = chunks[3];
    let inp_r = chunks[4];
    let status_r = chunks[5];
    if dashboard {
        g.dash_hits.transcript = Some((tr, start));
    }

    if !visible.is_empty() {
        // Inline lines sit flush left like the scrollback they flush into;
        // the dashboard pane keeps a one-column gutter.
        let pad = if dashboard { 1 } else { 0 };
        frame.render_widget(
            Paragraph::new(Text::from(visible))
                .style(Style::default().bg(theme::bg()))
                .block(Block::default().padding(Padding::horizontal(pad))),
            tr,
        );
    }

    // ── Slash command panel / @ mention panel ──
    if panel_r.height > 0 {
        if slash_panel_visible(&g.input_buffer) && !slash_filtered.is_empty() {
            render_slash_panel(frame, panel_r, slash_filtered, g.slash_menu_index);
        } else if !at_matches.is_empty() {
            render_at_panel(frame, panel_r, at_matches, g.at_menu_index);
        }
    }

    if let Some(pb) = prompt {
        crate::tui::render::prompt_box::render_prompt_box(frame, inp_r, pb);
    } else if g.session_picker_open {
        crate::tui::render::pickers::render_session_picker(frame, inp_r, g);
    } else {
        draw_composer(frame, inp_r, g, input_h);
    }

    // ── Footer: mode indicator · model / context ──
    super::chrome::render_footer(frame, status_r, g);

    // ── Centered popups ──
    if let Some(panel) = &g.mcp_panel {
        panel.render(frame, area, &g.mcp_server_statuses);
    } else if let Some(browser) = &g.plugin_browser {
        browser.render(frame, area);
    } else if let Some(prompt) = &g.sudo_prompt {
        prompt.render(frame, area);
    } else if let Some(picker) = &g.setup_picker {
        picker.render(frame, area);
    } else if g.connect_modal_open {
        render_connect_modal(frame, area, g);
    } else if g.command_palette_open {
        render_command_palette(frame, area, g);
    } else if g.model_picker_open {
        let (model_labels, visual_idx) = model_picker_labels_and_index(g);
        render_list_picker(
            frame,
            area,
            "model",
            &model_labels,
            visual_idx,
            &g.model_picker_search,
        );
    } else if g.agent_picker_open {
        render_list_picker(
            frame,
            area,
            "agent",
            &agent_picker_labels(),
            g.agent_picker_index,
            "",
        );
    } else if g.project_picker_open {
        render_list_picker(
            frame,
            area,
            "project",
            &project_picker_labels(g),
            g.project_picker_index,
            "",
        );
    } else if g.info_modal_open {
        render_info_modal(frame, area, g);
    }

    if g.toast.as_ref().is_some_and(|t| t.is_expired()) {
        g.toast = None;
    }

    g.branch_chip_bounds = None;
    g.sidebar_toggle_bounds = None;
}

/// The rounded composer box: `> input`, wrapped, scrolled to the cursor.
fn draw_composer(frame: &mut ratatui::Frame<'_>, inp_r: Rect, g: &TuiSessionState, input_h: u16) {
    let border_color = if toolbar_permission_is_bypass(&g.permission_mode) {
        theme::error()
    } else if g.input_buffer.starts_with('!') {
        // `!` runs a shell command: pink rule, like Claude's bash mode.
        theme::bash_border()
    } else {
        theme::muted()
    };
    let input_line = composer_line(&g.input_buffer, g.cursor_char_idx);
    // Char-wrap at the box's inner width (borders 2 + h-padding 2 = 4)
    // so long unbroken tokens wrap instead of overflowing the box.
    // This matches the height math (`input_h`, which counts wrapped
    // rows) so the box grows to fit. We pre-wrap rather than using
    // ratatui's word `Wrap`, which won't break a whitespace-free run.
    let box_inner_w = (inp_r.width as usize).saturating_sub(2).max(1);
    let input_lines = wrap_composer_line(&input_line, box_inner_w);
    // When the wrapped input is taller than the box (capped at
    // `input_h` rows), scroll so the cursor's row stays visible —
    // whether typing at the end or navigating up — like Codex's
    // composer. No scroll when everything fits.
    let visible_rows = input_h as usize;
    let total_rows = input_lines.len();
    let scroll_y = if total_rows <= visible_rows {
        0
    } else {
        let cursor_row = composer_cursor_row(&g.input_buffer, g.cursor_char_idx, box_inner_w);
        let max_scroll = total_rows - visible_rows;
        // Keep the cursor on the last visible row when it would fall
        // below the window; clamp to the valid scroll range.
        cursor_row.saturating_sub(visible_rows - 1).min(max_scroll)
    } as u16;
    frame.render_widget(
        Paragraph::new(Text::from(input_lines))
            .scroll((scroll_y, 0))
            .style(Style::default().bg(theme::bg()))
            .block(
                // Claude Code's prompt: rules above and below, no sides.
                Block::default()
                    .borders(Borders::TOP | Borders::BOTTOM)
                    .border_type(BorderType::Plain)
                    .border_style(Style::default().fg(border_color))
                    .padding(Padding::horizontal(1))
                    .style(Style::default().bg(theme::bg())),
            ),
        inp_r,
    );
    // The cursor is drawn by `composer_line` as a reverse-video cell,
    // which wraps to the correct line automatically — so we don't set
    // a hardware cursor (it can't track word-wrap and would leave a
    // stray block at the wrong spot on long/wrapped input).
}
