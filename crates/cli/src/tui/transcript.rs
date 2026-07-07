//! Transcript rendering — Antigravity/Codex-style inline output.
//!
//! User: `› text` (bold, dim prefix)
//! Assistant: `• text` (dim prefix), `  ` continuation
//! Tool running: `● ToolName(args)  running…` (yellow dot)
//! Tool done: `● ToolName(args)  120ms (ctrl+o to expand)` (green/red dot)
//! Thinking: `• text` (dim, italic)
//! Approval: `● tool  awaiting approval`

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use dcode_ai_common::event::QuestionSelection;

use crate::tool_ui;
use crate::tui::app::{
    LineAnswerHit, LineClickHit, line_has_text, prefixed_line, push_transcript_line,
    tool_header_detail_spans,
};
use crate::tui::markdown::render_markdown_lines_with_hits;
use crate::tui::render_helpers::{truncate_chars, wrap_text};
use crate::tui::state::{DisplayBlock, TuiSessionState};
use crate::tui::theme;

const DIM: Modifier = Modifier::DIM;
const BOLD: Modifier = Modifier::BOLD;
const ITALIC: Modifier = Modifier::ITALIC;

fn dim() -> Style {
    Style::default().add_modifier(DIM)
}
/// Light grey used for reasoning/thinking text — readable but de-emphasized.
fn thinking_grey() -> Color {
    Color::Rgb(150, 155, 170)
}

/// Whether the active theme uses a light background — picks the diff tint set.
fn theme_is_light() -> bool {
    if let Color::Rgb(r, g, b) = theme::bg() {
        // Rec. 601 perceived luminance; >150/255 reads as a light background.
        let lum = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        lum > 150.0
    } else {
        false
    }
}

/// Codex-style diff background tints: subtle green/red fills that stay distinct
/// from syntax colors. Dark terminals get muted tints; light terminals get
/// GitHub-style pastels (matching Codex's palette).
fn diff_add_bg() -> Color {
    if theme_is_light() {
        Color::Rgb(218, 251, 225)
    } else {
        Color::Rgb(33, 58, 43)
    }
}
fn diff_del_bg() -> Color {
    if theme_is_light() {
        Color::Rgb(255, 235, 233)
    } else {
        Color::Rgb(74, 34, 29)
    }
}

/// Index of the first line of a unified diff within a tool-output `detail`,
/// or `None` when there's no diff. Recognizes `git diff` headers, `---`/`+++`
/// file markers, and bare `@@` hunk headers.
fn diff_body_line_index(detail: &str) -> Option<usize> {
    detail
        .lines()
        .position(|l| l.starts_with("@@ ") || l.starts_with("--- ") || l.starts_with("diff "))
}

/// Derive a syntect language token from a file path's extension, e.g.
/// `src/foo.rs` → `rs`. Returns `None` when there's no usable extension.
fn lang_from_path_ext(path: &str) -> Option<String> {
    std::path::Path::new(path.trim())
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_string())
}

/// Parse a `@@ -old[,n] +new[,m] @@` hunk header into
/// `(old_start, new_start, max_line_seen)`. Returns `None` for non-headers.
fn parse_hunk_header(s: &str) -> Option<(usize, usize, usize)> {
    let body = s.strip_prefix("@@ ")?;
    let end = body.find(" @@")?;
    let mut parts = body[..end].split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let pair = |p: &str| -> (usize, usize) {
        let mut it = p.split(',');
        let start = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let count = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
        (start, count)
    };
    let (os, oc) = pair(old);
    let (ns, nc) = pair(new);
    Some((os, ns, (os + oc).max(ns + nc)))
}

/// Render a unified diff Codex-style: a right-aligned line-number gutter, a
/// `+`/`-`/` ` sign column, green/red foreground over subtle background tints,
/// and `⋮` separators between hunks. Caps output at `max_preview` rendered
/// lines with a fold hint, matching the rest of the tool-output preview model.
fn render_unified_diff(
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    diff: &str,
    width: usize,
    max_preview: usize,
    lang: Option<&str>,
) {
    // Pre-scan hunk headers so the gutter width fits the widest line number.
    let mut max_line = 1usize;
    for l in diff.lines() {
        if let Some((_, _, m)) = parse_hunk_header(l) {
            max_line = max_line.max(m);
        }
    }
    let gutter_w = max_line.to_string().len().max(2);

    let mut old_ln = 0usize;
    let mut new_ln = 0usize;
    let mut first_hunk = true;
    let mut shown = 0usize;
    let mut total = 0usize;
    // Stateful highlighter, reset at each hunk boundary so syntect parser state
    // (multi-line strings/comments) carries correctly within a hunk but doesn't
    // leak across the `⋮` gaps.
    let mut hl = lang.and_then(crate::tui::markdown::DiffHighlighter::new);

    for raw in diff.lines() {
        // The block header already names the file; skip diff file headers.
        if raw.starts_with("--- ")
            || raw.starts_with("+++ ")
            || raw.starts_with("diff ")
            || raw.starts_with("index ")
        {
            continue;
        }
        if let Some((os, ns, _)) = parse_hunk_header(raw) {
            old_ln = os;
            new_ln = ns;
            hl = lang.and_then(crate::tui::markdown::DiffHighlighter::new);
            if !first_hunk {
                total += 1;
                if shown < max_preview {
                    push_transcript_line(
                        lines,
                        hits,
                        Line::from(Span::styled(format!("    {:>gutter_w$} ⋮", ""), dim())),
                        None,
                    );
                    shown += 1;
                }
            }
            first_hunk = false;
            continue;
        }

        let first = raw.chars().next().unwrap_or(' ');
        let (sign, content, fg, bg, num) = if first == '+' {
            let n = new_ln;
            new_ln += 1;
            ('+', &raw[1..], theme::success(), Some(diff_add_bg()), n)
        } else if first == '-' {
            let n = old_ln;
            old_ln += 1;
            ('-', &raw[1..], theme::error(), Some(diff_del_bg()), n)
        } else {
            let c = raw.strip_prefix(' ').unwrap_or(raw);
            let n = new_ln;
            old_ln += 1;
            new_ln += 1;
            (' ', c, theme::muted(), None, n)
        };

        total += 1;
        if shown >= max_preview {
            continue;
        }

        // On light themes the pastel background carries the add/delete signal,
        // so content uses a dark foreground; the sign char keeps green/red.
        let content_fg = if bg.is_some() && theme_is_light() {
            theme::text()
        } else {
            fg
        };
        let mut sign_style = Style::default().fg(fg);
        let mut content_style = Style::default().fg(content_fg);
        if let Some(b) = bg {
            sign_style = sign_style.bg(b);
            content_style = content_style.bg(b);
        }
        let num_str = format!("{num:>gutter_w$}");
        // "    " indent + gutter + " " + sign char.
        let prefix_cols = 4 + gutter_w + 2;
        let avail = width.saturating_sub(prefix_cols).max(8);
        let wrapped = wrap_text(content, avail);

        // Feed every content line to the stateful highlighter so syntect parser
        // state stays in sync even for lines we render plain (wrapped). Use the
        // highlighted spans only when the line fits on one row. The diff
        // add/delete signal stays on the background tint + sign char; deletes get
        // a DIM overlay like Codex.
        let hl_spans = hl.as_mut().map(|h| h.line(content));
        let syntax_spans = if wrapped.len() == 1 && !content.trim().is_empty() {
            hl_spans.filter(|s| !s.is_empty())
        } else {
            None
        };

        if let Some(syn) = syntax_spans {
            let mut row_spans = vec![
                Span::styled(format!("    {num_str} "), dim()),
                Span::styled(sign.to_string(), sign_style),
            ];
            for (st, text) in syn {
                let mut s = st;
                if let Some(b) = bg {
                    s = s.bg(b);
                }
                if first == '-' {
                    s = s.add_modifier(DIM);
                }
                row_spans.push(Span::styled(text, s));
            }
            push_transcript_line(lines, hits, Line::from(row_spans), None);
            shown += 1;
        } else {
            for (i, wl) in wrapped.into_iter().enumerate() {
                if shown >= max_preview {
                    break;
                }
                let row = if i == 0 {
                    Line::from(vec![
                        Span::styled(format!("    {num_str} "), dim()),
                        Span::styled(sign.to_string(), sign_style),
                        Span::styled(wl, content_style),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled(format!("    {:gutter_w$}  ", ""), dim()),
                        Span::styled(wl, content_style),
                    ])
                };
                push_transcript_line(lines, hits, row, None);
                shown += 1;
            }
        }
    }

    if total > shown {
        push_transcript_line(
            lines,
            hits,
            Line::from(Span::styled(
                format!("    … +{} more lines (ctrl+o to fold)", total - shown),
                dim(),
            )),
            None,
        );
    }
}

/// Render a range of committed blocks into styled lines for flushing to
/// terminal scrollback via `insert_before`. Uses a temporary state view
/// that skips streaming content.
pub(crate) fn render_blocks_range(
    state: &TuiSessionState,
    start: usize,
    end: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let w = width.max(20) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineAnswerHit> = Vec::new();
    for (offset, block) in state.blocks[start..end].iter().enumerate() {
        let idx = start + offset;
        // Codex-style turn separator: a dim full-width rule before a user
        // message that opens a new turn (i.e. follows a response block), giving
        // clear visual boundaries between turns in the scrollback.
        if idx > 0
            && matches!(block, DisplayBlock::User(_))
            && starts_new_turn(&state.blocks[idx - 1])
        {
            let label = turn_summary(&state.blocks, idx);
            push_transcript_line(&mut lines, &mut hits, turn_rule(w, label), None);
        }
        render_block(block, state, w, &mut lines, &mut hits);
    }
    let (lines, _) = collapse_blank_runs(lines, hits);
    lines
}

/// True when `prev` is a block produced by the assistant's turn — used to draw a
/// turn separator only at real turn boundaries (not before the first message or
/// after a system notice).
fn starts_new_turn(prev: &DisplayBlock) -> bool {
    matches!(
        prev,
        DisplayBlock::Assistant(_)
            | DisplayBlock::ToolDone { .. }
            | DisplayBlock::ToolRunning { .. }
            | DisplayBlock::Thinking(_)
    )
}

/// A dim full-width horizontal rule marking a turn boundary, optionally opened
/// with a Codex-style label (`─ 3 tool calls · 1.2s ─────`).
fn turn_rule(w: usize, label: Option<String>) -> Line<'static> {
    let style = Style::default().fg(theme::border());
    match label {
        Some(lbl) => {
            let text = format!("─ {lbl} ");
            let text_w = text.chars().count().min(w);
            let dashes = w.saturating_sub(text_w);
            Line::from(vec![
                Span::styled(text, style),
                Span::styled("─".repeat(dashes), style),
            ])
        }
        None => Line::from(Span::styled("─".repeat(w), style)),
    }
}

/// Summarize the just-finished turn (from the previous user message up to
/// `user_idx`) as a Codex-style tool-activity label, or `None` for a purely
/// conversational turn. Computed straight from the block list — no extra timing
/// plumbing needed.
fn turn_summary(blocks: &[DisplayBlock], user_idx: usize) -> Option<String> {
    let mut calls = 0u32;
    let mut ms = 0u64;
    let mut i = user_idx;
    while i > 0 {
        i -= 1;
        match &blocks[i] {
            DisplayBlock::User(_) => break,
            DisplayBlock::ToolDone { duration_ms, .. } => {
                calls += 1;
                ms += duration_ms.unwrap_or(0);
            }
            _ => {}
        }
    }
    if calls == 0 {
        return None;
    }
    let dur = if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    };
    let noun = if calls == 1 {
        "tool call"
    } else {
        "tool calls"
    };
    Some(format!("{calls} {noun} · {dur}"))
}

/// For a width-resize reflow, return the first block index to re-emit so the
/// restored scrollback stays within `max_rows` rendered rows. Walks backward
/// from the transcript tail (newest first), mirroring Codex's row-capped reflow:
/// older blocks beyond the cap are dropped from scrollback but kept in memory
/// for the transcript overlay. Returns 0 when everything fits (or there are no
/// blocks), so small transcripts behave exactly as before.
pub(crate) fn reflow_start_block(state: &TuiSessionState, width: u16, max_rows: usize) -> usize {
    let n = state.blocks.len();
    let mut rows = 0usize;
    let mut start = n;
    while start > 0 {
        let candidate = start - 1;
        rows += render_blocks_range(state, candidate, start, width).len();
        // Always include this block (so the newest block shows even if it alone
        // exceeds the cap), then stop once we've crossed the row budget.
        start = candidate;
        if rows > max_rows {
            break;
        }
    }
    start
}

pub(crate) fn transcript_lines_and_hits(
    state: &TuiSessionState,
    width: u16,
) -> (Vec<Line<'static>>, Vec<LineAnswerHit>) {
    let w = width.max(20) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineAnswerHit> = Vec::new();

    // Live pane only renders blocks not yet flushed to terminal scrollback.
    let start = state.flushed_block_count.min(state.blocks.len());
    for (offset, block) in state.blocks[start..].iter().enumerate() {
        let idx = start + offset;
        if idx > 0
            && matches!(block, DisplayBlock::User(_))
            && starts_new_turn(&state.blocks[idx - 1])
        {
            let label = turn_summary(&state.blocks, idx);
            push_transcript_line(&mut lines, &mut hits, turn_rule(w, label), None);
        }
        render_block(block, state, w, &mut lines, &mut hits);
    }

    // ── Streaming thinking ── show the last few lines, light grey
    if let Some(thinking) = &state.streaming_thinking
        && !thinking.is_empty()
    {
        let think_style = Style::default().fg(thinking_grey()).add_modifier(ITALIC);
        let wrapped = wrap_text(thinking, w.saturating_sub(4));
        let shown = if state.thinking_expanded {
            wrapped.len()
        } else {
            6
        };
        let start = wrapped.len().saturating_sub(shown);
        for (i, text_line) in wrapped.iter().enumerate().skip(start) {
            let prefix = if i == start { "• thinking… " } else { "  " };
            push_transcript_line(
                &mut lines,
                &mut hits,
                Line::from(vec![
                    Span::styled(prefix, think_style),
                    Span::styled(text_line.clone(), think_style),
                ]),
                None,
            );
        }
    }

    // ── Streaming assistant ── identical styling to the finalized block so
    // there's no visual jump when generation completes.
    if let Some(stream) = &state.streaming_assistant
        && !stream.is_empty()
    {
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
        let marker_style = Style::default().fg(theme::assistant()).add_modifier(BOLD);
        let (md_lines, md_hits) =
            render_markdown_lines_with_hits(stream, state.code_line_numbers, w.saturating_sub(3));
        for (i, (md_line, md_hit)) in md_lines.into_iter().zip(md_hits).enumerate() {
            let prefix_style = if i == 0 { marker_style } else { dim() };
            let prefix = if i == 0 { "• " } else { "  " };
            push_transcript_line(
                &mut lines,
                &mut hits,
                prefixed_line(Span::styled(prefix, prefix_style), md_line),
                md_hit,
            );
        }
    }

    // ── Empty state ──
    if lines.is_empty() {
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
    }

    let (lines, mut hits) = collapse_blank_runs(lines, hits);

    for (i, hit) in hits.iter_mut().enumerate() {
        if hit.is_some() {
            continue;
        }
        if let Some(line) = lines.get(i) {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            if let Some(url) = extract_first_url(&text) {
                *hit = Some(LineClickHit::OpenLink(url));
            }
        }
    }

    (lines, hits)
}

fn render_block(
    block: &DisplayBlock,
    state: &TuiSessionState,
    w: usize,
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
) {
    match block {
        DisplayBlock::User(content) => {
            // User messages render as a full-width highlighted bar — bold accent
            // text on a subtle surface fill — so the prompt carries visual weight
            // and stays clearly separated from the assistant text around it.
            push_transcript_line(lines, hits, Line::default(), None);
            let user_style = Style::default()
                .fg(theme::user())
                .bg(theme::user_bar_bg())
                .add_modifier(BOLD);
            // Only the very first rendered row carries the `›` marker;
            // wrapped continuations indent under it (Codex-style), instead
            // of repeating the marker on every wrapped segment.
            let mut first_row = true;
            for raw_line in content.lines() {
                for wl in wrap_text(raw_line, w.saturating_sub(3)) {
                    let prefix = if first_row { "› " } else { "  " };
                    first_row = false;
                    // Pad to full width so the highlight spans the whole line.
                    let mut text = format!("{prefix}{wl}");
                    let pad = w.saturating_sub(text.chars().count());
                    text.push_str(&" ".repeat(pad));
                    push_transcript_line(
                        lines,
                        hits,
                        Line::from(Span::styled(text, user_style)),
                        None,
                    );
                }
            }
            push_transcript_line(lines, hits, Line::default(), None);
        }

        DisplayBlock::Assistant(content) => {
            // Assistant: accent-colored `•` marker so replies are easy to spot.
            push_transcript_line(lines, hits, Line::default(), None);
            let (md_lines, md_hits) = render_markdown_lines_with_hits(
                content,
                state.code_line_numbers,
                w.saturating_sub(3),
            );
            let marker_style = Style::default().fg(theme::assistant()).add_modifier(BOLD);
            for (i, (md_line, md_hit)) in md_lines.into_iter().zip(md_hits).enumerate() {
                let prefix_style = if i == 0 { marker_style } else { dim() };
                let prefix = if i == 0 { "• " } else { "  " };
                push_transcript_line(
                    lines,
                    hits,
                    prefixed_line(Span::styled(prefix, prefix_style), md_line),
                    md_hit,
                );
            }
            push_transcript_line(lines, hits, Line::default(), None);
        }
        DisplayBlock::ToolRunning {
            name,
            input,
            call_id: _,
        } => {
            let ui = tool_ui::metadata(name);
            // Blank line before so the tool call gets breathing room.
            push_transcript_line(lines, hits, Line::default(), None);
            // ● ToolName(args)  running…
            let mut spans = vec![
                Span::styled("● ", Style::default().fg(theme::warn())),
                Span::styled(
                    ui.label,
                    Style::default().fg(theme::text()).add_modifier(BOLD),
                ),
            ];
            let detail_spans = tool_header_detail_spans(name, input);
            if !detail_spans.is_empty() {
                spans.push(Span::styled("(", dim()));
                spans.extend(detail_spans);
                spans.push(Span::styled(")", dim()));
            }
            spans.push(Span::styled(
                "  running…",
                Style::default().fg(theme::warn()),
            ));
            push_transcript_line(lines, hits, Line::from(spans), None);
        }
        DisplayBlock::ToolDone {
            name,
            call_id,
            ok,
            detail,
            duration_ms,
        } => {
            let ui = tool_ui::metadata(name);
            let dot_color = if *ok {
                theme::success()
            } else {
                theme::error()
            };
            // Read-only tools (file reads, grep, status) collapse to a header by
            // default so their output doesn't clutter the transcript; edits/
            // writes stay expanded so the diff is visible.
            let collapsed = state.is_tool_block_collapsed_for(call_id, name);
            // Blank line before for spacing.
            push_transcript_line(lines, hits, Line::default(), None);
            let mut spans = vec![
                Span::styled("● ", Style::default().fg(dot_color)),
                Span::styled(
                    ui.label,
                    Style::default().fg(theme::text()).add_modifier(BOLD),
                ),
            ];
            let first_line = detail.lines().next().unwrap_or("");
            let file_path = first_line
                .strip_prefix("Wrote ")
                .or_else(|| first_line.strip_prefix("Edited "))
                .or_else(|| first_line.strip_prefix("Patched "))
                .or_else(|| first_line.strip_prefix("Created "))
                .or_else(|| first_line.strip_prefix("Deleted "));
            if let Some(fp) = file_path {
                // Drop any trailing "(replaced N occurrences)" annotation so the
                // header shows just the path, like Codex.
                let path_only = fp.trim().split(" (").next().unwrap_or(fp).trim();
                spans.push(Span::styled(
                    format!("({})", truncate_chars(path_only, 48)),
                    dim(),
                ));
            }
            if !*ok {
                let code = parse_exit_code(detail);
                let label = match code {
                    Some(c) => format!("  ✗ exit {c}"),
                    None => "  ✗".to_string(),
                };
                spans.push(Span::styled(
                    label,
                    Style::default().fg(theme::error()).add_modifier(BOLD),
                ));
            }
            if let Some(ms) = duration_ms {
                spans.push(Span::styled(format!("  {}", format_duration(*ms)), dim()));
            }
            let (adds, dels) = crate::tui::app::diff_change_counts(detail);
            if adds > 0 || dels > 0 {
                spans.push(Span::styled(format!("  +{adds} -{dels}"), dim()));
            }
            if collapsed && !detail.trim().is_empty() {
                spans.push(Span::styled("  (ctrl+o to expand)", dim()));
            }
            push_transcript_line(
                lines,
                hits,
                Line::from(spans),
                if detail.trim().is_empty() {
                    None
                } else {
                    Some(LineClickHit::CopyText(detail.clone()))
                },
            );

            // Show a capped preview of the output unless collapsed. A unified
            // diff (file edits, `git diff`) renders Codex-style with line
            // numbers, sign column, and tinted add/delete backgrounds. Other
            // output falls back to the plain capped preview.
            let diff_idx = if !collapsed && !detail.trim().is_empty() {
                diff_body_line_index(detail)
            } else {
                None
            };
            if let Some(di) = diff_idx {
                let diff_body = detail.lines().skip(di).collect::<Vec<_>>().join("\n");
                // Derive the language for syntax highlighting from the edited
                // file's extension (header path or the diff's `+++` line).
                let lang = file_path
                    .map(|fp| {
                        fp.trim()
                            .split(" (")
                            .next()
                            .unwrap_or(fp)
                            .trim()
                            .to_string()
                    })
                    .and_then(|p| lang_from_path_ext(&p));
                render_unified_diff(lines, hits, &diff_body, w, 40, lang.as_deref());
                push_transcript_line(lines, hits, Line::default(), None);
            } else if !collapsed && !detail.trim().is_empty() && file_path.is_none() {
                const MAX_PREVIEW: usize = 14;
                let mut shown = 0usize;
                let mut total = 0usize;
                for raw in detail.lines() {
                    total += 1;
                    if shown >= MAX_PREVIEW {
                        continue;
                    }
                    // Color diff-style lines (+/- /@@) and plan checkboxes.
                    let line_style = if raw.starts_with("[x]") {
                        Style::default().fg(theme::success())
                    } else if raw.starts_with("[~]") {
                        Style::default().fg(theme::accent()).add_modifier(BOLD)
                    } else if raw.starts_with("[ ]") {
                        Style::default().fg(theme::text())
                    } else if raw.starts_with('+') && !raw.starts_with("+++") {
                        Style::default().fg(theme::success())
                    } else if raw.starts_with('-') && !raw.starts_with("---") {
                        Style::default().fg(theme::error())
                    } else if raw.starts_with("@@") || raw.starts_with("diff ") {
                        Style::default().fg(theme::warn())
                    } else {
                        Style::default().fg(theme::muted())
                    };
                    for wl in wrap_text(raw, w.saturating_sub(4)) {
                        if shown >= MAX_PREVIEW {
                            break;
                        }
                        // Codex-style tree connector on the first output line ties
                        // the output to its tool header; plain indent thereafter.
                        let prefix = if shown == 0 { "  └ " } else { "    " };
                        push_transcript_line(
                            lines,
                            hits,
                            Line::from(vec![
                                Span::styled(prefix, dim()),
                                Span::styled(wl, line_style),
                            ]),
                            None,
                        );
                        shown += 1;
                    }
                }
                if total > MAX_PREVIEW {
                    push_transcript_line(
                        lines,
                        hits,
                        Line::from(Span::styled(
                            format!("    … +{} more lines (ctrl+o to fold)", total - MAX_PREVIEW),
                            dim(),
                        )),
                        None,
                    );
                }
            }
        }
        DisplayBlock::ApprovalPending(req) => {
            let ui = tool_ui::metadata(&req.tool);
            push_transcript_line(lines, hits, Line::default(), None);
            push_transcript_line(
                lines,
                hits,
                Line::from(vec![
                    Span::styled("● ", Style::default().fg(theme::warn())),
                    Span::styled(
                        ui.label,
                        Style::default().fg(theme::warn()).add_modifier(BOLD),
                    ),
                    Span::styled("  awaiting approval", dim()),
                ]),
                None,
            );
            push_transcript_line(
                lines,
                hits,
                Line::from(Span::styled(
                    "  y approve · n deny · a always allow",
                    Style::default().fg(theme::muted()),
                )),
                None,
            );
            push_transcript_line(lines, hits, Line::default(), None);
        }
        DisplayBlock::ApprovalResolved { tool, approved } => {
            let (icon, color) = if *approved {
                ("approved", theme::success())
            } else {
                ("denied", theme::error())
            };
            push_transcript_line(
                lines,
                hits,
                Line::from(vec![
                    Span::styled("● ", Style::default().fg(color)),
                    Span::styled(format!("{tool} "), Style::default().fg(theme::text())),
                    Span::styled(icon, Style::default().fg(color).add_modifier(BOLD)),
                ]),
                None,
            );
        }
        DisplayBlock::System(s) => {
            if s.is_empty() {
                return;
            }
            for part in s.lines() {
                if part.is_empty() {
                    push_transcript_line(lines, hits, Line::default(), None);
                } else {
                    for wl in wrap_text(part, w.saturating_sub(3)) {
                        push_transcript_line(
                            lines,
                            hits,
                            Line::from(Span::styled(format!("  {wl}"), dim())),
                            None,
                        );
                    }
                }
            }
        }
        DisplayBlock::Question(q) => {
            let selected_answer = state.answered_questions.get(&q.question_id);
            push_transcript_line(lines, hits, Line::default(), None);
            for text_line in wrap_text(&q.prompt, w.saturating_sub(4)) {
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(vec![
                        Span::styled("? ", Style::default().fg(theme::warn()).add_modifier(BOLD)),
                        Span::styled(text_line, Style::default().fg(theme::text())),
                    ]),
                    None,
                );
            }
            if !state.question_modal_open {
                let sug_sel = matches!(selected_answer, Some(QuestionSelection::Suggested));
                let sug_style = if sug_sel {
                    Style::default()
                        .fg(Color::Black)
                        .bg(theme::success())
                        .add_modifier(BOLD)
                } else {
                    Style::default().fg(theme::success())
                };
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(vec![
                        Span::styled("  [0] ", sug_style),
                        Span::styled(
                            truncate_chars(&q.suggested_answer, w.saturating_sub(10)),
                            sug_style,
                        ),
                    ]),
                    Some(LineClickHit::Question(QuestionSelection::Suggested)),
                );
                for (i, o) in q.options.iter().enumerate() {
                    let opt_sel = matches!(
                        selected_answer,
                        Some(QuestionSelection::Option { option_id }) if option_id == &o.id
                    );
                    let style = if opt_sel {
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::user())
                            .add_modifier(BOLD)
                    } else {
                        Style::default().fg(theme::text())
                    };
                    push_transcript_line(
                        lines,
                        hits,
                        Line::from(vec![
                            Span::styled(format!("  [{}] ", i + 1), style),
                            Span::styled(truncate_chars(&o.label, w.saturating_sub(10)), style),
                        ]),
                        Some(LineClickHit::Question(QuestionSelection::Option {
                            option_id: o.id.clone(),
                        })),
                    );
                }
            }
            push_transcript_line(lines, hits, Line::default(), None);
        }
        DisplayBlock::Thinking(content) => {
            // Light grey, italic — clearly distinct from replies but readable.
            let think_style = Style::default().fg(thinking_grey()).add_modifier(ITALIC);
            if state.thinking_expanded {
                let wrapped = wrap_text(content, w.saturating_sub(4));
                for (i, text_line) in wrapped.iter().enumerate() {
                    let prefix = if i == 0 { "• thinking  " } else { "  " };
                    push_transcript_line(
                        lines,
                        hits,
                        Line::from(vec![
                            Span::styled(prefix, think_style),
                            Span::styled(text_line.clone(), think_style),
                        ]),
                        Some(LineClickHit::ToggleThinking),
                    );
                }
            } else {
                let preview =
                    truncate_chars(content.lines().next().unwrap_or(""), w.saturating_sub(28));
                let multiline =
                    content.lines().count() > 1 || content.chars().count() > w.saturating_sub(28);
                let hint = if multiline {
                    "  (ctrl+t to expand)"
                } else {
                    ""
                };
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(vec![
                        Span::styled(format!("• thinking: {preview}"), think_style),
                        Span::styled(hint.to_string(), dim()),
                    ]),
                    Some(LineClickHit::ToggleThinking),
                );
            }
        }
        DisplayBlock::ErrorLine(s) => {
            // Errors get a red `✗` marker and full (non-dim) color so they are
            // easy to spot in the scrollback instead of blending in.
            let err = Style::default().fg(theme::error());
            for (i, wl) in wrap_text(s, w.saturating_sub(4)).into_iter().enumerate() {
                let (prefix, style) = if i == 0 {
                    ("✗ ", err.add_modifier(BOLD))
                } else {
                    ("  ", err)
                };
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(vec![
                        Span::styled(prefix, err.add_modifier(BOLD)),
                        Span::styled(wl, style),
                    ]),
                    None,
                );
            }
        }
    }
}

fn collapse_blank_runs(
    lines: Vec<Line<'static>>,
    hits: Vec<LineAnswerHit>,
) -> (Vec<Line<'static>>, Vec<LineAnswerHit>) {
    // Collapse runs of blank lines to a single blank so turns get exactly one
    // line of separation. Crucially we KEEP a block's leading blank: blocks flush
    // to scrollback one batch at a time, and trimming the leading blank made a
    // freshly-sent user message butt directly against the previous reply.
    // Trailing blanks are still trimmed so the composer sits under the last line.
    let mut out_lines: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    let mut out_hits: Vec<LineAnswerHit> = Vec::with_capacity(hits.len());
    let mut blank_run = 0u8;
    for (line, hit) in lines.into_iter().zip(hits) {
        let blank = !line_has_text(&line);
        if blank {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out_lines.push(line);
        out_hits.push(hit);
    }
    while out_lines.last().is_some_and(|l| !line_has_text(l)) {
        out_lines.pop();
        out_hits.pop();
    }
    (out_lines, out_hits)
}

fn extract_first_url(text: &str) -> Option<String> {
    for word in text.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| {
            matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | ',')
        });
        if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

fn parse_exit_code(detail: &str) -> Option<i32> {
    for line in detail.lines().rev().take(3) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("exit code: ") {
            return rest.trim().parse().ok();
        }
        if let Some(rest) = trimmed.strip_prefix("exit ") {
            return rest.trim().parse().ok();
        }
    }
    None
}

#[allow(dead_code)]
pub fn transcript_line_count_for_bench(state: &TuiSessionState, width: u16) -> usize {
    transcript_lines_and_hits(state, width).0.len()
}

#[allow(dead_code)]
fn fmt_wall_time(epoch_secs: u64) -> Option<String> {
    if epoch_secs == 0 {
        return None;
    }
    let secs_today = epoch_secs % 86400;
    Some(format!(
        "{:02}:{:02}",
        secs_today / 3600,
        (secs_today % 3600) / 60
    ))
}
