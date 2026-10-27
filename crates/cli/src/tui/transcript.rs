//! Transcript rendering — Claude Code-style inline output.
//!
//! User: `> text` (muted)
//! Assistant: `● text`, two-space continuation
//! Tool: `● Name(args)` (green ok / red error / muted running), result
//! lines under `  ⎿  ` (dim): summaries, diffs, todo lists, errors
//! Thinking: `✻ Thinking…` (dim italic, collapsed preview)

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use dcode_ai_common::event::QuestionSelection;

use crate::tool_ui;
use crate::tui::app::{
    LineAnswerHit, LineClickHit, line_has_text, prefixed_line, push_transcript_line,
};
use crate::tui::markdown::render_markdown_lines_with_hits;
use crate::tui::render::tool_detail::tool_header_detail_spans;
use crate::tui::render_helpers::{truncate_chars, wrap_text};
use crate::tui::state::{DisplayBlock, TuiSessionState};
use crate::tui::theme;

const DIM: Modifier = Modifier::DIM;
const BOLD: Modifier = Modifier::BOLD;
const ITALIC: Modifier = Modifier::ITALIC;

fn dim() -> Style {
    Style::default().add_modifier(DIM)
}
/// De-emphasised colour for reasoning/thinking text (follows the theme).
fn thinking_grey() -> Color {
    theme::muted()
}

/// Whether the active theme is meant for a light background.
fn theme_is_light() -> bool {
    theme::is_light()
}

/// Diff background tints from the active theme (`Color::Reset` = none).
pub(crate) fn diff_add_bg() -> Color {
    theme::diff_add()
}
pub(crate) fn diff_del_bg() -> Color {
    theme::diff_del()
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
                        Line::from(Span::styled(
                            format!("{RESULT_INDENT} {:>gutter_w$} ⋮", ""),
                            dim(),
                        )),
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
        // Result indent + gutter + " " + sign char.
        let prefix_cols = RESULT_INDENT.len() + gutter_w + 2;
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
                Span::styled(format!("{RESULT_INDENT}{num_str} "), dim()),
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
                        Span::styled(format!("{RESULT_INDENT}{num_str} "), dim()),
                        Span::styled(sign.to_string(), sign_style),
                        Span::styled(wl, content_style),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled(format!("{RESULT_INDENT}{:gutter_w$}  ", ""), dim()),
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
                format!(
                    "{RESULT_INDENT}… +{} lines (ctrl+o to expand)",
                    total - shown
                ),
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
        render_block(block, idx, state, w, &mut lines, &mut hits);
    }
    let (lines, _) = collapse_blank_runs(lines, hits);
    lines
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

/// Live-pane transcript: blocks not yet flushed to terminal scrollback, plus
/// streaming output. The dashboard has no scrollback, so it renders every
/// block from the start.
pub(crate) fn transcript_lines_and_hits(
    state: &TuiSessionState,
    width: u16,
) -> (Vec<Line<'static>>, Vec<LineAnswerHit>) {
    let start = if state.view_mode == crate::tui::state::ViewMode::Dashboard {
        0
    } else {
        state.flushed_block_count
    };
    transcript_lines_and_hits_from(state, width, start)
}

pub(crate) fn transcript_lines_and_hits_from(
    state: &TuiSessionState,
    width: u16,
    start: usize,
) -> (Vec<Line<'static>>, Vec<LineAnswerHit>) {
    let w = width.max(20) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineAnswerHit> = Vec::new();

    let start = start.min(state.blocks.len());
    for (offset, block) in state.blocks[start..].iter().enumerate() {
        let idx = start + offset;
        render_block(block, idx, state, w, &mut lines, &mut hits);
    }

    // ── Streaming thinking ── `✻ Thinking…` with the last few lines.
    if let Some(thinking) = &state.streaming_thinking
        && !thinking.is_empty()
    {
        let think_style = Style::default().fg(thinking_grey()).add_modifier(ITALIC);
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(Span::styled("✻ Thinking…", think_style)),
            Some(LineClickHit::ToggleThinking),
        );
        let wrapped = wrap_text(thinking, w.saturating_sub(4));
        let shown = if state.thinking_expanded {
            wrapped.len()
        } else {
            6
        };
        let start = wrapped.len().saturating_sub(shown);
        for text_line in wrapped.iter().skip(start) {
            push_transcript_line(
                &mut lines,
                &mut hits,
                Line::from(Span::styled(format!("  {text_line}"), think_style)),
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
        let (md_lines, md_hits) =
            render_markdown_lines_with_hits(stream, state.code_line_numbers, w.saturating_sub(3));
        for (i, (md_line, md_hit)) in md_lines.into_iter().zip(md_hits).enumerate() {
            let prefix = if i == 0 {
                bullet(theme::text())
            } else {
                Span::raw("  ")
            };
            push_transcript_line(
                &mut lines,
                &mut hits,
                prefixed_line(prefix, md_line),
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

/// `● ` in `color` — the Claude-style item bullet.
fn bullet(color: Color) -> Span<'static> {
    Span::styled("● ", Style::default().fg(color))
}

/// Blink phase for a running tool's bullet (Claude's ● flashes while a tool
/// works): on for 600 ms, off for 600 ms. Always on in tests.
pub(crate) fn tool_blink_on() -> bool {
    if cfg!(test) {
        return true;
    }
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    (ms / 600).is_multiple_of(2)
}

/// Whether any of the newest blocks is a tool still running (so the
/// transcript must redraw to animate its bullet).
pub(crate) fn has_running_tool(state: &TuiSessionState) -> bool {
    state
        .blocks
        .iter()
        .rev()
        .take(64)
        .any(|b| matches!(b, DisplayBlock::ToolRunning { .. }))
}

/// Indent of result lines under a tool header (`  ⎿  `).
const RESULT_INDENT: &str = "     ";

/// First result line: `  ⎿  text`; later lines indent to match.
fn result_line(first: bool, spans: Vec<Span<'static>>) -> Line<'static> {
    let lead = if first { "  ⎿  " } else { RESULT_INDENT };
    let mut out = vec![Span::styled(lead, dim())];
    out.extend(spans);
    Line::from(out)
}

/// Plural-aware `N thing(s)`.
fn count_noun(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// One-line summary of a finished tool call, shown when its output is
/// collapsed (or as the first result line).
fn tool_result_summary(name: &str, detail: &str) -> String {
    let n = detail.lines().count();
    match name {
        "read_file" => format!("Read {}", count_noun(n, "line", "lines")),
        "list_directory" => format!("Listed {}", count_noun(n, "path", "paths")),
        "search" | "search_code" => format!("Found {}", count_noun(n, "line", "lines")),
        "fetch_url" => format!("Fetched {}", count_noun(n, "line", "lines")),
        "web_search" => "Searched the web".to_string(),
        "git_status" | "git_diff" => count_noun(n, "line", "lines"),
        _ => {
            let first = detail.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            if first.is_empty() {
                "(No content)".to_string()
            } else {
                first.trim().to_string()
            }
        }
    }
}

/// `Updated path with N additions and M removals` for an edit diff.
fn edit_summary(path: &str, adds: usize, dels: usize) -> String {
    let mut s = format!("Updated {path}");
    if adds > 0 || dels > 0 {
        s.push_str(" with ");
        let mut parts = Vec::new();
        if adds > 0 {
            parts.push(count_noun(adds, "addition", "additions"));
        }
        if dels > 0 {
            parts.push(count_noun(dels, "removal", "removals"));
        }
        s.push_str(&parts.join(" and "));
    }
    s
}

/// `Name(args)` header spans in plain text (Claude style: no colouring of
/// the arguments).
fn tool_header(name: &str, input: &str, dot: Color, w: usize) -> Line<'static> {
    let ui = tool_ui::metadata(name);
    let mut spans = vec![
        bullet(dot),
        Span::styled(
            ui.label,
            Style::default().fg(theme::text()).add_modifier(BOLD),
        ),
    ];
    // File tools name their file; everything else shows its main argument.
    let args: String = crate::tui::render::tool_detail::tool_input_path(name, input)
        .unwrap_or_else(|| {
            tool_header_detail_spans(name, input)
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        });
    let args = args.trim();
    if !args.is_empty() {
        let budget = w.saturating_sub(spans_width(&spans) + 2).max(8);
        spans.push(Span::styled(
            format!("({})", truncate_chars(args, budget)),
            Style::default().fg(theme::text()),
        ));
    }
    Line::from(spans)
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// `update_plan` output (`[x] done`, `[~] doing`, `[ ] todo`) as a
/// Claude-style checklist.
fn render_todos(
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    detail: &str,
    w: usize,
) {
    let mut first = true;
    for raw in detail.lines() {
        let t = raw.trim();
        let (mark, text, style) = if let Some(rest) = t.strip_prefix("[x]") {
            (
                "☒ ",
                rest,
                Style::default()
                    .fg(theme::muted())
                    .add_modifier(Modifier::CROSSED_OUT),
            )
        } else if let Some(rest) = t.strip_prefix("[~]") {
            (
                "☐ ",
                rest,
                Style::default().fg(theme::accent()).add_modifier(BOLD),
            )
        } else if let Some(rest) = t.strip_prefix("[ ]") {
            ("☐ ", rest, Style::default().fg(theme::text()))
        } else {
            continue;
        };
        let text = truncate_chars(text.trim(), w.saturating_sub(10));
        push_transcript_line(
            lines,
            hits,
            result_line(
                first,
                vec![
                    Span::styled(mark, style.remove_modifier(Modifier::CROSSED_OUT)),
                    Span::styled(text, style),
                ],
            ),
            None,
        );
        first = false;
    }
    if first {
        push_transcript_line(
            lines,
            hits,
            result_line(true, vec![Span::styled("(no todos)", dim())]),
            None,
        );
    }
}

fn render_block(
    block: &DisplayBlock,
    idx: usize,
    state: &TuiSessionState,
    w: usize,
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
) {
    match block {
        DisplayBlock::User(content) => {
            // `> prompt`, muted, wrapped lines indented under the text.
            push_transcript_line(lines, hits, Line::default(), None);
            let style = Style::default().fg(theme::muted());
            let mut first_row = true;
            for raw_line in content.lines() {
                for wl in wrap_text(raw_line, w.saturating_sub(3)) {
                    let prefix = if first_row { "> " } else { "  " };
                    first_row = false;
                    push_transcript_line(
                        lines,
                        hits,
                        Line::from(vec![Span::styled(prefix, style), Span::styled(wl, style)]),
                        None,
                    );
                }
            }
        }

        DisplayBlock::Assistant(content) => {
            // `● reply`. Long replies collapse to a one-line preview on click
            // (see LineClickHit::ToggleAssistant).
            push_transcript_line(lines, hits, Line::default(), None);
            let collapsed = state.collapsed_assistant_blocks.contains(&idx);
            if collapsed {
                let line_count = content.lines().count();
                let first_line = content.lines().next().unwrap_or("").trim();
                let preview = truncate_chars(first_line, w.saturating_sub(24));
                let spans = vec![
                    bullet(theme::text()),
                    Span::styled(preview.to_string(), Style::default().fg(theme::text())),
                    Span::styled(format!("  ({line_count} lines, click to expand)"), dim()),
                ];
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(spans),
                    Some(LineClickHit::ToggleAssistant(idx)),
                );
            } else {
                let (md_lines, md_hits) = render_markdown_lines_with_hits(
                    content,
                    state.code_line_numbers,
                    w.saturating_sub(3),
                );
                for (i, (md_line, md_hit)) in md_lines.into_iter().zip(md_hits).enumerate() {
                    let prefix = if i == 0 {
                        bullet(theme::text())
                    } else {
                        Span::raw("  ")
                    };
                    // The first row also toggles collapse when it isn't
                    // already carrying a more specific hit (e.g. a link).
                    let hit = if i == 0 {
                        md_hit.or(Some(LineClickHit::ToggleAssistant(idx)))
                    } else {
                        md_hit
                    };
                    push_transcript_line(lines, hits, prefixed_line(prefix, md_line), hit);
                }
            }
        }
        DisplayBlock::ToolRunning {
            name,
            input,
            output,
            call_id: _,
        } => {
            push_transcript_line(lines, hits, Line::default(), None);
            push_transcript_line(
                lines,
                hits,
                {
                    let mut header = tool_header(name, input, theme::text(), w);
                    if !tool_blink_on() {
                        header.spans[0] = Span::raw("  ");
                    }
                    header
                },
                None,
            );
            // Last few lines of streamed output.
            let all: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
            let live = &all[all.len().saturating_sub(4)..];
            if live.is_empty() {
                let text = if name == "execute_bash" {
                    "Running… (ctrl+b to run in background)"
                } else {
                    "Running…"
                };
                push_transcript_line(
                    lines,
                    hits,
                    result_line(true, vec![Span::styled(text, dim())]),
                    None,
                );
            } else {
                for (i, l) in live.iter().enumerate() {
                    push_transcript_line(
                        lines,
                        hits,
                        result_line(
                            i == 0,
                            vec![Span::styled(
                                truncate_chars(l, w.saturating_sub(8)),
                                Style::default().fg(theme::muted()),
                            )],
                        ),
                        None,
                    );
                }
            }
        }
        DisplayBlock::ToolDone {
            name,
            call_id,
            input,
            ok,
            detail,
        } => {
            let dot_color = if *ok {
                theme::success()
            } else {
                theme::error()
            };
            // Read-only tools (file reads, grep, status) collapse to a summary
            // by default; edits stay expanded so the diff is visible.
            let collapsed = state.is_tool_block_collapsed_for(call_id, name);
            push_transcript_line(lines, hits, Line::default(), None);
            let header = if input.trim().is_empty() {
                // Older blocks without input: recover the path from the detail.
                let first_line = detail.lines().next().unwrap_or("");
                let path = ["Wrote ", "Edited ", "Patched ", "Created ", "Deleted "]
                    .iter()
                    .find_map(|p| first_line.strip_prefix(p))
                    .map(|fp| {
                        fp.trim()
                            .split(" (")
                            .next()
                            .unwrap_or(fp)
                            .trim()
                            .to_string()
                    });
                let mut line = tool_header(name, "", dot_color, w);
                if let Some(p) = path {
                    line.spans.push(Span::styled(
                        format!("({})", truncate_chars(&p, 48)),
                        Style::default().fg(theme::text()),
                    ));
                }
                line
            } else {
                tool_header(name, input, dot_color, w)
            };
            push_transcript_line(
                lines,
                hits,
                header,
                if detail.trim().is_empty() {
                    None
                } else {
                    Some(LineClickHit::CopyText(detail.clone()))
                },
            );

            if !*ok {
                // Errors: red, a few lines, never collapsed.
                let err = Style::default().fg(theme::error());
                let body: Vec<&str> = detail.lines().filter(|l| !l.trim().is_empty()).collect();
                let code = parse_exit_code(detail);
                for (i, l) in body.iter().take(8).enumerate() {
                    let text = if i == 0 {
                        match code {
                            Some(c) => format!("Error (exit {c}): {l}"),
                            None => format!("Error: {l}"),
                        }
                    } else {
                        l.to_string()
                    };
                    for (j, wl) in wrap_text(&text, w.saturating_sub(8))
                        .into_iter()
                        .enumerate()
                    {
                        push_transcript_line(
                            lines,
                            hits,
                            result_line(i == 0 && j == 0, vec![Span::styled(wl, err)]),
                            None,
                        );
                    }
                }
                if body.is_empty() {
                    push_transcript_line(
                        lines,
                        hits,
                        result_line(true, vec![Span::styled("Error", err)]),
                        None,
                    );
                } else if body.len() > 8 {
                    push_transcript_line(
                        lines,
                        hits,
                        result_line(
                            false,
                            vec![Span::styled(format!("… +{} lines", body.len() - 8), dim())],
                        ),
                        None,
                    );
                }
                return;
            }

            if name == "update_plan" {
                render_todos(lines, hits, detail, w);
                return;
            }

            // Reads can contain `---` lines (PDF page markers, Markdown); only
            // tools that change files show a diff summary.
            let reads = matches!(
                name.as_str(),
                "read_file" | "search_code" | "list_directory" | "fetch_url" | "web_search"
            );
            let diff_idx = if reads {
                None
            } else {
                diff_body_line_index(detail)
            };
            if let Some(di) = diff_idx {
                let diff_body = detail.lines().skip(di).collect::<Vec<_>>().join("\n");
                let (adds, dels) = crate::tui::render::tool_detail::diff_change_counts(&diff_body);
                let path = crate::tui::render::tool_detail::tool_input_path(name, input)
                    .unwrap_or_else(|| "file".to_string());
                let text = if name == "write_file" && dels == 0 {
                    format!("Wrote {} to {path}", count_noun(adds, "line", "lines"))
                } else {
                    edit_summary(&path, adds, dels)
                };
                let mut summary = vec![Span::styled(text, Style::default().fg(theme::muted()))];
                if collapsed {
                    summary.push(Span::styled(" (ctrl+o to expand)", dim()));
                }
                push_transcript_line(lines, hits, result_line(true, summary), None);
                if !collapsed {
                    let lang = lang_from_path_ext(&path);
                    render_unified_diff(lines, hits, &diff_body, w, 40, lang.as_deref());
                }
                return;
            }

            let summary = tool_result_summary(name, detail);
            if collapsed || detail.trim().is_empty() {
                let mut spans = vec![Span::styled(
                    truncate_chars(&summary, w.saturating_sub(28)),
                    Style::default().fg(theme::muted()),
                )];
                if !detail.trim().is_empty() && detail.lines().count() > 1 {
                    spans.push(Span::styled(" (ctrl+o to expand)", dim()));
                }
                push_transcript_line(lines, hits, result_line(true, spans), None);
                return;
            }

            const MAX_PREVIEW: usize = 14;
            let mut shown = 0usize;
            let mut total = 0usize;
            for raw in detail.lines() {
                total += 1;
                if shown >= MAX_PREVIEW {
                    continue;
                }
                let line_style = if raw.starts_with('+') && !raw.starts_with("+++") {
                    Style::default().fg(theme::success())
                } else if raw.starts_with('-') && !raw.starts_with("---") {
                    Style::default().fg(theme::error())
                } else {
                    Style::default().fg(theme::muted())
                };
                for wl in wrap_text(raw, w.saturating_sub(8)) {
                    if shown >= MAX_PREVIEW {
                        break;
                    }
                    push_transcript_line(
                        lines,
                        hits,
                        result_line(shown == 0, vec![Span::styled(wl, line_style)]),
                        None,
                    );
                    shown += 1;
                }
            }
            if total > MAX_PREVIEW {
                push_transcript_line(
                    lines,
                    hits,
                    result_line(
                        false,
                        vec![Span::styled(
                            format!("… +{} lines (ctrl+o to expand)", total - MAX_PREVIEW),
                            dim(),
                        )],
                    ),
                    None,
                );
            }
        }
        DisplayBlock::ApprovalPending(req) => {
            push_transcript_line(lines, hits, Line::default(), None);
            push_transcript_line(
                lines,
                hits,
                tool_header(&req.tool, &req.input, theme::warn(), w),
                None,
            );
            push_transcript_line(
                lines,
                hits,
                result_line(true, vec![Span::styled("Waiting for permission…", dim())]),
                None,
            );
        }
        DisplayBlock::ApprovalResolved { tool, approved } => {
            // An approval just lets the tool run; only a denial is news.
            if !*approved {
                push_transcript_line(
                    lines,
                    hits,
                    result_line(
                        true,
                        vec![Span::styled(
                            format!("You denied {tool}"),
                            Style::default().fg(theme::error()),
                        )],
                    ),
                    None,
                );
            }
        }
        DisplayBlock::System(s) => {
            if s.is_empty() {
                return;
            }
            push_transcript_line(lines, hits, Line::default(), None);
            let mut first = true;
            for part in s.lines() {
                if part.is_empty() {
                    push_transcript_line(lines, hits, Line::default(), None);
                    continue;
                }
                for wl in wrap_text(part, w.saturating_sub(8)) {
                    push_transcript_line(
                        lines,
                        hits,
                        result_line(first, vec![Span::styled(wl, dim())]),
                        None,
                    );
                    first = false;
                }
            }
        }
        DisplayBlock::Notice { text, ok } => {
            push_transcript_line(lines, hits, Line::default(), None);
            let color = if *ok {
                theme::success()
            } else {
                theme::error()
            };
            for (i, wl) in wrap_text(text, w.saturating_sub(4)).into_iter().enumerate() {
                let lead = if i == 0 {
                    bullet(color)
                } else {
                    Span::raw("  ")
                };
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(vec![
                        lead,
                        Span::styled(wl, Style::default().fg(theme::text())),
                    ]),
                    None,
                );
            }
        }
        DisplayBlock::TurnSummary(text) => {
            push_transcript_line(lines, hits, Line::default(), None);
            push_transcript_line(
                lines,
                hits,
                Line::from(vec![
                    Span::styled("✻ ", dim()),
                    Span::styled(truncate_chars(text, w.saturating_sub(4)), dim()),
                ]),
                None,
            );
        }
        DisplayBlock::Question(q) => {
            // Asked and answered, Claude-style: `⏺ question` / `⎿ answer`.
            // The live choice is drawn in the prompt box, not here.
            push_transcript_line(lines, hits, Line::default(), None);
            for (i, text_line) in wrap_text(&q.prompt, w.saturating_sub(4))
                .into_iter()
                .enumerate()
            {
                let prefix = if i == 0 {
                    bullet(theme::accent())
                } else {
                    Span::raw("  ")
                };
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(vec![
                        prefix,
                        Span::styled(text_line, Style::default().fg(theme::text())),
                    ]),
                    None,
                );
            }
            if let Some(sel) = state.answered_questions.get(&q.question_id) {
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(vec![
                        Span::styled("  ⎿  ", dim()),
                        Span::styled(
                            truncate_chars(&question_answer_label(q, sel), w.saturating_sub(8)),
                            Style::default().fg(theme::text()).add_modifier(BOLD),
                        ),
                    ]),
                    None,
                );
            }
        }
        DisplayBlock::Thinking(content) => {
            // `✻ Thinking…` collapsed to a dim italic preview; ctrl+t expands.
            let think_style = Style::default().fg(thinking_grey()).add_modifier(ITALIC);
            push_transcript_line(lines, hits, Line::default(), None);
            if state.thinking_expanded {
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(Span::styled("✻ Thinking…", think_style)),
                    Some(LineClickHit::ToggleThinking),
                );
                for text_line in wrap_text(content, w.saturating_sub(4)) {
                    push_transcript_line(
                        lines,
                        hits,
                        Line::from(Span::styled(format!("  {text_line}"), think_style)),
                        Some(LineClickHit::ToggleThinking),
                    );
                }
            } else {
                let preview =
                    truncate_chars(content.lines().next().unwrap_or(""), w.saturating_sub(34));
                let multiline =
                    content.lines().count() > 1 || content.chars().count() > w.saturating_sub(34);
                let hint = if multiline { " (ctrl+t to expand)" } else { "" };
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(vec![
                        Span::styled(format!("✻ Thinking… {preview}"), think_style),
                        Span::styled(hint.to_string(), dim()),
                    ]),
                    Some(LineClickHit::ToggleThinking),
                );
            }
        }
        DisplayBlock::ErrorLine(s) => {
            let err = Style::default().fg(theme::error());
            push_transcript_line(lines, hits, Line::default(), None);
            for (i, wl) in wrap_text(s, w.saturating_sub(4)).into_iter().enumerate() {
                let prefix = if i == 0 {
                    bullet(theme::error())
                } else {
                    Span::raw("  ")
                };
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(vec![prefix, Span::styled(wl, err)]),
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

/// What the user picked, in words: the option label(s) or their own text.
pub(crate) fn question_answer_label(
    q: &dcode_ai_common::event::InteractiveQuestionPayload,
    sel: &QuestionSelection,
) -> String {
    let label = |id: &str| {
        q.options
            .iter()
            .find(|o| o.id == id)
            .map(|o| o.label.clone())
            .unwrap_or_else(|| id.to_string())
    };
    match sel {
        QuestionSelection::Suggested => label(&q.suggested_answer),
        QuestionSelection::Option { option_id } => label(option_id),
        QuestionSelection::Options { option_ids } if option_ids.is_empty() => "(none)".into(),
        QuestionSelection::Options { option_ids } => option_ids
            .iter()
            .map(|id| label(id))
            .collect::<Vec<_>>()
            .join(", "),
        QuestionSelection::Custom { text } => text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn state_with_assistant_block(content: &str) -> TuiSessionState {
        let mut st = TuiSessionState::new(
            "session-x".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        st.push_block(DisplayBlock::Assistant(content.to_string()));
        st
    }

    fn flat(lines: &[Line<'_>]) -> String {
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
    fn edits_show_claude_summary_and_expanded_diff() {
        let mut st = state_with_assistant_block("done");
        st.push_block(DisplayBlock::ToolDone {
            name: "edit_file".into(),
            call_id: "c1".into(),
            input: r#"{"path":"a.txt","old_string":"x","new_string":"y"}"#.into(),
            ok: true,
            detail: "Edited a.txt\n@@ -1,3 +1,3 @@\n a\n-x\n+y\n b".into(),
        });
        st.push_block(DisplayBlock::ToolDone {
            name: "write_file".into(),
            call_id: "c2".into(),
            input: r#"{"path":"new.txt","content":"1\n2\n"}"#.into(),
            ok: true,
            detail: "Wrote new.txt\n@@ -0,0 +1,2 @@\n+1\n+2".into(),
        });
        let (lines, _) = transcript_lines_and_hits(&st, 80);
        let t = flat(&lines);
        assert!(t.contains("● Update(a.txt)"), "{t}");
        assert!(
            t.contains("  ⎿  Updated a.txt with 1 addition and 1 removal\n"),
            "{t}"
        );
        assert!(t.contains("-x") && t.contains("+y"), "diff expanded: {t}");
        assert!(t.contains("● Write(new.txt)"), "{t}");
        assert!(t.contains("⎿  Wrote 2 lines to new.txt"), "{t}");
    }

    #[test]
    fn todos_render_as_checklist() {
        let mut st = state_with_assistant_block("ok");
        st.push_block(DisplayBlock::ToolDone {
            name: "update_plan".into(),
            call_id: "c1".into(),
            input: "{}".into(),
            ok: true,
            detail: "[x] read code\n[~] fix bug\n[ ] run tests".into(),
        });
        let (lines, _) = transcript_lines_and_hits(&st, 80);
        let t = flat(&lines);
        assert!(t.contains("● Update Todos"), "{t}");
        assert!(t.contains("  ⎿  ☒ read code"), "{t}");
        assert!(
            t.contains("     ☐ fix bug") && t.contains("     ☐ run tests"),
            "{t}"
        );
    }

    #[test]
    fn failed_tools_show_red_error_result() {
        let mut st = state_with_assistant_block("ok");
        st.push_block(DisplayBlock::ToolDone {
            name: "execute_bash".into(),
            call_id: "c1".into(),
            input: r#"{"command":"cargo test"}"#.into(),
            ok: false,
            detail: "boom\nexit code: 101".into(),
        });
        let (lines, _) = transcript_lines_and_hits(&st, 80);
        let t = flat(&lines);
        assert!(t.contains("● Bash(cargo test)"), "{t}");
        assert!(t.contains("⎿  Error (exit 101): boom"), "{t}");
    }

    #[test]
    fn expanded_assistant_block_header_toggles_collapse() {
        let st = state_with_assistant_block("first line\nsecond line\nthird line");
        let (_, hits) = transcript_lines_and_hits(&st, 60);
        assert!(
            hits.iter()
                .any(|h| matches!(h, Some(LineClickHit::ToggleAssistant(0)))),
            "expanded assistant block must expose a click hit to collapse it: {hits:?}"
        );
    }

    #[test]
    fn collapsed_assistant_block_shows_preview_not_full_content() {
        let mut st = state_with_assistant_block("first line\nsecond line\nthird line");
        st.collapsed_assistant_blocks.insert(0);
        let (lines, hits) = transcript_lines_and_hits(&st, 60);

        let rendered: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            !rendered.iter().any(|l| l.contains("second line")),
            "collapsed block must not render its body: {rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .any(|l| l.contains("first line") && l.contains("click to expand")),
            "collapsed block must show a one-line preview with an expand hint: {rendered:?}"
        );
        assert!(
            hits.iter()
                .any(|h| matches!(h, Some(LineClickHit::ToggleAssistant(0)))),
            "collapsed assistant block must still expose a click hit to expand it: {hits:?}"
        );
    }
}
