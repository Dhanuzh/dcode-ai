//! Full-screen dashboard (hybrid UI, `Ctrl+\` / `/dashboard`):
//! session rail · session pane · context rail, on the alternate screen.

use super::frame::draw_session_pane;
use super::*;
use crate::tui::diff_review::DiffReview;
use crate::tui::render_helpers::truncate_chars;
use crate::tui::state::{DashTarget, RailTab};

const LEFT_W: u16 = 26;
const RIGHT_W: u16 = 42;
/// Both rails need this many columns to leave a usable centre pane.
const BOTH_RAILS_MIN_W: u16 = 120;
/// Below this only the centre pane is shown.
const RIGHT_RAIL_MIN_W: u16 = 90;

/// Narrowest a dragged rail can get, and the centre pane's minimum.
pub(super) const RAIL_MIN_W: u16 = 16;
pub(super) const CENTRE_MIN_W: u16 = 40;

/// Split the screen into (session rail, centre pane, context rail).
/// `left_w` / `right_w` are the widths the user dragged to (0 = default).
pub(super) fn dashboard_columns(
    area: Rect,
    left_w: u16,
    right_w: u16,
) -> (Option<Rect>, Rect, Option<Rect>) {
    let right_w = if right_w == 0 { RIGHT_W } else { right_w };
    let left_w = if left_w == 0 { LEFT_W } else { left_w };
    if area.width >= BOTH_RAILS_MIN_W.max(left_w + right_w + CENTRE_MIN_W) {
        let cols = Layout::horizontal([
            Constraint::Length(left_w),
            Constraint::Min(CENTRE_MIN_W),
            Constraint::Length(right_w),
        ])
        .split(area);
        (Some(cols[0]), cols[1], Some(cols[2]))
    } else if area.width >= RIGHT_RAIL_MIN_W.max(right_w + CENTRE_MIN_W) {
        let cols = Layout::horizontal([Constraint::Min(CENTRE_MIN_W), Constraint::Length(right_w)])
            .split(area);
        (None, cols[0], Some(cols[1]))
    } else {
        (None, area, None)
    }
}

/// Draw one dashboard frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_dashboard_frame(
    frame: &mut ratatui::Frame<'_>,
    g: &mut TuiSessionState,
    slash_filtered: &[&SlashEntry],
    at_matches: &[String],
    chrome_h: u16,
    transcript_cache: &mut TranscriptRenderCache,
) {
    let area = frame.area();
    frame.render_widget(ClearWidget, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::bg())),
        area,
    );

    let (left, centre, right) = dashboard_columns(area, g.dash_hits.left_w, g.dash_hits.right_w);
    g.dash_hits.targets.clear();
    g.dash_hits.transcript = None;
    g.dash_hits.left_rail = left;
    g.dash_hits.right_rail = right;
    if let Some(rect) = left {
        render_session_rail(frame, rect, g);
    }
    if let Some(rect) = right {
        render_right_rail(frame, rect, g);
    }
    draw_session_pane(
        frame,
        centre,
        g,
        slash_filtered,
        at_matches,
        chrome_h,
        transcript_cache,
    );
}

fn rail_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::border()))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1))
        .style(Style::default().bg(theme::bg()))
}

fn heading(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(theme::muted())
            .add_modifier(Modifier::BOLD),
    ))
}

fn muted(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(theme::muted()),
    ))
}

fn plain(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(theme::text()),
    ))
}

fn wrapped(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    textwrap::wrap(text, width.max(8))
        .into_iter()
        .map(|l| Line::from(Span::styled(l.into_owned(), style)))
        .collect()
}

/// Last 8 chars of the most distinctive (longest) `-`-separated segment:
/// `session-1791887456044093-0` → `56044093`, `…-child-abcdef12` → `abcdef12`.
fn short_id(id: &str) -> &str {
    let seg = id
        .split('-')
        .filter(|s| s.chars().any(|c| c.is_ascii_digit()))
        .max_by_key(|s| s.len())
        .unwrap_or(id);
    if seg.len() > 8 {
        &seg[seg.len() - 8..]
    } else {
        seg
    }
}

#[cfg(test)]
fn session_rail_lines(g: &TuiSessionState, width: usize) -> Vec<Line<'static>> {
    session_rail_layout(g, width).0
}

/// Session rail lines plus `(line index, sub-agent index)` for click mapping.
fn session_rail_layout(
    g: &TuiSessionState,
    width: usize,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    let mut agent_rows = Vec::new();
    let mut lines = vec![
        Line::from(vec![
            Span::styled("◆ ", Style::default().fg(theme::accent())),
            Span::styled(
                g.model.clone(),
                Style::default()
                    .fg(theme::text())
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        match super::chrome::mode_indicator(&g.permission_mode) {
            // Short form of the footer's mode indicator.
            Some((label, color)) => Line::from(Span::styled(
                label.split(" (").next().unwrap_or(&label).to_string(),
                Style::default().fg(color),
            )),
            None => muted(format!("{} · default mode", g.agent_profile)),
        },
    ];
    if !g.session_id.is_empty() {
        lines.push(muted(format!("session {}", short_id(&g.session_id))));
    }
    lines.push(Line::default());
    lines.push(heading("agents"));

    let (dot, color, state) = if g.busy || g.current_busy_state.is_active() {
        ("●", theme::warn(), "working")
    } else {
        ("●", theme::success(), "idle")
    };
    lines.push(Line::from(vec![
        Span::styled(format!("{dot} "), Style::default().fg(color)),
        Span::styled("main ", Style::default().fg(theme::text())),
        Span::styled(state, Style::default().fg(theme::muted())),
    ]));
    for (idx, agent) in g.subagents.iter().enumerate() {
        agent_rows.push((lines.len(), idx));
        let (dot, color) = if agent.running {
            ("◐", theme::warn())
        } else {
            ("○", theme::muted())
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{dot} "), Style::default().fg(color)),
            Span::styled(
                format!("{} ", short_id(&agent.id)),
                Style::default().fg(theme::text()),
            ),
            Span::styled(agent.phase.clone(), Style::default().fg(theme::muted())),
        ]));
        if !agent.task.is_empty() {
            let task = truncate_chars(&agent.task, width.saturating_sub(2) * 2);
            for l in wrapped(
                &task,
                width.saturating_sub(2),
                Style::default().fg(theme::muted()),
            ) {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(l.spans);
                agent_rows.push((lines.len(), idx));
                lines.push(Line::from(spans));
            }
        }
    }
    if g.subagents.is_empty() {
        lines.push(muted("  no sub-agents"));
    } else {
        lines.push(muted("click / Ctrl+G attach"));
    }
    lines.push(Line::default());
    lines.push(muted("Ctrl+\\ inline view"));
    (lines, agent_rows)
}

pub(super) fn context_rail_lines(g: &TuiSessionState, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![heading("plan")];
    match g.current_plan.as_deref().map(str::trim) {
        Some(plan) if !plan.is_empty() => {
            for raw in plan.lines() {
                lines.extend(wrapped(raw, width, Style::default().fg(theme::text())));
            }
        }
        _ => lines.push(muted("no plan yet")),
    }

    lines.push(Line::default());
    lines.push(heading("usage"));
    lines.push(plain(format!(
        "in {} · out {}",
        compact_tokens(g.input_tokens),
        compact_tokens(g.output_tokens)
    )));
    if dcode_ai_common::config::per_token_billing() {
        lines.push(plain(format!("cost ${:.4} (est.)", g.cost_usd)));
    } else {
        lines.push(muted("on your plan · /usage"));
    }

    let window = dcode_ai_runtime::model_limits::effective_context_window(&g.model) as u64;
    if window > 0 && g.context_tokens > 0 {
        let pct = (g.context_tokens.saturating_mul(100) / window).min(100);
        let filled = (pct as usize * 10 / 100).min(10);
        let color = if pct >= 85 {
            theme::error()
        } else if pct >= 60 {
            theme::warn()
        } else {
            theme::success()
        };
        lines.push(Line::from(vec![
            Span::styled("ctx ", Style::default().fg(theme::text())),
            Span::styled(
                format!("{}{}", "▰".repeat(filled), "▱".repeat(10 - filled)),
                Style::default().fg(color),
            ),
            Span::styled(format!(" {pct}%"), Style::default().fg(theme::text())),
        ]));
        lines.push(muted(format!(
            "{} / {}",
            compact_tokens(g.context_tokens),
            compact_tokens(window)
        )));
    }
    if g.context_compacted {
        lines.push(muted("context compacted"));
    }
    if !g.mcp_server_statuses.is_empty() {
        use dcode_ai_common::event::McpStartupStatus;
        lines.push(Line::default());
        lines.push(heading("mcp"));
        let mut servers: Vec<_> = g.mcp_server_statuses.iter().collect();
        servers.sort_by(|a, b| a.0.cmp(b.0));
        for (name, status) in servers {
            let (dot, color) = match status {
                McpStartupStatus::Ready => ("●", theme::success()),
                McpStartupStatus::Failed { .. } => ("●", theme::error()),
                _ => ("◐", theme::warn()),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{dot} "), Style::default().fg(color)),
                Span::styled(
                    truncate_chars(name, width.saturating_sub(2)),
                    Style::default().fg(theme::text()),
                ),
            ]));
        }
    }
    lines
}

fn compact_tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1_000.0),
        n => n.to_string(),
    }
}

fn render_session_rail(frame: &mut ratatui::Frame<'_>, rect: Rect, g: &mut TuiSessionState) {
    let block = rail_block("session");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let (lines, agent_rows) = session_rail_layout(g, inner.width as usize);
    for (line, idx) in agent_rows {
        if (line as u16) < inner.height {
            let row = Rect::new(inner.x, inner.y + line as u16, inner.width, 1);
            g.dash_hits.targets.push((row, DashTarget::Subagent(idx)));
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_right_rail(frame: &mut ratatui::Frame<'_>, rect: Rect, g: &mut TuiSessionState) {
    let focused = g.rail_focus;

    // Tabs live in the top border: `╭ context │ changes ─╮`, each clickable.
    let tabs = [
        (RailTab::Context, "context"),
        (RailTab::Changes, "changes"),
        (RailTab::Agents, "agents"),
        (RailTab::Files, "files"),
        (RailTab::Office, "office"),
    ];
    let mut spans = vec![Span::raw(" ")];
    let mut x = rect.x + 2;
    for (i, (tab, label)) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("│", Style::default().fg(theme::border())));
            x += 1;
        }
        let style = if *tab == g.rail_tab {
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::muted())
        };
        let w = label.chars().count() as u16;
        g.dash_hits
            .targets
            .push((Rect::new(x, rect.y, w, 1), DashTarget::Tab(*tab)));
        spans.push(Span::styled(*label, style));
        x += w;
    }
    spans.push(Span::raw(" "));

    let border = if focused {
        theme::accent()
    } else {
        theme::border()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Line::from(spans))
        .title_bottom(Line::from(Span::styled(
            if focused {
                " ←/→ tabs · Esc back "
            } else {
                " Alt+1-5 · Ctrl+D focus "
            },
            Style::default().fg(theme::muted()),
        )))
        .padding(Padding::horizontal(1))
        .style(Style::default().bg(theme::bg()));
    let body = block.inner(rect);
    frame.render_widget(block, rect);
    if body.height == 0 {
        return;
    }
    match g.rail_tab {
        RailTab::Context => frame.render_widget(
            Paragraph::new(Text::from(context_rail_lines(g, body.width as usize))),
            body,
        ),
        RailTab::Changes => {
            let (lines, item_rows) = changes_lines(
                &g.diff_review,
                body.width as usize,
                body.height as usize,
                focused,
            );
            for (line, item) in item_rows {
                let row = Rect::new(body.x, body.y + line as u16, body.width, 1);
                g.dash_hits
                    .targets
                    .push((row, DashTarget::ReviewItem(item)));
            }
            frame.render_widget(Paragraph::new(Text::from(lines)), body);
        }
        RailTab::Agents => frame.render_widget(
            Paragraph::new(Text::from(agents_lines(
                &g.subagents,
                body.width as usize,
                body.height as usize,
            ))),
            body,
        ),
        RailTab::Office => {
            use crate::tui::office::PersonRole;
            g.office.tidy();
            let tick = crate::tui::app::office_tick();
            // Who is here, under the room.
            let mut here: Vec<Line> = vec![muted("here now")];
            for p in g.office.people_ordered() {
                let (role, color) = match p.role {
                    PersonRole::Owner => ("you", theme::accent()),
                    PersonRole::Joined => ("joined", ratatui::style::Color::Rgb(0x3f, 0x7f, 0xbf)),
                    PersonRole::Watching => ("watching", theme::muted()),
                };
                here.push(Line::from(vec![
                    Span::styled("  ● ", Style::default().fg(color)),
                    Span::styled(
                        truncate_chars(&p.name, (body.width as usize).saturating_sub(16)),
                        Style::default().fg(theme::text()),
                    ),
                    Span::styled(format!("  {role}"), Style::default().fg(theme::muted())),
                ]));
            }
            let list_h = (here.len() as u16 + 1).min(body.height / 3);
            let n = g.office.stations().len();
            let (_, room_h) = crate::tui::office::office_layout(
                body.width,
                body.height.saturating_sub(list_h),
                n,
            );
            let room = Rect {
                height: room_h.min(body.height),
                ..body
            };
            frame.render_widget(
                crate::tui::office::OfficeView {
                    office: &g.office,
                    tick,
                    selected: None,
                },
                room,
            );
            let below = Rect {
                y: room.y + room.height + 1,
                height: body.height.saturating_sub(room.height + 1),
                ..body
            };
            if below.height > 0 {
                frame.render_widget(Paragraph::new(Text::from(here)), below);
            }
        }
        RailTab::Files => frame.render_widget(
            Paragraph::new(Text::from(files_lines(
                &g.file_map,
                body.width as usize,
                body.height as usize,
            ))),
            body,
        ),
    }
}

/// The files the agent touched as a tree: edits bright with their count,
/// reads dim, the file it is on now (or just touched) marked `◉`.
pub(super) fn files_lines(
    map: &crate::tui::activity::FileMap,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    use crate::tui::activity::{ago, fresh};
    if map.files.is_empty() {
        return vec![
            muted("no files touched yet"),
            Line::default(),
            muted("files the agent reads and"),
            muted("edits light up here live"),
        ];
    }
    let mut lines = vec![muted(format!(
        "{} files · {} edited",
        map.files.len(),
        map.edited()
    ))];
    let tree = map.tree();
    // The newest stay visible when the tree is long.
    let room = height.saturating_sub(1);
    let skip = tree.len().saturating_sub(room);
    for (depth, name, touch) in tree.into_iter().skip(skip) {
        let indent = "  ".repeat(depth);
        let Some(t) = touch else {
            lines.push(muted(truncate_chars(&format!("{indent}{name}"), width)));
            continue;
        };
        let live = fresh(t);
        let (mark, color) = if live {
            ("◉ ", theme::accent())
        } else if t.edits > 0 {
            ("✎ ", theme::warn())
        } else {
            ("· ", theme::muted())
        };
        let counts = if t.edits > 0 {
            format!(" {}✎", t.edits)
        } else {
            format!(" {}r", t.reads)
        };
        let when = format!(" {}", ago(t.last_at));
        let room = width.saturating_sub(indent.len() + 2 + counts.len() + when.len());
        let mut style = Style::default().fg(if t.edits > 0 {
            theme::text()
        } else {
            theme::muted()
        });
        if live {
            style = style.add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(vec![
            Span::raw(indent),
            Span::styled(mark, Style::default().fg(color)),
            Span::styled(truncate_chars(&name, room), style),
            Span::styled(counts, Style::default().fg(color)),
            Span::styled(when, Style::default().fg(theme::muted())),
        ]));
    }
    lines
}

/// Live sub-agent activity: running agents first, each with its task and
/// as many recent activity lines as fit.
pub(super) fn agents_lines(
    agents: &[crate::tui::state::SubagentRow],
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    if agents.is_empty() {
        return vec![
            muted("no sub-agents yet"),
            Line::default(),
            muted("the agent starts them with"),
            muted("spawn_subagent; their tool"),
            muted("activity streams here"),
        ];
    }
    let mut order: Vec<&crate::tui::state::SubagentRow> = agents.iter().collect();
    order.sort_by_key(|a| !a.running);
    // Header (+ task line + gap) per agent; the rest is shared by logs.
    let fixed = order.len() * 3;
    let per_log = (height.saturating_sub(fixed) / order.len()).max(1);
    let mut lines = Vec::new();
    for a in order {
        let (dot, color) = if a.running {
            ("◐", theme::warn())
        } else {
            ("●", theme::success())
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{dot} "), Style::default().fg(color)),
            Span::styled(
                short_id(&a.id).to_string(),
                Style::default()
                    .fg(theme::text())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}", if a.running { "running" } else { "done" }),
                Style::default().fg(theme::muted()),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            truncate_chars(&a.task, width),
            Style::default().fg(theme::text()),
        )));
        let start = a.log.len().saturating_sub(per_log);
        for entry in &a.log[start..] {
            lines.push(Line::from(Span::styled(
                format!("  {}", truncate_chars(entry, width.saturating_sub(2))),
                Style::default().fg(theme::muted()),
            )));
        }
        lines.push(Line::default());
    }
    lines
}

/// Lines for the changes tab — a summary, then one row per file — scrolled
/// so the selected file is visible, plus `(line index, review item)` for
/// each clickable row (relative to the body). The diff itself opens full
/// screen (Enter / click).
pub(super) fn changes_lines(
    review: &DiffReview,
    width: usize,
    height: usize,
    focused: bool,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    let footer = if focused {
        "Enter full diff · j/k file · Esc"
    } else {
        "click a file · Ctrl+D focus"
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rows: Vec<(usize, usize)> = Vec::new();

    if let Some(err) = &review.error {
        lines.extend(wrapped(err, width, Style::default().fg(theme::error())));
    } else if review.loaded && review.files.is_empty() {
        lines.push(muted("no unstaged changes"));
    } else if !review.loaded {
        lines.push(muted("press Ctrl+D to load"));
    } else {
        let added: usize = review.files.iter().map(|f| f.added).sum();
        let removed: usize = review.files.iter().map(|f| f.removed).sum();
        let n = review.files.len();
        lines.push(Line::from(vec![
            Span::styled(
                format!("{n} file{} ", if n == 1 { "" } else { "s" }),
                Style::default()
                    .fg(theme::text())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("+{added}"), Style::default().fg(theme::success())),
            Span::styled(format!(" −{removed}"), Style::default().fg(theme::error())),
        ]));
        lines.push(Line::default());
    }

    let header_rows = lines.len();
    let selected = review.selected_file();
    for (fi, file) in review.files.iter().enumerate() {
        let sel = fi == selected;
        let counts = if file.removed > 0 {
            format!("+{} −{}", file.added, file.removed)
        } else {
            format!("+{}", file.added)
        };
        // File name first (it says the most), its folder dimmed after.
        let (dir, name) = match file.path.rsplit_once('/') {
            Some((d, n)) => (format!(" {d}/"), n.to_string()),
            None => (String::new(), file.path.clone()),
        };
        let room = width.saturating_sub(counts.chars().count() + 3);
        let name = truncate_chars(&name, room);
        let dir = truncate_chars(&dir, room.saturating_sub(name.chars().count()));
        let pad = width.saturating_sub(
            2 + name.chars().count() + dir.chars().count() + counts.chars().count(),
        );
        let name_style = if sel && focused {
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else if sel {
            Style::default().fg(theme::accent())
        } else {
            Style::default().fg(theme::text())
        };
        rows.push((lines.len(), review.first_item_of(fi)));
        lines.push(Line::from(vec![
            Span::styled(
                if sel { "❯ " } else { "  " },
                Style::default().fg(theme::accent()),
            ),
            Span::styled(name, name_style),
            Span::styled(dir, Style::default().fg(theme::muted())),
            Span::raw(" ".repeat(pad)),
            Span::styled(
                format!("+{}", file.added),
                Style::default().fg(theme::success()),
            ),
            Span::styled(
                if file.removed > 0 {
                    format!(" −{}", file.removed)
                } else {
                    String::new()
                },
                Style::default().fg(theme::error()),
            ),
        ]));
    }

    // Keep the selected file on screen; the footer takes the last two rows.
    let avail = height.saturating_sub(2).max(1);
    let selected_line = header_rows + selected;
    let offset = (selected_line + 1).saturating_sub(avail);
    let mut visible: Vec<Line<'static>> = lines.into_iter().skip(offset).take(avail).collect();
    let rows = rows
        .into_iter()
        .filter(|(l, _)| *l >= offset && *l < offset + avail)
        .map(|(l, i)| (l - offset, i))
        .collect();
    while visible.len() < avail {
        visible.push(Line::default());
    }
    visible.push(Line::default());
    visible.push(muted(footer));
    (visible, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(lines: &[Line<'_>]) -> String {
        lines
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
    fn short_ids_use_the_distinctive_segment() {
        assert_eq!(short_id("session-1791887456044093-0"), "56044093");
        assert_eq!(short_id("session-123-child-abcdef12"), "abcdef12");
    }

    #[test]
    fn columns_adapt_to_width() {
        let (l, c, r) = dashboard_columns(Rect::new(0, 0, 160, 40), 0, 0);
        assert!(l.is_some() && r.is_some());
        assert_eq!(c.width, 160 - LEFT_W - RIGHT_W);

        let (l, _, r) = dashboard_columns(Rect::new(0, 0, 100, 40), 0, 0);
        assert!(l.is_none() && r.is_some());

        let (l, c, r) = dashboard_columns(Rect::new(0, 0, 70, 40), 0, 0);
        assert!(l.is_none() && r.is_none());
        assert_eq!(c.width, 70);

        // Dragged widths are used as given.
        let (l, c, r) = dashboard_columns(Rect::new(0, 0, 160, 40), 20, 50);
        assert_eq!((l.unwrap().width, c.width, r.unwrap().width), (20, 90, 50));
    }

    #[test]
    fn rails_show_agents_plan_and_usage() {
        let mut g = TuiSessionState::new(
            "session-1".into(),
            "claude-sonnet-5-5".into(),
            "@build".into(),
            "default".into(),
            std::path::PathBuf::from("."),
            false,
        );
        g.subagents.push(crate::tui::state::SubagentRow {
            id: "session-123-child-abcdef12".into(),
            task: "fix the failing tests".into(),
            phase: "running".into(),
            detail: String::new(),
            running: true,
            skill: None,
            log: Vec::new(),
        });
        g.current_plan = Some("1. read code\n2. fix bug".into());
        g.input_tokens = 12_400;
        g.output_tokens = 900;

        let left = text_of(&session_rail_lines(&g, 24));
        assert!(left.contains("claude-sonnet-5-5"));
        assert!(left.contains("main"));
        assert!(left.contains("abcdef12"));
        assert!(left.contains("fix the failing tests"));

        let right = text_of(&context_rail_lines(&g, 30));
        assert!(right.contains("2. fix bug"));
        assert!(right.contains("in 12.4k · out 900"));
    }

    #[test]
    fn agents_tab_shows_running_first_with_recent_activity() {
        let row = |id: &str, running: bool, log: &[&str]| crate::tui::state::SubagentRow {
            id: id.into(),
            task: format!("task {id}"),
            phase: String::new(),
            detail: String::new(),
            running,
            skill: None,
            log: log.iter().map(|s| s.to_string()).collect(),
        };
        let agents = vec![
            row("child-aaaa1111", false, &["read_file · a.rs"]),
            row("child-bbbb2222", true, &["one", "two", "three", "four"]),
        ];
        let text = text_of(&agents_lines(&agents, 30, 12));
        let running = text.find("bbbb2222").unwrap();
        let done = text.find("aaaa1111").unwrap();
        assert!(running < done, "{text}");
        assert!(text.contains("four") && !text.contains("  one"), "{text}");
        assert!(text_of(&agents_lines(&[], 30, 10)).contains("no sub-agents"));
    }

    #[test]
    fn ctrl_backslash_and_legacy_ctrl_4_toggle_the_view() {
        use crate::tui::app::keys::is_view_toggle;
        assert!(is_view_toggle(&KeyEvent::new(
            KeyCode::Char('\\'),
            KeyModifiers::CONTROL
        )));
        assert!(is_view_toggle(&KeyEvent::new(
            KeyCode::Char('4'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_view_toggle(&KeyEvent::new(
            KeyCode::Char('\\'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn snapshot_dashboard_frame() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut g = TuiSessionState::new(
            "session-1".into(),
            "claude-sonnet-5-5".into(),
            "@build".into(),
            "default".into(),
            std::path::PathBuf::from("."),
            false,
        );
        g.view_mode = crate::tui::state::ViewMode::Dashboard;
        g.blocks
            .push(DisplayBlock::User("fix the expiry check".into()));
        g.blocks.push(DisplayBlock::Assistant(
            "Fixed the off-by-one in token expiry.".into(),
        ));
        g.current_plan = Some("1. read auth.rs\n2. fix comparison".into());
        g.input_tokens = 12_400;
        g.output_tokens = 900;
        g.touch_transcript();

        let mut terminal = Terminal::new(TestBackend::new(130, 22)).expect("terminal");
        let mut cache = TranscriptRenderCache::default();
        terminal
            .draw(|frame| draw_dashboard_frame(frame, &mut g, &[], &[], 0, &mut cache))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let text: String = (0..buffer.area.height)
            .map(|y| {
                let row: String = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect();
                row.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // The status bar shows a live session timer; keep the snapshot stable.
        let text = mask_timer(&text);
        assert!(text.contains("session"));
        assert!(text.contains("context"));
        assert!(text.contains("Fixed the off-by-one"));
        insta::assert_snapshot!("dashboard_frame", text);
    }

    #[test]
    fn changes_tab_lists_files_and_records_click_targets() {
        use crate::tui::diff_hunk::parse_diff_hunks;
        use crate::tui::diff_review::FileChange;
        let mut review = DiffReview {
            loaded: true,
            ..Default::default()
        };
        review.files.push(FileChange {
            path: "src/auth.rs".into(),
            untracked: false,
            added: 2,
            removed: 2,
            hunks: parse_diff_hunks(
                &(1..=30).map(|i| format!("l{i}\n")).collect::<String>(),
                &(1..=30)
                    .map(|i| match i {
                        2 => "two\n".to_string(),
                        28 => "twenty-eight\n".to_string(),
                        _ => format!("l{i}\n"),
                    })
                    .collect::<String>(),
            ),
        });
        review.cursor = 1;

        let (lines, rows) = changes_lines(&review, 30, 20, true);
        let text = text_of(&lines);
        assert!(text.contains("1 file +2 −2"), "{text}");
        assert!(text.contains("❯ auth.rs src/"), "{text}");
        assert!(
            !text.contains("twenty-eight"),
            "no hunk previews in the rail"
        );
        assert!(text.contains("Enter full diff"));
        // One click target per file, at its first hunk.
        assert_eq!(rows.iter().map(|(_, i)| *i).collect::<Vec<_>>(), vec![0]);

        // Tabs and the selected hunk row become click targets.
        let mut g = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            std::path::PathBuf::from("."),
            false,
        );
        g.view_mode = crate::tui::state::ViewMode::Dashboard;
        g.rail_tab = RailTab::Changes;
        g.diff_review = review;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(130, 24)).expect("terminal");
        let mut cache = TranscriptRenderCache::default();
        terminal
            .draw(|frame| draw_dashboard_frame(frame, &mut g, &[], &[], 0, &mut cache))
            .expect("draw");
        let targets: Vec<_> = g.dash_hits.targets.iter().map(|(_, t)| *t).collect();
        assert!(targets.contains(&DashTarget::Tab(RailTab::Context)));
        assert!(targets.contains(&DashTarget::Tab(RailTab::Changes)));
        assert!(targets.contains(&DashTarget::ReviewItem(0)));
        assert!(g.dash_hits.transcript.is_some());
    }

    /// Replace `· <digits>s` (the status-bar session timer) with `· Ns`.
    fn mask_timer(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(pos) = rest.find("· ") {
            let (head, tail) = rest.split_at(pos + "· ".len());
            out.push_str(head);
            let digits = tail.chars().take_while(|c| c.is_ascii_digit()).count();
            if digits > 0 && tail[digits..].starts_with('s') {
                out.push_str("Ns");
                rest = &tail[digits + 1..];
            } else {
                rest = tail;
            }
        }
        out.push_str(rest);
        out
    }
}
