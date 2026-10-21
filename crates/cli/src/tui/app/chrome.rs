//! Claude-style chrome around the composer: the working spinner row above
//! it and the mode / model footer below it.

use super::*;

/// Spinner glyphs, played forward then back.
const SPINNER: [&str; 6] = ["·", "✢", "✳", "✶", "✻", "✽"];
const SPINNER_FRAME_MS: u128 = 120;

/// Verbs for the working line; one is picked per turn.
const VERBS: [&str; 16] = [
    "Thinking",
    "Pondering",
    "Crafting",
    "Brewing",
    "Computing",
    "Musing",
    "Working",
    "Cooking",
    "Forging",
    "Tinkering",
    "Mulling",
    "Reasoning",
    "Weaving",
    "Sketching",
    "Assembling",
    "Considering",
];

fn spinner_glyph(elapsed_ms: u128) -> &'static str {
    let n = SPINNER.len() as u128;
    let step = (elapsed_ms / SPINNER_FRAME_MS) % (2 * n - 2);
    let idx = if step < n { step } else { 2 * n - 2 - step };
    SPINNER[idx as usize]
}

/// The verb for the turn that started at `turn_started_ms` (stable per turn).
pub(super) fn turn_verb(turn_started_ms: Option<u64>) -> &'static str {
    let seed = turn_started_ms.unwrap_or(0) / 7;
    VERBS[(seed % VERBS.len() as u64) as usize]
}

fn compact_count(n: u64) -> String {
    if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// `✶ Pondering… (12s · ↓ 1.2k tokens · esc to interrupt)` while a turn runs.
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
    let accent = Style::default().fg(theme::accent());
    let verb = if g.current_busy_state == BusyState::ToolRunning && !g.process_title.is_empty() {
        // Name the running tool when one is active ("Running bash…").
        let t = g.process_title.trim();
        let mut c = t.chars();
        match c.next() {
            Some(f) => format!("{}{}", f.to_uppercase(), c.as_str()),
            None => turn_verb(g.turn_started_at).to_string(),
        }
    } else {
        turn_verb(g.turn_started_at).to_string()
    };
    let mut detail = format!("({secs}s");
    if g.turn_output_tokens > 0 {
        detail.push_str(&format!(
            " · ↓ {} tokens",
            compact_count(g.turn_output_tokens)
        ));
    }
    detail.push_str(" · esc to interrupt)");
    Some(Line::from(vec![
        Span::styled(
            format!(
                "{} ",
                spinner_glyph(g.busy_state_since.elapsed().as_millis())
            ),
            accent,
        ),
        Span::styled(format!("{verb}… "), accent),
        Span::styled(detail, Style::default().fg(theme::muted())),
    ]))
}

/// Permission-mode indicator for the footer, `None` in the default mode.
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
    if let Some(toast) = g.toast.as_ref().filter(|t| !t.is_expired()) {
        left.push(Span::styled(
            toast.message.clone(),
            Style::default().fg(theme::muted()),
        ));
    } else if let Some((label, color)) = mode_indicator(&g.permission_mode) {
        left.push(Span::styled(label, Style::default().fg(color)));
    } else if g.queued_steering + g.queued_followup > 0 {
        left.push(Span::styled(
            format!("{} queued", g.queued_steering + g.queued_followup),
            Style::default().fg(theme::muted()),
        ));
    } else {
        left.push(Span::styled(
            "? for shortcuts",
            Style::default().fg(theme::muted()),
        ));
    }

    if let Some(badge) = shells_badge(g) {
        left.push(Span::styled(
            format!(" · {badge}"),
            Style::default().fg(theme::accent()),
        ));
    }

    let mut right = g.model.clone();
    let win = dcode_ai_runtime::model_limits::detect_context_window(&g.model) as u64;
    if let Some(custom) = g.statusline_text.as_deref() {
        // `[ui] statusline_command` replaces the built-in right side.
        right = custom.to_string();
    } else if win > 0 && g.context_tokens > 0 {
        let used = (g.context_tokens.min(win) * 100 / win) as u8;
        if used >= 50 {
            right.push_str(&format!(" · {}% context left", 100 - used));
        }
    }
    right.push_str("  ");
    let mut right_spans = Vec::new();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_bounces_through_glyphs() {
        let frames: Vec<&str> = (0..10)
            .map(|i| spinner_glyph(i * SPINNER_FRAME_MS))
            .collect();
        assert_eq!(frames, ["·", "✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳", "✢"]);
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
    fn verb_is_stable_within_a_turn() {
        assert_eq!(turn_verb(Some(1_000)), turn_verb(Some(1_000)));
    }
}
