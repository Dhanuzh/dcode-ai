//! Claude-style chrome around the composer: the working spinner row above
//! it and the mode / model footer below it.

use super::*;

/// dcode-ai's working bar: a lit block sweeping back and forth.
const BAR_CELLS: usize = 6;
const BAR_LIT: usize = 2;
const BAR_FRAME_MS: u128 = 110;

/// `▰▰▱▱▱▱` with the lit block at the frame for `elapsed_ms`.
fn working_bar(elapsed_ms: u128) -> String {
    let span = (BAR_CELLS - BAR_LIT) as u128;
    let step = (elapsed_ms / BAR_FRAME_MS) % (2 * span);
    let at = if step <= span { step } else { 2 * span - step } as usize;
    (0..BAR_CELLS)
        .map(|i| {
            if (at..at + BAR_LIT).contains(&i) {
                '▰'
            } else {
                '▱'
            }
        })
        .collect()
}

/// `●○○`, `○●○`, `○○●` — the `dots` working style.
fn working_dots(elapsed_ms: u128) -> &'static str {
    ["●○○", "○●○", "○○●", "○●○"][((elapsed_ms / 250) % 4) as usize]
}

/// What the agent is doing, in plain words: `thinking`, `writing`, or the
/// running tool (`running bash`, `reading files`).
fn working_label(g: &TuiSessionState) -> String {
    match g.current_busy_state {
        BusyState::Streaming => "writing".into(),
        BusyState::ToolRunning if !g.process_title.trim().is_empty() => {
            g.process_title.trim().to_lowercase()
        }
        BusyState::ToolRunning => "running a tool".into(),
        _ => "thinking".into(),
    }
}

/// The label with a soft highlight sweeping across it.
fn shimmer(label: &str, elapsed_ms: u128) -> Vec<Span<'static>> {
    let chars: Vec<char> = label.chars().collect();
    let n = chars.len().max(1) as u128;
    // Sweep, then rest a moment before the next pass.
    let at = ((elapsed_ms / 70) % (n + 6)) as usize;
    chars
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let style = if i + 1 >= at && i <= at + 1 {
                Style::default()
                    .fg(theme::text())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::accent())
            };
            Span::styled(c.to_string(), style)
        })
        .collect()
}

fn compact_count(n: u64) -> String {
    if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// `▰▰▱▱▱▱ writing · 12s · ↓ 1.2k tokens · esc to stop` while a turn runs.
pub(super) fn spinner_line(g: &TuiSessionState) -> Option<Line<'static>> {
    let busy = g.busy || g.current_busy_state.is_active();
    if !busy || g.active_approval.is_some() {
        return None;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let started = g.turn_started_at.unwrap_or(now_ms);
    let secs = now_ms.saturating_sub(started) / 1000;
    let took = if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    };
    let muted = Style::default().fg(theme::muted());
    let anim = now_ms as u128;
    let mut spans = match g.working_style.as_str() {
        "plain" => vec![Span::styled("● ", Style::default().fg(theme::accent()))],
        "dots" => vec![Span::styled(
            format!("{} ", working_dots(anim)),
            Style::default().fg(theme::accent()),
        )],
        _ => vec![
            Span::styled(working_bar(anim), Style::default().fg(theme::accent())),
            Span::raw(" "),
        ],
    };
    let label = working_label(g);
    if g.shimmer && g.working_style != "plain" {
        spans.extend(shimmer(&label, anim));
    } else {
        spans.push(Span::styled(label, Style::default().fg(theme::accent())));
    }
    let mut detail = format!(" · {took}");
    if g.turn_output_tokens > 0 {
        detail.push_str(&format!(
            " · ↓ {} tokens",
            compact_count(g.turn_output_tokens)
        ));
    }
    detail.push_str(" · esc to stop");
    spans.push(Span::styled(detail, muted));
    Some(Line::from(spans))
}

/// Permission-mode indicator for the footer, `None` in the default mode.
/// Rows for messages typed during a turn: `↳ text · next step` for ones the
/// agent reads at its next step, `↳ text · after this turn` for queued ones,
/// plus a hint row. At most four messages are listed.
pub(super) fn queued_lines(g: &TuiSessionState, width: usize) -> Vec<Line<'static>> {
    let steering = g.steering_pending();
    let total = steering.len() + g.followup_queue.len();
    if total == 0 {
        return Vec::new();
    }
    let muted = Style::default().fg(theme::muted());
    let text = Style::default().fg(theme::text());
    let rows = steering.iter().map(|m| (m.as_str(), "next step")).chain(
        g.followup_queue
            .iter()
            .map(|m| (m.as_str(), "after this turn")),
    );
    let mut out: Vec<Line<'static>> = Vec::new();
    for (msg, when) in rows.take(4) {
        let first = msg
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        let room = width.saturating_sub(when.len() + 7).max(8);
        let shown: String = if first.chars().count() > room {
            first
                .chars()
                .take(room.saturating_sub(1))
                .chain(['…'])
                .collect()
        } else {
            first.to_string()
        };
        out.push(Line::from(vec![
            Span::styled("  ↳ ", muted),
            Span::styled(shown, text),
            Span::styled(format!(" · {when}"), muted),
        ]));
    }
    let more = total.saturating_sub(4);
    let hint = if more > 0 {
        format!("    +{more} more · ↑ to edit")
    } else {
        "    ↑ to edit".to_string()
    };
    out.push(Line::from(Span::styled(hint, muted)));
    out
}

pub(super) fn mode_indicator(permission_mode: &str) -> Option<(String, Color)> {
    let m = permission_mode
        .to_ascii_lowercase()
        .replace(['-', '_', ' '], "");
    let (label, color) = match m.as_str() {
        "acceptedits" => ("⏵⏵ accept edits on", Color::Rgb(175, 135, 255)),
        "plan" => ("⏸ plan mode on", Color::Rgb(72, 168, 158)),
        "bypasspermissions" => ("⏵⏵ bypass permissions on", theme::error()),
        "dontask" => ("⏵⏵ don't ask mode on", theme::warn()),
        "auto" => ("⏵⏵ auto mode on", Color::Rgb(232, 178, 64)),
        _ => return None,
    };
    Some((format!("{label} (shift+tab to cycle)"), color))
}

/// `1 shell` / `3 shells` while the agent's background jobs run.
pub(super) fn shells_badge(g: &TuiSessionState) -> Option<String> {
    match g.background_shells.len() {
        0 => None,
        1 => Some("1 shell".into()),
        n => Some(format!("{n} shells")),
    }
}

/// `(ready, total, failed)` MCP servers when any is not ready yet.
pub(super) fn mcp_health(g: &TuiSessionState) -> Option<(usize, usize, usize)> {
    use dcode_ai_common::event::McpStartupStatus;
    let total = g.mcp_server_count.max(g.mcp_server_statuses.len());
    if total == 0 {
        return None;
    }
    let ready = g
        .mcp_server_statuses
        .values()
        .filter(|s| matches!(s, McpStartupStatus::Ready))
        .count();
    let failed = g
        .mcp_server_statuses
        .values()
        .filter(|s| matches!(s, McpStartupStatus::Failed { .. }))
        .count();
    (ready < total).then_some((ready, total, failed))
}

/// Footer under the composer: mode indicator or `? for shortcuts` on the
/// left; model and context use on the right.
pub(super) fn render_footer(frame: &mut ratatui::Frame<'_>, rect: Rect, g: &TuiSessionState) {
    if rect.height == 0 {
        return;
    }
    let mut left: Vec<Span<'static>> = vec![Span::raw("  ")];
    if g.vim_enabled {
        let mode = if g.vim_normal {
            "-- NORMAL -- "
        } else {
            "-- INSERT -- "
        };
        left.push(Span::styled(mode, Style::default().fg(theme::muted())));
    }
    if let Some((_, hint)) = g
        .exit_hint
        .filter(|(until, _)| std::time::Instant::now() < *until)
    {
        left.push(Span::styled(hint, Style::default().fg(theme::muted())));
    } else if g.input_buffer.starts_with('!') {
        left.push(Span::styled(
            "! for shell mode",
            Style::default().fg(theme::bash_border()),
        ));
    } else if g.pasting {
        left.push(Span::styled(
            "⧗ pasting image…",
            Style::default().fg(theme::accent()),
        ));
    } else if let Some(toast) = g.toast.as_ref().filter(|t| !t.is_expired()) {
        left.push(Span::styled(
            toast.message.clone(),
            Style::default().fg(theme::muted()),
        ));
    } else if (g.busy || g.current_busy_state.is_active()) && !g.input_buffer.trim().is_empty() {
        left.push(Span::styled(
            "enter send now · tab queue for after",
            Style::default().fg(theme::muted()),
        ));
    } else if let Some((label, color)) = mode_indicator(&g.permission_mode) {
        left.push(Span::styled(label, Style::default().fg(color)));
    } else {
        left.push(Span::styled(
            "? for shortcuts",
            Style::default().fg(theme::muted()),
        ));
    }

    if let Some(goal) = &g.goal_label {
        left.push(Span::styled(
            format!(" · {goal}"),
            Style::default().fg(theme::accent()),
        ));
    }

    if let Some(badge) = shells_badge(g) {
        left.push(Span::styled(
            format!(" · {badge}"),
            Style::default().fg(theme::accent()),
        ));
    }

    let mut right = g.model.clone();
    let win = dcode_ai_runtime::model_limits::effective_context_window(&g.model) as u64;
    let mut context_warning = None;
    if let Some(custom) = g.statusline_text.as_deref() {
        // `[ui] statusline_command` replaces the built-in right side.
        right = custom.to_string();
    } else if let Some((text, warn)) = context_note(
        g.context_tokens,
        win,
        dcode_ai_runtime::context_manager::auto_compact_percent(&g.model) as u64,
    ) {
        if warn.is_none() {
            right.push_str(&format!(" · {text}"));
        } else {
            context_warning = Some((text, warn));
        }
    }
    right.push_str("  ");
    let mut right_spans = Vec::new();
    if let Some((text, Some(color))) = context_warning {
        right_spans.push(Span::styled(
            format!("{text} · "),
            Style::default().fg(color),
        ));
    }
    if let Some((ready, total, failed)) = mcp_health(g) {
        // Only news is shown: servers still starting or failed.
        let color = if failed > 0 {
            theme::error()
        } else {
            theme::warn()
        };
        right_spans.push(Span::styled(
            format!("mcp {ready}/{total} · "),
            Style::default().fg(color),
        ));
    }
    right_spans.push(Span::styled(right, Style::default().fg(theme::muted())));

    let lw: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let rw: usize = right_spans.iter().map(|s| s.content.chars().count()).sum();
    let gap = (rect.width as usize).saturating_sub(lw + rw);
    left.push(Span::raw(" ".repeat(gap)));
    left.extend(right_spans);
    frame.render_widget(Paragraph::new(Line::from(left)), rect);
}

/// Context use for the footer, like Claude Code: nothing below half full,
/// then how far automatic compaction is (`compact_at` = its point as % of
/// the window); amber in the last 10 points before it. Compaction runs on
/// its own, so there is no "/compact" nag.
fn context_note(
    tokens: u64,
    window: u64,
    compact_at: u64,
) -> Option<(String, Option<ratatui::style::Color>)> {
    if window == 0 || tokens == 0 {
        return None;
    }
    let used = tokens.min(window) * 100 / window;
    let compact_at = compact_at.clamp(10, 100);
    if used < 50 {
        return None;
    }
    if used >= compact_at {
        return Some(("compacting soon".into(), Some(theme::warn())));
    }
    let left = compact_at - used;
    let text = format!("{left}% until auto-compact");
    Some((text, (left <= 10).then(theme::warn)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_note_warns_near_the_limit() {
        assert_eq!(context_note(40, 100, 77), None);
        let calm = context_note(60, 100, 77).unwrap();
        assert_eq!(calm.0, "17% until auto-compact");
        assert!(calm.1.is_none());
        let near = context_note(70, 100, 77).unwrap();
        assert!(near.1.is_some(), "amber close to the point");
        assert_eq!(context_note(80, 100, 77).unwrap().0, "compacting soon");
        assert!(!context_note(95, 100, 77).unwrap().0.contains("/compact"));
    }

    #[test]
    fn working_bar_sweeps_back_and_forth() {
        let frames: Vec<String> = (0..9).map(|i| working_bar(i * BAR_FRAME_MS)).collect();
        assert_eq!(frames[0], "▰▰▱▱▱▱");
        assert_eq!(frames[4], "▱▱▱▱▰▰");
        assert_eq!(frames[8], "▰▰▱▱▱▱");
    }

    #[test]
    fn mode_indicator_matches_claude_wording() {
        assert_eq!(
            mode_indicator("AcceptEdits").unwrap().0,
            "⏵⏵ accept edits on (shift+tab to cycle)"
        );
        assert!(
            mode_indicator("Plan")
                .unwrap()
                .0
                .starts_with("⏸ plan mode on")
        );
        assert_eq!(
            mode_indicator("Auto").unwrap().0,
            "⏵⏵ auto mode on (shift+tab to cycle)"
        );
        assert!(mode_indicator("Default").is_none());
    }

    #[test]
    fn mcp_health_only_reports_problems() {
        use dcode_ai_common::event::McpStartupStatus;
        let mut g = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "Default".into(),
            std::path::PathBuf::from("."),
            false,
        );
        assert_eq!(mcp_health(&g), None);
        g.mcp_server_count = 2;
        g.mcp_server_statuses
            .insert("a".into(), McpStartupStatus::Ready);
        g.mcp_server_statuses.insert(
            "b".into(),
            McpStartupStatus::Failed {
                message: "boom".into(),
            },
        );
        assert_eq!(mcp_health(&g), Some((1, 2, 1)));
        g.mcp_server_statuses
            .insert("b".into(), McpStartupStatus::Ready);
        assert_eq!(mcp_health(&g), None);
    }

    #[test]
    fn working_label_says_what_is_happening() {
        let mut g = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "Default".into(),
            std::path::PathBuf::from("."),
            false,
        );
        g.current_busy_state = BusyState::Streaming;
        assert_eq!(working_label(&g), "writing");
        g.current_busy_state = BusyState::ToolRunning;
        g.process_title = "Running bash".into();
        assert_eq!(working_label(&g), "running bash");
        g.current_busy_state = BusyState::Thinking;
        assert_eq!(working_label(&g), "thinking");
    }
}
